//! Current safe-boundary ownership from existing protected compaction metadata.
use crate::protocol::{ApiError, RunWaitReason};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) fn read(
    db: &Connection,
    thread: &str,
    run: &str,
) -> Result<Option<RunWaitReason>, ApiError> {
    let row: Option<(Option<i64>, Option<String>)> = db.query_row(
        "SELECT CASE WHEN json_valid(m.value) THEN json_array_length(m.value) END, CASE WHEN json_valid(m.value) THEN CASE WHEN json_type(m.value,'$[0]')='text' THEN json_extract(m.value,'$[0]') END END FROM app_meta m WHERE m.key=?1 AND EXISTS(SELECT 1 FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id=?2 AND r.thread_id=?3 AND r.state='live' AND u.state IN ('preparing','running'))",
        params![crate::compaction::CompactionKeys::new(thread).inflight(),run,thread],
        |r|Ok((r.get(0)?,r.get(1)?)),
    ).optional().map_err(super::error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let (Some(count), Some(operation_id)) = row else {
        return Ok(None);
    };
    if count < 1 || operation_id.len() > 1024 {
        return Ok(None);
    }
    let saved: Option<(String,Option<String>)>=db.query_row(
        "SELECT created_at, CASE WHEN json_valid(result_ref_json) THEN CASE WHEN json_type(result_ref_json,'$.result.context_edit') IS NULL THEN 'compact' WHEN json_type(result_ref_json,'$.result.context_edit.action')='text' THEN json_extract(result_ref_json,'$.result.context_edit.action') ELSE 'invalid' END ELSE 'invalid' END FROM operation_receipt WHERE operation_id=?1",
        [&operation_id],|r|Ok((r.get(0)?,r.get(1)?)),
    ).optional().map_err(super::error)?;
    let Some((accepted_at, action)) = saved else {
        return Ok(None);
    };
    let action = action.unwrap_or_else(|| "compact".into());
    if !matches!(
        action.as_str(),
        "summary" | "spill" | "trim" | "omit" | "restore" | "maximal" | "compact"
    ) {
        return Ok(None);
    }
    Ok(Some(RunWaitReason {
        kind: "context_edit_boundary".into(),
        operation_id,
        action,
        accepted_at,
        operation_count: count as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
          INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now');
          INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now');
          INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('operation','fixture','{}','hash','completed','{\"result\":{\"context_edit\":{\"action\":\"summary\"}}}','2026-10-31T12:38:20Z');
          INSERT INTO app_meta(key,value)VALUES('compact.inflight.t','[\"operation\"]');").unwrap();
        db
    }
    #[tokio::test(start_paused = true)]
    async fn saved_wait_has_no_clock_expiry_and_terminal_or_removed_owner_clears_it() {
        let db = fixture();
        let first = read(&db, "t", "r").unwrap().unwrap();
        assert_eq!(first.action, "summary");
        assert_eq!(first.accepted_at, "2026-10-31T12:38:20Z");
        tokio::time::advance(std::time::Duration::from_secs(600)).await;
        assert_eq!(read(&db, "t", "r").unwrap().unwrap(), first);
        assert!(read(&db, "different", "r").unwrap().is_none());
        db.execute(
            "UPDATE turn SET state='completed',ended_at='now' WHERE id='u'",
            [],
        )
        .unwrap();
        assert!(read(&db, "t", "r").unwrap().is_none());
        db.execute(
            "UPDATE turn SET state='running',ended_at=NULL WHERE id='u'",
            [],
        )
        .unwrap();
        db.execute("DELETE FROM app_meta WHERE key='compact.inflight.t'", [])
            .unwrap();
        assert!(read(&db, "t", "r").unwrap().is_none());
    }
    #[test]
    fn damaged_wait_witness_does_not_block_run_get_or_projection() {
        for damage in [
            "DELETE FROM app_meta",
            "UPDATE app_meta SET value='bad'",
            "UPDATE app_meta SET value='[]'",
            "UPDATE app_meta SET value='[\"missing\"]'",
            "DELETE FROM operation_receipt",
            "UPDATE operation_receipt SET result_ref_json='bad'",
            "UPDATE operation_receipt SET result_ref_json='{\"result\":{\"context_edit\":{}}}'",
            "UPDATE operation_receipt SET result_ref_json='{\"result\":{\"context_edit\":{\"action\":\"unknown\"}}}'",
        ] {
            let db = fixture();
            db.execute_batch(damage).unwrap();
            let run = crate::control::db::run_details(&db, "r").unwrap();
            assert!(run.waiting_reason.is_none(), "{damage}");
            assert!(
                serde_json::to_value(run)
                    .unwrap()
                    .get("waiting_reason")
                    .is_none(),
                "{damage}"
            );
            let projection = crate::frontend::activity::project(&db, "t").unwrap();
            assert!(projection.snapshot.waiting_reason.is_none(), "{damage}");
        }
    }
}
