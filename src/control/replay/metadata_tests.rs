use super::*;

#[test]
fn shallow_epoch_ownership_preserves_literals_opaque_and_epoch_rules() {
    let block = json!({"type":"thinking","thinking":"literal reasoning_epoch","signature":"signed-byte-exact","reasoning_epoch":{"user_defined":7}});
    let opaque = crate::model::opaque::wrap("anthropic_messages", "thinking", block.clone());
    let reasoning = json!({"type":"reasoning","id":"signed-id","encrypted_content":"encrypted-byte-exact","summary":[],"status":"completed"});
    let ordinary = vec![
        json!({"type":"message","role":"user","content":[{"type":"input_text","text":"{\"reasoning_epoch\":7}"}]}),
        json!({"type":"function_call","call_id":"pair","name":"shell","arguments":"{\"reasoning_epoch\":7,\"nested\":{\"reasoning_epoch\":8}}"}),
        json!({"type":"function_call_output","call_id":"pair","output":" {\"reasoning_epoch\":7} "}),
    ];
    for epoch in [json!(7), json!(8), Value::Null, json!("malformed")] {
        for kept in [false, true] {
            let mut output = vec![reasoning.clone(), opaque.clone()];
            output.extend(ordinary.clone());
            for item in &mut output {
                item["reasoning_epoch"] = json!({"internal":"not necessarily an integer"});
            }
            let saved = json!({"reasoning_epoch":epoch,"group_id":"local-group","output":output});
            let actual = convert_kept(saved, 7, kept);
            let eligible = epoch == json!(7) || kept;
            let mut expected = if eligible {
                vec![reasoning.clone(), opaque.clone()]
            } else {
                vec![]
            };
            expected.extend(ordinary.clone());
            assert_eq!(actual, expected, "epoch={epoch}, kept={kept}");
            if eligible {
                assert_eq!(actual[1]["block"], block);
            }
        }
    }
    let without_epoch = json!({"output":[reasoning, opaque, ordinary[0]]});
    assert_eq!(convert(without_epoch.clone(), 7), vec![ordinary[0].clone()]);
    assert_eq!(convert_kept(without_epoch, 7, true).len(), 3);
    for epoch in [Value::Null, json!("bad"), json!([]), json!({"nested":1})] {
        let mut raw = ordinary[0].clone();
        raw["reasoning_epoch"] = epoch;
        assert_eq!(convert(raw.clone(), 7), vec![ordinary[0].clone()]);
        assert_eq!(convert(json!({"item":raw}), 7), vec![ordinary[0].clone()]);
    }
    assert!(
        convert(
            json!({"output":[],"item":ordinary[0],"role":"user","content":"distractor"}),
            7
        )
        .is_empty()
    );
    assert_eq!(
        convert(json!({"output":null,"item":ordinary[0]}), 7),
        vec![ordinary[0].clone()]
    );

    let client = crate::model::http::HttpModelClient::new(crate::config::Connection {
        base_url: "http://127.0.0.1:9/v1".into(),
        api_key_env: None,
        ..Default::default()
    })
    .unwrap();
    let mut request = crate::model::response_request("synthetic", ordinary.clone(), None, false);
    let schema = json!({"type":"function","name":"synthetic","parameters":{"type":"object","properties":{"reasoning_epoch":{"type":"integer"}}}});
    request["tools"] = json!([schema.clone()]);
    let (wire, _) = client
        .wire_request(request, Some("synthetic-thread"))
        .unwrap();
    assert_eq!(wire["tools"][0], schema);
    assert_eq!(wire["input"], json!(ordinary));
}

#[derive(Debug, PartialEq, Eq)]
struct StoredSnapshot {
    nodes: Vec<(String, i64, String)>,
    path: Vec<String>,
    state: String,
}
fn stored_snapshot(db: &Connection) -> rusqlite::Result<StoredSnapshot> {
    Ok(StoredSnapshot {
        nodes: db.prepare("SELECT id,payload_revision,payload_json FROM node ORDER BY id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?,
        path: db.prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
            .query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?,
        state: db.query_row("SELECT json_object('selection',selection_revision,'profile',profile_revision,'epoch',reasoning_epoch,'cursor',cursor_node_id) FROM thread WHERE id='t'", [], |r| r.get(0))?,
    })
}

#[tokio::test]
async fn historical_summary_bytes_survive_cold_warm_append_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("synthetic.db");
    let worker = crate::storage::DbWorker::open(path.clone()).await.unwrap();
    worker.call(|db| {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now')", [])?;
        let raw = " { \"reasoning_epoch\": 0, \"item\": { \"role\": \"developer\", \"content\": [{\"type\":\"input_text\",\"text\":\"historical summary: reasoning_epoch\"}], \"reasoning_epoch\": 0 } } ";
        let tx = db.transaction()?;
        crate::history::append_node(&tx,"t","summary",None,"summary",&serde_json::from_str(raw).unwrap(),"now")?;
        tx.execute("UPDATE node SET payload_json=?1 WHERE id='summary'", [raw])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let cache = ReplayCache::new(1_000_000);
    let expected = worker.call(move |db| {
        let before = stored_snapshot(db)?;
        let cold = cache.load(db, "t")?.materialize();
        assert_eq!(cold, vec![json!({"role":"developer","content":[{"type":"input_text","text":"historical summary: reasoning_epoch"}]})]);
        assert_eq!(cache.load(db, "t")?.materialize(), cold);
        assert_eq!(stored_snapshot(db)?, before, "cold and warm replay preserve actual stored bytes and selection");
        let tx = db.transaction()?;
        crate::history::append_node(&tx,"t","tail",None,"message",&json!({"role":"user","content":"append once"}),"now")?;
        tx.commit()?;
        let after_append = stored_snapshot(db)?;
        let appended = cache.load(db, "t")?.materialize();
        assert_eq!(appended.len(), 2);
        assert_eq!(appended[0], cold[0]);
        assert_eq!(appended[1], json!({"role":"user","content":"append once"}));
        assert_eq!(cache.load(db, "t")?.materialize(), appended);
        assert_eq!(ReplayCache::new(1_000_000).load(db,"t")?.materialize(), appended);
        assert_eq!(stored_snapshot(db)?, after_append);
        Ok((appended, after_append))
    }).await.unwrap();
    worker.shutdown().await.unwrap();
    let worker = crate::storage::DbWorker::open(path).await.unwrap();
    worker
        .call(move |db| {
            assert_eq!(
                stored_snapshot(db)?,
                expected.1,
                "reopen retains raw SQLite bytes"
            );
            let client = crate::model::http::HttpModelClient::new(crate::config::Connection {
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key_env: None,
                ..Default::default()
            })
            .unwrap();
            let items = ReplayCache::new(1_000_000).load(db, "t")?.materialize();
            assert_eq!(items, expected.0);
            let (wire, _) = client
                .wire_request(
                    crate::model::response_request("synthetic", items, None, false),
                    Some("t"),
                )
                .unwrap();
            assert_eq!(wire["input"], json!(expected.0));
            assert_eq!(
                stored_snapshot(db)?,
                expected.1,
                "final HTTP preparation cannot rewrite stored bytes"
            );
            Ok(())
        })
        .await
        .unwrap();
    worker.shutdown().await.unwrap();
}
