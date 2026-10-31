use super::*;
use crate::{app::DataOwner, presentation, ui::UiNode};
use serde_json::json;

#[tokio::test]
async fn repeated_call_ids_in_other_groups_cannot_replace_a_cancelled_calls_source() {
    let root = tempfile::tempdir().unwrap();
    let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let payload = json!({"group_id":"g","item":{"type":"function_call_output","call_id":"same","output":json!({"status":"cancelled","reason":"Run stopped before this call"}).to_string()}});
    let saved = payload.to_string();
    control.store.database.call(move|db|{
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','now'); INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now'),('other','a','u','completed','now'); INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('other','same',0,'shell','{\"command\":\"WRONG GROUP\"}','completed'),('g','same',2,'shell','{\"command\":\"printf 正しい🦀\"}','completed');")?;
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('result','tool_result','now',?1,length(?1))",[saved])?;
        db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','same','result','cancelled')",[])?;
        Ok(())
    }).await.unwrap();
    let text = crate::history::text_of_payload(&payload);
    let mut nodes = vec![UiNode {
        node_id: "result".into(),
        role: "tool".into(),
        text: text.clone(),
        base_char_offset: 0,
        total_chars: text.chars().count(),
        reasoning_ranges: vec![],
        presentation: presentation::project("tool_result", &payload, &text, 0),
    }];
    enrich(&control, &mut nodes).await.unwrap();
    let tool = nodes[0].presentation.blocks[0].tool.as_ref().unwrap();
    assert_eq!(tool.ordinal, Some(2));
    assert_eq!(tool.command, "printf 正しい🦀");
    assert_eq!(tool.status, "cancelled");
    assert!(
        tool.execution_id.is_none(),
        "cancelled/unadmitted calls have no invented shell identity"
    );
    assert_eq!(tool.reason, "作業停止のため取消");
    assert_eq!(nodes[0].text, text);
    assert_eq!(nodes[0].presentation.blocks[0].source_start, 0);
    // Missing/deleted source is not borrowed from another group with the same ID.
    nodes[0].node_id = "missing".into();
    nodes[0].presentation = presentation::project("tool_result", &payload, &text, 0);
    enrich(&control, &mut nodes).await.unwrap();
    let tool = nodes[0].presentation.blocks[0].tool.as_ref().unwrap();
    assert_eq!(tool.ordinal, None);
    assert!(tool.command.is_empty());
    assert!(tool.heading().contains("原文参照"));
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn call_and_background_result_share_current_exit_facts_and_failed_reads_clear_them() {
    let root = tempfile::tempdir().unwrap();
    let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let call = json!({"group_id":"g","output":[{"type":"function_call","name":"shell","call_id":"c","arguments":json!({"command":"printf fixture"}).to_string()}]});
    let result = json!({"group_id":"g","item":{"type":"function_call_output","call_id":"c","output":json!({"status":"running","execution_id":"e","reason":"human_backgrounded"}).to_string()}});
    let payloads = [call.to_string(), result.to_string()];
    control.store.database.call(move |db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','now'); INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now'); INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g','c',1,'shell','{\"command\":\"printf fixture\"}','completed','e'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,exit_code)VALUES('e','r','u','op','exited','/','{}','UTF-8','UTF-8','closed','now',7);")?;
        for (id, kind, body) in [("call", "assistant_response", &payloads[0]), ("result", "tool_result", &payloads[1])] {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'now',?3,length(?3))", rusqlite::params![id,kind,body])?;
        }
        db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','c','result','running')",[])?;
        Ok(())
    }).await.unwrap();
    let mut nodes = [call, result]
        .into_iter()
        .zip([("call", "assistant_response"), ("result", "tool_result")])
        .map(|(payload, (id, kind))| {
            let text = crate::history::text_of_payload(&payload);
            UiNode {
                node_id: id.into(),
                role: kind.into(),
                text: text.clone(),
                base_char_offset: 0,
                total_chars: text.chars().count(),
                reasoning_ranges: vec![],
                presentation: presentation::project(kind, &payload, &text, 0),
            }
        })
        .collect::<Vec<_>>();
    let original = nodes
        .iter()
        .map(|node| node.text.clone())
        .collect::<Vec<_>>();
    for code in [Some(7), Some(0), None, Some(7)] {
        control
            .store
            .database
            .call(move |db| {
                db.execute(
                    "UPDATE shell_execution SET exit_code=?1 WHERE id='e'",
                    [code],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        enrich(&control, &mut nodes).await.unwrap();
        for node in &nodes {
            let tool = node.presentation.blocks[0].tool.as_ref().unwrap();
            assert_eq!(tool.ordinal, Some(1));
            assert_eq!(tool.command, "printf fixture");
            assert!(
                tool.execution_id.is_some(),
                "call/result correlation has the actual shell identity"
            );
            assert_eq!(tool.current_state.as_deref(), Some("exited"));
            assert_eq!(tool.current_exit_code, code);
            assert_eq!(
                tool.exit_code, None,
                "saved background return has no terminal code"
            );
            assert!(tool.heading().starts_with(match code {
                Some(0) => "✓ #2 printf fixture",
                Some(_) => "✗ #2 printf fixture",
                None => "· #2 printf fixture",
            }));
            assert!(tool.heading().contains("返却時 背景継続"));
        }
    }
    let terminal_call = json!({"group_id":"g","output":[{"type":"function_call","name":"shell","call_id":"terminal","arguments":json!({"command":"printf terminal"}).to_string()}]});
    let terminal_result = json!({"group_id":"g","item":{"type":"function_call_output","call_id":"terminal","output":json!({"status":"exited","exit_code":7}).to_string()}});
    let saved = [terminal_call.to_string(), terminal_result.to_string()];
    control.store.database.call(move|db| {
        db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g','terminal',2,'shell','{\"command\":\"printf terminal\"}','completed','e-terminal')",[])?;
        db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,exit_code)VALUES('e-terminal','r','u','terminal-op','exited','/','{}','UTF-8','UTF-8','closed','now',7)",[])?;
        for (id,kind,body) in [("terminal-call","assistant_response",&saved[0]),("terminal-result","tool_result",&saved[1])] {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'now',?3,length(?3))",rusqlite::params![id,kind,body])?;
        }
        db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','terminal','terminal-result','exited')",[])?;
        Ok(())
    }).await.unwrap();
    let text = crate::history::text_of_payload(&terminal_call);
    let mut terminal_nodes = vec![UiNode {
        node_id: "terminal-call".into(),
        role: "assistant".into(),
        text: text.clone(),
        base_char_offset: 0,
        total_chars: text.chars().count(),
        reasoning_ranges: vec![],
        presentation: presentation::project("assistant_response", &terminal_call, &text, 0),
    }];
    enrich(&control, &mut terminal_nodes).await.unwrap();
    let call = terminal_nodes[0].presentation.blocks[0]
        .tool
        .as_ref()
        .unwrap();
    assert_eq!(call.status, "completed");
    assert_eq!(call.exit_code, None);
    assert!(call.heading().starts_with("✗ #3 printf terminal · 終了 7"));
    assert!(
        !call.heading().contains("返却時"),
        "a call carries no saved result code"
    );
    // A real DB read failure must clear the last observation before returning;
    // the UI separately labels this failed refresh as currently unconfirmed.
    control
        .store
        .database
        .call(|db| {
            db.execute_batch("ALTER TABLE tool_call RENAME TO hidden_tool_call")?;
            Ok(())
        })
        .await
        .unwrap();
    let failure = enrich(&control, &mut nodes).await;
    control
        .store
        .database
        .call(|db| {
            db.execute_batch("ALTER TABLE hidden_tool_call RENAME TO tool_call")?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(failure.is_err());
    for (node, text) in nodes.iter().zip(original) {
        let tool = node.presentation.blocks[0].tool.as_ref().unwrap();
        assert_eq!(tool.current_state, None);
        assert_eq!(tool.current_exit_code, None);
        assert_eq!(node.text, text);
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

fn connection_fixture() -> rusqlite::Connection {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
        INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
        INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');
        INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','now');
        INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now'),('other','a','u','completed','now');").unwrap();
    for n in 0..129 {
        db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g',?1,?2,'shell',?3,'completed')",rusqlite::params![format!("call-{n}"),n,json!({"command":format!("printf \u{1b}[31mcommand-{n}")}).to_string()]).unwrap();
    }
    db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('other','call-0',0,'shell','{\"command\":\"OTHER\"}','completed')",[]).unwrap();
    let direct =
        |call: &str| json!({"group_id":"g","item":{"type":"function_call","call_id":call}});
    let whole = json!({"group_id":"g","output":(0..129).map(|n|json!({"type":"function_call","call_id":format!("call-{n}")})).collect::<Vec<_>>()});
    for (id, kind, payload) in [
        ("direct-0", "assistant_response", direct("call-0")),
        ("direct-1", "assistant_response", direct("call-1")),
        ("whole", "assistant_response", whole),
        (
            "message",
            "assistant_response",
            json!({"group_id":"g","item":{"type":"message","content":[]}}),
        ),
        (
            "unproven",
            "assistant_response",
            json!({"item":{"type":"function_call","call_id":"call-0"}}),
        ),
        (
            "forged-result",
            "tool_result",
            json!({"group_id":"g","item":{"type":"function_call_output","call_id":"call-0"}}),
        ),
    ] {
        db.execute(
            "INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u',?2,'now',?3)",
            rusqlite::params![id, kind, payload.to_string()],
        )
        .unwrap();
    }
    db
}

#[test]
fn preprojection_direct_nodes_are_exact_and_owned_facts_apply_by_node_identity() {
    let db = connection_fixture();
    let facts = read_nodes_tx(&db, &["direct-0".into(), "direct-1".into()]).unwrap();
    assert_eq!(
        facts.sources().len(),
        2,
        "per-item nodes must not expand all129 group calls"
    );
    assert_eq!(facts.sources()[0].call_id, "call-0");
    assert_eq!(facts.sources()[1].call_id, "call-1");
    assert!(
        facts.sources()[0]
            .command
            .as_deref()
            .unwrap()
            .contains('\u{1b}'),
        "DB facts are raw; display sanitization occurs later"
    );
    drop(db);
    let mut nodes = [("direct-1", "call-1"), ("direct-0", "call-0")]
        .into_iter()
        .map(|(node, call)| UiNode {
            node_id: node.into(),
            presentation: presentation::NodePresentation {
                blocks: vec![presentation::PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(presentation::ToolPresentation {
                        call_id: call.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        })
        .collect::<Vec<_>>();
    apply(&mut nodes, &facts);
    for (node, ordinal) in nodes.iter().zip([1, 0]) {
        let tool = node.presentation.blocks[0].tool.as_ref().unwrap();
        assert_eq!(tool.ordinal, Some(ordinal));
        assert_eq!(tool.group_id.as_deref(), Some("g"));
        assert!(!tool.command.contains('\u{1b}'));
        assert!(tool.command.contains(&format!("command-{ordinal}")));
    }
}

#[test]
fn whole_response_candidate_range_is_bounded_and_uncertain_sources_stay_local() {
    let db = connection_fixture();
    let facts = read_nodes_tx(&db, &["whole".into()]).unwrap();
    assert_eq!(facts.sources().len(), 128);
    assert_eq!(facts.truncated_nodes, ["whole"]);
    assert!(
        facts
            .sources()
            .iter()
            .all(|source| source.node_id == "whole" && source.group_id == "g")
    );
    let mut nodes = vec![UiNode {
        node_id: "whole".into(),
        ..Default::default()
    }];
    apply(&mut nodes, &facts);
    assert!(nodes[0].presentation.metadata_limited && nodes[0].presentation.details_omitted);
    let uncertain = read_nodes_tx(
        &db,
        &["unproven".into(), "message".into(), "forged-result".into()],
    )
    .unwrap();
    assert!(
        uncertain.sources().is_empty(),
        "no global call-ID fallback or invented result relation"
    );
    assert_eq!(
        read_nodes_tx(&db, &vec!["direct-0".into(); 129])
            .err()
            .unwrap()
            .into_api_error()
            .code,
        crate::protocol::ErrorCode::InputLimit
    );
    let plan=db.prepare("EXPLAIN QUERY PLAN SELECT call_id FROM tool_call WHERE group_id=?1 ORDER BY ordinal,call_id LIMIT 129").unwrap().query_map(["g"],|row|row.get::<_,String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap().join("\n");
    assert!(
        plan.contains("SEARCH") && plan.contains("group_id=?"),
        "{plan}"
    );
}

#[test]
fn disjoint_tool_fact_sets_merge_without_repeating_node_call_identity() {
    let db = connection_fixture();
    let mut facts = read_nodes_tx(&db, &["direct-0".into()]).unwrap();
    facts.append(read_nodes_tx(&db, &["direct-1".into()]).unwrap());
    facts.append(read_nodes_tx(&db, &["direct-0".into()]).unwrap());
    assert_eq!(facts.sources().len(), 2);
}
