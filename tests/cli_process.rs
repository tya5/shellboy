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

#[test]
fn noninteractive_cli_without_subcommand_reports_no_arguments() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["code"], "invalid_state");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("No subcommand was given"), "{message}");
    assert!(message.ends_with("Arguments received: none"), "{message}");
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
        assert!(value.get("operation_id").is_none());
        assert!(value["error"]["details"].get("next_operation").is_none());
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
    cross_data_directory_sender_label("send").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_ask_records_the_sender_label_and_opens_a_reply_duty() {
    cross_data_directory_sender_label("ask").await;
}

async fn cross_data_directory_sender_label(verb: &str) {
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
                    parent_thread_id: None,
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
            verb,
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
    assert_eq!(
        accepted["expect_reply"].as_bool().unwrap_or(false),
        verb == "ask"
    );
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

#[tokio::test]
async fn headless_startup_errors_stay_on_stderr_without_opening_an_inherited_owner() {
    crate::test_watchdog::watchdog("headless startup error subprocesses", async {
        let root = tempfile::tempdir().unwrap();
        for args in [
            vec!["serve"],
            vec!["serve", "--data-dir="],
            vec!["serve", "--unknown-headless-option"],
            vec![
                "serve",
                "--data-dir",
                "child",
                "--operation-id",
                "not-an-rpc",
            ],
        ] {
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
                .args(args)
                .current_dir(root.path())
                .env("SHELLBOY_DATA_DIR", root.path().join("inherited"))
                .output()
                .await
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let text = String::from_utf8(output.stderr).unwrap();
            assert_eq!(text.lines().count(), 1, "{text}");
            let error: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(error["error"]["code"], "invalid_argument");
            assert!(!root.path().join("inherited").exists());
            assert!(!root.path().join("child").exists());
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_ready_accepts_cli_after_stdin_eof_and_rejects_duplicate_owner() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    crate::test_watchdog::watchdog("headless ready, EOF and lock subprocesses", async {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root.path().join("child");
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .args(["serve", "--data-dir", "child", "--config"])
            .arg(&config)
            .current_dir(root.path())
            .env("SHELLBOY_DATA_DIR", root.path().join("parent"))
            .env("SHELLBOY_SOCKET", "/not-the-child-owner.socket")
            .env("SHELLBOY_INSTANCE_ID", "parent-instance")
            .env("SHELLBOY_THREAD_ID", "parent-thread")
            .env("SHELLBOY_RUN_ID", "parent-run")
            .env("SHELLBOY_TURN_ID", "parent-turn")
            .env("SHELLBOY_EXECUTION_ID", "parent-execution")
            .env("SHELLBOY_ORIGIN_TOKEN", "parent-token")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut stderr = child.stderr.take().unwrap();
        let mut ready_line = String::new();
        assert!(stdout.read_line(&mut ready_line).await.unwrap() > 0);
        let ready: Value = serde_json::from_str(&ready_line).unwrap();
        assert_eq!(ready["event"], "ready");
        assert_eq!(ready["state"], "ready");
        let reported_data = std::path::Path::new(ready["data_dir"].as_str().unwrap());
        assert!(
            reported_data.is_absolute(),
            "readiness must name an absolute directory"
        );
        assert_eq!(
            reported_data.canonicalize().unwrap(),
            data.canonicalize().unwrap(),
            "readiness must identify the actual child directory, not its parent"
        );
        assert_ne!(ready["instance_id"], "parent-instance");
        assert_eq!(
            ready["protocol_version"],
            shellboy::protocol::PROTOCOL_VERSION
        );
        for _ in 0..2 {
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
                .arg("--data-dir")
                .arg(&data)
                .args(["thread", "list"])
                .env_remove("SHELLBOY_DATA_DIR")
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["ok"], true);
            assert!(
                child.try_wait().unwrap().is_none(),
                "EOF must not stop owner"
            );
        }
        let duplicate = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .args(["serve", "--data-dir"])
            .arg(&data)
            .arg("--config")
            .arg(&config)
            .output()
            .await
            .unwrap();
        assert!(!duplicate.status.success());
        assert!(duplicate.stdout.is_empty());
        assert_eq!(
            String::from_utf8(duplicate.stderr).unwrap().lines().count(),
            1
        );
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        let mut tail = String::new();
        stdout.read_to_string(&mut tail).await.unwrap();
        assert!(tail.is_empty(), "no unsolicited stdout after ready: {tail}");
        let mut errors = String::new();
        stderr.read_to_string(&mut errors).await.unwrap();
        assert!(errors.is_empty(), "{errors}");
        let record = root.path().join("stdout.jsonl");
        std::fs::write(&record, &ready_line).unwrap();
        assert_eq!(std::fs::read_to_string(record).unwrap().lines().count(), 1);
        assert!(!root.path().join("parent").exists());
        let db = rusqlite::Connection::open_with_flags(
            data.join("shellboy.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for table in ["operation_receipt", "model_attempt"] {
            assert_eq!(
                db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn headless_non_utf8_data_directory_is_a_startup_error_not_a_json_panic() {
    use std::os::unix::ffi::OsStringExt;
    crate::test_watchdog::watchdog("headless non-UTF-8 startup", async {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'd', 255]));
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .args(["serve", "--data-dir"])
            .arg(&data)
            .arg("--config")
            .arg(config)
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        let error: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(error["ok"], false);
        assert!(!data.join("endpoint.json").exists());
        if data.exists() {
            assert!(shellboy::app::OwnerLock::acquire(&data).is_ok());
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn headless_failed_ready_write_closes_endpoint_and_releases_owner() {
    crate::test_watchdog::watchdog("headless failed ready output cleanup", async {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root.path().join("child");
        // Invalid stdout handles can silently discard writes; use a real broken pipe.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .args(["serve", "--data-dir"])
            .arg(&data)
            .arg("--config")
            .arg(config)
            .stdout(std::process::Stdio::from(writer))
            .kill_on_drop(true)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
            .wait_with_output()
            .await
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        let error: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "io_failed");
        assert_eq!(error["error"]["message"], "Cannot write owner ready line");
        assert!(!data.join("endpoint.json").exists());
        let _lock = shellboy::app::OwnerLock::acquire(&data).unwrap();
        let db = rusqlite::Connection::open_with_flags(
            data.join("shellboy.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let integrity: String = db
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn storage_check_offline_checks_and_refuses_live_owner() {
    let root = tempfile::tempdir().unwrap();
    let db = shellboy::storage::DbWorker::open(root.path().join("shellboy.db"))
        .await
        .unwrap();
    db.shutdown().await.unwrap();
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(root.path())
            .args(["storage", "check"])
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let lock = shellboy::app::OwnerLock::acquire(root.path()).unwrap();
    let output = run();
    assert!(!output.status.success());
    drop(lock);
    let db = rusqlite::Connection::open(root.path().join("shellboy.db")).unwrap();
    db.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at) VALUES('bad','fixture','{}','hash','failed','malformed','now'); PRAGMA ignore_check_constraints=OFF;").unwrap();
    drop(db);
    let output = run();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("quick_check"));
}

#[tokio::test]
async fn storage_vacuum_cli_previews_runs_and_refuses_live_owner() {
    let root = tempfile::tempdir().unwrap();
    let db = shellboy::storage::DbWorker::open(root.path().join("shellboy.db"))
        .await
        .unwrap();
    db.shutdown().await.unwrap();
    let run = |extra: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(root.path())
            .args(["storage", "vacuum"])
            .args(extra)
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output.status.success(), value)
    };
    let (ok, preview) = run(&["--dry-run"]);
    assert!(ok, "{preview}");
    assert_eq!(preview["result"]["dry_run"], true);
    assert_eq!(preview["result"]["sufficient_space"], true);
    let (ok, done) = run(&[]);
    assert!(ok, "{done}");
    assert_eq!(done["result"]["quick_check"], "ok");
    let lock = shellboy::app::OwnerLock::acquire(root.path()).unwrap();
    let (ok, refused) = run(&[]);
    assert!(!ok);
    assert_eq!(refused["error"]["code"], "invalid_state");
    drop(lock);
}

#[cfg(unix)]
#[test]
fn width_cache_fake_terminal_benchmark() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/width_cache_pty.py"
        ))
        .arg(env!("CARGO_BIN_EXE_shellboy"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn protocol_schema_is_standalone_and_matches_the_generated_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .args(["protocol", "schema"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        schema["protocol_version"],
        shellboy::protocol::PROTOCOL_VERSION
    );
    assert!(schema["definitions"]["Request"].is_object());
    assert!(schema["definitions"]["Response"].is_object());

    let rejected = Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .args(["--operation-id", "not-applicable", "protocol", "schema"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalid_argument");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_thread_ids_resolve_unique_prefixes_and_reject_ambiguous_or_missing_targets() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n",
    )
    .unwrap();
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
    let cli = |args: Vec<String>| {
        let directory = owner.loaded.data_dir.clone();
        async move {
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
                .arg("--data-dir")
                .arg(directory)
                .args(args)
                .output()
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            (output.status.success(), value)
        }
    };

    let (ok, created) = cli(vec!["thread".into(), "create".into()]).await;
    assert!(ok, "{created}");
    let full_id = created["result"]["thread_id"].as_str().unwrap().to_owned();
    let prefix = full_id[..8].to_owned();
    let (ok, by_prefix) = cli(vec![
        "thread".into(),
        "get".into(),
        "--thread".into(),
        prefix,
    ])
    .await;
    assert!(ok, "{by_prefix}");
    assert_eq!(by_prefix["result"]["thread_id"], full_id);
    let (ok, by_full_id) = cli(vec![
        "thread".into(),
        "get".into(),
        "--thread".into(),
        full_id.clone(),
    ])
    .await;
    assert!(ok, "{by_full_id}");
    assert_eq!(by_full_id["result"]["thread_id"], full_id);

    owner.db.call(|db| {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('aaaaaaaa-0000-4000-8000-000000000001','/','/a','fixture','fixture','0'),('aaaaaaaa-0000-4000-8000-000000000002','/','/a','fixture','fixture','0')", [])?;
        Ok(())
    }).await.unwrap();
    let (ok, ambiguous) = cli(vec![
        "thread".into(),
        "get".into(),
        "--thread".into(),
        "aaaaaaaa".into(),
    ])
    .await;
    assert!(!ok);
    assert_eq!(ambiguous["error"]["code"], "invalid_argument");
    assert_eq!(ambiguous["error"]["details"]["reason"], "ambiguous");
    assert_eq!(
        ambiguous["error"]["details"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let (ok, missing) = cli(vec![
        "thread".into(),
        "get".into(),
        "--thread".into(),
        "deadbeef".into(),
    ])
    .await;
    assert!(!ok);
    assert_eq!(missing["error"]["code"], "not_found");

    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    task.await.unwrap();
}

async fn cli_prefix_failure(directory: &std::path::Path, args: &[&str]) -> Value {
    let output = crate::test_watchdog::watchdog(
        "CLI prefix rejection",
        tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .kill_on_drop(true)
            .arg("--data-dir")
            .arg(directory)
            .args(args)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn chat_ambiguity_uses_the_same_typed_cli_envelope_as_message_get() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let (send, mut incoming) = tokio::sync::mpsc::channel::<shellboy::ipc::Invocation>(4);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    // Different namespaces have different candidates; the same typed envelope
    // from a synthetic dispatcher proves both boundaries preserve every field.
    let task = tokio::spawn(async move {
        use shellboy::protocol::{ApiError, Command, ErrorCode, IdKind, Response};
        for expected in [IdKind::Thread, IdKind::Message] {
            let invocation = incoming.recv().await.unwrap();
            let Command::IdResolve(args) = invocation.request.command else {
                panic!("target dispatched instead of prefix preflight");
            };
            assert_eq!(args.kind, expected);
            assert_eq!(args.prefix, "aaaaaaaa");
            invocation.reply.send(Response::failure(invocation.request.operation_id,
                ApiError::new(ErrorCode::InvalidArgument, "ID prefix is ambiguous")
                    .hint("Pass more characters")
                    .details(serde_json::json!({"reason":"ambiguous","candidates":["aaaaaaaa-0000-4000-8000-000000000001","aaaaaaaa-0000-4000-8000-000000000002"]})),
            )).unwrap();
        }
    });
    let chat = cli_prefix_failure(&owner.loaded.data_dir, &["chat", "--thread", "aaaaaaaa"]).await;
    let get = cli_prefix_failure(
        &owner.loaded.data_dir,
        &["message", "get", "--message", "aaaaaaaa"],
    )
    .await;
    assert_eq!(chat, get);
    assert_eq!(
        chat["error"]["details"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    task.await.unwrap();
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn chat_missing_capability_uses_the_same_typed_cli_envelope_as_message_get() {
    use shellboy::{
        ipc::{Hello, Welcome},
        protocol::{PROTOCOL_VERSION, read_frame, write_frame},
    };
    let root = tempfile::tempdir().unwrap();
    let address = root.path().join("s");
    let listener = tokio::net::UnixListener::bind(&address).unwrap();
    std::fs::write(root.path().join("endpoint.json"), serde_json::to_vec(&serde_json::json!({
        "transport":"unix", "address": address, "instance_id":"fixture", "protocol_version":PROTOCOL_VERSION,
    })).unwrap()).unwrap();
    let task = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_frame::<_, Hello>(&mut stream).await.unwrap().unwrap();
            write_frame(
                &mut stream,
                &Welcome {
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: "fixture".into(),
                    capabilities: vec![],
                },
            )
            .await
            .unwrap();
            assert!(read_frame::<_, Value>(&mut stream).await.unwrap().is_none());
        }
    });
    let chat = cli_prefix_failure(root.path(), &["chat", "--thread", "aaaaaaaa"]).await;
    let get = cli_prefix_failure(root.path(), &["message", "get", "--message", "aaaaaaaa"]).await;
    assert_eq!(chat, get);
    assert_eq!(
        chat["error"]["details"]["required_capability"],
        "id.resolve"
    );
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_export_streams_saved_sources_and_tombstone() {
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let thread = "aaaaaaaa-0000-4000-8000-000000000001";
    let raw = r#"{ "encrypted_content": "keep\\u0020me", "number": 1.0 }"#;
    owner.db.call(move |db| {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,'/','/a','fixture','fixture','now')",[thread])?;
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,deleted_reason) VALUES
            ('one','reasoning','now',?1,NULL),('two','user','now','{}',NULL),
            ('three','user','now','{}',NULL),('collected','reasoning','now',NULL,'pruned_unreachable'),
            ('summary','summary','now','{}',NULL),('external','user','now','{}',NULL)",[raw])?;
        db.execute("INSERT INTO thread_path VALUES(?1,0,'summary')",[thread])?;
        db.execute_batch("INSERT INTO replacement VALUES('rep','summary','k','f','p','pruned','now');
            INSERT INTO replacement_source VALUES('rep',0,'one',0,NULL),('rep',1,'two',0,NULL),('rep',2,'three',0,NULL),('rep',3,'collected',0,NULL);
            INSERT INTO replacement VALUES('rep-second','summary','k2','f','p','completed','now');
            INSERT INTO replacement_source VALUES('rep-second',7,'collected',0,NULL),('rep-second',12,'one',0,NULL);
            INSERT INTO edge VALUES('one','two'),('two','collected'),('external','collected');")?;
        db.execute("INSERT INTO thread_replacement VALUES(?1,'rep')",[thread])?;
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let task = tokio::spawn(control.clone().serve(receive));
    let path = root.path().join("export.jsonl");
    let output = crate::test_watchdog::watchdog(
        "CLI export",
        tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .kill_on_drop(true)
            .env_remove("SHELLBOY_SOCKET")
            .arg("--data-dir")
            .arg(&owner.loaded.data_dir)
            .args(["thread", "export", "--thread", thread, "--out"])
            .arg(&path)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let bytes = std::fs::read(path).unwrap();
    let lines: Vec<_> = bytes.split_inclusive(|&b| b == b'\n').collect();
    let records: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(records.len(), 7);
    assert_eq!(records[0]["thread"]["thread_id"], thread);
    assert!(records[0]["change_position"].is_u64());
    for (record, id) in records[1..4].iter().zip(["one", "two", "three"]) {
        assert_eq!(record["role"], "replaced_source");
        assert_eq!(record["node"]["id"], id);
    }
    let internal = |id: &str| serde_json::json!({"id":id,"scope":"internal"});
    assert_eq!(records[1]["node"]["payload_json"], raw);
    assert_eq!(
        records[2]["relations"]["previous"],
        serde_json::json!([internal("one")])
    );
    let tombstone = &records[4];
    assert_eq!(tombstone["type"], "tombstone");
    assert_eq!(tombstone["node"]["id"], "collected");
    assert_eq!(tombstone["node"]["provenance"]["kind"], "reasoning");
    assert!(tombstone["node"].get("payload_json").is_none());
    assert_eq!(tombstone["role"], "replaced_source");
    assert_eq!(tombstone["reason"], "pruned_unreachable");
    assert_eq!(
        tombstone["relations"]["previous"],
        serde_json::json!([{"id":"external","scope":"external"}, internal("two")])
    );
    assert_eq!(
        tombstone["relations"]["replaced_by"],
        serde_json::json!([
            {"group_id":"rep","ordinal":3,"output":internal("summary")},
            {"group_id":"rep-second","ordinal":7,"output":internal("summary")}
        ])
    );
    let source = |ordinal, id| serde_json::json!({"ordinal":ordinal,"node":internal(id)});
    assert_eq!(records[5]["role"], "current");
    assert_eq!(records[5]["node"]["current_ordinal"], 0);
    assert_eq!(
        records[5]["relations"]["replacement_groups"],
        serde_json::json!([
            {"id":"rep","state":"pruned","sources":[source(0,"one"),source(1,"two"),source(2,"three"),source(3,"collected")]},
            {"id":"rep-second","state":"completed","sources":[source(7,"collected"),source(12,"one")]}
        ])
    );
    let trailer = &records[6];
    assert_eq!(trailer["lines"], 6);
    assert_eq!(trailer["change_position"], records[0]["change_position"]);
    let mut body = lines[..6].concat();
    let digest = |body: &[u8]| {
        Sha256::digest(body)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    assert_eq!(trailer["sha256"], digest(&body));
    body[1] ^= 1;
    assert_ne!(trailer["sha256"], digest(&body));
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_import_validates_and_atomically_round_trips_saved_history() {
    use crate::runtime_features::{
        answer, incoming_ordinary, owner, proxy, request, send, settled,
    };
    use shellboy::protocol::{
        Command, CompactPreviewArgs, ContextEditAction, ContextEditArgs, OperationTarget,
    };
    let root = tempfile::tempdir().unwrap();
    let (url, mut provider, proxy_task) = proxy().await;
    let owner = owner(root.path(), &url).await;
    std::fs::write(root.path().join("AGENTS.md"), "import fixture").unwrap();
    let source = "aaaaaaaa-0000-4000-8000-000000000002";
    let saved = r#"{ "reasoning_epoch":0, "output":[{"type":"reasoning","encrypted_content":"opaque-secret"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"retained reply"}]}] }"#;
    owner.db.call(move |db| {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,'/','/a','fixture','fixture','now')", [source])?;
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,deleted_reason) VALUES('original','assistant','now',?1,NULL),('old','message','now',NULL,'pruned_unreachable')",[saved])?;
        db.execute_batch(r#"
          INSERT INTO node(id,kind,created_at,payload_json) VALUES
            ('summary','summary','now','{"summary_format":"context-trim-v1","context_trim":{},"item":{"role":"developer","content":[{"type":"input_text","text":"selected summary"}]}}'),
            ('anchored','summary','now','{"context_trim":{"first_node_id":"anchored-source","original_order":["anchored-source","original"],"retained_node_ids":["original"]},"item":{"role":"developer","content":[{"type":"input_text","text":"anchored context notice"}]}}'),
            ('anchored-source','message','now','{"role":"user","content":[{"type":"input_text","text":"anchored original"}]}'),
            ('inner','summary','now','{"item":{"role":"developer","content":[{"type":"input_text","text":"nested source"}]}}'),
            ('call','assistant','now','{"group_id":"imported-group","reasoning_epoch":0,"output":[{"type":"reasoning","id":"bare-opaque","encrypted_content":"spill-secret"},{"type":"function_call","name":"shell","call_id":"import-call","arguments":"{}"}]}'),
            ('result','tool_result','now','{"group_id":"imported-group","item":{"type":"function_call_output","call_id":"import-call","output":"tool-result"}}');
          INSERT INTO replacement VALUES('original-group','inner','source-key','f','p','pruned','now'),('top-group','summary','top-key','f','p','completed','now'),('anchored-group','anchored','anchor-key','f','p','completed','now');
          INSERT INTO replacement_source VALUES('original-group',7,'old',0,NULL),('top-group',0,'inner',0,NULL),('top-group',1,'call',0,NULL),('top-group',2,'result',0,NULL),('anchored-group',0,'anchored-source',0,NULL);
          INSERT INTO edge VALUES('old','inner'),('inner','summary'),('call','summary'),('result','summary'),('summary','anchored'),('anchored-source','anchored'),('anchored','original');
        "#)?;
        db.execute("INSERT INTO thread_path VALUES(?1,0,'summary'),(?1,1,'anchored'),(?1,2,'original')",[source])?;
        db.execute("INSERT INTO thread_replacement VALUES(?1,'original-group'),(?1,'top-group'),(?1,'anchored-group')",[source])?;
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (ipc_send, receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, ipc_send).await.unwrap();
    let task = tokio::spawn(control.clone().serve(receive));
    let file = root.path().join("archive.jsonl");
    let data_dir = &owner.loaded.data_dir;
    let invoke = |args: Vec<String>| async move {
        let output = crate::test_watchdog::watchdog(
            "Thread import CLI",
            tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
                .kill_on_drop(true)
                .env_remove("SHELLBOY_SOCKET")
                .arg("--data-dir")
                .arg(data_dir)
                .arg("thread")
                .args(args)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {output:?}"))
    };
    let export = |thread: &str| {
        vec![
            "export".into(),
            "--thread".into(),
            thread.into(),
            "--out".into(),
            file.to_string_lossy().into_owned(),
        ]
    };
    let import = || {
        vec![
            "import".into(),
            "--in".into(),
            file.to_string_lossy().into_owned(),
        ]
    };
    assert_eq!(invoke(export(source)).await["ok"], true);
    let mut bytes = std::fs::read(&file).unwrap();
    bytes[1] ^= 1;
    std::fs::write(&file, &bytes).unwrap();
    let rejected = invoke(import()).await;
    assert_eq!(rejected["error"]["code"], "invalid_argument", "{rejected}");
    std::fs::write(&file, vec![b'x'; shellboy::protocol::MAX_FRAME_BYTES + 1]).unwrap();
    let rejected = invoke(import()).await;
    assert_eq!(rejected["error"]["code"], "input_limit", "{rejected}");
    let untouched = owner
        .db
        .call(|db| {
            Ok(db.query_row("SELECT count(*) FROM thread", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(untouched, 1);
    assert_eq!(invoke(export(source)).await["ok"], true);
    let response = invoke(import()).await;
    assert_eq!(response["ok"], true, "{response}");
    let imported = response["result"]["thread_id"].as_str().unwrap().to_owned();
    assert_ne!(source, imported);
    assert_eq!(invoke(export(&imported)).await["ok"], true);
    let response = invoke(import()).await;
    assert_eq!(response["ok"], true, "{response}");
    let second = response["result"]["thread_id"].as_str().unwrap().to_owned();
    let witness_thread = second.clone();
    let (marker, anchored) = owner.db.call(move |db| {
        let second = witness_thread;
        let row = db.query_row("SELECT n.id,n.payload_json,n.turn_id,n.import_origin_json,t.reasoning_epoch FROM thread_path p JOIN thread t ON t.id=p.thread_id JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal=2", [&second], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?)))?;
        assert_ne!(row.0, "original");
        assert_eq!(row.1,saved);
        assert!(row.2.is_none());
        assert_eq!(row.4, 0);
        let origins: Value = serde_json::from_str(&row.3).unwrap();
        assert_eq!(origins.as_array().unwrap().len(),2);
        assert_eq!(origins[0]["source_thread_id"], source);
        assert_eq!(origins[1]["source_thread_id"], imported);
        let graph = db.query_row("SELECT r.id,s.ordinal,n.id,n.deleted_reason FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN replacement_source s ON s.replacement_id=r.id JOIN node n ON n.id=s.source_node_id JOIN edge e ON e.src=n.id AND e.dst=r.output_node_id WHERE tr.thread_id=?1 AND r.state='pruned'",[&second],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
        assert_ne!(graph.0,"original-group");
        assert_eq!(graph.1,7);
        assert_ne!(graph.2,"old");
        assert_eq!(graph.3,"pruned_unreachable");
        assert_eq!(db.query_row("SELECT (SELECT count(*) FROM run)+(SELECT count(*) FROM turn)",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT count(*) FROM thread WHERE id=?1",[source],|r|r.get::<_, i64>(0))?,1);
        Ok(db.query_row("SELECT a.node_id,b.node_id FROM thread_path a JOIN thread_path b ON b.thread_id=a.thread_id AND b.ordinal=1 WHERE a.thread_id=?1 AND a.ordinal=0", [&second], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?)
    }).await.unwrap();
    let preview = request(
        &control,
        Command::CompactPreview(Box::new(CompactPreviewArgs {
            thread_id: Some(second.clone()),
            action: Some(ContextEditAction::Summary),
            from_node_id: Some(marker.clone()),
            to_node_id: Some(marker.clone()),
            ..Default::default()
        })),
    )
    .await;
    assert!(!preview.is_null());
    send(&control, &second, "new imported turn").await;
    let outbound = incoming_ordinary(&control, &mut provider).await;
    let input = outbound.body["input"].to_string();
    for text in ["selected summary", "retained reply", "new imported turn"] {
        assert!(input.contains(text), "{input}");
    }
    for text in [
        "opaque-secret",
        "spill-secret",
        "tool-result",
        "nested source",
    ] {
        assert!(!input.contains(text), "{input}");
    }
    outbound
        .reply
        .send((200, answer("ordinary imported answer")))
        .unwrap();
    settled(&control, &second).await;
    for (target, source_local) in [(anchored.clone(), true), (marker, false)] {
        let mut events = control.subscribe();
        let restore = request(
            &control,
            Command::ContextEdit(ContextEditArgs::for_range(
                ContextEditAction::Restore,
                Some(second.clone()),
                Some(target),
                None,
            )),
        )
        .await;
        let operation_id = restore["operation_id"].as_str().unwrap().to_owned();
        crate::test_watchdog::watchdog("imported marker restore", async {
            loop {
                let status = request(
                    &control,
                    Command::OperationGet(OperationTarget {
                        operation_id: operation_id.clone(),
                    }),
                )
                .await;
                match status["downstream"]["state"].as_str() {
                    Some("completed") => {
                        assert!(!source_local, "{status}");
                        break;
                    }
                    Some("failed") => {
                        assert!(source_local, "{status}");
                        assert!(
                            status["downstream"]["outcome"]["error"]["message"]
                                .as_str()
                                .unwrap()
                                .contains("imported marker with source-local anchors"),
                            "{status}"
                        );
                        break;
                    }
                    _ => match events.recv().await {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(error) => panic!("{error}"),
                    },
                }
            }
        })
        .await
        .unwrap();
    }
    let witness_thread = second.clone();
    assert!(
        owner
            .db
            .call(move |db| Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2)",
                [&witness_thread, &anchored],
                |r| r.get::<_, bool>(0)
            )?))
            .await
            .unwrap()
    );
    let config = std::sync::Arc::new(owner.loaded.config.clone());
    let spill = shellboy::compaction::Compactor::new(owner.db.clone(), config)
        .spill(&second, true)
        .await
        .unwrap();
    assert!(spill.spilled_groups > 0);
    send(&control, &second, "continue after spill").await;
    let outbound = incoming_ordinary(&control, &mut provider).await;
    let input = outbound.body["input"].to_string();
    for text in ["nested source", "import-call", "continue after spill"] {
        assert!(input.contains(text), "{input}");
    }
    for text in ["bare-opaque", "opaque-secret", "spill-secret"] {
        assert!(!input.contains(text), "{input}");
    }
    outbound
        .reply
        .send((200, answer("ordinary spilled answer")))
        .unwrap();
    settled(&control, &second).await;
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    task.await.unwrap();
    proxy_task.abort();
}

#[tokio::test]
async fn artifact_cli_round_trip_through_owner_changes_and_restart() {
    async fn start(
        root: &std::path::Path,
    ) -> (
        DataOwner,
        std::sync::Arc<Control>,
        LocalServer,
        tokio::task::JoinHandle<()>,
    ) {
        let owner = DataOwner::start(
            Some(root.join("config.toml")),
            Some(root.join("data")),
            root.into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.into());
        let (send, receive) = tokio::sync::mpsc::channel(8);
        let local = LocalServer::bind(&owner, send).await.unwrap();
        let serving = tokio::spawn(control.clone().serve(receive));
        (owner, control, local, serving)
    }
    async fn cli(owner: &DataOwner, args: &[&str]) -> Value {
        let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(&owner.loaded.data_dir)
            .current_dir(owner.loaded.data_dir.parent().unwrap().join("cli-cwd"))
            .env_remove("SHELLBOY_SOCKET")
            .env_remove("SHELLBOY_INSTANCE_ID")
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["result"].clone()
    }
    async fn hint(owner: &DataOwner, thread: &str, after: Option<&str>) -> String {
        let mut args = vec!["events", "wait", "--thread", thread, "--timeout-ms", "0"];
        if let Some(after) = after {
            args.extend(["--after", after]);
        }
        let result = cli(owner, &args).await;
        if after.is_some() {
            assert_eq!(result["changes"][0]["thread_id"], thread);
        }
        result["cursor"].as_str().unwrap().into()
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("config.toml"), "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let cwd = root.path().join("cli-cwd");
    std::fs::create_dir(&cwd).unwrap();
    #[cfg(unix)]
    let cwd = cwd.canonicalize().unwrap();
    let (owner, control, local, serving) = start(root.path()).await;
    let created = cli(&owner, &["thread", "create"]).await;
    let thread = created["thread_id"].as_str().unwrap();
    let at = hint(&owner, thread, None).await;
    let uri = "missing/../output.txt";
    let absolute = cwd.join(uri);
    let added = cli(
        &owner,
        &[
            "artifact",
            "add",
            "--thread",
            thread,
            "--uri",
            uri,
            "--title",
            "Draft",
            "--mime",
            "text/plain",
        ],
    )
    .await;
    let id = added["artifact_id"].as_str().unwrap();
    let at = hint(&owner, thread, Some(&at)).await;
    let list = cli(&owner, &["artifact", "list", "--thread", thread]).await;
    assert_eq!(
        list["items"][0]["latest"]["uri"],
        absolute.to_str().unwrap()
    );
    let v1 = cli(
        &owner,
        &["artifact", "get", "--artifact", id, "--version", "1"],
    )
    .await;
    assert_eq!(v1["version"]["mime"], "text/plain");
    let relative = "versions/../next.txt";
    let resolved = cli(
        &owner,
        &["artifact", "update", "--artifact", id, "--uri", relative],
    )
    .await;
    assert_eq!(
        resolved["version"]["uri"],
        cwd.join(relative).to_str().unwrap()
    );
    let next = "new+client:opaque value/%2F";
    let unix = "/already/../absolute";
    for uri in [
        unix,
        r"C:\missing\..\output.txt",
        r"\\server\share\output.txt",
        "https://example.test/a%2Fb",
        next,
    ] {
        let updated = cli(
            &owner,
            &["artifact", "update", "--artifact", id, "--uri", uri],
        )
        .await;
        assert_eq!(updated["version"]["uri"], uri);
    }
    hint(&owner, thread, Some(&at)).await;
    local.shutdown().await.unwrap();
    serving.abort();
    let _ = serving.await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    let (owner, control, local, serving) = start(root.path()).await;
    let latest = cli(
        &owner,
        &["artifact", "get", "--artifact", id, "--limit", "1"],
    )
    .await;
    assert_eq!(latest["latest"]["uri"], next);
    assert_eq!(latest["latest"]["title"], "Draft");
    assert!(latest["latest"]["mime"].is_null());
    let cursor = latest["versions"]["next_cursor"].as_str().unwrap();
    let older = cli(
        &owner,
        &["artifact", "get", "--artifact", id, "--cursor", cursor],
    )
    .await;
    assert_eq!(older["versions"]["items"][0]["version"], 6);
    let saved = cli(
        &owner,
        &["artifact", "get", "--artifact", id, "--version", "1"],
    )
    .await;
    assert_eq!(saved["version"], v1["version"]);
    let at = hint(&owner, thread, None).await;
    cli(&owner, &["artifact", "remove", "--artifact", id]).await;
    hint(&owner, thread, Some(&at)).await;
    assert_eq!(
        cli(&owner, &["artifact", "list", "--thread", thread]).await["items"],
        serde_json::json!([])
    );
    local.shutdown().await.unwrap();
    serving.abort();
    let _ = serving.await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn artifact_read_serves_a_published_app_folder_and_refuses_escapes() {
    use base64::Engine;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("config.toml"), "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let cwd = root.path().join("cli-cwd");
    std::fs::create_dir_all(cwd.join("kanban")).unwrap();
    std::fs::write(cwd.join("kanban").join("index.html"), "<script src=app.js>").unwrap();
    std::fs::write(cwd.join("kanban").join("app.js"), "board()").unwrap();
    std::fs::write(cwd.join("notes.txt"), "plain file").unwrap();
    std::fs::write(cwd.join("secret"), "outside").unwrap();
    let owner = DataOwner::start(
        Some(root.path().join("config.toml")),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let (send, receive) = tokio::sync::mpsc::channel(8);
    let local = LocalServer::bind(&owner, send).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let cli = async |args: &[&str]| -> (bool, Value) {
        let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(&owner.loaded.data_dir)
            .current_dir(&cwd)
            .env_remove("SHELLBOY_SOCKET")
            .env_remove("SHELLBOY_INSTANCE_ID")
            .args(args)
            .output()
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        (out.status.success(), value)
    };
    let ok = async |args: &[&str]| -> Value {
        let (success, value) = cli(args).await;
        assert!(success, "{value}");
        value["result"].clone()
    };
    let created = ok(&["thread", "create"]).await;
    let thread = created["thread_id"].as_str().unwrap();
    let added = ok(&[
        "artifact",
        "add",
        "--thread",
        thread,
        "--uri",
        "shellboy-app:./kanban",
        "--title",
        "Kanban",
    ])
    .await;
    let id = added["artifact_id"].as_str().unwrap();
    let uri = added["version"]["uri"].as_str().unwrap();
    let folder = uri.strip_prefix("shellboy-app:").unwrap();
    assert!(std::path::Path::new(folder).is_absolute(), "{uri}");
    let listed = ok(&["artifact", "list", "--thread", thread]).await;
    assert_eq!(listed["items"][0]["latest"]["uri"], uri);
    let read = async |version: &str, path: Option<&str>| -> (bool, Value) {
        let mut args = vec![
            "artifact",
            "read",
            "--thread",
            thread,
            "--artifact",
            id,
            "--version",
            version,
        ];
        if let Some(path) = path {
            args.extend(["--path", path]);
        }
        cli(&args).await
    };
    let decode = |value: &Value| {
        base64::engine::general_purpose::STANDARD
            .decode(value["result"]["base64"].as_str().unwrap())
            .unwrap()
    };
    for (path, expected) in [("index.html", "<script src=app.js>"), ("app.js", "board()")] {
        let (success, value) = read("1", Some(path)).await;
        assert!(success, "{value}");
        assert_eq!(decode(&value), expected.as_bytes());
    }
    let (success, refused) = read("1", Some("../secret")).await;
    assert!(!success);
    assert_eq!(refused["error"]["code"], "invalid_argument");
    assert_eq!(refused["error"]["details"]["reason"], "path_outside_folder");
    ok(&["artifact", "update", "--artifact", id, "--uri", "notes.txt"]).await;
    let (success, file) = read("2", None).await;
    assert!(success, "{file}");
    assert_eq!(decode(&file), b"plain file");
    let (success, refused) = read("2", Some("index.html")).await;
    assert!(!success);
    assert_eq!(refused["error"]["details"]["reason"], "path_not_allowed");
    // The older folder version still resolves exactly as saved.
    let (success, value) = read("1", Some("app.js")).await;
    assert!(success, "{value}");
    assert_eq!(decode(&value), b"board()");
    let (success, missing) = read("3", None).await;
    assert!(!success);
    assert_eq!(missing["error"]["code"], "target_unavailable");
    local.shutdown().await.unwrap();
    serving.abort();
    let _ = serving.await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[test]
fn schedule_add_refuses_retired_positional_command_with_help() {
    for words in [
        vec![
            "schedule",
            "add",
            "--in",
            "10m",
            "--",
            "sh",
            "-c",
            "echo hello",
        ],
        vec![
            "--config=PRIVATE_CONFIG",
            "schedule",
            "--data-dir",
            "PRIVATE_DIRECTORY",
            "add",
            "--in",
            "10m",
            "sh",
            "--description",
            "PRIVATE_DESCRIPTION",
            "-c",
            "echo hello",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .args(words)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["error"]["code"], "invalid_argument");
        let message = response["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(r#"["sh", "-c", "echo hello"]"#),
            "{response}"
        );
        assert!(message.contains("--command '...'"), "{response}");
        assert!(!message.contains("PRIVATE_"), "{response}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn schedule_add_command_round_trips_and_runs_nested_shellboy_send() {
    use std::{path::Path, process::Stdio};
    use tokio::io::{AsyncBufReadExt, BufReader};

    async fn cli(data: &Path, args: &[&str]) -> Value {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(data)
            .args(args)
            .output()
            .await
            .unwrap();
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            output.status.success() && response["ok"] == true,
            "{response}"
        );
        response["result"].clone()
    }

    crate::test_watchdog::watchdog("schedule CLI nested shellboy send", async {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let config = root.path().join("config.toml");
        std::fs::write(&config, "default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:1/v1\"\n[shell]\nkind=\"bash\"\n").unwrap();
        let mut owner = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .args(["serve", "--data-dir"]).arg(&data).arg("--config").arg(config)
            .current_dir(root.path()).stdin(Stdio::null()).stdout(Stdio::piped())
            .kill_on_drop(true).spawn().unwrap();
        let mut stdout = BufReader::new(owner.stdout.take().unwrap());
        let mut ready = String::new();
        stdout.read_line(&mut ready).await.unwrap();
        let ready: Value = serde_json::from_str(&ready).unwrap();
        assert_eq!(ready["event"], "ready");
        let thread = cli(&data, &["thread", "create"]).await;
        let thread = thread["thread_id"].as_str().unwrap();
        let command = r#"sh -c 'shellboy message send --thread "$SHELLBOY_THREAD_ID" --text "Review is needed"'"#;
        let listed = cli(&data, &["schedule", "add", "--thread", thread, "--in", "10m", "--command", command]).await;
        let list = cli(&data, &["schedule", "list", "--thread", thread]).await;
        let items = list["items"].as_array().expect("schedule items");
        let item = items.iter().find(|item| item["schedule_id"] == listed["schedule_id"]).unwrap();
        assert_eq!(item["command"], command);
        let baseline = cli(&data, &["events", "wait", "--thread", thread]).await;
        let mut cursor = baseline["cursor"].as_str().unwrap().to_owned();
        cli(&data, &["schedule", "add", "--thread", thread, "--in", "1s", "--command", command]).await;
        let execution = loop {
            let list = cli(&data, &["shell", "list", "--thread", thread]).await;
            if let Some(item) = list["items"].as_array().unwrap().first() {
                break item["execution_id"].as_str().unwrap().to_owned();
            }
            let changed = cli(&data, &["events", "wait", "--thread", thread, "--after", &cursor, "--timeout-ms", "10000"]).await;
            assert_eq!(changed["status"], "changed", "{changed}");
            cursor = changed["cursor"].as_str().unwrap().to_owned();
        };
        let condition = serde_json::json!([{"type":"shell_ended", "execution_id":execution}]).to_string();
        let settled = cli(&data, &["wait", "--conditions-json", &condition, "--timeout-ms", "10000"]).await;
        assert_eq!(settled["status"], "satisfied", "{settled}");
        let saved = cli(&data, &["shell", "get", "--execution", &execution]).await;
        let read = cli(&data, &["shell", "read", "--execution", &execution]).await;
        assert_eq!(saved["exit_code"], 0, "{saved}: {read}");
        let sent: Value = serde_json::from_str(read["text"].as_str().unwrap()).unwrap();
        assert_eq!(sent["ok"], true, "{sent}");
        assert_eq!(sent["result"]["thread_id"], thread, "{sent}");
        cli(&data, &["shutdown"]).await;
        assert!(owner.wait().await.unwrap().success());
    }).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn named_service_endpoints_follow_the_exact_execution() {
    use shellboy::{
        ipc,
        protocol::{
            Command, ErrorCode, ServiceAddArgs, ServiceEndpointArgs, ServiceSetArgs, ServiceTarget,
            ThreadTarget,
        },
    };
    use std::{
        io::Read,
        os::unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
        path::PathBuf,
    };
    fn publication_pipe(path: &std::path::Path) -> tokio::io::unix::AsyncFd<std::fs::File> {
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .unwrap();
        // The owned, nonblocking file keeps the same descriptor until deregistration.
        unsafe { tokio::io::unix::AsyncFd::register(file) }.unwrap()
    }
    async fn publication(pipe: &tokio::io::unix::AsyncFd<std::fs::File>) -> PathBuf {
        crate::test_watchdog::watchdog("fixture publication signal", async {
            loop {
                let mut ready = pipe.readable().await.unwrap();
                let mut bytes = [0; 1024];
                if let Ok(read) = ready.try_io(|fd| fd.get_ref().read(&mut bytes)) {
                    let line = std::str::from_utf8(&bytes[..read.unwrap()]).unwrap();
                    return PathBuf::from(line.strip_suffix('\n').unwrap());
                }
            }
        })
        .await
        .unwrap()
    }
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let retired = data.join(format!(
        "service-endpoint-{}-{}.json",
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    ));
    let unrelated = data.join("service-endpoint-not-a-publication.json");
    std::fs::write(&retired, b"retired publication fixture").unwrap();
    std::fs::write(&unrelated, b"not a publication").unwrap();
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    assert!(!retired.exists());
    assert!(unrelated.is_file());
    let control = Control::new(&owner, root.path().into());
    let (send, receive) = tokio::sync::mpsc::channel(8);
    let local = LocalServer::bind(&owner, send).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let rpc = async |command| ipc::call(&data, command, None, None).await.unwrap();
    let ok = async |command| {
        let reply = rpc(command).await;
        assert!(reply.ok, "{:?}", reply.error);
        reply.result.unwrap()
    };
    let create = async || {
        ok(
            serde_json::from_value(serde_json::json!({"command":"thread.create","args":{}}))
                .unwrap(),
        )
        .await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let first = create().await;
    let second = create().await;
    let fifo = root.path().join("publication");
    let pipe = publication_pipe(&fifo);
    let fifo = fifo.to_str().unwrap().replace('\'', "'\"'\"'");
    let add = |thread: &str, name: &str, port: u16| {
        Command::ServiceAdd(ServiceAddArgs {
            thread_id: Some(thread.into()),
            name: Some(name.into()),
            cwd: None,
            description: None,
            command: format!(
                r#"file="$SHELLBOY_SERVICE_ENDPOINT_FILE"; test -f "$file" || exit 17; token=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n'); printf '{{"port":{port},"token":"%s"}}' "$token" > "$file"; printf '%s\n' "$file" > '{fifo}'; exec sleep 600"#
            ),
        })
    };
    let id = |result: Value| result["service_id"].as_str().unwrap().to_owned();
    let a = id(ok(add(&first, "tracker", 32101)).await);
    let first_file = publication(&pipe).await;
    let b = id(ok(add(&second, "tracker", 32102)).await);
    let second_file = publication(&pipe).await;
    let other = id(ok(add(&first, "other", 32103)).await);
    let _other_file = publication(&pipe).await;
    let target = |id: &str| ServiceTarget {
        service_id: id.into(),
    };
    let exact = |id: &str| {
        Command::ServiceEndpoint(ServiceEndpointArgs {
            service_id: Some(id.into()),
            thread_id: None,
            name: None,
        })
    };
    let named = |thread: &str| {
        Command::ServiceEndpoint(ServiceEndpointArgs {
            service_id: None,
            thread_id: Some(thread.into()),
            name: Some("tracker".into()),
        })
    };
    let conflict = rpc(add(&first, "tracker", 32104)).await.error.unwrap();
    assert_eq!(conflict.details.unwrap()["reason"], "service_name_conflict");
    let set = |id: &str, name: &str| {
        Command::ServiceSet(ServiceSetArgs {
            service_id: id.into(),
            name: name.into(),
        })
    };
    let conflict = rpc(set(&other, "tracker")).await.error.unwrap();
    assert_eq!(conflict.details.unwrap()["reason"], "service_name_conflict");
    assert_eq!(ok(set(&other, "")).await["name"], Value::Null);
    assert_eq!(
        ok(Command::ServiceGet(target(&other))).await["name"],
        Value::Null
    );
    assert_eq!(ok(set(&other, "renamed")).await["name"], "renamed");
    let observe = async |thread: &str, id: &str, file: &std::path::Path, expected_port| {
        // The child can signal before the owner commits the spawned transition.
        let mut changed = control.subscribe();
        let list =
            crate::test_watchdog::watchdog("owner registers the signalled execution", async {
                loop {
                    let list = ok(Command::ServiceList(ThreadTarget {
                        thread_id: Some(thread.into()),
                    }))
                    .await;
                    let mut selected = list["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["name"] == "tracker");
                    let item = selected.next().unwrap();
                    assert!(selected.next().is_none());
                    assert_eq!(item["service_id"], id);
                    match item["latest"]["state"].as_str() {
                        Some("running") => break list,
                        Some("starting") => {
                            changed.recv().await.unwrap();
                        }
                        state => panic!("unexpected fixture state: {state:?}"),
                    }
                }
            })
            .await
            .unwrap();
        let endpoint = ok(exact(id)).await;
        let metadata = std::fs::metadata(file).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(endpoint["port"], expected_port);
        assert!(
            serde_json::from_slice::<Value>(&std::fs::read(file).unwrap()).unwrap() == endpoint
        );
        let token = endpoint["token"].as_str().unwrap();
        assert!(!list.to_string().contains(token));
        token.to_owned()
    };
    let first_token = observe(&first, &a, &first_file, 32101).await;
    observe(&second, &b, &second_file, 32102).await;
    assert_ne!(first_file, second_file);
    // A contender cannot run recovery against the active owner's publications.
    assert!(
        DataOwner::start(Some(config), Some(data.clone()), root.path().into())
            .await
            .is_err()
    );
    assert!(first_file.is_file());
    assert!(second_file.is_file());
    assert_eq!(ok(named(&first)).await["port"], 32101);
    assert_eq!(ok(named(&second)).await["port"], 32102);
    let binary = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\"'\"'");
    let probe = id(ok(Command::ServiceAdd(ServiceAddArgs {
        thread_id: Some(second.clone()), name: None, cwd: None, description: None,
        command: format!(r#"printf '%s\n' "$SHELLBOY_SERVICE_ENDPOINT_FILE" > '{fifo}'; test "$SHELLBOY_THREAD_ID" = '{second}' || exit 2; result=$('{binary}' service endpoint --thread '{first}' --name tracker) || exit 3; case "$result" in *32101*) echo hub-ok;; *) exit 4;; esac"#),
    })).await);
    let probe_file = publication(&pipe).await;
    let probe_base = probe_file.file_name().unwrap().to_str().unwrap();
    let execution = &probe_base["service-endpoint-".len() + 37..probe_base.len() - ".json".len()];
    let wait = serde_json::from_value(serde_json::json!({"command":"wait", "args":{"conditions":[{"type":"shell_ended","execution_id":execution}],"timeout_ms":30000}})).unwrap();
    let settled = crate::test_watchdog::watchdog("Thread B's exact shell settles", ok(wait))
        .await
        .unwrap();
    assert_eq!(settled["status"], "satisfied");
    let service = ok(Command::ServiceGet(target(&probe))).await;
    assert_eq!(service["latest"]["execution_id"], execution);
    assert_eq!(service["latest"]["exit_code"], 0);
    assert!(
        service["latest"]["output_tail"]
            .as_str()
            .unwrap()
            .contains("hub-ok")
    );
    ok(Command::ServiceStop(target(&a))).await;
    assert!(!first_file.exists());
    let stopped = rpc(exact(&a)).await.error.unwrap();
    assert_eq!(stopped.code, ErrorCode::InvalidState);
    assert_eq!(stopped.details.unwrap()["reason"], "service_not_running");
    assert_eq!(ok(exact(&b)).await["port"], 32102);
    ok(Command::ServiceStart(target(&a))).await;
    let restarted = publication(&pipe).await;
    let restarted_token = observe(&first, &a, &restarted, 32101).await;
    assert_ne!(first_file, restarted);
    assert!(first_token != restarted_token);
    ok(Command::ServiceRemove(target(&a))).await;
    assert!(!restarted.exists());
    assert_eq!(
        rpc(exact(&a)).await.error.unwrap().code,
        ErrorCode::TargetUnavailable
    );
    ok(Command::ServiceStop(target(&b))).await;
    assert!(!second_file.exists());
    local.shutdown().await.unwrap();
    serving.abort();
    let _ = serving.await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
