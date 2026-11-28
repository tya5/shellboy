use serde_json::json;
use shellboy::{
    app::{DataOwner, OwnerLock},
    ipc::{self, LocalServer},
    protocol::{Command, ErrorCode, Response, ThreadListArgs},
};
use std::path::Path;
use tokio::sync::mpsc;

async fn owner(root: &Path) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config, "").unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.to_path_buf())
        .await
        .unwrap()
}

#[tokio::test]
async fn data_owner_exclusion_distinct_dirs_and_clean_release() {
    let root = tempfile::tempdir().unwrap();
    let first = owner(root.path()).await;
    assert!(OwnerLock::acquire(&first.loaded.data_dir).is_err());
    let other = OwnerLock::acquire(&root.path().join("other")).unwrap();
    drop(other);
    let path = first.loaded.data_dir.clone();
    first.shutdown().await.unwrap();
    assert!(OwnerLock::acquire(&path).is_ok());
}

#[tokio::test]
async fn restart_records_uncertainty_without_reexecuting_or_killing_pids() {
    let root = tempfile::tempdir().unwrap();
    let first = owner(root.path()).await;
    first.db.call(|db|{
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','','','now')",[])?;
        db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now')",[])?;
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now')",[])?;
        db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('s','r','u','op','running','/','{}','UTF-8','UTF-8','open','now')",[])?;
        Ok(())
    }).await.unwrap();
    first.shutdown().await.unwrap();
    let second = owner(root.path()).await;
    let values = second
        .db
        .call(|db| {
            Ok((
                db.query_row("SELECT state FROM run WHERE id='r'", [], |r| {
                    r.get::<_, String>(0)
                })?,
                db.query_row(
                    "SELECT end_reason,output_complete FROM shell_execution WHERE id='s'",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(values.0, "stopped");
    assert_eq!(
        values.1.0,
        if cfg!(unix) {
            "previous_instance_interrupted_os_state_unverified"
        } else {
            "previous_instance_interrupted"
        }
    );
    assert_eq!(values.1.1, 0);
    second.db.call(|db| {
        let text: String = db.query_row(
            "SELECT json_extract(payload_json,'$.text') FROM node WHERE kind='interruption_context' ORDER BY rowid DESC LIMIT 1",
            [], |r| r.get(0),
        )?;
        let snapshot: serde_json::Value = serde_json::from_str(&text).unwrap();
        let shell = &snapshot["change"]["shells"]["items"][0];
        assert_eq!(shell["execution_id"], "s");
        assert_eq!(shell["startup_state"], if cfg!(unix) { "failed" } else { "stopped" });
        if cfg!(unix) { assert_eq!(shell["os_state"], "unverified"); }
        else { assert!(shell.get("os_state").is_none()); }
        assert_eq!(shell["result_uncertain"], true);
        Ok(())
    }).await.unwrap();
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_handshake_reply_and_cancelled_observation() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, mut receive) = mpsc::channel(8);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let directory = owner.loaded.data_dir.clone();
    let client = tokio::spawn(async move {
        ipc::call(
            &directory,
            Command::ThreadList(ThreadListArgs::default()),
            None,
            None,
        )
        .await
    });
    let invocation = receive.recv().await.unwrap();
    assert!(
        invocation
            .request
            .operation_id
            .starts_with(&owner.instance_id)
    );
    invocation
        .reply
        .send(Response::success(
            invocation.request.operation_id,
            json!({"items":[]}),
        ))
        .unwrap();
    assert!(client.await.unwrap().unwrap().ok);
    let directory = owner.loaded.data_dir.clone();
    let client = tokio::spawn(async move {
        ipc::call(
            &directory,
            Command::ThreadList(ThreadListArgs::default()),
            None,
            None,
        )
        .await
    });
    let invocation = receive.recv().await.unwrap();
    client.abort();
    let _ = client.await;
    crate::test_watchdog::watchdog(
        "tests/app_ipc.rs: local_handshake_reply_and_cancelled_observation",
        invocation.disconnected.cancelled(),
    )
    .await
    .unwrap();
    // Receipt processing can still complete after the CLI observer disappears.
    owner
        .db
        .call(|db| {
            db.execute(
                "INSERT INTO app_meta VALUES('cancel-observer','mutation-committed')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        invocation
            .reply
            .send(Response::success(
                invocation.request.operation_id,
                json!({"saved":true})
            ))
            .is_err()
    );
    server.shutdown().await.unwrap();
    let directory = owner.loaded.data_dir.clone();
    owner.shutdown().await.unwrap();
    let error = ipc::call(
        &directory,
        Command::ThreadList(ThreadListArgs::default()),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::AppNotRunning);
}

#[tokio::test]
async fn record_page_contract_cli_and_native_ipc() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let control = shellboy::control::Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    owner.db.call(|db| {
        db.execute_batch(r#"
            INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)
            VALUES ('a','needle a','/','','p','m','2025-01-01T00:00:00Z'),
                   ('b','needle b','/','','p','m','2025-01-02T00:00:00Z');
            INSERT INTO node(id,kind,created_at,payload_json)
            VALUES ('na','message','2025-01-01T00:00:00Z','{}'),
                   ('nb','message','2025-01-02T00:00:00Z','{}');
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)
            VALUES ('ra','oa','{}','a','na','applied','2025-01-01T00:00:00Z','{"reply":{"state":"pending"},"ask_name":"human","to_human":true}'),
                   ('rb','ob','{}','b','nb','applied','2025-01-02T00:00:00Z','{"reply":{"state":"pending"},"ask_name":"human","to_human":true}');
        "#)?;
        Ok(())
    }).await.unwrap();
    let (send, receive) = mpsc::channel(8);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let data = &owner.loaded.data_dir;
    for (args, expected_filters, first, second) in [
        (
            &["thread", "list", "--query", "needle"][..],
            json!({"order":"updated","archived":"exclude","query_omitted":true}),
            "b",
            "a",
        ),
        (
            &["message", "list", "--awaiting", "--to", "human"][..],
            json!({"to_omitted":true}),
            "na",
            "nb",
        ),
    ] {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(data)
            .args(args)
            .args(["--limit", "1"])
            .env_remove("SHELLBOY_SOCKET")
            .env_remove("SHELLBOY_INSTANCE_ID")
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let response: Response<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        let page = response.result.unwrap();
        assert_eq!(page["limit"], 1);
        assert_eq!(page["filters"], expected_filters);
        let cursor = page["next_cursor"].as_str().unwrap();
        let (id_key, command) = if args[0] == "thread" {
            assert_eq!(page["items"][0]["awaiting_thread_count"], 0);
            assert_eq!(page["items"][0]["awaiting_thread_count_exact"], true);
            (
                "thread_id",
                Command::ThreadList(
                    serde_json::from_value(json!({"query":"needle","limit":1,"cursor":cursor}))
                        .unwrap(),
                ),
            )
        } else {
            (
                "message_id",
                Command::MessageList(
                    serde_json::from_value(json!({"to":"human","limit":1,"cursor":cursor}))
                        .unwrap(),
                ),
            )
        };
        assert_eq!(page["items"][0][id_key], first);
        let native = ipc::call(data, command, None, None).await.unwrap();
        let page = native.result.unwrap();
        assert_eq!(page["limit"], 1);
        assert_eq!(page["filters"], expected_filters);
        assert_eq!(page["items"][0][id_key], second);
        assert!(page.get("next_cursor").is_none());
    }
    server.shutdown().await.unwrap();
    serving.await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn rooted_thread_list_pages_and_intersects_exact_ids_over_ipc() {
    use shellboy::protocol::{ThreadGetArgs, ThreadListPage, ThreadListState};

    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let control = shellboy::control::Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    owner.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at,deleted_at) VALUES
            ('root',NULL,'Root','/','','p','m','2026-11-24T00:00:00Z',NULL),
            ('bridge','root','Bridge','/','','p','m','2026-11-24T00:00:00Z','2026-11-24T00:00:01Z'),
            ('leaf','bridge','Leaf','/','','p','m','2026-11-24T00:00:03Z',NULL),
            ('child','root','Child','/','','p','m','2026-11-24T00:00:02Z',NULL),
            ('other',NULL,'Other','/','','p','m','2026-11-24T00:00:04Z',NULL);
            UPDATE thread SET archived_at='2026-11-24T00:00:01Z' WHERE id='child';
            INSERT INTO run(id,thread_id,state,created_at,ended_at) VALUES
            ('finished','leaf','completed','2026-11-24T00:00:00Z','2026-11-24T00:00:01Z'),
            ('active','child','live','2026-11-24T00:00:00Z',NULL);
            INSERT INTO node(id,kind,created_at,payload_json) VALUES ('answer','message','2026-11-24T00:00:01Z','{\"content\":[{\"type\":\"output_text\",\"text\":\"selected answer\"}]}');
            INSERT INTO turn(id,thread_id,run_id,state,created_at,answer_node_id) VALUES
            ('finished-turn','leaf','finished','completed','2026-11-24T00:00:00Z','answer');")?;
        Ok(())
    }).await.unwrap();
    let (send, receive) = mpsc::channel(8);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let data = &owner.loaded.data_dir;
    let list = |args| async move {
        let response = ipc::call(data, Command::ThreadList(args), None, None).await?;
        match response.error {
            Some(error) => Err(error),
            None => Ok(serde_json::from_value::<ThreadListPage>(response.result.unwrap()).unwrap()),
        }
    };
    let args = ThreadListArgs {
        root: Some("root".into()),
        limit: 1,
        include_activity: true,
        include_descendant_count: true,
        archived: shellboy::protocol::ArchivedFilter::Include,
        ..Default::default()
    };
    assert_eq!(
        list(ThreadListArgs {
            include_detail: true,
            ..args.clone()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    let first = list(args.clone()).await.unwrap();
    assert_eq!(first.items[0].thread_id, "leaf");
    let zero = first.items[0].descendants.as_ref().unwrap();
    assert_eq!(zero.total, 0);
    assert!(!zero.capped);
    assert!(!zero.cycle);
    let details = ipc::call(
        data,
        Command::ThreadGet(ThreadGetArgs {
            thread_id: Some("leaf".into()),
            display: false,
        }),
        None,
        None,
    )
    .await
    .unwrap()
    .result
    .unwrap();
    assert_eq!(
        serde_json::to_value(&first.items[0].activity_detail).unwrap(),
        details["activity"]
    );
    assert_eq!(details["activity"]["turn"]["answer_node_id"], "answer");
    let completion = first.items[0].latest_completion.as_ref().unwrap();
    assert_eq!(completion.run_id, "finished");
    assert_eq!(completion.ended_at, "2026-11-24T00:00:01Z");
    let second = list(ThreadListArgs {
        cursor: first.next_cursor.clone(),
        ..args.clone()
    })
    .await
    .unwrap();
    assert_eq!(second.items[0].thread_id, "child");
    assert!(second.next_cursor.is_none());
    assert!(second.read_sequence > first.read_sequence);
    let error = list(ThreadListArgs {
        root: Some("other".into()),
        cursor: first.next_cursor,
        ..args.clone()
    })
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::CursorExpired);
    let active = list(ThreadListArgs {
        state: Some(ThreadListState::ActiveWork),
        ..args.clone()
    })
    .await
    .unwrap();
    assert_eq!(
        active
            .items
            .iter()
            .map(|row| row.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["child"]
    );
    assert!(active.items[0].archived_at.is_some());
    let other = list(ThreadListArgs {
        state: Some(ThreadListState::OtherWork),
        ..args.clone()
    })
    .await
    .unwrap();
    assert_eq!(
        other
            .items
            .iter()
            .map(|row| row.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["leaf"]
    );
    let root_facts = list(ThreadListArgs {
        thread_ids: Some(vec!["root".into()]),
        include_descendant_count: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let descendants = root_facts.items[0].descendants.as_ref().unwrap();
    assert_eq!(descendants.total, 2);
    assert_eq!(descendants.by_state[&ThreadListState::ActiveWork], 1);
    assert_eq!(descendants.by_state[&ThreadListState::Completed], 1);
    let exact = list(ThreadListArgs {
        include_detail: true,
        archived: shellboy::protocol::ArchivedFilter::Exclude,
        state: Some(ThreadListState::Completed),
        thread_ids: Some(vec![
            "root".into(),
            "leaf".into(),
            "child".into(),
            "other".into(),
        ]),
        ..args
    })
    .await
    .unwrap();
    let evidence = exact.items[0].detail.as_ref().unwrap();
    assert_eq!(evidence.parent_title.as_deref(), Some("Bridge"));
    assert!(!evidence.answer_truncated);
    assert!(first.items[0].detail.is_none());
    assert!(!exact.items[0].title_truncated);
    assert!(evidence.parent_deleted);
    assert_eq!(evidence.answer_excerpt.as_deref(), Some("selected answer"));
    assert_eq!(
        exact
            .items
            .into_iter()
            .map(|row| row.thread_id)
            .collect::<Vec<_>>(),
        ["leaf"]
    );
    let long_title = "界".repeat(900);
    let long_answer = "界".repeat(600);
    let title = long_title.clone();
    let answer = long_answer.clone();
    owner
        .db
        .call(move |db| {
            db.execute("UPDATE thread SET title=?1 WHERE id='leaf'", [title])?;
            db.execute(
                "UPDATE node SET payload_json=?1 WHERE id='answer'",
                [
                    serde_json::json!({"content":[{"type":"output_text","text":answer}]})
                        .to_string(),
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let truncated = list(ThreadListArgs {
        thread_ids: Some(vec!["leaf".into()]),
        include_detail: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let row = &truncated.items[0];
    assert!(row.title_truncated && row.title.len() <= 2048);
    assert!(long_title.starts_with(&row.title) && !row.title.is_empty());
    let detail = row.detail.as_ref().unwrap();
    let excerpt = detail.answer_excerpt.as_deref().unwrap();
    assert!(detail.answer_truncated && long_answer.starts_with(excerpt));
    assert_eq!(excerpt.chars().count(), 512);
    server.shutdown().await.unwrap();
    serving.await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
