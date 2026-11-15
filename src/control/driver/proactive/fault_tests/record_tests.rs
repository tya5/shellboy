//! #226: the automatic attempt record is final or `unknown` with the exact set
//! of committed stages, whatever fails after a stage commits.
use super::*;

const ORDER: [&str; 4] = [
    "closed_spill",
    "closed_summary",
    "active_spill",
    "active_summary",
];

macro_rules! record_test {
    ($name:ident, $fault:expr) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            record_boundaries($fault).await;
        }
    };
}
record_test!(record_after_storage_fault, Fault::Storage);
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_target_percent_is_inherited_when_thread_is_unset() {
    boundary_with_thread_override(Fault::Storage, Scenario::ClosedSpill, false).await;
}
record_test!(record_after_cancellation, Fault::Cancellation);
record_test!(record_after_remeasurement_fault, Fault::Remeasurement);
// The two longest faults run each boundary as its own test with its own watchdog.
macro_rules! boundary_tests {
    ($fault:expr, $closed_spill:ident, $closed_summary:ident, $active_spill:ident, $active_summary:ident) => {
        boundary_test!($closed_spill, $fault, Scenario::ClosedSpill);
        boundary_test!($closed_summary, $fault, Scenario::ClosedSummary);
        boundary_test!($active_spill, $fault, Scenario::ActiveSpill);
        boundary_test!($active_summary, $fault, Scenario::ActiveSummary);
    };
}
macro_rules! boundary_test {
    ($name:ident, $fault:expr, $scenario:expr) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            crate::test_watchdog::watchdog(
                "automatic attempt record boundary",
                record_boundary($fault, $scenario),
            )
            .await
            .unwrap();
        }
    };
}
boundary_tests!(
    Fault::Persistent,
    record_after_persistent_fault_closed_spill,
    record_after_persistent_fault_closed_summary,
    record_after_persistent_fault_active_spill,
    record_after_persistent_fault_active_summary
);
record_test!(
    retired_automatic_writes_are_absent,
    Fault::RetiredWritesAbsent
);
record_test!(record_after_rejected_final_write, Fault::RejectFinal);
record_test!(rejected_stage_fact_prevents_adoption, Fault::RejectFact);

async fn record_boundaries(fault: Fault) {
    crate::test_watchdog::watchdog("automatic attempt record", async {
        for scenario in Scenario::ALL {
            record_boundary(fault, scenario).await;
        }
    })
    .await
    .unwrap();
}

struct Saved {
    attempt: String,
    record: Value,
}

fn trigger_identity(trigger: &Value) -> Value {
    json!({
        "input_tokens": trigger["input_tokens"],
        "auto_at_tokens": trigger["auto_at_tokens"],
        "target_percent": trigger["target_percent"],
        "target_tokens": trigger["target_tokens"],
        "a_input_tokens": trigger["a_input_tokens"],
    })
}

async fn record_boundary(fault: Fault, scenario: Scenario) {
    let stage = scenario.stage();
    let Fx {
        root,
        url,
        limits,
        owner,
        control,
        target,
        mut receive,
        server,
        guard_before,
        reached,
        checkpoint_reached,
        release_checkpoint,
    } = prepare(fault, scenario).await;
    if fault != Fault::RejectFact {
        crate::test_watchdog::watchdog("record boundary postcommit barrier", async {
            while !checkpoint_reached.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let checkpoint = assert_record(
        &control,
        &target,
        fault,
        scenario,
        stage,
        &guard_before,
        true,
    )
    .await;
    // A rejected stage fact has no stage barrier: its live observation point
    // is after the driver's terminal write for the failed stage.
    let live_phase = if fault == Fault::RejectFact {
        ObservationPhase::AfterRejectedStageTransaction
    } else {
        ObservationPhase::AtCommittedCheckpoint
    };
    assert_eq!(
        checkpoint.record["outcome"],
        expected_outcome(fault, scenario, live_phase),
        "record is observed only at its explicit live observation point"
    );
    if fault != Fault::RejectFact {
        assert!(checkpoint_reached.load(Ordering::SeqCst));
        assert_boundary(&control, &target, scenario, fault, "checkpoint").await;
        if fault == Fault::Remeasurement {
            assert!(
                control.compactor.fail_remeasurement.load(Ordering::SeqCst),
                "remeasurement failure is armed at the checkpoint"
            );
        }
        release_checkpoint.notify_one();
        after_release(&control, &mut receive, fault, scenario, &reached).await;
    }
    let shutdown = control.shutdown().await;
    let final_write_is_expected = fault != Fault::RejectFinal || !scenario.summary();
    if matches!(
        fault,
        Fault::Persistent
            | Fault::Remeasurement
            | Fault::RejectFact
            | Fault::Storage
            | Fault::RetiredWritesAbsent
    ) || fault == Fault::RejectFinal && final_write_is_expected
    {
        assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
    } else {
        shutdown.unwrap();
    }
    if fault != Fault::RejectFact && final_write_is_expected {
        assert!(
            reached.load(Ordering::SeqCst),
            "declared postcommit hook reached its configured checkpoint"
        );
    }
    assert!(
        receive.try_recv().is_err(),
        "no extra provider POST after checkpoint release and shutdown"
    );
    let terminal = assert_record(
        &control,
        &target,
        fault,
        scenario,
        stage,
        &guard_before,
        false,
    )
    .await;
    assert_eq!(
        terminal.attempt, checkpoint.attempt,
        "same attempt across phases"
    );
    if fault == Fault::RejectFinal && scenario == Scenario::ActiveSpill {
        let target_id = target.clone();
        let (expected_path, expected_revision, expected_epoch) = control
            .store
            .database
            .call(|db| {
                Ok(
                    db.query_row("SELECT path,revision,epoch FROM fault_boundary", [], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })?,
                )
            })
            .await
            .unwrap();
        let expected_path: Vec<String> = serde_json::from_str(&expected_path).unwrap();
        let (actual_path,revision,epoch,summary_replacements,spill_replacements)=control.store.database.call(move|db|{
            let path=db.prepare("SELECT p.node_id FROM thread_path p WHERE p.thread_id=?1 ORDER BY p.ordinal")?.query_map([&target_id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let revision_epoch=db.query_row("SELECT selection_revision,reasoning_epoch FROM thread WHERE id=?1",[&target_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?;
            let summaries=db.query_row("SELECT count(*) FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND n.kind='summary'",[&target_id],|r|r.get::<_,i64>(0))?;
            let spills=db.query_row("SELECT count(*) FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND n.kind='spill'",[&target_id],|r|r.get::<_,i64>(0))?;
            Ok((path,revision_epoch.0,revision_epoch.1,summaries,spills))
        }).await.unwrap();
        assert!(
            actual_path.starts_with(&expected_path),
            "strict FINISH rollback preserves the captured selected path prefix; append-only tail may extend it"
        );
        assert!(
            revision >= expected_revision,
            "legal append can advance selection revision"
        );
        assert_eq!(
            epoch, expected_epoch,
            "strict summary FINISH rollback preserves reasoning epoch"
        );
        assert_eq!(
            summary_replacements, 0,
            "failed FINISH leaves no adopted summary replacement"
        );
        assert_eq!(
            spill_replacements, 2,
            "the earlier committed spills remain adopted"
        );
    }
    assert_eq!(
        trigger_identity(&terminal.record["trigger"]),
        trigger_identity(&checkpoint.record["trigger"]),
        "immutable original trigger tuple across phases"
    );
    if scenario.continues_to_active_summary(fault) && fault == Fault::RejectFinal {
        // Strict FINISH and summary adoption share one transaction. RejectFinal
        // rolls that transaction back, so an entered stage is not a committed fact.
        assert_eq!(terminal.record["outcome"], "unknown", "{}", terminal.record);
        assert_eq!(
            terminal.record["committed_stages"], checkpoint.record["committed_stages"],
            "failed strict FINISH cannot publish an active-summary stage fact"
        );
    } else if scenario.continues_to_active_summary(fault) {
        let mut retained = terminal.record["committed_stages"].clone();
        assert!(
            retained
                .as_object_mut()
                .unwrap()
                .remove("active_summary")
                .is_some(),
            "{}",
            terminal.record
        );
        assert_eq!(
            retained, checkpoint.record["committed_stages"],
            "committed stage facts retained across terminal write"
        );
    } else {
        assert_eq!(
            terminal.record["committed_stages"], checkpoint.record["committed_stages"],
            "committed stage facts retained across terminal write"
        );
    }
    if fault == Fault::Cancellation && scenario == Scenario::ActiveSpill {
        // Stop is already set at the active-spill checkpoint. The required
        // pre-cancel fast path enters the closed-summary caller, returns before
        // planning, and does not manufacture an active fallback stage.
        let record = &terminal.record;
        assert_eq!(
            record["compaction_outcome"]["phase"], "closed_summary",
            "{record}"
        );
        assert_eq!(
            record["compaction_outcome"]["outcome"]["reason"], "cancelled",
            "{record}"
        );
        assert!(
            record["error"].is_object(),
            "typed cancellation error: {record}"
        );
    }

    if fault == Fault::RetiredWritesAbsent {
        let hits: i64 = control
            .store
            .database
            .call(|db| {
                Ok(
                    db.query_row("SELECT count(*) FROM retired_write_hits", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(
            hits, 0,
            "no retired W0/W2/W3 write through terminal finalization before trigger teardown"
        );
    }
    assert!(
        receive.try_recv().is_err(),
        "no provider POST after terminal outcome"
    );
    drop_fault_triggers(&control).await;
    drop(control);
    owner.shutdown().await.unwrap();
    let owner = proactive_owner(root.path(), &url, &limits).await;
    let control = Control::new(&owner, root.path().into());
    let after = assert_record(
        &control,
        &target,
        fault,
        scenario,
        stage,
        &guard_before,
        false,
    )
    .await;
    assert_eq!(
        terminal.record, after.record,
        "restart preserves the exact post-shutdown terminal record byte-for-byte"
    );
    if after.record["outcome"] == "unknown" {
        drop_fault_triggers(&control).await;
        drop(control);
        owner.shutdown().await.unwrap();
        let restarted = proactive_owner(root.path(), &url, &limits).await;
        let restarted_control = Control::new(&restarted, root.path().into());
        let response = restarted_control
            .request(crate::protocol::Request::new(
                format!("compact-list-restart:{}", after.attempt),
                Command::CompactList(CompactListArgs {
                    thread_id: Some(target.clone()),
                    since: None,
                    until: None,
                    limit: 50,
                }),
            ))
            .await;
        assert!(
            response.ok,
            "restarted in-process CompactList: {:?}",
            response.error
        );
        let list = response.result.unwrap();
        let item = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["attempt_id"] == after.attempt.as_str())
            .unwrap_or_else(|| panic!("reopened CompactList omitted persisted attempt: {list}"));
        assert_eq!(item["kind"], "automatic_reduction");
        assert_eq!(item["state"], "unknown");
        let _ = restarted_control.shutdown().await;
        restarted.shutdown().await.unwrap();
        return;
    }
    later_attempt(&control, &mut receive, &target, &after).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

// Only triggers this fault fixture actually registers. Query before dropping so
// no legacy fault_start or unrelated application trigger is touched.
const FIXTURE_TRIGGER_NAMES: &[&str] = &[
    "fault_adopt",
    "fault_epoch",
    "fault_restage",
    "fault_begin",
    "fault_fact",
    "persistent_attempt",
    "persistent_insert",
    "persistent_update",
    "persistent_delete",
    "reject_write",
    "reject_final_attempt",
    "retired_exception_write",
    "retired_active_insert",
    "retired_active_update",
    "retired_active_delete",
    "retired_guard_insert",
    "retired_guard_update",
    "retired_guard_delete",
];
async fn drop_fault_triggers(control: &Control) {
    control.store.database.call(|db| {
        for schema in ["temp", "main"] {
            for name in FIXTURE_TRIGGER_NAMES {
                let exists: bool = db.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_master WHERE type='trigger' AND name=?1)"),
                    [name], |row| row.get(0),
                )?;
                if exists {
                    db.execute_batch(&format!("DROP TRIGGER {schema}.\"{name}\""))?;
                }
            }
        }
        Ok(())
    }).await.unwrap();
}

/// A fresh above-threshold report starts a new automatic attempt: neither the
/// #180 guard nor a retained or removed witness suppresses compaction, and a
/// provisional target is verified against the new report.
async fn later_attempt(
    control: &Arc<Control>,
    receive: &mut mpsc::Receiver<Incoming>,
    target: &str,
    saved: &Saved,
) {
    let command = if cfg!(windows) {
        "'0' * 50000"
    } else {
        "printf '%050000d' 0"
    };
    send(control, target, "again").await;
    let mut calls = 0;
    loop {
        let request = incoming(control, receive).await;
        if request.body["tools"] == json!([]) {
            request.reply.send((200, answer("Later summary."))).unwrap();
            continue;
        }
        let original = saved.attempt.clone();
        let started: i64 = control.store.database.call(move |db| Ok(db.query_row(
            "SELECT count(*) FROM model_attempt WHERE id<>?1 AND json_type(error_json,'$.automatic_trim_attempt')='object'",
            [original], |r| r.get(0))?)).await.unwrap();
        if started > 0 {
            request
                .reply
                .send((200, answer_with_usage("done", 10)))
                .unwrap();
            break;
        }
        calls += 1;
        assert!(calls <= 6, "a later automatic attempt must start");
        request.reply.send((200, json!({"id":format!("later-{calls}"),"status":"completed","usage":{"input_tokens":200,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("later-{calls}"),"name":"shell","arguments":json!({"command":command}).to_string()}]}))).unwrap();
    }
    settled(control, target).await;
    let original = saved.attempt.clone();
    let verified = control.store.database.call(move |db| Ok(db.query_row(
        "SELECT json_extract(error_json,'$.automatic_trim_attempt.target_verification.outcome') FROM model_attempt WHERE id=?1",
        [original], |r| r.get::<_, Option<String>>(0))?)).await.unwrap();
    if saved.record["target_plan"].is_object()
        && saved.record["committed_stages"]
            .as_object()
            .is_some_and(|s| !s.is_empty())
    {
        assert!(
            matches!(verified.as_deref(), Some("met" | "insufficient")),
            "the provisional or final target is verified"
        );
    }
}

async fn assert_record(
    control: &Arc<Control>,
    target: &str,
    fault: Fault,
    scenario: Scenario,
    stage: &'static str,
    guard_before: &[(String, String)],
    live: bool,
) -> Saved {
    assert_immutable(control).await;
    let target = target.to_owned();
    let guard_before = guard_before.to_vec();
    let (attempt, record, replacements, witness, pending, boundary, begin, epoch) = {
        let (attempt, record, replacements, witness, pending, boundary, begin, epoch) = control.store.database.call({
            let target = target.clone();
            move |db| {
                let (attempt, record): (String, String) = db.query_row(
                    "SELECT id,json_extract(error_json,'$.automatic_trim_attempt') FROM model_attempt WHERE json_type(error_json,'$.automatic_trim_attempt')='object'",
                    [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                // Every replacement adopted for the Thread: (kind, output node).
                let replacements: Vec<(String, String)> = db.prepare("SELECT n.kind,n.id FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 ORDER BY n.rowid")?
                    .query_map([&target], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
                let witness: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key='compact.automatic_active.'||?1", [&target], |r| r.get(0))?;
                let pending: Option<String> = db.query_row("SELECT value FROM app_meta WHERE key='compact.automatic_target.'||?1", [&target], |r| r.get(0)).optional()?;
                let boundary: Option<(i64, i64)> = db.query_row("SELECT revision,epoch FROM fault_boundary", [], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten();
                let begin: Option<i64> = db.query_row("SELECT epoch FROM fault_begin", [], |r| r.get(0)).optional().ok().flatten();
                let epoch: i64 = db.query_row("SELECT reasoning_epoch FROM thread WHERE id=?1", [&target], |r| r.get(0))?;
                Ok((attempt, record, replacements, witness, pending, boundary, begin, epoch))
            }
        }).await.unwrap();
        (
            attempt,
            record,
            replacements,
            witness,
            pending,
            boundary,
            begin,
            epoch,
        )
    };
    let record: Value = serde_json::from_str(&record).unwrap();
    assert_eq!(record["reason"], REASON);
    assert_trigger_evidence(&record["trigger"], scenario);
    // The guard is untouched whenever the attempt's own guard write is not
    // reached or is rejected. A healthy W3 (cancellation, rejected final
    // write) legitimately saves its normal result before the failure.
    let guard_expected = guard_before.to_vec();
    assert_eq!(
        guard_rows(control).await,
        guard_expected,
        "the #180 guard/status rows remain byte-exact through this observation"
    );
    // This record is read after shutdown/reopen, so terminal writers that
    // succeeded are reflected; final-record write denials remain provisional.
    let phase = if live {
        if fault == Fault::RejectFact {
            ObservationPhase::AfterRejectedStageTransaction
        } else {
            ObservationPhase::AtCommittedCheckpoint
        }
    } else {
        ObservationPhase::AfterShutdownAndRestart
    };
    let outcome = expected_outcome(fault, scenario, phase);
    assert_eq!(record["outcome"], outcome, "{fault:?} {stage} {record}");
    assert_eq!(
        record["provisional"].as_bool().unwrap_or(false),
        outcome == "unknown",
        "{record}"
    );
    // Stage facts: fixed, content-free, and equal to the replacements adopted.
    let facts = record["committed_stages"]
        .as_object()
        .expect("committed_stages")
        .clone();
    let keys: Vec<&str> = ORDER
        .iter()
        .copied()
        .filter(|name| facts.contains_key(*name))
        .collect();
    assert_eq!(
        keys.len(),
        facts.len(),
        "only the four stage names: {record}"
    );
    let expected: Vec<&str> = match (stage, fault) {
        (_, Fault::RetiredWritesAbsent) => keys.clone(),
        _ => {
            let upto = ORDER.iter().position(|n| *n == stage).unwrap();
            let base: &[&str] = match stage {
                "closed_spill" | "active_spill" => &[],
                "closed_summary" => &["closed_spill"],
                _ => &["active_spill"],
            };
            let mut set: Vec<&str> = base.to_vec();
            let _ = upto;
            if fault != Fault::RejectFact {
                set.push(stage);
            }
            // After release the driver enters the active summary of the same
            // attempt (§3 summary branch); it commits its own stage fact.
            if !live
                && scenario.continues_to_active_summary(fault)
                && !(fault == Fault::RejectFinal && scenario == Scenario::ActiveSpill)
            {
                set.push("active_summary");
            }
            set
        }
    };
    assert_eq!(
        keys, expected,
        "{fault:?} {stage}: stages equal the committed replacements: {record}"
    );
    assert!(
        facts.contains_key(stage) || fault == Fault::RejectFact,
        "{record}"
    );
    let spill_total: u64 = facts
        .iter()
        .filter(|(k, _)| k.ends_with("_spill"))
        .map(|(_, f)| f["spilled_groups"].as_u64().unwrap())
        .sum();
    let spills = replacements.iter().filter(|(k, _)| k == "spill").count() as u64;
    assert_eq!(
        spill_total, spills,
        "spill facts equal adopted spill replacements: {record}"
    );
    let mut fact_summaries: Vec<String> = facts
        .values()
        .filter_map(|f| f["summary_node_id"].as_str().map(str::to_owned))
        .collect();
    let mut summaries: Vec<String> = replacements
        .iter()
        .filter(|(k, _)| k == "summary")
        .map(|(_, id)| id.clone())
        .collect();
    fact_summaries.sort();
    summaries.sort();
    assert_eq!(
        fact_summaries, summaries,
        "summary facts equal adopted summary replacements: {record}"
    );
    for (name, fact) in &facts {
        assert!(
            bounded_stage_fact(fact, name),
            "fixed bounded content-free schema: {fact}"
        );
    }

    // The captured boundary is re-taken by each later stage of the same
    // attempt, so it describes the last committed stage.
    let last = keys.last().copied().unwrap_or(stage);
    if let (Some((revision, boundary_epoch)), true) = (boundary, facts.contains_key(stage)) {
        assert_eq!(facts[last]["selection_revision"], revision);
        if fault.continues() {
            // The capture follows later epoch advances of the same attempt.
            assert!(facts[last]["reasoning_epoch"].as_i64().unwrap() <= boundary_epoch);
        } else {
            assert_eq!(facts[last]["reasoning_epoch"], boundary_epoch);
        }
    }
    if fault == Fault::RejectFact {
        // The rejected fact rolled its own adoption back, including the epoch.
        // With no committed stage the reference is the epoch at attempt begin.
        let expect = keys.last().map_or_else(
            || begin.expect("attempt begin epoch captured"),
            |last| facts[*last]["reasoning_epoch"].as_i64().unwrap(),
        );
        assert_eq!(
            epoch, expect,
            "the rejected stage left the reasoning epoch alone"
        );
        let kind = if stage.ends_with("spill") {
            "spill"
        } else {
            "summary"
        };
        let count = replacements.iter().filter(|(k, _)| k == kind).count();
        let committed = if kind == "spill" {
            spills
        } else {
            fact_summaries.len() as u64
        };
        assert_eq!(
            count as u64, committed,
            "no replacement for the rejected stage"
        );
        assert!(
            boundary.is_none() || stage != keys.last().copied().unwrap_or(""),
            "{stage}"
        );
    }
    // The TUI activity witness (#165, effective-spec "activity") is shown while
    // an attempt runs and is removed at its end; after shutdown/reopen it must
    // survive only while writes keep failing (a stale witness is removed at the
    // next owner start; the reopen check asserts that).
    assert!(
        witness == 0 || live || fault == Fault::Persistent,
        "activity witness must not outlive the attempt {fault:?} live={live}"
    );
    if fault == Fault::RetiredWritesAbsent {
        let hits: i64 = control
            .store
            .database
            .call(|db| {
                Ok(db.query_row("SELECT count(*) FROM retired_write_hits", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(
            hits, 0,
            "no retired W2/W3 production write through {live} observation"
        );
    }
    assert!(
        pending.is_none(),
        "no retired automatic_target marks are written: {fault:?} {record}"
    );
    Saved { attempt, record }
}

fn bounded_stage_fact(fact: &Value, name: &str) -> bool {
    let Some(object) = fact.as_object() else {
        return false;
    };
    let keys = object
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let gc_fact = fact.get("drop_all_reasoning").is_some();
    let expected = if gc_fact {
        [
            "stage",
            "selection_revision",
            "reasoning_epoch",
            "summary_node_id",
            "source_nodes",
            "spilled_groups",
            "drop_all_reasoning",
            "captured_reasoning_epoch",
            "source_characters",
            "summary_characters",
            "summary_limit",
            "model_requests",
            "summary_usage",
        ]
        .into_iter()
        .collect()
    } else {
        [
            "stage",
            "selection_revision",
            "reasoning_epoch",
            "summary_node_id",
            "source_nodes",
            "spilled_groups",
            "removed_characters",
            "source_characters",
            "summary_characters",
            "summary_limit",
            "model_requests",
            "summary_usage",
        ]
        .into_iter()
        .collect()
    };
    if keys != expected || fact["stage"] != name {
        return false;
    }
    if object.iter().any(|(key, value)| match key.as_str() {
        "stage" => !value.as_str().is_some_and(|s| s.len() <= 32),
        "summary_node_id" => {
            !value.is_null()
                && !value
                    .as_str()
                    .is_some_and(|id| id.len() <= 128 && uuid::Uuid::parse_str(id).is_ok())
        }
        "drop_all_reasoning" => !value.is_boolean(),
        "summary_usage" => !value.is_null() && !bounded_summary_usage(value),
        "source_nodes" | "spilled_groups" => value.as_u64().is_none(),
        "selection_revision" | "reasoning_epoch" | "captured_reasoning_epoch" => {
            value.as_i64().is_none()
        }
        _ => !value.is_null() && value.as_u64().is_none() && value.as_i64().is_none(),
    }) {
        return false;
    }
    fact.to_string().len() < 1024
}
fn bounded_summary_usage(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let keys = object
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if keys
        != [
            "requests",
            "input_tokens",
            "cached_tokens",
            "cache_creation_tokens",
            "output_tokens",
        ]
        .into_iter()
        .collect()
        || !value["requests"].is_u64()
    {
        return false;
    }
    [
        "input_tokens",
        "cached_tokens",
        "cache_creation_tokens",
        "output_tokens",
    ]
    .into_iter()
    .all(|name| {
        let n = &value[name];
        let Some(fields) = n.as_object() else {
            return false;
        };
        fields
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            == ["known_sum", "unknown_count", "overflow"]
                .into_iter()
                .collect()
            && (n["known_sum"].is_null() || n["known_sum"].is_u64())
            && n["unknown_count"].is_u64()
            && n["overflow"].is_boolean()
    })
}

#[test]
fn bounded_stage_schema_rejects_numeric_strings_content_and_untyped_usage() {
    let valid = json!({"stage":"active_summary","selection_revision":2,"reasoning_epoch":4,"summary_node_id":"550e8400-e29b-41d4-a716-446655440000","source_nodes":2,"spilled_groups":0,"drop_all_reasoning":true,"captured_reasoning_epoch":3,"source_characters":120,"summary_characters":24,"summary_limit":4000,"model_requests":1,"summary_usage":{"requests":1,"input_tokens":{"known_sum":4,"unknown_count":0,"overflow":false},"cached_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"output_tokens":{"known_sum":2,"unknown_count":0,"overflow":false}}});
    assert!(bounded_stage_fact(&valid, "active_summary"));
    let mut bad = valid.clone();
    bad["source_nodes"] = json!("2");
    assert!(!bounded_stage_fact(&bad, "active_summary"));
    let mut bad = valid.clone();
    bad["summary_node_id"] = json!("secret provider text");
    assert!(!bounded_stage_fact(&bad, "active_summary"));
    let mut bad = valid;
    bad["summary_usage"]["input_tokens"]["known_sum"] = json!("secret");
    assert!(!bounded_stage_fact(&bad, "active_summary"));
}

// The two uncalibrated call sites have no target plan, so the driver fixture
// above never reaches them. They are exercised through `summarize_once` itself
// on a real mid-Turn or next-Turn history that a held provider request leaves
// in place; no automatic attempt triggers because every reported usage stays
// below the threshold.
macro_rules! record_site_test {
    ($name:ident, $active:expr) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            crate::test_watchdog::watchdog("uncalibrated site", async {
                for fault in [Fault::Storage, Fault::Persistent] {
                    uncalibrated_site(fault, $active).await;
                }
            })
            .await
            .unwrap();
        }
    };
}

record_site_test!(
    uncalibrated_active_summary_is_recorded_and_checkpointed,
    true
);
record_site_test!(
    uncalibrated_late_prefix_summary_is_recorded_and_checkpointed,
    false
);

async fn uncalibrated_site(fault: Fault, active: bool) {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let limits =
        "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n";
    let owner = proactive_owner(root.path(), &url, limits).await;
    let control = Control::new(&owner, root.path().into());
    let target = thread(&control).await;
    // Use persisted, completed tool/source history with no live Driver task.
    // The old fixture held an in-flight Driver response while calling the
    // internal summary stage with a different local boundary Mutex; native GC
    // correctly waited for Control's real per-Thread boundary, deadlocking the
    // test itself. This seed uses production schema/path/tool rows and the same
    // real Control boundary below, without an invented driver snapshot.
    let target_for_seed = target.clone();
    let target_for_seed = target.clone();
    let(active_run,request_turn,attempt)=control.store.database.call(move|db|{
        let tx=db.transaction()?;
        let(profile,model,effort):(String,String,Option<String>)=tx.query_row("SELECT profile_id,model_id,effort FROM thread WHERE id=?1",[&target_for_seed],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let run=uuid::Uuid::new_v4().to_string();
        let request_turn=uuid::Uuid::new_v4().to_string();
        let old_run=(!active).then(||uuid::Uuid::new_v4().to_string());
        let old_turn=(!active).then(||uuid::Uuid::new_v4().to_string());
        if !active{
            let earlier_run=uuid::Uuid::new_v4().to_string();let earlier_turn=uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed','fixture','fixture')",rusqlite::params![earlier_run,target_for_seed])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,generation,created_at,ended_at)VALUES(?1,?2,?3,'completed',0,'fixture','fixture')",rusqlite::params![earlier_turn,target_for_seed,earlier_run])?;
            for(kind,role,text)in[("user","user","earlier request ".repeat(20_000)),("assistant_response","assistant","earlier answer ".repeat(20_000))]{let node=uuid::Uuid::new_v4().to_string();crate::history::append_node(&tx,&target_for_seed,&node,Some(&earlier_turn),kind,&json!({"role":role,"content":[{"type":"input_text","text":text}]}),"fixture")?;}
        }
        if let (Some(old_run),Some(old_turn))=(&old_run,&old_turn){
            tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed','fixture','fixture')",rusqlite::params![old_run,target_for_seed])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,generation,created_at,ended_at)VALUES(?1,?2,?3,'completed',0,'fixture','fixture')",rusqlite::params![old_turn,target_for_seed,old_run])?;
            for(kind,role,text)in[("user","user","prior request ".repeat(20_000)),("assistant_response","assistant","prior answer ".repeat(20_000))]{let node=uuid::Uuid::new_v4().to_string();crate::history::append_node(&tx,&target_for_seed,&node,Some(old_turn),kind,&json!({"role":role,"content":[{"type":"input_text","text":text}]}),"fixture")?;}
        }
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,'live','fixture')",rusqlite::params![run,target_for_seed])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES(?1,?2,?3,'running',0,'fixture')",rusqlite::params![request_turn,target_for_seed,run])?;
        let source_turn=if active{request_turn.clone()}else{old_turn.clone().expect("closed path has old turn")};
        let mut attempt=uuid::Uuid::new_v4().to_string();
        let insert_attempt=|tx:&rusqlite::Transaction<'_>,attempt:&str,turn:&str|->Result<(),crate::storage::StorageError>{
            let revision:i64=tx.query_row("SELECT selection_revision FROM thread WHERE id=?1",[&target_for_seed],|r|r.get(0))?;
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,effort,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,?2,0,?3,?4,?5,?6,0,'completed','{}','fixture')",rusqlite::params![attempt,turn,revision,profile,model,effort])?;Ok(())
        };
        if active{
            let user=uuid::Uuid::new_v4().to_string();
            crate::history::append_node(&tx,&target_for_seed,&user,Some(&request_turn),"user",&json!({"role":"user","content":[{"type":"input_text","text":"current request"}]}),"fixture")?;
            insert_attempt(&tx,&attempt,&request_turn)?;
            for index in 1..=2{
                let group=format!("g{index}");let call=format!("c{index}");let call_node=uuid::Uuid::new_v4().to_string();let result_node=uuid::Uuid::new_v4().to_string();
                let group_attempt=if index==1{attempt.clone()}else{let next=uuid::Uuid::new_v4().to_string();insert_attempt(&tx,&next,&request_turn)?;attempt=next.clone();next};
                tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,?3,'completed','fixture')",rusqlite::params![group,group_attempt,request_turn])?;
                let item=json!({"type":"function_call","call_id":call,"name":"shell","arguments":"{}"});
                let call_payload=json!({"role":"assistant","group_id":group,"output":[item.clone()]});
                crate::history::append_node(&tx,&target_for_seed,&call_node,Some(&request_turn),"assistant_response",&call_payload,"fixture")?;
                tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_id,item_type,status,payload_json,node_id)VALUES(?1,0,?2,'function_call','completed',?3,?4)",rusqlite::params![group_attempt,call,item.to_string(),call_node])?;
                tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,?2,0,'shell','{}','completed')",rusqlite::params![group,call])?;
                let result_payload=json!({"role":"tool","group_id":group,"call_id":call,"content":"tool output ".repeat(10_000)});
                crate::history::append_node(&tx,&target_for_seed,&result_node,Some(&request_turn),"tool_result",&result_payload,"fixture")?;
                tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?2,?3,'completed')",rusqlite::params![group,call,result_node])?;
            }
        }else{
            insert_attempt(&tx,&attempt,&request_turn)?;
            let node=uuid::Uuid::new_v4().to_string();
            crate::history::append_node(&tx,&target_for_seed,&node,Some(&request_turn),"user",&json!({"role":"user","content":[{"type":"input_text","text":"current request"}]}),"fixture")?;
        }
        tx.commit()?;Ok((run,request_turn,attempt))
    }).await.unwrap();
    let thread_id = target.clone();
    let automatic = Automatic {
        attempt: attempt.clone(),
        token: "site-token".into(),
        thread: thread_id.clone(),
        turn: request_turn,
    };
    let begin = (attempt.clone(), automatic.token.clone());
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            crate::compaction::attempt_begin_tx(
                &tx,
                &begin.0,
                &begin.1,
                REASON,
                crate::compaction::ABoundaryObservation::Observe,
                &json!({}),
                &Value::Null,
            )?;
            tx.execute(
                crate::compaction::discard::STAGE_SQL,
                rusqlite::params![&begin.0, &begin.1, "active_summary"],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let stage = if active {
        "active_summary"
    } else {
        "closed_summary"
    };
    let reached = Arc::new(AtomicBool::new(false));
    *control.automatic_fault.lock().unwrap() = Some(crate::control::AutomaticFault {
        stage,
        kind: if fault == Fault::Persistent { 4 } else { 1 },
        reached: reached.clone(),
        checkpoint_reached: Arc::new(AtomicBool::new(false)),
        release_checkpoint: Arc::new({
            let notify = tokio::sync::Notify::new();
            // This standalone retry-stage test consumes its own post-commit
            // failure before the test reads the attempt record below.
            notify.notify_one();
            notify
        }),
    });
    let slot = DriverSlot {
        id: "site".into(),
        run_id: active_run,
        stop: tokio_util::sync::CancellationToken::new(),
        nudge: Arc::new(tokio::sync::Notify::new()),
        foreground: Arc::new(crate::control::foreground::ForegroundSlot::default()),
        turn: Arc::new(tokio::sync::Mutex::new(None)),
    };
    let mutex = control.boundary(&thread_id).await;
    let mut boundary = crate::compaction::boundary::Boundary::new(&mutex, mutex.lock().await);
    let result=async{
        let work=summary_stage(&control,&control.compactor,&thread_id,&slot,active,&mut boundary,&automatic,slot.stop.clone());
        tokio::pin!(work);
        eprintln!("uncalibrated phase=await-summary-http active={active} fault={fault:?}");
        let request=tokio::select!{
            result=&mut work=>panic!("uncalibrated summary ended before provider admission: active={active} fault={fault:?} no_target={} err={:?}",result.as_ref().is_ok_and(|value|value.no_target),result.as_ref().err().map(|error|(error.code,error.message.clone()))),
            request=incoming(&control,&mut receive)=>request,
        };
        eprintln!("uncalibrated phase=summary-http-reached active={active} fault={fault:?}");
        assert_eq!(request.body["tools"],json!([]));
        request.reply.send((200,answer("Short site summary."))).unwrap();
        work.await
    }.await;
    let error = result.unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::SaveFailed,
        "the checkpoint fault follows the commit: {error:?}"
    );
    assert!(reached.load(Ordering::SeqCst));
    drop(boundary);
    let facts: Value = control.store.database.call({
        let attempt = attempt.clone();
        move |db| Ok(serde_json::from_str(&db.query_row("SELECT json_extract(error_json,'$.automatic_trim_attempt') FROM model_attempt WHERE id=?1", [attempt], |r| r.get::<_, String>(0))?).unwrap())
    }).await.unwrap();
    assert_eq!(
        facts["outcome"], "adopted",
        "strict summary FINISH committed before the postcommit fault hook"
    );
    assert_eq!(facts["provisional"], false);
    let recorded = facts["committed_stages"].as_object().unwrap();
    assert_eq!(
        recorded.keys().map(String::as_str).collect::<Vec<_>>(),
        [stage]
    );
    let selected: Vec<String> = control.store.database.call({
        let target = target.clone();
        move |db| Ok(db.prepare("SELECT n.id FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND n.kind='summary'")?.query_map([target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }).await.unwrap();
    assert_eq!(
        selected,
        [recorded[stage]["summary_node_id"]
            .as_str()
            .unwrap()
            .to_owned()]
    );
    assert!(recorded[stage]["source_nodes"].as_u64().unwrap() > 0);
    // Uncalibrated: there is no target plan, so no provisional target either.
    let pending: i64 = control
        .store
        .database
        .call({
            let target = target.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key='compact.automatic_target.'||?1",
                    [target],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(pending, 0);
    let _ = control.shutdown().await;
    owner.shutdown().await.unwrap();
    server.abort();
}
