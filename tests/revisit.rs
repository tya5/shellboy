//! Revisit lifecycle through real Control/HTTP, plus trusted Store-boundary authority fixtures.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
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
                                serde_json::from_slice::<Value>(&bytes[i + 4..i + 4 + length])
                                    .unwrap(),
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
async fn owner(root: &std::path::Path, url: &str, limit: Option<u64>) -> DataOwner {
    let config = root.join("config.toml");
    let limits = limit
        .map(|n| format!("[limits]\nrevisit_max_per_run={n}\n"))
        .unwrap_or_default();
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\nresponse_header_timeout_ms=15000\n{limits}")).unwrap();
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

fn invocation(control: &Control, command: Command) -> Request {
    Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        command,
    )
}
fn set(run: &str, text: &str) -> Command {
    Command::RevisitSet(RevisitSetArgs {
        run_id: Some(run.into()),
        text: text.into(),
    })
}
async fn begin(control: &Arc<Control>, thread: &str) -> MessageAccepted {
    serde_json::from_value(
        request(
            control,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some(thread.into()),
                text: "One original user instruction".into(),
            }),
        )
        .await,
    )
    .unwrap()
}
async fn run_state(control: &Control, run: &str, expected: &str) {
    let run = run.to_owned();
    timeout(Duration::from_secs(10), async {
        loop {
            let id = run.clone();
            let (state, reason): (String, Option<String>) = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT state,stop_reason FROM run WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?)
                })
                .await
                .unwrap();
            if state == expected {
                break;
            }
            assert!(
                !matches!(state.as_str(), "stopped" | "failed" | "completed"),
                "Run {run} ended in {state}: {reason:?}, expected {expected}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Run lifecycle watchdog");
}
async fn saved(control: &Control, run: &str) -> Value {
    let key = format!("revisit.{run}");
    control
        .store
        .database
        .call(move |db| {
            let text: String =
                db.query_row("SELECT value FROM app_meta WHERE key=?1", [key], |r| {
                    r.get(0)
                })?;
            Ok(serde_json::from_str(&text).unwrap())
        })
        .await
        .unwrap()
}
async fn event_count(control: &Control, run: &str) -> i64 {
    let run = run.to_owned();
    control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM event WHERE kind='revisit' AND target_run_id=?1",
                [run],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_completion_delivers_once_and_allows_many_cycles_unless_explicitly_limited() {
    for (limit, cycles) in [(None, 16_u64), (Some(2), 2), (Some(0), 0)] {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = owner(root.path(), &url, limit).await;
        let control = Control::new(&owner, root.path().into());
        let thread = thread(&control).await;
        let accepted = begin(&control, &thread).await;
        let run = accepted.run_id.unwrap();
        let mut current = incoming(&mut receive).await;
        for ordinal in 0..cycles {
            let instruction = format!("REVISIT_INSTRUCTION_{ordinal}: verify remaining work");
            let registration = request(&control, set(&run, &instruction)).await;
            assert_eq!(registration["consumed_count"], ordinal);
            assert_eq!(registration["pending"]["text"], instruction);
            assert_eq!(event_count(&control, &run).await, ordinal as i64);
            current
                .reply
                .send((200, answer("Finished this Turn")))
                .unwrap();
            current = incoming(&mut receive).await;
            let inputs = current.body["input"].as_array().unwrap();
            assert!(
                inputs
                    .iter()
                    .any(|item| item["role"] == "developer"
                        && item.to_string().contains(&instruction)),
                "Revisit must be an ordinary developer event: {}",
                current.body
            );
            assert_eq!(saved(&control, &run).await["consumed_count"], ordinal + 1);
            assert!(saved(&control, &run).await["pending"].is_null());
        }
        if limit.is_some() {
            let response = control
                .request(invocation(&control, set(&run, "Over limit")))
                .await;
            assert_eq!(response.error.unwrap().code, ErrorCode::RevisitLimitReached);
        }
        current.reply.send((200, answer("Final answer"))).unwrap();
        run_state(&control, &run, "completed").await;
        assert_eq!(event_count(&control, &run).await, cycles as i64);
        assert!(
            timeout(Duration::from_millis(100), receive.recv())
                .await
                .is_err(),
            "consumed reservation must not fire again"
        );
        let id = run.clone();
        let (turns, messages): (i64, i64) = control
            .store
            .database
            .call(move |db| {
                Ok((
                    db.query_row("SELECT count(*) FROM turn WHERE run_id=?1", [&id], |r| {
                        r.get(0)
                    })?,
                    db.query_row("SELECT count(*) FROM receipt WHERE run_id=?1", [id], |r| {
                        r.get(0)
                    })?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(turns, cycles as i64 + 1);
        assert_eq!(messages, 1, "Revisit is not a synthetic user message");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_wait_preserves_reservation_and_only_normal_completion_fires_it() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url, None).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let accepted = begin(&control, &thread).await;
    let run = accepted.run_id.unwrap();
    let first = incoming(&mut receive).await;
    request(&control, set(&run, "AFTER_RECOVERY_ONLY")).await;
    first
        .reply
        .send((
            500,
            json!({"error":{"message":"synthetic provider failure","type":"server_error"}}),
        ))
        .unwrap();
    let target = run.clone();
    timeout(Duration::from_secs(10),async {loop {let id=target.clone();let recovery=control.store.database.call(move|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state='recovery_wait')",[id],|r|r.get::<_,bool>(0))?)).await.unwrap();if recovery{break;}tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
    assert_eq!(event_count(&control, &run).await, 0);
    assert_eq!(
        saved(&control, &run).await["pending"]["text"],
        "AFTER_RECOVERY_ONLY"
    );
    assert!(
        timeout(Duration::from_millis(100), receive.recv())
            .await
            .is_err()
    );
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    incoming(&mut receive)
        .await
        .reply
        .send((200, answer("Recovered")))
        .unwrap();
    let revisit = incoming(&mut receive).await;
    assert!(revisit.body.to_string().contains("AFTER_RECOVERY_ONLY"));
    revisit.reply.send((200, answer("Verified"))).unwrap();
    run_state(&control, &run, "completed").await;
    assert_eq!(event_count(&control, &run).await, 1);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_a_held_request_drops_pending_without_firing() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url, None).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let run = begin(&control, &thread).await.run_id.unwrap();
    let held = incoming(&mut receive).await;
    request(&control, set(&run, "MUST_NOT_FIRE_AFTER_STOP")).await;
    request(
        &control,
        Command::RunStop(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    run_state(&control, &run, "stopped").await;
    assert!(saved(&control, &run).await["pending"].is_null());
    assert_eq!(event_count(&control, &run).await, 0);
    let _ = held.reply.send((200, answer("Late response")));
    assert!(
        timeout(Duration::from_millis(100), receive.recv())
            .await
            .is_err()
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

async fn seed_run(control: &Arc<Control>, id: &str, owner_run: Option<&str>) -> String {
    let thread = thread(control).await;
    let t = thread.clone();
    let id = id.to_owned();
    let owner = owner_run.map(str::to_owned);
    control.store.database.call(move|db| {
        db.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES(?1,?2,?3,'live','2026-10-01T00:00:00Z')",rusqlite::params![id,t,owner])?;
        Ok(())
    }).await.unwrap();
    thread
}
fn trusted_origin(control: &Control, run: &str, thread: &str) -> Origin {
    Origin {
        instance_id: control.store.instance_id.clone(),
        thread_id: thread.into(),
        run_id: run.into(),
        turn_id: "trusted-store-test-turn".into(),
        execution_id: "trusted-store-test-shell".into(),
        token: "not-an-issued-token".into(),
    }
}
async fn store_request(
    control: &Control,
    command: Command,
    caller: Option<Origin>,
) -> Result<Value, ApiError> {
    control
        .store
        .execute(invocation(control, command), caller)
        .await
        .map(|result| result.result)
}

#[tokio::test]
async fn trusted_store_authority_replacement_receipts_and_read_only_get_are_consistent() {
    // Store accepts an already authenticated caller. The separate Control check
    // below proves that passing this unissued token through RPC is rejected.
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path(), "http://127.0.0.1:9/v1", None).await;
    let control = Control::new(&owner, root.path().into());
    let parent_thread = seed_run(&control, "parent", None).await;
    let child_thread = seed_run(&control, "child", Some("parent")).await;
    seed_run(&control, "grandchild", Some("child")).await;
    seed_run(&control, "unrelated", None).await;
    let parent = trusted_origin(&control, "parent", &parent_thread);
    let child = trusted_origin(&control, "child", &child_thread);
    let own = store_request(
        &control,
        Command::RevisitSet(RevisitSetArgs {
            run_id: None,
            text: "SELF".into(),
        }),
        Some(parent.clone()),
    )
    .await
    .unwrap();
    assert_eq!(own["run_id"], "parent");
    assert_eq!(own["pending"]["set_by"]["relation"], "self");
    let operation = invocation(&control, set("child", "Parent instruction"));
    let original = control
        .store
        .execute(operation.clone(), Some(parent.clone()))
        .await
        .unwrap()
        .result;
    assert_eq!(original["pending"]["set_by"]["relation"], "owner");
    let replacement = store_request(
        &control,
        set("child", "Child instruction"),
        Some(child.clone()),
    )
    .await
    .unwrap();
    assert_eq!(replacement["replaced_other"], true);
    assert_eq!(replacement["previous_text_preview"], "Parent instruction");
    assert_eq!(
        control
            .store
            .execute(operation.clone(), Some(parent.clone()))
            .await
            .unwrap()
            .result,
        original,
        "replay returns original saved outcome"
    );
    let mut conflict = operation;
    conflict.command = set("child", "Changed with same operation");
    assert_eq!(
        control
            .store
            .execute(conflict, Some(parent.clone()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    let receipts = || async {
        control
            .store
            .database
            .call(|db| {
                Ok(
                    db.query_row("SELECT count(*) FROM operation_receipt", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap()
    };
    let before = receipts().await;
    let get = store_request(
        &control,
        Command::RevisitGet(RevisitArgs {
            run_id: Some("child".into()),
        }),
        Some(parent.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        get["pending"]["text"], "Child instruction",
        "replay must not restore overwritten state"
    );
    assert_eq!(receipts().await, before, "get allocates no durable receipt");
    for target in ["grandchild", "unrelated"] {
        for command in [
            set(target, "Denied"),
            Command::RevisitGet(RevisitArgs {
                run_id: Some(target.into()),
            }),
            Command::RevisitCancel(RevisitArgs {
                run_id: Some(target.into()),
            }),
        ] {
            assert_eq!(
                store_request(&control, command, Some(parent.clone()))
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidState
            );
        }
    }
    assert_eq!(
        store_request(
            &control,
            Command::RevisitGet(RevisitArgs { run_id: None }),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidState
    );
    let human = store_request(&control, set("child", "Human instruction"), None)
        .await
        .unwrap();
    assert_eq!(human["replaced_other"], true);
    assert_eq!(human["pending"]["set_by"]["kind"], "human");
    let mut forged = invocation(&control, Command::RevisitGet(RevisitArgs { run_id: None }));
    forged.origin = Some(parent.clone());
    assert_eq!(
        control.request(forged).await.error.unwrap().code,
        ErrorCode::InvalidArgument
    );
    for expected in [true, false] {
        assert_eq!(
            store_request(
                &control,
                Command::RevisitCancel(RevisitArgs {
                    run_id: Some("child".into())
                }),
                Some(parent.clone())
            )
            .await
            .unwrap()["cancelled"],
            expected
        );
    }
    control
        .store
        .database
        .call(|db| {
            db.execute("UPDATE run SET state='stopped' WHERE id='parent'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        store_request(&control, set("child", "Dead owner"), Some(parent))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidState
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_captures_pending_instruction_then_drops_it_without_automatic_delivery() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url, None).await;
    let control = Control::new(&owner, root.path().into());
    let thread = seed_run(&control, "interrupted-revisit", None).await;
    request(
        &control,
        set("interrupted-revisit", "INTERRUPTED_REVISIT_CAPTURE_ONLY"),
    )
    .await;
    // Close the DB owner without graceful Control capture, simulating the
    // durable startup input left by a terminated previous process.
    owner.shutdown().await.unwrap();
    drop(control);
    let reopened = DataOwner::start(
        Some(root.path().join("config.toml")),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&reopened, root.path().into());
    assert!(saved(&control, "interrupted-revisit").await["pending"].is_null());
    assert_eq!(event_count(&control, "interrupted-revisit").await, 0);
    let body=control.store.database.call(|db|{
        Ok(db.query_row("SELECT payload_json FROM node WHERE kind='interruption_context' ORDER BY rowid DESC LIMIT 1",[],|r|r.get::<_,String>(0))?)
    }).await.unwrap();
    let node: Value = serde_json::from_str(&body).unwrap();
    let snapshot: Value = serde_json::from_str(node["text"].as_str().unwrap()).unwrap();
    assert!(
        snapshot
            .to_string()
            .contains("INTERRUPTED_REVISIT_CAPTURE_ONLY")
    );
    assert_eq!(
        snapshot["change"]["revisits"]["items"][0]["consumed_count"],
        0
    );
    assert_eq!(
        snapshot["change"]["revisits"]["items"][0]["pending"]["set_by"]["kind"],
        "human"
    );
    assert!(
        timeout(Duration::from_millis(100), receive.recv())
            .await
            .is_err(),
        "startup must not resume reservation"
    );
    let fresh = begin(&control, &thread).await;
    let next = incoming(&mut receive).await;
    assert!(
        next.body
            .to_string()
            .contains("INTERRUPTED_REVISIT_CAPTURE_ONLY"),
        "next actual Turn receives the interruption facts"
    );
    next.reply
        .send((200, answer("Acknowledged interruption")))
        .unwrap();
    run_state(&control, fresh.run_id.as_ref().unwrap(), "completed").await;
    request(
        &control,
        Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: thread.clone(),
            descendants: false,
        }),
    )
    .await;
    assert_eq!(
        control
            .store
            .database
            .call(|db| Ok(db.query_row(
                "SELECT count(*) FROM app_meta WHERE key='revisit.interrupted-revisit'",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        0
    );
    control.shutdown().await.unwrap();
    reopened.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_revisit_waits_for_owned_background_shell_then_fires() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url, None).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let run = begin(&control, &thread).await.run_id.unwrap();
    let first = incoming(&mut receive).await;
    let command = if cfg!(windows) {
        "[Console]::ReadLine()"
    } else {
        "read value"
    };
    first.reply.send((200,json!({"id":"background-owned-work","status":"completed","output":[{"type":"function_call","call_id":"background-wait","name":"shell","arguments":json!({"command":command,"background":true,"stdin_open":true}).to_string()}]}))).unwrap();
    let after_tool = incoming(&mut receive).await;
    assert!(
        after_tool.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output")
    );
    let id = run.clone();
    let execution: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT id FROM shell_execution WHERE owner_run_id=?1 AND state='running'",
                [id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    request(&control, set(&run, "AFTER_OWNED_SHELL_FINISHES")).await;
    after_tool
        .reply
        .send((200, answer("Foreground answer complete")))
        .unwrap();
    // Wait for the Turn to complete before testing the owned-work gate.
    let id = run.clone();
    timeout(Duration::from_secs(10),async{loop{let target=id.clone();let done=control.store.database.call(move|db|Ok(db.query_row("SELECT NOT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting'))",[target],|r|r.get::<_,bool>(0))?)).await.unwrap();if done{break;}tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
    assert_eq!(event_count(&control, &run).await, 0);
    assert_eq!(
        saved(&control, &run).await["pending"]["text"],
        "AFTER_OWNED_SHELL_FINISHES"
    );
    assert!(
        timeout(Duration::from_millis(100), receive.recv())
            .await
            .is_err()
    );
    request(
        &control,
        Command::ShellEof(ExecutionTarget {
            execution_id: execution,
        }),
    )
    .await;
    let shell_ended = incoming(&mut receive).await;
    assert!(shell_ended.body.to_string().contains("shell_ended"));
    assert_eq!(
        event_count(&control, &run).await,
        0,
        "owned completion event is processed before Revisit"
    );
    shell_ended
        .reply
        .send((200, answer("Shell completion acknowledged")))
        .unwrap();
    let revisit = incoming(&mut receive).await;
    assert!(
        revisit
            .body
            .to_string()
            .contains("AFTER_OWNED_SHELL_FINISHES")
    );
    revisit
        .reply
        .send((200, answer("Owned work verified")))
        .unwrap();
    run_state(&control, &run, "completed").await;
    assert_eq!(event_count(&control, &run).await, 1);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

fn cli_command(args: &[&str]) -> String {
    let quote = |text: &str| {
        if cfg!(windows) {
            format!("'{}'", text.replace('\'', "''"))
        } else {
            format!("'{}'", text.replace('\'', "'\\''"))
        }
    };
    let mut command = if cfg!(windows) {
        "& ".to_owned()
    } else {
        String::new()
    };
    command.push_str(&quote(env!("CARGO_BIN_EXE_shellboy")));
    for arg in args {
        command.push(' ');
        command.push_str(&quote(arg));
    }
    command
}
fn tool(command: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"function_call","call_id":uuid::Uuid::new_v4().to_string(),"name":"shell","arguments":json!({"command":command}).to_string()}]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_shell_cli_registers_its_authenticated_own_run_without_explicit_target() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, proxy) = proxy().await;
    let owner = owner(root.path(), &url, None).await;
    let control = Control::new(&owner, root.path().into());
    let (send, receive_ipc) = mpsc::channel(16);
    let ipc = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive_ipc));
    let thread = thread(&control).await;
    let run = begin(&control, &thread).await.run_id.unwrap();
    incoming(&mut receive)
        .await
        .reply
        .send((
            200,
            tool(&cli_command(&[
                "revisit",
                "set",
                "--text",
                "MODEL_OWN_REVISIT",
            ])),
        ))
        .unwrap();
    let after_tool = incoming(&mut receive).await;
    let reservation = saved(&control, &run).await;
    assert_eq!(reservation["pending"]["set_by"]["kind"], "model");
    assert_eq!(reservation["pending"]["set_by"]["relation"], "self");
    assert_eq!(reservation["pending"]["set_by"]["run_id"], run);
    after_tool
        .reply
        .send((200, answer("Original task finished")))
        .unwrap();
    let revisit = incoming(&mut receive).await;
    assert!(
        revisit.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["role"] == "developer"
                && item.to_string().contains("MODEL_OWN_REVISIT"))
    );
    revisit
        .reply
        .send((200, answer("Self check finished")))
        .unwrap();
    run_state(&control, &run, "completed").await;
    assert_eq!(event_count(&control, &run).await, 1);
    control.shutdown().await.unwrap();
    ipc.shutdown().await.unwrap();
    serving.abort();
    owner.shutdown().await.unwrap();
    proxy.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_revisit_finishes_before_parent_receives_child_completion_and_its_own_revisit() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, proxy) = proxy().await;
    let owner = owner(root.path(), &url, None).await;
    let control = Control::new(&owner, root.path().into());
    let (send, receive_ipc) = mpsc::channel(16);
    let ipc = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive_ipc));
    let parent_thread = thread(&control).await;
    let child_thread = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            cwd: None,
            parent_id: Some(parent_thread.clone()),
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let parent_run = begin(&control, &parent_thread).await.run_id.unwrap();
    let first = incoming(&mut receive).await;
    request(&control, set(&parent_run, "PARENT_REVISIT_AFTER_CHILD")).await;
    first
        .reply
        .send((
            200,
            tool(&cli_command(&[
                "message",
                "send",
                "--thread",
                &child_thread,
                "--text",
                "OWNED_CHILD_INPUT",
            ])),
        ))
        .unwrap();
    let a = incoming(&mut receive).await;
    let b = incoming(&mut receive).await;
    let is_child =
        |body: &Value| {
            body["input"].as_array().unwrap().iter().any(|item| {
                item["role"] == "user" && item.to_string().contains("OWNED_CHILD_INPUT")
            })
        };
    let (child, parent) = if is_child(&a.body) {
        assert!(!is_child(&b.body));
        (a, b)
    } else {
        assert!(is_child(&b.body));
        (b, a)
    };
    assert_eq!(child.session.as_deref(), Some(child_thread.as_str()));
    assert_eq!(parent.session.as_deref(), Some(parent_thread.as_str()));
    let target = parent_run.clone();
    let child_run: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT id FROM run WHERE owner_run_id=?1 AND state='live'",
                [target],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    request(&control, set(&child_run, "CHILD_REVISIT_BEFORE_REPORT")).await;
    parent
        .reply
        .send((200, answer("Waiting for owned child")))
        .unwrap();
    child
        .reply
        .send((200, answer("Child original answer")))
        .unwrap();
    // Child Turn completion may independently wake the parent before the
    // child's Revisit request reaches the proxy. Route by the real Thread
    // session header, rather than assuming a cross-Thread HTTP arrival order.
    let child_revisit = timeout(Duration::from_secs(20), async {
        for _ in 0..8 {
            let next = incoming(&mut receive).await;
            let parent_id = parent_run.clone();
            let ended: i64 = control.store.database.call(move |db| {
                Ok(db.query_row("SELECT count(*) FROM event WHERE kind='child_run_ended' AND target_run_id=?1", [parent_id], |r| r.get(0))?)
            }).await.unwrap();
            assert_eq!(ended, 0, "child cannot report Run completion before its Revisit finishes");
            assert_eq!(event_count(&control, &parent_run).await, 0);
            if next.session.as_deref() == Some(child_thread.as_str()) {
                assert!(next.body["input"].as_array().unwrap().iter().any(|item| item["role"] == "developer" && item.to_string().contains("CHILD_REVISIT_BEFORE_REPORT")), "child request must carry its Revisit event: {}", next.body);
                return next;
            }
            assert_eq!(next.session.as_deref(), Some(parent_thread.as_str()));
            assert!(next.body.to_string().contains("child_turn_completed") || next.body.to_string().contains("shell_ended"), "unexpected parent notification: {}", next.body);
            next.reply.send((200, answer("Parent observed progress; child still owns work"))).unwrap();
        }
        panic!("child Revisit did not arrive within the bounded notification sequence");
    }).await.expect("child Revisit routing watchdog");
    child_revisit
        .reply
        .send((200, answer("Child verified all work")))
        .unwrap();
    let parent_revisit = timeout(Duration::from_secs(20), async {
        for _ in 0..8 {
            let next = incoming(&mut receive).await;
            assert_eq!(next.session.as_deref(), Some(parent_thread.as_str()));
            if next.body["input"].as_array().unwrap().iter().any(|item| {
                item["role"] == "developer"
                    && item.to_string().contains("PARENT_REVISIT_AFTER_CHILD")
            }) {
                assert!(
                    next.body.to_string().contains("child_run_ended"),
                    "parent must process child Run completion before own Revisit"
                );
                return next;
            }
            assert_eq!(event_count(&control, &parent_run).await, 0);
            assert!(
                next.body.to_string().contains("child_turn_completed")
                    || next.body.to_string().contains("child_run_ended")
                    || next.body.to_string().contains("shell_ended"),
                "unexpected parent notification: {}",
                next.body
            );
            next.reply
                .send((200, answer("Parent accepted child progress/completion")))
                .unwrap();
        }
        panic!("parent Revisit did not arrive within the bounded notification sequence");
    })
    .await
    .expect("parent Revisit routing watchdog");
    parent_revisit
        .reply
        .send((200, answer("Parent self check finished")))
        .unwrap();
    run_state(&control, &parent_run, "completed").await;
    assert_eq!(event_count(&control, &child_run).await, 1);
    assert_eq!(event_count(&control, &parent_run).await, 1);
    control.shutdown().await.unwrap();
    ipc.shutdown().await.unwrap();
    serving.abort();
    owner.shutdown().await.unwrap();
    proxy.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_racing_normal_completion_is_either_delivered_or_rejected_as_terminal() {
    for _ in 0..4 {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = owner(root.path(), &url, None).await;
        let control = Control::new(&owner, root.path().into());
        let thread = thread(&control).await;
        let run = begin(&control, &thread).await.run_id.unwrap();
        let held = incoming(&mut receive).await;
        let (_, registration) = timeout(Duration::from_secs(20), async {
            tokio::join!(
                async {
                    held.reply
                        .send((200, answer("Completion racing registration")))
                        .unwrap();
                    tokio::task::yield_now().await;
                },
                control.request(invocation(&control, set(&run, "REGISTER_COMPLETION_RACE")))
            )
        })
        .await
        .expect("registration/completion race watchdog");
        if registration.ok {
            let revisit = incoming(&mut receive).await;
            assert!(
                revisit.body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["role"] == "developer"
                        && item.to_string().contains("REGISTER_COMPLETION_RACE")),
                "accepted reservation must not be stranded by completion"
            );
            revisit
                .reply
                .send((200, answer("Race registration delivered")))
                .unwrap();
            run_state(&control, &run, "completed").await;
            assert_eq!(event_count(&control, &run).await, 1);
            assert!(saved(&control, &run).await["pending"].is_null());
        } else {
            assert_eq!(
                registration.error.unwrap().code,
                ErrorCode::InvalidState,
                "only terminal-target rejection is valid when completion wins"
            );
            run_state(&control, &run, "completed").await;
            assert_eq!(event_count(&control, &run).await, 0);
        }
        assert!(
            timeout(Duration::from_millis(100), receive.recv())
                .await
                .is_err(),
            "no duplicate delivery after either serialized outcome"
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_user_input_and_cancel_keep_the_same_run_without_premature_revisit() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url, None).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let original = begin(&control, &thread).await;
    let run = original.run_id.unwrap();
    let held = incoming(&mut receive).await;
    request(&control, set(&run, "CANCEL_BEFORE_QUEUED_INPUT_COMPLETES")).await;
    let followup: MessageAccepted = serde_json::from_value(
        timeout(
            Duration::from_secs(20),
            request(
                &control,
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some(thread),
                    text: "FOLLOWUP_ACCEPTED_BEFORE_COMPLETION".into(),
                }),
            ),
        )
        .await
        .expect("follow-up acceptance watchdog"),
    )
    .unwrap();
    assert_eq!(followup.run_id.as_deref(), Some(run.as_str()));
    let cancelled = request(
        &control,
        Command::RevisitCancel(RevisitArgs {
            run_id: Some(run.clone()),
        }),
    )
    .await;
    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(event_count(&control, &run).await, 0);
    let _ = held.reply.send((200, answer("Late superseded answer")));
    let latest = incoming(&mut receive).await;
    assert!(latest.body["input"].as_array().unwrap().iter().any(|item| {
        item["role"] == "user"
            && item
                .to_string()
                .contains("FOLLOWUP_ACCEPTED_BEFORE_COMPLETION")
    }));
    latest
        .reply
        .send((200, answer("Answer includes follow-up")))
        .unwrap();
    run_state(&control, &run, "completed").await;
    assert_eq!(event_count(&control, &run).await, 0);
    assert!(saved(&control, &run).await["pending"].is_null());
    let target = run.clone();
    let receipts: i64 = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM receipt WHERE run_id=?1",
                [target],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        receipts, 2,
        "both accepted user messages retain their own receipt"
    );
    assert!(
        timeout(Duration::from_millis(100), receive.recv())
            .await
            .is_err()
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn malformed_optional_metadata_does_not_block_observation_or_stop_and_never_invents_counts() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path(), "http://127.0.0.1:9/v1", None).await;
    let control = Control::new(&owner, root.path().into());
    for (ordinal, broken) in [
        "{",
        "{}",
        r#"{"pending":null,"consumed_count":"unknown","last_fired_at":null}"#,
    ]
    .into_iter()
    .enumerate()
    {
        let run = format!("broken-{ordinal}");
        let thread = seed_run(&control, &run, None).await;
        let key = format!("revisit.{run}");
        let damaged = broken.to_owned();
        control
            .store
            .database
            .call(move |db| {
                db.execute(
                    "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
                    rusqlite::params![key, damaged],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let get = control
            .request(invocation(
                &control,
                Command::RevisitGet(RevisitArgs {
                    run_id: Some(run.clone()),
                }),
            ))
            .await;
        let error = get.error.unwrap();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(error.hint.unwrap().contains("Stop"));
        let details = request(
            &control,
            Command::RunGet(RunTarget {
                run_id: run.clone(),
            }),
        )
        .await;
        assert!(
            details.get("revisit").is_none(),
            "unreadable optional metadata must not become a fabricated zero counter"
        );
        let list = request(
            &control,
            Command::ThreadList(ThreadListArgs {
                limit: MAX_LIST_LIMIT,
                cursor: None,
                query: None,
            }),
        )
        .await;
        let item = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["thread_id"] == thread)
            .unwrap();
        assert!(item.get("revisit").is_none());
        request(
            &control,
            Command::RunStop(RunTarget {
                run_id: run.clone(),
            }),
        )
        .await;
        run_state(&control, &run, "stopped").await;
        let key = format!("revisit.{run}");
        assert_eq!(
            control
                .store
                .database
                .call(move |db| Ok(db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key=?1",
                    [key],
                    |r| r.get::<_, i64>(0)
                )?))
                .await
                .unwrap(),
            0
        );
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
