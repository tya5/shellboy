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
                let body = body.to_string();
                let _=stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
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
            now: false,
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
                now: false,
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
            now: false,
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
            now: false,
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
                now: false,
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
                now: false,
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
    assert_eq!(collected["raw_bytes_collected"], 2097152);
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
                now: false,
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
                now: false,
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
    let target = thread.clone();
    let run: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
                [target],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    request(
        &control,
        Command::RevisitSet(RevisitSetArgs {
            run_id: Some(run.clone()),
            text: "Do not fire after Quit admission".into(),
        }),
    )
    .await;
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
    let reservation = request(
        &control,
        Command::RevisitGet(RevisitArgs {
            run_id: Some(run.clone()),
        }),
    )
    .await;
    assert!(
        reservation["pending"].is_object(),
        "Quit admission preserves the reservation until shutdown captures it"
    );
    assert_eq!(
        reservation["consumed_count"], 0,
        "Quit admission must not fire Revisit"
    );
    let target = run.clone();
    assert_eq!(
        control
            .store
            .database
            .call(move |db| Ok(db.query_row(
                "SELECT count(*) FROM event WHERE kind='revisit' AND target_run_id=?1",
                [target],
                |r| r.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        0
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
                now: false,
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
            now: false,
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
            now: false,
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
            now: false,
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
                now: false,
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
                now: false,
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
            now: false,
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
