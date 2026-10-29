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
async fn repeated_incomplete_uploads_stop_after_two_attempts_when_trimming_is_disabled() {
    upload_recovery(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partial_large_upload_is_reduced_once_before_the_resumed_header_failure() {
    upload_recovery(true).await;
}

async fn upload_recovery(auto_trim: bool) {
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
            "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\nresponse_header_timeout_ms=3000\nstream_idle_timeout_ms=10000\nresponse_max_retries=8\nresponse_retry_max_delay_ms=1\nauto_trim_on_disconnect={auto_trim}\n"
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
        for (index, (_, body)) in attempts.iter().enumerate() {
            let saved: Value = serde_json::from_str(body).unwrap();
            let observation = &saved["error"]["http_observation"];
            if index == 0 || !auto_trim {
                assert_eq!(observation["phase"], "upload_source_incomplete");
                assert!(observation["request_body_bytes"].as_u64().unwrap() > 16 * 1024 * 1024);
                assert!(observation["body_source_bytes_consumed"].as_u64().unwrap() > 0);
                assert!(observation["body_source_bytes_consumed"].as_u64().unwrap()
                    < observation["request_body_bytes"].as_u64().unwrap());
            } else {
                assert_eq!(observation["phase"], "awaiting_headers_delivery_unknown");
                assert!(observation["request_body_bytes"].as_u64().unwrap() < 1024 * 1024,
                    "the resumed upload must actually be reduced");
                assert_eq!(observation["body_source_bytes_consumed"], observation["request_body_bytes"]);
            }
            assert_eq!(saved.get("automatic_trim").is_some(), auto_trim && index == 0);
        }
        let first: Value = serde_json::from_str(&attempts[0].1).unwrap();
        let last: Value = serde_json::from_str(&attempts[1].1).unwrap();
        if auto_trim {
            assert!(first.get("automatic_retry").is_none());
            assert!(first["automatic_trim"]["omitted_nodes"].as_u64().unwrap() > 0);
        } else {
            assert_eq!(first["automatic_retry"]["number"], 1);
            assert!(last["error"]["message"].as_str().unwrap().contains("repeated incomplete request-body source"));
        }
        assert!(last.get("automatic_retry").is_none());
        assert_eq!(db.query_row("SELECT count(*) FROM replacement", [], |r| r.get::<_, i64>(0))?, i64::from(auto_trim));
        assert!(db.query_row("SELECT length(CAST(payload_json AS BLOB)) FROM node WHERE id='large-upload-source'", [], |r| r.get::<_, i64>(0))? > 16 * 1024 * 1024);
        assert_eq!(db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path WHERE node_id='large-upload-source')", [], |r| r.get::<_, bool>(0))?, !auto_trim);
        if auto_trim {
            assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM replacement_source WHERE source_node_id='large-upload-source')", [], |r| r.get::<_, bool>(0))?);
        }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens() {
    use tokio::io::AsyncWriteExt;
    let root = tempfile::tempdir().unwrap();
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved); // A closed endpoint produces genuine connection-refused errors.
    const REQUIRED_FAILURES: u64 = 3;
    const HEADER_TIMEOUT_MS: u64 = 3_000;
    const RETRY_DELAY_MS: u64 = 100;
    // This observes three complete failed attempts, not a five-second SLA.
    // Each may consume its own header/connect budget, plus capped retry delay.
    // Keep a separate five-second fixture preparation allowance. In particular,
    // loopback refusal must not be assumed instantaneous on every native OS.
    let observation_budget =
        Duration::from_millis(5_000 + REQUIRED_FAILURES * (HEADER_TIMEOUT_MS + RETRY_DELAY_MS));
    // After listen starts, allow one already in-flight failed request and the
    // next successful request, plus capped retry and fixture preparation.
    let transition_budget = Duration::from_millis(5_000 + 2 * HEADER_TIMEOUT_MS + RETRY_DELAY_MS);
    let config = root.path().join("config.toml");
    std::fs::write(&config, format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\nresponse_max_retries=8\nresponse_retry_max_delay_ms={RETRY_DELAY_MS}\nresponse_header_timeout_ms={HEADER_TIMEOUT_MS}\n")).unwrap();
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
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some(thread),
                text: "recover after actual proxy startup".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let turn = accepted.turn_id.unwrap();
    let observed = timeout(observation_budget, async {
        loop {
            let turn = turn.clone();
            let (failed, state): (i64, String) = control
                .store
                .database
                .call(move |db| {
                    let state =
                        db.query_row("SELECT state FROM turn WHERE id=?1", [&turn], |r| r.get(0))?;
                    let failed = db.query_row(
                        "SELECT count(*) FROM model_attempt WHERE turn_id=?1 AND state='failed'",
                        [turn],
                        |r| r.get(0),
                    )?;
                    Ok((failed, state))
                })
                .await
                .unwrap();
            assert_ne!(
                state, "recovery_wait",
                "connect failures must not hit the partial-upload two-strike rule"
            );
            if failed >= REQUIRED_FAILURES as i64 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    if observed.is_err() {
        let target = turn.clone();
        let progress = timeout(Duration::from_secs(1), control.store.database.call(move |db| {
        let state: String = db.query_row("SELECT state FROM turn WHERE id=?1", [&target], |r| r.get(0))?;
        let attempts = db.prepare("SELECT json_object('state',state,'kind',json_extract(error_json,'$.error.kind'),'phase',json_extract(error_json,'$.error.http_observation.phase'),'source_bytes',json_extract(error_json,'$.error.http_observation.body_source_bytes_consumed'),'duration_ms',round((julianday(ended_at)-julianday(started_at))*86400000)) FROM model_attempt WHERE turn_id=?1 ORDER BY rowid LIMIT 9")?
            .query_map([target], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"turn_state":state,"attempts":attempts}))
    })).await;
        panic!(
            "three failed connections exceeded derived {}ms observation budget; saved progress: {progress:?}",
            observation_budget.as_millis()
        );
    }
    let listener = TcpListener::bind(address).await.unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = timeout(transition_budget, listener.accept())
            .await
            .unwrap()
            .unwrap();
        stream.set_nodelay(true).unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 8192];
            let count = stream.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let length = String::from_utf8_lossy(&bytes[..at])
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= at + 4 + length {
                    break;
                }
            }
        }
        let body = json!({"id":"proxy-restored","status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"connection restored"}]}]}).to_string();
        stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    timeout(transition_budget, async {
        loop {
            let target = turn.clone();
            let state: String = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row("SELECT state FROM turn WHERE id=?1", [target], |r| r.get(0))?)
                })
                .await
                .unwrap();
            assert_ne!(state, "recovery_wait");
            if state == "completed" {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Driver must automatically recover once the proxy listens");
    control
        .store
        .database
        .call(move |db| {
            let rows = db
                .prepare(
                    "SELECT state,error_json,round((julianday(ended_at)-julianday(started_at))*86400000) FROM model_attempt WHERE turn_id=?1 ORDER BY rowid",
                )?
                .query_map([turn], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<f64>>(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(rows.len() >= 4);
            assert_eq!(rows.last().unwrap().0, "completed");
            let mut observations = Vec::new();
            for (index, (state, body, wall_ms)) in rows[..rows.len() - 1].iter().enumerate() {
                assert_eq!(state, "failed");
                let saved: Value = serde_json::from_str(body.as_ref().unwrap()).unwrap();
                assert_eq!(
                    saved["error"]["http_observation"]["phase"],
                    "before_body_source"
                );
                assert_eq!(saved["automatic_retry"]["number"], index + 1);
                assert!(saved.get("automatic_trim").is_none());
                observations.push(json!({"kind":saved["error"]["kind"],"phase":saved["error"]["http_observation"]["phase"],"source_bytes":saved["error"]["http_observation"]["body_source_bytes_consumed"],"attempt_wall_ms":wall_ms}));
            }
            eprintln!("CLOSED_PROXY_OBSERVATIONS {}", json!(observations));
            assert_eq!(
                db.query_row("SELECT count(*) FROM replacement", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    server.await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
