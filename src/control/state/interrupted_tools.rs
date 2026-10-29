//! Complete current-format interrupted calls without replaying external work.
use super::*;

const PAGE: usize = 32;
const REASON: &str = "previous_instance_interrupted";

#[derive(Default, Debug)]
pub(crate) struct RepairReport {
    pub repaired_groups: u64,
    pub unresolved_groups: u64,
    pub retired_groups: u64,
    pub failures: Vec<crate::diagnostics::ToolRepairFailure>,
}
#[derive(Debug)]
enum RepairError {
    Structural(crate::diagnostics::ToolRepairReason),
    Storage(StorageError),
}
impl From<rusqlite::Error> for RepairError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}
impl From<StorageError> for RepairError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
type RepairResult<T> = Result<T, RepairError>;
use crate::diagnostics::ToolRepairReason;

fn bounded_id(value: &str) -> String {
    value.chars().take(128).collect()
}
fn fatal(error: StorageError, thread: &str, group: &str) -> StorageError {
    let mut error = error.into_api_error();
    error.message = format!(
        "Interrupted tool repair failed for Thread {} group {}: {}",
        bounded_id(thread),
        bounded_id(group),
        error.message
    );
    error.details = Some(json!({"thread_id":bounded_id(thread),"group_id":bounded_id(group)}));
    error.into()
}

pub(crate) fn repair_interrupted_tool_groups(
    tx: &rusqlite::Transaction<'_>,
    at: &str,
) -> Result<RepairReport, StorageError> {
    let mut report = RepairReport::default();
    let mut after: Option<String> = None;
    loop {
        let comparison = if after.is_some() { ">" } else { ">=" };
        let sql = format!(
            "SELECT g.id,g.attempt_id,g.turn_id,g.state,u.thread_id FROM tool_group g JOIN turn u ON u.id=g.turn_id WHERE g.id {comparison} ?1 AND (g.state<>'completed' OR EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state IN ('pending','running') OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id)))) ORDER BY g.id LIMIT {PAGE}"
        );
        let groups = tx
            .prepare_cached(&sql)?
            .query_map([after.as_deref().unwrap_or("")], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if groups.is_empty() {
            break;
        }
        for (group, attempt, turn, state, thread) in groups {
            tx.execute_batch("SAVEPOINT startup_tool_group")?;
            let repair = (|| -> RepairResult<bool> {
                crate::control::attachment_delivery::cancel_interrupted_group_tx(tx, &group, at)
                    .map_err(StorageError::from)?;
                if collected_source_group(tx, &attempt, &turn)? {
                    return Ok(false);
                }
                if !matches!(state.as_str(), "running" | "completed") {
                    return Err(RepairError::Structural(ToolRepairReason::UnknownGroupState));
                }
                let bad_cursor: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM output_item o JOIN thread_path p ON p.node_id=o.node_id JOIN thread t ON t.id=p.thread_id WHERE o.attempt_id=?1 AND o.item_type='function_call' AND t.deleted_at IS NULL AND t.cursor_node_id IS NOT (SELECT tail.node_id FROM thread_path tail WHERE tail.thread_id=t.id ORDER BY tail.ordinal DESC LIMIT 1))", [&attempt], |r|r.get(0))?;
                if bad_cursor {
                    return Err(RepairError::Structural(ToolRepairReason::CursorMismatch));
                }
                repair_calls(tx, &group, &attempt, &turn, at)?;
                splice_results(tx, &group, &attempt, at)?;
                tx.execute(
                    "UPDATE tool_group SET state='completed' WHERE id=?1",
                    [&group],
                )?;
                Ok(true)
            })();
            match repair {
                Ok(repaired) => {
                    tx.execute_batch("RELEASE startup_tool_group")?;
                    if repaired {
                        report.repaired_groups += 1;
                    } else {
                        report.retired_groups += 1;
                    }
                }
                Err(RepairError::Structural(reason)) => {
                    tx.execute_batch("ROLLBACK TO startup_tool_group; RELEASE startup_tool_group")?;
                    report.unresolved_groups += 1;
                    if report.failures.len() < 32 {
                        report.failures.push(crate::diagnostics::ToolRepairFailure {
                            thread_id: bounded_id(&thread),
                            group_id: Some(bounded_id(&group)),
                            reason,
                        });
                    }
                }
                Err(RepairError::Storage(error)) => return Err(fatal(error, &thread, &group)),
            }
            after = Some(group);
        }
    }
    Ok(report)
}

/// GC leaves call metadata after deliberately collecting deleted-owner payloads.
/// Skip only that explicit, unreachable tombstone state, never a retained error.
fn collected_source_group(
    tx: &rusqlite::Transaction<'_>,
    attempt: &str,
    turn: &str,
) -> Result<bool, StorageError> {
    let owner_deleted: bool = tx.query_row("SELECT t.deleted_at IS NOT NULL FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=?1", [turn], |r|r.get(0))?;
    if !owner_deleted {
        return Ok(false);
    }
    let invalid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM output_item o LEFT JOIN node n ON n.id=o.node_id WHERE o.attempt_id=?1 AND o.item_type='function_call' AND NOT COALESCE(n.payload_json IS NULL AND n.deleted_reason='thread_deleted_unreachable' AND json_extract(o.payload_json,'$.type')='shellboy_tombstone' AND json_extract(o.payload_json,'$.reason')='thread_deleted_unreachable' AND json_extract(o.payload_json,'$.node_id')=o.node_id,0))", [attempt], |r|r.get(0))?;
    if invalid {
        return Ok(false);
    }
    let sources = tx.prepare_cached("SELECT DISTINCT node_id FROM output_item WHERE attempt_id=?1 AND item_type='function_call' LIMIT 2")?
        .query_map([attempt], |r|r.get::<_,Option<String>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let [Some(source)] = sources.as_slice() else {
        return Ok(false);
    };
    Ok(!crate::history::reachability::searchable(tx, source)?)
}

fn repair_calls(
    tx: &rusqlite::Transaction<'_>,
    group: &str,
    attempt: &str,
    turn: &str,
    at: &str,
) -> RepairResult<()> {
    let mut after = -1i64;
    loop {
        let calls = tx.prepare_cached("SELECT c.call_id,c.ordinal,c.state,c.operation_id,r.node_id FROM tool_call c LEFT JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id WHERE c.group_id=?1 AND c.ordinal>?2 ORDER BY c.ordinal LIMIT 32")?
            .query_map(params![group,after], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if calls.is_empty() {
            break;
        }
        for (call, ordinal, state, execution, saved_result) in calls {
            after = ordinal;
            if !matches!(state.as_str(), "pending" | "running" | "completed") {
                return Err(RepairError::Structural(ToolRepairReason::UnknownCallState));
            }
            if saved_result.is_none() {
                let source: Option<String> = tx.query_row("SELECT node_id FROM output_item WHERE attempt_id=?1 AND item_type='function_call' AND json_extract(payload_json,'$.call_id')=?2", params![attempt,call], |r|r.get::<_,Option<String>>(0)).optional()?.flatten();
                if source.is_none() {
                    return Err(RepairError::Structural(ToolRepairReason::MissingCallSource));
                }
                let not_started = state == "pending" && execution.is_none();
                let mut result = json!({"status":if not_started {"not_started"} else {"interrupted"},
                    "reason":REASON,"result_uncertain":!not_started,"output_complete":false});
                if let Some(execution) = &execution {
                    result["execution_id"] = json!(execution);
                    let saved: Option<(String,Option<i64>,Option<bool>,String)> = tx.query_row("SELECT state,exit_code,output_complete,turn_id FROM shell_execution WHERE id=?1", [execution], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
                    if let Some((state, exit, complete, execution_turn)) = saved {
                        if execution_turn != turn {
                            return Err(RepairError::Structural(
                                ToolRepairReason::ExecutionTurnMismatch,
                            ));
                        }
                        result["saved_execution"] =
                            json!({"state":state,"exit_code":exit,"output_complete":complete});
                    }
                }
                let node = uuid::Uuid::new_v4().to_string();
                let payload = json!({"item":{"type":"function_call_output","call_id":call,"output":result.to_string()},"group_id":group,"startup_repair":true}).to_string();
                tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,?2,'tool_result',?3,?4,?5,?6)", params![node,turn,at,payload,payload.len() as i64,crate::digest::sha256_hex(payload.as_bytes())])?;
                tx.execute(
                    "INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?2,?3,?4)",
                    params![
                        group,
                        call,
                        node,
                        if not_started {
                            "not_started"
                        } else {
                            "interrupted"
                        }
                    ],
                )?;
            }
            tx.execute("UPDATE tool_call SET state='completed' WHERE group_id=?1 AND call_id=?2 AND state IN ('pending','running')", params![group,call])?;
        }
    }
    Ok(())
}

// This fragment identifies only the immutable results created in this startup
// and not selected in the destination yet. No existing result is moved.
const NEW_RESULTS: &str = "SELECT r.node_id,c.ordinal FROM tool_call c JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id JOIN node n ON n.id=r.node_id WHERE c.group_id=?1 AND n.created_at=?2 AND json_extract(n.payload_json,'$.startup_repair')=1 AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?3 AND p.node_id=r.node_id)";

fn splice_results(
    tx: &rusqlite::Transaction<'_>,
    group: &str,
    attempt: &str,
    at: &str,
) -> RepairResult<()> {
    let any_new: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_result r JOIN node n ON n.id=r.node_id WHERE r.group_id=?1 AND n.created_at=?2 AND json_extract(n.payload_json,'$.startup_repair')=1)", params![group,at], |r|r.get(0))?;
    if !any_new {
        return Ok(());
    }
    let sources = tx.prepare_cached("SELECT DISTINCT node_id FROM output_item WHERE attempt_id=?1 AND item_type='function_call' AND node_id IS NOT NULL LIMIT 2")?.query_map([attempt], |r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let [source] = sources.as_slice() else {
        return Err(RepairError::Structural(
            ToolRepairReason::AmbiguousCallSource,
        ));
    };
    let mut after: Option<String> = None;
    loop {
        let comparison = if after.is_some() { ">" } else { ">=" };
        let sql = format!(
            "SELECT t.id FROM thread t JOIN thread_path p ON p.thread_id=t.id WHERE p.node_id=?1 AND t.deleted_at IS NULL AND t.id {comparison} ?2 ORDER BY t.id LIMIT {PAGE}"
        );
        let threads = tx
            .prepare_cached(&sql)?
            .query_map(params![source, after.as_deref().unwrap_or("")], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if threads.is_empty() {
            break;
        }
        for thread in threads {
            splice_thread(tx, group, source, &thread, at)?;
            after = Some(thread);
        }
    }
    Ok(())
}

fn splice_thread(
    tx: &rusqlite::Transaction<'_>,
    group: &str,
    source: &str,
    thread: &str,
    at: &str,
) -> RepairResult<()> {
    let cursor: Option<String> = tx.query_row(
        "SELECT cursor_node_id FROM thread WHERE id=?1",
        [thread],
        |r| r.get(0),
    )?;
    let tail: Option<String> = tx
        .query_row(
            "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1",
            [thread],
            |r| r.get(0),
        )
        .optional()?;
    if cursor != tail {
        return Err(RepairError::Structural(ToolRepairReason::CursorMismatch));
    }
    let count_sql = format!("SELECT count(*) FROM ({NEW_RESULTS})");
    let count: i64 = tx.query_row(&count_sql, params![group, at, thread], |r| r.get(0))?;
    if count == 0 {
        return Ok(());
    }
    let (anchor_ordinal, anchor): (i64,String) = tx.query_row("SELECT p.ordinal,p.node_id FROM thread_path p WHERE p.thread_id=?1 AND (p.node_id=?2 OR p.node_id IN (SELECT node_id FROM tool_result WHERE group_id=?3)) ORDER BY p.ordinal DESC LIMIT 1", params![thread,source,group], |r|Ok((r.get(0)?,r.get(1)?)))?;
    let next: Option<String> = tx.query_row("SELECT node_id FROM thread_path WHERE thread_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT 1", params![thread,anchor_ordinal], |r|r.get(0)).optional()?;
    let last_sql = format!("SELECT node_id FROM ({NEW_RESULTS}) ORDER BY ordinal DESC LIMIT 1");
    let last: String = tx.query_row(&last_sql, params![group, at, thread], |r| r.get(0))?;
    let edge_sql = format!(
        "WITH repaired AS (SELECT node_id,lag(node_id) OVER(ORDER BY ordinal) AS previous FROM ({NEW_RESULTS})) INSERT INTO edge(src,dst) SELECT COALESCE(previous,?4),node_id FROM repaired x WHERE NOT EXISTS(SELECT 1 FROM edge e WHERE e.src=COALESCE(x.previous,?4) AND e.dst=x.node_id)"
    );
    tx.execute(&edge_sql, params![group, at, thread, anchor])?;
    if let Some(next) = next {
        tx.execute("INSERT INTO edge(src,dst) SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM edge WHERE src=?1 AND dst=?2)", params![last,next])?;
    }
    let maximum: i64 = tx.query_row(
        "SELECT max(ordinal) FROM thread_path WHERE thread_id=?1",
        [thread],
        |r| r.get(0),
    )?;
    let shift = maximum
        .checked_add(count)
        .and_then(|n| n.checked_add(1))
        .ok_or(RepairError::Structural(ToolRepairReason::OrdinalOverflow))?;
    let threshold = anchor_ordinal
        .checked_add(1)
        .ok_or(RepairError::Structural(ToolRepairReason::OrdinalOverflow))?;
    maximum
        .checked_add(shift)
        .ok_or(RepairError::Structural(ToolRepairReason::OrdinalOverflow))?;
    let shifted_threshold = threshold
        .checked_add(shift)
        .ok_or(RepairError::Structural(ToolRepairReason::OrdinalOverflow))?;
    tx.execute(
        "UPDATE thread_path SET ordinal=ordinal+?3 WHERE thread_id=?1 AND ordinal>=?2",
        params![thread, threshold, shift],
    )?;
    let insert_sql = format!(
        "INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT ?3,?4+row_number() OVER(ORDER BY ordinal)-1,node_id FROM ({NEW_RESULTS})"
    );
    tx.execute(&insert_sql, params![group, at, thread, threshold])?;
    tx.execute(
        "UPDATE thread_path SET ordinal=ordinal-?3+?4 WHERE thread_id=?1 AND ordinal>=?2",
        params![thread, shifted_threshold, shift, count],
    )?;
    tx.execute("UPDATE thread SET cursor_node_id=(SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1),selection_revision=selection_revision+1,revision=revision+1,updated_at=?2 WHERE id=?1", params![thread,at])?;
    if tx.execute(
        "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
        [],
    )? != 1
    {
        return Err(
            StorageError::Schema("Interrupted result root revision is missing".into()).into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','old');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('run','t','stopped','old');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','t','run','stopped','old');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','turn',0,0,'p','m',0,'completed','{}','old');
            INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('group','attempt','turn','running','old');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,output_complete,end_reason,started_at)VALUES('exec','run','turn','exec','stopped','/','{\"command\":\"never repeated\"}','UTF-8','UTF-8','closed',0,'previous_instance_interrupted','old');").unwrap();
        let tx = db.transaction().unwrap();
        let calls = ["a", "b", "c"].map(|call|json!({"type":"function_call","name":"shell","call_id":call,"arguments":"{\"command\":\"never repeated\"}"}));
        crate::history::append_node(
            &tx,
            "t",
            "source",
            Some("turn"),
            "assistant_response",
            &json!({"output":calls,"group_id":"group"}),
            "old",
        )
        .unwrap();
        for (ordinal, call) in calls.iter().enumerate() {
            let id = call["call_id"].as_str().unwrap();
            tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('attempt',?1,'function_call','completed',?2,'source')", params![ordinal as i64,call.to_string()]).unwrap();
            tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('group',?1,?2,'shell','{}',?3,?4)",params![id,ordinal as i64,if id=="a"{"completed"}else if id=="b"{"running"}else{"pending"},(id=="b").then_some("exec")]).unwrap();
        }
        crate::history::append_node(
            &tx,
            "t",
            "audit-a",
            None,
            "interruption_context",
            &json!({"text":"old interruption snapshot"}),
            "old",
        )
        .unwrap();
        crate::history::append_node(&tx,"t","real-a",Some("turn"),"tool_result",&json!({"item":{"type":"function_call_output","call_id":"a","output":"{\"status\":\"exited\",\"exit_code\":0}"},"group_id":"group"}),"old").unwrap();
        tx.execute(
            "INSERT INTO tool_result VALUES('group','a','real-a','exited')",
            [],
        )
        .unwrap();
        crate::history::append_node(
            &tx,
            "t",
            "audit-b",
            None,
            "interruption_details",
            &json!({"text":"later interruption detail"}),
            "old",
        )
        .unwrap();
        tx.commit().unwrap();
        db
    }
    fn path(db: &Connection) -> Vec<String> {
        db.prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    fn edges(db: &Connection) -> Vec<(String, String)> {
        db.prepare("SELECT src,dst FROM edge ORDER BY src,dst")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    #[test]
    fn bad_group_rolls_back_only_itself_and_other_thread_repairs_continue() {
        let mut db = fixture();
        db.execute_batch(r#"
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('bad-thread','/','/','p','m','old');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('bad-run','bad-thread','stopped','old');
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('bad-turn','bad-thread','bad-run','stopped','old');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('bad-attempt','bad-turn',0,0,'p','m',0,'completed','{}','old');
            INSERT INTO tool_group VALUES('aaa-bad','bad-attempt','bad-turn','running','old');
            INSERT INTO tool_call VALUES('aaa-bad','first',0,'shell','{}','running','bad-exec');
            INSERT INTO tool_call VALUES('aaa-bad','missing',1,'shell','{}','pending',NULL);
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('bad-exec','bad-run','bad-turn','bad-exec','stopped','/','{}','UTF-8','UTF-8','closed','old');
        "#).unwrap();
        {
            let tx = db.transaction().unwrap();
            let output = json!([{"type":"function_call","call_id":"first","name":"shell","arguments":"{}"},{"type":"function_call","call_id":"missing","name":"shell","arguments":"{}"}]);
            crate::history::append_node(
                &tx,
                "bad-thread",
                "bad-source",
                Some("bad-turn"),
                "assistant_response",
                &json!({"output":output,"group_id":"aaa-bad"}),
                "old",
            )
            .unwrap();
            tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id) VALUES('bad-attempt',0,'function_call','completed',?1,'bad-source')",[output[0].to_string()]).unwrap();
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('bad-attachment','attachment_source','old','{}')",[]).unwrap();
            tx.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at) VALUES('bad-attachment','t','held','bad-op',?1,'old')",[json!({"kind":"shell","execution_id":"bad-exec","run_id":"bad-run"}).to_string()]).unwrap();
            tx.commit().unwrap();
        }
        let before: i64 = db
            .query_row(
                "SELECT selection_revision FROM thread WHERE id='bad-thread'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let tx = db.transaction().unwrap();
        let report = repair_interrupted_tool_groups(&tx, "startup").unwrap();
        assert_eq!(report.repaired_groups, 1);
        assert_eq!(report.unresolved_groups, 1);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].thread_id, "bad-thread");
        assert_eq!(report.failures[0].group_id.as_deref(), Some("aaa-bad"));
        assert!(matches!(
            report.failures[0].reason,
            ToolRepairReason::MissingCallSource
        ));
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM tool_result WHERE group_id='aaa-bad'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM node WHERE turn_id='bad-turn'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM thread_path WHERE thread_id='bad-thread'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            tx.query_row(
                "SELECT selection_revision FROM thread WHERE id='bad-thread'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            before
        );
        assert_eq!(
            tx.query_row("SELECT state FROM tool_group WHERE id='aaa-bad'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "running"
        );
        assert_eq!(
            tx.query_row(
                "SELECT state FROM tool_call WHERE group_id='aaa-bad' AND call_id='first'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "running"
        );
        assert_eq!(
            tx.query_row(
                "SELECT status FROM attachment_delivery WHERE node_id='bad-attachment'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "held"
        );
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM tool_result WHERE group_id='group'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        tx.commit().unwrap();
    }

    #[test]
    fn unresolved_group_diagnostic_previews_are_bounded() {
        let mut db = fixture();
        for i in 0..40 {
            db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES(?1,'attempt','turn','unknown','old')",[format!("bad-{i:02}")]).unwrap();
        }
        let tx = db.transaction().unwrap();
        let report = repair_interrupted_tool_groups(&tx, "startup").unwrap();
        assert_eq!(report.unresolved_groups, 40);
        assert_eq!(report.failures.len(), 32);
        assert_eq!(report.repaired_groups, 1);
        tx.commit().unwrap();
    }

    #[test]
    fn bad_cursor_is_reported_even_when_every_real_result_already_exists() {
        let mut db = fixture();
        {
            let tx = db.transaction().unwrap();
            repair_interrupted_tool_groups(&tx, "first-startup").unwrap();
            tx.commit().unwrap();
        }
        db.execute_batch("UPDATE tool_group SET state='running' WHERE id='group'; UPDATE thread SET cursor_node_id='source' WHERE id='t';").unwrap();
        let before = path(&db);
        let tx = db.transaction().unwrap();
        let report = repair_interrupted_tool_groups(&tx, "second-startup").unwrap();
        assert_eq!(report.unresolved_groups, 1);
        assert_eq!(report.repaired_groups, 0);
        assert!(matches!(
            report.failures[0].reason,
            ToolRepairReason::CursorMismatch
        ));
        assert_eq!(
            tx.query_row("SELECT state FROM tool_group WHERE id='group'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "running"
        );
        tx.commit().unwrap();
        assert_eq!(path(&db), before);
    }

    #[test]
    fn missing_global_root_revision_remains_fatal_with_group_context() {
        let mut db = fixture();
        db.execute("DELETE FROM app_meta WHERE key='root_revision'", [])
            .unwrap();
        let tx = db.transaction().unwrap();
        let error = repair_interrupted_tool_groups(&tx, "startup")
            .unwrap_err()
            .into_api_error();
        assert_eq!(error.code, ErrorCode::SaveFailed);
        assert_eq!(error.details.unwrap()["group_id"], "group");
        assert!(error.message.contains("root revision"));
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM tool_result", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn terminal_turn_repairs_partial_group_without_moving_or_rewriting_real_results() {
        let mut db = fixture();
        let original_edges = edges(&db);
        let real: (String, i64, String) = db
            .query_row(
                "SELECT payload_json,payload_revision,body_hash FROM node WHERE id='real-a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let tx = db.transaction().unwrap();
        repair_interrupted_tool_groups(&tx, "startup").unwrap();
        tx.commit().unwrap();
        let rows = db.prepare("SELECT r.call_id,r.node_id,n.payload_json FROM tool_result r JOIN node n ON n.id=r.node_id WHERE r.group_id='group' ORDER BY r.call_id").unwrap().query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, "real-a");
        for (call, _, payload) in &rows[1..] {
            let payload: Value = serde_json::from_str(payload).unwrap();
            assert_eq!(payload["group_id"], "group");
            assert_eq!(payload["item"]["call_id"], call.as_str());
            let result: Value =
                serde_json::from_str(payload["item"]["output"].as_str().unwrap()).unwrap();
            assert_eq!(result["reason"], REASON);
            assert_eq!(
                result["status"],
                if call == "b" {
                    "interrupted"
                } else {
                    "not_started"
                }
            );
            assert_eq!(result["result_uncertain"], call == "b");
            assert_eq!(result["output_complete"], false);
            if call == "b" {
                assert_eq!(result["execution_id"], "exec");
                assert_eq!(result["saved_execution"]["state"], "stopped");
            } else {
                assert!(result.get("execution_id").is_none());
            }
        }
        assert_eq!(
            path(&db),
            vec![
                "source".into(),
                "audit-a".into(),
                "real-a".into(),
                rows[1].1.clone(),
                rows[2].1.clone(),
                "audit-b".into()
            ]
        );
        assert_eq!(
            db.query_row(
                "SELECT payload_json,payload_revision,body_hash FROM node WHERE id='real-a'",
                [],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?
                ))
            )
            .unwrap(),
            real
        );
        let new_edges = edges(&db);
        assert!(original_edges.iter().all(|edge| new_edges.contains(edge)));
        assert_eq!(db.query_row("WITH RECURSIVE reachable(a,b) AS(SELECT src,dst FROM edge UNION SELECT r.a,e.dst FROM reachable r JOIN edge e ON e.src=r.b)SELECT count(*) FROM reachable WHERE a=b",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM tool_call WHERE state<>'completed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let unchanged = (
            path(&db),
            edges(&db),
            db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            db.query_row(
                "SELECT selection_revision FROM thread WHERE id='t'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        );
        let tx = db.transaction().unwrap();
        repair_interrupted_tool_groups(&tx, "second-startup").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            (
                path(&db),
                edges(&db),
                db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                db.query_row(
                    "SELECT selection_revision FROM thread WHERE id='t'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap()
            ),
            unchanged
        );
    }
    #[test]
    fn repair_rolls_back_nodes_calls_and_path_if_a_later_result_cannot_be_saved() {
        let mut db = fixture();
        let before = (
            path(&db),
            edges(&db),
            db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
                .unwrap(),
        );
        db.execute_batch("CREATE TRIGGER reject_final_repair BEFORE INSERT ON tool_result WHEN NEW.call_id='c' BEGIN SELECT RAISE(ABORT,'synthetic result save failure'); END;").unwrap();
        {
            let tx = db.transaction().unwrap();
            let error = repair_interrupted_tool_groups(&tx, "startup")
                .unwrap_err()
                .into_api_error();
            assert_eq!(error.code, ErrorCode::SaveFailed);
            assert_eq!(error.details.as_ref().unwrap()["thread_id"], "t");
            assert_eq!(error.details.as_ref().unwrap()["group_id"], "group");
        }
        assert_eq!(
            (
                path(&db),
                edges(&db),
                db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
                    .unwrap()
            ),
            before
        );
        assert_eq!(
            db.query_row("SELECT state FROM tool_call WHERE call_id='b'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "running"
        );
        assert_eq!(
            db.query_row("SELECT state FROM tool_group", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "running"
        );
    }
    #[test]
    fn a_running_group_with_all_real_results_is_finalized_without_new_history() {
        let mut db = fixture();
        {
            let tx = db.transaction().unwrap();
            repair_interrupted_tool_groups(&tx, "startup").unwrap();
            tx.commit().unwrap();
        }
        db.execute("UPDATE tool_group SET state='running'", [])
            .unwrap();
        let before = (
            path(&db),
            edges(&db),
            db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
                .unwrap(),
        );
        {
            let tx = db.transaction().unwrap();
            repair_interrupted_tool_groups(&tx, "second-startup").unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            (
                path(&db),
                edges(&db),
                db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
                    .unwrap()
            ),
            before
        );
        assert_eq!(
            db.query_row("SELECT state FROM tool_group", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "completed"
        );
    }
    fn collect_deleted_fixture(db: &Connection) {
        // Exact persisted shape produced by gc::history::tombstone_node, while
        // retaining lifecycle metadata as ordinary history GC does.
        db.execute_batch("UPDATE thread SET deleted_at='deleted' WHERE id='t';
            UPDATE node SET payload_json=NULL,payload_bytes=0,payload_revision=payload_revision+1,deleted_reason='thread_deleted_unreachable';
            UPDATE output_item SET payload_json=json_object('type','shellboy_tombstone','reason','thread_deleted_unreachable','node_id',node_id);").unwrap();
    }
    #[test]
    fn collected_deleted_groups_do_not_block_repair_of_an_unrelated_live_thread() {
        let mut db = fixture();
        collect_deleted_fixture(&db);
        let tx = db.transaction().unwrap();
        tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('live','/','/AGENTS.md','p','m','old');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('live-run','live','stopped','old');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('live-turn','live','live-run','stopped','old');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('live-attempt','live-turn',0,0,'p','m',0,'completed','{}','old');
            INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('z-live-group','live-attempt','live-turn','running','old');
            INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('z-live-group','live-call',0,'shell','{}','pending');").unwrap();
        let call =
            json!({"type":"function_call","name":"shell","call_id":"live-call","arguments":"{}"});
        crate::history::append_node(
            &tx,
            "live",
            "live-source",
            Some("live-turn"),
            "assistant_response",
            &json!({"output":[call],"group_id":"z-live-group"}),
            "old",
        )
        .unwrap();
        tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('live-attempt',0,'function_call','completed',?1,'live-source')",[call.to_string()]).unwrap();
        repair_interrupted_tool_groups(&tx, "startup").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT state FROM tool_group WHERE id='z-live-group'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "completed"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM tool_result WHERE group_id='z-live-group'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT state FROM tool_group WHERE id='group'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "running",
            "retired lifecycle records are not fabricated as completed"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM tool_result WHERE group_id='group'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert!(
            db.query_row(
                "SELECT payload_json IS NULL FROM node WHERE id='source'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );
        let nodes: i64 = db
            .query_row("SELECT count(*) FROM node", [], |r| r.get(0))
            .unwrap();
        let tx = db.transaction().unwrap();
        repair_interrupted_tool_groups(&tx, "second-startup").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            nodes
        );
    }
    #[test]
    fn inconsistent_retained_or_shared_gc_tombstones_are_not_silently_skipped() {
        for consumer in ["owner", "fork", "summary"] {
            let mut db = fixture();
            collect_deleted_fixture(&db);
            match consumer {
                "owner" => {
                    db.execute("UPDATE thread SET deleted_at=NULL WHERE id='t'", [])
                        .unwrap();
                }
                "fork" => {
                    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,cursor_node_id)VALUES('live','/','/AGENTS.md','p','m','old','source');INSERT INTO thread_path VALUES('live',0,'source');").unwrap();
                }
                "summary" => {
                    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('live','/','/AGENTS.md','p','m','old');").unwrap();
                    let tx = db.transaction().unwrap();
                    crate::history::append_node(
                        &tx,
                        "live",
                        "summary",
                        None,
                        "summary",
                        &json!({"text":"retained summary"}),
                        "old",
                    )
                    .unwrap();
                    tx.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('replacement','summary','fixture','summary-v1','fixture','completed','old')",[]).unwrap();
                    tx.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('replacement',0,'source',1)",[]).unwrap();
                    tx.execute(
                        "INSERT INTO thread_replacement VALUES('live','replacement')",
                        [],
                    )
                    .unwrap();
                    tx.commit().unwrap();
                }
                _ => unreachable!(),
            }
            let tx = db.transaction().unwrap();
            let report = repair_interrupted_tool_groups(&tx, "startup").unwrap();
            assert_eq!(
                report.retired_groups, 0,
                "{consumer}: retained source cannot be treated as collected"
            );
            assert_eq!(report.repaired_groups, 0, "{consumer}");
            assert_eq!(report.unresolved_groups, 1, "{consumer}");
            assert_eq!(report.failures.len(), 1, "{consumer}");
            assert!(
                matches!(
                    report.failures[0].reason,
                    ToolRepairReason::MissingCallSource
                ),
                "{consumer}"
            );
            assert_eq!(
                tx.query_row("SELECT state FROM tool_group WHERE id='group'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                "running"
            );
            assert_eq!(
                tx.query_row(
                    "SELECT count(*) FROM tool_result WHERE group_id='group'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
    }
}
