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
