//! Actual projection/window producers, synthetic saved canonical payloads only.
use rusqlite::Connection;
use serde_json::{Value, json};
use shellboy::{
    history::{
        append_node,
        items::{ITEM_TEXT_BYTES, project},
        window::window,
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
    for node in legacy["items"].as_array().unwrap() {
        assert!(node.get("items").is_none());
        assert!(node.get("items_unavailable_reason").is_none());
    }
    if let Some(dir) = std::env::var_os("CORE296_FIXTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("schema.json"), schema_json()).unwrap();
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
    let mut retry = args();
    retry.ordinal = Some(128);
    retry.after = 3;
    let retry = serde_json::to_value(window(&db, &retry).unwrap()).unwrap();
    assert_eq!(retry["items"].as_array().unwrap().len(), 4);
    assert!(retry["items"][0]["items"].is_array());
}
