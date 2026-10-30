//! Body-free submission correlations survive disposal of completed frozen copies.
use super::db::DraftSubmission;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) const MARKER_BYTES: usize = 64 * 1024;

pub(super) fn marker(operation: &str, source: &str, revision: u64, message: &str) -> Option<Value> {
    if [operation, source, message]
        .iter()
        .any(|id| id.is_empty() || id.len() > 4096)
        || revision > i64::MAX as u64
    {
        return None;
    }
    let value = json!({"operation_id":operation,"source_draft_id":source,
        "source_revision":revision,"message_id":message});
    (value.to_string().len() <= MARKER_BYTES).then_some(value)
}

pub(super) fn save_marker(db: &Connection, operation: &str, value: &Value) -> rusqlite::Result<()> {
    db.execute("UPDATE receipt SET result_json=json_set(COALESCE(result_json,'{}'),'$.draft_submission',json(?2)) WHERE client_operation_id=?1", params![operation, value.to_string()])?;
    Ok(())
}

// SQL fragments are internal column expressions, never interpolated user input.
pub(crate) fn fence(source: &str, revision: &str) -> String {
    format!(
        "(EXISTS(SELECT 1 FROM draft submitted WHERE submitted.state='submitted' AND json_extract(submitted.body_json,'$._submission.source_draft_id')={source} AND json_extract(submitted.body_json,'$._submission.source_revision')>={revision}) OR EXISTS(SELECT 1 FROM receipt binding WHERE json_extract(binding.result_json,'$.draft_submission.operation_id')=binding.client_operation_id AND json_extract(binding.result_json,'$.draft_submission.message_id')=binding.message_node_id AND json_extract(binding.result_json,'$.draft_submission.source_draft_id')={source} AND json_type(binding.result_json,'$.draft_submission.source_revision')='integer' AND json_extract(binding.result_json,'$.draft_submission.source_revision')>={revision}))"
    )
}

/// The caller owns the admission/cancellation/GC transaction. Uncertain facts retain the copy.
pub(crate) fn collect_operation(db: &Connection, operation: &str) -> rusqlite::Result<usize> {
    let id = DraftSubmission::id_for_operation(operation);
    let row: Option<(String, String, String)> = db.query_row(
        "SELECT json_extract(d.body_json,'$._submission'),r.message_node_id,r.target_thread_id FROM draft d JOIN receipt r ON r.client_operation_id=?2 WHERE d.id=?1 AND d.state='submitted' AND (r.result_json IS NULL OR json_type(r.result_json)='object') AND json_type(d.body_json,'$._submission.source_revision')='integer' AND json_extract(d.body_json,'$._submission.source_revision')=d.revision AND json_extract(d.body_json,'$.draft_id')=json_extract(d.body_json,'$._submission.source_draft_id') AND d.thread_id=r.target_thread_id AND r.path_admission<>'unknown' AND (r.path_admission='admitted' OR r.state='cancelled') AND octet_length(json_extract(d.body_json,'$._submission'))<=?3",
        params![id,operation,MARKER_BYTES as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    let Some((metadata, message, thread)) = row else {
        return Ok(0);
    };
    let Ok(metadata) = serde_json::from_str::<Value>(&metadata) else {
        return Ok(0);
    };
    if metadata["operation_id"].as_str() != Some(operation)
        || metadata["message_id"].as_str() != Some(&message)
        || metadata["accepted_thread_id"].as_str() != Some(&thread)
    {
        return Ok(0);
    }
    let Some(source) = metadata["source_draft_id"].as_str() else {
        return Ok(0);
    };
    let Some(revision) = metadata["source_revision"].as_u64() else {
        return Ok(0);
    };
    let Some(value) = marker(operation, source, revision, &message) else {
        return Ok(0);
    };
    save_marker(db, operation, &value)?;
    db.execute("DELETE FROM draft WHERE id=?1 AND state='submitted'", [id])
}

pub(crate) fn collect_batch(db: &Connection) -> rusqlite::Result<usize> {
    let operations = db.prepare("SELECT r.client_operation_id FROM draft d JOIN receipt r ON r.client_operation_id=json_extract(d.body_json,'$._submission.operation_id') WHERE d.state='submitted' AND (r.result_json IS NULL OR json_type(r.result_json)='object') AND json_type(d.body_json,'$._submission.source_revision')='integer' AND json_extract(d.body_json,'$._submission.source_revision')=d.revision AND json_extract(d.body_json,'$.draft_id')=json_extract(d.body_json,'$._submission.source_draft_id') AND d.thread_id=r.target_thread_id AND json_extract(d.body_json,'$._submission.accepted_thread_id')=r.target_thread_id AND json_extract(d.body_json,'$._submission.message_id')=r.message_node_id AND r.path_admission<>'unknown' AND (r.path_admission='admitted' OR r.state='cancelled') ORDER BY d.updated_at,d.id LIMIT 128")?
        .query_map([], |r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut removed = 0;
    for operation in operations {
        removed += collect_operation(db, &operation)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','0')",[]).unwrap();
        db
    }
    fn copy(db: &Connection, op: &str, admission: &str, state: &str, draft_state: &str) -> String {
        let id = DraftSubmission::id_for_operation(op);
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','0','{}')",
            [op],
        )
        .unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json)VALUES(?1,?1,'null','t',?1,?2,?3,'0','{}')",params![op,state,admission]).unwrap();
        let body = json!({"draft_id":"composer","revision":7,"text":"PRIVATE_FROZEN_TEXT","attachments":[{"path":"PRIVATE_PATH"}],"_submission":{"operation_id":op,"source_draft_id":"composer","source_revision":7,"message_id":op,"accepted_thread_id":"t"}});
        db.execute(
            "INSERT INTO draft VALUES(?1,'t',7,?2,?3,'0')",
            params![id, body.to_string(), draft_state],
        )
        .unwrap();
        id
    }
    #[test]
    fn terminal_copies_only_and_marker_is_bounded_body_free_and_rollback_safe() {
        let mut db = fixture();
        for (op, admission, state, draft_state, eligible) in [
            ("admitted", "admitted", "accepted", "submitted", true),
            ("cancelled", "unadmitted", "cancelled", "submitted", true),
            ("queued", "unadmitted", "held", "submitted", false),
            ("unknown", "unknown", "cancelled", "submitted", false),
            ("submitting", "admitted", "applied", "submitting", false),
            ("editing", "admitted", "applied", "editing", false),
            ("archive", "admitted", "applied", "archived", false),
        ] {
            let id = copy(&db, op, admission, state, draft_state);
            {
                let tx = db.transaction().unwrap();
                assert_eq!(collect_operation(&tx, op).unwrap(), usize::from(eligible));
            }
            assert!(
                db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft WHERE id=?1)",
                    [&id],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap(),
                "rollback must retain the copy"
            );
            let tx = db.transaction().unwrap();
            assert_eq!(collect_operation(&tx, op).unwrap(), usize::from(eligible));
            tx.commit().unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft WHERE id=?1)",
                    [id],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap(),
                !eligible
            );
            let saved: String = db
                .query_row("SELECT result_json FROM receipt WHERE id=?1", [op], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(!saved.contains("PRIVATE_"));
            if eligible {
                let saved: Value = serde_json::from_str(&saved).unwrap();
                assert_eq!(saved["draft_submission"].as_object().unwrap().len(), 4);
                assert_eq!(saved["draft_submission"]["source_revision"], 7);
                assert!(saved["draft_submission"].to_string().len() <= MARKER_BYTES);
            }
        }
        assert!(marker("op", "id", 7, "msg").is_some());
        assert!(marker("op", &"x".repeat(4097), 7, "msg").is_none());
        assert!(marker("op", "id", u64::MAX, "msg").is_none());
        assert!(
            marker(&"\0".repeat(4096), &"\0".repeat(4096), 7, "msg")
                .unwrap()
                .to_string()
                .len()
                <= MARKER_BYTES
        );
    }
    #[test]
    fn legacy_gc_is_batched_correlated_and_keeps_unknown_and_cancelled_archives() {
        let mut db = fixture();
        for n in 0..130 {
            copy(
                &db,
                &format!("old-{n:03}"),
                "admitted",
                "applied",
                "submitted",
            );
        }
        let unknown = copy(&db, "unknown", "unknown", "cancelled", "submitted");
        let mismatched = copy(&db, "mismatch", "admitted", "applied", "submitted");
        db.execute("UPDATE draft SET body_json=json_set(body_json,'$._submission.message_id','not-the-receipt') WHERE id=?1",[&mismatched]).unwrap();
        for expected in [128, 2, 0] {
            let tx = db.transaction().unwrap();
            assert_eq!(collect_batch(&tx).unwrap(), expected);
            tx.commit().unwrap();
        }
        for id in [unknown, mismatched] {
            assert!(
                db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft WHERE id=?1)",
                    [id],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
            );
        }
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE json_type(result_json,'$.draft_submission')='object'",[],|r|r.get::<_,i64>(0)).unwrap(),130);
    }
    #[test]
    fn admission_and_retirement_rollback_together_if_deletion_fails() {
        let mut db = fixture();
        let id = copy(&db, "admit", "unadmitted", "accepted", "submitted");
        db.execute(
            "INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('t',0,'admit')",
            [],
        )
        .unwrap();
        db.execute_batch("CREATE TRIGGER fail_retirement BEFORE DELETE ON draft BEGIN SELECT RAISE(ABORT,'synthetic deletion failure'); END;").unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(
                crate::control::message_delivery::mark_admitted_tx(&tx, "t", &["admit".into()])
                    .is_err()
            );
        }
        assert_eq!(
            db.query_row(
                "SELECT path_admission FROM receipt WHERE id='admit'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "unadmitted"
        );
        assert!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM draft WHERE id=?1)",
                [id],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );
        db.execute_batch("DROP TRIGGER fail_retirement").unwrap();
        let tx = db.transaction().unwrap();
        crate::control::message_delivery::mark_admitted_tx(&tx, "t", &["admit".into()]).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT path_admission FROM receipt WHERE id='admit'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "admitted"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM draft", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn receipt_fence_survives_copy_deletion_and_does_not_hide_newer_edits() {
        let mut db = fixture();
        copy(&db, "op", "admitted", "applied", "submitted");
        let tx = db.transaction().unwrap();
        collect_operation(&tx, "op").unwrap();
        tx.commit().unwrap();
        for (source, revision, expected) in [
            ("composer", 6, true),
            ("composer", 7, true),
            ("composer", 8, false),
            ("other", 7, false),
        ] {
            assert_eq!(
                db.query_row(
                    &format!("SELECT {}", fence("?1", "?2")),
                    params![source, revision],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap(),
                expected
            );
        }
    }
}
