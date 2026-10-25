//! Model catalog CLI/control boundaries against an isolated loopback HTTP server.
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
async fn failed_fetch_is_redacted_saved_and_replayed_without_another_http_request() {
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
    assert_eq!(control.request(req.clone()).await, failed);
    let mut conflict = req;
    conflict.command = list(Some("first"), None);
    assert_eq!(
        control.request(conflict).await.error.unwrap().code,
        ErrorCode::OperationConflict
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
    assert_eq!(control.request(req).await, cancelled);
    assert!(rx.try_recv().is_err());
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
async fn owner_shutdown_cancels_pending_http_and_finalizes_receipt() {
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
                    "SELECT state FROM operation_receipt WHERE operation_id=?1",
                    [operation],
                    |r| r.get::<_, String>(0)
                )?,
                "failed"
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
                json!({"protocol_version":1,"operation_id":"op","command":"model.list","args":args})
            )
            .is_err()
        );
    }
    let req = Request::from_value(
        json!({"protocol_version":1,"operation_id":"op","command":"model.list","args":{}}),
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
async fn concurrent_identical_calls_and_later_replay_share_one_saved_catalog() {
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
    got.reply
        .send((200, json!({"data":[{"id":"only-model"}]})))
        .unwrap();
    let first = first.await.unwrap();
    assert!(first.ok);
    assert_eq!(second.await.unwrap(), first);
    assert_eq!(control.request(req).await, first);
    assert!(rx.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
