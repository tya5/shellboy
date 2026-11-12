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
    assert_eq!(
        checkpoint.record["outcome"],
        expected_outcome(fault, ObservationPhase::AtCommittedCheckpoint),
        "record is observed only while its explicit stage barrier is held"
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
        if fault == Fault::Remeasurement && scenario != Scenario::ActiveSummary {
            crate::test_watchdog::watchdog("record remeasurement fault consumed", async {
                while control.compactor.fail_remeasurement.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        } else if fault == Fault::Remeasurement {
            assert!(
                control.compactor.fail_remeasurement.load(Ordering::SeqCst),
                "ActiveSummary abort happens before remeasurement"
            );
        }
    }
    let shutdown = control.shutdown().await;
    if matches!(
        fault,
        Fault::Persistent
            | Fault::RejectFinal
            | Fault::Remeasurement
            | Fault::RejectFact
            | Fault::Storage
            | Fault::RetiredWritesAbsent
    ) {
        assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
    } else {
        shutdown.unwrap();
    }
    if fault != Fault::RejectFact {
        assert!(
            reached.load(Ordering::SeqCst),
            "intended injector reached before shutdown"
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
    assert_eq!(
        trigger_identity(&terminal.record["trigger"]),
        trigger_identity(&checkpoint.record["trigger"]),
        "immutable original trigger tuple across phases"
    );
    assert_eq!(
        terminal.record["committed_stages"], checkpoint.record["committed_stages"],
        "committed stage facts retained across terminal write"
    );

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
        let restarted = crate::app::DataOwner::start(
            None,
            Some(root.path().to_path_buf()),
            root.path().to_path_buf(),
        )
        .await
        .unwrap();
        let response = crate::ipc::call(
            root.path(),
            Command::CompactList(CompactListArgs {
                thread_id: Some(target.clone()),
                since: None,
                until: None,
                limit: 50,
            }),
            None,
            None,
        )
        .await
        .unwrap();
        let list = response
            .result
            .expect("restarted public CompactList succeeds");
        let item = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["attempt_id"] == after.attempt.as_str())
            .unwrap_or_else(|| panic!("reopened CompactList omitted persisted attempt: {list}"));
        assert_eq!(item["kind"], "automatic_reduction");
        assert_eq!(item["state"], "unknown");
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
    let (attempt, record, replacements, witness, pending, boundary, initial, epoch) = {
        let (attempt, record, replacements, witness, pending, boundary, initial, epoch) = control.store.database.call({
            let target = target.clone();
            move |db| {
                let (attempt, record): (String, String) = db.query_row(
                    "SELECT id,json_extract(error_json,'$.automatic_trim_attempt') FROM model_attempt WHERE json_type(error_json,'$.automatic_trim_attempt')='object'",
                    [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                // Every replacement adopted for the Thread: (kind, output node).
                let replacements: Vec<(String, String)> = db.prepare("SELECT n.kind,n.id FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 ORDER BY n.rowid")?
                    .query_map([&target], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
                let witness: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key='compact.automatic_active.'||?1 AND key!='compact.automatic_active.retired-write-test'", [&target], |r| r.get(0))?;
                let pending: Option<String> = db.query_row("SELECT value FROM app_meta WHERE key='compact.automatic_target.'||?1", [&target], |r| r.get(0)).optional()?;
                let boundary: Option<(i64, i64)> = db.query_row("SELECT revision,epoch FROM fault_boundary", [], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten();
                let initial: Option<i64> = db.query_row("SELECT epoch FROM fault_initial", [], |r| r.get(0)).optional().ok().flatten();
                let epoch: i64 = db.query_row("SELECT reasoning_epoch FROM thread WHERE id=?1", [&target], |r| r.get(0))?;
                Ok((attempt, record, replacements, witness, pending, boundary, initial, epoch))
            }
        }).await.unwrap();
        (
            attempt,
            record,
            replacements,
            witness,
            pending,
            boundary,
            initial,
            epoch,
        )
    };
    let record: Value = serde_json::from_str(&record).unwrap();
    assert_eq!(record["reason"], REASON);
    assert_trigger_evidence(&record["trigger"], scenario);
    // The guard is untouched whenever the attempt's own guard write is not
    // reached or is rejected. A healthy W3 (cancellation, rejected final
    // write) legitimately saves its normal result before the failure.
    let mut guard_expected = guard_before.to_vec();
    if fault == Fault::RetiredWritesAbsent {
        guard_expected.extend([
            (
                "compact.automatic_guard.retired-write-test".into(),
                "seed-guard".into(),
            ),
            (
                "compact.automatic_status.retired-write-test".into(),
                "seed-status".into(),
            ),
        ]);
        guard_expected.sort();
    }
    if !matches!(fault, Fault::RetiredWritesAbsent) {
        assert_eq!(
            guard_rows(control).await,
            guard_expected,
            "the #180 guard is exactly the pre-attempt value"
        );
    }
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
    let outcome = expected_outcome(fault, phase);
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
        (_, Fault::RejectFinal) => keys.clone(),
        (_, Fault::Cancellation | Fault::RetiredWritesAbsent) => keys.clone(),
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
        let object = fact.as_object().unwrap();
        assert_eq!(fact["stage"], name.as_str());
        for (key, value) in object {
            let allowed = match key.as_str() {
                "stage" | "summary_node_id" => value.is_string() || value.is_null(),
                "summary_usage" => value.is_object() || value.is_null(),
                _ => value.is_u64() || value.is_i64() || value.is_null(),
            };
            assert!(
                allowed,
                "fact {name} field {key} is not fixed-size content-free data: {fact}"
            );
        }
        assert!(fact.to_string().len() < 1024, "fact stays small: {fact}");
    }
    if let (Some((revision, boundary_epoch)), true) = (boundary, facts.contains_key(stage)) {
        assert_eq!(facts[stage]["selection_revision"], revision);
        if fault.continues() {
            // The capture follows later epoch advances of the same attempt.
            assert!(facts[stage]["reasoning_epoch"].as_i64().unwrap() <= boundary_epoch);
        } else {
            assert_eq!(facts[stage]["reasoning_epoch"], boundary_epoch);
        }
    }
    if fault == Fault::RejectFact {
        // The rejected fact rolled its own adoption back, including the epoch.
        let expect = keys.last().map_or(initial.unwrap(), |last| {
            facts[*last]["reasoning_epoch"].as_i64().unwrap()
        });
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
    // The approved redesign supersedes historical D-226 W0 prose: no persistent
    // activity witness is written. Persistent attempt-record faults are evidenced
    // by the provisional record and exact stage facts, not a retired W0 marker.
    assert_eq!(
        witness, 0,
        "no legacy activity witness {fault:?} live={live}"
    );
    if fault == Fault::RetiredWritesAbsent {
        let rows: Vec<(String, String)> = control.store.database.call(|db| {
            Ok(db.prepare("SELECT key,value FROM app_meta WHERE key='compact.automatic_active.retired-write-test' ORDER BY key")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        }).await.unwrap();
        if live {
            assert_eq!(
                rows,
                [(
                    "compact.automatic_active.retired-write-test".into(),
                    "seed-active".into()
                )],
                "legacy W0 row remains byte-exact through the held checkpoint"
            );
        } else {
            assert!(
                rows.is_empty(),
                "normal terminal cleanup removes the retired W0 sentinel row"
            );
        }
        if live {
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
                "no retired W0/W2/W3 production write was attempted"
            );
        }
    }
    assert!(
        pending.is_none(),
        "no retired automatic_target marks are written: {fault:?} {record}"
    );
    Saved { attempt, record }
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
    if !active {
        for text in ["hi", "yo"] {
            send(&control, &target, text).await;
            incoming(&control, &mut receive)
                .await
                .reply
                .send((200, answer("ok")))
                .unwrap();
            settled(&control, &target).await;
        }
    }
    send(&control, &target, &"protected current ".repeat(15_000)).await;
    let command = if cfg!(windows) {
        "'0' * 50000"
    } else {
        "printf '%050000d' 0"
    };
    for index in 0..2 {
        incoming(&control, &mut receive).await.reply.send((200, json!({"id":format!("site-{index}"),"status":"completed","usage":{"input_tokens":100,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("site-{index}"),"name":"shell","arguments":json!({"command":command}).to_string()}]}))).unwrap();
    }
    let held = incoming(&control, &mut receive).await;
    let held = if active {
        held
    } else {
        held.reply
            .send((200, answer_with_usage("done", 100)))
            .unwrap();
        settled(&control, &target).await;
        send(&control, &target, "next turn").await;
        incoming(&control, &mut receive).await
    };
    // Calibration needs the request size; none is the uncalibrated case.
    let (thread_id, run, turn, attempt) = control.store.database.call({
        let target = target.clone();
        move |db| Ok(db.query_row(
            "SELECT t.thread_id,t.run_id,t.id,(SELECT a.id FROM model_attempt a WHERE a.state='completed' ORDER BY a.rowid DESC LIMIT 1) FROM turn t WHERE t.thread_id=?1 ORDER BY t.rowid DESC LIMIT 1",
            [target], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?)
    }).await.unwrap();
    let automatic = Automatic {
        attempt: attempt.clone(),
        token: "site-token".into(),
        thread: thread_id.clone(),
        turn,
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
                &json!({}),
                &Value::Null,
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
        run_id: run,
        stop: tokio_util::sync::CancellationToken::new(),
        nudge: Arc::new(tokio::sync::Notify::new()),
        foreground: Arc::new(crate::control::foreground::ForegroundSlot::default()),
        turn: Arc::new(tokio::sync::Mutex::new(None)),
    };
    let mutex = tokio::sync::Mutex::new(());
    let mut boundary = crate::compaction::boundary::Boundary::new(&mutex, mutex.lock().await);
    let work = summary_stage(
        &control,
        &control.compactor,
        &thread_id,
        &slot,
        active,
        &mut boundary,
        &automatic,
    );
    let answer_summary = async {
        let request = incoming(&control, &mut receive).await;
        assert_eq!(request.body["tools"], json!([]));
        request
            .reply
            .send((200, answer("Short site summary.")))
            .unwrap();
    };
    let (result, ()) = tokio::join!(work, answer_summary);
    assert_eq!(
        result.unwrap_err().code,
        ErrorCode::SaveFailed,
        "the checkpoint fault follows the commit"
    );
    assert!(reached.load(Ordering::SeqCst));
    drop(boundary);
    let facts: Value = control.store.database.call({
        let attempt = attempt.clone();
        move |db| Ok(serde_json::from_str(&db.query_row("SELECT json_extract(error_json,'$.automatic_trim_attempt') FROM model_attempt WHERE id=?1", [attempt], |r| r.get::<_, String>(0))?).unwrap())
    }).await.unwrap();
    assert_eq!(facts["outcome"], "unknown");
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
    drop(held);
    let _ = control.shutdown().await;
    owner.shutdown().await.unwrap();
    server.abort();
}
