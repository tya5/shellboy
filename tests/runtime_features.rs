//! Cross-service operation boundaries using a real owner and loopback proxy.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, attachments::AttachmentInput, control::Control, protocol::*};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::timeout,
};
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
    let config = root.join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
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
async fn incoming(receive: &mut mpsc::Receiver<Incoming>) -> Incoming {
    timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn settled(control: &Control, thread: &str) {
    let target = thread.to_owned();
    timeout(Duration::from_secs(5),async{loop{let target=target.clone();if control.store.database.call(move|db|Ok(db.query_row("SELECT NOT EXISTS(SELECT 1 FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting'))",[target],|r|r.get::<_,bool>(0))?)).await.unwrap(){break;}tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
}
async fn thread(control: &Arc<Control>) -> String {
    request(
        control,
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
        .into()
}
async fn send(control: &Arc<Control>, thread: &str, text: &str) {
    request(
        control,
        Command::MessageSend(MessageSendArgs {
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
    let retry = incoming(&mut receive).await;
    let messages: Vec<&Value> = retry.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user" && item["content"][0]["text"] == original)
        .collect();
    assert_eq!(messages.len(), 1, "retry must reuse the accepted original");
    assert!(
        retry
            .body
            .to_string()
            .contains("valid repaired instructions")
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
    let first = incoming(&mut receive).await;
    let text = first.body.to_string();
    assert!(text.contains("input_file"));
    assert!(text.contains("Zmlyc3QtY29udGVudA=="));
    first.reply.send((200, answer("first read"))).unwrap();
    settled(&control, &thread).await;
    let provenance=control.store.database.call(|db|Ok(db.query_row("SELECT count(*) FROM attachment_ref a JOIN receipt r ON r.message_node_id=a.source_node_id",[],|r|r.get::<_,i64>(0))?)).await.unwrap();
    assert_eq!(provenance, 1);
    std::fs::write(&file, b"second-content-longer").unwrap();
    send(&control, &thread, "read again").await;
    let second = incoming(&mut receive).await;
    let text = second.body.to_string();
    assert!(text.contains("c2Vjb25kLWNvbnRlbnQtbG9uZ2Vy"));
    assert!(!text.contains("Zmlyc3QtY29udGVudA=="));
    second.reply.send((200, answer("changed"))).unwrap();
    settled(&control, &thread).await;
    std::fs::remove_file(&file).unwrap();
    send(&control, &thread, "missing now").await;
    let third = incoming(&mut receive).await;
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
        incoming(&mut receive)
            .await
            .reply
            .send((200, answer("saved answer")))
            .unwrap();
        settled(&control, &thread).await;
    }
    send(&control, &thread, "current task").await;
    let ordinary = incoming(&mut receive).await;
    assert_eq!(ordinary.session.as_deref(), Some(thread.as_str()));
    assert!(ordinary.body.get("prompt_cache_key").is_none());
    let accepted = timeout(
        Duration::from_millis(500),
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
    assert!(
        timeout(Duration::from_millis(30), receive.recv())
            .await
            .is_err()
    );
    ordinary.reply.send((200, answer("last answer"))).unwrap();
    let summary = incoming(&mut receive).await;
    assert_eq!(summary.session.as_deref(), Some(thread.as_str()));
    assert!(summary.body.get("prompt_cache_key").is_none());
    assert_eq!(summary.body["tools"], json!([]));
    assert_eq!(summary.body["store"], false);
    summary
        .reply
        .send((200, answer("## 目的\nSummarized earlier turns")))
        .unwrap();
    timeout(Duration::from_secs(5), async {
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
async fn clear_overflow_without_spill_skips_first_retry_summarizes_once_then_stops_at_second_failure()
 {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    for n in 0..2 {
        send(&control, &thread, &format!("previous task {n}")).await;
        incoming(&mut receive)
            .await
            .reply
            .send((200, answer("saved")))
            .unwrap();
        settled(&control, &thread).await;
    }
    send(&control, &thread, "overflowing task").await;
    let overflow =
        json!({"error":{"code":"context_length_exceeded","message":"fixture context exceeded"}});
    incoming(&mut receive)
        .await
        .reply
        .send((400, overflow.clone()))
        .unwrap();
    let summary = incoming(&mut receive).await;
    assert_eq!(summary.body["tools"], json!([]));
    assert_eq!(
        control.replay_cache_stats().pinned_bytes,
        0,
        "failed HTTP input must be unpinned before waiting for compaction"
    );
    summary.reply.send((200, answer("small summary"))).unwrap();
    let retry = incoming(&mut receive).await;
    assert_eq!(retry.body["tools"].as_array().unwrap().len(), 1);
    retry.reply.send((400, overflow)).unwrap();
    settled(&control, &thread).await;
    assert!(
        timeout(Duration::from_millis(60), receive.recv())
            .await
            .is_err()
    );
    let state = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT state FROM turn ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(state, "recovery_wait");
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
    let first = incoming(&mut receive).await;
    let command = if cfg!(windows) {
        format!(
            "Set-Content -LiteralPath '{}' -Value unexpected",
            marker.display()
        )
    } else {
        format!("touch '{}'", marker.display())
    };
    first.reply.send((200,json!({"id":"badgroup","status":"completed","output":[{"type":"function_call","call_id":"good","name":"shell","arguments":json!({"command":command}).to_string()},{"type":"function_call","call_id":"bad","name":"shell","arguments":"broken-json"}]}))).unwrap();
    let second = incoming(&mut receive).await;
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
        let seconds = if cfg!(windows) { 15 } else { 5 };
        match timeout(Duration::from_secs(seconds), receive.recv()).await {
            Ok(Some(request)) => request,
            failed => {
                let diagnostic = timeout(Duration::from_secs(1), control.store.database.call(|db| {
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
    let owner = owner(root.path(), &url).await;
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
        request.reply.send((200,json!({"id":format!("group-{ordinal}"),"status":"completed","output":[{"type":"function_call","call_id":format!("call-{ordinal}"),"name":"shell","arguments":json!({"command":format!("echo raw-group-{ordinal}")}).to_string()}]}))).unwrap();
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
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0]["omitted"], true);
    assert_eq!(outputs[1]["omitted"], true);
    assert!(
        outputs[2]["output"]
            .as_str()
            .unwrap()
            .contains("raw-group-2")
    );
    retry_one.reply.send((400, overflow.clone())).unwrap();
    let summary = next(&mut receive, &control, "second overflow: summary request").await;
    assert_eq!(summary.body["tools"], json!([]));
    summary
        .reply
        .send((
            200,
            answer("Earlier groups completed; exact references remain."),
        ))
        .unwrap();
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
    // With no completed Turn to release, make minimum explicit-overflow
    // progress by releasing the oldest current complete tool group;
    // the two newer saved groups remain, rather than summarizing all safe ones.
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0]["call_id"], "call-1");
    assert_eq!(outputs[1]["call_id"], "call-2");
    assert!(outputs.iter().all(|output| output["omitted"] == true));
    retry_two.reply.send((400, overflow)).unwrap();
    settled(&control, &thread).await;
    assert!(
        timeout(Duration::from_millis(60), receive.recv())
            .await
            .is_err()
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
    assert_eq!(counts, (3, 3, 3));
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnecting_a_send_observer_does_not_lose_commit_effects_or_race_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let (url, _receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let (blocked, ready) = oneshot::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let database = control.store.database.clone();
    let blocker = tokio::spawn(async move {
        database
            .call(move |_| {
                let _ = blocked.send(());
                wait_release.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let operation = format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4());
    let operation_copy = operation.clone();
    let sender = control.clone();
    let mut observer = tokio::spawn(async move {
        sender
            .request(Request::new(
                operation_copy,
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some(thread),
                    text: "accepted despite observer cancellation".into(),
                }),
            ))
            .await
    });
    assert!(
        timeout(Duration::from_millis(30), &mut observer)
            .await
            .is_err()
    );
    observer.abort();
    let stopping = control.clone();
    let mut shutdown = tokio::spawn(async move { stopping.shutdown().await });
    assert!(
        timeout(Duration::from_millis(20), &mut shutdown)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    blocker.await.unwrap();
    timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let result = control
        .store
        .database
        .call(move |db| {
            Ok((
                db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id=?1",
                    [operation],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM run WHERE state IN ('live','stopping')",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM turn WHERE state IN ('preparing','running','waiting')",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(result, (1, 0, 0));
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
        incoming(&mut receive)
            .await
            .reply
            .send((200, answer("saved")))
            .unwrap();
        settled(&control, &thread).await;
    }
    send(&control, &thread, "current recovery").await;
    incoming(&mut receive)
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
    let summary = incoming(&mut receive).await;
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
    timeout(Duration::from_secs(5), async {
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
            thread_id: Some(thread.clone()),
            text: "old original request".into(),
        }),
    )
    .await;
    incoming(&mut receive)
        .await
        .reply
        .send((200, answer("old original answer")))
        .unwrap();
    settled(&control, &thread).await;
    timeout(Duration::from_secs(5),async{loop{let target=thread.clone();if control.store.database.call(move|db|Ok(db.query_row("SELECT NOT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state IN ('live','stopping'))",[target],|r|r.get::<_,bool>(0))?)).await.unwrap(){break;}tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
    let operation = format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4());
    let node = accepted["message_id"].as_str().unwrap().to_owned();
    let args = ThreadCreateArgs {
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
    let next = incoming(&mut receive).await;
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
            thread_id: Some(thread.clone()),
            text: "running original".into(),
        }),
    )
    .await;
    let old = incoming(&mut receive).await;
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
    let new = incoming(&mut receive).await;
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
    let first = incoming(&mut receive).await;
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
        timeout(Duration::from_millis(100), receive.recv())
            .await
            .is_err(),
        "settings must not resume recovery_wait"
    );
    let target = thread.clone();
    let (run,pending):(String,i64) = control.store.database.call(move|db|Ok((db.query_row("SELECT run_id FROM turn WHERE thread_id=?1 AND state='recovery_wait'",[&target],|r|r.get(0))?,db.query_row("SELECT count(*) FROM delivery d JOIN event e ON e.id=d.event_id WHERE e.target_thread_id=?1 AND e.kind='context_changed' AND d.state='pending'",[target],|r|r.get(0))?))).await.unwrap();
    assert!(pending >= 2);
    request(&control, Command::RunRetry(RunTarget { run_id: run })).await;
    let retry = incoming(&mut receive).await;
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
        let call = incoming(&mut receive).await;
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
    let first = incoming(&mut receive).await;
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
    let retry = incoming(&mut receive).await;
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
        timeout(Duration::from_millis(150), receive.recv())
            .await
            .is_err(),
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
    let resumed = incoming(&mut receive).await;
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
    let count_instruction = |body: &Value, expected: &str| {
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["role"] == "developer")
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter(|part| part["type"] == "input_text" && part["text"] == expected)
            .count()
    };
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some(thread.clone()),
                text: "create the per-thread instructions through shell".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let first = incoming(&mut receive).await;
    assert_eq!(count_instruction(&first.body, instruction), 0);
    let first_turn = accepted.turn_id.unwrap();
    let query = first_turn.clone();
    let absent_snapshot: String=control.store.database.call(move|db|{
        let (id,size,body):(String,Option<i64>,String)=db.query_row("SELECT s.id,s.observed_size,s.body FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[query],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert!(size.is_none());assert!(body.is_empty());Ok(id)
    }).await.unwrap();
    let command = format!("printf '%s' '{instruction}' > AGENTS.md");
    first.reply.send((200,json!({"id":"create-agents-response","status":"completed","output":[{"id":"create-agents-item","type":"function_call","call_id":"create-agents-call","name":"shell","arguments":json!({"command":command,"background":false}).to_string()}]}))).unwrap();
    let after_tool = incoming(&mut receive).await;
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
                thread_id: Some(thread.clone()),
                text: "observe the new instructions at the next Turn".into(),
            }),
        )
        .await,
    )
    .unwrap();
    assert_ne!(next.turn_id.as_deref(), Some(first_turn.as_str()));
    let next_request = incoming(&mut receive).await;
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
    let after_reopen = incoming(&mut receive).await;
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
    let pending = incoming(&mut receive).await;
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
    let next = incoming(&mut receive).await;
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
    let pending = incoming(&mut receive).await;
    let marker = root.path().join("forbidden-second-tool");
    let second = format!("printf forbidden > '{}'", marker.display());
    pending.reply.send((200,json!({"id":"two-tools","status":"completed","output":[
        {"type":"function_call","name":"shell","call_id":"first","arguments":json!({"command":"printf FIRST; read ignored","stdin_open":true}).to_string()},
        {"type":"function_call","name":"shell","call_id":"second","arguments":json!({"command":second}).to_string()}
    ]}))).unwrap();
    // Synchronize on persisted output from the first actual OS process, then
    // keep it blocked on stdin until the Quit acceptance has closed the gate.
    let execution = timeout(Duration::from_secs(5), async {
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
    let next = incoming(&mut receive).await;
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
                thread_id: Some(parent.clone()),
                text: "synthetic session parent".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let first = incoming(&mut receive).await;
    assert_eq!(first.session.as_deref(), Some(parent.as_str()));
    assert!(first.body.get("prompt_cache_key").is_none());
    first.reply.send((200, answer("parent response"))).unwrap();
    settled(&control, &parent).await;
    let child = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
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
        let actual = incoming(&mut receive).await;
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
    let sent = incoming(&mut receive).await;
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
    completed["usage"] = json!({"input_tokens":123,"output_tokens":4,"total_tokens":127});
    sent.reply.send((200, completed)).unwrap();
    settled(&control, &target).await;
    control.store.database.call(move |db| {
        let (manifest, usage): (String, String) = db.query_row(
            "SELECT input_manifest_json,usage_json FROM model_attempt WHERE state='completed' ORDER BY rowid DESC LIMIT 1", [],
            |row| Ok((row.get(0)?, row.get(1)?)))?;
        let manifest: Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["input_characters"], characters);
        assert_eq!(manifest["history_characters"], history_characters);
        assert_eq!(serde_json::from_str::<Value>(&usage).unwrap()["input_tokens"], 123);
        Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
