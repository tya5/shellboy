//! Revisit (`message send --deliver complete`) through real Control and a local HTTP mock.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};
struct Incoming {
    body: Value,
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
                let body = loop {
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
                            break serde_json::from_slice::<Value>(&bytes[i + 4..i + 4 + length])
                                .unwrap();
                        }
                    }
                };
                let (reply, result) = oneshot::channel();
                if send.send(Incoming { body, reply }).await.is_err() {
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
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\nresponse_header_timeout_ms=15000\n")).unwrap();
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
    crate::test_watchdog::watchdog("tests/revisit.rs: incoming", receive.recv())
        .await
        .unwrap()
        .unwrap()
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
async fn send(
    control: &Arc<Control>,
    thread: &str,
    text: &str,
    deliver: Deliver,
) -> MessageAccepted {
    serde_json::from_value(
        request(
            control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                deliver,
                thread_id: Some(thread.into()),
                text: text.into(),
            }),
        )
        .await,
    )
    .unwrap()
}
async fn run_state(control: &Control, run: &str, expected: &str) {
    let run = run.to_owned();
    crate::test_watchdog::watchdog("tests/revisit.rs: run_state", async {
        loop {
            let id = run.clone();
            let state: String = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row("SELECT state FROM run WHERE id=?1", [id], |r| r.get(0))?)
                })
                .await
                .unwrap();
            if state == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Run lifecycle watchdog");
}
/// Saved receipt state and hold reason of one message.
async fn receipt(control: &Control, message: &str) -> (String, Option<String>, Option<String>) {
    let message = message.to_owned();
    control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT state,hold_reason,run_id FROM receipt WHERE message_node_id=?1",
                [message],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap()
}
async fn run_revisit(control: &Arc<Control>, run: &str) -> Option<RevisitSummary> {
    serde_json::from_value::<RunDetails>(
        request(control, Command::RunGet(RunTarget { run_id: run.into() })).await,
    )
    .unwrap()
    .revisit
}
/// Texts appear in this request exactly in the given order; `absent` do not appear.
fn assert_order(body: &Value, present: &[&str], absent: &[&str]) {
    let text = body.to_string();
    let mut last = 0;
    for item in present {
        let at = text[last..]
            .find(item)
            .unwrap_or_else(|| panic!("{item} missing or out of order: {text}"));
        last += at + item.len();
    }
    for item in absent {
        assert!(!text.contains(item), "{item} delivered too early: {text}");
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    owner: DataOwner,
    control: Arc<Control>,
    receive: mpsc::Receiver<Incoming>,
    server: tokio::task::JoinHandle<()>,
    thread: String,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let (url, receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    Fixture {
        _root: root,
        owner,
        control,
        receive,
        server,
        thread,
    }
}
impl Fixture {
    async fn finish(self) {
        self.control.shutdown().await.unwrap();
        self.owner.shutdown().await.unwrap();
        self.server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_run_receives_complete_messages_in_send_order_when_it_would_complete() {
    let mut f = fixture().await;
    let run = send(&f.control, &f.thread, "ORIGINAL", Deliver::Next)
        .await
        .run_id
        .unwrap();
    let first = incoming(&mut f.receive).await;
    let mut ids = Vec::new();
    for text in ["COMPLETE_ONE", "COMPLETE_TWO"] {
        let accepted = send(&f.control, &f.thread, text, Deliver::Complete).await;
        assert_eq!(accepted.deliver, Deliver::Complete);
        assert_eq!(accepted.delivery_status, DeliveryStatus::Held);
        assert_eq!(accepted.assignment, Assignment::Pending);
        assert!(accepted.run_id.is_none());
        ids.push(accepted.message_id);
    }
    assert_eq!(
        run_revisit(&f.control, &run).await,
        Some(RevisitSummary {
            pending: true,
            consumed_count: 0
        })
    );
    assert_order(
        &first.body,
        &["ORIGINAL"],
        &["COMPLETE_ONE", "COMPLETE_TWO"],
    );
    first.reply.send((200, answer("First answer"))).unwrap();
    let revisit = incoming(&mut f.receive).await;
    assert_order(
        &revisit.body,
        &["ORIGINAL", "COMPLETE_ONE", "COMPLETE_TWO"],
        &[],
    );
    for id in &ids {
        let (_, _, assigned) = receipt(&f.control, id).await;
        assert_eq!(
            assigned.as_deref(),
            Some(run.as_str()),
            "same Run continues"
        );
    }
    revisit.reply.send((200, answer("Checked"))).unwrap();
    run_state(&f.control, &run, "completed").await;
    assert_eq!(
        run_revisit(&f.control, &run).await,
        Some(RevisitSummary {
            pending: false,
            consumed_count: 2
        })
    );
    assert!(f.receive.try_recv().is_err(), "delivered once");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_live_run_complete_is_delivered_immediately_like_a_normal_send() {
    let mut f = fixture().await;
    let accepted = send(&f.control, &f.thread, "IMMEDIATE", Deliver::Complete).await;
    assert_eq!(accepted.assignment, Assignment::NewRun);
    assert_eq!(accepted.delivery_status, DeliveryStatus::Accepted);
    let run = accepted.run_id.unwrap();
    let first = incoming(&mut f.receive).await;
    assert_order(&first.body, &["IMMEDIATE"], &[]);
    first.reply.send((200, answer("Done"))).unwrap();
    run_state(&f.control, &run, "completed").await;
    assert_eq!(run_revisit(&f.control, &run).await, None);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_completing_between_send_and_complete_still_delivers() {
    let mut f = fixture().await;
    let run = send(&f.control, &f.thread, "WORK", Deliver::Next)
        .await
        .run_id
        .unwrap();
    incoming(&mut f.receive)
        .await
        .reply
        .send((200, answer("Finished early")))
        .unwrap();
    run_state(&f.control, &run, "completed").await;
    let accepted = send(&f.control, &f.thread, "LATE_REVISIT", Deliver::Complete).await;
    assert_eq!(accepted.assignment, Assignment::NewRun);
    let next = incoming(&mut f.receive).await;
    assert_order(&next.body, &["WORK", "LATE_REVISIT"], &[]);
    next.reply.send((200, answer("Checked"))).unwrap();
    run_state(&f.control, accepted.run_id.as_deref().unwrap(), "completed").await;
    f.finish().await;
}

/// Later now/default messages overtake waiting complete messages; each timing
/// class keeps its own send order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn now_and_default_messages_overtake_waiting_complete_messages() {
    use Deliver::*;
    let cases: &[&[(&str, Deliver)]] = &[
        &[("C_THEN_NOW_C", Complete), ("C_THEN_NOW_N", Now)],
        &[("C_THEN_DEFAULT_C", Complete), ("C_THEN_DEFAULT_D", Next)],
        &[
            ("MIX_C1", Complete),
            ("MIX_N1", Now),
            ("MIX_C2", Complete),
            ("MIX_N2", Now),
            ("MIX_D1", Next),
        ],
    ];
    for case in cases {
        let mut f = fixture().await;
        let run = send(&f.control, &f.thread, "BASE", Next)
            .await
            .run_id
            .unwrap();
        let first = incoming(&mut f.receive).await;
        for (text, deliver) in *case {
            send(&f.control, &f.thread, text, *deliver).await;
        }
        let complete = case
            .iter()
            .filter(|(_, deliver)| *deliver == Complete)
            .map(|(text, _)| *text)
            .collect::<Vec<_>>();
        let ordinary = case
            .iter()
            .filter(|(_, deliver)| *deliver != Complete)
            .map(|(text, _)| *text)
            .collect::<Vec<_>>();
        first.reply.send((200, answer("Base answer"))).unwrap();
        let second = incoming(&mut f.receive).await;
        assert_order(&second.body, &ordinary, &complete);
        second.reply.send((200, answer("Ordinary answer"))).unwrap();
        let third = incoming(&mut f.receive).await;
        assert_order(&third.body, &complete, &[]);
        third.reply.send((200, answer("Revisit answer"))).unwrap();
        run_state(&f.control, &run, "completed").await;
        f.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_one_waiting_complete_message_leaves_the_others() {
    let mut f = fixture().await;
    let run = send(&f.control, &f.thread, "BASE", Deliver::Next)
        .await
        .run_id
        .unwrap();
    let first = incoming(&mut f.receive).await;
    let cancelled = send(
        &f.control,
        &f.thread,
        "CANCELLED_COMPLETE",
        Deliver::Complete,
    )
    .await;
    send(&f.control, &f.thread, "KEPT_COMPLETE", Deliver::Complete).await;
    let result = request(
        &f.control,
        Command::MessageCancel(MessageCancelArgs {
            message_ids: vec![cancelled.message_id.clone()],
        }),
    )
    .await;
    assert_eq!(result["items"][0]["outcome"], "cancelled");
    first.reply.send((200, answer("Base answer"))).unwrap();
    let revisit = incoming(&mut f.receive).await;
    assert_order(&revisit.body, &["KEPT_COMPLETE"], &["CANCELLED_COMPLETE"]);
    revisit.reply.send((200, answer("Checked"))).unwrap();
    run_state(&f.control, &run, "completed").await;
    assert_eq!(
        receipt(&f.control, &cancelled.message_id).await.0,
        "cancelled"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_releases_waiting_messages_as_ordinary_held_input() {
    let mut f = fixture().await;
    let run = send(&f.control, &f.thread, "STOPPED_WORK", Deliver::Next)
        .await
        .run_id
        .unwrap();
    let held = incoming(&mut f.receive).await;
    let waiting = send(&f.control, &f.thread, "RELEASED_AT_STOP", Deliver::Complete).await;
    request(
        &f.control,
        Command::RunStop(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    run_state(&f.control, &run, "stopped").await;
    let _ = held.reply.send((200, answer("Late response")));
    assert_eq!(
        receipt(&f.control, &waiting.message_id).await,
        ("held".into(), Some("stopping".into()), None),
        "the same held input a send to the stopping Run gets"
    );
    assert!(f.receive.try_recv().is_err(), "stop starts nothing");
    let next_run = send(&f.control, &f.thread, "NEW_INSTRUCTION", Deliver::Next)
        .await
        .run_id
        .unwrap();
    let next = incoming(&mut f.receive).await;
    assert_order(&next.body, &["RELEASED_AT_STOP", "NEW_INSTRUCTION"], &[]);
    next.reply.send((200, answer("New answer"))).unwrap();
    run_state(&f.control, &next_run, "completed").await;
    assert!(f.receive.try_recv().is_err(), "delivered once");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn human_turn_interruption_releases_waiting_messages_at_run_end() {
    let mut f = fixture().await;
    let accepted = send(&f.control, &f.thread, "INTERRUPTED_WORK", Deliver::Next).await;
    let run = accepted.run_id.clone().unwrap();
    let held = incoming(&mut f.receive).await;
    send(
        &f.control,
        &f.thread,
        "RELEASED_AFTER_ESC",
        Deliver::Complete,
    )
    .await;
    f.control
        .interrupt_turn(f.thread.clone(), accepted.turn_id.unwrap())
        .await
        .unwrap();
    drop(held);
    let revisit = incoming(&mut f.receive).await;
    assert_order(
        &revisit.body,
        &["turn_interrupted", "RELEASED_AFTER_ESC"],
        &[],
    );
    revisit.reply.send((200, answer("Checked"))).unwrap();
    run_state(&f.control, &run, "completed").await;
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_releases_waiting_messages_to_the_pending_path_on_next_start() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner_one = owner(root.path(), &url).await;
    let control = Control::new(&owner_one, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "BEFORE_SHUTDOWN", Deliver::Next).await;
    let held = incoming(&mut receive).await;
    let waiting = send(
        &control,
        &thread,
        "RELEASED_AFTER_RESTART",
        Deliver::Complete,
    )
    .await;
    control.shutdown().await.unwrap();
    owner_one.shutdown().await.unwrap();
    drop(held);
    let owner_two = owner(root.path(), &url).await;
    let control = Control::new(&owner_two, root.path().into());
    assert_eq!(
        receipt(&control, &waiting.message_id).await,
        (
            "held".into(),
            Some("previous_instance_interrupted".into()),
            None
        )
    );
    let next_run = send(&control, &thread, "AFTER_RESTART", Deliver::Next)
        .await
        .run_id
        .unwrap();
    let next = incoming(&mut receive).await;
    assert_order(
        &next.body,
        &["RELEASED_AFTER_RESTART", "AFTER_RESTART"],
        &[],
    );
    next.reply.send((200, answer("Resumed"))).unwrap();
    run_state(&control, &next_run, "completed").await;
    control.shutdown().await.unwrap();
    owner_two.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_wait_hands_waiting_messages_to_the_ordinary_recovery_rules() {
    let mut f = fixture().await;
    let run = send(&f.control, &f.thread, "BASE", Deliver::Next)
        .await
        .run_id
        .unwrap();
    let first = incoming(&mut f.receive).await;
    let waiting = send(
        &f.control,
        &f.thread,
        "RELEASED_AT_RECOVERY",
        Deliver::Complete,
    )
    .await;
    first
        .reply
        .send((
            500,
            json!({"error":{"message":"synthetic provider failure","type":"server_error"}}),
        ))
        .unwrap();
    let target = run.clone();
    let control = f.control.clone();
    crate::test_watchdog::watchdog("tests/revisit.rs: recovery_wait", async move {
        loop {
            let id = target.clone();
            let recovery = control.store.database.call(move |db| Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state='recovery_wait')",[id],|r|r.get::<_,bool>(0))?)).await.unwrap();
            if recovery {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        receipt(&f.control, &waiting.message_id).await,
        (
            "held".into(),
            Some("recovery_wait".into()),
            Some(run.clone())
        ),
        "the same held input a machine send during recovery wait gets"
    );
    request(
        &f.control,
        Command::RunRetry(RunTarget {
            run_id: run.clone(),
        }),
    )
    .await;
    let retried = incoming(&mut f.receive).await;
    assert_order(&retried.body, &["BASE", "RELEASED_AT_RECOVERY"], &[]);
    retried.reply.send((200, answer("Recovered"))).unwrap();
    run_state(&f.control, &run, "completed").await;
    assert!(f.receive.try_recv().is_err(), "delivered once");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_message_waits_for_owned_background_shell() {
    let mut f = fixture().await;
    let run = send(&f.control, &f.thread, "BASE", Deliver::Next)
        .await
        .run_id
        .unwrap();
    let first = incoming(&mut f.receive).await;
    let command = if cfg!(windows) {
        "[Console]::ReadLine()"
    } else {
        "read value"
    };
    first.reply.send((200,json!({"id":"background-owned-work","status":"completed","output":[{"type":"function_call","call_id":"background-wait","name":"shell","arguments":json!({"command":command,"background":true,"stdin_open":true}).to_string()}]}))).unwrap();
    let after_tool = incoming(&mut f.receive).await;
    let id = run.clone();
    let execution: String = f
        .control
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
    send(
        &f.control,
        &f.thread,
        "AFTER_OWNED_SHELL_FINISHES",
        Deliver::Complete,
    )
    .await;
    after_tool
        .reply
        .send((200, answer("Foreground answer complete")))
        .unwrap();
    let id = run.clone();
    let control = f.control.clone();
    crate::test_watchdog::watchdog("tests/revisit.rs: owned shell turn", async move {
        loop {
            let target = id.clone();
            let done = control.store.database.call(move |db| Ok(db.query_row("SELECT NOT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting'))",[target],|r|r.get::<_,bool>(0))?)).await.unwrap();
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(f.receive.try_recv().is_err());
    request(
        &f.control,
        Command::ShellEof(ExecutionTarget {
            execution_id: execution,
        }),
    )
    .await;
    let shell_ended = incoming(&mut f.receive).await;
    assert_order(
        &shell_ended.body,
        &["shell_ended"],
        &["AFTER_OWNED_SHELL_FINISHES"],
    );
    shell_ended
        .reply
        .send((200, answer("Shell completion acknowledged")))
        .unwrap();
    let revisit = incoming(&mut f.receive).await;
    assert_order(&revisit.body, &["AFTER_OWNED_SHELL_FINISHES"], &[]);
    revisit
        .reply
        .send((200, answer("Owned work verified")))
        .unwrap();
    run_state(&f.control, &run, "completed").await;
    f.finish().await;
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
async fn model_shell_cli_defers_a_message_to_its_own_run_completion() {
    let mut f = fixture().await;
    let (send_ipc, receive_ipc) = mpsc::channel(16);
    let ipc = shellboy::ipc::LocalServer::bind(&f.owner, send_ipc)
        .await
        .unwrap();
    let serving = tokio::spawn(f.control.clone().serve(receive_ipc));
    let run = send(&f.control, &f.thread, "BASE", Deliver::Next)
        .await
        .run_id
        .unwrap();
    incoming(&mut f.receive)
        .await
        .reply
        .send((
            200,
            tool(&cli_command(&[
                "message",
                "send",
                "--deliver",
                "complete",
                "--text",
                "MODEL_OWN_REVISIT",
            ])),
        ))
        .unwrap();
    let after_tool = incoming(&mut f.receive).await;
    let waiting: i64 = f
        .control
        .store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT count(*) FROM receipt WHERE state='held' AND hold_reason='run_completion' AND json_extract(sender_json,'$.kind')='model'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(waiting, 1, "{}", after_tool.body);
    after_tool
        .reply
        .send((200, answer("Original task finished")))
        .unwrap();
    let revisit = incoming(&mut f.receive).await;
    assert_order(
        &revisit.body,
        &["Original task finished", "MODEL_OWN_REVISIT"],
        &[],
    );
    revisit
        .reply
        .send((200, answer("Self check finished")))
        .unwrap();
    run_state(&f.control, &run, "completed").await;
    assert_eq!(
        run_revisit(&f.control, &run).await,
        Some(RevisitSummary {
            pending: false,
            consumed_count: 1
        })
    );
    f.control.shutdown().await.unwrap();
    ipc.shutdown().await.unwrap();
    serving.abort();
    f.owner.shutdown().await.unwrap();
    f.server.abort();
}
