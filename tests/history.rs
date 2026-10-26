use rusqlite::{Connection, params};
use serde_json::{Value, json};
use shellboy::{
    history::{History, append_node, select_path, text_of_payload},
    protocol::{ErrorCode, HistoryGetArgs, HistoryListArgs, ThreadForkArgs},
    storage::{DbWorker, StorageError},
};
use tempfile::TempDir;

async fn repository() -> (TempDir, DbWorker, History) {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("db")).await.unwrap();
    let cwd = directory.path().join("project");
    let agents = cwd.join("AGENTS.md");
    database.call(move|connection| {
        connection.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,effort,reasoning_epoch,updated_at) VALUES('thread',?1,?2,'proxy','fixture','custom',7,'now')",params![cwd.to_string_lossy(),agents.to_string_lossy()])?;
        Ok(())
    }).await.unwrap();
    let history = History::new(database.clone());
    (directory, database, history)
}

async fn append(database: &DbWorker, id: &str, kind: &str, payload: Value) {
    let id = id.to_owned();
    let kind = kind.to_owned();
    database
        .call(move |connection| {
            let tx = connection.transaction()?;
            append_node(&tx, "thread", &id, None, &kind, &payload, "now")?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
}

fn body(node: &str, limit: u32, cursor: Option<String>) -> HistoryGetArgs {
    HistoryGetArgs {
        node_id: node.into(),
        limit_bytes: limit,
        cursor,
        encoding: "UTF-8".into(),
    }
}
fn listing(thread: &str, limit: u32, cursor: Option<String>) -> HistoryListArgs {
    HistoryListArgs {
        thread_id: thread.into(),
        limit,
        cursor,
        run_id: None,
        since: None,
        state: None,
    }
}
fn path(connection: &Connection, id: &str) -> rusqlite::Result<Vec<String>> {
    connection
        .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
        .query_map([id], |row| row.get(0))?
        .collect()
}

#[tokio::test]
async fn append_is_immutable_atomic_and_updates_exact_path() {
    let (_directory, database, history) = repository().await;
    append(
        &database,
        "a",
        "user",
        json!({"role":"user","content":[{"type":"input_text","text":"first"}]}),
    )
    .await;
    append(
        &database,
        "b",
        "assistant_response",
        json!({"output":[{"type":"message","content":[{"type":"output_text","text":"second"}]}]}),
    )
    .await;
    let result = database
        .call(|connection| {
            let tx = connection.transaction()?;
            append_node(
                &tx,
                "thread",
                "a",
                None,
                "user",
                &json!({"text":"replacement"}),
                "later",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(StorageError::Sqlite(_))));
    let item = history.get(body("a", 65536, None)).await.unwrap();
    assert_eq!(item.text, "first");
    assert_eq!(item.next_node_ids, vec!["b"]);
    database.call(|connection| {
        assert_eq!(path(connection,"thread")?,vec!["a","b"]);
        let (cursor,selection,root):(String,i64,String)=connection.query_row("SELECT cursor_node_id,selection_revision,(SELECT value FROM app_meta WHERE key='root_revision') FROM thread WHERE id='thread'",[],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        assert_eq!((cursor,selection,root),("b".into(),2,"2".into()));
        Ok(())
    }).await.unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn summary_can_precede_existing_suffix_and_fork_keeps_shared_sources() {
    let (_directory, database, history) = repository().await;
    for id in ["a", "b", "c"] {
        append(&database, id, "user", json!({"text":id})).await;
    }
    database.call(|connection| {
        let tx=connection.transaction()?;
        tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('summary','summary','now','{\"text\":\"summary text\"}')",[])?;
        tx.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES('rep','summary','ordered a,b','1','1','ready','now')",[])?;
        tx.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision) VALUES('rep',0,'b',0),('rep',1,'a',0)",[])?;
        tx.execute("INSERT INTO thread_replacement VALUES('thread','rep')",[])?;
        select_path(&tx,"thread",3,&["summary".into(),"c".into()],"now")?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let summary = history.get(body("summary", 65536, None)).await.unwrap();
    assert_eq!(
        summary.source_node_ids,
        vec!["b", "a"],
        "source order follows original ranges, not sorted IDs"
    );
    assert_eq!(summary.next_node_ids, vec!["c"]);
    assert_eq!(
        history
            .get(body("c", 65536, None))
            .await
            .unwrap()
            .previous_node_ids,
        vec!["b", "summary"]
    );
    let child = history
        .fork(ThreadForkArgs {
            thread_id: "thread".into(),
            node_id: "summary".into(),
            cwd: None,
        })
        .await
        .unwrap();
    assert_eq!(
        child.cwd,
        _directory.path().join("project").to_string_lossy()
    );
    assert_eq!(child.connection, "proxy");
    assert_eq!(child.model, "fixture");
    assert_eq!(child.effort.as_deref(), Some("custom"));
    assert_eq!(child.parent_id.as_deref(), Some("thread"));
    assert_eq!(child.run_id, None);
    append(&database, "d", "user", json!({"text":"parent only"})).await;
    let child_id = child.thread_id.clone();
    database.call(move |connection| {
        let tx=connection.transaction()?;
        append_node(&tx,&child_id,"fork_only",None,"user",&json!({"text":"child only"}),"later")?;
        assert_eq!(path(&tx,"thread")?,vec!["summary","c","d"]);
        assert_eq!(path(&tx,&child_id)?,vec!["summary","fork_only"]);
        assert_eq!(tx.query_row("SELECT count(*) FROM node WHERE id='summary'",[],|row| row.get::<_,i64>(0))?,1,"fork must share node, not duplicate it");
        assert_eq!(tx.query_row("SELECT count(*) FROM thread_replacement WHERE thread_id=?1 AND replacement_id='rep'",[&child_id],|row| row.get::<_,i64>(0))?,1);
        assert_eq!(tx.query_row("SELECT reasoning_epoch FROM thread WHERE id=?1",[&child_id],|row| row.get::<_,i64>(0))?,7);
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_cannot_split_tool_calls_from_any_required_result() {
    let (_directory, database, history) = repository().await;
    append(
        &database,
        "response",
        "assistant_response",
        json!({"group_id":"group","output":[
            {"type":"function_call","call_id":"one","name":"shell","arguments":"{}"},
            {"type":"function_call","call_id":"two","name":"shell","arguments":"{}"}
        ]}),
    )
    .await;
    let args = |id: &str| ThreadForkArgs {
        thread_id: "thread".into(),
        node_id: id.into(),
        cwd: None,
    };
    assert_eq!(
        history.fork(args("response")).await.unwrap_err().code,
        ErrorCode::InvalidState
    );
    append(&database,"result_one","tool_result",json!({"group_id":"group","type":"function_call_output","call_id":"one","output":"one done"})).await;
    assert_eq!(
        history.fork(args("result_one")).await.unwrap_err().code,
        ErrorCode::InvalidState
    );
    append(
        &database,
        "additional",
        "user",
        json!({"text":"instruction while tool group runs"}),
    )
    .await;
    append(&database,"result_two","tool_result",json!({"group_id":"group","item":{"type":"function_call_output","call_id":"two","output":"cancelled"}})).await;
    let child = history.fork(args("result_two")).await.unwrap();
    let page = history
        .list(listing(&child.thread_id, 200, None))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 4);
    assert!(page.next_cursor.is_none());
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn list_cursors_bind_revision_and_snippets_limit_unicode_characters() {
    let (_directory, database, history) = repository().await;
    append(&database, "a", "user", json!({"text":"あ".repeat(700)})).await;
    append(&database, "b", "user", json!({"text":"b"})).await;
    let first = history.list(listing("thread", 1, None)).await.unwrap();
    assert_eq!(first.items[0].excerpt.chars().count(), 512);
    let second = history
        .list(listing("thread", 200, first.next_cursor.clone()))
        .await
        .unwrap();
    assert_eq!(second.items[0].node_id, "b");
    assert!(second.next_cursor.is_none());
    append(&database, "c", "user", json!({"text":"c"})).await;
    assert_eq!(
        history
            .list(listing("thread", 1, first.next_cursor))
            .await
            .unwrap_err()
            .code,
        ErrorCode::CursorExpired
    );
    let other = history
        .fork(ThreadForkArgs {
            thread_id: "thread".into(),
            node_id: "c".into(),
            cwd: Some("../different/nested/..".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        other.cwd,
        _directory.path().join("different").to_string_lossy()
    );
    let expected_agents = _directory
        .path()
        .join("project")
        .join("AGENTS.md")
        .to_string_lossy()
        .into_owned();
    database
        .call(move |connection| {
            let agents: String = connection.query_row(
                "SELECT agents_path FROM thread WHERE id=?1",
                [other.thread_id],
                |row| row.get(0),
            )?;
            assert_eq!(agents, expected_agents);
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn body_pages_keep_utf8_boundaries_reject_encoding_change_and_expire_on_tombstone() {
    let (_directory, database, history) = repository().await;
    let original = "あ😀e\n".repeat(10000);
    append(&database, "large", "user", json!({"text":original})).await;
    let first = history.get(body("large", 7, None)).await.unwrap();
    assert_eq!(first.text, "あ😀");
    assert_eq!(
        history.get(body("large", 1, None)).await.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let mut changed = body("large", 7, first.next_cursor.clone());
    changed.encoding = "Shift_JIS".into();
    assert_eq!(
        history.get(changed).await.unwrap_err().code,
        ErrorCode::CursorEncodingMismatch
    );
    let mut assembled = String::new();
    let mut cursor = None;
    loop {
        let page = history.get(body("large", 65536, cursor)).await.unwrap();
        assert!(page.text.len() <= 65536);
        assembled.push_str(&page.text);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(assembled, original);
    database.call(|connection| {
        connection.execute("UPDATE node SET payload_json=NULL,payload_revision=payload_revision+1,deleted_reason='explicit fixture collection' WHERE id='large'",[])?;
        Ok(())
    }).await.unwrap();
    assert_eq!(
        history
            .get(body("large", 7, first.next_cursor))
            .await
            .unwrap_err()
            .code,
        ErrorCode::CursorExpired
    );
    let tombstone = history.get(body("large", 65536, None)).await.unwrap();
    assert_eq!(tombstone.text, "");
    assert_eq!(
        tombstone.tombstone_reason.as_deref(),
        Some("explicit fixture collection")
    );
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_path_adoption_rolls_back_and_dag_cycles_are_rejected() {
    let (_directory, database, _history) = repository().await;
    append(&database, "a", "user", json!({"text":"a"})).await;
    append(&database, "b", "user", json!({"text":"b"})).await;
    let stale = database
        .call(|connection| {
            let tx = connection.transaction()?;
            select_path(&tx, "thread", 1, &["a".into()], "now")?;
            tx.commit()?;
            Ok(())
        })
        .await;
    assert_eq!(
        stale.unwrap_err().into_api_error().code,
        shellboy::protocol::ErrorCode::InvalidState
    );
    let cycle = database
        .call(|connection| {
            let tx = connection.transaction()?;
            select_path(&tx, "thread", 2, &["b".into(), "a".into()], "now")?;
            tx.commit()?;
            Ok(())
        })
        .await;
    assert!(matches!(cycle, Err(StorageError::Sqlite(_))));
    database
        .call(|connection| {
            assert_eq!(path(connection, "thread")?, vec!["a", "b"]);
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_copies_attachment_selection_by_reference_and_only_for_prefix() {
    let (_directory, database, history) = repository().await;
    append(&database, "a", "user", json!({"text":"a"})).await;
    append(&database, "b", "user", json!({"text":"b"})).await;
    database.call(|connection| {
        for (attachment,node) in [("first","a"),("later","b")] {
            connection.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at) VALUES(?1,?2,'/external/same.png','image','external','now')",params![attachment,node])?;
            connection.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order) VALUES('thread',?1,'active','now',0)",[attachment])?;
        }
        Ok(())
    }).await.unwrap();
    let child = history
        .fork(ThreadForkArgs {
            thread_id: "thread".into(),
            node_id: "a".into(),
            cwd: None,
        })
        .await
        .unwrap();
    database
        .call(move |connection| {
            let ids = connection
                .prepare("SELECT attachment_id FROM attachment_selection WHERE thread_id=?1")?
                .query_map([child.thread_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(ids, vec!["first"]);
            assert_eq!(
                connection.query_row("SELECT count(*) FROM attachment_ref", [], |row| row
                    .get::<_, i64>(0))?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    database.shutdown().await.unwrap();
}

#[test]
fn text_extraction_keeps_reasoning_text_and_excludes_opaque_data() {
    let payload = json!({"output":[
        {"type":"reasoning","encrypted_content":"secret opaque","summary":[{"type":"summary_text","text":"summary"}],"text":[{"type":"reasoning_text","text":"visible reasoning"}]},
        {"type":"message","id":"not natural text","content":[{"type":"output_text","text":"answer"}]},
        {"type":"unknown_extension","encrypted_content":"also opaque","provider_metadata":"do not expose","text":"unknown extension text"}
    ]});
    let text = text_of_payload(&payload);
    assert!(text.contains("summary"));
    assert!(text.contains("visible reasoning"));
    assert!(text.contains("answer"));
    assert!(!text.contains("opaque"));
    assert!(!text.contains("not natural text"));
    assert!(!text.contains("do not expose"));
    assert!(!text.contains("unknown extension text"));
}

#[tokio::test]
async fn history_metadata_links_real_executions_and_preserves_fork_origin() {
    let (_root, database, history) = repository().await;
    database.call(|db| {
        let tx=db.transaction()?;
        tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','/parent','/AGENTS.md','proxy','fixture','now'),('fork','/fork-current','/AGENTS.md','proxy','fixture','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','2026-10-25T00:00:00Z'); INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES('origin-run','thread','parent-run','live','2026-10-25T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('origin-turn','thread','origin-run','running','2026-10-25T00:00:00Z'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','origin-turn',0,0,'proxy','fixture',0,'completed','{}','2026-10-25T00:00:00Z');")?;
        append_node(&tx,"thread","user-request",Some("origin-turn"),"user",&json!({"text":"do work"}),"2026-10-25T00:00:00Z")?;
        append_node(&tx,"thread","calls",Some("origin-turn"),"assistant_response",&json!({"output":[{"type":"message","content":[{"type":"output_text","text":"A request with multiple calls"}]}]}),"2026-10-25T09:00:01+09:00")?;
        append_node(&tx,"thread","result",Some("origin-turn"),"tool_result",&json!({"text":"forged execution_id=not-a-shell; do not infer links"}),"2026-10-25T00:00:02Z")?;
        tx.execute("INSERT INTO tool_group VALUES('group','attempt','origin-turn','completed','2026-10-25T00:00:01Z')",[])?;
        for ordinal in 0..2 {
            tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('attempt',?1,'function_call','completed','{}','calls')",[ordinal])?;
        }
        for ordinal in 0..9 {
            let call=format!("call-{ordinal}");let execution=format!("shell-{ordinal}");
            tx.execute("INSERT INTO tool_call VALUES('group',?1,?2,'shell','{}','completed',?3)",params![call,ordinal,execution])?;
            // operation_id is deliberately distinct: the authoritative join is
            // tool_call.operation_id -> shell_execution.id.
            tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,'origin-run','origin-turn',?2,'running','/actual-execution',?3,'UTF-8','UTF-8','closed','2026-10-25T00:00:01Z')",params![execution,format!("receipt-{ordinal}"),json!({"command":format!("printf 日本語\n{}","x".repeat(100))}).to_string()])?;
        }
        tx.execute("INSERT INTO tool_call VALUES('group','never-started',9,'shell','{}','cancelled',NULL)",[])?;
        tx.execute("INSERT INTO tool_call VALUES('group','mismatched-owner',10,'shell','{}','completed','wrong-shell')",[])?;
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('wrong-shell','parent-run','origin-turn','wrong-op','starting','/wrong','{}','UTF-8','UTF-8','closed','now')",[])?;
        tx.execute("INSERT INTO tool_result VALUES('group','call-0','result','running')",[])?;
        tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)SELECT 'fork',ordinal,node_id FROM thread_path WHERE thread_id='thread'",[])?;
        tx.execute("UPDATE thread SET selection_revision=1 WHERE id='fork'",[])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    let page = history.list(listing("fork", 20, None)).await.unwrap();
    assert_eq!(
        page.items.len(),
        3,
        "multiple output items and calls must not duplicate node rows"
    );
    let request = page.items.iter().find(|n| n.node_id == "calls").unwrap();
    assert_eq!(
        request
            .provenance
            .as_ref()
            .unwrap()
            .origin_thread_id
            .as_deref(),
        Some("thread")
    );
    assert_eq!(
        request.context.as_ref().unwrap().owner_run_id.as_deref(),
        Some("parent-run")
    );
    assert_eq!(request.selection.as_ref().unwrap().thread_id, "fork");
    assert_eq!(
        request.selection.as_ref().unwrap().current_cwd,
        "/fork-current"
    );
    assert_eq!(request.execution_count, 9);
    assert_eq!(request.executions.len(), 8);
    assert!(request.executions_truncated);
    let execution = &request.executions[0];
    assert_eq!(execution.execution_id, "shell-0");
    assert_eq!(execution.call_state, "completed");
    assert_eq!(
        execution.state, "running",
        "call result saved does not mean background shell ended"
    );
    assert_eq!(execution.cwd, "/actual-execution");
    assert!(execution.command_excerpt.chars().count() <= 80);
    assert!(!execution.command_excerpt.contains('\n'));
    assert!(execution.command_truncated);
    assert_eq!(execution.request_node_id.as_deref(), Some("calls"));
    assert_eq!(execution.result_node_id.as_deref(), Some("result"));
    assert_eq!(
        page.items
            .iter()
            .find(|n| n.node_id == "result")
            .unwrap()
            .execution_count,
        1
    );
    assert_eq!(
        page.items
            .iter()
            .find(|n| n.node_id == "user-request")
            .unwrap()
            .execution_count,
        0
    );
    let mut args = listing("fork", 1, None);
    args.run_id = Some("origin-run".into());
    args.since = Some("2026-10-25T00:00:01Z".into());
    args.state = Some("running".into());
    let first = history.list(args.clone()).await.unwrap();
    assert_eq!(first.items[0].node_id, "calls");
    let cursor = first.next_cursor.unwrap();
    let mut next = args.clone();
    next.cursor = Some(cursor.clone());
    assert_eq!(
        history.list(next.clone()).await.unwrap().items[0].node_id,
        "result"
    );
    next.run_id = Some("parent-run".into());
    assert_eq!(
        history.list(next).await.unwrap_err().code,
        ErrorCode::CursorExpired
    );
    database
        .call(|db| {
            db.execute(
                "UPDATE shell_execution SET state='exited' WHERE id='shell-0'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    args.cursor = Some(cursor);
    assert_eq!(
        history.list(args).await.unwrap_err().code,
        ErrorCode::CursorExpired,
        "shell state changed without a path revision"
    );
    let mut exited = listing("fork", 20, None);
    exited.state = Some("exited".into());
    assert_eq!(history.list(exited).await.unwrap().items.len(), 2);
    let mut invalid = listing("fork", 20, None);
    invalid.since = Some("yesterday".into());
    assert_eq!(
        history.list(invalid).await.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let mut wrong_run = listing("fork", 20, None);
    wrong_run.run_id = Some("parent-run".into());
    assert!(history.list(wrong_run).await.unwrap().items.is_empty());
    database.shutdown().await.unwrap();
}

#[tokio::test]
async fn audit_received_originals_page_exact_saved_json_without_changing_replay_text() {
    let (_dir, db, history) = repository().await;
    for kind in ["incomplete_response", "model_stream_observation"] {
        let source = kind.to_owned();
        let saved=json!({"partial_reasoning":"理由🦀","partial_text":"保存済み本文".repeat(30),"completed_items":{},"raw_response":{"output":[]}}).to_string();
        let expected = saved.clone();
        let kind = kind.to_owned();
        db.call(move|db|{db.execute("INSERT INTO node(id,kind,payload_json,payload_bytes,created_at)VALUES(?1,?1,?2,length(?2),'now')",rusqlite::params![kind,saved])?;Ok(())}).await.unwrap();
        let mut cursor = None;
        let mut actual = String::new();
        loop {
            let page = history.get(body(&source, 47, cursor)).await.unwrap();
            actual.push_str(&page.text);
            if page.next_cursor.is_none() {
                break;
            }
            cursor = page.next_cursor;
        }
        assert_eq!(actual, expected);
        assert_eq!(
            text_of_payload(&serde_json::from_str::<Value>(&actual).unwrap()),
            ""
        );
    }
    db.shutdown().await.unwrap();
}
