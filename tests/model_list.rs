//! Model catalog and query-observation IPC boundaries against isolated HTTP servers.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

struct Incoming {
    head: String,
    reply: oneshot::Sender<(u16, Value)>,
}
async fn proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                loop {
                    let mut byte = [0];
                    if stream.read(&mut byte).await.unwrap() == 0 {
                        return;
                    }
                    bytes.push(byte[0]);
                    if bytes.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8(bytes).unwrap();
                let (reply, result) = oneshot::channel();
                if send.send(Incoming { head, reply }).await.is_err() {
                    return;
                }
                if let Ok((status, body)) = result.await {
                    let body = body.to_string();
                    let _ = stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                }
            });
        }
    });
    (format!("http://{address}"), receive, task)
}
async fn owner(root: &std::path::Path, url: &str) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config, format!("default_connection=\"first\"\ndefault_model=\"unchanged-model\"\n[connections.first]\nbase_url=\"{url}/first/v1\"\n[connections.second]\nbase_url=\"{url}/second/v1\"\n[connections.second.headers]\nAuthorization=\"Bearer private-catalog-fixture\"\n")).unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
fn request(control: &Control, command: Command) -> Request {
    Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        command,
    )
}
fn list(connection: Option<&str>, thread: Option<&str>) -> Command {
    Command::ModelList(ModelListArgs {
        connection: connection.map(Into::into),
        thread_id: thread.map(Into::into),
    })
}
async fn incoming(rx: &mut mpsc::Receiver<Incoming>) -> Incoming {
    timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn fetch(
    control: &Arc<Control>,
    rx: &mut mpsc::Receiver<Incoming>,
    command: Command,
    path: &str,
) -> Value {
    let control = control.clone();
    let req = request(&control, command);
    let call = tokio::spawn(async move { control.request(req).await });
    let got = incoming(rx).await;
    assert!(
        got.head.starts_with(&format!("GET {path} HTTP/1.1\r\n")),
        "{}",
        got.head
    );
    if path.contains("second") {
        assert!(
            got.head
                .to_lowercase()
                .contains("authorization: bearer private-catalog-fixture")
        );
    }
    got.reply
        .send((200, json!({"data":[{"id":"z"},{"id":"a"},{"id":"z"}]})))
        .unwrap();
    let result = call.await.unwrap();
    assert!(result.ok, "{:?}", result.error);
    let value = result.result.unwrap();
    let typed: ModelListResult = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(typed.models, ["a", "z"]);
    value
}
#[tokio::test]
async fn default_explicit_and_thread_catalog_selection_never_changes_thread_or_generates() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut rx, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let create = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: Some("second".into()),
            cwd: None,
            model: None,
            effort: None,
            parent_id: None,
        }),
    );
    let thread = control.request(create).await.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        fetch(&control, &mut rx, list(None, None), "/first/v1/models").await["connection"],
        "first"
    );
    assert_eq!(
        fetch(
            &control,
            &mut rx,
            list(Some("second"), None),
            "/second/v1/models"
        )
        .await["connection"],
        "second"
    );
    assert_eq!(
        fetch(
            &control,
            &mut rx,
            list(None, Some(&thread)),
            "/second/v1/models"
        )
        .await["connection"],
        "second"
    );
    assert_eq!(
        fetch(
            &control,
            &mut rx,
            list(Some("first"), Some(&thread)),
            "/first/v1/models"
        )
        .await["connection"],
        "first"
    );
    control
        .store
        .database
        .call(move |db| {
            let (profile, model): (String, String) = db.query_row(
                "SELECT profile_id,model_id FROM thread WHERE id=?1",
                [thread],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(
                (profile.as_str(), model.as_str()),
                ("second", "unchanged-model")
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
    assert!(rx.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
#[tokio::test]
async fn failed_catalog_fetch_is_redacted_and_same_id_observes_again_without_receipts() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut rx, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let req = request(&control, list(Some("second"), None));
    let copied = req.clone();
    let worker = control.clone();
    let call = tokio::spawn(async move { worker.request(copied).await });
    incoming(&mut rx)
        .await
        .reply
        .send((
            401,
            json!({"error":{"message":"private-catalog-fixture","code":"not_allowed"}}),
        ))
        .unwrap();
    let failed = call.await.unwrap();
    assert!(!failed.ok);
    assert_eq!(failed.error.as_ref().unwrap().code, ErrorCode::IoFailed);
    assert!(
        !serde_json::to_string(&failed)
            .unwrap()
            .contains("private-catalog-fixture")
    );
    assert!(failed.error.as_ref().unwrap().message.contains("401"));
    for connection in ["second", "first"] {
        let mut observation = req.clone();
        observation.command = list(Some(connection), None);
        let worker = control.clone();
        let call = tokio::spawn(async move { worker.request(observation).await });
        let got = incoming(&mut rx).await;
        assert!(
            got.head
                .starts_with(&format!("GET /{connection}/v1/models "))
        );
        got.reply
            .send((200, json!({"data":[{"id":"fresh"}]})))
            .unwrap();
        let fresh = call.await.unwrap();
        assert!(fresh.ok, "{:?}", fresh.error);
        assert_eq!(fresh.result.unwrap()["models"], json!(["fresh"]));
    }
    assert_eq!(
        owner
            .db
            .call(|db| Ok(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .await
            .unwrap(),
        0
    );
    assert!(rx.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
#[tokio::test]
async fn invalid_thread_unknown_connection_forged_origin_and_disconnected_caller_never_fetch() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut rx, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    for (command, expected) in [
        (list(None, Some("missing")), ErrorCode::ThreadNotFound),
        (
            list(Some("first"), Some("missing")),
            ErrorCode::ThreadNotFound,
        ),
        (list(Some("missing"), None), ErrorCode::ConfigInvalid),
    ] {
        assert_eq!(
            control
                .request(request(&control, command))
                .await
                .error
                .unwrap()
                .code,
            expected
        );
    }
    let mut forged = request(&control, list(Some("first"), None));
    forged.origin = Some(Origin {
        instance_id: control.store.instance_id.clone(),
        thread_id: "missing".into(),
        run_id: "r".into(),
        turn_id: "t".into(),
        execution_id: "s".into(),
        token: "forged".into(),
    });
    assert_eq!(
        control.request(forged).await.error.unwrap().code,
        ErrorCode::InvalidArgument
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let req = request(&control, list(None, None));
    let cancelled = control.request_cancellable(req.clone(), cancel).await;
    assert_eq!(
        cancelled.error.as_ref().unwrap().code,
        ErrorCode::InvalidState
    );
    assert!(rx.try_recv().is_err());
    // A disconnected observation is not a saved failed operation. A connected
    // caller using the same correlation ID must fetch current state again.
    let worker = control.clone();
    let fresh = tokio::spawn(async move { worker.request(req).await });
    incoming(&mut rx)
        .await
        .reply
        .send((200, json!({"data":[{"id":"after-cancel"}]})))
        .unwrap();
    let fresh = fresh.await.unwrap();
    assert!(fresh.ok, "{:?}", fresh.error);
    assert_eq!(fresh.operation_id, cancelled.operation_id);
    assert_eq!(fresh.result.unwrap()["models"], json!(["after-cancel"]));
    assert_eq!(
        owner
            .db
            .call(|db| Ok(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |row| row
                    .get::<_, i64>(0))?
            ))
            .await
            .unwrap(),
        0
    );
    control.shutdown().await.unwrap();
    assert_eq!(
        control
            .request(request(&control, list(None, None)))
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::InvalidState
    );
    owner.shutdown().await.unwrap();
    server.abort();
}
#[tokio::test]
async fn owner_shutdown_cancels_pending_catalog_without_creating_a_receipt() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut rx, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let req = request(&control, list(None, None));
    let worker = control.clone();
    let copied = req.clone();
    let call = tokio::spawn(async move { worker.request(copied).await });
    let pending = incoming(&mut rx).await;
    timeout(Duration::from_secs(5), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    let result = call.await.unwrap();
    assert_eq!(result.error.unwrap().code, ErrorCode::InvalidState);
    let operation = req.operation_id;
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM operation_receipt WHERE operation_id=?1",
                    [operation],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    drop(pending);
    owner.shutdown().await.unwrap();
    server.abort();
}
#[test]
fn model_list_contract_rejects_blank_and_unknown_fields_and_strictly_types_results() {
    for args in [
        json!({"connection":" "}),
        json!({"thread_id":""}),
        json!({"extra":true}),
    ] {
        assert!(
            Request::from_value(
                json!({"protocol_version":shellboy::protocol::PROTOCOL_VERSION,"operation_id":"op","command":"model.list","args":args})
            )
            .is_err()
        );
    }
    let req = Request::from_value(
        json!({"protocol_version":shellboy::protocol::PROTOCOL_VERSION,"operation_id":"op","command":"model.list","args":{}}),
    )
    .unwrap();
    assert_eq!(req.command, list(None, None));
    assert!(
        serde_json::from_value::<ModelListResult>(
            json!({"connection":"first","models":[],"extra":1})
        )
        .is_err()
    );
}

#[tokio::test]
async fn concurrent_identical_catalog_observations_and_later_same_id_fetch_fresh_results() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut rx, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let req = request(&control, list(None, None));
    let a = control.clone();
    let ar = req.clone();
    let first = tokio::spawn(async move { a.request(ar).await });
    let got = incoming(&mut rx).await;
    let b = control.clone();
    let br = req.clone();
    let second = tokio::spawn(async move { b.request(br).await });
    let other = incoming(&mut rx).await;
    got.reply
        .send((200, json!({"data":[{"id":"first"}]})))
        .unwrap();
    other
        .reply
        .send((200, json!({"data":[{"id":"second"}]})))
        .unwrap();
    let first = first.await.unwrap();
    let second = second.await.unwrap();
    assert!(first.ok && second.ok);
    assert_eq!(first.result.unwrap()["models"], json!(["first"]));
    assert_eq!(second.result.unwrap()["models"], json!(["second"]));
    let worker = control.clone();
    let call = tokio::spawn(async move { worker.request(req).await });
    incoming(&mut rx)
        .await
        .reply
        .send((200, json!({"data":[{"id":"third"}]})))
        .unwrap();
    assert_eq!(
        call.await.unwrap().result.unwrap()["models"],
        json!(["third"])
    );
    assert_eq!(
        owner
            .db
            .call(|db| Ok(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .await
            .unwrap(),
        0
    );
    assert!(rx.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn receipt_free_observation_checks_the_complete_transport_envelope() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut rx, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let req = Request::new("i".repeat(MAX_FRAME_BYTES - 4096), list(None, None));
    assert!(
        serde_json::to_vec(&req).unwrap().len() < MAX_FRAME_BYTES,
        "the request itself must fit the wire frame"
    );
    let worker = control.clone();
    let call = tokio::spawn(async move { worker.request(req).await });
    let models = (0..128)
        .map(|i| json!({"id": format!("model-{i:03}-{}", "x".repeat(64))}))
        .collect::<Vec<_>>();
    incoming(&mut rx)
        .await
        .reply
        .send((200, json!({"data":models})))
        .unwrap();
    let response = call.await.unwrap();
    assert_eq!(response.error.as_ref().unwrap().code, ErrorCode::InputLimit);
    assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_FRAME_BYTES);
    assert_eq!(
        owner
            .db
            .call(|db| Ok(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .await
            .unwrap(),
        0
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn runtime_observations_keep_receipts_constant_and_release_raw_output_pins() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut rx, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let created = control
        .request(request(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        ))
        .await;
    let thread = created.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    owner.db.call(move |db| {
        let tx = db.transaction()?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('read-run',?1,'completed','now')", [&thread])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('read-turn',?1,'read-run','completed','now')", [&thread])?;
        tx.execute_batch("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('read-shell','read-run','read-turn','read-tool','exited','/tmp','{}','UTF-8','UTF-8','closed',1,0,1,'now','now');")?;
        tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('read-shell',0,'stdout',0,?1,'now')", [vec![b'a';65536]])?;
        tx.commit()?; Ok(())
    }).await.unwrap();
    let usage = || {
        |db: &mut rusqlite::Connection| {
            Ok((
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row(
                    "SELECT logical_bytes FROM quota_counter WHERE category='operation_receipt'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT COALESCE(SUM(reserved_bytes),0) FROM operation_receipt",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        }
    };
    let before = owner.db.call(usage()).await.unwrap();
    let commands = [
        list(None, None),
        Command::StorageStatus(StorageArgs { category: None }),
        Command::IndexStatus(IndexArgs::default()),
        Command::IndexPreview(IndexArgs {
            dimensions: None,
            scope: Some(IndexScope::All),
            ..Default::default()
        }),
        Command::HistorySearch(HistorySearchArgs {
            query: "no selected space".into(),
            ..Default::default()
        }),
        Command::ShellGet(ExecutionTarget {
            execution_id: "read-shell".into(),
        }),
        Command::ShellRead(ShellReadArgs {
            execution_id: "read-shell".into(),
            cursor: None,
            encoding: Some("UTF-8".into()),
            limit_bytes: 65536,
        }),
    ];
    for round in 0..10 {
        for command in &commands {
            // Deliberately reuse one correlation ID across arguments and rounds.
            let req = Request::new("observation:shared", command.clone());
            let result = if matches!(command, Command::ModelList(_)) {
                let worker = control.clone();
                let call = tokio::spawn(async move { worker.request(req).await });
                incoming(&mut rx)
                    .await
                    .reply
                    .send((200, json!({"data":[{"id":format!("model-{round}")}]})))
                    .unwrap();
                call.await.unwrap()
            } else {
                control.request(req).await
            };
            assert!(result.ok, "{}: {:?}", command.name(), result.error);
            if matches!(command, Command::ShellRead(_)) {
                assert_eq!(
                    result.result.unwrap()["text"].as_str().unwrap().len(),
                    65536
                );
            }
        }
    }
    assert_eq!(owner.db.call(usage()).await.unwrap(), before);
    assert_eq!(
        owner
            .db
            .call(
                |db| Ok(db.query_row("SELECT COUNT(*) FROM runtime_pin", [], |r| r
                    .get::<_, i64>(0))?)
            )
            .await
            .unwrap(),
        0
    );
    assert!(
        rx.try_recv().is_err(),
        "index preview and no-space search must not fetch a model"
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_search_ipc_disconnect_closes_embedding_http_and_next_observation_succeeds() {
    use sha2::Digest;
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (entered, stalled) = oneshot::channel();
    let (closed, disconnected) = oneshot::channel();
    let http = tokio::spawn(async move {
        let mut entered = Some(entered);
        let mut closed = Some(closed);
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let split = loop {
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break request.len();
                }
            };
            let head = String::from_utf8_lossy(&request);
            assert!(head.starts_with("POST /v1/embeddings HTTP/1.1"));
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap();
            request.resize(split + length, 0);
            stream.read_exact(&mut request[split..]).await.unwrap();
            if index == 1 {
                entered.take().unwrap().send(()).unwrap();
                // No headers are sent. Socket close must come from IPC caller
                // cancellation, well before the configured 60-second timeout.
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                closed.take().unwrap().send(()).unwrap();
            } else {
                let body = json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}).to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        }
    });
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("[connections.embed]\nbase_url='{url}'\nrequest_timeout_ms=60000\n[embedding]\nconnection='embed'\nmodel='fixture-embedding'\n")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db| {
        let body = json!({"role":"user","content":[{"type":"input_text","text":"cat fixture"}]}).to_string();
        let hash = sha2::Sha256::digest(body.as_bytes()).iter().map(|byte|format!("{byte:02x}")).collect::<String>();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('ipc-search-source','user','2026-01-01T00:00:00Z',?1,?2,?3)",rusqlite::params![body,body.len() as i64,hash])?;
        Ok(())
    }).await.unwrap();
    control
        .index
        .enqueue_new(&["ipc-search-source".into()])
        .await
        .unwrap();
    for _ in 0..4 {
        if !control
            .index
            .process_one(CancellationToken::new())
            .await
            .unwrap()
        {
            break;
        }
    }
    assert_eq!(
        control
            .index
            .status(IndexArgs::default())
            .await
            .unwrap()
            .done,
        1
    );
    let (send, receive) = mpsc::channel(8);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let directory = owner.loaded.data_dir.clone();
    let query = || {
        Command::HistorySearch(HistorySearchArgs {
            query: "cat".into(),
            ..Default::default()
        })
    };
    let command = query();
    let observer =
        tokio::spawn(async move { shellboy::ipc::call(&directory, command, None, None).await });
    timeout(Duration::from_secs(5), stalled)
        .await
        .unwrap()
        .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(5), disconnected)
        .await
        .expect("IPC disconnect must close query HTTP before request_timeout_ms=60000")
        .unwrap();
    let next = timeout(
        Duration::from_secs(5),
        shellboy::ipc::call(&owner.loaded.data_dir, query(), None, None),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(next.ok, "{:?}", next.error);
    assert_eq!(next.result.unwrap().as_array().unwrap().len(), 1);
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM operation_receipt", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(5), http)
        .await
        .unwrap()
        .unwrap();
    local.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    serving.await.unwrap();
}
