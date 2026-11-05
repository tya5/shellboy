//! Saved display prefixes. All ordering/aggregation happens after native limits.
use super::*;

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
 (SELECT a.rowid native_rowid FROM main.model_attempt a INDEXED BY automatic_compaction_turn
 WHERE a.turn_id=u.id AND {AUTO} ORDER BY a.started_at DESC,a.id DESC LIMIT 33)) ids FROM turn u
), model_attempt AS MATERIALIZED (
 SELECT a.rowid rowid,a.id,a.turn_id,a.started_at,a.ended_at,CASE WHEN octet_length(a.error_json)<=131072 THEN a.error_json END error_json,octet_length(a.error_json)>131072 source_oversized FROM auto_ids u CROSS JOIN json_each(u.ids) j CROSS JOIN main.model_attempt a ON a.rowid=j.value
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
 OR EXISTS(SELECT 1 FROM model_attempt WHERE source_oversized OR octet_length(id)>512)", prefix());
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
    let auto_root = "COALESCE(json_extract(a.error_json,'$.automatic_trim'),json_extract(a.error_json,'$.automatic_trim_attempt.result'),json_extract(a.error_json,'$.automatic_trim_attempt.summary'),json_extract(a.error_json,'$.automatic_trim_attempt.error.details'),'{}')";
    let nodes=format!("{} , node_ids AS MATERIALIZED (
      SELECT {} id FROM operation_receipt o CROSS JOIN app_meta m ON m.key='compact.result.'||o.operation_id WHERE COALESCE(json_extract(o.result_ref_json,'$.result.context_edit.action'),'compact')<>'restore'
      UNION ALL SELECT {} FROM model_attempt a
      UNION ALL SELECT r.output_node_id FROM thread_replacement tr CROSS JOIN replacement r ON r.id=tr.replacement_id
    ) SELECT EXISTS(SELECT 1 FROM node_ids i CROSS JOIN node n ON n.id=i.id WHERE n.kind='summary' AND octet_length(n.payload_json)>131072)",prefix(),output_node(&receipt_root),output_node(auto_root));
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
        });
    }
    // Spill provenance matching can itself inspect lifetime-sized source sets.
    // Keep bounded display records separate rather than inventing associations.
    let display_latest_complete = Some(
        !saturated
            && receipts.len() < 5
            && automatic.len() < 5
            && replacements.len() < 5
            && !receipts.iter().any(|row| {
                serde_json::from_str::<Value>(&row.raw)
                    .ok()
                    .is_some_and(|v| v["spilled_groups"].as_u64().is_some_and(|n| n > 0))
            }),
    );
    let post_input = post_inputs(db, thread, &replacements)?;
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
        post_input,
        display_latest_complete,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;
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
            db.execute(r#"INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES(?1,?2,0,0,'p','m',0,'failed','{}','{"automatic_trim_attempt":{"outcome":"failed"}}','2026-01-01T00:00:00Z')"#,params![format!("attempt{i:06}"),turn]).unwrap();
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
    fn costs(db: &Connection) -> Vec<(i32, i32)> {
        [receipt_sql(), automatic_sql(), replacement_sql()]
            .iter()
            .map(|sql| {
                let mut q = db.prepare(sql).unwrap();
                {
                    let mut rows = q.query(params!["t", 5]).unwrap();
                    while rows.next().unwrap().is_some() {}
                }
                (
                    q.get_status(StatementStatus::VmStep),
                    q.get_status(StatementStatus::FullscanStep),
                )
            })
            .collect()
    }
    #[test]
    fn owned_receipt_history_tenfold_growth_is_bounded_and_completion_order_is_not_invented() {
        let db = fixture();
        grow(&db, 0, 256);
        let a = costs(&db);
        grow(&db, 256, 2560);
        let b = costs(&db);
        for (before, after) in a.iter().zip(&b) {
            assert!(
                after.0 <= before.0 && after.0 < 250_000 && after.1 < 10_000,
                "{a:?}->{b:?}"
            );
        }
        let raw = read_display(&db, "t").unwrap();
        let v = prepare(raw).unwrap();
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
        assert_ne!(v["items"][0]["event_id"], exact["items"][0]["event_id"]);
        eprintln!("bounded compaction actual per-query VM/fullscan256->2560: {a:?}->{b:?}");
    }
}
