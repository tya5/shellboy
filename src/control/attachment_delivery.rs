//! Dedicated attachment provenance is adopted only between complete tool groups.
use crate::protocol::{ApiError, ErrorCode};
use rusqlite::{OptionalExtension, Transaction, params};

// Held inputs keep their original Run assignment. Explicit retry resumes the
// same live Run; a new Run must neither inherit nor wait on an old held source.
const ELIGIBLE_PENDING: &str = "d.status IN ('accepted','held') AND ((d.status='accepted' AND d.target_run_id IS NULL) OR EXISTS(SELECT 1 FROM run r WHERE r.id=d.target_run_id AND r.thread_id=d.thread_id AND r.state='live')) AND NOT EXISTS(SELECT 1 FROM run origin_run WHERE d.status='held' AND json_extract(d.origin_json,'$.kind')='shell' AND origin_run.id=json_extract(d.origin_json,'$.run_id') AND origin_run.state NOT IN ('live','stopping') AND NOT EXISTS(SELECT 1 FROM shell_execution s JOIN tool_call c ON c.operation_id=s.id JOIN tool_group g ON g.id=c.group_id WHERE s.id=json_extract(d.origin_json,'$.execution_id') AND g.state='completed'))";

/// Freeze both destinations and operations produced by a stopping Run.
pub fn hold_run_tx(tx: &Transaction<'_>, run: &str) -> Result<(), ApiError> {
    tx.execute("UPDATE attachment_delivery SET status='held' WHERE status='accepted' AND (target_run_id=?1 OR (json_extract(origin_json,'$.kind')='shell' AND json_extract(origin_json,'$.run_id')=?1))",[run]).map_err(sql)?;
    Ok(())
}

// Match the authenticated execution and its owner, rather than trusting only
// a group-like identifier in origin JSON.
const INTERRUPTED_GROUP_DELIVERY: &str = "d.status IN ('accepted','held') AND json_extract(d.origin_json,'$.kind')='shell' AND EXISTS(SELECT 1 FROM shell_execution s JOIN tool_call c ON c.operation_id=s.id WHERE c.group_id=?1 AND s.id=json_extract(d.origin_json,'$.execution_id') AND s.owner_run_id=json_extract(d.origin_json,'$.run_id'))";

/// A repaired group must not make previously unadopted side effects eligible.
/// Source nodes/files and already-applied deliveries remain unchanged.
pub(crate) fn cancel_interrupted_group_tx(
    tx: &Transaction<'_>,
    group: &str,
    at: &str,
) -> Result<usize, ApiError> {
    tx.execute(
        &format!("UPDATE thread SET revision=revision+1,selection_revision=selection_revision+1,updated_at=?2 WHERE id IN (SELECT d.thread_id FROM attachment_delivery d WHERE {INTERRUPTED_GROUP_DELIVERY})"),
        params![group, at],
    ).map_err(sql)?;
    let changed = tx.execute(
        &format!("UPDATE attachment_delivery AS d SET status='cancelled' WHERE {INTERRUPTED_GROUP_DELIVERY}"),
        [group],
    ).map_err(sql)?;
    if changed > 0 {
        tx.execute(
            "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
            [],
        )
        .map_err(sql)?;
    }
    Ok(changed)
}

pub fn has_pending_tx(tx: &Transaction<'_>, thread: &str) -> Result<bool, ApiError> {
    tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM attachment_delivery d WHERE d.thread_id=?1 AND {ELIGIBLE_PENDING})"),[thread],|r|r.get(0)).map_err(sql)
}

/// Append immutable accepted attachment source nodes to this Thread's selected
/// path. The caller serializes this with Turn/model admission in its transaction.
pub fn apply_pending_tx(tx: &Transaction<'_>, thread: &str, now: &str) -> Result<usize, ApiError> {
    let blocked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run r LEFT JOIN turn u ON u.run_id=r.id WHERE r.thread_id=?1 AND (r.state='stopping' OR (r.state='live' AND u.state='recovery_wait'))) OR EXISTS(SELECT 1 FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND a.state IN ('preparing','inflight')) OR EXISTS(SELECT 1 FROM tool_group g JOIN turn u ON u.id=g.turn_id WHERE u.thread_id=?1 AND g.state<>'completed' AND u.state IN ('preparing','running','waiting'))",[thread],|r|r.get(0)).map_err(sql)?;
    if blocked {
        return Ok(0);
    }
    let nodes:Vec<String>=tx.prepare(&format!("SELECT d.node_id FROM attachment_delivery d WHERE d.thread_id=?1 AND {ELIGIBLE_PENDING} ORDER BY d.rowid")).map_err(sql)?.query_map([thread],|r|r.get(0)).map_err(sql)?.collect::<rusqlite::Result<_>>().map_err(sql)?;
    let mut applied = 0;
    for node in nodes {
        // A shell can target another Thread. Its source group must be complete
        // too, even when the destination Thread is currently idle.
        let origin_pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM attachment_delivery d WHERE d.node_id=?1 AND d.thread_id=?2 AND json_extract(d.origin_json,'$.kind')='shell' AND NOT EXISTS(SELECT 1 FROM shell_execution s JOIN tool_call c ON c.operation_id=s.id JOIN tool_group g ON g.id=c.group_id WHERE s.id=json_extract(d.origin_json,'$.execution_id') AND s.owner_run_id=json_extract(d.origin_json,'$.run_id') AND g.state='completed'))",params![node,thread],|r|r.get(0)).map_err(sql)?;
        if origin_pending {
            break;
        }
        // Control commits receipts before/after attachment operations. Adopt
        // preceding pending user nodes first without acknowledging model input.
        let earlier:Vec<String>=tx.prepare("SELECT r.message_node_id FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 AND r.state='accepted' AND n.rowid<(SELECT rowid FROM node WHERE id=?2) AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=n.id) ORDER BY n.rowid").map_err(sql)?.query_map(params![thread,node],|r|r.get(0)).map_err(sql)?.collect::<rusqlite::Result<_>>().map_err(sql)?;
        for message in earlier {
            append_selected(tx, thread, &message, now)?;
        }
        append_selected(tx, thread, &node, now)?;
        tx.execute(
            "UPDATE attachment_delivery SET status='applied' WHERE node_id=?1 AND thread_id=?2",
            params![node, thread],
        )
        .map_err(sql)?;
        applied += 1;
    }
    Ok(applied)
}
fn append_selected(
    tx: &Transaction<'_>,
    thread: &str,
    node: &str,
    now: &str,
) -> Result<(), ApiError> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2)",
            params![thread, node],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if exists {
        return Ok(());
    }
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM node WHERE id=?1 AND deleted_reason IS NULL AND payload_json IS NOT NULL)",[&node],|r|r.get(0)).map_err(sql)?;
    if !valid {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "Pending attachment provenance is missing",
        ));
    }
    let previous: Option<String> = tx
        .query_row(
            "SELECT cursor_node_id FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [thread],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::ThreadNotFound,
                "Attachment target Thread was deleted",
            )
        })?;
    let last: Option<String> = tx
        .query_row(
            "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1",
            [thread],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    if previous != last {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "Attachment target cursor differs from selected path",
        ));
    }
    if let Some(previous) = previous {
        tx.execute(
            "INSERT INTO edge(src,dst)VALUES(?1,?2)",
            params![previous, node],
        )
        .map_err(sql)?;
    }
    tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)SELECT ?1,COALESCE(MAX(ordinal),-1)+1,?2 FROM thread_path WHERE thread_id=?1",params![thread,node]).map_err(sql)?;
    tx.execute("UPDATE thread SET cursor_node_id=?2,selection_revision=selection_revision+1,revision=revision+1,updated_at=?3 WHERE id=?1",params![thread,node,now]).map_err(sql)?;
    tx.execute(
        "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
        [],
    )
    .map_err(sql)?;
    Ok(())
}

fn sql(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(error.to_string())
}

#[cfg(test)]
mod interruption_tests {
    use super::*;

    #[test]
    fn repair_cancels_only_unadopted_authenticated_group_sources_atomically() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch(r#"
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/','p','m','before');
            INSERT INTO run VALUES('r','t',NULL,'stopped','before','before',NULL);
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','stopped','before');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('a','u',0,0,'p','m',0,'completed','{}','before');
            INSERT INTO tool_group VALUES('g','a','u','running','before');
            INSERT INTO tool_call VALUES('g','c',0,'shell','{}','completed','s');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('s','r','u','s','stopped','/','{}','UTF-8','UTF-8','closed','before');
        "#).unwrap();
        for (id, status, origin) in [
            (
                "accepted",
                "accepted",
                r#"{"kind":"shell","execution_id":"s","run_id":"r"}"#,
            ),
            (
                "held",
                "held",
                r#"{"kind":"shell","execution_id":"s","run_id":"r"}"#,
            ),
            (
                "applied",
                "applied",
                r#"{"kind":"shell","execution_id":"s","run_id":"r"}"#,
            ),
            ("external", "accepted", r#"{"kind":"external"}"#),
            (
                "foreign",
                "held",
                r#"{"kind":"shell","execution_id":"s","run_id":"other"}"#,
            ),
        ] {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'attachment_source','before','{}')",[id]).unwrap();
            db.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at) VALUES(?1,'t',?2,?1,?3,'before')",params![id,status,origin]).unwrap();
        }
        {
            let tx = db.transaction().unwrap();
            assert_eq!(cancel_interrupted_group_tx(&tx, "g", "after").unwrap(), 2);
            // Transaction failure must preserve original eligibility and revision.
            tx.rollback().unwrap();
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM attachment_delivery WHERE status='cancelled'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT selection_revision FROM thread WHERE id='t'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let tx = db.transaction().unwrap();
        assert_eq!(cancel_interrupted_group_tx(&tx, "g", "after").unwrap(), 2);
        assert_eq!(cancel_interrupted_group_tx(&tx, "g", "again").unwrap(), 0);
        assert_eq!(
            tx.query_row(
                "SELECT selection_revision FROM thread WHERE id='t'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            tx.query_row(
                "SELECT value FROM app_meta WHERE key='root_revision'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "1"
        );
        assert_eq!(
            tx.query_row(
                "SELECT status FROM attachment_delivery WHERE node_id='applied'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "applied"
        );
        assert_eq!(tx.query_row("SELECT count(*) FROM attachment_delivery WHERE node_id IN ('external','foreign') AND status IN ('accepted','held')",[],|r|r.get::<_,i64>(0)).unwrap(),2);
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM node WHERE payload_json='{}'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            5
        );
        tx.commit().unwrap();
    }
}
