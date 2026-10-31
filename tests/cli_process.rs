use serde_json::Value;
use shellboy::{app::DataOwner, control::Control, ipc::LocalServer};
use std::process::Command;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_environment_bypasses_metadata_but_never_overrides_another_data_directory() {
    use shellboy::{app::Endpoint, protocol::Response};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let first = DataOwner::start(
        Some(config.clone()),
        Some(root.path().join("first")),
        root.path().into(),
    )
    .await
    .unwrap();
    let second = DataOwner::start(
        Some(config),
        Some(root.path().join("second")),
        root.path().into(),
    )
    .await
    .unwrap();
    let (send, mut a) = tokio::sync::mpsc::channel(4);
    let first_server = LocalServer::bind(&first, send).await.unwrap();
    let (send, mut b) = tokio::sync::mpsc::channel(4);
    let second_server = LocalServer::bind(&second, send).await.unwrap();
    let endpoint: Endpoint = serde_json::from_slice(
        &std::fs::read(first.loaded.data_dir.join("endpoint.json")).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(first.loaded.data_dir.join("endpoint.json")).unwrap();
    let replies = tokio::spawn(async move {
        loop {
            let (item, owner) =
                tokio::select! {item=a.recv()=>(item,"first"),item=b.recv()=>(item,"second")};
            let Some(item) = item else { break };
            let _ = item.reply.send(Response::success(
                item.request.operation_id,
                serde_json::json!({"owner":owner}),
            ));
        }
    });
    for (directory, instance, expected) in [
        (
            &first.loaded.data_dir,
            endpoint.instance_id.as_str(),
            Some("first"),
        ),
        (
            &second.loaded.data_dir,
            endpoint.instance_id.as_str(),
            Some("second"),
        ),
        (&first.loaded.data_dir, "wrong-instance", None),
    ] {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(directory)
            .args(["thread", "list"])
            .env("SHELLBOY_DATA_DIR", &first.loaded.data_dir)
            .env("SHELLBOY_SOCKET", &endpoint.address)
            .env("SHELLBOY_INSTANCE_ID", instance)
            .output()
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        if let Some(expected) = expected {
            assert!(output.status.success(), "{value}");
            assert_eq!(value["result"]["owner"], expected);
        } else {
            assert!(!output.status.success());
            assert_eq!(value["error"]["code"], "protocol_mismatch");
        }
    }
    first_server.shutdown().await.unwrap();
    second_server.shutdown().await.unwrap();
    replies.abort();
    let _ = replies.await;
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
}

#[test]
fn process_help_version_and_errors_use_expected_exit_and_output() {
    let binary = env!("CARGO_BIN_EXE_shellboy");
    for flag in ["--help", "--version"] {
        let output = Command::new(binary).arg(flag).output().unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
    }
    let output = Command::new(binary).arg("does-not-exist").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "invalid_argument");
    assert!(value.get("operation_id").is_none());
    assert_eq!(value["ok"], false);
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(binary)
        .arg("--data-dir")
        .arg(root.path())
        .args(["thread", "list"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "app_not_running");
    assert!(value.get("operation_id").is_none());
    let output = Command::new(binary)
        .arg("--data-dir")
        .arg(root.path())
        .args([
            "--operation-id",
            "known-instance:provided",
            "thread",
            "list",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["operation_id"], "known-instance:provided");
    assert_eq!(value["error"]["code"], "app_not_running");
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_cli_connects_to_owner_and_durable_retransmission_does_not_create_twice() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:9/v1\"\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let task = tokio::spawn(control.clone().serve(receive));
    let operation = format!("{}:cli-fixture", owner.instance_id);
    async fn cli(directory: &std::path::Path, args: &[&str]) -> (bool, Value) {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(directory)
            .args(args)
            .output()
            .await
            .unwrap();
        (
            output.status.success(),
            serde_json::from_slice(&output.stdout).unwrap(),
        )
    }
    let (ok, first) = cli(
        &owner.loaded.data_dir,
        &["--operation-id", &operation, "thread", "create"],
    )
    .await;
    assert!(ok, "{first}");
    let (ok, repeat) = cli(
        &owner.loaded.data_dir,
        &["--operation-id", &operation, "thread", "create"],
    )
    .await;
    assert!(ok);
    assert_eq!(first, repeat);
    let (ok, conflict) = cli(
        &owner.loaded.data_dir,
        &[
            "--operation-id",
            &operation,
            "thread",
            "create",
            "--model",
            "changed",
        ],
    )
    .await;
    assert!(!ok);
    assert_eq!(conflict["error"]["code"], "operation_conflict");
    let (ok, list) = cli(&owner.loaded.data_dir, &["thread", "list"]).await;
    assert!(ok);
    assert_eq!(list["result"]["items"].as_array().unwrap().len(), 1);
    let thread = first["result"]["thread_id"].as_str().unwrap();
    let (ok, accepted) = cli(
        &owner.loaded.data_dir,
        &[
            "message",
            "send",
            "--thread",
            thread,
            "--text",
            "persisted despite missing model connection",
        ],
    )
    .await;
    assert!(ok, "{accepted}");
    assert_eq!(accepted["result"]["delivery_status"], "accepted");
    let message = accepted["result"]["message_id"].as_str().unwrap();
    let (ok, saved) = cli(
        &owner.loaded.data_dir,
        &["message", "get", "--message", message],
    )
    .await;
    assert!(ok, "{saved}");
    assert_eq!(saved["result"]["message_id"], message);
    // Cost inspection must also work before an embedding profile exists. The
    // real CLI/RPC path returns finite counts, without authorizing paid work.
    async fn index_state(control: &Control) -> (i64, i64, i64) {
        control
            .store
            .database
            .call(|db| {
                Ok((
                    db.query_row("SELECT COUNT(*) FROM embedding_job", [], |row| row.get(0))?,
                    db.query_row("SELECT COUNT(*) FROM index_space", [], |row| row.get(0))?,
                    db.query_row(
                        "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'index.%'",
                        [],
                        |row| row.get(0),
                    )?,
                ))
            })
            .await
            .unwrap()
    }
    // The runnable driver independently records index context. Observe that
    // saved boundary before measuring preview's non-mutation contract.
    let mut changes = control.subscribe();
    let observed_key = format!("index.observed.{thread}");
    crate::test_watchdog::watchdog(
        "driver index context saved before preview baseline",
        async {
            loop {
                let key = observed_key.clone();
                let saved = control
                    .store
                    .database
                    .call(move |db| {
                        db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                            [key],
                            |row| row.get::<_, bool>(0),
                        )
                        .map_err(Into::into)
                    })
                    .await
                    .unwrap();
                if saved {
                    break;
                }
                changes.recv().await.unwrap();
            }
        },
    )
    .await
    .unwrap();
    let index_before = index_state(&control).await;
    assert_eq!(index_before, (0, 0, 1));
    let preview_operation = format!("{}:cost-preview", owner.instance_id);
    let preview_args = [
        "--operation-id",
        &preview_operation,
        "index",
        "preview",
        "--source",
        message,
    ];
    let (ok, preview) = cli(&owner.loaded.data_dir, &preview_args).await;
    assert!(ok, "{preview}");
    assert_eq!(preview["result"]["sources"], 1);
    assert!(preview["result"]["characters"].as_u64().unwrap() > 0);
    assert!(preview["result"]["segments"].as_u64().unwrap() > 0);
    let (ok, preview_replay) = cli(&owner.loaded.data_dir, &preview_args).await;
    assert!(ok, "{preview_replay}");
    assert_eq!(preview_replay, preview);
    assert_eq!(index_state(&control).await, index_before);
    // Saturate only a temporary owner's receipt budget. The real CLI must still
    // resolve the receipt-free observation path. Pressure may expire saved
    // terminal outcomes, but retransmission must never create the Thread again.
    let instance = owner.instance_id.clone();
    control.store.database.call(move |db| {
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes) VALUES(?1,?2,'null','fixture','pending','2026-10-25',268435456)", rusqlite::params![format!("{instance}:saturated-fixture"),instance])?;
        Ok(())
    }).await.unwrap();
    let (ok, refused) = cli(&owner.loaded.data_dir, &["thread", "create"]).await;
    assert!(!ok);
    assert_eq!(refused["error"]["code"], "storage_limit");
    let (ok, lookup) = cli(
        &owner.loaded.data_dir,
        &["operation", "get", "--operation", &operation],
    )
    .await;
    assert!(ok, "{lookup}");
    assert_eq!(lookup["result"]["state"], "completed");
    assert_eq!(lookup["result"]["result_expired"], true);
    assert_eq!(lookup["result"]["replay_policy"], "operation_expired");
    assert_eq!(
        lookup["result"]["outcome"]["error"]["code"],
        "operation_expired"
    );
    let (ok, canonical) = cli(
        &owner.loaded.data_dir,
        &["operation", "get", "--operation", &operation],
    )
    .await;
    assert!(ok, "{canonical}");
    assert_eq!(lookup["result"], canonical["result"]);
    let (ok, status) = cli(&owner.loaded.data_dir, &["status"]).await;
    assert!(ok, "{status}");
    assert_eq!(
        status["result"]["receipt_budget"]["full_result_admission_blocked"],
        true
    );
    let (ok, replay) = cli(
        &owner.loaded.data_dir,
        &["--operation-id", &operation, "thread", "create"],
    )
    .await;
    assert!(!ok, "{replay}");
    assert_eq!(replay["error"]["code"], "operation_expired");
    let (ok, list) = cli(&owner.loaded.data_dir, &["thread", "list"]).await;
    assert!(ok, "{list}");
    assert_eq!(list["result"]["items"].as_array().unwrap().len(), 1);
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    task.await.unwrap();
}

#[test]
fn parser_errors_identify_known_arguments_without_echoing_rejected_values() {
    let binary = env!("CARGO_BIN_EXE_shellboy");
    for (args, kind, argument) in [
        (
            vec!["history", "list", "--limit", "PRIVATE_REJECTED_VALUE"],
            "ValueValidation",
            Some("--limit"),
        ),
        (
            vec!["shell", "get"],
            "MissingRequiredArgument",
            Some("--execution"),
        ),
        (
            vec!["PRIVATE_REJECTED_SUBCOMMAND"],
            "InvalidSubcommand",
            None,
        ),
    ] {
        let output = Command::new(binary).args(args).output().unwrap();
        assert!(!output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("PRIVATE_REJECTED"), "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert_eq!(value["error"]["details"]["parser_kind"], kind, "{value}");
        assert!(
            value["error"]["hint"].as_str().unwrap().contains("Usage:"),
            "{value}"
        );
        if let Some(argument) = argument {
            assert!(
                value["error"]["details"]["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == argument),
                "{value}"
            );
        }
        assert!(value.get("operation_id").is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_data_directory_sender_label_survives_but_model_origin_is_stripped() {
    use shellboy::protocol::*;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("receiving")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let created = control
        .store
        .execute(
            Request::new(
                format!("{}:create", owner.instance_id),
                Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    model: None,
                    cwd: None,
                    effort: None,
                    parent_id: None,
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let thread = created["thread_id"].as_str().unwrap().to_owned();
    let (send, mut receive) = tokio::sync::mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let store = control.store.clone();
    let responder = tokio::spawn(async move {
        let invocation = receive.recv().await.unwrap();
        assert!(
            invocation.request.origin.is_none(),
            "cross-owner credentials must be removed by CLI"
        );
        let id = invocation.request.operation_id.clone();
        let result = store
            .execute(invocation.request, None)
            .await
            .unwrap()
            .result;
        invocation
            .reply
            .send(Response::success(id, result.clone()))
            .unwrap();
        result
    });
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .arg("--data-dir")
        .arg(&owner.loaded.data_dir)
        .args([
            "message",
            "send",
            "--thread",
            &thread,
            "--sender-label",
            "other owner: leader",
            "--text",
            "synthetic cross-owner input",
        ])
        .env("SHELLBOY_DATA_DIR", root.path().join("sending"))
        .env("SHELLBOY_INSTANCE_ID", "other-instance")
        .env("SHELLBOY_THREAD_ID", "other-thread")
        .env("SHELLBOY_RUN_ID", "other-run")
        .env("SHELLBOY_TURN_ID", "other-turn")
        .env("SHELLBOY_EXECUTION_ID", "other-shell")
        .env("SHELLBOY_ORIGIN_TOKEN", "PRIVATE_CROSS_OWNER_TOKEN")
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let accepted = responder.await.unwrap();
    let detail = control
        .store
        .execute(
            Request::new(
                "observe-cross-owner",
                Command::MessageGet(MessageTarget {
                    message_id: accepted["message_id"].as_str().unwrap().into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(
        detail["sender"],
        serde_json::json!({"kind":"external_cli","label":"other owner: leader"})
    );
    assert!(!detail.to_string().contains("PRIVATE_CROSS_OWNER_TOKEN"));
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
