//! Saved display prefixes. All ordering/aggregation happens after native limits.
use super::*;

const AUTOMATIC_ROWS: &str = "auto_ids u CROSS JOIN json_each(u.ids) j CROSS JOIN main.automatic_compaction c ON c.rowid=j.value";

fn prefix() -> String {
    format!("WITH operation_receipt AS MATERIALIZED (
 SELECT operation_id,created_at,CASE WHEN octet_length(result_ref_json)<=131072 THEN result_ref_json END result_ref_json,octet_length(result_ref_json)>131072 source_oversized FROM main.operation_receipt INDEXED BY compact_operation_thread
 WHERE CASE WHEN json_valid(result_ref_json) THEN json_extract(result_ref_json,'$.result.status')='queued' ELSE 0 END
 AND json_extract(result_ref_json,'$.result.thread_id')=?1
 ORDER BY created_at DESC,operation_id DESC LIMIT 201
), turn AS MATERIALIZED (
 SELECT id,thread_id FROM main.turn INDEXED BY turns_by_thread WHERE thread_id=?1 ORDER BY created_at DESC LIMIT 33
), auto_ids AS MATERIALIZED (
 SELECT (SELECT json_group_array(native_rowid) FROM
 (SELECT c.rowid native_rowid FROM main.automatic_compaction c INDEXED BY automatic_compaction_turn
 WHERE c.turn_id=u.id ORDER BY c.attempt_started_at DESC,c.attempt_id DESC LIMIT 33)) ids FROM turn u
), automatic_compaction AS MATERIALIZED (
 SELECT c.attempt_id,c.turn_id,c.attempt_started_at,c.outcome,c.reason,c.token,
 CASE WHEN octet_length(c.result_json)<=131072 THEN c.result_json END result_json,
 CASE WHEN octet_length(c.trigger_json)<=131072 THEN c.trigger_json END trigger_json,
 CASE WHEN octet_length(c.failure_json)<=1024 THEN c.failure_json END failure_json
 FROM {AUTOMATIC_ROWS}
), thread_replacement AS MATERIALIZED (
 SELECT * FROM main.thread_replacement WHERE thread_id=?1 ORDER BY replacement_id DESC LIMIT 201
) ")
}
fn receipt_sql() -> String {
    format!(
        "{}{}",
        prefix(),
        receipt_source_query(&Window::default(), true)
    )
}
fn automatic_sql() -> String {
    format!(
        "{}{}",
        prefix(),
        automatic_source_query(&Window::default(), true)
    )
}
fn replacement_sql() -> String {
    format!(
        "{}{}",
        prefix(),
        replacement_source_query(&Window::default(), true)
    )
}

pub(in crate::readback) fn read_display(
    db: &Connection,
    thread: &str,
) -> Result<RawCompactions, crate::storage::StorageError> {
    super::super::register(db)?;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [thread],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(ApiError::new(ErrorCode::TargetUnavailable, "Thread is unavailable").into());
    }
    // Size checks use point lookups on finite source prefixes before any JSON
    // projection; no connection-wide callback or SQLite limit is installed.
    let flags=format!("{}SELECT
 (SELECT count(*) FROM operation_receipt)=201 OR (SELECT count(*) FROM turn)=33
 OR EXISTS(SELECT 1 FROM auto_ids WHERE json_array_length(ids)=33)
 OR (SELECT count(*) FROM thread_replacement)=201,
 EXISTS(SELECT 1 FROM operation_receipt WHERE source_oversized) OR EXISTS(SELECT 1 FROM operation_receipt o CROSS JOIN app_meta m ON m.key='compact.result.'||o.operation_id WHERE octet_length(m.value)>131072 OR octet_length(o.operation_id)>512)
 OR EXISTS(SELECT 1 FROM {AUTOMATIC_ROWS} JOIN main.model_attempt a ON a.id=c.attempt_id WHERE octet_length(c.attempt_id)>512 OR octet_length(c.result_json)>131072 OR octet_length(c.trigger_json)>131072 OR octet_length(a.error_json)>131072)", prefix());
    let (saturated, oversized): (bool, bool) =
        db.query_row(&flags, [thread], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if oversized {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Compaction display source exceeds limits",
        )
        .into());
    }
    let meta = "CASE WHEN json_valid(m.value) THEN m.value ELSE '{}' END";
    let receipt_root = format!(
        "COALESCE(json_extract({meta},'$.outcome.result'),json_extract({meta},'$.outcome.error.details'),'{{}}')"
    );

    let nodes=format!("{} , node_ids AS MATERIALIZED (
      SELECT {} id FROM operation_receipt o CROSS JOIN app_meta m ON m.key='compact.result.'||o.operation_id WHERE COALESCE(json_extract(o.result_ref_json,'$.result.context_edit.action'),'compact')<>'restore'
      UNION ALL SELECT {} FROM automatic_compaction c
      UNION ALL SELECT r.output_node_id FROM thread_replacement tr CROSS JOIN replacement r ON r.id=tr.replacement_id
    ) SELECT EXISTS(SELECT 1 FROM node_ids i CROSS JOIN node n ON n.id=i.id WHERE n.kind='summary' AND octet_length(n.payload_json)>131072)",prefix(),output_node(&receipt_root),output_node(AUTOMATIC_METRICS));
    let oversized_node: bool = db.query_row(&nodes, [thread], |r| r.get(0))?;
    if oversized_node {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Compaction display node exceeds limits",
        )
        .into());
    }
    let mut budget = CopyBudget::display();
    let mut receipts = Vec::new();
    let mut statement = db.prepare(&receipt_sql())?;
    let mut rows = statement.query(params![thread, 5])?;
    while let Some(row) = rows.next()? {
        receipts.push(ReceiptRow {
            id: budget.text(row, 0, false)?,
            at: budget.optional(row, 2)?,
            kind: budget.text(row, 3, false)?,
            raw: budget.text(row, 4, true)?,
            state: budget.optional(row, 5)?,
            operation_count: row.get(6)?,
            in_window: true,
        });
    }
    let mut automatic = Vec::new();
    let mut statement = db.prepare(&automatic_sql())?;
    let mut rows = statement.query(params![thread, 5])?;
    while let Some(row) = rows.next()? {
        automatic.push(AutomaticRow {
            id: budget.text(row, 0, false)?,
            at: budget.optional(row, 1)?,
            raw: budget.text(row, 2, true)?,
            failure: budget.optional(row, 3)?,
            retry: row.get(4)?,
            turn: budget.text(row, 5, false)?,
            outcome: budget.text(row, 6, false)?,
            failures: row.get(7)?,
            reason: budget.optional(row, 8)?,
            trigger: budget.optional(row, 9)?,
            fact: budget.optional(row, 10)?,
        });
    }
    let mut replacements = Vec::new();
    let mut statement = db.prepare(&replacement_sql())?;
    let mut rows = statement.query(params![thread, 5])?;
    while let Some(row) = rows.next()? {
        replacements.push(ReplacementRow {
            id: budget.text(row, 0, false)?,
            at: budget.text(row, 1, false)?,
            format: budget.text(row, 2, false)?,
            raw: budget.text(row, 3, true)?,
            inherited: row.get(4)?,
        });
    }
    // Spill provenance matching can itself inspect lifetime-sized source sets.
    // Keep bounded display records separate rather than inventing associations.
    // Expired receipts can hide a later restore even when every prefix fits.
    let display_latest_complete = Some(false);
    // Exact post-adoption linkage can scan older Turns and attempts.
    Ok(RawCompactions {
        thread: thread.into(),
        limit: 1,
        window: Window::new(None, None)?,
        cap: 201,
        saturated,
        receipts,
        automatic,
        replacements,
        covered_spills: Default::default(),
        post_input: Default::default(),
        display_latest_complete,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        crate::readback::register(&db).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('original','message','2026-01-01T00:00:00Z','{}'),('marker','summary','2026-01-01T00:00:00Z','{}');").unwrap();
        db
    }
    fn grow(db: &Connection, from: usize, to: usize) {
        db.execute_batch("BEGIN").unwrap();
        for i in from..to {
            let id = format!("op{i:06}");
            let at = if i == 0 {
                "2099-01-01T00:00:00Z"
            } else {
                "2026-01-01T00:00:00Z"
            };
            db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES(?1,'test','{}','hash','completed',?2,'2026-01-01T00:00:00Z')",params![id,json!({"result":{"status":"queued","thread_id":"t","context_edit":{"action":"restore"}}}).to_string()]).unwrap();
            db.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2)",params![format!("compact.result.{id}"),json!({"state":"completed","ended_at":at,"outcome":{"result":{"omitted_nodes":0}}}).to_string()]).unwrap();
            let turn = format!("turn{i:06}");
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,'t','r','completed','2026-01-01T00:00:00Z')",[&turn]).unwrap();
            db.execute(r#"INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?2),?1,?2,0,0,'p','m',0,'failed','{}','2026-01-01T00:00:00Z')"#,params![format!("attempt{i:06}"),turn]).unwrap();
            db.execute("INSERT INTO automatic_compaction(attempt_id,turn_id,attempt_started_at,outcome) VALUES(?1,?2,'2026-01-01T00:00:00Z','failed')",params![format!("attempt{i:06}"),turn]).unwrap();
            let replacement = format!("replacement{i:06}");
            db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES(?1,'marker',?1,'summary-v1','test','completed','2026-01-01T00:00:00Z')",[&replacement]).unwrap();
            db.execute(
                "INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('t',?1)",
                [&replacement],
            )
            .unwrap();
            db.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES(?1,0,'original',0)",[&replacement]).unwrap();
        }
        db.execute_batch("COMMIT").unwrap();
    }
    #[test]
    fn bounded_compaction_prefix_seeks_use_native_order() {
        let db = fixture();
        let sql = format!(
            "EXPLAIN QUERY PLAN {} SELECT * FROM operation_receipt CROSS JOIN automatic_compaction CROSS JOIN thread_replacement",
            prefix()
        );
        let plan = db
            .prepare(&sql)
            .unwrap()
            .query_map(["t"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for index in [
            "compact_operation_thread",
            "turns_by_thread",
            "automatic_compaction_turn",
            "sqlite_autoindex_thread_replacement_1",
        ] {
            assert!(
                plan.iter()
                    .any(|step| step.starts_with("SEARCH") && step.contains(index)),
                "missing seek through {index}: {plan:?}"
            );
        }
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "prefix must not sort an unbounded input: {plan:?}"
        );
    }

    #[test]
    fn expired_restore_cannot_certify_an_older_replacement_as_latest() {
        let db = fixture();
        grow(&db, 0, 1);
        let before = prepare(read_display(&db, "t").unwrap()).unwrap();
        assert_eq!(before["items"][0]["kind"], "restore");
        db.execute(
            "DELETE FROM operation_receipt WHERE operation_id=?1",
            ["op000000"],
        )
        .unwrap();
        let after = prepare(read_display(&db, "t").unwrap()).unwrap();
        assert_ne!(after["items"][0]["kind"], "restore");
        assert_eq!(after["coverage"]["latest_complete"], false);
    }
    #[test]
    fn owned_receipt_history_tenfold_growth_keeps_completion_order_uninvented() {
        let db = fixture();
        grow(&db, 0, 2560);
        let v = crate::readback::last_compaction(&db, "t").unwrap();
        assert_eq!(v["coverage"]["latest_complete"], false);
        let exact = crate::readback::compactions(
            &db,
            "t",
            &CompactListArgs {
                thread_id: Some("t".into()),
                since: None,
                until: None,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(exact["items"][0]["event_id"], "operation:op000000");
        assert!(v["bounded_candidate"]["event_id"].is_string());
        assert_ne!(
            v["bounded_candidate"]["event_id"],
            exact["items"][0]["event_id"]
        );
    }
}
