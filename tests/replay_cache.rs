use serde_json::{Value, json};
use shellboy::{
    control::replay::{ReplayCache, ReplayLease},
    history::{append_node, select_path},
    storage::DbWorker,
};
struct Fixture {
    _root: tempfile::TempDir,
    db: DbWorker,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        Self { _root: root, db }
    }
    async fn thread(&self, id: &str) {
        let id = id.to_owned();
        self.db.call(move|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/AGENTS.md','profile-a','model-a','now')",[id])?;Ok(())}).await.unwrap();
    }
    async fn append(&self, thread: &str, id: &str, payload: Value) {
        let thread = thread.to_owned();
        let id = id.to_owned();
        self.db
            .call(move |db| {
                let tx = db.transaction()?;
                append_node(&tx, &thread, &id, None, "fixture", &payload, "now")?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn load(&self, cache: &ReplayCache, thread: &str) -> ReplayLease {
        let cache = cache.clone();
        let thread = thread.to_owned();
        self.db
            .call(move |db| {
                let tx = db.transaction()?;
                let lease = cache.load(&tx, &thread)?;
                tx.commit()?;
                Ok(lease)
            })
            .await
            .unwrap()
    }
}
fn user(text: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}]})
}
fn reasoning(label: &str, epoch: i64) -> Value {
    json!({"reasoning_epoch":epoch,"output":[{"type":"reasoning","id":format!("reasoning-{label}"),"summary":[{"type":"summary_text","text":format!("summary-{label}")}],"encrypted_content":format!("opaque-{label}")},{"type":"message","id":format!("answer-{label}"),"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":format!("answer-{label}")}]}]})
}

#[tokio::test]
async fn same_epoch_preserves_reasoning_without_encrypted_content_and_never_fabricates_it() {
    let f = Fixture::new().await;
    f.thread("t").await;
    let cache = ReplayCache::new(1_000_000);
    let output = json!([
        {"type":"reasoning","id":"rs-id-only","summary":[]},
        {"type":"reasoning","id":"rs-summary","summary":[{"type":"summary_text","text":"visible summary"}],"encrypted_content":null},
        {"type":"reasoning","id":"rs-opaque","summary":[],"encrypted_content":"opaque-returned"},
        {"type":"message","id":"msg-answer","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"answer"}]}
    ]);
    let payload = json!({"reasoning_epoch":0,"output":output});
    f.append("t", "answer", payload.clone()).await;
    let original = output.as_array().unwrap().clone();
    assert_eq!(f.load(&cache, "t").await.materialize(), original);
    // A cache hit must not normalize missing/null opaque content or drop IDs.
    assert_eq!(f.load(&cache, "t").await.materialize(), original);
    f.db.call(|db| {
        db.execute(
            "UPDATE thread SET reasoning_epoch=1,profile_revision=1 WHERE id='t'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        f.load(&cache, "t").await.materialize(),
        vec![original[3].clone()]
    );
    f.db.call(move |db| {
        let stored: String = db.query_row(
            "SELECT payload_json FROM node WHERE id='answer'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), payload);
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn unchanged_path_hits_and_append_decodes_only_new_nodes_without_modifying_originals() {
    let f = Fixture::new().await;
    f.thread("t").await;
    let cache = ReplayCache::new(1_000_000);
    let original = user("元JSON 日本語");
    f.append("t", "user", original.clone()).await;
    let first = f.load(&cache, "t").await;
    assert_eq!(cache.stats().parsed_nodes, 1);
    assert_eq!(first.materialize(), vec![original.clone()]);
    let again = f.load(&cache, "t").await;
    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().parsed_nodes, 1);
    assert_eq!(cache.stats().live_bytes, cache.stats().retained_bytes);
    let answer = reasoning("a", 0);
    f.append("t", "answer", answer.clone()).await;
    let appended = f.load(&cache, "t").await;
    assert_eq!(cache.stats().parsed_nodes, 2);
    assert_eq!(cache.stats().append_hits, 1);
    let mut expected = vec![original.clone()];
    expected.extend(answer["output"].as_array().unwrap().iter().cloned());
    assert_eq!(appended.materialize(), expected);
    assert_eq!(first.materialize(), vec![original]);
    f.db.call(move |db| {
        let body: String =
            db.query_row("SELECT payload_json FROM node WHERE id='answer'", [], |r| {
                r.get(0)
            })?;
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), answer);
        Ok(())
    })
    .await
    .unwrap();
    drop((first, again, appended));
    assert_eq!(cache.stats().pinned_bytes, 0);
    assert!(cache.stats().retained_bytes > 0);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn whole_thread_lru_is_shared_and_evicted_request_pins_stay_live_until_last_clone_drops() {
    let f = Fixture::new().await;
    for (thread, node) in [("a", "na"), ("b", "nb")] {
        f.thread(thread).await;
        f.append(thread, node, user(&"fixed payload ".repeat(100)))
            .await;
    }
    let sizing = ReplayCache::new(1_000_000);
    drop(f.load(&sizing, "a").await);
    let budget = sizing.stats().retained_bytes as u64;
    drop(sizing);
    let cache = ReplayCache::new(budget);
    let first = f.load(&cache, "a").await;
    let shared = first.clone();
    let pinned = cache.stats().pinned_bytes;
    drop(first);
    assert_eq!(cache.stats().pinned_bytes, pinned);
    let second = f.load(&cache, "b").await;
    assert_eq!(cache.stats().threads, 1);
    assert_eq!(cache.stats().evictions, 1);
    assert!(cache.stats().retained_bytes <= budget as usize);
    assert!(cache.stats().live_bytes > cache.stats().retained_bytes);
    assert!(cache.stats().pinned_bytes > pinned);
    drop(second);
    assert_eq!(cache.stats().pinned_bytes, pinned);
    let reread = f.load(&cache, "a").await;
    assert_eq!(cache.stats().parsed_nodes, 3);
    assert_eq!(reread.materialize(), shared.materialize());
    assert!(cache.stats().retained_bytes <= budget as usize);
    drop((shared, reread));
    assert_eq!(cache.stats().pinned_bytes, 0);
    assert_eq!(cache.stats().live_bytes, cache.stats().retained_bytes);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn zero_or_oversized_budget_bypasses_retention_and_accounts_for_live_request_data() {
    let f = Fixture::new().await;
    f.thread("t").await;
    f.append("t", "n", user(&"large ".repeat(200))).await;
    for budget in [0, 1] {
        let cache = ReplayCache::new(budget);
        let lease = f.load(&cache, "t").await;
        assert_eq!(cache.stats().retained_bytes, 0);
        assert_eq!(cache.stats().threads, 0);
        assert!(cache.stats().live_bytes > 0);
        assert_eq!(cache.stats().live_bytes, cache.stats().pinned_bytes);
        assert_eq!(lease.materialize().len(), 1);
        drop(lease);
        assert_eq!(cache.stats().live_bytes, 0);
        assert_eq!(cache.stats().pinned_bytes, 0);
        drop(f.load(&cache, "t").await);
        assert_eq!(cache.stats().parsed_nodes, 2);
    }
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn compaction_rewind_and_source_revision_invalidate_but_effort_only_change_reuses_payloads() {
    let f = Fixture::new().await;
    f.thread("t").await;
    f.append("t", "first", user("first")).await;
    f.append("t", "last", user("last")).await;
    let cache = ReplayCache::new(1_000_000);
    drop(f.load(&cache, "t").await);
    assert_eq!(cache.stats().parsed_nodes, 2);
    f.db.call(|db| {
        db.execute(
            "UPDATE thread SET effort='high',profile_revision=profile_revision+1 WHERE id='t'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    drop(f.load(&cache, "t").await);
    assert_eq!(cache.stats().parsed_nodes, 2);
    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().settings_reuses, 1);
    f.db.call(|db|{let tx=db.transaction()?;tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('summary','summary','now','{\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"summary\"}]}',80)",[])?;select_path(&tx,"t",2,&["summary".into(),"last".into()],"now")?;tx.commit()?;Ok(())}).await.unwrap();
    let summary = f.load(&cache, "t").await;
    assert_eq!(cache.stats().parsed_nodes, 4);
    assert_eq!(summary.materialize()[0]["role"], "developer");
    f.db.call(|db| {
        let tx = db.transaction()?;
        select_path(&tx, "t", 3, &["summary".into()], "now")?;
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    let rewind = f.load(&cache, "t").await;
    assert_eq!(cache.stats().parsed_nodes, 5);
    assert_eq!(rewind.materialize().len(), 1);
    f.db.call(|db|{db.execute("UPDATE node SET payload_revision=payload_revision+1,payload_json=NULL,payload_bytes=0,deleted_reason='fixture collected' WHERE id='summary'",[])?;Ok(())}).await.unwrap();
    let tombstone = f.load(&cache, "t").await;
    assert!(tombstone.materialize().is_empty());
    assert_eq!(cache.stats().parsed_nodes, 5);
    assert_eq!(
        rewind.materialize().len(),
        1,
        "a pinned immutable item remains usable by its existing request"
    );
    drop((summary, rewind, tombstone));
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn epoch_switch_back_never_revives_old_opaque_and_fork_has_an_independent_cache_entry() {
    let f = Fixture::new().await;
    f.thread("t").await;
    let a = reasoning("a", 0);
    f.append("t", "a", a.clone()).await;
    let cache = ReplayCache::new(1_000_000);
    let original = f.load(&cache, "t").await;
    assert!(
        serde_json::to_string(&original.materialize())
            .unwrap()
            .contains("opaque-a")
    );
    f.db.call(|db|{db.execute("UPDATE thread SET profile_id='profile-b',model_id='model-b',reasoning_epoch=1,profile_revision=1 WHERE id='t'",[])?;Ok(())}).await.unwrap();
    f.append("t", "b", reasoning("b", 1)).await;
    let b = f.load(&cache, "t").await;
    let text = serde_json::to_string(&b.materialize()).unwrap();
    assert!(!text.contains("opaque-a"));
    assert!(text.contains("opaque-b"));
    assert!(text.contains("answer-a"));
    f.db.call(|db|{db.execute("UPDATE thread SET profile_id='profile-a',model_id='model-a',reasoning_epoch=2,profile_revision=2 WHERE id='t'",[])?;Ok(())}).await.unwrap();
    f.append("t", "a2", reasoning("a2", 2)).await;
    let restored = f.load(&cache, "t").await;
    let text = serde_json::to_string(&restored.materialize()).unwrap();
    assert!(!text.contains("opaque-a\""));
    assert!(!text.contains("opaque-b"));
    assert!(text.contains("opaque-a2"));
    let fork_thread = shellboy::history::History::new(f.db.clone())
        .fork(shellboy::protocol::ThreadForkArgs {
            thread_id: "t".into(),
            node_id: "a2".into(),
            cwd: None,
        })
        .await
        .unwrap();
    let fork = f.load(&cache, &fork_thread.thread_id).await;
    assert_eq!(fork.materialize(), restored.materialize());
    assert_eq!(cache.stats().threads, 2);
    f.db.call(move |db| {
        let saved: String =
            db.query_row("SELECT payload_json FROM node WHERE id='a'", [], |r| {
                r.get(0)
            })?;
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), a);
        Ok(())
    })
    .await
    .unwrap();
    drop((original, b, restored, fork));
    assert_eq!(cache.stats().pinned_bytes, 0);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_conversion_preserves_tool_items_and_keeps_internal_metadata_out_of_replay() {
    let f = Fixture::new().await;
    f.thread("t").await;
    f.append("t","event",json!({"role":"developer","event_id":"internal","content":[{"type":"input_text","text":"event"}]})).await;
    let call = json!({"type":"function_call","id":"item","call_id":"call","name":"shell","arguments":"{\"command\":\"printf ok\"}","phase":"commentary"});
    f.append(
        "t",
        "call",
        json!({"output":[call.clone()],"reasoning_epoch":0}),
    )
    .await;
    let result = json!({"type":"function_call_output","call_id":"call","output":"原文 result"});
    f.append(
        "t",
        "result",
        json!({"item":result.clone(),"group_id":"group"}),
    )
    .await;
    f.append("t", "metadata", json!({"text":"not an API item"}))
        .await;
    let cache = ReplayCache::new(1_000_000);
    let lease = f.load(&cache, "t").await;
    let items = lease.materialize();
    assert_eq!(items.len(), 3);
    assert!(items[0].get("event_id").is_none());
    assert_eq!(items[1], call);
    assert_eq!(items[2], result);
    f.db.call(|db| {
        let body: String =
            db.query_row("SELECT payload_json FROM node WHERE id='event'", [], |r| {
                r.get(0)
            })?;
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["event_id"],
            "internal"
        );
        Ok(())
    })
    .await
    .unwrap();
    drop(lease);
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn sparse_lru_metadata_is_reclaimed_and_all_allocated_container_capacity_is_counted() {
    let f = Fixture::new().await;
    for i in 0..80 {
        let id = format!("t{i:03}");
        f.thread(&id).await;
        f.append(&id, &format!("n{i:03}"), user("small")).await;
    }
    let cache = ReplayCache::new(150_000);
    for i in 0..80 {
        drop(f.load(&cache, &format!("t{i:03}")).await);
        assert!(cache.stats().retained_bytes <= 150_000);
    }
    let before = cache.stats().metadata_bytes;
    assert!(before > 0);
    // This ordinary append makes one entry exceed the complete budget. Replacing
    // small entries by large entries must not retain a large empty map buffer.
    for i in 0..80 {
        f.append(
            &format!("t{i:03}"),
            &format!("large{i:03}"),
            user(&"large ".repeat(30_000)),
        )
        .await;
        drop(f.load(&cache, &format!("t{i:03}")).await);
        assert!(cache.stats().retained_bytes <= 150_000);
    }
    assert_eq!(cache.stats().threads, 0);
    assert_eq!(cache.stats().metadata_bytes, 0);
    assert_eq!(cache.stats().retained_bytes, 0);
    assert_eq!(cache.stats().live_bytes, 0);
    f.db.shutdown().await.unwrap();
}
