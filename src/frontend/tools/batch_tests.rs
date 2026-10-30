use super::*;
use crate::frontend::sql_trace::Statements;
use serde_json::{Value, json};

fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
      INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
      INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now'),('foreign','t','r','completed','now');
      INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','now'),('other-a','u',0,0,'p','m',0,'completed','{}','now');
      INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now'),('other','other-a','u','completed','now');").unwrap();
    for n in 0..150 {
        let call = format!("call-{n}");
        let node = format!("result-{n}");
        let execution = format!("exec-{n}");
        let status = if n == 10 {
            "cancelled"
        } else if n == 9 {
            "running"
        } else {
            "exited"
        };
        let output =
            json!({"status":status,"reason":if n==9 {"human_backgrounded"}else{"completed"}});
        let payload = json!({"group_id":"g","item":{"type":"function_call_output","call_id":call,"output":output.to_string()}});
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','tool_result','now',?2)",rusqlite::params![node,payload.to_string()]).unwrap();
        db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g',?1,?2,'shell',?3,?4,?5)",rusqlite::params![call,100+n*17,json!({"command":format!("printf {n}"),"large_ignored_argument":"x".repeat(2048)}).to_string(),if n==10 {"cancelled"}else{"completed"},execution]).unwrap();
        db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,exit_code)VALUES(?1,'r',?2,?1,'exited','/',?3,'UTF-8','UTF-8','closed','now',0)",rusqlite::params![execution,if n==1 {"foreign"}else{"u"},json!({"completion":{"elapsed_ms":123},"large_ignored_metadata":"y".repeat(2048)}).to_string()]).unwrap();
        db.execute(
            "INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g',?1,?2,?3)",
            rusqlite::params![call, node, status],
        )
        .unwrap();
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','assistant_response','now',?2)",rusqlite::params![format!("direct-{n}"),json!({"group_id":"g","item":{"type":"function_call","call_id":call}}).to_string()]).unwrap();
        db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('a',?1,'function_call','completed',?2,?3)",rusqlite::params![n+10,json!({"call_id":call}).to_string(),format!("direct-{n}")]).unwrap();
    }
    let whole = json!({"group_id":"g","response":{"output":(0..150).map(|n|json!({"type":"function_call","call_id":format!("call-{n}")})).collect::<Vec<_>>()}});
    for (id, kind, payload) in [
        ("whole", "assistant_response", whole),
        (
            "spoof",
            "tool_result",
            json!({"group_id":"g","item":{"type":"function_call_output","call_id":"call-0"}}),
        ),
        (
            "unknown",
            "assistant_response",
            json!({"item":{"type":"function_call","call_id":"call-0"}}),
        ),
        (
            "message",
            "assistant_response",
            json!({"group_id":"g","item":{"type":"message","content":[]}}),
        ),
        (
            "other-result",
            "tool_result",
            json!({"group_id":"other","item":{"type":"function_call_output","call_id":"call-0"}}),
        ),
    ] {
        db.execute(
            "INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u',?2,'now',?3)",
            rusqlite::params![id, kind, payload.to_string()],
        )
        .unwrap();
    }
    db.execute_batch("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('other','call-0',5,'shell','{\"command\":\"OTHER GROUP\"}','completed'); INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('other','call-0','other-result','exited');").unwrap();
    db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('first-owner','u','assistant_response','now','{\"group_id\":\"g\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call-0\"}}')",[]).unwrap();
    // Earlier valid output-item mapping must win, including an explicit null
    // mapping: a later non-null item cannot silently replace that first item.
    db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('a',0,'function_call','completed','{\"call_id\":\"call-0\"}','first-owner'),('a',1,'function_call','completed','{\"call_id\":\"call-2\"}',NULL)",[]).unwrap();
    db
}
fn facts(facts: &ToolFacts) -> Vec<Value> {
    facts.sources().iter().map(|source|json!({"node":source.node_id,"call":source.call_id,"ordinal":source.ordinal,"command":source.command,
        "state":source.result_state,"shell":source.shell_state,"code":source.shell_exit_code,"released":source.released,"group":source.group_id,
        "prior":source.prior_reason,"execution":source.execution_id,"call_node":source.call_node_id,"result_node":source.result_node_id,"elapsed":source.elapsed_ms})).collect()
}

#[test]
fn fifty_direct_results_execute_fixed_two_statements_with_exact_source_parity() {
    let db = fixture();
    let ids = (0..50).map(|n| format!("result-{n}")).collect::<Vec<_>>();
    let trace = Statements::new(&db);
    let batch = read_nodes_tx(&db, &ids).unwrap();
    assert_eq!(
        trace.count(),
        2,
        "one selector batch and one source-facts batch, not100 node/call statements"
    );
    drop(trace);
    let requests = (0..50)
        .map(|n| (format!("result-{n}"), format!("call-{n}")))
        .collect::<Vec<_>>();
    let exact = read_tx_reference(&db, &requests).unwrap();
    assert_eq!(facts(&batch), facts(&exact));
    assert_eq!(batch.sources().len(), 50);
    assert_eq!(
        batch.sources()[0].call_node_id.as_deref(),
        Some("first-owner")
    );
    let cancelled = batch
        .sources()
        .iter()
        .find(|source| source.call_id == "call-10")
        .unwrap();
    assert_eq!(
        cancelled.prior_reason.as_deref(),
        Some("human_backgrounded")
    );
    assert!(
        batch
            .sources()
            .iter()
            .find(|source| source.call_id == "call-1")
            .unwrap()
            .shell_state
            .is_none(),
        "foreign Turn shell never supplies current state"
    );
    assert!(
        batch
            .sources()
            .iter()
            .find(|source| source.call_id == "call-2")
            .unwrap()
            .call_node_id
            .is_none(),
        "first valid item's null mapping is retained"
    );
}

#[test]
fn mixed_whole_direct_duplicate_and_spoof_scopes_cap_real_sparse_calls_in_three_statements() {
    let db = fixture();
    let ids = [
        "direct-0",
        "whole",
        "direct-0",
        "other-result",
        "spoof",
        "unknown",
        "message",
    ]
    .map(str::to_owned);
    let trace = Statements::new(&db);
    let batch = read_nodes_tx(&db, &ids).unwrap();
    assert_eq!(trace.count(), 3);
    drop(trace);
    let mut requests = vec![("direct-0".into(), "call-0".into())];
    requests.extend((0..128).map(|n| ("whole".into(), format!("call-{n}"))));
    requests.push(("other-result".into(), "call-0".into()));
    assert_eq!(
        facts(&batch),
        facts(&read_tx_reference(&db, &requests).unwrap())
    );
    assert_eq!(batch.truncated_nodes, ["whole"]);
    let whole = batch
        .sources()
        .iter()
        .filter(|source| source.node_id == "whole")
        .collect::<Vec<_>>();
    assert_eq!(whole.len(), 128);
    assert_eq!(whole[0].ordinal, 100);
    assert_eq!(whole[127].ordinal, 100 + 127 * 17);
    assert!(whole.iter().all(|source| source.call_id != "call-128"));
    assert_eq!(
        batch
            .sources()
            .iter()
            .filter(|source| source.node_id == "direct-0")
            .count(),
        1
    );
    assert_eq!(
        batch.sources().last().unwrap().command.as_deref(),
        Some("OTHER GROUP")
    );
}

#[test]
fn empty_and_oversized_scope_execute_no_sql_and_group_seek_has_index_range_proof() {
    let db = fixture();
    let trace = Statements::new(&db);
    assert!(read_nodes_tx(&db, &[]).unwrap().sources().is_empty());
    assert_eq!(
        read_nodes_tx(&db, &vec!["whole".into(); 129])
            .err()
            .unwrap()
            .into_api_error()
            .code,
        ErrorCode::InputLimit
    );
    assert_eq!(trace.count(), 0);
    drop(trace);
    let plan = db
        .prepare(&format!("EXPLAIN QUERY PLAN {GROUP_CALL_BATCH_SQL}"))
        .unwrap()
        .query_map(["[\"g\"]"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(
        plan.contains("SEARCH c") && plan.contains("group_id=? AND ordinal>?)"),
        "{plan}"
    );
}

#[test]
fn projected_fifty_call_scope_is_two_statements_and_matches_independent_old_oracle() {
    let db = fixture();
    let requests = (0..50)
        .map(|n| (format!("result-{n}"), format!("call-{n}")))
        .collect::<Vec<_>>();
    let trace = Statements::new(&db);
    let batch = read_tx(&db, &requests).unwrap();
    assert_eq!(
        trace.count(),
        2,
        "one saved-source resolver and one shared facts statement"
    );
    drop(trace);
    assert_eq!(
        facts(&batch),
        facts(&read_tx_reference(&db, &requests).unwrap())
    );
}

#[test]
fn projected_unique_legacy_ambiguous_collected_and_repeated_relations_keep_old_semantics() {
    let db = fixture();
    for (id,kind,payload,reason) in [
        ("legacy","tool_result",Some(json!({"item":{"type":"function_call_output","call_id":"call-0"}}).to_string()),None),
        ("ambiguous","tool_result",Some("{}".into()),None),
        ("collected","tool_result",None,Some("thread_deleted_unreachable")),
        ("wrong-group","tool_result",Some(json!({"group_id":"other","item":{"type":"function_call_output","call_id":"call-3"}}).to_string()),None),
        ("other-kind-linked","event",Some("{}".into()),None),
    ] {
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,deleted_reason)VALUES(?1,'u',?2,'now',?3,?4)",rusqlite::params![id,kind,payload,reason]).unwrap();
    }
    for (call, node) in [
        ("call-0", "legacy"),
        ("call-1", "ambiguous"),
        ("call-2", "collected"),
        ("call-3", "wrong-group"),
        ("call-4", "other-kind-linked"),
    ] {
        db.execute(
            "UPDATE tool_result SET node_id=?2 WHERE group_id='g' AND call_id=?1",
            rusqlite::params![call, node],
        )
        .unwrap();
    }
    db.execute_batch("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('other','call-1',6,'shell','{}','completed'); INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('other','call-1','ambiguous','exited');").unwrap();
    let requests = [
        ("legacy", "call-0"),
        ("ambiguous", "call-1"),
        ("collected", "call-2"),
        ("missing", "call-0"),
        ("wrong-group", "call-3"),
        ("other-kind-linked", "call-4"),
        ("legacy", "call-0"),
    ]
    .map(|(node, call)| (node.to_owned(), call.to_owned()));
    let trace = Statements::new(&db);
    let batch = read_tx(&db, &requests).unwrap();
    assert_eq!(trace.count(), 2);
    drop(trace);
    assert_eq!(
        facts(&batch),
        facts(&read_tx_reference(&db, &requests).unwrap())
    );
    assert_eq!(
        batch
            .sources()
            .iter()
            .map(|source| source.node_id.as_str())
            .collect::<Vec<_>>(),
        ["legacy", "collected", "other-kind-linked", "legacy"]
    );
    // A saved group takes precedence over an ambiguous result fallback, exactly
    // as before; do not replace it with a guessed group by call ID.
    db.execute(
        "UPDATE node SET payload_json='{\"group_id\":\"g\"}' WHERE id='ambiguous'",
        [],
    )
    .unwrap();
    let requests = [("ambiguous".into(), "call-1".into())];
    let batch = read_tx(&db, &requests).unwrap();
    assert_eq!(
        facts(&batch),
        facts(&read_tx_reference(&db, &requests).unwrap())
    );
    assert_eq!(batch.sources().len(), 1);
    assert_eq!(batch.sources()[0].group_id, "g");
}

#[test]
fn projected_empty_and_bound_failures_do_not_execute_sql() {
    let db = fixture();
    let trace = Statements::new(&db);
    assert!(read_tx(&db, &[]).unwrap().sources().is_empty());
    let too_many_nodes = (0..129)
        .map(|n| (format!("node-{n}"), "call".into()))
        .collect::<Vec<_>>();
    assert_eq!(
        read_tx(&db, &too_many_nodes)
            .err()
            .unwrap()
            .into_api_error()
            .code,
        ErrorCode::InputLimit
    );
    let too_many_calls = vec![("node".into(), "call".into()); MAX_NODES * MAX_GROUP_CALLS + 1];
    assert_eq!(
        read_tx(&db, &too_many_calls)
            .err()
            .unwrap()
            .into_api_error()
            .code,
        ErrorCode::InputLimit
    );
    assert_eq!(trace.count(), 0);
}

#[test]
fn collected_source_lookup_uses_existing_index_without_unrelated_relation_scan() {
    use rusqlite::StatementStatus;

    let mut db = fixture();
    // Current GC may retire payloads while retaining exact tool-result relations.
    db.execute("UPDATE node SET payload_json=NULL WHERE id='result-0'", [])
        .unwrap();
    let columns = db
        .prepare("PRAGMA index_info(tool_result_node)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(2))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(columns, vec!["node_id"]);
    let encoded = serde_json::to_string(
        &(0..50)
            .map(|n| (format!("result-{n}"), format!("call-{n}")))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let measure = |db: &Connection| {
        let mut statement = db.prepare(PROJECTED_SOURCE_BATCH_SQL).unwrap();
        let rows = statement
            .query_map([&encoded], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 50);
        assert_eq!(rows[0], ("result-0".into(), "g".into()));
        statement.get_status(StatementStatus::VmStep)
    };
    let small = measure(&db);
    let tx = db.transaction().unwrap();
    for n in 0..10_000 {
        let id = format!("unrelated-{n}");
        tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','tool_result','now',NULL)",[&id]).unwrap();
        tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g',?1,?2,'shell','{}','completed')",rusqlite::params![id,100_000+n]).unwrap();
        tx.execute(
            "INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g',?1,?1,'exited')",
            [&id],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let large = measure(&db);
    assert!(
        large <= small + 100,
        "unrelated relations must not cause linear work: {small} -> {large} VM steps"
    );
    let plan = db
        .prepare(&format!("EXPLAIN QUERY PLAN {PROJECTED_SOURCE_BATCH_SQL}"))
        .unwrap()
        .query_map([encoded], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter().any(|step| step.contains("tool_result_node")),
        "{plan:?}"
    );
}
