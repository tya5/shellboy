//! Durable path-admission facts and recovery inside existing Control transactions.
//! path_admission is bounded scalar metadata, like receipt.state/hold_reason;
//! logical receipt accounting continues to count sender/result payloads only.
//! Recovery context payloads use the existing event byte accounting.
use crate::{
    history,
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

const TERMINAL: &str = "(EXISTS(SELECT 1 FROM run r WHERE r.id=receipt.run_id AND r.state IN ('completed','stopped','failed')) OR EXISTS(SELECT 1 FROM turn u WHERE u.id=receipt.turn_id AND u.state IN ('completed','stopped','failed')))";

/// Call only at an actual message-delivery path boundary, not generic history selection.
pub(crate) fn mark_admitted_tx(
    db: &Connection,
    thread: &str,
    nodes: &[String],
) -> rusqlite::Result<()> {
    for node in nodes {
        let changed = db.execute("UPDATE receipt SET path_admission='admitted' WHERE target_thread_id=?1 AND message_node_id=?2 AND path_admission<>'admitted' AND EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=?2)", params![thread,node])?;
        if changed > 0 {
            super::reply::admission_notice_tx(db, thread, node)?;
        }
        let operation: Option<String> = db.query_row("SELECT client_operation_id FROM receipt WHERE target_thread_id=?1 AND message_node_id=?2",params![thread,node],|r|r.get(0)).optional()?;
        if let Some(operation) = operation {
            super::submission_retention::collect_operation(db, &operation)?;
        }
    }
    Ok(())
}

/// Preserve immediate message/notice order while retaining ordinary batch semantics.
pub(crate) fn append_admitted_tx(
    tx: &Transaction<'_>,
    thread: &str,
    revision: u64,
    nodes: &[String],
    at: &str,
) -> Result<(), StorageError> {
    if history::selection_revision(tx, thread, history::ThreadScope::Live)? as u64 != revision {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "selected path revision changed; discard late adoption",
        )
        .into());
    }
    let mut has_notice = false;
    for node in nodes {
        has_notice |= tx.query_row("SELECT EXISTS(SELECT 1 FROM receipt WHERE message_node_id=?1 AND (json_extract(result_json,'$.expect_reply')=1 OR json_extract(result_json,'$.reply_to') IS NOT NULL))", [node], |r| r.get::<_, bool>(0))?;
    }
    if has_notice {
        for node in nodes {
            let revision = history::selection_revision(tx, thread, history::ThreadScope::Live)?;
            history::append_existing_nodes(
                tx,
                thread,
                revision as u64,
                std::slice::from_ref(node),
                at,
            )?;
            mark_admitted_tx(tx, thread, std::slice::from_ref(node))?;
        }
    } else {
        history::append_existing_nodes(tx, thread, revision, nodes, at)?;
        mark_admitted_tx(tx, thread, nodes)?;
    }
    Ok(())
}

/// Shared startup, stop-drain and fallback classification. Repeated observations
/// preserve the first saved reason/time; no assignment or immutable node changes.
pub(crate) fn repair_terminal_tx(db: &Connection, at: &str) -> rusqlite::Result<()> {
    db.execute("UPDATE receipt SET state='cancelled',hold_reason='target_or_source_unavailable',updated_at=?1 WHERE state IN ('accepted','held') AND (NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL))",[at])?;
    db.execute("UPDATE receipt SET hold_reason='uncertain_path_admission',updated_at=?1 WHERE state='held' AND path_admission='unknown' AND run_id IS NULL AND turn_id IS NULL AND hold_reason IS NOT 'uncertain_path_admission'",[at])?;
    db.execute(&format!("UPDATE receipt SET state=CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END,hold_reason=CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END,updated_at=?1 WHERE state IN ('accepted','held') AND {TERMINAL} AND (state<>CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END OR hold_reason IS NOT CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END)"),[at])?;
    Ok(())
}

/// Repair only this recipient's oldest 128 rows needing a state change.
/// The global startup repair must never run inside foreground reply admission.
fn repair_thread_batch_tx(db: &Connection, thread: &str, at: &str) -> rusqlite::Result<()> {
    let terminal = format!("({TERMINAL} AND NOT EXISTS(SELECT 1 FROM run live_run WHERE live_run.id=receipt.run_id AND live_run.state IN ('live','stopping')))");
    let rows = db.prepare(&format!("SELECT id FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state IN ('accepted','held') AND ((NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL)) OR (state='held' AND path_admission='unknown' AND run_id IS NULL AND turn_id IS NULL AND hold_reason IS NOT 'uncertain_path_admission') OR ({terminal} AND (state<>CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END OR hold_reason IS NOT CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END))) ORDER BY created_at,rowid LIMIT 128"))?
        .query_map([thread], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for id in rows {
        db.execute(&format!("UPDATE receipt SET state=CASE WHEN NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL) THEN 'cancelled' WHEN {terminal} AND path_admission='admitted' THEN 'applied' ELSE 'held' END, hold_reason=CASE WHEN NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL) THEN 'target_or_source_unavailable' WHEN path_admission='unknown' THEN 'uncertain_path_admission' WHEN {terminal} AND path_admission='admitted' THEN 'terminal_after_path_admission' ELSE 'terminal_before_path_admission' END,updated_at=?2 WHERE id=?1"), params![id,at])?;
    }
    Ok(())
}

pub(super) fn interrupted_remaining(db: &Connection, thread: &str) -> rusqlite::Result<bool> {
    let terminal = format!("({TERMINAL} AND NOT EXISTS(SELECT 1 FROM run live_run WHERE live_run.id=receipt.run_id AND live_run.state IN ('live','stopping')))");
    db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='held' AND path_admission='unadmitted' AND ({terminal} OR (run_id IS NULL AND turn_id IS NULL AND hold_reason IN ('stopping','previous_instance_interrupted'))))"), [thread], |r| r.get(0))
}

/// Caller has established a new Run under human/live-owning-parent authority.
/// Existing message and receipt provenance stays fixed; only the selected path
/// and the original delivery outcome advance. All writes share send's transaction.
pub(crate) fn recover_on_resume_tx(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    actor: Value,
    at: &str,
) -> Result<usize, StorageError> {
    repair_thread_batch_tx(tx, thread, at)?;
    let terminal = format!("({TERMINAL} AND NOT EXISTS(SELECT 1 FROM run live_run WHERE live_run.id=receipt.run_id AND live_run.state IN ('live','stopping')))");
    // Merge interrupted and current recovery input before applying LIMIT: neither
    // source may overtake an older receipt from the other source.
    let mut q = tx.prepare(&format!("SELECT id,message_node_id,run_id,turn_id,COALESCE(run_id=?2 AND turn_id=?3 AND hold_reason='recovery_wait',0) FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='held' AND ((path_admission='unadmitted' AND ({terminal} OR (run_id IS NULL AND turn_id IS NULL AND hold_reason IN ('stopping','previous_instance_interrupted')))) OR (run_id=?2 AND turn_id=?3 AND hold_reason='recovery_wait')) ORDER BY created_at,rowid LIMIT 128"))?;
    let rows = q
        .query_map(params![thread,run,turn], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    // A reader's 128-row window is not an admission invariant. Each explicit
    // resume releases the oldest batch; later passes retain and drain the rest.
    if rows.is_empty() {
        return Ok(0);
    }
    for (receipt, node, old_run, old_turn, _) in &rows {
        if old_run.is_none() && old_turn.is_none() {
            // This is the existing first assignment, never a reassignment.
            tx.execute("UPDATE receipt SET run_id=?2,turn_id=?3,result_json=json_set(COALESCE(result_json,'{}'),'$.assignment','existing_turn','$.delivery_status','applied','$.run_id',?2,'$.turn_id',?3) WHERE id=?1 AND run_id IS NULL AND turn_id IS NULL",params![receipt,run,turn])?;
            tx.execute(
                "UPDATE node SET turn_id=?2 WHERE id=?1 AND turn_id IS NULL",
                params![node, turn],
            )?;
        }
    }
    let nodes = rows.iter().map(|row| row.1.clone()).collect::<Vec<_>>();
    let revision = history::selection_revision(tx, thread, history::ThreadScope::Live)?;
    append_admitted_tx(tx, thread, revision as u64, &nodes, at)?;
    for (receipt, _, _, _, current_recovery) in &rows {
        tx.execute("UPDATE receipt SET state=CASE WHEN ?3 THEN 'accepted' ELSE 'applied' END,hold_reason=CASE WHEN ?3 THEN NULL ELSE 'recovered_at_explicit_resume' END,updated_at=?2 WHERE id=?1 AND state='held' AND path_admission='admitted'", params![receipt,at,current_recovery])?;
    }
    let sources = rows.iter().map(|(receipt,node,old_run,old_turn,_)|json!({"receipt_id":receipt,"message_id":node,"original_run_id":old_run,"original_turn_id":old_turn})).collect::<Vec<_>>();
    super::context::record(
        tx,
        thread,
        "interrupted_messages_recovered",
        actor,
        json!({"run_id":run,"turn_id":turn,"sources":sources,"meaning":"Earlier interrupted instructions were admitted before the new message; original assignments remain unchanged"}),
        at,
    )?;
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','before'); INSERT INTO run(id,thread_id,state,created_at)VALUES('old','t','stopped','before'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('old-turn','t','old','stopped','before');").unwrap();
        db
    }
    fn input(db: &Connection, id: &str, admission: &str, at: &str) {
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'old-turn','user',?2,'{\"role\":\"user\",\"content\":[]}')",params![id,at]).unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,path_admission)VALUES(?1,?1,'null','t',?1,'old','old-turn','held',?2,?3)",params![id,at,admission]).unwrap();
    }
    fn state(db: &Connection, id: &str) -> (String, String, String) {
        db.query_row(
            "SELECT state,hold_reason,updated_at FROM receipt WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    #[test]
    fn terminal_repair_separates_admitted_unadmitted_unknown_and_preserves_first_observation() {
        let db = fixture();
        input(&db, "admitted", "admitted", "0");
        input(&db, "unseen", "unadmitted", "1");
        input(&db, "unknown", "unknown", "2");
        input(&db, "missing", "unadmitted", "3");
        db.execute(
            "UPDATE node SET payload_json=NULL,deleted_reason='collected' WHERE id='missing'",
            [],
        )
        .unwrap();
        repair_terminal_tx(&db, "repair").unwrap();
        assert_eq!(
            state(&db, "admitted"),
            (
                "applied".into(),
                "terminal_after_path_admission".into(),
                "repair".into()
            )
        );
        assert_eq!(
            state(&db, "unseen"),
            (
                "held".into(),
                "terminal_before_path_admission".into(),
                "repair".into()
            )
        );
        assert_eq!(
            state(&db, "unknown"),
            (
                "held".into(),
                "uncertain_path_admission".into(),
                "repair".into()
            )
        );
        assert_eq!(state(&db, "missing").0, "cancelled");
        repair_terminal_tx(&db, "later").unwrap();
        assert_eq!(state(&db, "unseen").2, "repair");
        assert_eq!(state(&db, "unknown").2, "repair");
        assert_eq!(state(&db, "admitted").2, "repair");
    }
    #[test]
    fn recovery_is_ordered_once_preserves_provenance_and_does_not_revive_removed_admitted_history()
    {
        let mut db = fixture();
        input(&db, "second", "unadmitted", "2");
        input(&db, "first", "unadmitted", "1");
        input(&db, "removed", "admitted", "0");
        input(&db, "uncertain", "unknown", "0");
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('unassigned','user','0.5','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,hold_reason)VALUES('unassigned','unassigned','null','t','unassigned','held','0.5','stopping');").unwrap();
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('new','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('new-turn','t','new','preparing','now');").unwrap();
        let tx = db.transaction().unwrap();
        assert_eq!(
            recover_on_resume_tx(&tx, "t", "new", "new-turn", json!({"kind":"human"}), "now")
                .unwrap(),
            3
        );
        assert_eq!(
            recover_on_resume_tx(
                &tx,
                "t",
                "new",
                "new-turn",
                json!({"kind":"human"}),
                "later"
            )
            .unwrap(),
            0
        );
        let nodes: Vec<String> = tx
            .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(nodes, ["unassigned", "first", "second"]);
        let assigned: (String, String) = tx
            .query_row(
                "SELECT run_id,turn_id FROM receipt WHERE id='unassigned'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(assigned, ("new".into(), "new-turn".into()));
        for id in ["first", "second"] {
            let saved:(String,String,String,String)=tx.query_row("SELECT r.run_id,r.turn_id,n.turn_id,r.path_admission FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            assert_eq!(
                saved,
                (
                    "old".into(),
                    "old-turn".into(),
                    "old-turn".into(),
                    "admitted".into()
                )
            );
            assert_eq!(state(&tx, id).1, "recovered_at_explicit_resume");
        }
        let events:i64=tx.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='interrupted_messages_recovered'",[],|r|r.get(0)).unwrap();
        assert_eq!(events, 1);
        assert_eq!(state(&tx, "uncertain").0, "held");
        assert_eq!(state(&tx, "removed").0, "applied");
        tx.commit().unwrap();
    }
    #[test]
    fn recipient_repair_writes_at_most_128_and_retains_unrelated_rows() {
        let db = fixture();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('unrelated','/','/a','p','m','before')",[]).unwrap();
        for n in 0..257 { input(&db, &format!("n{n:03}"), "unadmitted", "0"); }
        input(&db, "foreign", "unadmitted", "0");
        db.execute("UPDATE receipt SET target_thread_id='unrelated' WHERE id='foreign'", []).unwrap();
        db.execute_batch("CREATE TEMP TABLE writes(id TEXT); CREATE TEMP TRIGGER audit_repair AFTER UPDATE ON receipt BEGIN INSERT INTO writes VALUES(new.id); END;").unwrap();
        for expected in [128,128,1,0] {
            db.execute("DELETE FROM writes", []).unwrap();
            repair_thread_batch_tx(&db, "t", "repair").unwrap();
            assert_eq!(db.query_row("SELECT count(*) FROM writes", [], |r| r.get::<_,i64>(0)).unwrap(), expected);
            assert_eq!(db.query_row("SELECT hold_reason FROM receipt WHERE id='foreign'", [], |r| r.get::<_,Option<String>>(0)).unwrap(), None);
        }
    }

    #[test]
    fn mixed_recovery_sources_share_one_oldest_first_unit() {
        let mut db = fixture();
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('new','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('next','t','new','recovery_wait','now');").unwrap();
        for n in 0..257 {
            let id = format!("n{n:03}");
            input(&db, &id, "unadmitted", &format!("{n:03}"));
            if n % 2 == 0 {
                db.execute("UPDATE receipt SET run_id='new',turn_id='next',hold_reason='recovery_wait' WHERE id=?1", [&id]).unwrap();
            }
        }
        for (generation, count) in [(0,128),(1,256),(2,257)] {
            let tx = db.transaction().unwrap();
            assert!(super::super::state::resume_recovery_turn_tx(&tx,"t","new","next",generation,"resume").unwrap());
            let selected = tx.prepare("SELECT node_id FROM thread_path ORDER BY ordinal").unwrap().query_map([], |r| r.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
            assert_eq!(selected, (0..count).map(|n| format!("n{n:03}")).collect::<Vec<_>>());
            assert_eq!(tx.query_row("SELECT count(*) FROM receipt WHERE state='held'", [], |r| r.get::<_,i64>(0)).unwrap(), 257-count);
            tx.commit().unwrap();
        }
    }

    #[test]
    fn recovery_passes_preserve_order_restart_and_stale_retry_safety() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery.db");
        let db = fixture();
        for n in 0..257 {
            input(&db, &format!("n{n:03}"), "unadmitted", "0");
        }
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('new','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES('next','t','new','recovery_wait','now','{}');").unwrap();
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()])
            .unwrap();
        drop(db);
        for (generation, applied, expected_state) in [
            (0, 128, "recovery_wait"),
            (1, 256, "recovery_wait"),
            (2, 257, "preparing"),
        ] {
            let mut db = Connection::open(&path).unwrap();
            db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
            let tx = db.transaction().unwrap();
            assert!(
                super::super::state::resume_recovery_turn_tx(
                    &tx, "t", "new", "next", generation, "resume"
                )
                .unwrap()
            );
            assert!(
                !super::super::state::resume_recovery_turn_tx(
                    &tx,
                    "t",
                    "new",
                    "next",
                    generation,
                    "stale concurrent retry"
                )
                .unwrap()
            );
            assert_eq!(
                tx.query_row(
                    "SELECT count(*) FROM receipt WHERE state='applied'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                applied
            );
            assert_eq!(
                tx.query_row("SELECT state FROM turn WHERE id='next'", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                expected_state
            );
            let selected = tx
                .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                selected,
                (0..applied).map(|n| format!("n{n:03}")).collect::<Vec<_>>()
            );
            tx.commit().unwrap();
        }
    }

    #[test]
    fn bounded_recovery_and_partial_path_failure_roll_back_every_change() {
        let mut db = fixture();
        for n in 0..129 {
            input(&db, &format!("n{n:03}"), "unadmitted", "0");
        }
        let tx = db.transaction().unwrap();
        assert_eq!(
            recover_on_resume_tx(&tx, "t", "new", "u", json!({"kind":"human"}), "now").unwrap(),
            128
        );
        assert_eq!(
            tx.query_row("SELECT state FROM receipt WHERE id='n128'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "held"
        );
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute("DELETE FROM receipt WHERE id='n128'", [])
            .unwrap();
        db.execute_batch("CREATE TRIGGER fail_recovery BEFORE INSERT ON thread_path WHEN new.node_id='n001' BEGIN SELECT RAISE(ABORT,'synthetic path failure'); END;").unwrap();
        let tx = db.transaction().unwrap();
        assert!(
            recover_on_resume_tx(&tx, "t", "new", "u", json!({"kind":"human"}), "now").is_err()
        );
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM receipt WHERE state='applied'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn startup_stop_drain_and_shutdown_share_terminal_classification() {
        for boundary in ["startup", "stop", "shutdown"] {
            let mut db = fixture();
            input(&db, "pending", "unadmitted", "0");
            input(&db, "seen", "admitted", "1");
            db.execute_batch("UPDATE run SET state='live'; UPDATE turn SET state='running'; UPDATE receipt SET state='accepted';").unwrap();
            let tx = db.transaction().unwrap();
            match boundary {
                "startup" => {
                    crate::control::state::interrupt_previous_work(&tx, "end").unwrap();
                }
                "stop" => crate::control::state::stop_drained_runs(
                    &tx,
                    &[("old".into(), "t".into())],
                    false,
                )
                .unwrap(),
                _ => crate::control::state::finish_drained_shutdown(&tx).unwrap(),
            }
            assert_eq!(state(&tx, "pending").0, "held", "{boundary}");
            assert_eq!(
                state(&tx, "pending").1,
                "terminal_before_path_admission",
                "{boundary}"
            );
            assert_eq!(state(&tx, "seen").0, "applied", "{boundary}");
            tx.commit().unwrap();
        }
    }
    #[test]
    fn admission_fact_requires_real_target_path_and_survives_path_removal() {
        let mut db = fixture();
        input(&db, "input", "unadmitted", "0");
        mark_admitted_tx(&db, "t", &["input".into()]).unwrap();
        assert_eq!(
            db.query_row("SELECT path_admission FROM receipt", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "unadmitted"
        );
        let tx = db.transaction().unwrap();
        history::append_existing_nodes(&tx, "t", 0, &["input".into()], "now").unwrap();
        mark_admitted_tx(&tx, "t", &["input".into()]).unwrap();
        tx.execute("DELETE FROM thread_path", []).unwrap();
        repair_terminal_tx(&tx, "stopped").unwrap();
        assert_eq!(state(&tx, "input").0, "applied");
        tx.commit().unwrap();
    }
}
