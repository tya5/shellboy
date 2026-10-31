//! Real registry/transport adoption against temporary storage and loopback only.
use super::*;
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

fn take_signal(signal: &Notify) -> bool {
    let mut future = std::pin::pin!(signal.notified());
    matches!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(())
    )
}

async fn fixture(url: String) -> (tempfile::TempDir, DbWorker, Arc<IndexService>) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("shellboy.db"))
        .await
        .unwrap();
    let mut config = Config::default();
    config.connections.insert(
        "fixture".into(),
        crate::config::Connection {
            base_url: url,
            ..Default::default()
        },
    );
    config.embedding.connection = Some("fixture".into());
    config.embedding.model = Some("synthetic".into());
    config.indexing.auto_history = true;
    config.indexing.retries = 0;
    let service = IndexService::new(db.clone(), Arc::new(config));
    db.call(|db| {
        let body = json!({"role":"user","content":[{"type":"input_text","text":"synthetic source"}]}).to_string();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('source','user','2026-01-01T00:00:00Z',?1,?2,?3)", params![body,body.len() as i64,hash(body.as_bytes())])?;
        Ok(())
    }).await.unwrap();
    (root, db, service)
}

async fn proxy() -> (
    String,
    oneshot::Receiver<()>,
    oneshot::Sender<bool>,
    JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (entered, started) = oneshot::channel();
    let (release, outcome) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut part = [0; 4096];
            let n = socket.read(&mut part).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&part[..n]);
            assert!(bytes.len() <= 64 * 1024);
            if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let length = String::from_utf8_lossy(&bytes[..at])
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= at + 4 + length {
                    let request: Value =
                        serde_json::from_slice(&bytes[at + 4..at + 4 + length]).unwrap();
                    assert_eq!(request["input"].as_array().unwrap().len(), 1);
                    break;
                }
            }
        }
        entered.send(()).unwrap();
        let success = outcome.await.unwrap();
        let (status, body) = if success {
            (200, json!({"data":[{"index":0,"embedding":[1.0,2.0]}]}))
        } else {
            (
                401,
                json!({"error":{"code":"invalid_api_key","message":"synthetic authorization failure"}}),
            )
        };
        let body = body.to_string();
        socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    (url, started, release, task)
}

#[tokio::test]
async fn saved_signal_is_distinct_from_wake_and_observes_claim_then_finished_or_failed_without_ui()
{
    for success in [false, true] {
        let (url, started, release, server) = proxy().await;
        let (_root, db, service) = fixture(url).await;
        let args = IndexArgs::default();
        assert!(!take_signal(&service.saved_changed));
        service.status(args.clone()).await.unwrap();
        service.wake();
        assert!(take_signal(&service.notify));
        assert!(
            !take_signal(&service.saved_changed),
            "read/wake is not saved progress"
        );
        assert_eq!(
            service
                .enqueue_new(&["source".into()])
                .await
                .unwrap()
                .segments,
            1
        );
        assert!(take_signal(&service.saved_changed));
        let duplicate = service.enqueue_new(&["source".into()]).await.unwrap();
        assert_eq!(duplicate.segments, 0);
        assert!(
            !take_signal(&service.saved_changed),
            "INSERT OR IGNORE no-op is quiet"
        );
        let worker = service.clone();
        let processing =
            tokio::spawn(async move { worker.process_one(CancellationToken::new()).await });
        crate::test_watchdog::watchdog("committed index fixture", started)
            .await
            .unwrap()
            .unwrap();
        assert!(
            take_signal(&service.saved_changed),
            "claim is observable before HTTP completes"
        );
        assert_eq!(service.status(args.clone()).await.unwrap().running, 1);
        assert!(
            !take_signal(&service.saved_changed),
            "status is a simple saved read"
        );
        release.send(success).unwrap();
        assert!(
            crate::test_watchdog::watchdog("committed index fixture", processing)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        );
        crate::test_watchdog::watchdog("committed index fixture", server)
            .await
            .unwrap()
            .unwrap();
        let status = service.status(args).await.unwrap();
        assert_eq!(status.running, 0);
        assert_eq!(status.done, u64::from(success));
        assert_eq!(status.failed, u64::from(!success));
        assert_eq!(status.paused, !success);
        assert!(
            take_signal(&service.saved_changed),
            "adoption leaves one permit without a visible UI waiter"
        );
        assert!(
            !take_signal(&service.saved_changed),
            "coalesced, not an event queue"
        );
        assert!(!service.process_one(CancellationToken::new()).await.unwrap());
        assert!(
            !take_signal(&service.saved_changed),
            "idle/paused work is quiet"
        );
        if success {
            let space = service
                .status(IndexArgs::default())
                .await
                .unwrap()
                .space_id
                .unwrap();
            service.select_space(&space).await.unwrap();
            service.retry(IndexArgs::default()).await.unwrap();
            assert!(
                !take_signal(&service.saved_changed),
                "already-selected space and retry without failed work are no-ops"
            );
        }
        db.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn rejected_enqueue_rolls_back_without_signal_or_saved_job_and_retry_noop_is_quiet() {
    let (_root, db, service) = fixture("http://127.0.0.1:9/v1".into()).await;
    service.retry(IndexArgs::default()).await.unwrap();
    assert!(!take_signal(&service.saved_changed));
    db.call(|db| {
        db.execute_batch("CREATE TRIGGER reject_bootstrap BEFORE INSERT ON app_meta WHEN NEW.key LIKE 'index.bootstrap.%' AND json_extract(NEW.value,'$.segment.start')=1 BEGIN SELECT RAISE(ABORT,'synthetic enqueue rollback'); END;")?;
        Ok(())
    }).await.unwrap();
    let source = service.load_source("source").await.unwrap().unwrap();
    let body_kind = source_bodies(&source)[0].0.clone();
    let work = [0, 1]
        .into_iter()
        .map(|start| {
            Work::pending(
                source.witness.canonical(),
                body_kind.clone(),
                Segment {
                    start,
                    end: start + 1,
                    text: if start == 0 { "s" } else { "y" }.into(),
                },
                "active",
                "synthetic-rollback",
            )
        })
        .collect();
    let error = service
        .enqueue_batch(service.key().unwrap(), work)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SaveFailed);
    assert!(!take_signal(&service.saved_changed));
    db.call(|db| {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'index.bootstrap.%'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM embedding_job", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_partial_authorization_recovery_preserves_commit_signal_before_later_decode_error()
{
    let (_root, db, service) = fixture("http://127.0.0.1:9/v1".into()).await;
    db.call(|db| {
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('index.auto.a',?1)",
            [r#"{"node_id":"source","state":"running","error":null}"#],
        )?;
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('index.auto.b',?1)",
            [r#"{"state":"running"}"#],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(service.start().await.is_err());
    assert!(
        take_signal(&service.saved_changed),
        "earlier per-row autocommit survives later decode failure"
    );
    assert!(!take_signal(&service.saved_changed));
    db.call(|db| {
        let first: AutoAdmission = json_meta(db, "index.auto.a")?.unwrap();
        assert_eq!(first.state, "failed");
        let second: Value = json_meta(db, "index.auto.b")?.unwrap();
        assert_eq!(second["state"], "running");
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_control_index_retry_receipt_replay_does_not_clear_new_pause_or_notify_again() {
    use crate::{
        app::DataOwner,
        control::Control,
        protocol::{Command, Request},
    };
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n[embedding]\nconnection='fixture'\nmodel='synthetic'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    assert!(!take_signal(&control.index.saved_changed));
    let key = IndexKeys::new(control.index.profile_key.as_deref().unwrap()).pause();
    let seed_key = key.clone();
    owner
        .db
        .call(move |db| {
            db.execute(
                "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
                params![seed_key, json!({"kind":"synthetic_pause"}).to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let request = Request::new(
        format!("{}:index-notification-retry", owner.instance_id),
        Command::IndexRetry(IndexArgs::default()),
    );
    let first = control.request(request.clone()).await;
    assert!(first.ok, "{first:?}");
    assert!(take_signal(&control.index.saved_changed));
    assert!(!take_signal(&control.index.saved_changed));
    let seed_key = key.clone();
    owner
        .db
        .call(move |db| {
            db.execute(
                "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
                params![seed_key, json!({"kind":"later_pause"}).to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let replay = control.request(request).await;
    assert!(replay.ok, "{replay:?}");
    assert_eq!(replay.result, first.result);
    assert!(
        !take_signal(&control.index.saved_changed),
        "replay never invokes the mutator again"
    );
    owner
        .db
        .call(move |db| {
            assert_eq!(
                meta(db, &key)?,
                Some(json!({"kind":"later_pause"}).to_string())
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
