//! Real Driver recovery after repeated stalled request-body consumption.
//! Synthetic loopback traffic only; the server reads a prefix and never drains it.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = timeout(
        Duration::from_secs(10),
        control.request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        )),
    )
    .await
    .expect("Control request did not finish");
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_incomplete_uploads_stop_after_two_attempts_without_trimming_history() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let server_stop = stop.clone();
    // Enough capacity for the two expected prefixes and one forbidden extra.
    let (prefixes, mut received) = mpsc::channel(3);
    let server = tokio::spawn(async move {
        let mut held = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = server_stop.cancelled() => break,
                accepted = listener.accept(), if held.len() < 3 => {
                    let (mut stream, _) = accepted.unwrap();
                    stream.set_nodelay(true).unwrap();
                    let prefixes = prefixes.clone();
                    let stop = server_stop.clone();
                    held.spawn(async move {
                        let mut prefix = [0u8; 1024];
                        tokio::select! {
                            biased;
                            _ = stop.cancelled() => return,
                            result = stream.read_exact(&mut prefix) => { result.unwrap(); }
                        }
                        assert!(prefix.starts_with(b"POST /v1/responses HTTP/1.1\r\n"));
                        prefixes.send(()).await.unwrap();
                        // Holding the socket without reading the remaining body
                        // creates real send-side backpressure, not a fake error.
                        stop.cancelled().await;
                        drop(stream);
                    });
                }
            }
        }
        while let Some(result) = held.join_next().await {
            result.unwrap();
        }
    });
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\nresponse_header_timeout_ms=3000\nstream_idle_timeout_ms=10000\nresponse_max_retries=8\nresponse_retry_max_delay_ms=1\nauto_trim_on_disconnect=true\n"
        ),
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
    let thread = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let target = thread.clone();
    control.store.database.call(move |db| {
        let tx = db.transaction()?;
        shellboy::history::append_node(&tx, &target, "large-upload-source", None, "user_message",
            &json!({"role":"user","content":[{"type":"input_text","text":format!("RETAIN_UPLOAD_SOURCE {}", "x".repeat(16 * 1024 * 1024))}]}), "2026-10-01T00:00:00Z")?;
        shellboy::history::append_node(&tx, &target, "old-answer", None, "assistant_response",
            &json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old complete answer"}]}]}), "2026-10-01T00:00:01Z")?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some(thread),
                text: "small current request after a large saved history".into(),
            }),
        )
        .await,
    )
    .unwrap();
    for _ in 0..2 {
        timeout(Duration::from_secs(15), received.recv())
            .await
            .expect("the server must actually read a request prefix before timeout")
            .expect("prefix server stopped");
    }
    let turn = accepted.turn_id.unwrap();
    let observed_turn = turn.clone();
    timeout(Duration::from_secs(15), async {
        loop {
            let turn = observed_turn.clone();
            let state: String = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row("SELECT state FROM turn WHERE id=?1", [turn], |r| r.get(0))?)
                })
                .await
                .unwrap();
            if state == "recovery_wait" {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second incomplete source must enter recovery");
    assert!(
        timeout(Duration::from_millis(300), received.recv())
            .await
            .is_err(),
        "a third request reached the server despite upload recovery"
    );
    control.store.database.call(move |db| {
        let attempts = db.prepare("SELECT state,error_json FROM model_attempt WHERE turn_id=?1 ORDER BY rowid")?
            .query_map([turn], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].0, "failed");
        assert_eq!(attempts[1].0, "incomplete");
        for (_, body) in &attempts {
            let saved: Value = serde_json::from_str(body).unwrap();
            let observation = &saved["error"]["http_observation"];
            assert_eq!(observation["phase"], "upload_source_incomplete");
            assert!(observation["request_body_bytes"].as_u64().unwrap() > 16 * 1024 * 1024);
            assert!(observation["body_source_bytes_consumed"].as_u64().unwrap()
                < observation["request_body_bytes"].as_u64().unwrap());
            assert!(saved.get("automatic_trim").is_none());
        }
        let first: Value = serde_json::from_str(&attempts[0].1).unwrap();
        let last: Value = serde_json::from_str(&attempts[1].1).unwrap();
        assert_eq!(first["automatic_retry"]["number"], 1);
        assert!(last.get("automatic_retry").is_none());
        assert!(last["error"]["message"].as_str().unwrap().contains("repeated incomplete request-body source"));
        assert_eq!(db.query_row("SELECT count(*) FROM replacement", [], |r| r.get::<_, i64>(0))?, 0);
        assert!(db.query_row("SELECT length(CAST(payload_json AS BLOB)) FROM node WHERE id='large-upload-source'", [], |r| r.get::<_, i64>(0))? > 16 * 1024 * 1024);
        assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path WHERE node_id='large-upload-source')", [], |r| r.get::<_, bool>(0))?);
        Ok(())
    }).await.unwrap();
    timeout(Duration::from_secs(5), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(5), owner.shutdown())
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
