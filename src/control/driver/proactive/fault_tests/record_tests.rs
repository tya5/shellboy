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
record_test!(record_after_cancellation, Fault::Cancellation);
record_test!(record_after_remeasurement_fault, Fault::Remeasurement);
record_test!(record_after_cleanup_fault, Fault::Cleanup);
record_test!(record_after_persistent_fault, Fault::Persistent);
record_test!(
    record_after_rejected_exception_write,
    Fault::RejectException
);
record_test!(record_after_rejected_final_write, Fault::RejectFinal);
record_test!(record_after_rejected_guard_write, Fault::RejectGuard);
record_test!(rejected_stage_fact_prevents_adoption, Fault::RejectFact);

async fn record_boundaries(fault: Fault) {
    crate::test_watchdog::watchdog("automatic attempt record", async {
        for active in [false, true] {
            for summary in [false, true] {
                record_boundary(fault, active, summary).await;
            }
        }
    })
    .await
    .unwrap();
}

fn boundary_stage(active: bool, summary: bool) -> &'static str {
    match (active, summary) {
        (false, false) => "closed_spill",
        (false, true) => "closed_summary",
        (true, false) => "active_spill",
        (true, true) => "active_summary",
    }
}

struct Saved {
    attempt: String,
    record: Value,
}

async fn record_boundary(fault: Fault, active: bool, summary: bool) {
    let stage = boundary_stage(active, summary);
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
    } = prepare(fault, active, summary).await;
    let live = assert_record(&control, &target, fault, stage, &guard_before, true).await;
    drop_fault_triggers(&control).await;
    drop(control);
    owner.shutdown().await.unwrap();
    let owner = proactive_owner(root.path(), &url, limits).await;
    let control = Control::new(&owner, root.path().into());
    let after = assert_record(&control, &target, fault, stage, &guard_before, false).await;
    assert_eq!(
        live.record, after.record,
        "restart reconciles nothing and rewrites nothing"
    );
    later_attempt(&control, &mut receive, &target, &after).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

async fn drop_fault_triggers(control: &Control) {
    control
        .store
        .database
        .call(|db| {
            db.execute_batch(
                "DROP TRIGGER fault_adopt; DROP TRIGGER fault_epoch; DROP TRIGGER fault_start;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
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
                let witness: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key='compact.automatic_active.'||?1", [&target], |r| r.get(0))?;
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
    // The guard is untouched whenever the attempt's own guard write is not
    // reached or is rejected. A healthy W3 (cancellation, rejected final
    // write) legitimately saves its normal result before the failure.
    let mut guard_expected = guard_before.to_vec();
    if fault == Fault::RejectGuard {
        guard_expected.push((format!("compact.automatic_status.{target}"), "seed".into()));
    }
    if !matches!(fault, Fault::Cancellation | Fault::RejectFinal) {
        assert_eq!(
            guard_rows(control).await,
            guard_expected,
            "the #180 guard is exactly the pre-attempt value"
        );
    }
    let outcome = match fault {
        Fault::Cancellation => "adopted",
        Fault::Storage | Fault::Remeasurement | Fault::RejectFact => "exception",
        _ => "unknown",
    };
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
        (_, Fault::RejectFinal | Fault::RejectGuard) => keys.clone(),
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
    // Witness: only a rejected real cleanup leaves it; restart removes it.
    let retained = live && matches!(fault, Fault::Cleanup | Fault::Persistent);
    assert_eq!(
        witness,
        i64::from(retained),
        "witness {fault:?} live={live}"
    );
    // Pending target: provisional until a final write replaces it.
    if !facts.is_empty() {
        let pending: Value = serde_json::from_str(&pending.expect("pending target")).unwrap();
        assert_eq!(pending["attempt"], attempt.as_str());
        let last = keys.last().unwrap();
        let scope = match *last {
            "closed_spill" => "spill",
            "closed_summary" => "prefix",
            _ => "active_turn",
        };
        if fault == Fault::Cancellation {
            assert!(
                pending.get("provisional").is_none(),
                "final write replaced the provisional target"
            );
        } else {
            assert_eq!(pending["provisional"], true);
            assert_eq!(pending["scope"], scope);
            assert!(pending["marks"].is_null());
        }
    }
    if outcome == "unknown" && !live {
        let list = request(
            control,
            Command::CompactList(CompactListArgs {
                thread_id: Some(target.clone()),
                since: None,
                until: None,
                limit: 50,
            }),
        )
        .await;
        let item = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["attempt_id"] == attempt.as_str())
            .unwrap_or_else(|| panic!("{list}"));
        assert_eq!(item["kind"], "automatic_reduction");
        assert_eq!(item["state"], "unknown");
    }
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
        target_tokens: None,
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
    let order = Order {
        prefer_active: !active,
        middle_new: active,
        active_spill_new: false,
        ordinary_new: !active,
    };
    let (mut midturn, mut scope, mut shortfall) = (Value::Null, "", None);
    let mut stages = json!({"closed_spill":{"ran":false,"skipped":"not_reached"},"closed_summary":{"ran":false,"skipped":"not_reached"},"active_spill":{"ran":false,"skipped":"not_reached"},"active_summary":{"ran":false,"skipped":"not_reached"}});
    let mut committed = Vec::new();
    let work = summarize_once(
        &control,
        &thread_id,
        &slot,
        order,
        &mut midturn,
        &mut scope,
        None,
        &mut shortfall,
        &mut stages,
        &mut committed,
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
    assert_eq!(
        committed.len(),
        1,
        "the committed stage is kept for the cancellation record"
    );
    assert_eq!(stages[stage]["outcome"], "adopted");
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
