//! Stage C uses synthetic owners and HTTP request/reply barriers, never timing sleeps.
use crate::runtime_features::{answer, incoming, owner, proxy, request, send, settled, thread};
use serde_json::{Value, json};
use shellboy::{
    control::Control,
    protocol::{
        Command, ConfigChange, ConfigGetArgs, ConfigSetArgs, CredentialsSetArgs, ErrorCode,
        Request, ThreadCreateArgs,
    },
};
use std::sync::Arc;

fn change(field: &str, value: Value) -> ConfigChange {
    ConfigChange {
        field: field.into(),
        value: Some(value),
    }
}
async fn edit(control: &Arc<Control>, changes: Vec<ConfigChange>) -> Value {
    let got = request(control, Command::ConfigGet(ConfigGetArgs {})).await;
    request(
        control,
        Command::ConfigSet(ConfigSetArgs {
            expected_hash: got["file"]["content_hash"].as_str().unwrap().into(),
            changes,
        }),
    )
    .await
}
async fn named_thread(control: &Arc<Control>, name: &str) -> String {
    request(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            connection: Some(name.into()),
            model: Some("fixture".into()),
            agents: None,
            cwd: None,
            effort: None,
            parent_id: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .into()
}
async fn recovery(control: &Arc<Control>, thread: &str) -> Value {
    settled(control, thread).await;
    let target = thread.to_owned();
    control.store.database.call(move |db| {
        let (state, reason): (String, String) = db.query_row(
            "SELECT state,recovery_reason_json FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",
            [target], |row| Ok((row.get(0)?, row.get(1)?)))?;
        assert_eq!(state, "recovery_wait");
        Ok(serde_json::from_str(&reason).unwrap())
    }).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_connections_swap_during_request_and_defaults_apply_without_restart() {
    crate::test_watchdog::watchdog("live connections swap", async {
        let root = tempfile::tempdir().unwrap();
        let (old_url, mut old_receive, old_server) = proxy().await;
        let (new_url, mut new_receive, new_server) = proxy().await;
        let owner = owner(root.path(), &old_url).await;
        let control = Control::new(&owner, root.path().into());
        let existing = thread(&control).await;
        send(&control, &existing, "old request").await;
        let pending = incoming(&control, &mut old_receive).await;
        let changed = edit(
            &control,
            vec![
                change("connections.fixture.base_url", json!(new_url)),
                change("connections.next", json!({"base_url":new_url})),
                change("default_connection", json!("next")),
                change("default_model", json!("new-model")),
            ],
        )
        .await;
        assert_eq!(changed["restart_required"], false);
        assert_eq!(changed["applied_to_running_owner"], true);
        for entry in changed["changes"].as_array().unwrap() {
            assert_eq!(entry["restart_required"], false, "{entry}");
            assert_eq!(entry["applied_to_running_owner"], true, "{entry}");
        }
        pending
            .reply
            .send((200, answer("old client completed")))
            .unwrap();
        settled(&control, &existing).await;
        // The existing Thread keeps its chosen model, but its next POST uses the new endpoint.
        send(&control, &existing, "new request").await;
        let next = incoming(&control, &mut new_receive).await;
        assert_eq!(next.body["model"], "fixture");
        assert!(next.body.to_string().contains("old client completed"));
        next.reply
            .send((200, answer("new client completed")))
            .unwrap();
        settled(&control, &existing).await;
        let fresh = thread(&control).await;
        send(&control, &fresh, "new defaults").await;
        let next = incoming(&control, &mut new_receive).await;
        assert_eq!(next.body["model"], "new-model");
        next.reply
            .send((200, answer("new defaults completed")))
            .unwrap();
        settled(&control, &fresh).await;
        let got = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
        assert_eq!(got["loaded"]["values"]["default_connection"], "next");
        assert_eq!(got["loaded"]["values"]["default_model"], "new-model");
        assert_eq!(
            got["loaded"]["values"]["connections"]["fixture"]["base_url"],
            new_url
        );
        assert_eq!(got["differing_fields"], json!([]));
        assert_eq!(got["restart"]["all_fields_require_restart"], false);
        // Byte hashes still honestly distinguish the startup file from its edited bytes.
        assert_eq!(got["differs"], true);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        old_server.abort();
        new_server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_connections_removal_during_run_preserves_response_and_recovers_without_panic() {
    crate::test_watchdog::watchdog("live connections removal", async {
        for status in [200, 500] {
            let root = tempfile::tempdir().unwrap();
            let (url, mut receive, server) = proxy().await;
            let owner = owner(root.path(), &url).await;
            let control = Control::new(&owner, root.path().into());
            edit(&control, vec![change("connections.transient", json!({"base_url":url}))]).await;
            let target = named_thread(&control, "transient").await;
            send(&control, &target, "preserved input").await;
            let pending = incoming(&control, &mut receive).await;
            edit(&control, vec![ConfigChange { field:"connections.transient".into(), value:None }]).await;
            pending.reply.send((status, if status == 200 { answer("preserved response") }
                else { json!({"error":{"message":"synthetic server failure"}}) })).unwrap();
            if status == 200 {
                settled(&control, &target).await;
                let id = target.clone();
                let state: String = control.store.database.call(move |db| Ok(db.query_row(
                    "SELECT state FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1", [id], |r|r.get(0))?)).await.unwrap();
                assert_eq!(state, "completed");
                send(&control, &target, "next request needs explicit recovery").await;
            }
            assert_eq!(recovery(&control, &target).await["code"], "config_invalid");
            let id = target.clone();
            let (attempts, retained): (i64, bool) = control.store.database.call(move |db| Ok((
                db.query_row("SELECT count(*) FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1", [&id], |r|r.get(0))?,
                db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.payload_json LIKE '%preserved input%')", [&id], |r|r.get(0))?
            ))).await.unwrap();
            assert_eq!(attempts, 1, "missing connection never sends another POST");
            assert!(retained);
            assert!(receive.try_recv().is_err());
            control.shutdown().await.unwrap(); owner.shutdown().await.unwrap(); server.abort();
        }
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_connections_config_and_credentials_writes_are_serialized() {
    crate::test_watchdog::watchdog("live connections credentials", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = owner(root.path(), &url).await;
        let control = Control::new(&owner, root.path().into());
        let got = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
        let set_config = request(
            &control,
            Command::ConfigSet(ConfigSetArgs {
                expected_hash: got["file"]["content_hash"].as_str().unwrap().into(),
                changes: vec![
                    change("default_model", json!("concurrent-model")),
                    change(
                        "connections.fixture.headers",
                        json!({"X-Synthetic":"updated"}),
                    ),
                ],
            }),
        );
        let set_key = request(
            &control,
            Command::CredentialsSet(CredentialsSetArgs {
                connection: "fixture".into(),
                value: "synthetic-stage-c-key".into(),
            }),
        );
        let (written, key) = tokio::join!(set_config, set_key);
        assert_eq!(written["restart_required"], false);
        assert!(!key.to_string().contains("synthetic-stage-c-key"));
        let got = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
        assert_eq!(got["loaded"]["credential_source"]["fixture"], "stored");
        assert_eq!(got["loaded"]["values"]["default_model"], "concurrent-model");
        assert_eq!(got["differing_fields"], json!([]));
        let target = thread(&control).await;
        send(&control, &target, "after both writes").await;
        let next = incoming(&control, &mut receive).await;
        assert_eq!(next.body["model"], "concurrent-model");
        next.reply
            .send((200, answer("concurrent writes complete")))
            .unwrap();
        settled(&control, &target).await;
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_connections_default_deletion_is_rejected_before_writing() {
    crate::test_watchdog::watchdog("live connections default deletion", async {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(root.path(), "http://127.0.0.1:1/v1").await;
        let control = Control::new(&owner, root.path().into());
        let got = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
        let bytes = std::fs::read(root.path().join("config.toml")).unwrap();
        let response = control
            .request(Request::new(
                format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                Command::ConfigSet(ConfigSetArgs {
                    expected_hash: got["file"]["content_hash"].as_str().unwrap().into(),
                    changes: vec![
                        ConfigChange {
                            field: "default_connection".into(),
                            value: None,
                        },
                        ConfigChange {
                            field: "connections.fixture".into(),
                            value: None,
                        },
                    ],
                }),
            ))
            .await;
        let error = response.error.unwrap();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert_eq!(error.details.unwrap()["written"], false);
        assert_eq!(
            std::fs::read(root.path().join("config.toml")).unwrap(),
            bytes
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_connections_catalog_removal_keeps_its_start_generation() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };
    crate::test_watchdog::watchdog("live catalog generation", async {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (started, admitted) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
                assert!(head.len() < 16 * 1024);
            }
            assert!(String::from_utf8(head).unwrap().starts_with("GET /v1/models "));
            started.send(()).unwrap();
            released.await.unwrap();
            let body = r#"{"data":[{"id":"catalog-model"}]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let owner = owner(root.path(), "http://127.0.0.1:1/v1").await;
        let control = Control::new(&owner, root.path().into());
        edit(&control, vec![change("connections.catalog", json!({"base_url":url,"api":"chat_completions"}))]).await;
        let pending_control = control.clone();
        let pending = tokio::spawn(async move {
            request(&pending_control, Command::ModelList(shellboy::protocol::ModelListArgs {
                connection:Some("catalog".into()), thread_id:None,
            })).await
        });
        admitted.await.unwrap();
        edit(&control,vec![ConfigChange {field:"connections.catalog".into(),value:None}]).await;
        release.send(()).unwrap();
        let result = pending.await.unwrap();
        assert_eq!(result["connection"],"catalog");
        assert_eq!(result["api"],"chat_completions");
        assert_eq!(result["models"],json!(["catalog-model"]));
        let missing=control.request(Request::new(format!("{}:{}",control.store.instance_id,uuid::Uuid::new_v4()),
            Command::ModelList(shellboy::protocol::ModelListArgs {connection:Some("catalog".into()),thread_id:None}))).await;
        assert_eq!(missing.error.unwrap().code,ErrorCode::ConfigInvalid);
        server.await.unwrap();
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}
