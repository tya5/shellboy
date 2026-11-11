//! Actual projection/window producers, synthetic saved canonical payloads only.
use rusqlite::Connection;
use serde_json::{Value, json};
use shellboy::{
    history::{
        append_node,
        items::{ITEM_TEXT_BYTES, project},
        window,
    },
    protocol::*,
};

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../src/storage/current.sql"))
        .unwrap();
    db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','fixture','fixture','now')",[]).unwrap();
    db
}
fn append(db: &mut Connection, id: &str, kind: &str, payload: Value) {
    let tx = db.transaction().unwrap();
    append_node(&tx, "t", id, None, kind, &payload, "now").unwrap();
    tx.commit().unwrap();
}
fn args() -> HistoryWindowArgs {
    serde_json::from_value(
        json!({"thread_id":"t","before":0,"after":199,"ordinal":0,"items":true,"excerpt_chars":0}),
    )
    .unwrap()
}
fn kinds(items: &[HistoryItem]) -> Vec<String> {
    items
        .iter()
        .map(|v| {
            serde_json::to_value(v).unwrap()["kind"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect()
}
#[test]
fn actual_projection_order_empty_reasoning_unknown_and_utf8() {
    let payload = json!({"output":[
        {"type":"message","content":[{"type":"output_text","text":"first"}]},
        {"type":"function_call","call_id":"a","arguments":"SECRET_ARGS"},
        {"type":"reasoning","summary":[{"text":"SECRET_SUMMARY"}],"encrypted_content":"SECRET_OPAQUE"},
        {"type":"message","content":[{"type":"output_text","text":""},{"type":"output_text","text":"界".repeat(6000)}]},
        {"type":"future","text":"SECRET_FUTURE"}
    ]});
    let items = project("node", "assistant_response", &payload, &[]);
    assert_eq!(
        kinds(&items),
        [
            "text",
            "tool_call",
            "reasoning_unavailable",
            "text",
            "text",
            "unavailable"
        ]
    );
    assert!(matches!(&items[3],HistoryItem::Text{text,text_complete:true,..} if text.is_empty()));
    assert!(
        matches!(&items[4],HistoryItem::Text{text,text_complete:false,..} if text.len()==ITEM_TEXT_BYTES-ITEM_TEXT_BYTES%3 && text==&"界".repeat(ITEM_TEXT_BYTES/3))
    );
    assert!(!serde_json::to_string(&items).unwrap().contains("SECRET_"));
    assert_eq!(
        items,
        project("node", "assistant_response", &payload, &[]),
        "same saved coordinates have stable identities"
    );
}
#[test]
fn actual_window_opt_in_and_encoding_fixture() {
    let mut db = db();
    append(
        &mut db,
        "n0",
        "user",
        json!({"role":"user","content":[{"type":"input_text","text":"synthetic input"}]}),
    );
    append(
        &mut db,
        "n1",
        "assistant_response",
        json!({"output":[{"type":"message","content":[{"type":"output_text","text":"A"}]},{"type":"function_call","call_id":"a","arguments":"SECRET_ARGS"}]}),
    );
    append(
        &mut db,
        "n2",
        "assistant_response",
        json!({"output":[{"type":"reasoning","summary":[{"text":"SECRET_REASON"}]},{"type":"message","content":[{"type":"output_text","text":""}]}]}),
    );
    append(
        &mut db,
        "n3",
        "tool_result",
        json!({"item":{"type":"function_call_output","call_id":"a","output":"{\"secret\":\"SECRET_RESULT\"}"}}),
    );
    let result = window(&db, &args()).unwrap();
    let encoded = serde_json::to_value(&result).unwrap();
    assert_eq!(result.items.len(), 4);
    assert_eq!(encoded["items"][0]["items"][0]["kind"], "text");
    assert_eq!(encoded["items"][1]["items"][1]["kind"], "tool_call");
    assert_eq!(
        encoded["items"][2]["items"][0]["kind"],
        "reasoning_unavailable"
    );
    assert_eq!(encoded["items"][3]["items"][0]["kind"], "tool_result");
    let projected: Vec<_> = encoded["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| &n["items"])
        .collect();
    assert!(
        !serde_json::to_string(&projected)
            .unwrap()
            .contains("SECRET_")
    );
    let mut legacy = args();
    legacy.items = false;
    let legacy = serde_json::to_value(window(&db, &legacy).unwrap()).unwrap();
    let decoded: HistoryWindow = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        legacy,
        "legacy absent flattened fields survive decode/encode"
    );
    for node in legacy["items"].as_array().unwrap() {
        assert!(node.get("items").is_none());
        assert!(node.get("items_unavailable_reason").is_none());
    }
    if let Some(dir) = std::env::var_os("CORE296_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("schema.json"), schema_json()).unwrap();
        std::fs::write(
            dir.join("legacy-window.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("actual-window.json"),
            serde_json::to_vec_pretty(&encoded).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn actual_window_guard_null_legacy_oversized_and_existing_stale_anchor() {
    let mut db = db();
    append(&mut db, "null", "user", json!({"role":"user","content":[]}));
    db.execute("UPDATE node SET payload_json=NULL WHERE id='null'", [])
        .unwrap();
    append(
        &mut db,
        "legacy",
        "user",
        json!({"role":"user","content":[{"type":"input_text","text":"legacy"}]}),
    );
    db.execute("UPDATE node SET payload_bytes=0 WHERE id='legacy'", [])
        .unwrap();
    append(
        &mut db,
        "huge",
        "user",
        json!({"role":"user","content":[{"type":"input_text","text":"界".repeat(400000)}]}),
    );
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    for node in result["items"].as_array().unwrap() {
        let rendered = serde_json::to_string(&node["items"]).unwrap();
        assert!(rendered.contains("unavailable"), "{}", node["node_id"]);
        assert!(!rendered.contains("SECRET_"));
    }
    assert_eq!(result["items"][2]["items"][0]["reason"], "source_budget");
    if let Some(dir) = std::env::var_os("CORE296_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("unavailable-window.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
    }
    let mut stale = args();
    stale.expect_node_id = Some("wrong".into());
    assert!(window(&db, &stale).is_err());
}
#[test]
fn actual_window_response_budget_keeps_all_later_ordinary_nodes() {
    let mut db = db();
    // Two max-sized text items per node: 128 nodes exactly consume 4 MiB.
    let text = "x".repeat(ITEM_TEXT_BYTES);
    for n in 0..132 {
        append(
            &mut db,
            &format!("n{n}"),
            "assistant_response",
            json!({"output":[{"type":"message","content":[{"type":"output_text","text":text},{"type":"output_text","text":text}]}]}),
        );
    }
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    let nodes = result["items"].as_array().unwrap();
    assert_eq!(nodes.len(), 132);
    assert!(nodes[127]["items"].is_array());
    for node in &nodes[128..] {
        assert!(node["items"].is_null());
        assert_eq!(node["items_unavailable_reason"], "response_budget");
        assert!(node.get("node_id").is_some());
    }
    let decoded: HistoryWindow = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        result,
        "nullable projection must survive actual decode/encode"
    );
    if let Some(dir) = std::env::var_os("CORE296_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("response-budget-window.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
    }
    let mut retry = args();
    retry.ordinal = Some(128);
    retry.after = 3;
    let retry = serde_json::to_value(window(&db, &retry).unwrap()).unwrap();
    assert_eq!(retry["items"].as_array().unwrap().len(), 4);
    assert!(retry["items"][0]["items"].is_array());
    if let Some(dir) = std::env::var_os("CORE296_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("rewindow-window.json"),
            serde_json::to_vec_pretty(&retry).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn actual_projection_bounds_execution_links_and_preserves_source_identity() {
    let links: Vec<HistoryExecutionLink> = (0..12)
        .map(|n| {
            serde_json::from_value(json!({
            "group_id":"g", "call_id":"a", "call_ordinal":0, "call_state":"completed",
            "execution_id":format!("e{n}"), "state":"exited", "cwd":"/synthetic",
            "command_excerpt":"SECRET_COMMAND", "command_truncated":false,
            "description":"SECRET_DESCRIPTION", "request_node_id":"node", "result_node_id":"result"
        })).unwrap()
        })
        .collect();
    let payload = json!({"output":[
        {"type":"function_call","call_id":"a","arguments":"SECRET_ARGUMENT"},
        {"type":"message","content":[{"type":"output_text","text":"same"},{"type":"output_text","text":"same"}]}
    ]});
    let result = project("node", "assistant_response", &payload, &links);
    assert_eq!(result.len(), 3);
    let encoded = serde_json::to_value(&result).unwrap();
    let ids = encoded[0]["execution_ids"].as_array().unwrap();
    assert!(ids.len() <= 8);
    assert!(
        !ids.is_empty(),
        "authoritative matching links must not be discarded"
    );
    assert_ne!(
        encoded[1]["item_identity"], encoded[2]["item_identity"],
        "equal text at different coordinates is not one item"
    );
    assert!(!serde_json::to_string(&result).unwrap().contains("SECRET_"));
    assert_eq!(
        result,
        project("node", "assistant_response", &payload, &links)
    );
}

#[test]
fn actual_window_args_legacy_default_and_skip_false() {
    let value = json!({"thread_id":"t","before":0,"after":0,"ordinal":0});
    let legacy: HistoryWindowArgs = serde_json::from_value(value).unwrap();
    assert!(!legacy.items);
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("items")
            .is_none()
    );
    let mut opted = legacy;
    opted.items = true;
    assert_eq!(serde_json::to_value(&opted).unwrap()["items"], true);
}

#[tokio::test]
async fn actual_store_history_window_existing_read_lane_reuses_caller_transaction() {
    use shellboy::{config::Config, control::db::Store, storage::DbWorker};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let database = DbWorker::open(root.path().join("synthetic.db"))
        .await
        .unwrap();
    database.call(|db| {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','fixture','fixture','now')",[])?;
        let tx=db.transaction()?;
        append_node(&tx,"t","public-node",None,"user",&json!({"role":"user","content":[{"type":"input_text","text":"synthetic public route"}]}),"now")?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let config=Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let store = Store::new(
        database.clone(),
        Arc::new(config),
        root.path().into(),
        "fixture".into(),
    );
    // Store routes this PureRead command through existing inspection RO transaction;
    // a bare history::window(&Connection) call cannot detect nested BEGIN.
    let result = store
        .execute(
            Request::new("fixture:public-window", Command::HistoryWindow(args())),
            None,
        )
        .await;
    assert!(
        result.is_ok(),
        "public existing read-lane request failed: {:?}",
        result.as_ref().err()
    );
    let encoded = result.unwrap().result;
    assert_eq!(
        encoded["items"][0]["items"][0]["text"],
        "synthetic public route"
    );
    if let Some(dir) = std::env::var_os("CORE296_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("public-route-window.json"),
            serde_json::to_vec_pretty(&encoded).unwrap(),
        )
        .unwrap();
    }
    drop(store);
    database.shutdown().await.unwrap();
}

#[test]
fn readable_reasoning_actual_window_schema_roundtrip_order_and_utf8() {
    let mut db = db();
    append(
        &mut db,
        "readable",
        "assistant_response",
        json!({"output":[
            {"type":"reasoning","summary":[{"type":"summary_text","text":"first"},{"type":"summary_text","text":"界".repeat(6000)}],"encrypted_content":"SECRET_CIPHER"},
            {"type":"function_call","call_id":"a","arguments":"SECRET_ARGS"},
            {"type":"message","content":[{"type":"output_text","text":"answer"}]}
        ]}),
    );
    let result = window(&db, &args()).unwrap();
    let encoded = serde_json::to_value(&result).unwrap();
    let items = &encoded["items"][0]["items"];
    assert_eq!(items[0]["kind"], "reasoning_text");
    assert_eq!(items[0]["text"], "first");
    assert_eq!(items[0]["text_complete"], true);
    assert_eq!(items[1]["kind"], "reasoning_text");
    assert_eq!(items[1]["text_complete"], false);
    assert_eq!(
        items[1]["text"].as_str().unwrap().len(),
        ITEM_TEXT_BYTES - ITEM_TEXT_BYTES % 3
    );
    assert_eq!(items[2]["kind"], "tool_call");
    assert_eq!(items[3]["kind"], "text");
    assert_eq!(
        items[0]["item_identity"],
        r#"["readable","/output/0/summary/0"]"#
    );
    assert_ne!(items[0]["item_identity"], items[1]["item_identity"]);
    assert!(!serde_json::to_string(items).unwrap().contains("SECRET"));
    let decoded: HistoryWindow = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
    assert_eq!(
        serde_json::to_value(window(&db, &args()).unwrap()).unwrap(),
        encoded
    );
    let schema: Value = serde_json::from_str(&schema_json()).unwrap();
    let item_schema = schema["definitions"]["HistoryItem"].to_string();
    assert!(item_schema.contains("reasoning_text"), "{item_schema}");
    assert!(item_schema.contains("text_complete"));
}

#[test]
fn readable_reasoning_actual_window_source_budget_and_opaque_marker() {
    let mut db = db();
    append(
        &mut db,
        "huge-reason",
        "assistant_response",
        json!({"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"x".repeat(1024*1024)}]}]}),
    );
    append(
        &mut db,
        "opaque-reason",
        "assistant_response",
        json!({"output":[{"type":"provider_opaque","dialect":"anthropic_messages","kind":"thinking","block":{"type":"thinking","thinking":"SECRET_THINKING","signature":"SECRET_SIGNATURE"}}]}),
    );
    let encoded = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    assert_eq!(encoded["items"][0]["items"][0]["reason"], "source_budget");
    assert_eq!(
        encoded["items"][1]["items"][0]["kind"],
        "reasoning_unavailable"
    );
    for node in encoded["items"].as_array().unwrap() {
        assert!(
            !serde_json::to_string(&node["items"])
                .unwrap()
                .contains("SECRET")
        );
    }
}

#[test]
fn readable_reasoning_response_budget_counts_text_and_keeps_atomic_suffix() {
    let mut db = db();
    let text = "r".repeat(ITEM_TEXT_BYTES);
    // Mix ordinary and reasoning leaves: both consume the same byte ledger.
    for n in 0..130 {
        append(
            &mut db,
            &format!("reason{n}"),
            "assistant_response",
            json!({"output":[
                {"type":"reasoning","summary":[{"type":"summary_text","text":text}]},
                {"type":"message","content":[{"type":"output_text","text":text}]}
            ]}),
        );
    }
    let encoded = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    let nodes = encoded["items"].as_array().unwrap();
    assert_eq!(nodes.len(), 130);
    assert_eq!(nodes[127]["items"].as_array().unwrap().len(), 2);
    for node in &nodes[128..] {
        assert!(node["items"].is_null());
        assert_eq!(node["items_unavailable_reason"], "response_budget");
    }
    let mut retry = args();
    retry.ordinal = Some(128);
    retry.after = 1;
    let retried = serde_json::to_value(window(&db, &retry).unwrap()).unwrap();
    assert_eq!(retried["items"][0]["items"][0]["kind"], "reasoning_text");
    assert_eq!(retried["items"][0]["items"][0]["text_complete"], true);
}

#[test]
fn generic_call_zero_exec() {
    let mut db = db();
    append(&mut db, "generic", "assistant_response", json!({"output":[{"type":"function_call","name":"lookup","call_id":"call-1","arguments":"SECRET_ARGS","extra":"SECRET_EXTRA"}]}));
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    let item = &result["items"][0]["items"][0];
    assert_eq!(item["execution_ids"], json!([]));
    assert_eq!(item["descriptor"], json!({"name":"lookup","name_state":"available","call_id":"call-1","call_id_state":"available"}));
    assert_eq!(item["detail_availability"], "unavailable");
    assert!(!item.to_string().contains("SECRET"));
}

#[test]
fn generic_result_zero_exec() {
    let mut db = db();
    append(&mut db, "generic-result", "tool_result", json!({"item":{"type":"function_call_output","call_id":"call-1","name":"SECRET_RESULT_NAME","output":"SECRET_RESULT"}}));
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    let item = &result["items"][0]["items"][0];
    assert_eq!(item["descriptor"], json!({"name_state":"not_applicable","call_id":"call-1","call_id_state":"available"}));
    assert_eq!(item["detail_availability"], "unavailable");
    assert!(!item.to_string().contains("SECRET"));
}

#[test]
fn invalid_descriptor_unavailable() {
    let mut db = db();
    let fields = [(json!(null), "missing"), (json!(42), "invalid"), (json!(""), "invalid"), (json!("x".repeat(257)), "field_budget")];
    for (i, (value, _)) in fields.iter().enumerate() {
        append(&mut db, &format!("invalid{i}"), "assistant_response", json!({"output":[{"type":"function_call","name":value,"call_id":value}]}));
    }
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    for (i, (_, expected)) in fields.iter().enumerate() {
        let descriptor = &result["items"][i]["items"][0]["descriptor"];
        assert_eq!(descriptor["name_state"], *expected);
        assert_eq!(descriptor["call_id_state"], *expected);
        assert!(descriptor.get("name").is_none());
        assert!(descriptor.get("call_id").is_none());
    }
}

#[test]
fn compacted_call_descriptor() {
    let mut db = db();
    append(&mut db, "spill-generic", "spill", json!({"output":[{"type":"function_call","name":"lookup","call_id":"call","arguments":"SECRET_ARGS"},{"type":"function_call_output","call_id":"call","output":"SECRET_OMITTED_METADATA"}]}));
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    let items = &result["items"][0]["items"];
    assert_eq!(items[0]["descriptor"]["name"], "lookup");
    assert_eq!(items[1]["descriptor"]["name_state"], "not_applicable");
    assert!(!items.to_string().contains("SECRET"));
}

#[test]
fn conflicting_calls_not_merged() {
    let mut db = db();
    for (node, name) in [("first-call", "one"), ("second-call", "two")] {
        append(&mut db, node, "assistant_response", json!({"output":[{"type":"function_call","name":name,"call_id":"duplicate"}]}));
    }
    append(&mut db, "orphan-result", "tool_result", json!({"item":{"type":"function_call_output","call_id":"duplicate","output":"SECRET"}}));
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    let first = &result["items"][0]["items"][0];
    let second = &result["items"][1]["items"][0];
    assert_ne!(first["item_identity"], second["item_identity"]);
    assert_eq!(first["descriptor"]["name"], "one");
    assert_eq!(second["descriptor"]["name"], "two");
    assert_eq!(result["items"][2]["items"][0]["descriptor"]["name_state"], "not_applicable");
}

#[test]
fn descriptor_secret_exclusion_and_exact_unicode_id_budget() {
    let mut db = db();
    append(&mut db, "safe-fields", "assistant_response", json!({"output":[
        {"type":"function_call","name":"lookup","call_id":"界".repeat(85),"arguments":"SECRET_ARGS","extra":{"name":"SECRET_EXTRA"}},
        {"type":"function_call","name":"bad name","call_id":"界".repeat(86)},
        {"type":"provider_opaque","name":"SECRET_OPAQUE","call_id":"SECRET_OPAQUE_ID"}
    ]}));
    let result = serde_json::to_value(window(&db, &args()).unwrap()).unwrap();
    let items = &result["items"][0]["items"];
    assert_eq!(items[0]["descriptor"]["call_id"], "界".repeat(85));
    assert_eq!(items[1]["descriptor"]["call_id_state"], "field_budget");
    assert_eq!(items[1]["descriptor"]["name_state"], "invalid");
    assert!(!items.to_string().contains("SECRET"));
    let _: Vec<HistoryItem> = serde_json::from_value(items.clone()).unwrap();
}
