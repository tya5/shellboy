use serde_json::{Value, json};
use shellboy::{config::Config, control::db::Store, protocol::*, storage::DbWorker};
use std::sync::Arc;

async fn fixture() -> (tempfile::TempDir, Store, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = DbWorker::open(dir.path().join("synthetic.db"))
        .await
        .unwrap();
    let config=Config::parse("default_connection='test'\ndefault_model='fixture'\n[connections.test]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let store = Store::new(
        db,
        Arc::new(config),
        dir.path().to_path_buf(),
        "instance".into(),
    );
    let root: ThreadDetails = serde_json::from_value(
        store
            .execute(
                request(Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    cwd: None,
                    model: None,
                    effort: None,
                    parent_id: None,
                })),
                None,
            )
            .await
            .unwrap()
            .result,
    )
    .unwrap();
    (dir, store, root.thread_id)
}
fn request(command: Command) -> Request {
    Request::new(format!("instance:{}", uuid::Uuid::new_v4()), command)
}
fn args(root: &str, descendants: bool) -> ThreadDeleteArgs {
    ThreadDeleteArgs {
        thread_id: root.into(),
        descendants,
        preview_marker: None,
    }
}
async fn children(store: &Store, root: &str, ids: &[String]) {
    let root = root.to_owned();
    let ids = ids.to_vec();
    store.database.call(move|db| {
        let tx=db.transaction()?;
        for id in ids {tx.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) SELECT ?1,id,cwd,agents_path,profile_id,model_id,updated_at FROM thread WHERE id=?2",rusqlite::params![id,root])?;}
        tx.commit()?;Ok(())
    }).await.unwrap();
}
async fn preview(store: &Store, root: &str, descendants: bool) -> Value {
    store
        .execute(
            request(Command::ThreadDeletePreview(args(root, descendants))),
            None,
        )
        .await
        .unwrap()
        .result
}
async fn preview_set(
    store: &Store,
    ids: Vec<String>,
) -> Result<Value, shellboy::protocol::ApiError> {
    store
        .execute(
            request(Command::ThreadDeleteSetPreview(ThreadDeleteSetArgs {
                thread_ids: ids,
            })),
            None,
        )
        .await
        .map(|result| result.result)
}
fn expected_set_marker(ids: &[String]) -> DeletionPreviewMarker {
    use sha2::{Digest, Sha256};
    let ids = ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let mut hash = Sha256::new();
    hash.update(b"shellboy.thread.delete.v1\0");
    for id in &ids {
        hash.update((id.len() as u32).to_be_bytes());
        hash.update(id.as_bytes());
    }
    DeletionPreviewMarker {
        thread_count: ids.len() as u32,
        ids_hash: hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}
fn marker(value: &Value) -> DeletionPreviewMarker {
    serde_json::from_value(value["preview_marker"].clone()).unwrap()
}
async fn facts(store: &Store) -> (i64, i64, i64, String) {
    store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM thread", [], |r| r.get(0))?,
                db.query_row(
                    "SELECT count(*) FROM thread WHERE deleted_at IS NOT NULL",
                    [],
                    |r| r.get(0),
                )?,
                db.query_row("SELECT count(*) FROM gc_job", [], |r| r.get(0))?,
                db.query_row(
                    "SELECT value FROM app_meta WHERE key='root_revision'",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn added_descendant_rejects_without_deleting_and_omitted_marker_preserves_old_behavior() {
    let (_dir, store, root) = fixture().await;
    children(&store, &root, &["child".into()]).await;
    let mut guarded = args(&root, true);
    guarded.preview_marker = Some(marker(&preview(&store, &root, true).await));
    assert_eq!(guarded.preview_marker.as_ref().unwrap().thread_count, 2);
    children(&store, &root, &["new-child-after-confirmation".into()]).await;
    let before = facts(&store).await;
    assert_eq!(
        store
            .execute(request(Command::ThreadDelete(guarded)), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PreviewStale
    );
    assert_eq!(
        facts(&store).await,
        before,
        "reject must preserve Threads, root revision and GC admission"
    );
    let old: ThreadDeleteArgs =
        serde_json::from_value(json!({"thread_id":root,"descendants":true})).unwrap();
    assert!(old.preview_marker.is_none());
    store
        .execute(request(Command::ThreadDelete(old)), None)
        .await
        .unwrap();
    let after = facts(&store).await;
    assert_eq!(after.0, 3);
    assert_eq!(after.1, 3);
    assert_eq!(after.2, before.2 + 1);
}

#[tokio::test]
async fn same_count_substitution_rejects_hash_then_new_preview_deletes_only_matching_set() {
    let (_dir, store, root) = fixture().await;
    children(&store, &root, &["old-child".into()]).await;
    let old = marker(&preview(&store, &root, true).await);
    store
        .database
        .call(|db| {
            db.execute(
                "UPDATE thread SET deleted_at='fixture' WHERE id='old-child'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    children(&store, &root, &["replacement-child".into()]).await;
    let new = marker(&preview(&store, &root, true).await);
    assert_eq!(old.thread_count, new.thread_count);
    assert_ne!(old.ids_hash, new.ids_hash);
    let mut guarded = args(&root, true);
    guarded.preview_marker = Some(old);
    let before = facts(&store).await;
    assert_eq!(
        store
            .execute(request(Command::ThreadDelete(guarded.clone())), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PreviewStale
    );
    assert_eq!(facts(&store).await, before);
    guarded.preview_marker = Some(new);
    let result = store
        .execute(request(Command::ThreadDelete(guarded)), None)
        .await
        .unwrap();
    assert_eq!(result.result["accepted"], true);
    assert_eq!(facts(&store).await.1, 3);
}

#[tokio::test]
async fn exact_boundary_overflow_has_no_marker_and_guarded_execute_cannot_expand_its_budget() {
    let (_dir, store, root) = fixture().await;
    children(
        &store,
        &root,
        &(0..256)
            .map(|i| format!("child-{i:04}"))
            .collect::<Vec<_>>(),
    )
    .await;
    let mut guarded = args(&root, true);
    guarded.preview_marker = Some(marker(&preview(&store, &root, true).await));
    assert_eq!(guarded.preview_marker.as_ref().unwrap().thread_count, 257);
    children(&store, &root, &["overflow".into()]).await;
    let value = preview(&store, &root, true).await;
    assert!(value["preview_marker"].is_null());
    assert_eq!(value["preview_marker_unavailable_reason"], "input_limit");
    assert_eq!(
        value["selection"]["thread_count"], 258,
        "legacy exact counts stay exact"
    );
    let before = facts(&store).await;
    assert_eq!(
        store
            .execute(request(Command::ThreadDelete(guarded)), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PreviewStale
    );
    assert_eq!(facts(&store).await, before);
    assert_eq!(marker(&preview(&store, &root, false).await).thread_count, 1);
    store
        .execute(request(Command::ThreadDelete(args(&root, true))), None)
        .await
        .unwrap();
    assert_eq!(
        facts(&store).await.1,
        258,
        "unmarked subtree behavior has no new cap"
    );
}

#[tokio::test]
async fn root_only_marker_does_not_treat_a_new_unselected_child_as_a_mismatch() {
    let (_dir, store, root) = fixture().await;
    let mut guarded = args(&root, false);
    guarded.preview_marker = Some(marker(&preview(&store, &root, false).await));
    children(&store, &root, &["survivor".into()]).await;
    store
        .execute(request(Command::ThreadDelete(guarded)), None)
        .await
        .unwrap();
    assert_eq!(facts(&store).await.1, 1);
    let survivor: bool = store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT deleted_at IS NULL FROM thread WHERE id='survivor'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert!(survivor);
}

#[tokio::test]
async fn deleted_root_is_stale_and_valid_marker_does_not_bypass_new_live_run_blocker() {
    let (_dir, store, root) = fixture().await;
    let mut guarded = args(&root, true);
    guarded.preview_marker = Some(marker(&preview(&store, &root, true).await));
    let target = root.clone();
    store.database.call(move|db|{db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('blocker',?1,'live','fixture')",[target])?;Ok(())}).await.unwrap();
    assert_eq!(
        store
            .execute(request(Command::ThreadDelete(guarded.clone())), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidState
    );
    assert_eq!(facts(&store).await.1, 0);
    store
        .database
        .call(|db| {
            db.execute("UPDATE run SET state='stopped' WHERE id='blocker'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .execute(request(Command::ThreadDelete(args(&root, true))), None)
        .await
        .unwrap();
    let before = facts(&store).await;
    assert_eq!(
        store
            .execute(request(Command::ThreadDelete(guarded)), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PreviewStale
    );
    assert_eq!(facts(&store).await, before);
}

#[test]
fn old_protocol2_request_omits_marker_and_malformed_markers_are_rejected() {
    let old: ThreadDeleteArgs =
        serde_json::from_value(json!({"thread_id":"root","descendants":true})).unwrap();
    let wire = serde_json::to_value(request(Command::ThreadDelete(old))).unwrap();
    assert!(!wire.to_string().contains("preview_marker"));
    let decoded: Request = serde_json::from_value(wire).unwrap();
    decoded.command.validate().unwrap();
    for (count, hash) in [
        (0, "0".repeat(64)),
        (258, "0".repeat(64)),
        (1, "A".repeat(64)),
        (1, "0".repeat(63)),
        (1, "g".repeat(64)),
    ] {
        let mut a = args("root", true);
        a.preview_marker = Some(DeletionPreviewMarker {
            thread_count: count,
            ids_hash: hash,
        });
        assert_eq!(
            Command::ThreadDelete(a).validate().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    let a: ThreadDeleteArgs =
        serde_json::from_value(json!({"thread_id":"root","preview_marker":null})).unwrap();
    assert!(a.preview_marker.is_none());
    let mut valid = args("root", true);
    valid.preview_marker = Some(DeletionPreviewMarker {
        thread_count: 257,
        ids_hash: "a".repeat(64),
    });
    Command::ThreadDelete(valid).validate().unwrap();
    assert_eq!(
        serde_json::to_value(ErrorCode::PreviewStale).unwrap(),
        "preview_stale"
    );
}

#[tokio::test]
async fn set_preview_route_uses_migrated_schema_for_exact_ids_blockers_and_marker() {
    let (_dir, store, root) = fixture().await;
    children(
        &store,
        &root,
        &["selected".into(), "omitted".into(), "deleted".into()],
    )
    .await;
    children(&store, "omitted", &["outside-set-child".into()]).await;
    let root_for_db = root.clone();
    store.database.call(move |db| {
        db.execute("UPDATE thread SET deleted_at='fixture' WHERE id='deleted'", [])?;
        db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('live-root',?1,'live','fixture')", [&root_for_db])?;
        db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('live-outside','outside-set-child','live','fixture')", [])?;
        db.execute("INSERT INTO schedule(id,thread_id,cron,once_at,next_at,enabled,cwd,command) VALUES('scheduled-selected','selected','* * * * *',NULL,0,1,'/tmp','true')", [])?;
        db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('running-scheduled','scheduled-selected','op-scheduled','running','/tmp','{}','UTF-8','UTF-8','closed','fixture')", [])?;
        Ok(())
    }).await.unwrap();

    let mut ids = vec![root.clone(), "selected".into()];
    ids.sort();
    let result = preview_set(&store, ids.clone()).await.unwrap();
    assert_eq!(result["thread_ids"], json!(ids));
    assert_eq!(result["targets"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["preview_marker"],
        serde_json::to_value(expected_set_marker(&ids)).unwrap()
    );
    let root_target = result["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["thread_id"] == root)
        .unwrap();
    assert_eq!(root_target["live_run_count"], 1);
    let selected_target = result["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["thread_id"] == "selected")
        .unwrap();
    assert_eq!(
        selected_target["scheduled_execution_ids"],
        json!(["running-scheduled"])
    );
    assert_eq!(selected_target["scheduled_execution_count"], 1);
    assert_eq!(result["deletable_now"], false);
    assert!(!result["targets"].to_string().contains("outside-set-child"));
    assert!(!result["targets"].to_string().contains("live-outside"));

    assert_eq!(
        preview_set(&store, vec!["unknown".into()])
            .await
            .unwrap_err()
            .code,
        ErrorCode::ThreadNotFound
    );
    assert_eq!(
        preview_set(&store, vec!["deleted".into()])
            .await
            .unwrap_err()
            .code,
        ErrorCode::ThreadNotFound
    );
}
