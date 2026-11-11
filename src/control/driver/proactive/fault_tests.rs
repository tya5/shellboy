//! Post-adoption data safety, independent of best-effort attempt diagnostics.
use super::*;
use crate::app::DataOwner;
use crate::protocol::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

mod record_tests;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    Storage,
    Cancellation,
    Remeasurement,
    Cleanup,
    /// #226: after the checkpoint every attempt-record and automatic app_meta
    /// write is rejected, so only the stage facts and provisional target remain.
    Persistent,
    /// #226: the real exception write (W2) is rejected after a real cleanup.
    RejectException,
    /// #226: the real final write (W4) is rejected; the attempt runs to its end.
    RejectFinal,
    /// #226: the real guard write (W3) is rejected; the attempt runs to its end.
    RejectGuard,
    /// #226: the stage fact itself is rejected before the stage adopts.
    RejectFact,
}
impl Fault {
    /// The attempt continues after the checkpoint instead of failing at it.
    fn continues(self) -> bool {
        matches!(
            self,
            Fault::RejectFinal | Fault::RejectGuard | Fault::RejectFact
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_storage_boundaries() {
    boundaries(Fault::Storage).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_cancellation_boundaries() {
    boundaries(Fault::Cancellation).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_remeasurement_boundaries() {
    boundaries(Fault::Remeasurement).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_cleanup_boundaries() {
    boundaries(Fault::Cleanup).await;
}

async fn boundaries(fault: Fault) {
    crate::test_watchdog::watchdog("automatic postcommit data safety", async {
        for active in [false, true] {
            for summary in [false, true] {
                boundary(fault, active, summary).await;
            }
        }
    })
    .await
    .unwrap();
}

async fn prepare(fault: Fault, active: bool, summary: bool) -> Fx {
    eprintln!("automatic boundary {fault:?} active={active} summary={summary}");
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
        incoming(&control, &mut receive).await.reply.send((200, json!({"id":format!("boundary-{index}"),"status":"completed","usage":{"input_tokens":100,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("boundary-{index}"),"name":"shell","arguments":json!({"command":command}).to_string()}]}))).unwrap();
    }
    let request_boundary = incoming(&control, &mut receive).await;
    let reached = Arc::new(AtomicBool::new(false));
    let stage = match (active, summary) {
        (false, false) => "closed_spill",
        (false, true) => "closed_summary",
        (true, false) => "active_spill",
        (true, true) => "active_summary",
    };
    *control.automatic_fault.lock().unwrap() = Some(crate::control::AutomaticFault {
        stage,
        kind: match fault {
            Fault::Cancellation => 0,
            Fault::Storage => 1,
            Fault::Remeasurement => 2,
            Fault::Cleanup => 3,
            Fault::Persistent => 4,
            Fault::RejectException => 5,
            Fault::RejectFinal => 6,
            Fault::RejectGuard => 7,
            Fault::RejectFact => 8,
        },
        reached: reached.clone(),
    });
    let kind = if summary { "summary" } else { "spill" };
    let scope = if active && summary {
        " AND json_extract(n.payload_json,'$.summary_scope')='active_turn'"
    } else {
        ""
    };
    // Capture immutable witnesses in the adoption transaction. Fault injection
    // itself runs after COMMIT, before the next driver stage.
    // The epoch update is later in that SAME transaction; capture it separately.
    control.store.database.call(move |db| {
        db.execute_batch(&format!(r#"
            CREATE TABLE fault_boundary(path TEXT, revision INTEGER, epoch INTEGER);
            CREATE TABLE fault_initial(epoch INTEGER);
            CREATE TRIGGER fault_start AFTER INSERT ON app_meta
            WHEN NEW.key GLOB 'compact.automatic_active.*' AND NOT EXISTS(SELECT 1 FROM fault_initial)
            BEGIN INSERT INTO fault_initial SELECT reasoning_epoch FROM thread
              WHERE 'compact.automatic_active.'||id=NEW.key; END;
            CREATE TABLE fault_originals AS SELECT * FROM node;
            CREATE TABLE fault_nodes AS SELECT * FROM node WHERE 0;
            CREATE TABLE fault_replacements AS SELECT * FROM replacement WHERE 0;
            CREATE TABLE fault_sources AS SELECT * FROM replacement_source WHERE 0;
            CREATE TRIGGER fault_adopt AFTER UPDATE OF selection_revision ON thread
            WHEN NOT EXISTS(SELECT 1 FROM fault_boundary)
            AND EXISTS(SELECT 1 FROM app_meta WHERE key='compact.automatic_active.'||NEW.id)
            AND EXISTS(SELECT 1 FROM thread_path p JOIN node n ON n.id=p.node_id
                       WHERE p.thread_id=NEW.id AND n.kind='{kind}'{scope})
            BEGIN
              INSERT INTO fault_boundary SELECT
                (SELECT json_group_array(node_id) FROM (SELECT node_id FROM thread_path WHERE thread_id=NEW.id ORDER BY ordinal)),
                NEW.selection_revision, NEW.reasoning_epoch;
              INSERT INTO fault_nodes SELECT * FROM node;
              INSERT INTO fault_replacements SELECT * FROM replacement;
              INSERT INTO fault_sources SELECT * FROM replacement_source;
            END;
            CREATE TRIGGER fault_epoch AFTER UPDATE OF reasoning_epoch ON thread
            WHEN EXISTS(SELECT 1 FROM fault_boundary)
            BEGIN UPDATE fault_boundary SET epoch=NEW.reasoning_epoch; END;
        "#))?;
        Ok(())
    }).await.unwrap();
    // #226: faults that must precede the stage are installed before it runs.
    let (hit_flag, stage_name) = (reached.clone(), stage);
    if fault == Fault::RejectFact {
        control.store.database.call(move |db| {
            db.create_scalar_function("fault_fact_hit", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
                hit_flag.store(true, Ordering::SeqCst);
                Ok(1i64)
            })?;
            db.execute_batch(&format!("CREATE TEMP TRIGGER fault_fact BEFORE UPDATE OF error_json ON model_attempt
                WHEN json_type(NEW.error_json,'$.automatic_trim_attempt.committed_stages.{stage_name}')='object'
                BEGIN SELECT fault_fact_hit(); SELECT RAISE(ABORT,'injected stage fact rejection'); END;"))?;
            Ok(())
        }).await.unwrap();
    }
    let guard_before = guard_rows(&control).await;
    request_boundary.reply.send((200, json!({"id":"boundary-small","status":"completed","usage":{"input_tokens":if active {200} else {100},"output_tokens":1},"output":[{"type":"function_call","call_id":"boundary-small","name":"shell","arguments":json!({"command":"echo last"}).to_string()}]}))).unwrap();
    if !active {
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("done", 200)))
            .unwrap();
        settled(&control, &target).await;
        send(&control, &target, "next turn").await;
    }
    if summary {
        let summary_request = incoming(&control, &mut receive).await;
        assert_eq!(summary_request.body["tools"], json!([]));
        summary_request
            .reply
            .send((200, answer("Short adopted summary.")))
            .unwrap();
    }
    loop {
        if reached.load(Ordering::SeqCst) {
            break;
        }
        if let Ok(prefix) = receive.try_recv() {
            assert!(
                active || fault.continues(),
                "closed boundary must fail before another request: {}",
                prefix
                    .body
                    .to_string()
                    .chars()
                    .take(600)
                    .collect::<String>()
            );
            assert_eq!(prefix.body["tools"], json!([]));
            prefix
                .reply
                .send((200, answer("Short prefix summary.")))
                .unwrap();
        }
        tokio::task::yield_now().await;
    }
    let shutdown = control.shutdown().await;
    if fault != Fault::Cancellation {
        assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
    } else {
        shutdown.unwrap();
    }
    assert!(
        receive.try_recv().is_err(),
        "no provider continuation after failure"
    );
    assert!(!control.compactor.fail_remeasurement.load(Ordering::SeqCst));
    Fx {
        root,
        url,
        limits,
        owner,
        control,
        target,
        receive,
        server,
        guard_before,
    }
}

struct Fx {
    root: tempfile::TempDir,
    url: String,
    limits: &'static str,
    owner: DataOwner,
    control: Arc<Control>,
    target: String,
    receive: mpsc::Receiver<Incoming>,
    server: tokio::task::JoinHandle<()>,
    guard_before: Vec<(String, String)>,
}

/// Persisted #180 guard and status rows, byte-exact.
async fn guard_rows(control: &Control) -> Vec<(String, String)> {
    control.store.database.call(|db| {
        Ok(db.prepare("SELECT key,value FROM app_meta WHERE key GLOB 'compact.automatic_guard.*' OR key GLOB 'compact.automatic_status.*' ORDER BY key")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap()
}

async fn boundary(fault: Fault, active: bool, summary: bool) {
    let Fx {
        root,
        url,
        limits,
        owner,
        control,
        target,
        mut receive,
        server,
        guard_before: _,
    } = prepare(fault, active, summary).await;
    if fault == Fault::Cleanup {
        let id = target.clone();
        control
            .store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM app_meta WHERE key='compact.automatic_active.'||?1",
                        [id],
                        |r| r.get::<_, i64>(0)
                    )?,
                    1,
                    "real cleanup write was rejected"
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    assert_boundary(&control, &target, active, summary, "shutdown").await;
    control
        .store
        .database
        .call(|db| {
            db.execute_batch(
                "DROP TRIGGER fault_adopt; DROP TRIGGER fault_epoch; DROP TRIGGER fault_start;
            DROP TRIGGER IF EXISTS fault_cleanup;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // Reopen through normal startup repair, not by patching run/turn states.
    drop(control);
    owner.shutdown().await.unwrap();
    let owner = proactive_owner(root.path(), &url, limits).await;
    let control = Control::new(&owner, root.path().into());
    assert_boundary(&control, &target, active, summary, "reopen").await;
    let id = target.clone();
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key='compact.automatic_active.'||?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    // The synthetic protected user exceeds the ordinary 64K segment baseline.
    // Use the public manual whole-group summary operation on one replacement,
    // without altering the saved user, widening limits, or bypassing validation.
    let id = target.clone();
    let node = control.store.database.call(move |db| Ok(db.query_row(
        "SELECT p.node_id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind IN ('spill','summary') ORDER BY p.ordinal LIMIT 1",
        [id], |r| r.get::<_, String>(0))?)).await.unwrap();
    let edit = ContextEditArgs {
        thread_id: Some(target.clone()),
        action: ContextEditAction::Summary,
        from_node_id: Some(node.clone()),
        to_node_id: Some(node),
        max_chars: None,
        max_percent: None,
        segment_chars: None,
        reduce_rounds: None,
    };
    // Normal Control admission, queue and Thread boundary, not a direct edit.
    let accepted = request(&control, Command::ContextEdit(edit)).await;
    let operation = accepted["operation_id"].as_str().unwrap().to_owned();
    let mut requests = 0;
    let result = loop {
        tokio::select! {
            status = request(&control, Command::OperationGet(OperationTarget { operation_id: operation.clone() })) => {
                assert_ne!(status["downstream"]["state"], "failed", "{status}");
                if status["downstream"]["state"] == "completed" {
                    break status["downstream"]["outcome"]["result"].clone();
                }
                tokio::task::yield_now().await;
            }
            request = receive.recv() => {
                let request = request.expect("loopback remains available");
                assert_eq!(request.body["tools"], json!([]));
                requests += 1;
                request.reply.send((200, answer("ok"))).unwrap();
            }
        }
    };
    assert!(
        requests > 0,
        "exercise the next summary, including map/reduce if needed"
    );
    assert!(
        result["compact"]["no_target"] == false,
        "next compaction must actually adopt, not silently skip"
    );
    assert!(result["compact"]["summary_node_id"].is_string(), "{result}");
    assert!(result["compact"]["source_nodes"].as_u64().unwrap() > 0);
    assert!(
        result["after_characters"].as_u64().unwrap()
            < result["before_characters"].as_u64().unwrap()
    );
    assert_immutable(&control).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

async fn assert_immutable(control: &Control) {
    control.store.database.call(|db| {
        // Exact row comparison protects originals, replacement records and ordered
        // provenance, including hashes/revisions, even after another compaction.
        for (saved, current) in [("fault_originals", "node"), ("fault_nodes", "node"), ("fault_replacements", "replacement"), ("fault_sources", "replacement_source")] {
            let missing: i64 = db.query_row(&format!("SELECT count(*) FROM (SELECT * FROM {saved} EXCEPT SELECT * FROM {current})"), [], |r| r.get(0))?;
            assert_eq!(missing, 0, "immutable rows changed: {current}");
        }
        let replacements: Vec<(String, String, String)> = db.prepare("SELECT r.id,n.kind,n.payload_json FROM replacement r JOIN node n ON n.id=r.output_node_id")?.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (replacement, kind, body) in replacements {
            let payload: Value = serde_json::from_str(&body).unwrap();
            let sources: Vec<String> = db.prepare("SELECT source_node_id FROM replacement_source WHERE replacement_id=?1 ORDER BY ordinal")?.query_map([replacement], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            assert_eq!(payload["source_node_ids"], json!(sources), "payload and ordered provenance describe the same complete replacement");
            if kind == "spill" { assert_eq!(sources.len(), 2, "one whole call/result group"); }
        }
        assert_eq!(db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0))?, 0);
        assert_eq!(db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?, "ok");
        Ok(())
    }).await.unwrap();
}

async fn assert_boundary(
    control: &Control,
    target: &str,
    active: bool,
    summary: bool,
    phase: &'static str,
) {
    assert_immutable(control).await;
    let target = target.to_owned();
    control.store.database.call(move |db| {
        let (path, revision, epoch): (String, i64, i64) = db.query_row("SELECT path,revision,epoch FROM fault_boundary", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let (current_revision, current_epoch, cursor): (i64, i64, String) = db.query_row("SELECT selection_revision,reasoning_epoch,cursor_node_id FROM thread WHERE id=?1", [&target], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let current_path: Vec<String> = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?.query_map([&target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let committed_path: Vec<String> = serde_json::from_str(&path).unwrap();
        assert!(current_path.len() >= committed_path.len());
        assert_eq!(&current_path[..committed_path.len()], committed_path, "committed selection is preserved exactly");
        let appended = &current_path[committed_path.len()..];
        assert_interruption_audits(db, appended, phase, committed_path.len())?;
        assert_eq!(current_path.last(), Some(&cursor));
        assert_eq!(current_revision, revision + appended.len() as i64, "only validated audit append commits advance selection_revision");
        assert_eq!(current_epoch, epoch, "committed reasoning epoch survives failure/restart");
        let initial_epoch: i64 = db.query_row("SELECT epoch FROM fault_initial", [], |r| r.get(0))?;
        assert_eq!(epoch, initial_epoch + 1 + i64::from(summary), "each committed stage advances the fixture's reasoning epoch once");
        let kinds: Vec<String> = db.prepare("SELECT n.kind FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind IN ('spill','summary') ORDER BY p.ordinal")?.query_map([&target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let expected = match (active, summary) {
            (_, false) => vec!["spill", "spill"],
            (false, true) => vec!["summary", "spill", "spill"],
            (true, true) => vec!["summary"],
        };
        assert_eq!(kinds, expected, "all intended replacements, not a partial group");
        let incomplete: i64 = db.query_row("SELECT count(*) FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id WHERE tr.thread_id=?1 AND (r.state!='completed' OR NOT EXISTS(SELECT 1 FROM replacement_source s WHERE s.replacement_id=r.id) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=r.output_node_id AND n.payload_json IS NOT NULL))", [&target], |r| r.get(0))?;
        assert_eq!(incomplete, 0);
        let missing: i64 = db.query_row("SELECT count(*) FROM replacement_source s LEFT JOIN node n ON n.id=s.source_node_id WHERE n.id IS NULL OR n.payload_json IS NULL OR n.payload_revision!=s.source_revision", [], |r| r.get(0))?;
        assert_eq!(missing, 0);
        Ok(())
    }).await.unwrap();
}

// Per-Control, single-use synchronization point: the adoption future has returned
// (including its epoch write and COMMIT), and no subsequent stage has begun.
pub(super) async fn checkpoint(
    control: &Arc<Control>,
    slot: &DriverSlot,
    stage: &str,
) -> Result<(), ApiError> {
    let injection = {
        let mut pending = control.automatic_fault.lock().unwrap();
        if pending.as_ref().is_some_and(|p| p.stage == stage) {
            pending.take()
        } else {
            None
        }
    };
    let Some(injection) = injection else {
        return Ok(());
    };
    assert!(
        !slot.stop.is_cancelled(),
        "faults must be independent of prior cancellation"
    );
    let result = match injection.kind {
        0 => {
            slot.stop.cancel();
            assert!(slot.stop.is_cancelled(), "token is cancelled before the next stage");
            Ok(())
        }
        1 => control.store.database.call(|db| {
            db.execute_batch("CREATE TEMP TABLE rejected_write(value INTEGER);
                CREATE TEMP TRIGGER reject_write BEFORE INSERT ON rejected_write
                BEGIN SELECT RAISE(ABORT,'injected independent postcommit storage write failure'); END;")?;
            db.execute("INSERT INTO rejected_write VALUES(1)", [])?;
            Ok(())
        }).await.map_err(storage_error),
        2 => {
            control.compactor.fail_remeasurement.store(true, Ordering::SeqCst);
            Ok(())
        }
        3 => {
            let hit = injection.reached.clone();
            control.store.database.call(move |db| {
                db.create_scalar_function("fault_cleanup_hit", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
                    hit.store(true, Ordering::SeqCst);
                    Ok(1i64)
                })?;
                db.execute_batch("CREATE TEMP TRIGGER fault_cleanup BEFORE DELETE ON app_meta
                    WHEN OLD.key GLOB 'compact.automatic_active.*'
                    BEGIN SELECT fault_cleanup_hit(); SELECT RAISE(ABORT,'injected actual cleanup write failure'); END;")?;
                Ok(())
            }).await.map_err(storage_error)?;
            // A non-storage boundary error routes to the REAL unconditional
            // cleanup. Only its DELETE fails with SaveFailed; no stop is injected.
            Err(ApiError::new(ErrorCode::InvalidState, "injected boundary error before cleanup"))
        }
        4 => control.store.database.call(|db| {
            db.execute_batch("CREATE TEMP TRIGGER persistent_attempt BEFORE UPDATE OF error_json ON model_attempt
                WHEN json_type(NEW.error_json,'$.automatic_trim_attempt')='object'
                BEGIN SELECT RAISE(ABORT,'injected persistent attempt write failure'); END;
                CREATE TEMP TRIGGER persistent_insert BEFORE INSERT ON app_meta WHEN NEW.key GLOB 'compact.automatic_*'
                BEGIN SELECT RAISE(ABORT,'injected persistent meta write failure'); END;
                CREATE TEMP TRIGGER persistent_update BEFORE UPDATE ON app_meta WHEN OLD.key GLOB 'compact.automatic_*'
                BEGIN SELECT RAISE(ABORT,'injected persistent meta write failure'); END;
                CREATE TEMP TRIGGER persistent_delete BEFORE DELETE ON app_meta WHEN OLD.key GLOB 'compact.automatic_*'
                BEGIN SELECT RAISE(ABORT,'injected persistent meta write failure'); END;
                CREATE TEMP TABLE rejected_write(value INTEGER);
                CREATE TEMP TRIGGER reject_write BEFORE INSERT ON rejected_write
                BEGIN SELECT RAISE(ABORT,'injected independent postcommit storage write failure'); END;")?;
            db.execute("INSERT INTO rejected_write VALUES(1)", [])?;
            Ok(())
        }).await.map_err(storage_error),
        5..=7 => {
            let hit = injection.reached.clone();
            let kind = injection.kind;
            let thread = control.store.database.call(|db| Ok(db.query_row("SELECT id FROM thread LIMIT 1", [], |r| r.get::<_, String>(0))?)).await.map_err(storage_error)?;
            control.store.database.call(move |db| {
                db.create_scalar_function("fault_site_hit", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
                    hit.store(true, Ordering::SeqCst);
                    Ok(1i64)
                })?;
                let abort = "BEGIN SELECT fault_site_hit(); SELECT RAISE(ABORT,'injected production write rejection'); END;";
                if kind == 7 {
                    // Seed a status row so the real clearing DELETE has a row to reject.
                    db.execute("INSERT INTO app_meta(key,value) VALUES('compact.automatic_status.'||?1,'seed')", [&thread])?;
                    db.execute_batch(&format!("CREATE TEMP TRIGGER fault_site BEFORE INSERT ON app_meta WHEN NEW.key GLOB 'compact.automatic_guard.*' {abort}
                        CREATE TEMP TRIGGER fault_site_delete BEFORE DELETE ON app_meta WHEN OLD.key GLOB 'compact.automatic_status.*' OR OLD.key GLOB 'compact.automatic_guard.*' {abort}
                        CREATE TEMP TRIGGER fault_site_update BEFORE UPDATE ON app_meta WHEN OLD.key GLOB 'compact.automatic_guard.*' {abort}"))?;
                } else {
                    let when = if kind == 5 {
                        "json_extract(NEW.error_json,'$.automatic_trim_attempt.outcome')='exception'"
                    } else {
                        "json_extract(NEW.error_json,'$.automatic_trim_attempt.outcome') NOT IN ('unknown','exception')"
                    };
                    db.execute_batch(&format!("CREATE TEMP TRIGGER fault_site BEFORE UPDATE OF error_json ON model_attempt WHEN {when} {abort}"))?;
                }
                Ok(())
            }).await.map_err(storage_error)?;
            if injection.kind == 5 {
                // A storage failure takes the real exception path (W2).
                control.store.database.call(|db| {
                    db.execute_batch("CREATE TEMP TABLE rejected_write(value INTEGER);
                        CREATE TEMP TRIGGER reject_write BEFORE INSERT ON rejected_write
                        BEGIN SELECT RAISE(ABORT,'injected independent postcommit storage write failure'); END;")?;
                    db.execute("INSERT INTO rejected_write VALUES(1)", [])?;
                    Ok(())
                }).await.map_err(storage_error)
            } else {
                Ok(())
            }
        }
        8 => Ok(()),
        _ => unreachable!(),
    };
    if injection.kind != 0 {
        assert!(
            !slot.stop.is_cancelled(),
            "storage faults do not depend on cancellation"
        );
    }
    if matches!(injection.kind, 1 | 4) {
        assert_eq!(result.as_ref().unwrap_err().code, ErrorCode::SaveFailed);
    }
    if !matches!(injection.kind, 3 | 5..=8) {
        injection.reached.store(true, Ordering::SeqCst);
    }
    result
}

async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
fn answer(text: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]})
}
// Fixture guards detect deadlocks, not product latency. Capture only bounded
// saved lifecycle facts on failure; request bodies and error messages stay out.
async fn progress(control: &Control) -> Result<Value, crate::storage::StorageError> {
    control.store.database.call(|db| {
        let mut state = serde_json::Map::new();
        for (name, query) in [
            ("runs", "SELECT json_object('id',substr(id,1,128),'thread_id',substr(thread_id,1,128),'state',substr(state,1,64),'stop_reason',substr(stop_reason,1,128)) FROM run ORDER BY rowid DESC LIMIT 8"),
            ("turns", "SELECT json_object('id',substr(t.id,1,128),'thread_id',substr(t.thread_id,1,128),'run_id',substr(t.run_id,1,128),'state',substr(t.state,1,64),'generation',t.generation,'instruction_snapshot_present',t.instruction_snapshot_id IS NOT NULL,'run_state',substr(r.state,1,64),'recovery_code',substr(json_extract(t.recovery_reason_json,'$.code'),1,128),'attempt_state',substr(a.state,1,64),'error_kind',substr(json_extract(a.error_json,'$.error.kind'),1,128),'http_phase',substr(json_extract(a.error_json,'$.error.http_observation.phase'),1,128)) FROM turn t JOIN run r ON r.id=t.run_id LEFT JOIN model_attempt a ON a.id=(SELECT latest.id FROM model_attempt latest WHERE latest.turn_id=t.id ORDER BY latest.rowid DESC LIMIT 1) ORDER BY t.rowid DESC LIMIT 8"),
        ] {
            let rows = db.prepare(query)?.query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            state.insert(name.into(), json!(rows));
        }
        Ok(Value::Object(state))
    }).await
}
#[track_caller]
fn incoming<'a>(
    control: &'a Control,
    receive: &'a mut mpsc::Receiver<Incoming>,
) -> impl std::future::Future<Output = Incoming> + 'a {
    let caller = std::panic::Location::caller();
    async move {
        match crate::test_watchdog::watchdog("tests/runtime_features.rs: incoming", receive.recv())
            .await
        {
            Ok(Some(value)) => value,
            result => {
                let diagnostic = crate::test_watchdog::watchdog(
                    "tests/runtime_features.rs: incoming",
                    progress(control),
                )
                .await;
                panic!(
                    "loopback request watchdog at {}:{}; receive={}; saved lifecycle facts={diagnostic:?}",
                    caller.file(),
                    caller.line(),
                    if result.is_err() {
                        "timeout"
                    } else {
                        "sender closed"
                    }
                );
            }
        }
    }
}
#[track_caller]
fn settled<'a>(
    control: &'a Control,
    thread: &'a str,
) -> impl std::future::Future<Output = ()> + 'a {
    let caller = std::panic::Location::caller();
    let mut events = control.subscribe();
    async move {
        let result = crate::test_watchdog::watchdog("tests/runtime_features.rs: settled", async {
            loop {
                let target = thread.to_owned();
                if control.store.database.call(move |db| Ok(db.query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting'))",
                    [target], |row| row.get::<_, bool>(0),
                )?)).await.unwrap() {
                    break;
                }
                loop {
                    match events.recv().await {
                        Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!(
                            "settlement event stream closed at {}:{} for Thread {thread}",
                            caller.file(), caller.line()
                        ),
                        _ => {}
                    }
                }
            }
        }).await;
        if result.is_err() {
            let diagnostic = crate::test_watchdog::watchdog(
                "tests/runtime_features.rs: settled",
                progress(control),
            )
            .await;
            panic!(
                "Thread {thread} did not settle within the fixture watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                caller.file(),
                caller.line()
            );
        }
    }
}
async fn thread(control: &Arc<Control>) -> String {
    request(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .into()
}
async fn send(control: &Arc<Control>, thread: &str, text: &str) {
    request(
        control,
        Command::MessageSend(MessageSendArgs {
            resume_recovery: false,
            human: false,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.into()),
            text: text.into(),
        }),
    )
    .await;
}

async fn proactive_owner(root: &std::path::Path, url: &str, extra: &str) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n{extra}")).unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
fn answer_with_usage(text: &str, input_tokens: u64) -> Value {
    let mut body = answer(text);
    body["usage"] =
        json!({"input_tokens":input_tokens,"output_tokens":1,"total_tokens":input_tokens+1});
    body
}

struct Incoming {
    body: Value,
    reply: oneshot::Sender<(u16, Value)>,
}
async fn proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel(8);
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                let body = loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(at) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let size: usize = String::from_utf8_lossy(&bytes[..at])
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|s| s.trim().parse().ok())
                            })
                            .unwrap();
                        if bytes.len() >= at + 4 + size {
                            break serde_json::from_slice(&bytes[at + 4..at + 4 + size]).unwrap();
                        }
                    }
                };
                let (reply, receive) = oneshot::channel();
                if send.send(Incoming { body, reply }).await.is_err() {
                    return;
                }
                if let Ok((status, body)) = receive.await {
                    let body = body.to_string();
                    let _ = stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
                }
            });
        }
    });
    (format!("http://{address}/v1"), receive, server)
}

fn assert_interruption_audits(
    db: &rusqlite::Connection,
    appended: &[String],
    phase: &'static str,
    captured_prefix_len: usize,
) -> Result<(), crate::storage::StorageError> {
    let sections = [
        "runs",
        "turns",
        "shells",
        "tool_calls",
        "unreflected_messages",
        "pending_operations_with_unknown_targets",
    ];
    let mut details = std::collections::BTreeMap::new();
    let mut reasons = std::collections::BTreeSet::new();
    for (offset, id) in appended.iter().enumerate() {
        let (kind, turn, at, body): (String, Option<String>, String, String) = db.query_row(
            "SELECT kind,turn_id,created_at,payload_json FROM node WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        assert!(turn.is_none(), "postcommit suffix metadata: {}", {
            // Failure-only closed metadata: never print node/Turn IDs, payload,
            // paths, or provider fields. Unknown kinds/states are not forwarded.
            let node_kind = match kind.as_str() {
                "user"
                | "message"
                | "model_output"
                | "tool_call"
                | "tool_result"
                | "summary"
                | "spill"
                | "interruption_context"
                | "interruption_details" => kind.as_str(),
                _ => "other",
            };
            let metadata=db.query_row(
                "SELECT r.state,t.generation FROM turn t JOIN run r ON r.id=t.run_id WHERE t.id=?1",
                [turn.as_deref()],|r|Ok((r.get(0)?,r.get(1)?)),
            ).optional();
            let query_failed = metadata.is_err();
            let (run_state, generation): (Option<String>, Option<i64>) =
                metadata.unwrap_or(None).unwrap_or((None, None));
            let run_state = match run_state.as_deref() {
                Some("live" | "stopping" | "completed" | "cancelled" | "failed") => {
                    run_state.as_deref().unwrap()
                }
                _ if query_failed => "query_failed",
                _ => "other_or_missing",
            };
            json!({"node_kind":node_kind,"turn_present":turn.is_some(),"ordinal":captured_prefix_len+offset,"phase":phase,"run_state":run_state,"turn_generation":generation,"captured_prefix_len":captured_prefix_len})
        });
        let payload: Value = serde_json::from_str(&body).unwrap();
        let audit: Value = serde_json::from_str(payload["text"].as_str().unwrap()).unwrap();
        match kind.as_str() {
            "interruption_details" => {
                assert!(sections.contains(&audit["section"].as_str().unwrap()));
                assert!(!audit["items"].as_array().unwrap().is_empty());
                details.insert(
                    id.clone(),
                    (audit["section"].clone(), at, audit["items"].clone()),
                );
            }
            "interruption_context" => {
                assert_eq!(audit["snapshot_node_id"], *id);
                assert_eq!(audit["observed_at"], at);
                let reason = audit["kind"].as_str().unwrap();
                let source = match reason {
                    "app_shutdown" => "shutdown",
                    "previous_instance_interrupted" => "startup",
                    _ => panic!("unexpected audit reason {reason}"),
                };
                assert!(reasons.insert(reason.to_owned()));
                assert_eq!(audit["actor"], json!({"kind":"shellboy","source":source}));
                assert_eq!(audit["change"]["reason"], reason);
                assert_eq!(audit["change"]["automatic_replay"], false);
                assert_eq!(audit["change"]["result_uncertain"], true);
                let mut referenced = std::collections::BTreeSet::new();
                for section in sections {
                    for detail in audit["change"][section]["detail_node_ids"]
                        .as_array()
                        .unwrap()
                    {
                        let detail = detail.as_str().unwrap();
                        assert!(referenced.insert(detail.to_owned()));
                        let (actual_section, actual_at, items) = details
                            .get(detail)
                            .expect("only preceding audit details allowed");
                        assert_eq!(actual_section, section);
                        assert_eq!(actual_at, &at);
                        assert_eq!(items, &audit["change"][section]["items"]);
                    }
                }
                assert_eq!(
                    referenced,
                    details.keys().cloned().collect(),
                    "no unreferenced suffix nodes"
                );
                details.clear();
            }
            _ => panic!("unexpected selected suffix kind {kind}"),
        }
    }
    assert!(details.is_empty(), "no incomplete audit batch");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_trigger_real_profile_rejection_is_visible_in_status_and_log() {
    crate::test_watchdog::watchdog("completed trigger discard pipeline", async {
        let root=tempfile::tempdir().unwrap();
        let (url,mut receive,server)=proxy().await;
        let owner=proactive_owner(root.path(),&url,"[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n").await;
        let control=Control::new(&owner,root.path().into());
        let target=thread(&control).await;
        send(&control,&target,"Original synthetic request PRIVATE_SENTINEL").await;
        incoming(&control,&mut receive).await.reply.send((200,answer_with_usage("completed first answer",100))).unwrap();
        settled(&control,&target).await;
        send(&control,&target,"next synthetic request").await;
        incoming(&control,&mut receive).await.reply.send((200,answer_with_usage("completed second answer",200))).unwrap();
        settled(&control,&target).await;
        send(&control,&target,"trigger automatic compaction").await;
        let summary=incoming(&control,&mut receive).await;
        assert_eq!(summary.body["tools"],json!([]));
        let scope=target.clone();
        let (attempt,usage,response)=control.store.database.call(move |db| {
            let row=db.query_row("SELECT a.id,a.usage_json,a.response_id FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND a.state='completed' AND json_extract(a.error_json,'$.automatic_trim_attempt.token') IS NOT NULL ORDER BY a.rowid DESC LIMIT 1",[&scope],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?)))?;
            // Real captured-input invalidation, not a forged rejection/error body.
            db.execute("UPDATE thread SET profile_revision=profile_revision+1 WHERE id=?1",[scope])?;
            Ok(row)
        }).await.unwrap();
        summary.reply.send((200,answer("short synthetic summary"))).unwrap();
        let expected=attempt.clone();
        crate::test_watchdog::watchdog("discard durable publication",async {
            loop {
                let id=expected.clone();
                let saved=control.store.database.call(move |db|Ok(db.query_row("SELECT json_extract(error_json,'$.automatic_trim_attempt.discard.reason') FROM model_attempt WHERE id=?1",[id],|r|r.get::<_,Option<String>>(0))?)).await.unwrap();
                if saved.as_deref()==Some("profile_changed") {break;}
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let scope=target.clone();let id=attempt.clone();
        let status_fact=control.store.database.call(move |db| {
            let page=crate::status::snapshot(db,&StatusArgs{thread_id:Some(scope),all:true,limit:200,..Default::default()},"fixture").unwrap();
            let current=page.items.iter().find(|row|row.id==id).unwrap();
            let fact=current.compaction_discard.unwrap();
            assert_eq!(fact.reason,crate::compaction::discard::DiscardReason::ProfileChanged);
            assert_eq!(fact.phase,crate::compaction::discard::DiscardPhase::ClosedSummary);
            let point=crate::status::observation::prepare(crate::status::observation::read_status_tx(db,&[crate::status::observation::Key{kind:crate::status::observation::Kind::Attempt,id:id.clone()}],&mut Default::default()).unwrap());
            assert_eq!(point["items"][0]["current"]["compaction_discard"],serde_json::to_value(fact).unwrap());
            assert!(!point.to_string().contains("PRIVATE_SENTINEL"));
            let saved=db.query_row("SELECT state,usage_json,response_id FROM model_attempt WHERE id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?)))?;
            assert_eq!(saved,("completed".into(),usage,response));
            assert!(!serde_json::to_string(&page).unwrap().contains("PRIVATE_SENTINEL"));
            Ok(fact)
        }).await.unwrap();
        control.shutdown().await.unwrap();
        drop(control);owner.shutdown().await.unwrap();server.abort();
        let text=std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
        let facts=text.lines().map(|line|serde_json::from_str::<Value>(line).unwrap()).filter(|row|row["event"]=="compaction_discard").collect::<Vec<_>>();
        assert_eq!(facts.len(),1);
        assert_eq!(facts[0]["compaction_discard"]["reason"],"profile_changed");
        assert_eq!(facts[0]["ids"]["attempt_id"],attempt);
        assert_eq!(facts[0]["compaction_discard"],serde_json::to_value(status_fact).unwrap());
        assert!(!facts[0].to_string().contains("PRIVATE_SENTINEL"));
    }).await.unwrap();
}
