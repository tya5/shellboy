//! Blank stored titles are resolved from the selected user path without writes.
use serde_json::{Value, json};
use shellboy::{config::Config, control::db::Store, protocol::*, storage::DbWorker};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

async fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    let config=Config::parse("default_connection=\"test\"\ndefault_model=\"fixture\"\n[connections.test]\nbase_url=\"http://127.0.0.1:1/v1\"\n").unwrap();
    let store = Store::new(
        db,
        Arc::new(config),
        root.path().join("project"),
        "title-test".into(),
    );
    (root, store)
}
fn request(command: Command) -> Request {
    Request::new(format!("title-test:{}", uuid::Uuid::new_v4()), command)
}
async fn execute(store: &Store, command: Command) -> Value {
    store.execute(request(command), None).await.unwrap().result
}
async fn create(store: &Store) -> ThreadDetails {
    serde_json::from_value(
        execute(
            store,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        )
        .await,
    )
    .unwrap()
}
async fn details(store: &Store, id: &str) -> ThreadDetails {
    serde_json::from_value(
        execute(
            store,
            Command::ThreadGet(ThreadTarget {
                thread_id: Some(id.into()),
            }),
        )
        .await,
    )
    .unwrap()
}
async fn list(
    store: &Store,
    limit: u32,
    cursor: Option<String>,
    query: Option<&str>,
) -> Page<ThreadSummary> {
    serde_json::from_value(
        execute(
            store,
            Command::ThreadList(ThreadListArgs {
                limit,
                cursor,
                query: query.map(Into::into),
            }),
        )
        .await,
    )
    .unwrap()
}
async fn send(store: &Store, id: &str, text: &str) -> MessageAccepted {
    serde_json::from_value(
        execute(
            store,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                now: false,
                thread_id: Some(id.into()),
                text: text.into(),
            }),
        )
        .await,
    )
    .unwrap()
}
async fn apply_user_fixture(store: &Store, thread: &str, message: &str) {
    let thread = thread.to_owned();
    let message = message.to_owned();
    store.database.call(move |db| {
        let tx = db.transaction()?;
        tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)SELECT ?1,COALESCE(MAX(ordinal),-1)+1,?2 FROM thread_path WHERE thread_id=?1",rusqlite::params![thread,message])?;
        tx.execute("UPDATE receipt SET state='applied' WHERE target_thread_id=?1 AND message_node_id=?2",rusqlite::params![thread,message])?;
        tx.execute("UPDATE thread SET cursor_node_id=?2 WHERE id=?1",rusqlite::params![thread,message])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
}
async fn append(store: &Store, thread: &str, kind: &str, payload: Option<Value>) -> String {
    let node = uuid::Uuid::new_v4().to_string();
    let returned = node.clone();
    let thread = thread.to_owned();
    let kind = kind.to_owned();
    store.database.call(move|db| {
        let tx=db.transaction()?;
        let body=payload.map(|value|value.to_string());
        tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'now',?3,?4)",rusqlite::params![node,kind,body,body.as_ref().map_or(0,|body|body.len()) as i64])?;
        tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)SELECT ?1,COALESCE(MAX(ordinal),-1)+1,?2 FROM thread_path WHERE thread_id=?1",rusqlite::params![thread,node])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    returned
}
fn user(text: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}]})
}

#[tokio::test]
async fn real_send_populates_blank_display_title_from_first_user_not_latest_output() {
    let (_root, store) = fixture().await;
    let thread = create(&store).await;
    assert_eq!(thread.title, "未送信");
    let accepted = send(&store, &thread.thread_id, "  最初の\n依頼\tを確認  ").await;
    assert_eq!(
        details(&store, &thread.thread_id).await.title,
        "最初の 依頼 を確認"
    );
    apply_user_fixture(&store, &thread.thread_id, &accepted.message_id).await;
    append(&store,&thread.thread_id,"assistant_response",Some(json!({"output":[{"type":"message","content":[{"type":"output_text","text":"最後の回答はタイトルにしない"}]}]}))).await;
    append(
        &store,
        &thread.thread_id,
        "shell",
        Some(json!({"text":"TOOL_METADATA_NOT_TITLE"})),
    )
    .await;
    send(&store, &thread.thread_id, "二度目のユーザー本文").await;
    let title = details(&store, &thread.thread_id).await.title;
    assert_eq!(title, "最初の 依頼 を確認");
    let page = list(&store, 50, None, None).await;
    assert_eq!(page.items[0].title, title);
    let id = thread.thread_id.clone();
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT title FROM thread WHERE id=?1", [id], |r| r
                    .get::<_, String>(0))?,
                ""
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(!accepted.message_id.is_empty());
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_title_is_preserved_and_shared_fork_uses_its_selected_user_path() {
    let (_root, store) = fixture().await;
    let source = create(&store).await;
    let first = send(&store, &source.thread_id, "共有する最初の依頼").await;
    apply_user_fixture(&store, &source.thread_id, &first.message_id).await;
    let branch: ThreadDetails = serde_json::from_value(
        execute(
            &store,
            Command::ThreadFork(ThreadForkArgs {
                agents: None,
                thread_id: source.thread_id.clone(),
                node_id: first.message_id,
                cwd: None,
            }),
        )
        .await,
    )
    .unwrap();
    assert_eq!(branch.title, "共有する最初の依頼");
    let source_id = source.thread_id.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE thread SET title='明示タイトル Ω' WHERE id=?1",
                [source_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        details(&store, &source.thread_id).await.title,
        "明示タイトル Ω"
    );
    assert_eq!(
        details(&store, &branch.thread_id).await.title,
        "共有する最初の依頼"
    );
    let page = list(&store, 50, None, None).await;
    assert_eq!(
        page.items
            .iter()
            .find(|item| item.thread_id == source.thread_id)
            .unwrap()
            .title,
        "明示タイトル Ω"
    );
    assert_eq!(
        list(&store, 50, None, Some("明示タイトル ω"))
            .await
            .items
            .len(),
        1
    );
    assert_eq!(
        list(&store, 50, None, Some("共有する最初"))
            .await
            .items
            .len(),
        1
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_known_user_input_text_counts_and_empty_inputs_have_explicit_fallbacks() {
    let (_root, store) = fixture().await;
    let thread = create(&store).await;
    append(
        &store,
        &thread.thread_id,
        "assistant_response",
        Some(json!({"text":"not a user title"})),
    )
    .await;
    assert_eq!(details(&store, &thread.thread_id).await.title, "未送信");
    append(&store,&thread.thread_id,"user",Some(json!({"role":"user","metadata":{"text":"SECRET_METADATA"},"content":[{"type":"input_image","text":"not input text"},{"type":"function_call","text":"TOOL_METADATA"}]}))).await;
    assert_eq!(details(&store, &thread.thread_id).await.title, "名称未設定");
    append(&store, &thread.thread_id, "user", Some(user(" \n\t "))).await;
    append(&store, &thread.thread_id, "user", None).await;
    assert_eq!(details(&store, &thread.thread_id).await.title, "名称未設定");
    append(&store,&thread.thread_id,"user",Some(json!({"role":"user","content":[{"type":"input_text","text":"後続の最初"},{"type":"text","text":"の本文"}]}))).await;
    assert_eq!(
        details(&store, &thread.thread_id).await.title,
        "後続の最初 の本文"
    );
    assert!(
        list(&store, 50, None, Some("SECRET_METADATA"))
            .await
            .items
            .is_empty()
    );
    assert!(
        list(&store, 50, None, Some("TOOL_METADATA"))
            .await
            .items
            .is_empty()
    );
    assert_eq!(
        list(&store, 50, None, Some("後続の最初 の本文"))
            .await
            .items
            .len(),
        1
    );
    let legacy = create(&store).await;
    append(&store,&legacy.thread_id,"user",Some(json!({"role":"user","content":"  旧形式の\nユーザー本文 ","metadata":{"text":"NOT_A_TITLE"}}))).await;
    assert_eq!(
        details(&store, &legacy.thread_id).await.title,
        "旧形式の ユーザー本文"
    );
    assert_eq!(
        list(&store, 50, None, Some("旧形式の ユーザー本文"))
            .await
            .items
            .len(),
        1
    );
    let metadata = create(&store).await;
    append(
        &store,
        &metadata.thread_id,
        "user",
        Some(json!({"role":"user","content":{"text":"OBJECT_METADATA_ONLY"}})),
    )
    .await;
    assert_eq!(
        details(&store, &metadata.thread_id).await.title,
        "名称未設定"
    );
    assert!(
        list(&store, 50, None, Some("OBJECT_METADATA_ONLY"))
            .await
            .items
            .is_empty()
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn displayed_title_search_paginates_matches_and_retains_query_watermark() {
    let (_root, store) = fixture().await;
    for i in 0..7 {
        let thread = create(&store).await;
        send(
            &store,
            &thread.thread_id,
            if i % 2 == 0 {
                "日本\nAlpha 検索対象"
            } else {
                "除外対象"
            },
        )
        .await;
        let id = thread.thread_id;
        store
            .database
            .call(move |db| {
                db.execute(
                    "UPDATE thread SET updated_at=?2 WHERE id=?1",
                    rusqlite::params![id, format!("2026-10-25T00:00:0{i}Z")],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let first = list(&store, 2, None, Some("日本 alpha")).await;
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());
    assert!(
        first
            .items
            .iter()
            .all(|item| item.title == "日本 Alpha 検索対象")
    );
    let extra = create(&store).await;
    send(&store, &extra.thread_id, "日本 Alpha 新規だがwatermark外").await;
    let second = list(&store, 2, first.next_cursor.clone(), Some("日本 alpha")).await;
    assert_eq!(second.items.len(), 2);
    assert!(second.next_cursor.is_none());
    assert!(second.items.iter().all(|item| {
        !first
            .items
            .iter()
            .any(|old| old.thread_id == item.thread_id)
    }));
    let changed = store
        .execute(
            request(Command::ThreadList(ThreadListArgs {
                limit: 2,
                cursor: first.next_cursor,
                query: Some("changed".into()),
            })),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(changed.code, ErrorCode::CursorExpired);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn derived_titles_strip_controls_and_bound_unicode_without_splitting_graphemes() {
    let (_root, store) = fixture().await;
    let thread = create(&store).await;
    let raw = format!("\u{1b}[31m日本\u{1b}[0m\r\n{}", "e\u{301}🙂".repeat(100));
    send(&store, &thread.thread_id, &raw).await;
    let title = details(&store, &thread.thread_id).await.title;
    assert!(title.starts_with("日本 e\u{301}🙂"));
    assert!(title.ends_with('…'));
    assert!(title.graphemes(true).count() <= 80);
    assert!(title.chars().count() <= 160);
    assert!(!title.chars().any(char::is_control));
    assert_eq!(
        list(&store, 50, None, Some("日本 e\u{301}🙂")).await.items[0].title,
        title
    );
    let family = create(&store).await;
    send(&store, &family.thread_id, &"👨‍👩‍👧‍👦".repeat(100)).await;
    let title = details(&store, &family.thread_id).await.title;
    assert!(title.chars().count() <= 160);
    assert!(title.graphemes(true).all(|g| g == "👨‍👩‍👧‍👦" || g == "…"));
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_accepted_and_held_titles_do_not_leak_other_threads_or_unselected_applied_history()
{
    let (_root, store) = fixture().await;
    let target = create(&store).await;
    let other = create(&store).await;
    send(&store, &other.thread_id, "別Threadの本文は使わない").await;
    let first = send(&store, &target.thread_id, "受付済みの最初の本文").await;
    let second = send(&store, &target.thread_id, "次に受付した本文").await;
    assert_eq!(
        details(&store, &target.thread_id).await.title,
        "受付済みの最初の本文"
    );
    let message = first.message_id.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE receipt SET state='held' WHERE message_node_id=?1",
                [message],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        details(&store, &target.thread_id).await.title,
        "受付済みの最初の本文"
    );
    let message = first.message_id;
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE receipt SET state='cancelled' WHERE message_node_id=?1",
                [message],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        details(&store, &target.thread_id).await.title,
        "次に受付した本文"
    );
    let message = second.message_id;
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE receipt SET state='applied' WHERE message_node_id=?1",
                [message],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(details(&store, &target.thread_id).await.title, "未送信");
    append(
        &store,
        &target.thread_id,
        "user",
        Some(user("選択経路の本文を優先")),
    )
    .await;
    send(&store, &target.thread_id, "未適用の新しい本文").await;
    assert_eq!(
        details(&store, &target.thread_id).await.title,
        "選択経路の本文を優先"
    );
    assert_eq!(
        list(&store, 50, None, Some("選択経路の本文を優先"))
            .await
            .items
            .len(),
        1
    );
    store.database.shutdown().await.unwrap();
}

fn set_title(thread: Option<&str>, title: Option<&str>, model: Option<&str>) -> Command {
    Command::ThreadSet(ThreadSetArgs {
        agents: None,
        connection: None,
        thread_id: thread.map(Into::into),
        cwd: None,
        model: model.map(Into::into),
        effort: None,
        title: title.map(Into::into),
    })
}

#[tokio::test]
async fn explicit_rename_reset_and_omitted_title_preserve_model_context_boundaries() {
    let (_root, store) = fixture().await;
    let thread = create(&store).await;
    send(&store, &thread.thread_id, "自動名の元になる依頼").await;
    let id = thread.thread_id.clone();
    let before=store.database.call(move|db| {
        db.execute("UPDATE thread SET updated_at='2000-01-01T00:00:00Z' WHERE id=?1",[&id])?;
        Ok(db.query_row("SELECT revision,profile_revision,reasoning_epoch,(SELECT count(*) FROM event WHERE target_thread_id=?1 AND kind='context_changed') FROM thread WHERE id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)))?)
    }).await.unwrap();
    let renamed: ThreadDetails = serde_json::from_value(
        execute(
            &store,
            set_title(Some(&thread.thread_id), Some("  名前を変更 Ω  "), None),
        )
        .await,
    )
    .unwrap();
    assert_eq!(renamed.title, "名前を変更 Ω");
    assert_eq!(renamed.model, thread.model);
    let id = thread.thread_id.clone();
    store.database.call(move|db| {
        let after=db.query_row("SELECT revision,profile_revision,reasoning_epoch,(SELECT count(*) FROM event WHERE target_thread_id=?1 AND kind='context_changed'),updated_at,title FROM thread WHERE id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?;
        assert_eq!((after.0,after.1,after.2,after.3),(before.0+1,before.1,before.2,before.3));
        assert_ne!(after.4,"2000-01-01T00:00:00Z");assert_eq!(after.5,"名前を変更 Ω");Ok(())
    }).await.unwrap();
    assert_eq!(
        list(&store, 50, None, Some("名前を変更 ω"))
            .await
            .items
            .len(),
        1
    );
    assert!(
        list(&store, 50, None, Some("自動名の元"))
            .await
            .items
            .is_empty()
    );
    let settings: ThreadDetails = serde_json::from_value(
        execute(
            &store,
            set_title(Some(&thread.thread_id), None, Some("new-model")),
        )
        .await,
    )
    .unwrap();
    assert_eq!(settings.title, "名前を変更 Ω");
    assert_eq!(settings.model, "new-model");
    let reset: ThreadDetails = serde_json::from_value(
        execute(
            &store,
            set_title(Some(&thread.thread_id), Some("   \u{3000}  "), None),
        )
        .await,
    )
    .unwrap();
    assert_eq!(reset.title, "自動名の元になる依頼");
    assert_eq!(reset.model, "new-model");
    assert_eq!(
        list(&store, 50, None, Some("自動名の元")).await.items.len(),
        1
    );
    assert!(
        list(&store, 50, None, Some("名前を変更"))
            .await
            .items
            .is_empty()
    );
    let id = thread.thread_id;
    store.database.call(move|db| {
        assert_eq!(db.query_row("SELECT title FROM thread WHERE id=?1",[&id],|r|r.get::<_,String>(0))?,"");
        assert_eq!(db.query_row("SELECT profile_revision FROM thread WHERE id=?1",[&id],|r|r.get::<_,i64>(0))?,before.1+1);
        assert_eq!(db.query_row("SELECT reasoning_epoch FROM thread WHERE id=?1",[&id],|r|r.get::<_,i64>(0))?,before.2+1);
        assert_eq!(db.query_row("SELECT count(*) FROM event WHERE target_thread_id=?1 AND kind='context_changed'",[id],|r|r.get::<_,i64>(0))?,before.3+1);
        Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn title_setter_replays_once_resolves_caller_and_does_not_require_valid_old_profile() {
    let (_root, store) = fixture().await;
    let thread = create(&store).await;
    let id = thread.thread_id.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE thread SET profile_id='removed-connection' WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let origin = Origin {
        instance_id: "title-test".into(),
        thread_id: thread.thread_id.clone(),
        run_id: "fixture-run".into(),
        turn_id: "fixture-turn".into(),
        execution_id: "fixture-shell".into(),
        token: "fixture-issued-origin".into(),
    };
    let req = request(set_title(None, Some("自身のThreadを改名"), None));
    let first = store
        .execute(req.clone(), Some(origin.clone()))
        .await
        .unwrap()
        .result;
    assert_eq!(first["title"], "自身のThreadを改名");
    assert_eq!(
        store
            .execute(req.clone(), Some(origin.clone()))
            .await
            .unwrap()
            .result,
        first
    );
    let mut changed = req;
    changed.command = set_title(None, Some("別名"), None);
    assert_eq!(
        store.execute(changed, Some(origin)).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    let id = thread.thread_id;
    store.database.call(move|db| {
        let result=db.query_row("SELECT revision,profile_revision,reasoning_epoch,profile_id FROM thread WHERE id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?;
        assert_eq!(result,(1,0,0,"removed-connection".into()));Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn title_setter_validates_single_line_unicode_limit_before_mutation_and_accepts_combined_settings()
 {
    let (_root, store) = fixture().await;
    let thread = create(&store).await;
    for invalid in [
        "a\nb".into(),
        "a\rb".into(),
        "a\tb".into(),
        "\0".into(),
        "\u{1b}[31m".into(),
        "a\u{2028}b".into(),
        "a\u{2029}b".into(),
        "名".repeat(513),
    ] {
        assert_eq!(
            store
                .execute(
                    request(set_title(Some(&thread.thread_id), Some(&invalid), None)),
                    None
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(details(&store, &thread.thread_id).await.title, "未送信");
    let exact = "名".repeat(512);
    let result = execute(
        &store,
        set_title(
            Some(&thread.thread_id),
            Some(&exact),
            Some("combined-model"),
        ),
    )
    .await;
    assert_eq!(result["title"], exact);
    assert_eq!(result["model"], "combined-model");
    let omitted =
        serde_json::to_value(set_title(Some(&thread.thread_id), None, Some("next-model"))).unwrap();
    assert!(
        omitted["args"].get("title").is_none(),
        "omission retains old operation fingerprints"
    );
    let reset = execute(&store, set_title(Some(&thread.thread_id), Some(""), None)).await;
    assert_eq!(reset["title"], "未送信");
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn list_excerpt_uses_selected_speech_not_event_reasoning_tools_or_other_threads() {
    let (_root, store) = fixture().await;
    let thread = create(&store).await;
    let other = create(&store).await;
    append(
        &store,
        &thread.thread_id,
        "user_message",
        Some(user("依頼の本文")),
    )
    .await;
    let answer=append(&store,&thread.thread_id,"assistant_response",Some(json!({"output":[
        {"type":"reasoning","summary":[{"type":"summary_text","text":"推論は抜粋へ出さない"}],"encrypted_content":"opaque"},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"回答 日本語🦀"}]},
        {"type":"function_call","call_id":"call","arguments":"{\"command\":\"TOOL MUST NOT PREVIEW\"}"}
    ]}))).await;
    append(
        &store,
        &thread.thread_id,
        "event",
        Some(json!({"text":"Shellboy event v1: {\"kind\":\"index_state\"}"})),
    )
    .await;
    append(
        &store,
        &thread.thread_id,
        "tool_result",
        Some(json!({"text":"RAW RESULT MUST NOT PREVIEW"})),
    )
    .await;
    append(&store,&thread.thread_id,"assistant_response",Some(json!({"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"最新だが推論だけ"}]},{"type":"vendor_opaque","text":"OPAQUE MUST NOT PREVIEW"}]}))).await;
    append(
        &store,
        &other.thread_id,
        "assistant_response",
        Some(json!({"text":"OTHER THREAD MUST NOT PREVIEW"})),
    )
    .await;
    let page = list(&store, 20, None, None).await;
    let item = page
        .items
        .iter()
        .find(|item| item.thread_id == thread.thread_id)
        .unwrap();
    assert_eq!(item.excerpt, "回答 日本語🦀");
    let id = thread.thread_id.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "DELETE FROM thread_path WHERE thread_id=?1 AND node_id=?2",
                rusqlite::params![id, answer],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let page = list(&store, 20, None, None).await;
    assert_eq!(
        page.items
            .iter()
            .find(|item| item.thread_id == thread.thread_id)
            .unwrap()
            .excerpt,
        "依頼の本文"
    );
    store.database.shutdown().await.unwrap();
}
