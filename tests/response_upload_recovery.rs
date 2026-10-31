//! Real Driver recovery after repeated stalled request-body consumption.
//! Synthetic loopback traffic only; the server reads a prefix and never drains it.
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    control::{Control, EngineEvent},
    protocol::*,
};
use std::sync::Arc;
use tokio::{io::AsyncReadExt, net::TcpListener, sync::mpsc};
use tokio_util::sync::CancellationToken;

// Fixture bounds cover scheduling, persisted transitions and cleanup, not HTTP deadlines.
use crate::test_watchdog::WATCHDOG;

async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = crate::test_watchdog::watchdog(
        "tests/response_upload_recovery.rs: request",
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
async fn incomplete_uploads_without_trimming_use_the_configured_finite_budget() {
    upload_recovery(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_uploads_reduce_on_the_second_failure_and_keep_the_retry_budget() {
    upload_recovery(true).await;
}

async fn upload_recovery(auto_trim: bool) {
    // The product's 3s header deadline bounds HTTP setup/upload/header waiting.
    // Observing the next prefix also includes prior replay preparation and
    // (when enabled) reducing 16MiB of saved history. This watchdog bounds that
    // fixture work, not HTTP.
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
            "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\nresponse_header_timeout_ms=3000\nstream_idle_timeout_ms=10000\nresponse_max_retries=2\nresponse_retry_max_delay_ms=1\nauto_trim_on_disconnect={auto_trim}\n"
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
            agents: None,
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
                sender_label: None,
                now: false,
                thread_id: Some(thread.clone()),
                text: "small current request after a large saved history".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let turn = accepted.turn_id.unwrap();
    for ordinal in 1..=3 {
        match crate::test_watchdog::watchdog(
            "tests/response_upload_recovery.rs: upload_recovery",
            received.recv(),
        )
        .await
        {
            Ok(Some(())) => {}
            result => panic!(
                "request prefix {ordinal}/3 was not actually read (auto_trim={auto_trim}, result={result:?}); saved upload facts: {}",
                upload_facts(&control, &turn).await,
            ),
        }
    }
    let mut events = control.subscribe();
    let observed_turn = turn.clone();
    let recovery = crate::test_watchdog::watchdog(
        "tests/response_upload_recovery.rs: upload_recovery",
        async {
            loop {
                let turn = observed_turn.clone();
                let state: String = control
                    .store
                    .database
                    .call(move |db| {
                        Ok(
                            db.query_row("SELECT state FROM turn WHERE id=?1", [turn], |r| {
                                r.get(0)
                            })?,
                        )
                    })
                    .await
                    .unwrap();
                if state == "recovery_wait" {
                    break;
                }
                changed(&mut events, &thread).await;
            }
        },
    )
    .await;
    assert!(
        recovery.is_ok(),
        "finite upload retry budget must enter recovery (auto_trim={auto_trim}); saved upload facts: {}",
        upload_facts(&control, &turn).await,
    );
    control.store.database.call(move |db| {
        let attempts = db.prepare("SELECT state,error_json FROM model_attempt WHERE turn_id=?1 ORDER BY rowid")?
            .query_map([turn], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[0].0, "failed");
        assert_eq!(attempts[1].0, "failed");
        assert_eq!(attempts[2].0, "incomplete");
        for (index, (_, body)) in attempts.iter().enumerate() {
            let saved: Value = serde_json::from_str(body).unwrap();
            let observation = &saved["error"]["http_observation"];
            if index < 2 || !auto_trim {
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
            assert_eq!(saved.get("automatic_trim").is_some(), auto_trim && index == 1);
        }
        let first: Value = serde_json::from_str(&attempts[0].1).unwrap();
        let last: Value = serde_json::from_str(&attempts[2].1).unwrap();
        assert_eq!(first["automatic_retry"]["number"],1);
        assert_eq!(last["automatic_retry_exhausted"],json!({"retries":2,"limit":2}));
        assert!(last.get("automatic_retry").is_none());
        assert_eq!(db.query_row("SELECT count(*) FROM replacement", [], |r| r.get::<_, i64>(0))?, i64::from(auto_trim));
        assert!(db.query_row("SELECT length(CAST(payload_json AS BLOB)) FROM node WHERE id='large-upload-source'", [], |r| r.get::<_, i64>(0))? > 16 * 1024 * 1024);
        assert_eq!(db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path WHERE node_id='large-upload-source')", [], |r| r.get::<_, bool>(0))?, !auto_trim);
        if auto_trim {
            assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM replacement_source WHERE source_node_id='large-upload-source')", [], |r| r.get::<_, bool>(0))?);
        }
        Ok(())
    }).await.unwrap();
    crate::test_watchdog::watchdog(
        "tests/response_upload_recovery.rs: upload_recovery",
        control.shutdown(),
    )
    .await
    .unwrap()
    .unwrap();
    crate::test_watchdog::watchdog(
        "tests/response_upload_recovery.rs: upload_recovery",
        owner.shutdown(),
    )
    .await
    .unwrap()
    .unwrap();
    stop.cancel();
    crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: upload_recovery", server)
        .await
        .unwrap()
        .unwrap();
}

async fn upload_facts(control: &Control, turn: &str) -> Value {
    let turn = turn.to_owned();
    match crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: upload_facts", control.store.database.call(move |db| {
        let (state, reason): (String, Option<String>) = db.query_row(
            "SELECT state,recovery_reason_json FROM turn WHERE id=?1", [&turn],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let attempts = db.prepare("SELECT state,error_json FROM model_attempt WHERE turn_id=?1 ORDER BY rowid")?
            .query_map([turn], |row| Ok(json!({"state":row.get::<_,String>(0)?,"error":row.get::<_,Option<String>>(1)?})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"turn_state":state,"recovery_reason":reason,"attempts":attempts}))
    })).await {
        Ok(Ok(facts)) => facts,
        result => json!({"diagnostic_error":format!("{result:?}")}),
    }
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
    // Native refusal and subsequent admission are observed under the same fixture watchdog.
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
            agents: None,
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
                sender_label: None,
                now: false,
                thread_id: Some(thread.clone()),
                text: "recover after actual proxy startup".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let turn = accepted.turn_id.unwrap();
    let mut events = control.subscribe();
    let observed = crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens", async {
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
            changed(&mut events, &thread).await;
        }
    })
    .await;
    if observed.is_err() {
        let target = turn.clone();
        let progress = crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens", control.store.database.call(move |db| {
        let state: String = db.query_row("SELECT state FROM turn WHERE id=?1", [&target], |r| r.get(0))?;
        let attempts = db.prepare("SELECT json_object('state',state,'kind',json_extract(error_json,'$.error.kind'),'phase',json_extract(error_json,'$.error.http_observation.phase'),'source_bytes',json_extract(error_json,'$.error.http_observation.body_source_bytes_consumed'),'duration_ms',round((julianday(ended_at)-julianday(started_at))*86400000)) FROM model_attempt WHERE turn_id=?1 ORDER BY rowid LIMIT 9")?
            .query_map([target], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"turn_state":state,"attempts":attempts}))
    })).await;
        panic!(
            "three failed connections exceeded {}ms fixture watchdog; saved progress: {progress:?}",
            WATCHDOG.as_millis()
        );
    }
    let listener = TcpListener::bind(address).await.unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens", listener.accept()).await.unwrap().unwrap();
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
    crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens", async {
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
            changed(&mut events, &thread).await;
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
    crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens", server).await.unwrap().unwrap();
    crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens", control.shutdown())
        .await
        .unwrap()
        .unwrap();
    crate::test_watchdog::watchdog("tests/response_upload_recovery.rs: refused_proxy_connections_keep_retrying_beyond_two_and_recover_when_it_listens", owner.shutdown()).await.unwrap().unwrap();
}

// Subscribe before the saved-state read; lag means re-read, not lost completion.
async fn changed(events: &mut tokio::sync::broadcast::Receiver<EngineEvent>, thread: &str) {
    loop {
        match events.recv().await {
            Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => return,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                panic!("fixture event stream closed for Thread {thread}")
            }
            _ => {}
        }
    }
}
