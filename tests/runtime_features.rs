//! Cross-service operation boundaries using a real owner and loopback proxy.
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    attachments::AttachmentInput,
    control::{Control, EngineEvent},
    protocol::*,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};
fn working_instructions(body: &Value) -> Option<(String, &str)> {
    let messages: Vec<_> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "developer")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .filter(|text| text.starts_with("Working instructions from "))
        .collect();
    assert!(
        messages.len() <= 1,
        "instructions must have one developer message"
    );
    messages.first().map(|text| {
        let (header, original) = text.split_once('\n').expect("one header LF before original body");
        assert!(!header.chars().any(|ch| ch.is_control() || matches!(ch,'\u{2028}'|'\u{2029}')));
        let quoted = header.strip_prefix("Working instructions from ").unwrap()
            .strip_suffix(" (user-editable; re-read at each new Turn start; same-Turn retries use the saved snapshot):").unwrap();
        (serde_json::from_str(quoted).expect("header path is a JSON string"), original)
    })
}
fn assert_working_instructions(body: &Value, path: &std::path::Path, original: &str) {
    let (source, actual) =
        working_instructions(body).expect("present instruction developer message");
    assert_eq!(source, path.to_str().unwrap());
    assert_eq!(
        actual.as_bytes(),
        original.as_bytes(),
        "original body must remain exact"
    );
}

struct Incoming {
    body: Value,
    session: Option<String>,
    reply: oneshot::Sender<(u16, Value)>,
}
async fn proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    proxy_with_sse(false).await
}
async fn proxy_with_sse(
    sse: bool,
) -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut chunk = [0; 8192];
                let (body, session) = loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..i]);
                        let length = head
                            .lines()
                            .find_map(|s| {
                                s.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap();
                        if bytes.len() >= i + 4 + length {
                            let session = head.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("x-shellboy-session-id")
                                    .then(|| value.trim().to_owned())
                            });
                            break (
                                serde_json::from_slice(&bytes[i + 4..i + 4 + length]).unwrap(),
                                session,
                            );
                        }
                    }
                };
                let (reply, result) = oneshot::channel();
                if send
                    .send(Incoming {
                        body,
                        session,
                        reply,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                let Ok((status, body)) = result.await else {
                    return;
                };
                let (content_type, body) = if sse && status == 200 {
                    (
                        "text/event-stream",
                        format!(
                            "data: {}\n\n",
                            json!({"type":match body["status"].as_str() {
                                Some("incomplete") => "response.incomplete",
                                Some("failed") => "response.failed",
                                _ => "response.completed",
                            },"response":body})
                        ),
                    )
                } else {
                    ("application/json", body.to_string())
                };
                let _=stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
        }
    });
    (format!("http://{address}/v1"), receive, task)
}
async fn owner(root: &std::path::Path, url: &str) -> DataOwner {
    owner_with_retries(root, url, 0).await
}
async fn owner_with_retries(root: &std::path::Path, url: &str, retries: u32) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms=10\n")).unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
fn answer(text: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]})
}
// Fixture guards detect deadlocks, not product latency. Capture only bounded
// saved lifecycle facts on failure; request bodies and error messages stay out.
async fn progress(control: &Control) -> Result<Value, shellboy::storage::StorageError> {
    control.store.database.call(|db| {
        let mut state = serde_json::Map::new();
        for (name, query) in [
            ("runs", "SELECT json_object('id',substr(id,1,128),'thread_id',substr(thread_id,1,128),'state',substr(state,1,64),'stop_reason',substr(stop_reason,1,128)) FROM run ORDER BY rowid DESC LIMIT 8"),
            ("turns", "SELECT json_object('id',substr(t.id,1,128),'thread_id',substr(t.thread_id,1,128),'run_id',substr(t.run_id,1,128),'state',substr(t.state,1,64),'generation',t.generation,'instruction_snapshot_present',t.instruction_snapshot_id IS NOT NULL,'run_state',substr(r.state,1,64),'recovery_code',substr(json_extract(t.recovery_reason_json,'$.code'),1,128),'attempt_state',substr(a.state,1,64),'error_kind',substr(json_extract(a.error_json,'$.error.kind'),1,128),'http_phase',substr(json_extract(a.error_json,'$.error.http_observation.phase'),1,128)) FROM turn t JOIN run r ON r.id=t.run_id LEFT JOIN model_attempt a ON a.id=(SELECT latest.id FROM model_attempt latest WHERE latest.turn_id=t.id ORDER BY latest.rowid DESC LIMIT 1) ORDER BY t.rowid DESC LIMIT 8"),
        ] {
            let rows = db.prepare(query)?.query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            state.insert(name.into(), json!(rows));
        }
        Ok(Value::Object(state))
    }).await
}
#[track_caller]
fn incoming<'a>(
    control: &'a Control,
    receive: &'a mut mpsc::Receiver<Incoming>,
) -> impl std::future::Future<Output = Incoming> + 'a {
    let caller = std::panic::Location::caller();
    async move {
        match crate::test_watchdog::watchdog("tests/runtime_features.rs: incoming", receive.recv())
            .await
        {
            Ok(Some(value)) => value,
            result => {
                let diagnostic = crate::test_watchdog::watchdog(
                    "tests/runtime_features.rs: incoming",
                    progress(control),
                )
                .await;
                panic!(
                    "loopback request watchdog at {}:{}; receive={}; saved lifecycle facts={diagnostic:?}",
                    caller.file(),
                    caller.line(),
                    if result.is_err() {
                        "timeout"
                    } else {
                        "sender closed"
                    }
                );
            }
        }
    }
}
#[track_caller]
fn settled<'a>(
    control: &'a Control,
    thread: &'a str,
) -> impl std::future::Future<Output = ()> + 'a {
    let caller = std::panic::Location::caller();
    let mut events = control.subscribe();
    async move {
        let result = crate::test_watchdog::watchdog("tests/runtime_features.rs: settled", async {
            loop {
                let target = thread.to_owned();
                if control.store.database.call(move |db| Ok(db.query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting'))",
                    [target], |row| row.get::<_, bool>(0),
                )?)).await.unwrap() {
                    break;
                }
                loop {
                    match events.recv().await {
                        Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!(
                            "settlement event stream closed at {}:{} for Thread {thread}",
                            caller.file(), caller.line()
                        ),
                        _ => {}
                    }
                }
            }
        }).await;
        if result.is_err() {
            let diagnostic = crate::test_watchdog::watchdog(
                "tests/runtime_features.rs: settled",
                progress(control),
            )
            .await;
            panic!(
                "Thread {thread} did not settle within the fixture watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                caller.file(),
                caller.line()
            );
        }
    }
}
async fn thread(control: &Arc<Control>) -> String {
    request(
        control,
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
        .into()
}
async fn send(control: &Arc<Control>, thread: &str, text: &str) {
    request(
        control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.into()),
            text: text.into(),
        }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preparation_failure_preserves_accepted_message_and_retry_reuses_its_turn() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    std::fs::write(root.path().join("AGENTS.md"), [0xff, 0xfe]).unwrap();
    let thread = thread(&control).await;
    let original = "受付済み本文\nretry exactly this message";
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: original.into(),
            }),
        )
        .await,
    )
    .unwrap();
    settled(&control, &thread).await;
    assert!(receive.try_recv().is_err(), "preparation must precede HTTP");
    let turn = accepted.turn_id.clone().unwrap();
    let run = accepted.run_id.clone().unwrap();
    let check_turn = turn.clone();
    let check_run = run.clone();
    let check_message = accepted.message_id.clone();
    control
        .store
        .database
        .call(move |db| {
            let (state, reason, generation): (String, String, i64) = db.query_row(
                "SELECT state,recovery_reason_json,generation FROM turn WHERE id=?1",
                [&check_turn],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(state, "recovery_wait");
            let reason: ApiError = serde_json::from_str(&reason).unwrap();
            assert_eq!(reason.code, ErrorCode::IoFailed);
            assert_eq!(generation, 0);
            let (saved_run, saved_turn, body, receipt_state, snapshot): (
                String, String, String, String, Option<String>,
            ) = db.query_row(
                "SELECT r.run_id,r.turn_id,n.payload_json,r.state,t.instruction_snapshot_id FROM receipt r JOIN node n ON n.id=r.message_node_id JOIN turn t ON t.id=r.turn_id WHERE r.message_node_id=?1",
                [check_message],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )?;
            assert_eq!(saved_run, check_run);
            assert_eq!(saved_turn, check_turn);
            assert_eq!(receipt_state, "accepted");
            assert!(snapshot.is_none(), "the failed read created no snapshot");
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["content"][0]["text"], original);
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();

    std::fs::write(root.path().join("AGENTS.md"), "valid repaired instructions").unwrap();
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    let retry = incoming(&control, &mut receive).await;
    let messages: Vec<&Value> = retry.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user" && item["content"][0]["text"] == original)
        .collect();
    assert_eq!(messages.len(), 1, "retry must reuse the accepted original");
    assert_working_instructions(
        &retry.body,
        &root.path().join("AGENTS.md"),
        "valid repaired instructions",
    );
    retry.reply.send((200, answer("recovered"))).unwrap();
    settled(&control, &thread).await;
    control
        .store
        .database
        .call(move |db| {
            let (message, saved_run, saved_turn, state): (String, String, String, String) = db
                .query_row(
                    "SELECT message_node_id,run_id,turn_id,state FROM receipt",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
            assert_eq!(message, accepted.message_id);
            assert_eq!(saved_run, run);
            assert_eq!(saved_turn, turn);
            assert_eq!(state, "applied");
            let (state, generation): (String, i64) = db.query_row(
                "SELECT state,generation FROM turn WHERE id=?1",
                [saved_turn],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(state, "completed");
            assert_eq!(generation, 1);
            for table in ["receipt", "run", "turn", "model_attempt"] {
                let count: i64 =
                    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 1, "no duplicate {table} after retry");
            }
            let body: String = db.query_row(
                "SELECT payload_json FROM node WHERE id=?1",
                [message],
                |row| row.get(0),
            )?;
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["content"][0]["text"], original);
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_references_and_first_message_commit_together_and_changed_missing_files_never_reuse_stale_data()
 {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let file = root.path().join("sample.txt");
    std::fs::write(&file, b"first-content").unwrap();
    let accepted = control
        .send_draft(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_id: None,
                connection: None,
                model: None,
                effort: None,
            },
            None,
            "read the attachment".into(),
            vec![AttachmentInput {
                path: file.clone(),
                kind: AttachmentKind::File,
            }],
        )
        .await
        .unwrap();
    let thread = accepted["thread_id"].as_str().unwrap().to_owned();
    let first = incoming(&control, &mut receive).await;
    let text = first.body.to_string();
    assert!(text.contains("input_file"));
    assert!(text.contains("Zmlyc3QtY29udGVudA=="));
    first.reply.send((200, answer("first read"))).unwrap();
    settled(&control, &thread).await;
    let provenance=control.store.database.call(|db|Ok(db.query_row("SELECT count(*) FROM attachment_ref a JOIN receipt r ON r.message_node_id=a.source_node_id",[],|r|r.get::<_,i64>(0))?)).await.unwrap();
    assert_eq!(provenance, 1);
    std::fs::write(&file, b"second-content-longer").unwrap();
    send(&control, &thread, "read again").await;
    let second = incoming(&control, &mut receive).await;
    let text = second.body.to_string();
    assert!(text.contains("c2Vjb25kLWNvbnRlbnQtbG9uZ2Vy"));
    assert!(!text.contains("Zmlyc3QtY29udGVudA=="));
    second.reply.send((200, answer("changed"))).unwrap();
    settled(&control, &thread).await;
    std::fs::remove_file(&file).unwrap();
    send(&control, &thread, "missing now").await;
    let third = incoming(&control, &mut receive).await;
    let text = third.body.to_string();
    assert!(!text.contains("input_file"));
    assert!(!text.contains("c2Vjb25kLWNvbnRlbnQtbG9uZ2Vy"));
    third.reply.send((200, answer("missing reported"))).unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_accepts_during_http_wait_and_adopts_without_starting_an_extra_turn() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    for n in 0..2 {
        send(&control, &thread, &format!("task {n}")).await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer("saved answer")))
            .unwrap();
        settled(&control, &thread).await;
    }
    send(&control, &thread, "current task").await;
    let ordinary = incoming(&control, &mut receive).await;
    assert_eq!(ordinary.session.as_deref(), Some(thread.as_str()));
    assert!(ordinary.body.get("prompt_cache_key").is_none());
    let accepted = crate::test_watchdog::watchdog("tests/runtime_features.rs: manual_compact_accepts_during_http_wait_and_adopts_without_starting_an_extra_turn",
        request(
            &control,
            Command::Compact(ThreadTarget {
                thread_id: Some(thread.clone()),
            }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(accepted["status"], "queued");
    assert!(receive.try_recv().is_err());
    ordinary.reply.send((200, answer("last answer"))).unwrap();
    let summary = incoming(&control, &mut receive).await;
    assert_eq!(summary.session.as_deref(), Some(thread.as_str()));
    assert!(summary.body.get("prompt_cache_key").is_none());
    assert_eq!(summary.body["tools"], json!([]));
    assert_eq!(summary.body["store"], false);
    summary
        .reply
        .send((200, answer("## 目的\nSummarized earlier turns")))
        .unwrap();
    crate::test_watchdog::watchdog("tests/runtime_features.rs: manual_compact_accepts_during_http_wait_and_adopts_without_starting_an_extra_turn", async {
        loop {
            let value = request(
                &control,
                Command::OperationGet(OperationTarget {
                    operation_id: accepted["operation_id"].as_str().unwrap().into(),
                }),
            )
            .await;
            if value["downstream"]["outcome"]["result"]["summary_node_id"].is_string() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let counts=control.store.database.call(|db|Ok((db.query_row("SELECT count(*) FROM turn",[],|r|r.get::<_,i64>(0))?,db.query_row("SELECT count(*) FROM node WHERE kind='summary'",[],|r|r.get::<_,i64>(0))?,db.query_row("SELECT count(*) FROM node WHERE kind='user_message' AND payload_json IS NOT NULL",[],|r|r.get::<_,i64>(0))?))).await.unwrap();
    assert_eq!(counts.0, 3);
    assert_eq!(counts.1, 1);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overflow_without_maximum_target_summarizes_without_unchanged_resend() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner_with_retries(root.path(), &url, 8).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "protected overflowing task").await;
    incoming(&control,&mut receive).await.reply.send((200,json!({"id":"tool","status":"completed","output":[{
        "type":"function_call","call_id":"protected-tool","name":"shell","arguments":json!({"command":format!("echo {}","x".repeat(2000))}).to_string()
    }]}))).unwrap();
    let pending = incoming(&control, &mut receive).await;
    let target = thread.clone();
    control.store.database.call(move |db| {
        let tx=db.transaction()?;
        let request:String=tx.query_row("SELECT message_node_id FROM receipt WHERE target_thread_id=?1 ORDER BY created_at LIMIT 1",[&target],|r|r.get(0))?;
        let selected=tx.prepare("SELECT n.id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND (n.id=?2 OR json_extract(n.payload_json,'$.group_id') IS NOT NULL) ORDER BY p.ordinal")?.query_map(rusqlite::params![target,request],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(selected.len(),3,"only the request and latest complete pair remain");
        let revision:i64=tx.query_row("SELECT selection_revision FROM thread WHERE id=?1",[&target],|r|r.get(0))?;
        shellboy::history::select_path(&tx,&target,revision as u64,&selected,"2026-10-30T00:00:00Z")?;
        tx.commit()?; Ok(())
    }).await.unwrap();
    let overflow =
        json!({"error":{"code":"context_length_exceeded","message":"fixture context exceeded"}});
    pending.reply.send((400, overflow.clone())).unwrap();
    let summary = incoming(&control, &mut receive).await;
    assert_eq!(
        summary.body["tools"],
        json!([]),
        "no unchanged conversation resend before the final stage"
    );
    summary.reply.send((200, answer("OK"))).unwrap();
    let retry = incoming(&control, &mut receive).await;
    assert_eq!(retry.body["tools"].as_array().unwrap().len(), 1);
    retry.reply.send((400, overflow)).unwrap();
    settled(&control, &thread).await;
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "recovery_wait"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                3
            );
            let saved: String = db.query_row(
                "SELECT error_json FROM model_attempt WHERE state='failed'",
                [],
                |r| r.get(0),
            )?;
            let saved: Value = serde_json::from_str(&saved).unwrap();
            assert_eq!(
                saved["automatic_trim_attempt"]["maximum_outcome"],
                "no_safe_reduction"
            );
            assert_eq!(saved["automatic_retry"]["number"], 1);
            Ok(())
        })
        .await
        .unwrap();
    assert!(receive.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_second_tool_prevents_first_side_effect_and_every_call_receives_a_result() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let marker = root.path().join("must-not-exist");
    send(&control, &thread, "bad group").await;
    let first = incoming(&control, &mut receive).await;
    let command = if cfg!(windows) {
        format!(
            "Set-Content -LiteralPath '{}' -Value unexpected",
            marker.display()
        )
    } else {
        format!("touch '{}'", marker.display())
    };
    first.reply.send((200,json!({"id":"badgroup","status":"completed","output":[{"type":"function_call","call_id":"good","name":"shell","arguments":json!({"command":command}).to_string()},{"type":"function_call","call_id":"bad","name":"shell","arguments":"broken-json"}]}))).unwrap();
    let second = incoming(&control, &mut receive).await;
    let outputs: Vec<Value> = second.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["type"] == "function_call_output")
        .map(|v| serde_json::from_str(v["output"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0]["status"], "cancelled");
    assert_eq!(outputs[1]["status"], "error");
    assert!(!marker.exists());
    second.reply.send((200, answer("corrected"))).unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_overflow_retries_preserve_call_groups_and_never_execute_saved_tools_twice() {
    async fn next(
        receive: &mut mpsc::Receiver<Incoming>,
        control: &Control,
        phase: &str,
    ) -> Incoming {
        // This boundary includes a real PowerShell launch under concurrent owner tests.
        // Match the native Windows shell fixture's bounded startup/output allowance.
        let _seconds = if cfg!(windows) { 15 } else { 5 };
        match crate::test_watchdog::watchdog("tests/runtime_features.rs: next", receive.recv())
            .await
        {
            Ok(Some(request)) => request,
            failed => {
                let diagnostic = crate::test_watchdog::watchdog("tests/runtime_features.rs: next", control.store.database.call(|db| {
                    let mut observations = Vec::new();
                    for (label, sql) in [
                        ("turn", "SELECT json_object('state',state,'reason',recovery_reason_json) FROM turn LIMIT 8"),
                        ("attempt", "SELECT json_object('state',state,'error',error_json) FROM model_attempt ORDER BY started_at LIMIT 16"),
                        ("tool", "SELECT json_object('call',call_id,'state',state,'arguments',arguments_json) FROM tool_call LIMIT 8"),
                        ("shell", "SELECT json_object('state',state,'exit_code',exit_code,'reason',end_reason,'sequence',saved_sequence,'complete',output_complete) FROM shell_execution LIMIT 8"),
                        ("raw", "SELECT json_object('chunks',count(*),'bytes',coalesce(sum(length(bytes)),0)) FROM output_chunk"),
                    ] {
                        let mut statement = db.prepare(sql)?;
                        let rows = statement.query_map([], |row| row.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()?;
                        observations.push((label, rows));
                    }
                    Ok(observations)
                })).await;
                panic!(
                    "no model request at {phase}; receive_closed={}; saved observations={diagnostic:?}",
                    matches!(failed, Ok(None))
                );
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner_with_retries(root.path(), &url, 8).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "three tool groups then overflow").await;
    for ordinal in 0..3 {
        let request = next(
            &mut receive,
            &control,
            &format!("before tool group {ordinal}"),
        )
        .await;
        request.reply.send((200,json!({"id":format!("group-{ordinal}"),"status":"completed","output":[{"type":"function_call","call_id":format!("call-{ordinal}"),"name":"shell","arguments":json!({"command":format!("echo raw-group-{ordinal}{}", "x".repeat(2000))}).to_string()}]}))).unwrap();
    }
    let overflow = json!({"error":{"code":"context_length_exceeded","message":"fixture pressure"}});
    next(&mut receive, &control, "after all three tool groups")
        .await
        .reply
        .send((400, overflow.clone()))
        .unwrap();
    let retry_one = next(&mut receive, &control, "first overflow: spill retry").await;
    let context = current_context(&control, &thread).await;
    assert_current_context_once(&retry_one.body, &context, "after automatic spill");
    let outputs: Vec<Value> = retry_one.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["type"] == "function_call_output")
        .map(|v| serde_json::from_str(v["output"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(outputs.len(), 1);
    assert!(
        retry_one.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "function_call_output" && v["call_id"] == "call-2")
    );
    assert!(
        outputs[0]["output"]
            .as_str()
            .unwrap()
            .contains("raw-group-2")
    );
    retry_one.reply.send((400, overflow.clone())).unwrap();
    let summary = next(&mut receive, &control, "second overflow: summary request").await;
    assert_eq!(summary.body["tools"], json!([]));
    summary.reply.send((200, answer("OK"))).unwrap();
    let retry_two = next(
        &mut receive,
        &control,
        "second overflow: adopted summary retry",
    )
    .await;
    assert_current_context_once(&retry_two.body, &context, "after automatic summary");
    let outputs: Vec<Value> = retry_two.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["type"] == "function_call_output")
        .map(|v| serde_json::from_str(v["output"].as_str().unwrap()).unwrap())
        .collect();
    assert!(outputs.len() <= 1);
    for output in &outputs {
        assert_eq!(output["call_id"], "call-2");
    }
    retry_two.reply.send((400, overflow)).unwrap();
    settled(&control, &thread).await;
    assert!(receive.try_recv().is_err());
    let run_id = control
        .store
        .database
        .call(|db| {
            Ok(
                db.query_row("SELECT id FROM run WHERE state='live'", [], |r| {
                    r.get::<_, String>(0)
                })?,
            )
        })
        .await
        .unwrap();
    let turn_context = request(&control, Command::RunGet(RunTarget { run_id })).await;
    assert_eq!(
        turn_context["context"]["recovery_reason"]["details"]["automatic_reduction_unavailable"],
        true
    );
    let counts = control
        .store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("SELECT count(*) FROM tool_result", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row(
                    "SELECT count(*) FROM replacement WHERE format_version='spill-v1'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((counts.0, counts.1), (3, 3));
    assert!(counts.2 <= 3);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_recovery_cancels_an_inflight_manual_summary_without_adopting_it() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    for n in 0..2 {
        send(&control, &thread, &format!("previous {n}")).await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer("saved")))
            .unwrap();
        settled(&control, &thread).await;
    }
    send(&control, &thread, "current recovery").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((
            500,
            json!({"error":{"code":"fixture_unknown","message":"explicit recovery"}}),
        ))
        .unwrap();
    settled(&control, &thread).await;
    let compact = request(
        &control,
        Command::Compact(ThreadTarget {
            thread_id: Some(thread.clone()),
        }),
    )
    .await;
    let summary = incoming(&control, &mut receive).await;
    assert_eq!(summary.body["tools"], json!([]));
    let details = request(
        &control,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread),
        }),
    )
    .await;
    request(
        &control,
        Command::RunStop(RunTarget {
            run_id: details["run_id"].as_str().unwrap().into(),
        }),
    )
    .await;
    let _ = summary
        .reply
        .send((200, answer("late summary must not be selected")));
    crate::test_watchdog::watchdog("tests/runtime_features.rs: stopping_recovery_cancels_an_inflight_manual_summary_without_adopting_it", async {
        loop {
            let status = request(
                &control,
                Command::OperationGet(OperationTarget {
                    operation_id: compact["operation_id"].as_str().unwrap().into(),
                }),
            )
            .await;
            if status["downstream"]["state"] == "failed" {
                assert_eq!(status["state"], "completed");
                assert_eq!(status["outcome"]["result"], compact);
                assert_eq!(status["replay_policy"], "return_saved_outcome");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let count = control
        .store
        .database
        .call(|db| {
            Ok(
                db.query_row("SELECT count(*) FROM node WHERE kind='summary'", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editing_first_request_forks_an_empty_prefix_and_accepts_once_with_new_settings() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let accepted = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "old original request".into(),
        }),
    )
    .await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer("old original answer")))
        .unwrap();
    settled(&control, &thread).await;
    crate::test_watchdog::watchdog("tests/runtime_features.rs: editing_first_request_forks_an_empty_prefix_and_accepts_once_with_new_settings",async{loop{let target=thread.clone();if control.store.database.call(move|db|Ok(db.query_row("SELECT NOT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state IN ('live','stopping'))",[target],|r|r.get::<_,bool>(0))?)).await.unwrap(){break;}tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
    let operation = format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4());
    let node = accepted["message_id"].as_str().unwrap().to_owned();
    let args = ThreadCreateArgs {
        agents: None,
        connection: Some("fixture".into()),
        model: Some("edited-model".into()),
        effort: Some(String::new()),
        cwd: Some(root.path().to_str().unwrap().into()),
        parent_id: None,
    };
    let edited = control
        .edit_request_with_settings(
            operation.clone(),
            thread.clone(),
            node.clone(),
            None,
            "edited replacement request".into(),
            vec![],
            args.clone(),
        )
        .await
        .unwrap();
    let next = incoming(&control, &mut receive).await;
    assert_eq!(next.body["model"], "edited-model");
    assert!(next.body.get("reasoning").is_none());
    assert!(!next.body.to_string().contains("old original request"));
    assert!(next.body.to_string().contains("edited replacement request"));
    next.reply.send((200, answer("edited answer"))).unwrap();
    let new_thread = edited["thread_id"].as_str().unwrap().to_owned();
    assert_ne!(new_thread, thread);
    settled(&control, &new_thread).await;
    let repeated = control
        .edit_request_with_settings(
            operation,
            thread.clone(),
            node.clone(),
            None,
            "edited replacement request".into(),
            vec![],
            args,
        )
        .await
        .unwrap();
    assert_eq!(repeated["message_id"], edited["message_id"]);
    let count = control
        .store
        .database
        .call(move |db| {
            Ok((
                db.query_row("SELECT count(*) FROM thread", [], |r| r.get::<_, i64>(0))?,
                db.query_row(
                    "SELECT count(*) FROM thread_path WHERE thread_id=?1 AND node_id=?2",
                    rusqlite::params![thread, node],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM thread_path WHERE thread_id=?1 AND node_id=?2",
                    rusqlite::params![new_thread, node],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(count, (2, 1, 0));
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edited_request_requires_exact_run_consent_and_waits_for_old_run_to_stop() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let accepted = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "running original".into(),
        }),
    )
    .await;
    let old = incoming(&control, &mut receive).await;
    let node = accepted["message_id"].as_str().unwrap().to_owned();
    let run = accepted["run_id"].as_str().unwrap().to_owned();
    for consent in [None, Some("stale-other-run".into())] {
        let error = control
            .edit_request(
                format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                thread.clone(),
                node.clone(),
                consent,
                "replacement".into(),
                vec![],
            )
            .await
            .unwrap_err();
        assert_eq!(error.details.unwrap()["run_id"], run);
    }
    let edited = control
        .edit_request(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            thread,
            node,
            Some(run.clone()),
            "replacement".into(),
            vec![],
        )
        .await
        .unwrap();
    let state = control
        .store
        .database
        .call(move |db| {
            Ok(
                db.query_row("SELECT state FROM run WHERE id=?1", [run], |r| {
                    r.get::<_, String>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(state, "stopped");
    let _ = old.reply.send((200, answer("too late")));
    let new = incoming(&control, &mut receive).await;
    new.reply.send((200, answer("new run answer"))).unwrap();
    settled(&control, edited["thread_id"].as_str().unwrap()).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_observations_wait_for_a_request_boundary_and_survive_failed_http() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "first").await;
    let first = incoming(&control, &mut receive).await;
    let text = first.body.to_string();
    for kind in [
        "run_started",
        "turn_started",
        "instructions_observed",
        "timezone",
    ] {
        assert!(text.contains(kind), "missing {kind}: {text}");
    }
    first
        .reply
        .send((500, json!({"error":{"message":"fixture failure"}})))
        .unwrap();
    settled(&control, &thread).await;
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            agents: None,
            title: None,
            thread_id: Some(thread.clone()),
            cwd: None,
            connection: None,
            model: Some("changed-model".into()),
            effort: Some("low".into()),
        }),
    )
    .await;
    assert!(
        receive.try_recv().is_err(),
        "settings must not resume recovery_wait"
    );
    let target = thread.clone();
    let (run,pending):(String,i64) = control.store.database.call(move|db|Ok((db.query_row("SELECT run_id FROM turn WHERE thread_id=?1 AND state='recovery_wait'",[&target],|r|r.get(0))?,db.query_row("SELECT count(*) FROM delivery d JOIN event e ON e.id=d.event_id WHERE e.target_thread_id=?1 AND e.kind='context_changed' AND d.state='pending'",[target],|r|r.get(0))?))).await.unwrap();
    assert!(pending >= 2);
    request(&control, Command::RunRetry(RunTarget { run_id: run })).await;
    let retry = incoming(&control, &mut receive).await;
    let text = retry.body.to_string();
    for kind in [
        "thread_settings",
        "recovery_resumed",
        "run_started",
        "changed-model",
    ] {
        assert!(text.contains(kind), "missing {kind}");
    }
    assert_eq!(retry.body["model"], "changed-model");
    retry.reply.send((200, answer("done"))).unwrap();
    settled(&control, &thread).await;
    let target = thread.clone();
    let count:i64=control.store.database.call(move|db|Ok(db.query_row("SELECT count(*) FROM event e WHERE e.target_thread_id=?1 AND e.kind='context_changed' AND NOT EXISTS(SELECT 1 FROM delivery d WHERE d.event_id=e.id AND d.state='processed')",[target],|r|r.get(0))?)).await.unwrap();
    assert_eq!(count, 0);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_a_b_a_never_revives_prior_reasoning_but_preserves_canonical_dag() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let old_a = json!({"id":"reasoning-old-A","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":"旧Aの推論"}],"encrypted_content":"b2xkLUE="});
    let from_b = json!({"id":"reasoning-B","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":"Bの推論"}],"encrypted_content":"bW9kZWwtQg=="});
    let new_a = json!({"id":"reasoning-new-A","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":"戻ったAの新しい推論"}],"encrypted_content":"bmV3LUE="});
    let cases = [
        ("model-A", vec![], Some(old_a.clone()), "ANSWER_OLD_A"),
        ("model-A", vec![old_a.clone()], None, "ANSWER_SAME_A"),
        ("model-B", vec![], Some(from_b.clone()), "ANSWER_B"),
        ("model-B", vec![from_b.clone()], None, "ANSWER_SAME_B"),
        ("model-A", vec![], Some(new_a.clone()), "ANSWER_NEW_A"),
        ("model-A", vec![new_a.clone()], None, "ANSWER_FINAL_A"),
    ];
    let mut previous_model = "fixture";
    let mut originals = Vec::new();
    for (step, (model, expected_reasoning, reasoning, text)) in cases.into_iter().enumerate() {
        if previous_model != model {
            request(
                &control,
                Command::ThreadSet(ThreadSetArgs {
                    agents: None,
                    title: None,
                    thread_id: Some(thread.clone()),
                    connection: None,
                    model: Some(model.into()),
                    effort: None,
                    cwd: None,
                }),
            )
            .await;
            previous_model = model;
        }
        send(&control, &thread, &format!("instruction {step}")).await;
        let call = incoming(&control, &mut receive).await;
        assert_eq!(call.body["model"], model);
        assert_eq!(call.body["store"], false);
        let actual_reasoning = call.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            actual_reasoning, expected_reasoning,
            "reasoning epoch at request {step}"
        );
        let wire = call.body.to_string();
        if step >= 2 {
            assert!(!wire.contains("b2xkLUE="));
            assert!(!wire.contains("旧Aの推論"));
        }
        if step >= 4 {
            assert!(!wire.contains("bW9kZWwtQg=="));
            assert!(!wire.contains("Bの推論"));
        }
        if step > 0 {
            assert!(
                call.body.to_string().contains("ANSWER_OLD_A"),
                "switching model must retain ordinary answer history"
            );
        }
        if step > 2 {
            assert!(call.body.to_string().contains("ANSWER_B"));
        }
        let mut reply = answer(text);
        if let Some(item) = &reasoning {
            reply["output"]
                .as_array_mut()
                .unwrap()
                .insert(0, item.clone());
        }
        call.reply.send((200, reply)).unwrap();
        settled(&control, &thread).await;
        if let Some(item) = reasoning {
            let target = thread.clone();
            let item_id = item["id"].as_str().unwrap().to_owned();
            let original=control.store.database.call(move|db|Ok(db.query_row("SELECT n.id,n.payload_json FROM node n JOIN thread_path p ON p.node_id=n.id WHERE p.thread_id=?1 AND n.kind='assistant_response' AND json_extract(n.payload_json,'$.output[0].id')=?2",rusqlite::params![target,item_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?)).await.unwrap();
            originals.push((original.0, original.1, item));
        }
    }
    let target = thread.clone();
    control
        .store
        .database
        .call(move |db| {
            let mut epochs = std::collections::BTreeSet::new();
            for (node, body, item) in originals {
                let saved: String =
                    db.query_row("SELECT payload_json FROM node WHERE id=?1", [&node], |r| {
                        r.get(0)
                    })?;
                assert_eq!(
                    saved, body,
                    "canonical reasoning node changed after model switches"
                );
                let value: Value = serde_json::from_str(&saved).unwrap();
                assert_eq!(value["output"][0], item);
                epochs.insert(value["reasoning_epoch"].as_i64().unwrap());
                let selected: i64 = db.query_row(
                    "SELECT count(*) FROM thread_path WHERE thread_id=?1 AND node_id=?2",
                    rusqlite::params![target, node],
                    |r| r.get(0),
                )?;
                assert_eq!(
                    selected, 1,
                    "reasoning original remains on the selected DAG path"
                );
                let reaches_cursor:bool=db.query_row("WITH RECURSIVE descendants(id) AS (SELECT ?1 UNION SELECT e.dst FROM edge e JOIN descendants d ON e.src=d.id) SELECT EXISTS(SELECT 1 FROM descendants WHERE id=(SELECT cursor_node_id FROM thread WHERE id=?2))",rusqlite::params![node,target],|r|r.get(0))?;
                assert!(reaches_cursor,"retained reasoning node remains an ancestor of the current DAG cursor");
            }
            assert_eq!(epochs.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
            let attempts = db
                .prepare("SELECT model_id,reasoning_epoch,state FROM model_attempt ORDER BY rowid")?
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(
                attempts,
                vec![
                    ("model-A".to_owned(), 1, "completed".to_owned()),
                    ("model-A".to_owned(), 1, "completed".to_owned()),
                    ("model-B".to_owned(), 2, "completed".to_owned()),
                    ("model-B".to_owned(), 2, "completed".to_owned()),
                    ("model-A".to_owned(), 3, "completed".to_owned()),
                    ("model-A".to_owned(), 3, "completed".to_owned())
                ]
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reopened_provider_history_replays_without_item_ids_after_explicit_http_recovery() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let original_owner = owner(root.path(), &url).await;
    let original_control = Control::new(&original_owner, root.path().into());
    let thread = thread(&original_control).await;
    let reasoning = json!({"type":"reasoning","id":"rs-retained","status":"completed","summary":[],"encrypted_content":"opaque-same-epoch"});
    let message = json!({"type":"message","id":"chatcmpl-proxy-answer","role":"assistant","phase":"final_answer","status":"completed","content":[{"type":"output_text","text":"previous provider answer"}],"provider_extension":{"id":"nested-id-kept","value":7}});
    let call = json!({"type":"function_call","id":"chatcmpl-proxy-call","call_id":"call-kept-exactly","name":"shell","arguments":"{\"command\":\"printf historical\"}","phase":"commentary","status":"completed","provider_extension":{"id":"nested-call-id-kept"}});
    let result = json!({"type":"function_call_output","call_id":"call-kept-exactly","output":"historical result"});
    let target = thread.clone();
    let saved_reasoning = reasoning.clone();
    let saved_message = message.clone();
    let saved_call = call.clone();
    let saved_result = result.clone();
    let original_bodies = original_control.store.database.call(move |db| {
        let epoch: i64 = db.query_row("SELECT reasoning_epoch FROM thread WHERE id=?1", [&target], |r| r.get(0))?;
        let tx = db.transaction()?;
        shellboy::history::append_node(&tx, &target, "legacy-provider-response", None, "assistant_response", &json!({"reasoning_epoch":epoch,"output":[saved_reasoning,saved_message,saved_call]}), "2026-10-01T00:00:00Z")?;
        shellboy::history::append_node(&tx, &target, "legacy-provider-result", None, "tool_result", &json!({"item":saved_result}), "2026-10-01T00:00:01Z")?;
        let bodies = ["legacy-provider-response", "legacy-provider-result"].into_iter().map(|id| {
            tx.query_row("SELECT payload_json FROM node WHERE id=?1", [id], |r| r.get::<_, String>(0))
        }).collect::<rusqlite::Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(bodies)
    }).await.unwrap();
    original_control.shutdown().await.unwrap();
    drop(original_control);
    original_owner.shutdown().await.unwrap();

    // Reopen existing persisted history before starting the recoverable Run.
    // Graceful owner shutdown intentionally terminates live Runs; it is not a
    // supported way to retain an in-flight RunRetry target across a restart.
    let reopened = DataOwner::start(
        Some(root.path().join("config.toml")),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&reopened, root.path().into());
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: "continue existing provider history".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let mut expected_message = message;
    expected_message.as_object_mut().unwrap().remove("id");
    let mut expected_call = call;
    expected_call.as_object_mut().unwrap().remove("id");
    let check_input = |body: &Value| {
        let items = body["input"].as_array().unwrap();
        assert!(
            items.contains(&reasoning),
            "same-epoch reasoning must remain exact"
        );
        assert!(
            items.contains(&expected_message),
            "message phase/status/unknown fields must survive"
        );
        assert!(
            items.contains(&expected_call),
            "function call linkage and unknown fields must survive"
        );
        assert!(
            items.contains(&result),
            "completed tool result must remain linked"
        );
        assert!(!items.iter().any(|item| matches!(
            item["type"].as_str(),
            Some("message" | "function_call")
        ) && item.get("id").is_some()));
    };
    let first = incoming(&control, &mut receive).await;
    assert_eq!(first.session.as_deref(), Some(thread.as_str()));
    check_input(&first.body);
    first
        .reply
        .send((
            400,
            json!({"error":{"code":"fixture_recovery","message":"explicit retry required"}}),
        ))
        .unwrap();
    settled(&control, &thread).await;
    let turn = accepted.turn_id.unwrap();
    let target = turn.clone();
    control
        .store
        .database
        .call(move |db| {
            let state: String =
                db.query_row("SELECT state FROM turn WHERE id=?1", [target], |r| r.get(0))?;
            assert_eq!(state, "recovery_wait");
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        receive.try_recv().is_err(),
        "HTTP400 must not automatically retry"
    );
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: accepted.run_id.unwrap(),
        }),
    )
    .await;
    let retry = incoming(&control, &mut receive).await;
    assert_eq!(retry.session.as_deref(), Some(thread.as_str()));
    check_input(&retry.body);
    // Model a Responses backend that rejects provider-specific output item IDs.
    let rejects_ids = retry.body["input"].as_array().unwrap().iter().any(|item| {
        matches!(item["type"].as_str(), Some("message" | "function_call"))
            && item.get("id").is_some()
    });
    retry.reply.send(if rejects_ids {
        (400, json!({"error":{"code":"invalid_value","message":"item.id must have a Responses-compatible prefix"}}))
    } else {
        (200, answer("existing history and explicit recovery succeeded"))
    }).unwrap();
    settled(&control, &thread).await;
    control
        .store
        .database
        .call(move |db| {
            let state: String =
                db.query_row("SELECT state FROM turn WHERE id=?1", [turn], |r| r.get(0))?;
            assert_eq!(state, "completed");
            for (id, original) in ["legacy-provider-response", "legacy-provider-result"]
                .into_iter()
                .zip(original_bodies)
            {
                let saved: String =
                    db.query_row("SELECT payload_json FROM node WHERE id=?1", [id], |r| {
                        r.get(0)
                    })?;
                assert_eq!(
                    saved, original,
                    "outbound normalization must not mutate persisted JSON"
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    reopened.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quota_recovery_wait_collects_via_control_and_requires_explicit_same_message_retry() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url={url:?}\n[limits]\ndata_warning_bytes=524288\ndata_max_bytes=1048576\n")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let target = thread.clone();
    let instance = control.store.instance_id.clone();
    // A live reader pin protects the expired fixture until explicit collection.
    control.store.database.call(move|db|{
        db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('quota-old-run',?1,'completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",[&target])?;
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('quota-old-turn',?1,'quota-old-run','completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",[target])?;
        db.execute_batch("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('quota-raw','quota-old-run','quota-old-turn','quota-old-operation','exited','/','{}','UTF-8','UTF-8','closed',32,0,1,'2020-01-01T00:00:00Z','2020-01-01T00:00:01Z');")?;
        for sequence in 0..32_i64 { db.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('quota-raw',?1,'stdout',?2,zeroblob(65536),'2020-01-01T00:00:01Z')",rusqlite::params![sequence,sequence*65536])?; }
        db.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES('quota-pin',?1,'shell_output','quota-raw',0,?2)",rusqlite::params![instance,chrono::Utc::now().to_rfc3339()])?;
        Ok(())
    }).await.unwrap();
    let protected = request(
        &control,
        Command::StorageCollect(StorageArgs {
            category: Some(StorageCategory::Raw),
        }),
    )
    .await;
    assert_eq!(protected["raw_bytes_collected"], 0);
    assert_eq!(protected["protected_raw_bytes"], 2097152);
    let original = "Quota-held instruction 日本語\nretry the original once";
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: original.into(),
            }),
        )
        .await,
    )
    .unwrap();
    settled(&control, &thread).await;
    assert!(
        receive.try_recv().is_err(),
        "quota must stop the first HTTP attempt"
    );
    let turn = accepted.turn_id.clone().unwrap();
    let run = accepted.run_id.clone().unwrap();
    let target = turn.clone();
    let check_message = accepted.message_id.clone();
    let check_run = run.clone();
    control
        .store
        .database
        .call(move |db| {
            let (saved_run,saved_turn,body,state):(String,String,String,String)=db.query_row("SELECT r.run_id,r.turn_id,n.payload_json,r.state FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.message_node_id=?1",[&check_message],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            assert_eq!(saved_run,check_run);assert_eq!(saved_turn,target);assert_eq!(state,"accepted");assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["content"][0]["text"],original);
            let (state, reason, generation): (String, String, i64) = db.query_row(
                "SELECT state,recovery_reason_json,generation FROM turn WHERE id=?1",
                [&target],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(state, "recovery_wait");
            assert_eq!(generation, 0);
            let error: ApiError = serde_json::from_str(&reason).unwrap();
            assert_eq!(error.code, ErrorCode::StorageLimit);
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM model_attempt WHERE turn_id=?1",
                    [target],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            db.execute("DELETE FROM runtime_pin WHERE id='quota-pin'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let collected = request(
        &control,
        Command::StorageCollect(StorageArgs {
            category: Some(StorageCategory::Raw),
        }),
    )
    .await;
    // The capacity refusal marked every storage kind (#171 backstop), so the
    // maintenance requester may reclaim the released output before this
    // explicit collect does; either way none of it remains (status below).
    let reclaimed = collected["raw_bytes_collected"].as_u64().unwrap();
    assert!(reclaimed == 2097152 || reclaimed == 0, "{collected}");
    let status = request(
        &control,
        Command::StorageStatus(StorageArgs { category: None }),
    )
    .await;
    assert_eq!(status["limit_reached"], false);
    assert_eq!(status["logical_raw_bytes"], 0);
    assert!(
        receive.try_recv().is_err(),
        "capacity reclamation must not automatically resume a model request"
    );
    let target = turn.clone();
    control
        .store
        .database
        .call(move |db| {
            let row: (String, i64) = db.query_row(
                "SELECT state,generation FROM turn WHERE id=?1",
                [&target],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(row, ("recovery_wait".to_owned(), 0));
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM model_attempt WHERE turn_id=?1",
                    [target],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    let resumed = incoming(&control, &mut receive).await;
    let matching = resumed.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user" && item["content"][0]["text"] == original)
        .count();
    assert_eq!(matching, 1);
    resumed
        .reply
        .send((200, answer("quota retry completed")))
        .unwrap();
    settled(&control, &thread).await;
    control.store.database.call(move|db|{
        let (message,saved_run,saved_turn,state):(String,String,String,String)=db.query_row("SELECT message_node_id,run_id,turn_id,state FROM receipt WHERE message_node_id=?1",[&accepted.message_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!(message,accepted.message_id);assert_eq!(saved_run,run);assert_eq!(saved_turn,turn);assert_eq!(state,"applied");
        let (state,generation):(String,i64)=db.query_row("SELECT state,generation FROM turn WHERE id=?1",[&turn],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(state,"completed");assert_eq!(generation,1);
        let payload:String=db.query_row("SELECT payload_json FROM node WHERE id=?1",[message],|r|r.get(0))?;assert_eq!(serde_json::from_str::<Value>(&payload).unwrap()["content"][0]["text"],original);
        assert_eq!(db.query_row("SELECT count(*) FROM receipt",[],|r|r.get::<_,i64>(0))?,1);assert_eq!(db.query_row("SELECT count(*) FROM model_attempt WHERE turn_id=?1",[&turn],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT count(*) FROM turn WHERE run_id=?1",[run],|r|r.get::<_,i64>(0))?,1);Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_created_agents_waits_for_next_turn_and_survives_a_new_startup_cwd() {
    let root = tempfile::tempdir().unwrap();
    assert!(!root.path().join("AGENTS.md").exists());
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let target = thread.clone();
    let original_paths: (String, String) = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT cwd,agents_path FROM thread WHERE id=?1",
                [target],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    let instruction = "A01_CREATED_AGENTS 日本語\nUse the saved Thread cwd.\n";
    // Inspect only developer instruction text: the tool arguments intentionally
    // carry the same bytes later and are not evidence of instruction loading.
    let instruction_path = original_paths.1.clone();
    let count_instruction = move |body: &Value, expected: &str| {
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["role"] == "developer")
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter(|part| {
                part["type"] == "input_text"
                    && part["text"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("Working instructions from "))
            })
            .filter(|_| {
                let (path, original) = working_instructions(body).unwrap();
                assert_eq!(path, instruction_path);
                original.as_bytes() == expected.as_bytes()
            })
            .count()
    };
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: "create the per-thread instructions through shell".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let first = incoming(&control, &mut receive).await;
    assert_eq!(count_instruction(&first.body, instruction), 0);
    let first_turn = accepted.turn_id.unwrap();
    let query = first_turn.clone();
    let absent_snapshot: String=control.store.database.call(move|db|{
        let (id,size,body):(String,Option<i64>,String)=db.query_row("SELECT s.id,s.observed_size,s.body FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[query],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert!(size.is_none());assert!(body.is_empty());Ok(id)
    }).await.unwrap();
    let command = format!("printf '%s' '{instruction}' > AGENTS.md");
    first.reply.send((200,json!({"id":"create-agents-response","status":"completed","output":[{"id":"create-agents-item","type":"function_call","call_id":"create-agents-call","name":"shell","arguments":json!({"command":command,"background":false}).to_string()}]}))).unwrap();
    let after_tool = incoming(&control, &mut receive).await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
        instruction
    );
    assert_eq!(
        count_instruction(&after_tool.body, instruction),
        0,
        "the absent snapshot remains frozen within the original Turn"
    );
    assert!(
        after_tool.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call"
                && item["arguments"]
                    .as_str()
                    .is_some_and(|text| text.contains("A01_CREATED_AGENTS"))),
        "the command is present as tool history, not developer instructions"
    );
    after_tool
        .reply
        .send((
            200,
            answer("created instructions; current Turn still uses its absent snapshot"),
        ))
        .unwrap();
    settled(&control, &thread).await;
    let query = first_turn.clone();
    let saved_absent = absent_snapshot.clone();
    control.store.database.call(move|db|{
        let (snapshot,attempts):(String,i64)=db.query_row("SELECT instruction_snapshot_id,(SELECT count(*) FROM model_attempt WHERE turn_id=t.id) FROM turn t WHERE id=?1",[query],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(snapshot,saved_absent);assert_eq!(attempts,2);Ok(())
    }).await.unwrap();
    let next: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: "observe the new instructions at the next Turn".into(),
            }),
        )
        .await,
    )
    .unwrap();
    assert_ne!(next.turn_id.as_deref(), Some(first_turn.as_str()));
    let next_request = incoming(&control, &mut receive).await;
    assert_eq!(count_instruction(&next_request.body, instruction), 1);
    next_request
        .reply
        .send((200, answer("next Turn observed the new file")))
        .unwrap();
    settled(&control, &thread).await;
    let query = next.turn_id.unwrap();
    control.store.database.call(move|db|{
        let (snapshot,size,body):(String,Option<i64>,String)=db.query_row("SELECT s.id,s.observed_size,s.body FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[query],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_ne!(snapshot,absent_snapshot);assert_eq!(size,Some(instruction.len()as i64));assert_eq!(body,instruction);Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();

    let alternate = root.path().join("alternate-startup");
    std::fs::create_dir(&alternate).unwrap();
    let unrelated = "A01_OTHER_STARTUP_MUST_NOT_REPLACE_THREAD_INSTRUCTIONS";
    std::fs::write(alternate.join("AGENTS.md"), unrelated).unwrap();
    let reopened = DataOwner::start(
        Some(root.path().join("config.toml")),
        Some(root.path().join("data")),
        alternate.clone(),
    )
    .await
    .unwrap();
    let control = Control::new(&reopened, alternate);
    let details = request(
        &control,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.clone()),
        }),
    )
    .await;
    assert_eq!(details["cwd"], original_paths.0);
    let target = thread.clone();
    let actual_paths = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT cwd,agents_path FROM thread WHERE id=?1",
                [target],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(actual_paths, original_paths);
    send(
        &control,
        &thread,
        "verify the persisted cwd after owner restart",
    )
    .await;
    let after_reopen = incoming(&control, &mut receive).await;
    assert_eq!(count_instruction(&after_reopen.body, instruction), 1);
    assert_eq!(count_instruction(&after_reopen.body, unrelated), 0);
    after_reopen
        .reply
        .send((200, answer("persisted cwd and agents path preserved")))
        .unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    drop(control);
    reopened.shutdown().await.unwrap();
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_quit_prevents_tools_from_a_late_model_response_without_cancelling_capture_first()
{
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let (actions, _pending_actions) = shellboy::ui::actions::channel(1);
    actions.close_tool_starts_on_quit(&control);
    send(&control, &thread, "response will arrive after Quit").await;
    let pending = incoming(&control, &mut receive).await;
    let waiting = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            thread_id: Some(thread.clone()),
            text: "Do not fire after Quit admission".into(),
            deliver: Deliver::Complete,
        }),
    )
    .await["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    actions.try_send(shellboy::ui::UiAction::Quit).unwrap();
    let marker = root.path().join("forbidden-after-quit");
    let command = format!("printf forbidden > '{}'", marker.display());
    pending.reply.send((200,json!({"id":"late-tools","status":"completed","output":[{"type":"function_call","name":"shell","call_id":"late-call","arguments":json!({"command":command}).to_string()}]}))).unwrap();
    let next = incoming(&control, &mut receive).await;
    assert!(next.body["input"].as_array().unwrap().iter().any(|item| {
        item["type"] == "function_call_output"
            && item["output"]
                .as_str()
                .is_some_and(|body| body.contains("cancelled"))
    }));
    next.reply
        .send((200, answer("cancelled by owner exit")))
        .unwrap();
    settled(&control, &thread).await;
    assert!(!marker.exists());
    let target = waiting.clone();
    assert_eq!(
        control
            .store
            .database
            .call(move |db| Ok(db.query_row(
                "SELECT state FROM receipt WHERE message_node_id=?1",
                [target],
                |r| r.get::<_, String>(0)
            )?))
            .await
            .unwrap(),
        "held",
        "Quit admission must not deliver a waiting Revisit message"
    );
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM tool_call WHERE state='completed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM turn WHERE state='recovery_wait'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_during_the_first_tool_cancels_the_next_call_without_restarting_the_existing_process()
{
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let (actions, _pending_actions) = shellboy::ui::actions::channel(1);
    actions.close_tool_starts_on_quit(&control);
    send(&control, &thread, "two tool calls, exit during the first").await;
    let pending = incoming(&control, &mut receive).await;
    let marker = root.path().join("forbidden-second-tool");
    let second = format!("printf forbidden > '{}'", marker.display());
    pending.reply.send((200,json!({"id":"two-tools","status":"completed","output":[
        {"type":"function_call","name":"shell","call_id":"first","arguments":json!({"command":"printf FIRST; read ignored","stdin_open":true}).to_string()},
        {"type":"function_call","name":"shell","call_id":"second","arguments":json!({"command":second}).to_string()}
    ]}))).unwrap();
    // Synchronize on persisted output from the first actual OS process, then
    // keep it blocked on stdin until the Quit acceptance has closed the gate.
    let execution = crate::test_watchdog::watchdog("tests/runtime_features.rs: quit_during_the_first_tool_cancels_the_next_call_without_restarting_the_existing_process", async {
        loop {
            let found = owner.db.call(|db| {
                use rusqlite::OptionalExtension;
                Ok(db.query_row("SELECT s.id FROM shell_execution s WHERE json_extract(s.shell_json,'$.spawned')=1 AND EXISTS(SELECT 1 FROM output_chunk o WHERE o.execution_id=s.id)",[],|r|r.get::<_,String>(0)).optional()?)
            }).await.unwrap();
            if let Some(execution) = found { break execution; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    actions.try_send(shellboy::ui::UiAction::Quit).unwrap();
    request(
        &control,
        Command::ShellWrite(ShellWriteArgs {
            execution_id: execution,
            text: "continue\n".into(),
            encoding: "UTF-8".into(),
        }),
    )
    .await;
    let next = incoming(&control, &mut receive).await;
    let outputs = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert!(outputs.iter().any(|item| {
        item["call_id"] == "second"
            && item["output"]
                .as_str()
                .is_some_and(|body| body.contains("cancelled"))
    }));
    next.reply
        .send((200, answer("second call cancelled")))
        .unwrap();
    settled(&control, &thread).await;
    assert!(!marker.exists());
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM shell_execution WHERE state='exited' AND exit_code=0",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_session_header_uses_stable_thread_uuid_and_distinct_child_and_fork_ids() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let parent = thread(&control).await;
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(parent.clone()),
                text: "synthetic session parent".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let first = incoming(&control, &mut receive).await;
    assert_eq!(first.session.as_deref(), Some(parent.as_str()));
    assert!(first.body.get("prompt_cache_key").is_none());
    first.reply.send((200, answer("parent response"))).unwrap();
    settled(&control, &parent).await;
    let child = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            parent_id: Some(parent.clone()),
            cwd: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fork = request(
        &control,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: parent.clone(),
            node_id: accepted.message_id,
            cwd: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(parent, child);
    assert_ne!(parent, fork);
    assert_ne!(child, fork);
    for target in [&parent, &child, &fork] {
        uuid::Uuid::parse_str(target).expect("session source must be the real Thread UUID");
        send(&control, target, "synthetic session followup").await;
        let actual = incoming(&control, &mut receive).await;
        assert_eq!(actual.session.as_deref(), Some(target.as_str()));
        assert!(actual.body.get("prompt_cache_key").is_none());
        actual
            .reply
            .send((200, answer("distinct session response")))
            .unwrap();
        settled(&control, target).await;
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_unicode_request_input_characters_are_saved_before_http_and_retained_with_usage() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let target = thread(&control).await;
    send(&control, &target, "入力文字数の記録 🦀 \"quoted\"\n次の行").await;
    let sent = incoming(&control, &mut receive).await;
    let serialized_input = sent.body["input"].to_string();
    let characters = serialized_input.chars().count() as u64;
    assert!(
        serialized_input.len() as u64 > characters,
        "fixture must distinguish UTF-8 bytes"
    );
    let replay_thread = target.clone();
    let history_characters =
        control
            .store
            .database
            .call(move |db| {
                let (state, manifest): (String, String) = db.query_row(
            "SELECT state,input_manifest_json FROM model_attempt ORDER BY rowid DESC LIMIT 1", [],
            |row| Ok((row.get(0)?, row.get(1)?)))?;
                assert_eq!(
                    state, "inflight",
                    "measurement must be durable before HTTP completes"
                );
                let manifest: Value = serde_json::from_str(&manifest).unwrap();
                assert_eq!(manifest["input_characters"], characters);
                let replay = shellboy::control::replay::ReplayCache::new(1_000_000)
                    .load(db, &replay_thread)?;
                let serialized_history = json!(replay.materialize()).to_string();
                let history_characters = serialized_history.chars().count() as u64;
                assert_eq!(manifest["history_characters"], history_characters);
                assert!(history_characters <= characters);
                assert!(
                    serialized_history.len() as u64 > history_characters,
                    "history fixture also distinguishes Unicode scalar count from bytes"
                );
                Ok(history_characters)
            })
            .await
            .unwrap();
    let mut completed = answer("calibrated response");
    completed["usage"] = json!({"input_tokens":123,"input_tokens_details":{"cached_tokens":100,"extra":"x".repeat(128*1024)},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":127,"attribution":{"items":["x".repeat(128*1024)]}});
    sent.reply.send((200, completed)).unwrap();
    settled(&control, &target).await;
    control.store.database.call(move |db| {
        let (manifest, usage): (String, String) = db.query_row(
            "SELECT input_manifest_json,usage_json FROM model_attempt WHERE state='completed' ORDER BY rowid DESC LIMIT 1", [],
            |row| Ok((row.get(0)?, row.get(1)?)))?;
        let manifest: Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["input_characters"], characters);
        assert_eq!(manifest["history_characters"], history_characters);
        assert_eq!(serde_json::from_str::<Value>(&usage).unwrap(), json!({"input_tokens":123,"input_tokens_details":{"cached_tokens":100},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":127}));
        assert!(usage.len()<256);
        Ok(())
    }).await.unwrap();
    let details = request(
        &control,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(target.clone()),
        }),
    )
    .await;
    assert_eq!(
        details["context_usage"]["last_successful_attempt"]["input_tokens"],
        123
    );
    assert_eq!(
        details["context_usage"]["last_successful_attempt"]["cached_tokens"],
        100
    );
    assert!(
        details["context_usage"]
            .get("estimated_input_tokens")
            .is_none()
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn explicit_agents_changes_wait_for_new_turn_and_retry_keeps_snapshot() {
    #[derive(Debug, PartialEq, Eq)]
    struct SavedWork {
        runs: Vec<(String, String)>,
        turns: Vec<(String, String, String)>,
        attempts: Vec<(String, String, String)>,
    }
    async fn saved_work(control: &Control, thread: &str) -> SavedWork {
        let thread = thread.to_owned();
        control.store.database.call(move |db| {
            let runs = db.prepare("SELECT id,state FROM run WHERE thread_id=?1 ORDER BY id")?
                .query_map([&thread], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            let turns = db.prepare("SELECT id,run_id,state FROM turn WHERE thread_id=?1 ORDER BY id")?
                .query_map([&thread], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            let attempts = db.prepare("SELECT a.id,a.turn_id,a.state FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 ORDER BY a.id")?
                .query_map([&thread], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            Ok(SavedWork { runs, turns, attempts })
        }).await.unwrap()
    }
    fn developer_text(body: &Value) -> Vec<&str> {
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["role"] == "developer")
            .flat_map(|v| v["content"].as_array().into_iter().flatten())
            .filter_map(|v| v["text"].as_str())
            .collect()
    }
    fn reference_events(body: &Value) -> Vec<Value> {
        developer_text(body)
            .into_iter()
            .filter_map(|t| t.strip_prefix("Shellboy event v1: "))
            .filter_map(|t| serde_json::from_str::<Value>(t).ok())
            .filter(|e| e["payload"]["kind"] == "agents_reference")
            .collect()
    }
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    std::fs::write(root.path().join("old.md"), "OLD_EXPLICIT_ROLE").unwrap();
    std::fs::write(root.path().join("new.md"), "NEW_EXPLICIT_ROLE").unwrap();
    std::fs::write(root.path().join("middle.md"), "INTERMEDIATE_ROLE").unwrap();
    let created = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: Some("old.md".into()),
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await;
    let thread = created["thread_id"].as_str().unwrap().to_owned();
    request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "first turn".into(),
        }),
    )
    .await;
    let first = incoming(&control, &mut receive).await;
    assert_working_instructions(
        &first.body,
        &root.path().join("old.md"),
        "OLD_EXPLICIT_ROLE",
    );
    let first_events = reference_events(&first.body);
    assert_eq!(
        first_events.len(),
        1,
        "creation override is visible on the first Turn"
    );
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            agents: Some("middle.md".into()),
            ..Default::default()
        }),
    )
    .await;
    // Other settings still deliver at their normal boundary, not at a future Turn.
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            title: Some("changed".into()),
            effort: Some("brief".into()),
            ..Default::default()
        }),
    )
    .await;
    first.reply.send((200,json!({"id":"agents-tool","status":"completed","output":[{"type":"function_call","call_id":"agents-call","name":"shell","arguments":json!({"command":"echo synthetic","background":false}).to_string()}]}))).unwrap();
    let after_tool = incoming(&control, &mut receive).await;
    assert!(
        developer_text(&after_tool.body)
            .iter()
            .any(|t| t.starts_with("Shellboy event v1:")
                && t.contains("thread_settings")
                && t.contains("brief")),
        "unrelated settings must not defer"
    );
    assert_working_instructions(
        &after_tool.body,
        &root.path().join("old.md"),
        "OLD_EXPLICIT_ROLE",
    );
    assert!(
        developer_text(&after_tool.body)
            .iter()
            .find(|t| t.starts_with("Shellboy context v1:"))
            .unwrap()
            .contains(&format!(
                "AGENTS reference={}",
                root.path().join("old.md").display()
            ))
    );
    assert_eq!(
        reference_events(&after_tool.body).len(),
        1,
        "new reference event must not enter same-Turn history"
    );
    after_tool
        .reply
        .send((
            400,
            json!({"error":{"code":"synthetic","message":"fixture"}}),
        ))
        .unwrap();
    settled(&control, &thread).await;
    let run = request(
        &control,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.clone()),
        }),
    )
    .await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            agents: Some("new.md".into()),
            ..Default::default()
        }),
    )
    .await;
    std::fs::remove_file(root.path().join("old.md")).unwrap();
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    let retried = incoming(&control, &mut receive).await;
    assert_working_instructions(
        &retried.body,
        &root.path().join("old.md"),
        "OLD_EXPLICIT_ROLE",
    );
    assert_eq!(reference_events(&retried.body).len(), 1);
    retried
        .reply
        .send((200, answer("old snapshot complete")))
        .unwrap();
    settled(&control, &thread).await;
    request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "new turn".into(),
        }),
    )
    .await;
    let next = incoming(&control, &mut receive).await;
    assert_working_instructions(&next.body, &root.path().join("new.md"), "NEW_EXPLICIT_ROLE");
    let events = reference_events(&next.body);
    assert_eq!(events.len(), 3);
    let changed = events.last().unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        events.last().unwrap()["payload"]["change"]["after"]["path"],
        root.path().join("new.md").to_str().unwrap()
    );
    // Failure before adoption cannot consume the notification or lose its history node.
    next.reply
        .send((
            400,
            json!({"error":{"code":"synthetic","message":"fixture"}}),
        ))
        .unwrap();
    settled(&control, &thread).await;
    let run = request(
        &control,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.clone()),
        }),
    )
    .await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    let notification_retry = incoming(&control, &mut receive).await;
    assert_eq!(
        reference_events(&notification_retry.body)
            .iter()
            .filter(|v| v["event_id"] == changed)
            .count(),
        1
    );
    assert_working_instructions(
        &notification_retry.body,
        &root.path().join("new.md"),
        "NEW_EXPLICIT_ROLE",
    );
    notification_retry
        .reply
        .send((200, answer("new snapshot adopted")))
        .unwrap();
    settled(&control, &thread).await;
    let event = changed.clone();
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM node WHERE id=?1",
                    [format!("event-{event}")],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM delivery WHERE event_id=?1 AND state='processed'",
                    [event],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    // A completed Turn alone is not idle: await the existing durable Run boundary.
    let ended = request(
        &control,
        Command::Wait(WaitArgs {
            after: None,
            conditions: vec![WaitCondition::RunEnded { run_id: run }],
            timeout_ms: 5_000,
        }),
    )
    .await;
    assert_eq!(ended["status"], "satisfied");
    let before = saved_work(&control, &thread).await;
    assert!(!before.runs.is_empty() && !before.turns.is_empty() && !before.attempts.is_empty());
    assert!(before.runs.iter().all(|(_, state)| state == "completed"));
    assert!(
        before
            .turns
            .iter()
            .all(|(_, _, state)| state == "completed")
    );
    assert!(
        before
            .attempts
            .iter()
            .all(|(_, _, state)| matches!(state.as_str(), "completed" | "failed" | "incomplete"))
    );
    // Control::request completes apply_effects before returning. Compare all saved
    // row identities/states after that boundary, not a quiet interval on the proxy.
    std::fs::write(root.path().join("idle.md"), "IDLE_ROLE").unwrap();
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            agents: Some("idle.md".into()),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        saved_work(&control, &thread).await,
        before,
        "idle reference selection must not create or change Runs, Turns or attempts"
    );
    let target = thread.clone();
    control.store.database.call(move |db| {
        let (event, run): (String,Option<String>) = db.query_row(
            "SELECT id,target_run_id FROM event WHERE target_thread_id=?1 AND kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='agents_reference' ORDER BY rowid DESC LIMIT 1",
            [target], |r|Ok((r.get(0)?,r.get(1)?)))?;
        assert!(run.is_none(), "idle reference audit awaits a future Turn");
        assert_eq!(db.query_row("SELECT count(*) FROM delivery WHERE event_id=?1",[&event],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT count(*) FROM node WHERE id=?1",[format!("event-{event}")],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    std::fs::remove_file(root.path().join("idle.md")).unwrap();
    request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "selected file removed".into(),
        }),
    )
    .await;
    let absent = incoming(&control, &mut receive).await;
    assert!(working_instructions(&absent.body).is_none());
    absent
        .reply
        .send((200, answer("absent instructions allowed")))
        .unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn working_instruction_header_preserves_empty_and_original_snapshots() {
    use sha2::{Digest, Sha256};
    for original in ["", "original 日本語\r\nnext line\n"] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("AGENTS.md");
        std::fs::write(&source, original).unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = owner(root.path(), &url).await;
        let control = Control::new(&owner, root.path().into());
        let thread = thread(&control).await;
        let first = request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: "first snapshot".into(),
            }),
        )
        .await;
        let turn = first["turn_id"].as_str().unwrap().to_owned();
        let sent = incoming(&control, &mut receive).await;
        assert_working_instructions(&sent.body, &source, original);
        let id = turn.clone();
        let expected_body = original.to_owned();
        let expected_hash: String = Sha256::digest(original.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let expected_path = source.to_str().unwrap().to_owned();
        control.store.database.call(move |db| {
            let (path, body, hash, size):(String,String,String,Option<i64>) = db.query_row(
                "SELECT s.source_path,s.body,s.content_hash,s.observed_size FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            assert_eq!(path,expected_path);
            assert_eq!(body.as_bytes(),expected_body.as_bytes());
            assert_eq!(hash,expected_hash);
            assert_eq!(size,Some(expected_body.len() as i64));
            Ok(())
        }).await.unwrap();
        sent.reply.send((200, answer("first done"))).unwrap();
        settled(&control, &thread).await;
        std::fs::write(&source, "edited at the same source").unwrap();
        let next = request(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: "next Turn observes edit".into(),
            }),
        )
        .await;
        assert_ne!(next["turn_id"], first["turn_id"]);
        let sent = incoming(&control, &mut receive).await;
        assert_working_instructions(&sent.body, &source, "edited at the same source");
        control.store.database.call(move |db| {
            let body:String=db.query_row("SELECT s.body FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[turn],|r|r.get(0))?;
            assert_eq!(body,original,"later file edits cannot alter the saved original snapshot");
            Ok(())
        }).await.unwrap();
        sent.reply.send((200, answer("new snapshot done"))).unwrap();
        settled(&control, &thread).await;
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_working_instructions_recover_without_an_outbound_header() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("AGENTS.md");
    std::fs::write(&source, "unreadable fixture").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o0)).unwrap();
    if std::fs::File::open(&source).is_ok() {
        // Privileged runners can bypass mode bits; do not claim denial there.
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        return;
    }
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let sent = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "permission failure".into(),
        }),
    )
    .await;
    let id = sent["turn_id"].as_str().unwrap().to_owned();
    settled(&control, &thread).await;
    control
        .store
        .database
        .call(move |db| {
            let (state, reason, snapshot): (String, String, Option<String>) = db.query_row(
                "SELECT state,recovery_reason_json,instruction_snapshot_id FROM turn WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(state, "recovery_wait");
            assert_eq!(
                serde_json::from_str::<ApiError>(&reason).unwrap().code,
                ErrorCode::IoFailed
            );
            assert!(snapshot.is_none());
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM model_attempt WHERE turn_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        receive.try_recv().is_err(),
        "preparation failure precedes any HTTP request"
    );
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_response_usage_is_saved_without_provider_appendices_or_audit_loss() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    for status in ["failed", "incomplete"] {
        let target = thread(&control).await;
        send(&control, &target, "synthetic failure usage").await;
        let sent = incoming(&control, &mut receive).await;
        sent.reply.send((200,json!({"id":"failure-fixture","status":status,"error":{"code":"invalid_request_error","message":"synthetic diagnosis"},"usage":{"input_tokens":23,"input_tokens_details":{"cached_tokens":0,"appendix":"x".repeat(128*1024)},"output_tokens":3,"attribution":"x".repeat(128*1024)},"output":[]}))).unwrap();
        settled(&control, &target).await;
        control.store.database.call(move |db| {
            let (state,usage,error):(String,String,String)=db.query_row("SELECT a.state,a.usage_json,a.error_json FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 ORDER BY a.rowid DESC LIMIT 1",[target],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!(state,"incomplete", "received failure keeps the existing recovery classification");
            let canonical=json!({"input_tokens":23,"input_tokens_details":{"cached_tokens":0},"output_tokens":3});
            assert_eq!(serde_json::from_str::<Value>(&usage).unwrap(),canonical);
            let error:Value=serde_json::from_str(&error).unwrap();
            assert_eq!(error["raw_response"]["usage"],canonical);
            assert_eq!(error["raw_response"]["id"],"failure-fixture");
            assert_eq!(error["raw_response"]["status"],status);
            assert_eq!(error["raw_response"]["error"]["message"],"synthetic diagnosis");
            assert_eq!(error["raw_response"]["output"],json!([]));
            assert!(error.to_string().len()<4096);
            Ok(())
        }).await.unwrap();
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_summary_limit_failure_enters_recovery_without_adoption_or_resend() {
    crate::test_watchdog::watchdog("tests/runtime_features.rs: automatic_summary_limit_failure_enters_recovery_without_adoption_or_resend",async {
        let root=tempfile::tempdir().unwrap();let (url,mut receive,server)=proxy().await;
        let config=root.path().join("config.toml");
        std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=8\n[compaction]\nsummary_max_chars=2000\nsummary_map_max_requests=1\nsummary_reduce_max_requests=1\n")).unwrap();
        let owner=DataOwner::start(Some(config),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());let thread=thread(&control).await;
        send(&control,&thread,"protected task").await;
        receive.recv().await.unwrap().reply.send((200,json!({"id":"tool","status":"completed","output":[{"type":"function_call","call_id":"only-tool","name":"shell","arguments":json!({"command":"echo synthetic"}).to_string()}]}))).unwrap();
        let pending=receive.recv().await.unwrap();let target=thread.clone();
        control.store.database.call(move|db| {
            let tx=db.transaction()?;
            let request:String=tx.query_row("SELECT message_node_id FROM receipt WHERE target_thread_id=?1 ORDER BY created_at LIMIT 1",[&target],|r|r.get(0))?;
            let selected=tx.prepare("SELECT n.id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND (n.id=?2 OR json_extract(n.payload_json,'$.group_id') IS NOT NULL) ORDER BY p.ordinal")?.query_map(rusqlite::params![target,request],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let revision:i64=tx.query_row("SELECT selection_revision FROM thread WHERE id=?1",[&target],|r|r.get(0))?;
            shellboy::history::select_path(&tx,&target,revision as u64,&selected,"2026-10-31T00:00:00Z")?;tx.commit()?;Ok(())
        }).await.unwrap();
        pending.reply.send((400,json!({"error":{"code":"context_length_exceeded","message":"fixture pressure"}}))).unwrap();
        let log=root.path().join("summary-requests.jsonl");std::fs::write(&log,"").unwrap();
        for _ in 0..2 {
            let request=receive.recv().await.unwrap();assert_eq!(request.body["tools"],json!([]));
            use std::io::Write;
            writeln!(std::fs::OpenOptions::new().append(true).open(&log).unwrap(),"{}",request.body).unwrap();
            request.reply.send((200,answer(&"x".repeat(2001)))).unwrap();
        }
        loop {
            let ready=control.store.database.call(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE state='recovery_wait')",[],|r|r.get::<_,bool>(0))?)).await.unwrap();
            if ready {break;}tokio::task::yield_now().await;
        }
        control.store.database.call(|db| {
            assert_eq!(db.query_row("SELECT count(*) FROM node WHERE kind='summary'",[],|r|r.get::<_,i64>(0))?,0);
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt",[],|r|r.get::<_,i64>(0))?,2);
            let raw:String=db.query_row("SELECT error_json FROM model_attempt WHERE state='incomplete'",[],|r|r.get(0))?;
            let value:Value=serde_json::from_str(&raw).unwrap();let facts=&value["automatic_trim_attempt"]["error"]["details"];
            assert_eq!(facts["map_requests"],1);assert_eq!(facts["reduce_requests"],1);assert_eq!(facts["summary_limit"],2000);Ok(())
        }).await.unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(),2);
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();server.abort();
    }).await.expect("automatic summary recovery fixture watchdog");
}

struct HeadlessOwner {
    child: tokio::process::Child,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    stderr: tokio::process::ChildStderr,
    ready: Value,
}
impl HeadlessOwner {
    async fn start(data: &std::path::Path, config: &std::path::Path) -> Self {
        use tokio::io::AsyncBufReadExt;
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .args(["serve", "--data-dir"])
            .arg(data)
            .arg("--config")
            .arg(config)
            .current_dir(data.parent().unwrap())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let stderr = child.stderr.take().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).await.unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            stdout,
            stderr,
            ready,
        }
    }
    async fn finish(mut self, normal: bool, record: &std::path::Path) {
        let status = self.child.wait().await.unwrap();
        assert_eq!(status.success(), normal);
        let mut tail = String::new();
        self.stdout.read_to_string(&mut tail).await.unwrap();
        let mut errors = String::new();
        self.stderr.read_to_string(&mut errors).await.unwrap();
        assert!(errors.is_empty(), "{errors}");
        if normal {
            assert_eq!(tail.lines().count(), 1, "{tail}");
            let ended: Value = serde_json::from_str(&tail).unwrap();
            assert_eq!(ended["event"], "stopped");
            assert_eq!(ended["ok"], true);
            assert_eq!(ended["instance_id"], self.ready["instance_id"]);
        } else {
            assert!(tail.is_empty());
        }
        std::fs::write(record, format!("{}\n{tail}", self.ready)).unwrap();
        assert_eq!(
            std::fs::read_to_string(record).unwrap().lines().count(),
            if normal { 2 } else { 1 }
        );
    }
}
async fn headless_cli(data: &std::path::Path, args: &[&str]) -> Value {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .output()
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{value} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(value["ok"], true);
    value["result"].clone()
}
fn headless_db(data: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        data.join("shellboy.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}
fn record_headless_request(path: &std::path::Path, body: &Value) {
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap(),
        "{body}"
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_stop_boundaries_preserve_saved_tool_effects_and_recovery_counts() {
    for mode in [
        "before_request",
        "running_tool",
        "saved_result",
        "rpc",
        "signal",
        "disconnect",
    ] {
        if matches!(mode, "signal" | "disconnect") && !cfg!(unix) {
            continue;
        }
        crate::test_watchdog::watchdog("headless counted stop/restart boundary", async {
            let root = tempfile::tempdir().unwrap();
            let (url, mut incoming, proxy_task) = proxy().await;
            let data = root.path().join("data");
            let config = root.path().join("config.toml");
            std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
            let counter = root.path().join("effects.txt");
            let requests = root.path().join("requests.jsonl");
            std::fs::write(&requests, "").unwrap();
            let mut process = HeadlessOwner::start(&data, &config).await;
            let first_instance=process.ready["instance_id"].clone();
            let mut held = None;
            let mut thread = None;
            let expected_requests;
            if mode=="before_request" { expected_requests=0; }
            else {
                let created=headless_cli(&data, &["thread","create"]).await;
                let id=created["thread_id"].as_str().unwrap().to_owned();
                headless_cli(&data, &["message","send","--thread",&id,"--text","run one counted tool"]).await;
                let first=incoming.recv().await.unwrap();
                record_headless_request(&requests, &first.body);
                #[cfg(unix)]
                let quoted=counter.to_string_lossy().replace('\'',"'\"'\"'");
                #[cfg(unix)]
                let mut command=format!("printf 'once\\n' >> '{quoted}'");
                #[cfg(windows)]
                let mut command=format!("[IO.File]::AppendAllText('{}', \"once`n\")", counter.to_string_lossy().replace('\'',"''"));
                if mode=="running_tool" {
                    #[cfg(unix)] { command.push_str("; while :; do sleep 3600; done"); }
                    #[cfg(windows)] { command.push_str("; Start-Sleep -Seconds 3600"); }
                }
                first.reply.send((200,json!({"id":"one-counted-call","status":"completed","output":[{
                    "type":"function_call","call_id":"count-once","name":"shell","arguments":json!({"command":command}).to_string()
                }]}))).unwrap();
                if mode=="running_tool" {
                    loop {
                        if std::fs::read_to_string(&counter).is_ok_and(|s|s=="once\n") {break;}
                        tokio::task::yield_now().await;
                    }
                    let db=headless_db(&data);
                    let count:i64=db.query_row("SELECT COUNT(*) FROM tool_result WHERE call_id='count-once'",[],|r|r.get(0)).unwrap();
                    assert_eq!(count,0);
                    expected_requests=1;
                } else {
                    let next=incoming.recv().await.unwrap();
                    assert!(next.body["input"].as_array().unwrap().iter().any(|v|v["type"]=="function_call_output" && v["call_id"]=="count-once"));
                    record_headless_request(&requests,&next.body);
                    held=Some(next);
                    expected_requests=2;
                }
                thread=Some(id);
                assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(),1);
            }
            let normal=matches!(mode,"rpc"|"signal"|"disconnect");
            if mode=="rpc" {
                let accepted=headless_cli(&data,&["shutdown"]).await;
                assert_eq!(accepted["state"],"accepted");
                assert_eq!(accepted["shutdown_complete"],false);
            } else if mode=="disconnect" {
                #[cfg(unix)] {
                    let endpoint:shellboy::app::Endpoint=serde_json::from_slice(&std::fs::read(data.join("endpoint.json")).unwrap()).unwrap();
                    let mut stream=tokio::net::UnixStream::connect(&endpoint.address).await.unwrap();
                    write_frame(&mut stream,&json!({"protocol_version":PROTOCOL_VERSION})).await.unwrap();
                    let welcome:Value=read_frame(&mut stream).await.unwrap().unwrap();
                    assert_eq!(welcome["instance_id"],first_instance);
                    let request=Request::new(format!("{}:{}",endpoint.instance_id,uuid::Uuid::new_v4()),Command::OwnerShutdown(OwnerShutdownArgs{instance_id:endpoint.instance_id}));
                    write_frame(&mut stream,&request).await.unwrap();
                    drop(stream);
                }
            } else if mode=="signal" {
                #[cfg(unix)] assert_eq!(unsafe{libc::kill(process.child.id().unwrap() as i32,libc::SIGTERM)},0);
            } else { process.child.kill().await.unwrap(); }
            process.finish(normal,&root.path().join("first-stdout.jsonl")).await;
            drop(held);
            let restarted=HeadlessOwner::start(&data,&config).await;
            assert_ne!(restarted.ready["instance_id"],first_instance);
            let db=headless_db(&data);
            let integrity:String=db.query_row("PRAGMA quick_check",[],|r|r.get(0)).unwrap();
            assert_eq!(integrity,"ok");
            let attempts:i64=db.query_row("SELECT COUNT(*) FROM model_attempt",[],|r|r.get(0)).unwrap();
            assert_eq!(attempts,expected_requests);
            if let Some(thread)=thread {
                let (state,reason):(String,String)=db.query_row("SELECT state,stop_reason FROM run WHERE thread_id=?1 ORDER BY rowid LIMIT 1",[&thread],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
                assert_eq!(state,"stopped");
                assert_eq!(reason,if normal {"app_shutdown"} else {"previous_instance_interrupted"});
                let results:i64=db.query_row("SELECT COUNT(*) FROM tool_result WHERE call_id='count-once'",[],|r|r.get(0)).unwrap();
                assert_eq!(results,1);
                assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(),1);
                drop(db);
                let continuation=headless_cli(&data,&["message","send","--thread",&thread,"--text","continue without redoing saved work"]).await;
                let resumed=incoming.recv().await.unwrap();
                record_headless_request(&requests,&resumed.body);
                assert!(resumed.body["input"].as_array().unwrap().iter().any(|v|v["type"]=="function_call_output" && v["call_id"]=="count-once"));
                resumed.reply.send((200,answer("finished without another tool"))).unwrap();
                let resumed_run=continuation["run_id"].as_str().unwrap();
                loop {
                    let state:String=headless_db(&data).query_row("SELECT state FROM run WHERE id=?1",[resumed_run],|r|r.get(0)).unwrap();
                    if state=="completed" {break;}
                    tokio::task::yield_now().await;
                }
                assert_eq!(std::fs::read_to_string(&counter).unwrap(),"once\n");
            } else {drop(db);}
            assert_eq!(std::fs::read_to_string(&requests).unwrap().lines().count(),expected_requests as usize+usize::from(mode!="before_request"));
            assert!(incoming.try_recv().is_err(), "unexpected extra model request");
            eprintln!("headless-boundary {}",json!({"mode":mode,"first_requests":expected_requests,"all_requests":expected_requests+if mode=="before_request"{0}else{1},"effects":if mode=="before_request"{0}else{1},"first_stdout_rows":if normal{2}else{1},"restart_stdout_rows":2}));
            headless_cli(&data,&["shutdown"]).await;
            restarted.finish(true,&root.path().join("restart-stdout.jsonl")).await;
            proxy_task.abort(); let _=proxy_task.await;
        }).await.unwrap();
    }
}

#[tokio::test]
async fn headless_shutdown_requires_explicit_directory_and_final_output_loss_is_not_success() {
    crate::test_watchdog::watchdog(
        "headless shutdown addressing and final output failure",
        async {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join("config.toml");
            std::fs::write(&config, "").unwrap();
            let data = root.path().join("data");
            let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
                .arg("shutdown")
                .env("SHELLBOY_DATA_DIR", root.path().join("parent"))
                .output()
                .await
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stderr.is_empty());
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["error"]["code"], "invalid_argument");
            assert!(!root.path().join("parent").exists());
            let process = HeadlessOwner::start(&data, &config).await;
            let HeadlessOwner {
                mut child,
                stdout,
                mut stderr,
                ..
            } = process;
            drop(stdout);
            let accepted = headless_cli(&data, &["shutdown"]).await;
            assert_eq!(accepted["state"], "accepted");
            assert!(!child.wait().await.unwrap().success());
            let mut errors = String::new();
            stderr.read_to_string(&mut errors).await.unwrap();
            assert!(
                errors.is_empty(),
                "post-ready failures do not become startup stderr"
            );
            assert!(!data.join("endpoint.json").exists());
            let _lock = shellboy::app::OwnerLock::acquire(&data).unwrap();
            let integrity: String = headless_db(&data)
                .query_row("PRAGMA quick_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
        },
    )
    .await
    .unwrap();
}

fn serve_quote(path: &std::path::Path) -> String {
    #[cfg(unix)]
    {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
    }
    #[cfg(windows)]
    {
        format!("'{}'", path.to_string_lossy().replace('\'', "''"))
    }
}
async fn serve_pid_live(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let output = tokio::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .await
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        !state.trim().is_empty() && !state.trim_start().starts_with('Z')
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == 259
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_background_parent_stop_reaps_nested_owner_and_tool_without_reexecution() {
    for mode in ["run", "shell", "owner", "owner_kill"] {
        crate::test_watchdog::watchdog("headless actual background parent/nested ownership", async {
            let root = tempfile::tempdir().unwrap();
            let (parent_url, mut parent_requests, parent_proxy) = proxy().await;
            let (child_url, mut child_requests, child_proxy) = proxy().await;
            let parent_data = root.path().join("parent");
            let child_data = root.path().join("child");
            let parent_config = root.path().join("parent.toml");
            let child_config = root.path().join("child.toml");
            for (path, url) in [(&parent_config, parent_url), (&child_config, child_url)] {
                std::fs::write(path, format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
            }
            let mut parent = HeadlessOwner::start(&parent_data, &parent_config).await;
            let parent_thread = headless_cli(&parent_data, &["thread", "create"]).await["thread_id"].as_str().unwrap().to_owned();
            let accepted = headless_cli(&parent_data, &["message", "send", "--thread", &parent_thread, "--text", "start owned background child"]).await;
            let first = parent_requests.recv().await.unwrap();
            let parent_log = root.path().join("parent-requests.jsonl");
            record_headless_request(&parent_log, &first.body);
            let command = format!("{}{} serve --data-dir {} --config {}",
                if cfg!(windows) { "& " } else { "" }, serve_quote(std::path::Path::new(env!("CARGO_BIN_EXE_shellboy"))), serve_quote(&child_data), serve_quote(&child_config));
            first.reply.send((200, json!({"id":"child-owner","status":"completed","output":[{
                "type":"function_call","call_id":"child-owner","name":"shell",
                "arguments":json!({"command":command,"background":true}).to_string()
            }]}))).unwrap();
            let parent_next = parent_requests.recv().await.unwrap();
            record_headless_request(&parent_log, &parent_next.body);
            loop {
                if child_data.join("endpoint.json").exists() { break; }
                tokio::task::yield_now().await;
            }
            let child_pid = headless_cli(&child_data, &["status", "--memory"]).await["process"]["pid"].as_u64().unwrap() as u32;
            assert!(serve_pid_live(child_pid).await);
            let child_thread = headless_cli(&child_data, &["thread", "create"]).await["thread_id"].as_str().unwrap().to_owned();
            headless_cli(&child_data, &["message", "send", "--thread", &child_thread, "--text", "one counted nested tool"]).await;
            let child_first = child_requests.recv().await.unwrap();
            let child_log = root.path().join("child-requests.jsonl");
            record_headless_request(&child_log, &child_first.body);
            let effects = root.path().join("effects.txt");
            let tool_pid_file = root.path().join("tool.pid");
            #[cfg(unix)]
            let tool = format!("printf 'once\\n' >> {}; printf '%s' $$ > {}; exec sleep 3600", serve_quote(&effects), serve_quote(&tool_pid_file));
            #[cfg(windows)]
            let tool = format!("[IO.File]::AppendAllText({}, \"once`n\"); [IO.File]::WriteAllText({}, [string]$PID); Start-Sleep -Seconds 3600", serve_quote(&effects), serve_quote(&tool_pid_file));
            child_first.reply.send((200, json!({"id":"nested-tool","status":"completed","output":[{
                "type":"function_call","call_id":"nested-once","name":"shell","arguments":json!({"command":tool}).to_string()
            }]}))).unwrap();
            let tool_pid: u32 = loop {
                if let Ok(text) = std::fs::read_to_string(&tool_pid_file)
                    && let Ok(pid) = text.parse() { break pid; }
                tokio::task::yield_now().await;
            };
            assert!(serve_pid_live(tool_pid).await);
            assert_eq!(std::fs::read_to_string(&effects).unwrap(), "once\n");
            let execution: String = headless_db(&parent_data).query_row("SELECT id FROM shell_execution ORDER BY rowid DESC LIMIT 1", [], |r| r.get(0)).unwrap();
            match mode {
                "run" => { headless_cli(&parent_data, &["run", "stop", "--run", accepted["run_id"].as_str().unwrap()]).await; },
                "shell" => { headless_cli(&parent_data, &["shell", "stop", "--execution", &execution]).await; },
                "owner" => { headless_cli(&parent_data, &["shutdown"]).await; },
                _ => { parent.child.kill().await.unwrap(); },
            }
            while serve_pid_live(child_pid).await || serve_pid_live(tool_pid).await { tokio::task::yield_now().await; }
            drop(parent_next);
            if matches!(mode, "run" | "shell") { headless_cli(&parent_data, &["shutdown"]).await; }
            parent.finish(mode != "owner_kill", &root.path().join("parent-stdout.jsonl")).await;
            let restarted = HeadlessOwner::start(&child_data, &child_config).await;
            let db = headless_db(&child_data);
            assert_eq!(db.query_row("PRAGMA quick_check", [], |r| r.get::<_,String>(0)).unwrap(), "ok");
            assert_eq!(db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
            assert_eq!(db.query_row("SELECT COUNT(*) FROM tool_result WHERE call_id='nested-once'", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
            let (state, reason): (String,String) = db.query_row("SELECT state,stop_reason FROM run ORDER BY rowid LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert_eq!(state, "stopped");
            assert!(matches!(reason.as_str(), "app_shutdown" | "previous_instance_interrupted"));
            drop(db);
            assert_eq!(std::fs::read_to_string(&effects).unwrap(), "once\n");
            assert_eq!(std::fs::read_to_string(&parent_log).unwrap().lines().count(), 2);
            assert_eq!(std::fs::read_to_string(&child_log).unwrap().lines().count(), 1);
            assert!(child_requests.try_recv().is_err());
            let continued = headless_cli(&child_data, &["message", "send", "--thread", &child_thread, "--text", "continue from saved tool, do not repeat it"]).await;
            let continuation = child_requests.recv().await.unwrap();
            record_headless_request(&child_log, &continuation.body);
            assert!(continuation.body["input"].as_array().unwrap().iter().any(|v| v["type"] == "function_call_output" && v["call_id"] == "nested-once"));
            continuation.reply.send((200, answer("finished without another tool"))).unwrap();
            loop {
                if headless_cli(&child_data, &["run", "get", "--run", continued["run_id"].as_str().unwrap()]).await["state"] == "completed" { break; }
                tokio::task::yield_now().await;
            }
            assert_eq!(std::fs::read_to_string(&effects).unwrap(), "once\n");
            assert_eq!(std::fs::read_to_string(&child_log).unwrap().lines().count(), 2);
            headless_cli(&child_data, &["shutdown"]).await;
            restarted.finish(true, &root.path().join("child-restart-stdout.jsonl")).await;
            eprintln!("nested-parent {}", json!({"mode":mode,"parent_http":2,"child_http_before_continue":1,"child_http":2,"effects":1,"live_processes":0}));
            parent_proxy.abort(); child_proxy.abort();
        }).await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_ctrl_c_and_repeated_term_drain_or_force_kill_preserve_database() {
    for force in [false, true] {
        crate::test_watchdog::watchdog("headless signals at blocked snapshot", async {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join("config.toml");
            std::fs::write(&config, "").unwrap();
            let data = root.path().join("data");
            let mut owner = HeadlessOwner::start(&data, &config).await;
            let identity = owner.ready["instance_id"].clone();
            let endpoint: shellboy::app::Endpoint =
                serde_json::from_slice(&std::fs::read(data.join("endpoint.json")).unwrap())
                    .unwrap();
            let hold = rusqlite::Connection::open(data.join("shellboy.db")).unwrap();
            hold.execute_batch("BEGIN IMMEDIATE").unwrap();
            let pid = owner.child.id().unwrap() as i32;
            assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
            loop {
                if tokio::net::UnixStream::connect(&endpoint.address)
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
            assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
            if force {
                owner.child.kill().await.unwrap();
            }
            hold.execute_batch("ROLLBACK").unwrap();
            owner.finish(!force, &root.path().join("first.jsonl")).await;
            let restarted = HeadlessOwner::start(&data, &config).await;
            assert_ne!(restarted.ready["instance_id"], identity);
            let db = headless_db(&data);
            assert_eq!(
                db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            for table in ["operation_receipt", "model_attempt", "tool_result"] {
                assert_eq!(
                    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
            }
            drop(db);
            headless_cli(&data, &["shutdown"]).await;
            restarted
                .finish(true, &root.path().join("restart.jsonl"))
                .await;
        })
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_cli_discovers_recovery_wait_retries_and_stops_without_automatic_post() {
    crate::test_watchdog::watchdog("headless CLI recovery workflow", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut requests, proxy_task) = proxy().await;
        let data = root.path().join("data");
        let config = root.path().join("config.toml");
        std::fs::write(&config, format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
        let owner = HeadlessOwner::start(&data, &config).await;
        let thread = headless_cli(&data, &["thread", "create"]).await["thread_id"].as_str().unwrap().to_owned();
        let log = root.path().join("requests.jsonl");
        for retry in [true, false] {
            let sent = headless_cli(&data, &["message", "send", "--thread", &thread, "--text", "request fixture recovery"]).await;
            let run = sent["run_id"].as_str().unwrap();
            let incoming = requests.recv().await.unwrap();
            record_headless_request(&log, &incoming.body);
            incoming.reply.send((400, json!({"error":{"code":"invalid_request_error","message":"synthetic permanent error"}}))).unwrap();
            loop {
                let detail = headless_cli(&data, &["run", "get", "--run", run]).await;
                if detail["context"]["recovery_reason"].is_object() { break; }
                tokio::task::yield_now().await;
            }
            let listed = headless_cli(&data, &["status", "--thread", &thread]).await;
            assert!(listed.to_string().contains(run));
            assert!(requests.try_recv().is_err(), "no automatic POST in recovery");
            if retry {
                headless_cli(&data, &["run", "retry", "--run", run]).await;
                let incoming = requests.recv().await.unwrap();
                record_headless_request(&log, &incoming.body);
                incoming.reply.send((200, answer("completed after explicit retry"))).unwrap();
                loop {
                    if headless_cli(&data, &["run", "get", "--run", run]).await["state"] == "completed" { break; }
                    tokio::task::yield_now().await;
                }
            } else {
                headless_cli(&data, &["run", "stop", "--run", run]).await;
                loop {
                    if headless_cli(&data, &["run", "get", "--run", run]).await["state"] == "stopped" { break; }
                    tokio::task::yield_now().await;
                }
            }
        }
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 3);
        assert_eq!(headless_db(&data).query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get::<_,i64>(0)).unwrap(), 3);
        assert!(requests.try_recv().is_err());
        headless_cli(&data, &["shutdown"]).await;
        owner.finish(true, &root.path().join("stdout.jsonl")).await;
        proxy_task.abort();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_native_cutpoint_helper() {
    let Some(root) = std::env::var_os("SB5_NATIVE_CUT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let cut = std::env::var("SB5_NATIVE_CUT").unwrap();
    let pause = || {
        std::fs::write(root.join("cut.jsonl"), format!("{}\n", json!({"cut":cut}))).unwrap();
        // Only this disposable child parks; the parent kills it at the witness.
        loop {
            std::thread::park();
        }
    };
    let desired = cut.clone();
    let callback = |stage| {
        if (desired == "startup_database" && stage == shellboy::app::StartupStage::Database)
            || (desired == "startup_interrupted"
                && stage == shellboy::app::StartupStage::InterruptedWork)
        {
            pause();
        }
    };
    let prepared = shellboy::owner_session::initialize_reporting(
        Some(root.join("config.toml")),
        Some(root.join("data")),
        tokio_util::sync::CancellationToken::new(),
        callback,
    )
    .await
    .unwrap()
    .unwrap();
    if cut == "snapshot_committed" {
        prepared.control.prepare_shutdown().await.unwrap();
        pause();
    }
    if cut == "before_database_close" {
        prepared.control.shutdown().await.unwrap();
        pause();
    }
    panic!("unknown cutpoint");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_native_startup_snapshot_and_database_close_kills_keep_committed_facts() {
    crate::test_watchdog::watchdog("native common owner startup/drain cutpoints", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut requests, proxy_task) = proxy().await;
        let data = root.path().join("data");
        let config = root.path().join("config.toml");
        std::fs::write(&config, format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
        let seed = HeadlessOwner::start(&data, &config).await;
        let thread = headless_cli(&data, &["thread", "create"]).await["thread_id"].as_str().unwrap().to_owned();
        let sent = headless_cli(&data, &["message", "send", "--thread", &thread, "--text", "committed seed"]).await;
        let request = requests.recv().await.unwrap();
        let log = root.path().join("http.jsonl");
        record_headless_request(&log, &request.body);
        request.reply.send((200, answer("committed answer"))).unwrap();
        loop {
            if headless_cli(&data, &["run", "get", "--run", sent["run_id"].as_str().unwrap()]).await["state"] == "completed" { break; }
            tokio::task::yield_now().await;
        }
        headless_cli(&data, &["shutdown"]).await;
        seed.finish(true, &root.path().join("seed.jsonl")).await;
        for cut in ["startup_database", "startup_interrupted", "snapshot_committed", "before_database_close"] {
            let witness = root.path().join("cut.jsonl");
            std::fs::write(&witness, "").unwrap();
            let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime_features::headless_native_cutpoint_helper", "--nocapture"])
                .env("SB5_NATIVE_CUT_ROOT", root.path()).env("SB5_NATIVE_CUT", cut)
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
            loop {
                if std::fs::read_to_string(&witness).is_ok_and(|s| s.ends_with('\n') && serde_json::from_str::<Value>(&s).is_ok_and(|v| v["cut"] == cut)) { break; }
                assert!(child.try_wait().unwrap().is_none(), "cutpoint child exited before {cut}");
                tokio::task::yield_now().await;
            }
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            let owner = HeadlessOwner::start(&data, &config).await;
            let db = headless_db(&data);
            assert_eq!(db.query_row("PRAGMA quick_check", [], |r| r.get::<_,String>(0)).unwrap(), "ok");
            assert_eq!(db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
            assert_eq!(db.query_row("SELECT COUNT(*) FROM receipt", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
            assert_eq!(db.query_row("SELECT COUNT(*) FROM tool_result", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
            assert_eq!(db.query_row("SELECT state FROM run WHERE id=?1", [sent["run_id"].as_str().unwrap()], |r| r.get::<_,String>(0)).unwrap(), "completed");
            drop(db);
            assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
            assert!(requests.try_recv().is_err());
            headless_cli(&data, &["shutdown"]).await;
            owner.finish(true, &root.path().join(format!("{cut}.jsonl"))).await;
            eprintln!("native-cut {}", json!({"cut":cut,"witness_rows":1,"http":1,"message_receipts":1,"tool_effects":0}));
        }
        proxy_task.abort();
    }).await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_summary_boundary_waits_after_foreground_and_not_for_background_completion() {
    crate::test_watchdog::watchdog("summary boundary observation",async {
        let root=tempfile::tempdir().unwrap();let (url,mut receive,server)=proxy_with_sse(true).await;
        let owner=owner(root.path(),&url).await;let control=Control::new(&owner,root.path().into());
        let thread=thread(&control).await;
        for n in 0..2 {send(&control,&thread,&format!("older-{n} {}","x".repeat(4000))).await;incoming(&control,&mut receive).await.reply.send((200,answer("older answer"))).unwrap();settled(&control,&thread).await;}
        send(&control,&thread,"foreground and background gate").await;
        let response=json!({"id":"tool-gates","status":"completed","output":[
            {"type":"function_call","call_id":"background","name":"shell","arguments":json!({"command":"cat >/dev/null # background-gate","background":true,"stdin_open":true}).to_string()},
            {"type":"function_call","call_id":"foreground","name":"shell","arguments":json!({"command":"read -r line; exit 1 # foreground-gate","stdin_open":true}).to_string()}
        ]});
        let mut events=control.subscribe();incoming(&control,&mut receive).await.reply.send((200,response)).unwrap();
        let (background,foreground,run)=loop {
            let target=thread.clone();let rows=control.store.database.call(move|db|{
                let mut q=db.prepare("SELECT id,owner_run_id,json_extract(shell_json,'$.command') FROM shell_execution WHERE owner_run_id=(SELECT id FROM run WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1) ORDER BY rowid")?;
                q.query_map([target],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>().map_err(Into::into)
            }).await.unwrap();
            if rows.len()==2 {break(rows[0].0.clone(),rows[1].0.clone(),rows[0].1.clone());}
            let _=events.recv().await;
        };
        let accepted=request(&control,Command::Compact(ThreadTarget {thread_id:Some(thread.clone())})).await;
        request(&control,Command::ShellEof(ExecutionTarget {execution_id:foreground.clone()})).await;
        let summary=incoming(&control,&mut receive).await;
        assert_eq!(summary.body["tools"],json!([]),"the boundary owner issues the summary, not a new ordinary attempt");
        let blocked=request(&control,Command::RunGet(RunTarget {run_id:run.clone()})).await;
        assert_eq!(blocked["waiting_reason"]["kind"],"context_edit_boundary");
        assert_eq!(blocked["waiting_reason"]["operation_id"],accepted["operation_id"]);
        assert_eq!(blocked["waiting_reason"]["action"],"compact");
        let projected_thread=thread.clone();
        let projected=control.store.database.call(move|db|shellboy::frontend::activity::project(db,&projected_thread)).await.unwrap();
        assert_eq!(serde_json::to_value(projected.snapshot.waiting_reason).unwrap(),blocked["waiting_reason"]);
        // Finishing the background shell cannot release the held summary boundary.
        request(&control,Command::ShellEof(ExecutionTarget {execution_id:background.clone()})).await;
        loop {
            let ended=request(&control,Command::ShellGet(ExecutionTarget {execution_id:background.clone()})).await;
            if ended["state"]=="exited" {break;}let _=events.recv().await;
        }
        let still=request(&control,Command::RunGet(RunTarget {run_id:run.clone()})).await;
        assert_eq!(still["waiting_reason"],blocked["waiting_reason"]);
        assert!(receive.try_recv().is_err());
        summary.reply.send((200,answer("Earlier decisions retained."))).unwrap();
        let resumed=incoming(&control,&mut receive).await;
        assert_ne!(resumed.body["tools"],json!([]));
        let resumed_run=request(&control,Command::RunGet(RunTarget {run_id:run.clone()})).await;
        assert!(resumed_run.get("waiting_reason").is_none());
        assert!(resumed.body.to_string().contains("shell_ended"),"saved shell completions enter the next request boundary");
        resumed.reply.send((200,answer("resumed after summary"))).unwrap();settled(&control,&thread).await;
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();server.abort();
    }).await.unwrap();
}

#[tokio::test]
async fn description_raw_arguments_survive_adoption_and_next_model_request_byte_exact() {
    crate::test_watchdog::watchdog("description raw adoption and model replay", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut requests, server) = proxy().await;
        let owner = owner(root.path(), &url).await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        let arguments = format!(" {{ \"description\" : {}, \"command\" : \"echo RAW_DESCRIPTION_COMMAND\" }} \n", json!("語".repeat(100)));
        send(&control, &target, "one raw description call").await;
        let first = incoming(&control, &mut requests).await;
        let log = root.path().join("requests.jsonl");
        record_headless_request(&log, &first.body);
        first.reply.send((200, json!({"id":"raw-description","status":"completed","output":[{
            "type":"function_call","name":"shell","call_id":"raw-description-call","arguments":arguments
        }]}))).unwrap();
        let next = incoming(&control, &mut requests).await;
        record_headless_request(&log, &next.body);
        let replayed = next.body["input"].as_array().unwrap().iter().find(|v| v["type"] == "function_call" && v["call_id"] == "raw-description-call").unwrap();
        assert_eq!(replayed["arguments"], arguments);
        let original = arguments.clone();
        control.store.database.call(move |db| {
            let saved: String = db.query_row("SELECT arguments_json FROM tool_call WHERE call_id='raw-description-call'", [], |r| r.get(0))?;
            assert_eq!(saved, original);
            let payload: String = db.query_row("SELECT payload_json FROM node WHERE kind='assistant_response' AND json_extract(payload_json,'$.output[0].call_id')='raw-description-call'", [], |r| r.get(0))?;
            let payload: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["output"][0]["arguments"], original);
            assert!(payload.get("shell_description_truncations").is_none());
            Ok(())
        }).await.unwrap();
        next.reply.send((200, answer("complete"))).unwrap();
        settled(&control, &target).await;
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
        assert!(requests.try_recv().is_err());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_description_schema_keeps_missing_and_empty_model_output_executable() {
    crate::test_watchdog::watchdog("required description output fallback", async {
        let root=tempfile::tempdir().unwrap();let(url,mut receive,server)=proxy().await;
        let owner=owner(root.path(),&url).await;let control=Control::new(&owner,root.path().into());
        let target=thread(&control).await;
        for (id,description) in [("absent",None),("empty",Some(""))] {
            send(&control,&target,id).await;
            let first=incoming(&control,&mut receive).await;
            assert_eq!(first.body["tools"][0]["parameters"]["required"],json!(["command","description"]));
            let mut arguments=json!({"command":"echo REQUIRED_DESCRIPTION_FALLBACK"});
            if let Some(description)=description {arguments["description"]=json!(description);}
            first.reply.send((200,json!({"id":id,"status":"completed","output":[{"type":"function_call","call_id":id,"name":"shell","arguments":arguments.to_string()}]}))).unwrap();
            let next=incoming(&control,&mut receive).await;
            assert!(next.body["input"].as_array().unwrap().iter().any(|item|item["type"]=="function_call_output" && item["call_id"]==id && item["output"].as_str().is_some_and(|text|text.contains("REQUIRED_DESCRIPTION_FALLBACK"))));
            next.reply.send((200,answer("done"))).unwrap();settled(&control,&target).await;
        }
        control.store.database.call(|db| {
            assert_eq!(db.query_row("SELECT count(*) FROM shell_execution WHERE state='exited' AND exit_code=0",[],|r|r.get::<_,i64>(0))?,2);
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();server.abort();
    }).await.unwrap();
}

// #125 proactive compaction: reported input tokens of the last adopted attempt
// at or above `auto_at_tokens` compact once at the next request boundary.
async fn proactive_owner(root: &std::path::Path, url: &str, extra: &str) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n{extra}")).unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
fn answer_with_usage(text: &str, input_tokens: u64) -> Value {
    let mut body = answer(text);
    body["usage"] =
        json!({"input_tokens":input_tokens,"output_tokens":1,"total_tokens":input_tokens+1});
    body
}
async fn proactive_events(control: &Arc<Control>, thread: &str) -> Vec<Value> {
    let listed = request(
        control,
        Command::CompactList(CompactListArgs {
            thread_id: Some(thread.into()),
            since: None,
            until: None,
            limit: 50,
        }),
    )
    .await;
    listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["reason"] == "proactive_threshold")
        .cloned()
        .collect()
}
async fn proactive_partial_spill_fixture(oversized: bool) {
    crate::test_watchdog::watchdog("proactive partial spill adoption", async move {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let options = if oversized {
            "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n"
        } else {
            "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n"
        };
        let owner = proactive_owner(root.path(), &url, options).await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        for text in ["hi", "yo"] {
            send(&control, &target, text).await;
            incoming(&control, &mut receive).await.reply.send((200, answer("ok"))).unwrap();
            settled(&control, &target).await;
        }
        let current = if oversized { "protected current ".repeat(70_000) } else { "long current turn".to_owned() };
        send(&control, &target, &current).await;
        let tool_response = |index: u32, tokens: u32| json!({"id":format!("small-{index}"),"status":"completed","usage":{"input_tokens":tokens,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("small-{index}"),"name":"shell","arguments":json!({"command":"echo last"}).to_string()}]});
        let large_command = if cfg!(windows) { "'0' * 50000" } else { "printf '%050000d' 0" };
        for index in 0..2 {
            let current = incoming(&control, &mut receive).await;
            current.reply.send((200, json!({"id":format!("large-{index}"),"status":"completed", "usage":{"input_tokens":100,"output_tokens":1}, "output":[{
                "type":"function_call","call_id":format!("large-{index}"),"name":"shell",
                "arguments":json!({"command":large_command}).to_string()
            }]}))).unwrap();
        }
        let before = incoming(&control, &mut receive).await;
        let before_size = before.body["input"].as_array().unwrap().iter().filter_map(|item| item.get("output").and_then(Value::as_str)).map(str::len).sum::<usize>();
        assert!(before_size > 30_000, "fixture must contain both large tool outputs: {before_size}");
        before.reply.send((200, tool_response(2, 200))).unwrap();
        let summary = incoming(&control, &mut receive).await;
        assert_eq!(summary.body["tools"], json!([]));
        let first_summary_input = summary.body["input"].to_string();
        assert!(first_summary_input.contains("large-0") && first_summary_input.contains("large-1"));
        assert!(!first_summary_input.contains("small-2"), "the newest completed group is protected");
        let mut rejected = answer(&"too-long-summary ".repeat(180));
        rejected["usage"] = json!({"input_tokens":321,"output_tokens":123,"total_tokens":444});
        summary.reply.send((200, rejected)).unwrap();
        let after = incoming(&control, &mut receive).await;
        assert_ne!(after.body["tools"], json!([]), "the measured request must be an ordinary conversation request, not a summary reduce request");
        let context = current_context(&control, &target).await;
        assert_current_context_once(&after.body, &context, "after proactive spill");
        let after_size = after.body["input"].as_array().unwrap().iter().filter_map(|item| item.get("output").and_then(Value::as_str)).map(str::len).sum::<usize>();
        assert!(after_size < before_size / 3, "actual replay input must shrink: {before_size} -> {after_size}");
        let omitted = after.body["input"].as_array().unwrap().iter()
            .filter_map(|item| item.get("output").and_then(Value::as_str))
            .filter_map(|output| serde_json::from_str::<Value>(output).ok())
            .filter(|output| output["omitted"] == true).count();
        assert!(omitted >= 2, "both tool output replacements must be replayed: {omitted}");
        let first_events = proactive_events(&control, &target).await;
        assert_eq!(first_events.len(), 1, "{first_events:#?}");
        assert_eq!(first_events[0]["metrics"]["summary_rejected"], true, "{first_events:#?}");
        assert!(first_events[0]["metrics"]["source_characters"].as_u64().unwrap() < 3_000, "tiny summary source: {first_events:#?}");
        assert_eq!(first_events[0]["metrics"]["summary_usage"]["requests"], 1);
        assert_eq!(first_events[0]["metrics"]["summary_usage"]["reduce_requests"], 0);
        after.reply.send((200, tool_response(3, 205))).unwrap();
        let middle = incoming(&control, &mut receive).await;
        assert_eq!(middle.body["tools"], json!([]));
        let middle_input = middle.body["input"].to_string();
        assert!(middle_input.contains("small-2"), "the new middle target must contain the newly eligible completed group");
        assert!(!middle_input.contains("small-3"), "the newest tool group stays protected");
        middle.reply.send((200, answer("The earlier shell commands completed."))).unwrap();
        let gated = incoming(&control, &mut receive).await;
        assert_ne!(gated.body["tools"], json!([]), "the same sources must not trigger another summary");
        gated.reply.send((200, answer("finished"))).unwrap();
        settled(&control, &target).await;
        let events = proactive_events(&control, &target).await;
        assert!((1..=2).contains(&events.len()), "only a newly eligible tool group can add one attempt: {events:#?}");
        assert_eq!(events.iter().filter(|event| event["metrics"]["summary_rejected"] == true).count(), 1, "the rejected summary must not be reissued");
        let item = events.iter().find(|event| event["metrics"]["summary_rejected"] == true).expect("original summary rejection is retained");
        assert_eq!(events.len(), 2, "newly eligible middle sources produce one additional attempt");
        let later = events.iter().find(|event| event["metrics"]["summary_rejected"] != true).unwrap();
        assert_eq!(later["state"], "adopted", "the new middle summary must be adopted");
        assert_eq!(item["state"], "adopted");
        assert_eq!(item["metrics"]["summary_rejected"], true);
        assert!(item["metrics"]["spilled_groups"].as_u64().unwrap() >= 2);
        assert_eq!(item["metrics"]["summary_usage"]["input_tokens"]["known_sum"], 321);
        assert_eq!(item["metrics"]["summary_usage"]["output_tokens"]["known_sum"], 123);
        let attempt_id = item["attempt_id"].as_str().unwrap().to_owned();
        let saved: String = control.store.database.call(move |db| Ok(db.query_row(
            "SELECT error_json FROM model_attempt WHERE id=?1", [attempt_id], |r| r.get(0)
        )?)).await.unwrap();
        let saved: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["automatic_trim_attempt"]["summary_error"]["code"], "invalid_state");
        eprintln!("partial-spill replay bytes {before_size} -> {after_size}; summary input=321 output=123");
        control.shutdown().await.unwrap(); owner.shutdown().await.unwrap(); server.abort();
    }).await.unwrap();
}

/// #181: in a long Turn, proactive compaction replaces the old middle segment
/// with one developer summary, keeps the request and the latest tool group, and
/// starts a new reasoning epoch, so no earlier reasoning is replayed.
#[tokio::test]
async fn proactive_active_turn_summary_replaces_middle_and_drops_reasoning() {
    crate::test_watchdog::watchdog("proactive active-Turn summary", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        send(&control, &target, "long current turn").await;
        for (index, tokens) in [(0, 100), (1, 100), (2, 250)] {
            let current = incoming(&control, &mut receive).await;
            current.reply.send((200, json!({"id":format!("step-{index}"),"status":"completed",
                "usage":{"input_tokens":tokens,"output_tokens":1},
                "output":[
                    {"type":"reasoning","id":format!("rs-{index}"),"summary":[],"encrypted_content":"E".repeat(4_000)},
                    {"type":"function_call","call_id":format!("step-{index}"),"name":"shell",
                     "arguments":json!({"command":format!("echo STEP{index}")}).to_string()}
                ]}))).unwrap();
        }
        let summary = incoming(&control, &mut receive).await;
        assert_eq!(summary.body["tools"], json!([]), "the attempt must summarize before continuing");
        let material = summary.body["input"].to_string();
        assert!(material.contains("step-0") && material.contains("step-1"), "covered groups are summary input");
        assert!(!material.contains("step-2"), "the latest tool group stays verbatim");
        assert!(!material.contains("EEEE"), "encrypted reasoning is never summary material");
        let mut reply = answer("## Goal\nRun steps\n## Current state\nSTEP0 and STEP1 ran");
        reply["usage"] = json!({"input_tokens":50,"output_tokens":5});
        summary.reply.send((200, reply)).unwrap();
        let after = incoming(&control, &mut receive).await;
        assert_ne!(after.body["tools"], json!([]));
        let input = after.body["input"].as_array().unwrap();
        assert!(input.iter().all(|item| item["type"] != "reasoning"), "no reasoning from before the adoption: {input:?}");
        assert!(input.iter().all(|item| item["call_id"] != "step-0" && item["call_id"] != "step-1"));
        assert!(input.iter().any(|item| item["type"] == "function_call" && item["call_id"] == "step-2"));
        assert!(input.iter().any(|item| item["type"] == "function_call_output" && item["call_id"] == "step-2"));
        let request = input.iter().position(|item| item.to_string().contains("long current turn")).unwrap();
        let notice = input.iter().position(|item| item.to_string().contains("summary of earlier work in the active Turn")).unwrap();
        let latest = input.iter().position(|item| item["call_id"] == "step-2").unwrap();
        assert!(request < notice && notice < latest, "request, summary, then the retained suffix");
        assert_eq!(input[notice]["role"], "developer");
        // A response after the adoption carries the new epoch and is replayed.
        after.reply.send((200, json!({"id":"step-3","status":"completed","usage":{"input_tokens":120,"output_tokens":1},
            "output":[{"type":"reasoning","id":"rs-3","summary":[],"encrypted_content":"NEW"},
                {"type":"function_call","call_id":"step-3","name":"shell","arguments":json!({"command":"echo STEP3"}).to_string()}]}))).unwrap();
        let next = incoming(&control, &mut receive).await;
        assert_ne!(next.body["tools"], json!([]), "below the threshold: no further summary");
        let reasoning: Vec<_> = next.body["input"].as_array().unwrap().iter().filter(|item| item["type"] == "reasoning").collect();
        assert_eq!(reasoning.len(), 1);
        assert_eq!(reasoning[0]["encrypted_content"], "NEW");
        next.reply.send((200, answer("finished"))).unwrap();
        settled(&control, &target).await;
        let events = proactive_events(&control, &target).await;
        assert_eq!(events.len(), 1, "{events:#?}");
        assert_eq!(events[0]["state"], "adopted");
        let attempt_id = events[0]["attempt_id"].as_str().unwrap().to_owned();
        let (saved, epoch, executions): (String, i64, i64) = control.store.database.call(move |db| Ok((
            db.query_row("SELECT error_json FROM model_attempt WHERE id=?1", [attempt_id], |r| r.get(0))?,
            db.query_row("SELECT reasoning_epoch FROM thread", [], |r| r.get(0))?,
            db.query_row("SELECT count(*) FROM shell_execution", [], |r| r.get(0))?,
        ))).await.unwrap();
        let saved: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["automatic_trim_attempt"]["midturn"]["scope"], "active_turn");
        assert_eq!(epoch, 1);
        assert_eq!(executions, 4, "no saved tool is executed again");
        control.shutdown().await.unwrap(); owner.shutdown().await.unwrap(); server.abort();
    }).await.unwrap();
}

#[tokio::test]
async fn proactive_adopts_reducing_spills_when_tiny_summary_is_rejected() {
    proactive_partial_spill_fixture(false).await;
}

#[tokio::test]
async fn proactive_partial_spill_with_oversized_current_does_not_retry_summary() {
    proactive_partial_spill_fixture(true).await;
}

/// Two completed Turns (input 50 then `second`), then a third request. Returns
/// how many summary (tool-free) requests preceded the third ordinary request.
async fn proactive_third_turn(
    control: &Arc<Control>,
    receive: &mut mpsc::Receiver<Incoming>,
    thread: &str,
    second: u64,
    summary_reply: (u16, Value),
) -> usize {
    for (n, tokens) in [(0, 50), (1, second)] {
        send(control, thread, &format!("task {n}")).await;
        incoming(control, receive)
            .await
            .reply
            .send((200, answer_with_usage("saved answer", tokens)))
            .unwrap();
        settled(control, thread).await;
    }
    send(control, thread, "current task").await;
    let mut summaries = 0;
    loop {
        let next = incoming(control, receive).await;
        if next.body["tools"] == json!([]) {
            summaries += 1;
            assert!(summaries <= 4, "bounded summary requests");
            next.reply.send(summary_reply.clone()).unwrap();
            continue;
        }
        let context = current_context(control, thread).await;
        assert_current_context_once(&next.body, &context, "after proactive summary");
        next.reply
            .send((200, answer_with_usage("last answer", 10)))
            .unwrap();
        break;
    }
    settled(control, thread).await;
    summaries
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_threshold_compacts_once_at_the_next_boundary() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let summaries = proactive_third_turn(
        &control,
        &mut receive,
        &thread,
        150,
        (200, {
            let mut reply = answer("## 目的\nSummarized earlier turns");
            reply["usage"] = json!({"input_tokens":120,"input_tokens_details":{"cached_tokens":30,"cache_creation_tokens":4},"output_tokens":6});
            reply
        }),
    )
    .await;
    assert_eq!(summaries, 1);
    assert!(
        receive.try_recv().is_err(),
        "no further request after settling"
    );
    let events = proactive_events(&control, &thread).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["state"], "adopted");
    assert_eq!(events[0]["kind"], "automatic_reduction");
    assert_eq!(events[0]["metrics"]["reason"], "proactive_threshold");
    assert_eq!(events[0]["trigger"]["auto_at_tokens"], 100);
    assert_eq!(events[0]["trigger"]["input_tokens"], 150);
    assert!(events[0]["metrics"]["summary_node_id"].is_string());
    // #133: the summary request's reported usage, per event and in usage get.
    let usage = &events[0]["metrics"]["summary_usage"];
    assert_eq!(usage["requests"], 1, "{usage}");
    assert_eq!(usage["input_tokens"]["known_sum"], 120);
    assert_eq!(usage["output_tokens"]["known_sum"], 6);
    assert_eq!(usage["cached_tokens"]["known_sum"], 30);
    assert_eq!(usage["cache_creation_tokens"]["known_sum"], 4);
    let totals = request(
        &control,
        Command::UsageGet(UsageGetArgs {
            thread_id: Some(thread.clone()),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(totals["coverage"]["summary"], "recorded");
    assert_eq!(totals["compaction"]["requests"], 1);
    assert_eq!(totals["compaction"]["input_tokens"]["known_sum"], 120);
    assert_eq!(totals["compaction"]["output_tokens"]["known_sum"], 6);
    // Conversation attempts (inputs 50, 150, 10) exclude the summary request.
    assert_eq!(
        totals["model_attempts"]["completed"]["input_tokens"]["known_sum"],
        210
    );
    let summary_nodes = control
        .store
        .database
        .call(|db| {
            Ok(
                db.query_row("SELECT count(*) FROM node WHERE kind='summary'", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(summary_nodes, 1);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_threshold_does_not_fire_below_threshold_or_when_disabled() {
    for (extra, second) in [
        // Below the threshold.
        ("[compaction]\nauto_at_tokens=100\n", 99),
        // Global 0 disables.
        ("[compaction]\nauto_at_tokens=0\n", 150),
        // Per-connection 0 overrides an enabled global threshold.
        ("auto_at_tokens=0\n[compaction]\nauto_at_tokens=100\n", 150),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = proactive_owner(root.path(), &url, extra).await;
        let control = Control::new(&owner, root.path().into());
        let thread = thread(&control).await;
        let summaries =
            proactive_third_turn(&control, &mut receive, &thread, second, (500, json!({}))).await;
        assert_eq!(summaries, 0, "{extra}");
        assert!(
            proactive_events(&control, &thread).await.is_empty(),
            "{extra}"
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
        let below = log
            .lines()
            .filter(|line| line.contains("\"event\":\"automatic_compaction_below_threshold\""))
            .count();
        if second == 99 {
            assert!(
                (1..=2).contains(&below),
                "below threshold must be observable once per Turn: {below}"
            );
        } else {
            assert_eq!(
                below, 0,
                "disabled compaction is not a measured below-threshold event"
            );
        }
        assert!(!log.contains("saved answer"));
        assert!(!log.contains("current task"));
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_summary_failure_does_not_retry_for_token_growth_alone() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    for (n, tokens) in [(0, 50), (1, 150)] {
        send(&control, &thread, &format!("task {n}")).await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("saved answer", tokens)))
            .unwrap();
        settled(&control, &thread).await;
    }
    send(&control, &thread, "current task").await;
    let mut summaries = 0;
    // A large token increase in the current protected Turn is not a new
    // summarizable source and must not restart the rejected attempt.
    let mut ordinary = vec![answer_with_usage("done", 150), {
        let mut body = json!({"id":"tool","status":"completed","output":[{"type":"function_call","call_id":"proactive-tool","name":"shell","arguments":json!({"command":"echo synthetic"}).to_string()}]});
        body["usage"] = json!({"input_tokens":9999,"output_tokens":1});
        body
    }];
    while let Some(reply) = ordinary.last().cloned() {
        let next = incoming(&control, &mut receive).await;
        if next.body["tools"] == json!([]) {
            summaries += 1;
            assert!(summaries <= 4, "bounded summary requests");
            next.reply
                .send((400, json!({"error":{"code":"invalid_request_error","message":"fixture summary failure"}})))
                .unwrap();
            continue;
        }
        ordinary.pop();
        next.reply.send((200, reply)).unwrap();
    }
    settled(&control, &thread).await;
    assert_eq!(
        summaries, 1,
        "token growth alone must not retry the same source"
    );
    assert!(receive.try_recv().is_err());
    let events = proactive_events(&control, &thread).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["state"], "rejected");
    assert_eq!(events[0]["trigger"]["input_tokens"], 150);
    let usage = &events[0]["metrics"]["summary_usage"];
    assert_eq!(usage["requests"], 1);
    assert_eq!(usage["input_tokens"]["unknown_count"], 1);
    assert_eq!(usage["output_tokens"]["unknown_count"], 1);
    let (summary_nodes, turn_state) = control
        .store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM node WHERE kind='summary'", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row(
                    "SELECT state FROM turn ORDER BY rowid DESC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(summary_nodes, 0);
    assert_eq!(
        turn_state, "completed",
        "a failed proactive attempt never blocks the Turn"
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_retries_when_a_new_older_turn_becomes_eligible() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    for (n, tokens) in [(0, 50), (1, 150)] {
        send(&control, &thread, &format!("task {n}")).await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("saved answer", tokens)))
            .unwrap();
        settled(&control, &thread).await;
    }
    send(&control, &thread, "third task").await;
    let first = incoming(&control, &mut receive).await;
    assert_eq!(first.body["tools"], json!([]));
    first
        .reply
        .send((
            400,
            json!({"error":{"code":"invalid_request_error","message":"synthetic rejection"}}),
        ))
        .unwrap();
    let ordinary = incoming(&control, &mut receive).await;
    assert_ne!(ordinary.body["tools"], json!([]));
    ordinary
        .reply
        .send((200, answer_with_usage("third answer", 150)))
        .unwrap();
    settled(&control, &thread).await;
    send(&control, &thread, "fourth task").await;
    let second = incoming(&control, &mut receive).await;
    assert_eq!(
        second.body["tools"],
        json!([]),
        "the newly unprotected older Turn is eligible"
    );
    second.reply.send((200, answer("short summary"))).unwrap();
    let ordinary = incoming(&control, &mut receive).await;
    assert_ne!(ordinary.body["tools"], json!([]));
    ordinary
        .reply
        .send((200, answer_with_usage("finished", 10)))
        .unwrap();
    settled(&control, &thread).await;
    let events = proactive_events(&control, &thread).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| event["state"] == "rejected")
            .count(),
        1,
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["state"] == "adopted")
            .count(),
        1,
        "{events:?}"
    );
    assert!(receive.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_resumes_when_an_older_complete_tool_group_becomes_reducible() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "only completed task").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer_with_usage("saved", 150)))
        .unwrap();
    settled(&control, &thread).await;
    send(&control, &thread, "current task").await;
    let command = if cfg!(windows) {
        "powershell -NoProfile -Command \"[Console]::Write(('x' * 50000))\""
    } else {
        "head -c 50000 /dev/zero | tr '\\0' x"
    };
    for index in 0..2 {
        let request = incoming(&control, &mut receive).await;
        assert_ne!(
            request.body["tools"],
            json!([]),
            "there is no older Turn eligible for a summary"
        );
        request.reply.send((200, json!({"id":format!("response-{index}"),"status":"completed","usage":{"input_tokens":200,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("call-{index}"),"name":"shell","arguments":json!({"command":command}).to_string()}]}))).unwrap();
    }
    let mut next = incoming(&control, &mut receive).await;
    if next.body["tools"] == json!([]) {
        let input = next.body["input"].to_string();
        assert!(input.contains("call-0"));
        assert!(
            !input.contains("call-1"),
            "the latest group stays protected"
        );
        next.reply
            .send((200, answer("short middle summary")))
            .unwrap();
        next = incoming(&control, &mut receive).await;
    }
    assert_ne!(
        next.body["tools"],
        json!([]),
        "ordinary work continues after compaction"
    );
    assert!(
        next.body["input"].to_string().contains("call-1"),
        "the newest group must remain replayable"
    );
    next.reply
        .send((200, answer_with_usage("done", 10)))
        .unwrap();
    settled(&control, &thread).await;
    let events = proactive_events(&control, &thread).await;
    assert_eq!(
        events.len(),
        1,
        "one actual attempt follows the new eligible group: {events:?}"
    );
    assert_eq!(events[0]["state"], "adopted");
    assert!(events[0]["metrics"]["spilled_groups"].as_u64().unwrap_or(0) >= 1);
    assert!(receive.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_large_protected_turn_uses_bounded_no_target_proof() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(
        root.path(),
        &url,
        "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=100\n",
    )
    .await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "one protected completed turn").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer_with_usage("saved", 150)))
        .unwrap();
    settled(&control, &thread).await;
    let protected = "synthetic protected text ".repeat(48_000);
    assert!(protected.len() > 1024 * 1024);
    send(&control, &thread, &protected).await;
    let next = incoming(&control, &mut receive).await;
    assert_ne!(
        next.body["tools"],
        json!([]),
        "a large protected message is not a summary request"
    );
    let id = thread.clone();
    let projected = control
        .store
        .database
        .call(move |db| shellboy::frontend::activity::project(db, &id))
        .await
        .unwrap();
    assert_eq!(
        projected.snapshot.automatic_compaction_skip,
        Some(shellboy::ui::activity::AutomaticCompactionSkip::NoTarget)
    );
    assert!(projected.snapshot.automatic_compaction.is_none());
    next.reply
        .send((200, answer_with_usage("done", 10)))
        .unwrap();
    settled(&control, &thread).await;
    assert!(
        proactive_events(&control, &thread).await.is_empty(),
        "proof must precede any compaction attempt"
    );
    assert!(receive.try_recv().is_err());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_no_safe_reduction_is_not_started_or_retried() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "only completed task").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer_with_usage("saved", 150)))
        .unwrap();
    settled(&control, &thread).await;
    send(&control, &thread, "current task").await;
    // The only completed Turn and the latest complete tool group remain
    // protected. One tool call is a hard cap, not a usage-dependent loop.
    let next = incoming(&control, &mut receive).await;
    assert_ne!(next.body["tools"], json!([]));
    next.reply.send((200, json!({"id":"single-tool","status":"completed","usage":{"input_tokens":9999,"output_tokens":1},"output":[{"type":"function_call","call_id":"single-tool","name":"shell","arguments":json!({"command":"echo synthetic"}).to_string()}]}))).unwrap();
    let next = incoming(&control, &mut receive).await;
    assert_ne!(
        next.body["tools"],
        json!([]),
        "no summary source is eligible"
    );
    let projected_thread = thread.clone();
    let projected = control
        .store
        .database
        .call(move |db| shellboy::frontend::activity::project(db, &projected_thread))
        .await
        .unwrap();
    assert_eq!(
        projected.snapshot.automatic_compaction_skip,
        Some(shellboy::ui::activity::AutomaticCompactionSkip::NoTarget)
    );
    assert!(
        projected.snapshot.automatic_compaction.is_none(),
        "skipped work must not appear active"
    );
    next.reply
        .send((200, answer_with_usage("done", 10)))
        .unwrap();
    settled(&control, &thread).await;
    let events = proactive_events(&control, &thread).await;
    assert!(events.is_empty(), "no compaction was started: {events:?}");
    let key = format!("compact.automatic_guard.{thread}");
    let saved: String = control
        .store
        .database
        .call(move |db| {
            Ok(
                db.query_row("SELECT value FROM app_meta WHERE key=?1", [key], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["reason"], "no_target");
    assert!(saved["marks"].as_array().unwrap().is_empty());
    assert_eq!(saved["invalid_state"], 0);
    assert_eq!(saved["other_rejections"], 0);
    assert!(!saved.to_string().contains("only completed task"));
    assert!(receive.try_recv().is_err());
    request(
        &control,
        Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: thread.clone(),
            descendants: false,
            preview_marker: None,
        }),
    )
    .await;
    let keys = [
        format!("compact.automatic_guard.{thread}"),
        format!("compact.automatic_status.{thread}"),
    ];
    let remaining = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM app_meta WHERE key IN (?1,?2)",
                [&keys[0], &keys[1]],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        remaining, 0,
        "Thread deletion removes both bounded witnesses"
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_filter_terminal_preserves_facts_without_automatic_retry() {
    for streaming in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy_with_sse(streaming).await;
        let owner = owner_with_retries(root.path(), &url, 8).await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        send(&control, &target, "synthetic content filter fixture").await;
        let response = json!({
            "id": "filtered-fixture",
            "status": "incomplete",
            "incomplete_details": {"reason": "content_filter"},
            "content_filter_results": {
                "protected_material_code": {"filtered": true, "detected": true},
                "violence": {"filtered": false, "severity": "safe"}
            },
            "output": []
        });
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, response.clone()))
            .unwrap();
        settled(&control, &target).await;
        control.store.database.call(move |db| {
            let (state, attempts, error): (String, i64, String) = db.query_row(
                "SELECT t.state,(SELECT count(*) FROM model_attempt WHERE turn_id=t.id),a.error_json FROM turn t JOIN model_attempt a ON a.turn_id=t.id WHERE t.thread_id=?1",
                [target], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(state, "recovery_wait");
            assert_eq!(attempts, 1, "terminal incomplete is not an interrupted transport");
            let error: Value = serde_json::from_str(&error).unwrap();
            assert_eq!(error["raw_response"], response);
            assert!(error.get("automatic_retry").is_none());
            assert!(error.get("automatic_retry_exhausted").is_none());
            Ok(())
        }).await.unwrap();
        assert!(
            receive.try_recv().is_err(),
            "no second POST with retry budget enabled"
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_filter_recovery_readback_and_real_ui_are_bilingual_and_keep_unknowns() {
    use ratatui::{Terminal, backend::TestBackend};
    use shellboy::config::UiLanguage;
    use shellboy::ui::UiState;
    for (language, name) in [(UiLanguage::En, "en"), (UiLanguage::Ja, "ja")] {
        for streaming in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (url, mut receive, server) = proxy_with_sse(streaming).await;
            let config = root.path().join("config.toml");
            std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=8\nresponse_retry_max_delay_ms=10\n[ui]\nlanguage={name:?}\n")).unwrap();
            let owner = DataOwner::start(
                Some(config),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            for verdicts in [true, false] {
                let target = thread(&control).await;
                let accepted: MessageAccepted = serde_json::from_value(
                    request(
                        &control,
                        Command::MessageSend(MessageSendArgs {
                            sender_label: None,
                            deliver: Deliver::Next,
                            thread_id: Some(target.clone()),
                            text: "synthetic filter recovery".into(),
                        }),
                    )
                    .await,
                )
                .unwrap();
                let mut response = json!({"id":"filtered-fixture","status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[]});
                if verdicts {
                    response["content_filter_results"] = json!({"protected_material_code":{"filtered":true,"detected":true},"provider_future_category":{"filtered":false,"severity":"future-value"}});
                }
                incoming(&control, &mut receive)
                    .await
                    .reply
                    .send((200, response))
                    .unwrap();
                settled(&control, &target).await;
                let turn_id = accepted.turn_id.unwrap();
                let run_id = accepted.run_id.unwrap();
                let turn = request(&control, Command::TurnGet(TurnTarget { turn_id })).await;
                let run = request(&control, Command::RunGet(RunTarget { run_id })).await;
                assert_eq!(turn["state"], "recovery_wait");
                assert_eq!(turn["recovery_hint"], run["recovery_hint"]);
                for readback in [turn, run] {
                    let hint = readback["recovery_hint"].as_str().unwrap();
                    let error: ApiError = serde_json::from_str(hint).unwrap();
                    let facts: shellboy::model::content_filter::ContentFilterFacts =
                        serde_json::from_value(error.details.unwrap()["content_filter"].clone())
                            .unwrap();
                    assert!(facts.is_content_filter());
                    assert_eq!(facts.facts.is_empty(), !verdicts);
                    let text = shellboy::presentation::content_filter::details(&facts, language)
                        .join("\n");
                    if verdicts {
                        assert!(text.contains("protected_material_code/filtered=true"));
                        assert!(
                            text.contains("provider_future_category/severity=\"future-value\"")
                        );
                    }
                    assert!(text.contains(match language {
                        UiLanguage::Ja => "不明",
                        UiLanguage::En => "Unknown",
                    }));
                    for next in ["/retry", "/detach", "cp", "curl"] {
                        assert!(text.contains(next));
                        assert!(hint.contains(next));
                    }
                }
                let projection = shellboy::frontend::activity::read(&control, &target)
                    .await
                    .unwrap();
                assert_eq!(
                    projection.snapshot.phase,
                    shellboy::activity::ActivityPhase::Recovery
                );
                assert!(
                    projection
                        .snapshot
                        .recovery
                        .as_ref()
                        .unwrap()
                        .content_filter
                        .is_some()
                );
                for width in [80, 120] {
                    let mut ui = UiState::new(&control.store.config, root.path().into());
                    ui.select_thread(target.clone());
                    ui.apply(shellboy::ui::UiUpdate::Activity {
                        thread_id: target.clone(),
                        generation: ui.view().generation,
                        epoch: 1,
                        observed_at: projection.observed_at,
                        snapshot: projection.snapshot.clone(),
                    });
                    let mut terminal = Terminal::new(TestBackend::new(width, 28)).unwrap();
                    terminal
                        .draw(|frame| shellboy::ui::draw(frame, &mut ui))
                        .unwrap();
                    let screen: String = terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect();
                    let compact: String = screen.chars().filter(|ch| !ch.is_whitespace()).collect();
                    for expected in ["content_filter", "/retry", "/detach", "cp", "curl"] {
                        assert!(
                            compact.contains(expected),
                            "{name}/{streaming}/{width}: {screen}"
                        );
                    }
                    if verdicts {
                        assert!(compact.contains("protected_material_code"));
                    } else {
                        assert!(
                            compact.contains(match language {
                                UiLanguage::Ja => "不明",
                                UiLanguage::En => "Unknown",
                            }),
                            "{name}/{streaming}/{width}: {screen}"
                        );
                    }
                }
                assert!(
                    receive.try_recv().is_err(),
                    "no automatic retry for terminal content_filter"
                );
            }
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
            server.abort();
        }
    }
}

// #183: the per-Turn `Shellboy context v1` item is part of the saved history,
// so each request of a Turn is a strict append of the previous request.
fn context_lines(body: &Value) -> Vec<String> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .filter(|text| text.starts_with("Shellboy context v1:"))
        .map(str::to_owned)
        .collect()
}
/// The context line for the Thread's current settings and latest Turn.
async fn current_context(control: &Control, thread: &str) -> String {
    let thread = thread.to_owned();
    control.store.database.call(move |db| Ok(db.query_row(
        "SELECT t.id,u.run_id,u.id,t.cwd,t.profile_id,t.model_id,t.effort,COALESCE(s.source_path,t.agents_path) FROM thread t JOIN turn u ON u.thread_id=t.id LEFT JOIN instruction_snapshot s ON s.id=u.instruction_snapshot_id WHERE t.id=?1 ORDER BY u.rowid DESC LIMIT 1",
        [thread],
        |r| Ok(format!(
            "Shellboy context v1: thread={} run={} turn={} cwd={} connection={} model={} effort={:?}; AGENTS reference={}",
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
            r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, String>(7)?,
        )),
    )?)).await.unwrap()
}
/// The current context is present exactly once and is the latest context line.
#[track_caller]
fn assert_current_context_once(body: &Value, expected: &str, label: &str) {
    let lines = context_lines(body);
    assert_eq!(
        lines.iter().filter(|line| *line == expected).count(),
        1,
        "{label}: current context must appear exactly once; expected {expected:?}; got {lines:#?}"
    );
    assert_eq!(lines.last().map(String::as_str), Some(expected), "{label}");
}
#[track_caller]
fn assert_strict_append(previous: &Value, next: &Value, label: &str) {
    let before = previous["input"].as_array().unwrap();
    let after = next["input"].as_array().unwrap();
    let common = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let show = |items: &[Value], index: usize| {
        items.get(index).map_or_else(
            || "<none>".to_owned(),
            |item| item.to_string().chars().take(160).collect(),
        )
    };
    assert!(
        common == before.len() && after.len() > before.len(),
        "{label}: request input is not a strict append: previous {} items, next {} items, first difference at index {common}; previous[{common}]={}; next[{common}]={}",
        before.len(),
        after.len(),
        show(before, common),
        show(after, common),
    );
    assert_eq!(previous["tools"], next["tools"], "{label}: tools");
}
fn tool_call(id: &str, command: &str) -> Value {
    json!({"id":format!("response-{id}"),"status":"completed","output":[{"type":"function_call","call_id":id,"name":"shell","arguments":json!({"command":command}).to_string()}]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_requests_strictly_append_and_mid_turn_changes_are_appended() {
    let root = tempfile::tempdir().unwrap();
    let moved = root.path().join("moved");
    std::fs::create_dir(&moved).unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner_with_retries(root.path(), &url, 1).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "append task").await;
    let first = incoming(&control, &mut receive).await;
    let context = current_context(&control, &thread).await;
    assert_current_context_once(&first.body, &context, "first request");
    first
        .reply
        .send((200, tool_call("one", "echo one")))
        .unwrap();
    let second = incoming(&control, &mut receive).await;
    assert_strict_append(&first.body, &second.body, "same-Turn tool loop");
    assert_current_context_once(&second.body, &context, "same-Turn tool loop");
    // cwd and effort change while the second request is in flight.
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            cwd: Some(moved.to_str().unwrap().into()),
            effort: Some("high".into()),
            ..Default::default()
        }),
    )
    .await;
    second
        .reply
        .send((200, tool_call("two", "echo two")))
        .unwrap();
    let third = incoming(&control, &mut receive).await;
    assert_strict_append(&second.body, &third.body, "mid-Turn cwd/effort change");
    let changed = current_context(&control, &thread).await;
    assert!(
        changed.contains(&format!("cwd={} ", moved.display())),
        "{changed}"
    );
    assert!(changed.contains("effort=Some(\"high\")"), "{changed}");
    assert_current_context_once(&third.body, &changed, "mid-Turn cwd/effort change");
    assert_eq!(third.body["reasoning"]["effort"], "high");
    let suffix =
        &third.body["input"].as_array().unwrap()[second.body["input"].as_array().unwrap().len()..];
    assert!(
        suffix
            .iter()
            .any(|item| item.to_string().contains("thread_settings")),
        "the change event is appended with the new context: {suffix:#?}"
    );
    // A model change within the same Turn is also announced by appending.
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            model: Some("fixture-next".into()),
            ..Default::default()
        }),
    )
    .await;
    third
        .reply
        .send((200, tool_call("three", "echo three")))
        .unwrap();
    let fourth = incoming(&control, &mut receive).await;
    assert_eq!(fourth.body["model"], "fixture-next");
    assert_strict_append(&third.body, &fourth.body, "mid-Turn model change");
    let model_changed = current_context(&control, &thread).await;
    assert!(
        model_changed.contains("model=fixture-next "),
        "{model_changed}"
    );
    assert_current_context_once(&fourth.body, &model_changed, "mid-Turn model change");
    fourth.reply.send((200, answer("done"))).unwrap();
    settled(&control, &thread).await;
    // The next Turn appends after the completed one, with its own context.
    send(&control, &thread, "next turn").await;
    let fifth = incoming(&control, &mut receive).await;
    assert_strict_append(&fourth.body, &fifth.body, "next Turn");
    let next_turn = current_context(&control, &thread).await;
    assert_ne!(next_turn, model_changed);
    assert_current_context_once(&fifth.body, &next_turn, "next Turn");
    // An automatic retry resends exactly the same input.
    fifth
        .reply
        .send((
            500,
            json!({"error":{"code":"server_error","message":"fixture"}}),
        ))
        .unwrap();
    let retried = incoming(&control, &mut receive).await;
    assert_eq!(
        retried.body["input"], fifth.body["input"],
        "same-Turn retry"
    );
    retried.reply.send((200, answer("finished"))).unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mid_turn_omission_reemits_the_current_context_once() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "omission task").await;
    let mut current = incoming(&control, &mut receive).await;
    for call in ["one", "two"] {
        current
            .reply
            .send((200, tool_call(call, &format!("echo {call}"))))
            .unwrap();
        current = incoming(&control, &mut receive).await;
    }
    let context = current_context(&control, &thread).await;
    assert_current_context_once(&current.body, &context, "before omission");
    // Queued while the request is in flight; applied at the next boundary,
    // before the following request. It omits the Turn's saved context node.
    let queued = request(
        &control,
        Command::ContextEdit(ContextEditArgs {
            thread_id: Some(thread.clone()),
            action: ContextEditAction::Maximal,
            from_node_id: None,
            to_node_id: None,
            max_chars: None,
            max_percent: None,
        }),
    )
    .await;
    assert_eq!(queued["status"], "queued", "{queued}");
    current
        .reply
        .send((200, tool_call("three", "echo three")))
        .unwrap();
    let after = incoming(&control, &mut receive).await;
    assert!(
        after.body["input"]
            .to_string()
            .contains("was omitted after"),
        "the omission is adopted before this request: {}",
        after.body["input"]
    );
    assert_current_context_once(&after.body, &context, "after omission");
    // The re-emitted line is at the tail, after the protected latest group.
    let input = after.body["input"].as_array().unwrap();
    assert_eq!(
        input.last().unwrap()["content"][0]["text"],
        context.as_str()
    );
    after
        .reply
        .send((200, tool_call("four", "echo four")))
        .unwrap();
    let next = incoming(&control, &mut receive).await;
    assert_strict_append(&after.body, &next.body, "after omission");
    assert_current_context_once(&next.body, &context, "request after the re-emission");
    next.reply.send((200, answer("done"))).unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
