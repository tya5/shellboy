//! Cross-service operation boundaries using a real owner and loopback proxy.
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    attachments::AttachmentInput,
    control::{Control, EngineEvent},
    protocol::*,
};
use std::{collections::VecDeque, sync::Arc, time::Duration};
#[path = "runtime_features/compaction_failure_memory.rs"]
mod compaction_failure_memory;
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

pub(crate) struct Incoming {
    pub(crate) body: Value,
    pub(crate) session: Option<String>,
    pub(crate) reply: oneshot::Sender<(u16, Value)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderRequestKind {
    Ordinary,
    Summary,
}

impl Incoming {
    pub(crate) fn kind(&self) -> ProviderRequestKind {
        if self.body["tools"].as_array().is_some_and(Vec::is_empty) {
            ProviderRequestKind::Summary
        } else {
            ProviderRequestKind::Ordinary
        }
    }
}

/// Buffers the other request kind when a test waits for an ordinary or
/// summary request; this preserves both legal Run/summary arrival orders.
pub(crate) struct ProxyInbox {
    pub(crate) receiver: mpsc::Receiver<Incoming>,
    deferred: VecDeque<Incoming>,
}
impl ProxyInbox {
    pub(crate) async fn recv(&mut self) -> Option<Incoming> {
        match self.deferred.pop_front() {
            Some(request) => Some(request),
            None => self.receiver.recv().await,
        }
    }
    pub(crate) fn try_recv(&mut self) -> Result<Incoming, mpsc::error::TryRecvError> {
        self.deferred
            .pop_front()
            .map(Ok)
            .unwrap_or_else(|| self.receiver.try_recv())
    }
    async fn recv_kind(&mut self, expected: ProviderRequestKind) -> Option<Incoming> {
        if let Some(index) = self
            .deferred
            .iter()
            .position(|request| request.kind() == expected)
        {
            return self.deferred.remove(index);
        }
        while let Some(request) = self.receiver.recv().await {
            if request.kind() == expected {
                return Some(request);
            }
            self.deferred.push_back(request);
        }
        None
    }
}

// Loopback test ordering contract:
// (a) model_attempt admission commits before its HTTP request is sent;
// (b) no later database state is ordered against proxy arrival/parsing;
// (c) Run/Turn/attempt identity comes from the request content, accepted request,
//     or its saved record, never a separately observed current/latest/first row.
pub(crate) async fn proxy() -> (String, ProxyInbox, tokio::task::JoinHandle<()>) {
    proxy_with_sse(false).await
}
async fn proxy_with_sse(sse: bool) -> (String, ProxyInbox, tokio::task::JoinHandle<()>) {
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
                let retry_after = if status == 429 {
                    "Retry-After: 30\r\n"
                } else {
                    ""
                };
                let _=stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{retry_after}Connection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
        }
    });
    (
        format!("http://{address}/v1"),
        ProxyInbox {
            receiver: receive,
            deferred: VecDeque::new(),
        },
        task,
    )
}

pub(crate) async fn owner(root: &std::path::Path, url: &str) -> DataOwner {
    owner_with_retries(root, url, 0).await
}
async fn owner_with_retries(root: &std::path::Path, url: &str, retries: u32) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms=10\n")).unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
pub(crate) async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!(
                "{}:{}",
                control.store.operation_epoch.current(),
                uuid::Uuid::new_v4()
            ),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
/// The footer projection of what `thread.get` reports as `activity`.
async fn thread_get_projection(
    control: &Arc<Control>,
    thread: &str,
) -> shellboy::frontend::activity::Projection {
    let details: shellboy::protocol::ThreadDetails = serde_json::from_value(
        request(
            control,
            Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
                thread_id: Some(thread.to_owned()),
                display: true,
            }),
        )
        .await,
    )
    .unwrap();
    shellboy::frontend::activity::from_thread_activity(
        details.activity,
        shellboy::config::UiLanguage::Ja,
    )
}
pub(crate) fn answer(text: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]})
}
// Fixture guards detect deadlocks, not product latency. Capture only bounded
// saved lifecycle facts on failure; request bodies and error messages stay out.
async fn progress(control: &Control) -> Result<Value, shellboy::storage::StorageError> {
    control.store.database.call(|db| {
        let mut state = serde_json::Map::new();
        for (name, query) in [
            ("runs", "SELECT json_object('id',substr(id,1,128),'thread_id',substr(thread_id,1,128),'state',substr(state,1,64),'stop_reason',substr(stop_reason,1,128)) FROM run ORDER BY rowid DESC LIMIT 8"),
            ("turns", "SELECT json_object('id',substr(t.id,1,128),'thread_id',substr(t.thread_id,1,128),'run_id',substr(t.run_id,1,128),'state',substr(t.state,1,64),'generation',t.generation,'instruction_snapshot_present',t.instruction_snapshot_id IS NOT NULL,'run_state',substr(r.state,1,64),'recovery_code',substr(json_extract(t.recovery_reason_json,'$.code'),1,128),'attempt_state',substr(a.state,1,64),'error_kind',substr(json_extract(a.error_json,'$.error.kind'),1,128),'http_phase',substr(json_extract(a.input_manifest_json,'$.http_observation.phase'),1,128)) FROM turn t JOIN run r ON r.id=t.run_id LEFT JOIN model_attempt a ON a.id=(SELECT latest.id FROM model_attempt latest WHERE latest.turn_id=t.id ORDER BY latest.rowid DESC LIMIT 1) ORDER BY t.rowid DESC LIMIT 8"),
        ] {
            let rows = db.prepare(query)?.query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            state.insert(name.into(), json!(rows));
        }
        Ok(Value::Object(state))
    }).await
}
#[track_caller]
pub(crate) fn incoming<'a>(
    control: &'a Control,
    receive: &'a mut ProxyInbox,
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
fn incoming_kind<'a>(
    control: &'a Control,
    receive: &'a mut ProxyInbox,
    expected: ProviderRequestKind,
) -> impl std::future::Future<Output = Incoming> + 'a {
    let caller = std::panic::Location::caller();
    async move {
        match crate::test_watchdog::watchdog(
            "tests/runtime_features.rs: classified incoming",
            receive.recv_kind(expected),
        )
        .await
        {
            Ok(Some(request)) => request,
            result => {
                let diagnostic = crate::test_watchdog::watchdog(
                    "tests/runtime_features.rs: classified incoming",
                    progress(control),
                )
                .await;
                panic!(
                    "classified request watchdog at {}:{} kind={expected:?}; receive={}; saved lifecycle facts={diagnostic:?}",
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
pub(crate) fn incoming_summary<'a>(
    control: &'a Control,
    receive: &'a mut ProxyInbox,
) -> impl std::future::Future<Output = Incoming> + 'a {
    incoming_kind(control, receive, ProviderRequestKind::Summary)
}
#[track_caller]
pub(crate) fn incoming_ordinary<'a>(
    control: &'a Control,
    receive: &'a mut ProxyInbox,
) -> impl std::future::Future<Output = Incoming> + 'a {
    incoming_kind(control, receive, ProviderRequestKind::Ordinary)
}

// Summary and Conversation admissions both commit before their provider POST.
async fn summary_request_count_for_thread(control: &Control, thread: &str) -> i64 {
    let thread = thread.to_owned();
    control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM model_attempt WHERE thread_id=?1 AND kind='summary'",
                [thread],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap()
}

async fn model_attempt_count_for_thread(control: &Control, thread: &str) -> i64 {
    let thread = thread.to_owned();
    control
        .store
        .database
        .call(move |db| {
            db.query_row(
                "SELECT count(*) FROM model_attempt WHERE thread_id=?1 AND kind='conversation'",
                [thread],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap()
}

#[track_caller]
pub(crate) fn settled<'a>(
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
pub(crate) async fn thread(control: &Arc<Control>) -> String {
    request(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
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
async fn send_accepted(control: &Arc<Control>, thread: &str, text: &str) -> MessageAccepted {
    serde_json::from_value(
        request(
            control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.into()),
                text: text.into(),
            }),
        )
        .await,
    )
    .expect("accepted Turn identity")
}
pub(crate) async fn send(control: &Arc<Control>, thread: &str, text: &str) {
    request(
        control,
        Command::MessageSend(MessageSendArgs {
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
            format!(
                "{}:{}",
                control.store.operation_epoch.current(),
                uuid::Uuid::new_v4()
            ),
            ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_thread_id: None,
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
    assert!(text.contains("attachment_changed"));
    assert!(!text.contains("input_file"));
    assert!(!text.contains("c2Vjb25kLWNvbnRlbnQtbG9uZ2Vy"));
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
    let attempts_before_compact = model_attempt_count_for_thread(&control, &thread).await;
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
    assert_eq!(
        model_attempt_count_for_thread(&control, &thread).await,
        attempts_before_compact,
        "accepting explicit Compact while the named Ordinary request waits admits no extra Turn"
    );
    ordinary.reply.send((200, answer("last answer"))).unwrap();
    let summary = incoming_summary(&control, &mut receive).await;
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
async fn overflow_without_any_target_in_an_open_tool_loop_recovers_without_resend() {
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
    // #182: with the tool loop open and only the latest round left, there is
    // no active-Turn target and no earlier Turn: no summary and no unchanged
    // resend; the Turn waits for recovery.
    pending.reply.send((400, overflow)).unwrap();
    // #303 §10: the final-stage summary range stops before the last group, so
    // with only the latest round left there is no summary target at all.
    tokio::select! {
        _ = settled(&control, &thread) => {}
        next = receive.recv() => {
            let next = next.unwrap();
            panic!(
                "§10: no request may follow when only the latest round remains; got tools={} input={}",
                next.body["tools"].as_array().map_or(0, Vec::len),
                next.body["input"].to_string().chars().take(600).collect::<String>()
            );
        }
    }
    let outcome = overflow_compaction(&control, &thread).await;
    assert_eq!(outcome["state"], "no_safe_reduction");
    let turn = request(
        &control,
        Command::TurnGet(TurnTarget {
            turn_id: outcome["turn_id"].as_str().unwrap().to_owned(),
        }),
    )
    .await;
    assert_eq!(turn["state"], "recovery_wait");
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

async fn overflow_compaction(control: &Arc<Control>, thread: &str) -> Value {
    let page = request(
        control,
        Command::CompactList(CompactListArgs {
            thread_id: Some(thread.to_owned()),
            since: None,
            until: None,
            limit: 20,
        }),
    )
    .await;
    let mut items = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == "automatic_reduction");
    let outcome = items.next().expect("the overflow attempt is listed");
    assert!(items.next().is_none(), "one overflow attempt: {page}");
    outcome.clone()
}

async fn selected_compaction_nodes(
    control: &Arc<Control>,
    thread: &str,
    kind: &str,
) -> Vec<String> {
    selected_history(control, thread)
        .await
        .items
        .into_iter()
        .filter(|node| node.kind == kind)
        .map(|node| node.node_id)
        .collect()
}

async fn read_summary(control: &Arc<Control>, event: &Value) {
    let id = event["metrics"]["summary_node_id"]
        .as_str()
        .expect("reported Summary ID");
    let args = serde_json::from_value(json!({"node_id": id})).unwrap();
    let body = request(control, Command::HistoryRead(args)).await;
    assert_eq!(body["node_id"], id);
    assert_eq!(body["kind"], "summary");
    assert!(!body["source_node_ids"].as_array().unwrap().is_empty());
}

async fn selected_history(control: &Arc<Control>, thread: &str) -> HistoryWindow {
    let args = serde_json::from_value(json!({
        "thread_id": thread, "before": 199, "after": 0, "excerpt_chars": 0,
    }))
    .unwrap();
    let window: HistoryWindow =
        serde_json::from_value(request(control, Command::HistoryWindow(args)).await).unwrap();
    assert_eq!(
        window.items.len() as u64,
        window.total,
        "fixture fits one window"
    );
    window
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_second_tool_does_not_prevent_valid_call_and_each_receives_a_result() {
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
    assert_eq!(outputs[0]["status"], "exited");
    assert_eq!(outputs[1]["status"], "error");
    assert!(marker.exists());
    second.reply.send((200, answer("corrected"))).unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_overflow_retries_preserve_call_groups_and_never_execute_saved_tools_twice() {
    async fn next(receive: &mut ProxyInbox, control: &Control, phase: &str) -> Incoming {
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
    let accepted = send_accepted(&control, &thread, "three tool groups then overflow").await;
    let turn_id = accepted.turn_id.expect("accepted overflow Turn");
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
    // #182: the tool loop is open, so the overflow falls back to the
    // active-Turn summary instead of spill/trim.
    let summary = incoming_summary(&control, &mut receive).await;
    assert_eq!(summary.body["tools"], json!([]));
    summary.reply.send((200, answer("OK"))).unwrap();
    let retry_two = next(&mut receive, &control, "first overflow: summary retry").await;
    let context = current_context(&control, &turn_id).await;
    assert_current_context_once(&retry_two.body, &context, "after automatic summary");
    let input = retry_two.body["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .all(|v| v["type"] != "function_call" && v["type"] != "function_call_output"),
        "every tool round became summary text: {input:?}"
    );
    assert!(
        input.iter().any(|v| v.to_string().contains("raw-group-2")),
        "latest round as text"
    );
    retry_two.reply.send((400, overflow.clone())).unwrap();
    // After the active-Turn summary no tool loop is open, so the existing
    // final overflow stage summarizes once more; the overflow persists.
    let final_summary = incoming_summary(&control, &mut receive).await;
    assert_eq!(final_summary.body["tools"], json!([]));
    final_summary.reply.send((200, answer("OK"))).unwrap();
    let final_retry = next(&mut receive, &control, "second overflow: final retry").await;
    let attempts_at_final_retry = model_attempt_count_for_thread(&control, &thread).await;
    final_retry.reply.send((400, overflow)).unwrap();
    settled(&control, &thread).await;
    assert_eq!(
        model_attempt_count_for_thread(&control, &thread).await,
        attempts_at_final_retry,
        "the terminal overflow response admits no subsequent Ordinary attempt"
    );
    let run_id = accepted.run_id.expect("accepted overflow Run");
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
    assert_eq!(counts.2, 0, "no spill while the tool loop is open");
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
    let summary = incoming_summary(&control, &mut receive).await;
    assert_eq!(summary.body["tools"], json!([]));
    let details = request(
        &control,
        Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
            thread_id: Some(thread),
            display: false,
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
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
    let operation = format!(
        "{}:{}",
        control.store.operation_epoch.current(),
        uuid::Uuid::new_v4()
    );
    let node = accepted["message_id"].as_str().unwrap().to_owned();
    let args = ThreadCreateArgs {
        agents: None,
        connection: Some("fixture".into()),
        model: Some("edited-model".into()),
        effort: Some(String::new()),
        cwd: Some(root.path().to_str().unwrap().into()),
        parent_thread_id: None,
    };
    let edited = control
        .edit_request_with_settings(
            operation.clone(),
            thread.clone(),
            node.clone(),
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
async fn edited_request_refuses_active_run_without_stopping_it() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let accepted = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
    for _ in 0..2 {
        let error = control
            .edit_request(
                format!(
                    "{}:{}",
                    control.store.operation_epoch.current(),
                    uuid::Uuid::new_v4()
                ),
                thread.clone(),
                node.clone(),
                "replacement".into(),
                vec![],
            )
            .await
            .unwrap_err();
        assert_eq!(error.details.unwrap()["run_id"], run);
    }
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
    assert_eq!(state, "live");
    let _ = old.reply.send((200, answer("original answer")));
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
    let attempts_before_settings = model_attempt_count_for_thread(&control, &thread).await;
    request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            auto_compact_target_percent: None,
            summary_max_chars: None,
            max_tokens: None,
            spill: None,
            auto_compact_at: None,
            agents: None,
            title: None,
            thread_id: Some(thread.clone()),
            cwd: None,
            connection: None,
            model: Some("changed-model".into()),
            effort: Some("low".into()),
            parent_thread_id: None,
        })),
    )
    .await;
    assert_eq!(
        model_attempt_count_for_thread(&control, &thread).await,
        attempts_before_settings,
        "changing settings while the named Run waits admits no model request"
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
                Command::ThreadSet(Box::new(ThreadSetArgs {
                    auto_compact_target_percent: None,
                    summary_max_chars: None,
                    max_tokens: None,
                    spill: None,
                    auto_compact_at: None,
                    agents: None,
                    title: None,
                    thread_id: Some(thread.clone()),
                    connection: None,
                    model: Some(model.into()),
                    effort: None,
                    cwd: None,
                    parent_thread_id: None,
                })),
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
    assert_eq!(
        model_attempt_count_for_thread(&control, &thread).await,
        1,
        "the named Run remains at its one committed request after terminal HTTP400"
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
    // A running execution is not collectable. Unlike a pin row without a live
    // lease (which the maintenance Pins pass deletes at any time), it stays
    // protected until this test ends it.
    control.store.database.call(move|db|{
        db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('quota-old-run',?1,'completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",[&target])?;
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('quota-old-turn',?1,'quota-old-run','completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",[target])?;
        db.execute_batch("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('quota-raw','quota-old-run','quota-old-turn','quota-old-operation','running','/','{}','UTF-8','UTF-8','closed',32,NULL,0,'2020-01-01T00:00:00Z',NULL);")?;
        for sequence in 0..32_i64 { db.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('quota-raw',?1,'stdout',?2,zeroblob(65536),'2020-01-01T00:00:01Z')",rusqlite::params![sequence,sequence*65536])?; }
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
    // The report can be incomplete or another GC pass can consume its rescan.
    // Check the durable bytes, not a per-pass accounting snapshot.
    control.store.database.call(|db| {
        let (chunks, bytes): (i64, i64) = db.query_row(
            "SELECT count(*), COALESCE(sum(length(bytes)), 0) FROM output_chunk WHERE execution_id=?1",
            ["quota-raw"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!((chunks, bytes), (32, 2097152));
        Ok(())
    }).await.unwrap();
    let original = "Quota-held instruction 日本語\nretry the original once";
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
            db.execute("UPDATE shell_execution SET state='exited',exit_code=0,output_complete=1,ended_at='2020-01-01T00:00:01Z' WHERE id='quota-raw'", [])?;
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
    let observed_snapshot: String = control.store.database.call(move|db|{
        let (snapshot,size,body):(String,Option<i64>,String)=db.query_row("SELECT s.id,s.observed_size,s.body FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[query],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_ne!(snapshot,absent_snapshot);assert_eq!(size,Some(instruction.len()as i64));assert_eq!(body,instruction);Ok(snapshot)
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
        Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
            thread_id: Some(thread.clone()),
            display: false,
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
    let unchanged = send_accepted(
        &control,
        &thread,
        "verify the persisted cwd after owner restart",
    )
    .await
    .turn_id
    .unwrap();
    let after_reopen = incoming(&control, &mut receive).await;
    assert_eq!(count_instruction(&after_reopen.body, instruction), 1);
    assert_eq!(count_instruction(&after_reopen.body, unrelated), 0);
    after_reopen
        .reply
        .send((200, answer("persisted cwd and agents path preserved")))
        .unwrap();
    settled(&control, &thread).await;
    // An unchanged file is observed into the previous Turn's snapshot row.
    let shared: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT instruction_snapshot_id FROM turn WHERE id=?1",
                [unchanged],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(shared, observed_snapshot);
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
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
            parent_thread_id: Some(parent.clone()),
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
    let accepted = send_accepted(&control, &target, "入力文字数の記録 🦀 \"quoted\"\n次の行").await;
    let turn_id = accepted.turn_id.expect("accepted Unicode measurement Turn");
    let sent = incoming(&control, &mut receive).await;
    let serialized_input = sent.body["input"].to_string();
    let characters = serialized_input.chars().count() as u64;
    assert!(
        serialized_input.len() as u64 > characters,
        "fixture must distinguish UTF-8 bytes"
    );
    let replay_thread = target.clone();
    let report_turn = turn_id.clone();
    let history_characters = control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM model_attempt WHERE turn_id=?1",
                    [&report_turn],
                    |row| row.get::<_, i64>(0)
                )?,
                1,
                "the accepted Turn has one committed Ordinary admission before this request"
            );
            let manifest: String = db.query_row(
                "SELECT input_manifest_json FROM model_attempt WHERE turn_id=?1",
                [report_turn],
                |row| row.get(0),
            )?;
            let manifest: Value = serde_json::from_str(&manifest).unwrap();
            assert_eq!(manifest["input_characters"], characters);
            let replay =
                shellboy::control::replay::ReplayCache::new(1_000_000).load(db, &replay_thread)?;
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
    let report_turn = turn_id;
    control.store.database.call(move |db| {
        let (manifest, usage): (String, String) = db.query_row(
            "SELECT input_manifest_json,usage_json FROM model_attempt WHERE turn_id=?1 AND state='completed'", [report_turn],
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
        Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
            thread_id: Some(target.clone()),
            display: false,
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
async fn thread_get_reports_uncertain_context_for_a_large_saved_thread() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path(), "http://127.0.0.1:1/v1").await;
    let control = Control::new(&owner, root.path().into());
    let target = thread(&control).await;
    let id = target.clone();
    control.store.database.call(move |db| {
        let tx = db.transaction()?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('bounded',?1,'completed','2026-01-01T00:00:00Z')",[&id])?;
        tx.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000) INSERT INTO turn(id,thread_id,run_id,state,created_at) SELECT printf('bound-%04d',i),?1,'bounded','completed','2026-01-01T00:00:00Z' FROM n",[&id])?;
        tx.execute("INSERT INTO model_attempt(kind,id,thread_id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at) SELECT 'conversation',u.id,u.thread_id,u.id,0,0,t.profile_id,t.model_id,0,'completed',json_object('adoption',json_object('outcome','adopted')),json_object('input_tokens',3),u.created_at FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.run_id='bounded' ORDER BY u.id",[])?;
        tx.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2500) INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json) SELECT 'bound-2000',i,'message','completed','{}' FROM n",[])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let details = request(
        &control,
        Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
            thread_id: Some(target),
            display: false,
        }),
    )
    .await;
    assert!(details.get("usage_totals").is_none());
    let usage = &details["context_usage"];
    assert_eq!(usage["last_successful_attempt"]["attempt_id"], "bound-2000");
    assert_eq!(usage["last_successful_attempt"]["input_tokens"], 3);
    assert_eq!(
        usage["last_successful_attempt"]["next_request_pending"],
        Value::Null
    );
    assert_eq!(
        usage["bounded_observation"]["output_coverage_complete"],
        false
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
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
            parent_thread_id: None,
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
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            agents: Some("middle.md".into()),
            ..Default::default()
        })),
    )
    .await;
    // Other settings still deliver at their normal boundary, not at a future Turn.
    request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            title: Some("changed".into()),
            effort: Some("brief".into()),
            ..Default::default()
        })),
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
        Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
            thread_id: Some(thread.clone()),
            display: false,
        }),
    )
    .await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            agents: Some("new.md".into()),
            ..Default::default()
        })),
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
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
        Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
            thread_id: Some(thread.clone()),
            display: false,
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
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            agents: Some("idle.md".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(
        saved_work(&control, &thread).await,
        before,
        "idle reference selection must not create or change Runs, Turns or attempts"
    );
    let target = thread.clone();
    control.store.database.call(move |db| {
        assert_eq!(db.query_row(
                "SELECT count(*) FROM event WHERE target_thread_id=?1 AND kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='agents_reference' AND target_run_id IS NULL",
                [&target], |r| r.get::<_,i64>(0))?, 1, "one idle reference event has no target Run");
        let (event, run): (String,Option<String>) = db.query_row(
                "SELECT id,target_run_id FROM event WHERE target_thread_id=?1 AND kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='agents_reference' AND target_run_id IS NULL",
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
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
    let (url, _receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let sent = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            max_model_requests: None,
            resume_recovery: false,

            human: false,
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
        let accepted = send_accepted(&control, &target, "synthetic failure usage").await;
        let turn_id = accepted.turn_id.expect("accepted synthetic failure Turn");
        let sent = incoming(&control, &mut receive).await;
        sent.reply.send((200,json!({"id":"failure-fixture","status":status,"error":{"code":"invalid_request_error","message":"synthetic diagnosis"},"usage":{"input_tokens":23,"input_tokens_details":{"cached_tokens":0,"appendix":"x".repeat(128*1024)},"output_tokens":3,"attribution":"x".repeat(128*1024)},"output":[]}))).unwrap();
        settled(&control, &target).await;
        let report_turn = turn_id;
        control.store.database.call(move |db| {
            let (state,usage,error):(String,String,String)=db.query_row("SELECT state,usage_json,error_json FROM model_attempt WHERE turn_id=?1",[report_turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
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
async fn automatic_nonshrinking_summary_enters_recovery_without_adoption_or_resend() {
    crate::test_watchdog::watchdog("tests/runtime_features.rs: automatic_nonshrinking_summary_enters_recovery_without_adoption_or_resend",async {
        let root=tempfile::tempdir().unwrap();let (url,mut receive,server)=proxy().await;
        let config=root.path().join("config.toml");
        std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=8\n[compaction]\nsummary_max_chars=2000\n")).unwrap();
        let owner=DataOwner::start(Some(config),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());let thread=thread(&control).await;
        send(&control,&thread,"protected task").await;
        // #182: with the tool loop open, overflow uses the active-Turn summary,
        // which needs an older tool round to summarize besides the latest one.
        incoming(&control,&mut receive).await.reply.send((200,json!({"id":"older","status":"completed","output":[{"type":"function_call","call_id":"older-tool","name":"shell","arguments":json!({"command":"echo older"}).to_string()}]}))).unwrap();
        incoming(&control,&mut receive).await.reply.send((200,json!({"id":"tool","status":"completed","output":[{"type":"function_call","call_id":"only-tool","name":"shell","arguments":json!({"command":"echo synthetic"}).to_string()}]}))).unwrap();
        let pending=incoming(&control,&mut receive).await;let target=thread.clone();
        control.store.database.call(move|db| {
            let tx=db.transaction()?;
            let request:String=tx.query_row("SELECT message_node_id FROM receipt WHERE target_thread_id=?1 ORDER BY created_at LIMIT 1",[&target],|r|r.get(0))?;
            let selected=tx.prepare("SELECT n.id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND (n.id=?2 OR json_extract(n.payload_json,'$.group_id') IS NOT NULL) ORDER BY p.ordinal")?.query_map(rusqlite::params![target,request],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let revision:i64=tx.query_row("SELECT selection_revision FROM thread WHERE id=?1",[&target],|r|r.get(0))?;
            shellboy::history::select_path(&tx,&target,revision as u64,&selected,"2026-10-31T00:00:00Z")?;tx.commit()?;Ok(())
        }).await.unwrap();
        let before = selected_history(&control, &thread).await;
        pending.reply.send((400,json!({"error":{"code":"context_length_exceeded","message":"fixture pressure"}}))).unwrap();
        let request_summary = incoming_summary(&control, &mut receive).await;
        assert_eq!(request_summary.body["tools"], json!([]));
        request_summary.reply.send((200, answer(&"x".repeat(20_000)))).unwrap();
        tokio::select! {
            biased;
            unexpected = receive.recv() => panic!("rejected summary sent another request: {:?}", unexpected.map(|r| r.body)),
            _ = settled(&control, &thread) => {}
        }
        let outcome = overflow_compaction(&control, &thread).await;
        assert_eq!(outcome["state"], "rejected");
        let turn = request(&control, Command::TurnGet(TurnTarget {
            turn_id: outcome["turn_id"].as_str().unwrap().to_owned(),
        })).await;
        assert_eq!(turn["state"], "recovery_wait");
        let after = selected_history(&control, &thread).await;
        let input_nodes = |window: &HistoryWindow| window.items.iter()
            .filter(|node| node.kind != "event" && node.kind != "incomplete_response")
            .map(|node| node.node_id.clone()).collect::<Vec<_>>();
        assert_eq!(input_nodes(&after), input_nodes(&before), "rejection retains the input path");
        assert!(!after.items.iter().any(|node| node.kind == "summary"));
        assert_eq!(summary_request_count_for_thread(&control, &thread).await, 1,
            "the persisted pre-send Summary ledger admits no second Summary POST");
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
        let mut tail = String::new();
        self.stdout.read_to_string(&mut tail).await.unwrap();
        let mut errors = String::new();
        self.stderr.read_to_string(&mut errors).await.unwrap();
        fn diagnostic_tail(text: &str) -> std::borrow::Cow<'_, str> {
            let bytes = text.as_bytes();
            String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(2048)..])
        }
        assert_eq!(
            status.success(),
            normal,
            "phase={:?}, exit={status}, stdout_tail={:?}, stderr_tail={:?}",
            record.file_stem(),
            diagnostic_tail(&tail),
            diagnostic_tail(&errors),
        );
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
            let mut accepted_run = None;
            if mode=="before_request" { expected_requests=0; }
            else {
                let created=headless_cli(&data, &["thread","create"]).await;
                let id=created["thread_id"].as_str().unwrap().to_owned();
                let accepted = headless_cli(&data, &["message","send","--thread",&id,"--text","run one counted tool"]).await;
                accepted_run = Some(accepted["run_id"].as_str().expect("accepted initial Run").to_owned());
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
                    let request=Request::new(format!("{}:{}",welcome["operation_epoch"].as_str().unwrap(),uuid::Uuid::new_v4()),Command::OwnerShutdown(OwnerShutdownArgs{instance_id:endpoint.instance_id}));
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
                let accepted_run = accepted_run.as_deref().expect("the saved initial Run identity");
                let (state,reason):(String,String)=db.query_row("SELECT state,stop_reason FROM run WHERE id=?1",[accepted_run],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
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
            let expected_total = expected_requests as usize + usize::from(mode!="before_request");
            assert_eq!(std::fs::read_to_string(&requests).unwrap().lines().count(), expected_total);
            assert_eq!(headless_db(&data).query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get::<_,i64>(0)).unwrap(), expected_total as i64,
                "every dispatched model request has its committed attempt row");
            eprintln!("headless-boundary {}",json!({"mode":mode,"first_requests":expected_requests,"all_requests":expected_requests+if mode=="before_request"{0}else{1},"effects":if mode=="before_request"{0}else{1},"first_stdout_rows":if normal{2}else{1},"restart_stdout_rows":2}));
            headless_cli(&data,&["shutdown"]).await;
            restarted.finish(true,&root.path().join("restart-stdout.jsonl")).await;
            proxy_task.abort(); let _=proxy_task.await;
        }).await.unwrap();
    }
}

#[tokio::test]
async fn headless_shutdown_requires_explicit_directory() {
    crate::test_watchdog::watchdog("headless shutdown addressing", async {
        let root = tempfile::tempdir().unwrap();
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
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn headless_shutdown_final_output_loss_is_not_success() {
    crate::test_watchdog::watchdog("headless shutdown final output failure", async {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root.path().join("data");
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
    })
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
            let child_accepted = headless_cli(&child_data, &["message", "send", "--thread", &child_thread, "--text", "one counted nested tool"]).await;
            let child_run = child_accepted["run_id"].as_str().expect("accepted nested Run").to_owned();
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
            let parent_run = accepted["run_id"].as_str().expect("accepted parent Run");
            let execution: String = headless_db(&parent_data).query_row("SELECT id FROM shell_execution WHERE owner_run_id=?1", [parent_run], |r| r.get(0)).unwrap();
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
            let (state, reason): (String,String) = db.query_row("SELECT state,stop_reason FROM run WHERE id=?1", [&child_run], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert_eq!(state, "stopped");
            assert!(matches!(reason.as_str(), "app_shutdown" | "previous_instance_interrupted"));
            drop(db);
            assert_eq!(std::fs::read_to_string(&effects).unwrap(), "once\n");
            assert_eq!(std::fs::read_to_string(&parent_log).unwrap().lines().count(), 2);
            assert_eq!(std::fs::read_to_string(&child_log).unwrap().lines().count(), 1);
            assert_eq!(headless_db(&child_data).query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
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
async fn headless_repeated_term_does_not_escalate_drain() {
    crate::test_watchdog::watchdog("headless repeated TERM during drain", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut requests, proxy_task) = proxy().await;
        let config = root.path().join("config.toml");
        // The grace is only the owner's own TERM-to-KILL bound for the held
        // shell; the test releases the hold long before it could matter.
        std::fs::write(&config, format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n[io]\nterm_grace_ms=3600000\n")).unwrap();
        let data = root.path().join("data");
        let owner = HeadlessOwner::start(&data, &config).await;
        let identity = owner.ready["instance_id"].clone();
        let endpoint: shellboy::app::Endpoint =
            serde_json::from_slice(&std::fs::read(data.join("endpoint.json")).unwrap()).unwrap();
        let thread = headless_cli(&data, &["thread", "create"]).await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        headless_cli(&data, &["message", "send", "--thread", &thread, "--text", "hold a shell through drain"]).await;
        // The hold: a running shell that ignores TERM until the test creates
        // the release file. Shutdown waits for it, so the drain stays in
        // progress exactly as long as the test decides.
        let started = root.path().join("started");
        let release = root.path().join("release");
        let command = format!(
            "trap '' TERM; printf x > {}; while [ ! -e {} ]; do sleep 0.05; done",
            serve_quote(&started),
            serve_quote(&release)
        );
        let incoming = requests.recv().await.unwrap();
        incoming.reply.send((200, json!({"id":"hold","status":"completed","output":[{
            "type":"function_call","call_id":"hold-shell","name":"shell",
            "arguments":json!({"command":command}).to_string()
        }]}))).unwrap();
        while !started.exists() {
            tokio::task::yield_now().await;
        }
        let pid = owner.child.id().unwrap() as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
        // The endpoint closes once the owner has begun draining.
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
        std::fs::write(&release, "").unwrap();
        owner
            .finish(true, &root.path().join("signal-drain.jsonl"))
            .await;

        let reopened = HeadlessOwner::start(&data, &config).await;
        assert_ne!(reopened.ready["instance_id"], identity);
        headless_cli(&data, &["shutdown"]).await;
        reopened
            .finish(true, &root.path().join("restart.jsonl"))
            .await;
        proxy_task.abort();
    })
    .await
    .unwrap();
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
            assert_eq!(headless_db(&data).query_row("SELECT COUNT(*) FROM model_attempt WHERE turn_id IN (SELECT id FROM turn WHERE run_id=?1)", [run], |r| r.get::<_,i64>(0)).unwrap(), 1,
                "the exact recovery Run has no attempt beyond its received request");
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
    shellboy::owner_session::initialize_reporting(
        Some(root.join("config.toml")),
        Some(root.join("data")),
        tokio_util::sync::CancellationToken::new(),
        callback,
    )
    .await
    .unwrap()
    .unwrap();
    panic!("unknown cutpoint");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_native_startup_kills_keep_committed_facts() {
    crate::test_watchdog::watchdog("native owner startup interruption", async {
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
        for cut in ["startup_database", "startup_interrupted"] {
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
        let accepted = send_accepted(&control,&thread,"foreground and background gate").await;
        let accepted_run = accepted.run_id.expect("accepted shell gate Run");
        let response=json!({"id":"tool-gates","status":"completed","output":[
            {"type":"function_call","call_id":"background","name":"shell","arguments":json!({"command":"cat >/dev/null # background-gate","background":true,"stdin_open":true}).to_string()},
            {"type":"function_call","call_id":"foreground","name":"shell","arguments":json!({"command":"read -r line; exit 1 # foreground-gate","stdin_open":true}).to_string()}
        ]});
        let mut events=control.subscribe();incoming(&control,&mut receive).await.reply.send((200,response)).unwrap();
        let (background,foreground,run)=loop {
            let rows=control.store.database.call({ let run = accepted_run.clone(); move|db|{
                let mut q=db.prepare("SELECT id,owner_run_id,json_extract(shell_json,'$.command') FROM shell_execution WHERE owner_run_id=?1 ORDER BY rowid")?;
                q.query_map([run],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>().map_err(Into::into)
            }}).await.unwrap();
            if rows.len()==2 {break(rows[0].0.clone(),rows[1].0.clone(),rows[0].1.clone());}
            let _=events.recv().await;
        };
        let accepted=request(&control,Command::Compact(ThreadTarget {thread_id:Some(thread.clone())})).await;
        request(&control,Command::ShellEof(ExecutionTarget {execution_id:foreground.clone()})).await;
        let summary=incoming_summary(&control,&mut receive).await;
        assert_eq!(summary.body["tools"],json!([]),"the boundary owner issues the summary, not a new ordinary attempt");
        let blocked=request(&control,Command::RunGet(RunTarget {run_id:run.clone()})).await;
        assert_eq!(blocked["waiting_reason"]["kind"],"context_edit_boundary");
        assert_eq!(blocked["waiting_reason"]["operation_id"],accepted["operation_id"]);
        assert_eq!(blocked["waiting_reason"]["action"],"compact");
        let projected=thread_get_projection(&control,&thread).await;
        assert_eq!(serde_json::to_value(projected.snapshot.waiting_reason).unwrap(),blocked["waiting_reason"]);
        // Finishing the background shell cannot release the held summary boundary.
        request(&control,Command::ShellEof(ExecutionTarget {execution_id:background.clone()})).await;
        loop {
            let ended=request(&control,Command::ShellGet(ExecutionTarget {execution_id:background.clone()})).await;
            if ended["state"]=="exited" {break;}let _=events.recv().await;
        }
        let still=request(&control,Command::RunGet(RunTarget {run_id:run.clone()})).await;
        assert_eq!(still["waiting_reason"],blocked["waiting_reason"]);
        // Summary ledger `begin` commits before POST; the one held Summary remains the only admission.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &thread).await, 1);
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
        assert_eq!(model_attempt_count_for_thread(&control, &target).await, 2,
            "the two named Ordinary requests are the only committed attempts");
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
#[derive(Clone, Debug)]
struct ExpectedAutomaticAttempt {
    id: String,
    token: String,
}
#[derive(Clone, Debug)]
struct ReportOwner {
    attempt_id: String,
    row_turn_id: String,
    input_tokens: u64,
    request_token: String,
}

#[derive(Clone, Debug)]
struct PreparedReport {
    owner: ReportOwner,
}

#[derive(Clone, Debug)]
struct TriggerOwner {
    run_id: String,
    turn_id: String,
}

/// One immutable capture: original report owner and current Driver owner stay distinct.
#[derive(Clone, Debug)]
struct ReportBoundary {
    report: ReportOwner,
    trigger: TriggerOwner,
    gc_token: String,
}

#[derive(Clone, Debug)]
pub(super) struct FinishedReportBoundary {
    boundary: ReportBoundary,
    outcome: String,
}

async fn compact_attempt(control: &Arc<Control>, thread: &str, attempt: &str) -> Option<Value> {
    let result = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            Command::CompactList(CompactListArgs {
                thread_id: Some(thread.into()),
                since: None,
                until: None,
                limit: 50,
            }),
        ))
        .await;
    assert!(result.ok, "{:?}", result.error);
    let page = result.result.unwrap();
    assert_eq!(page["more"], false, "fixture fits in one compact.list page");
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["attempt_id"] == attempt)
        .cloned()
}

async fn automatic_attempt_record(control: &Arc<Control>, attempt: String) -> Value {
    let id = attempt.clone();
    let thread: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT thread_id FROM model_attempt WHERE id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    compact_attempt(control, &thread, &attempt)
        .await
        .expect("captured compact.list attempt")
}

// The fixture's FINISH handshake uses the internal token, not a display field.
async fn automatic_token(control: &Control, attempt: &str) -> String {
    let attempt = attempt.to_owned();
    control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT token FROM automatic_compaction WHERE attempt_id=?1",
                [attempt],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap()
}

async fn prepared_report_from_incoming(
    control: &Arc<Control>,
    report_turn_id: &str,
    request_ordinal: usize,
    request: &Incoming,
    input_tokens: u64,
) -> PreparedReport {
    assert_eq!(
        request.kind(),
        ProviderRequestKind::Ordinary,
        "reports originate from an ordinary request"
    );
    let turn = report_turn_id.to_owned();
    // The Driver awaits the full request response before looping to admit another
    // attempt (`src/control/driver.rs:384-400, 548-586`); rows are inserted before
    // send (`src/control/driver/request.rs:114-139, 408`). Thus the k-th Ordinary
    // proxy request in a Turn is its k-th row, without consulting mutable state.
    let ordinal = (request_ordinal - 1) as i64;
    let (attempt_id, request_token): (String, Option<String>) = control
        .store
        .database
        .call(move |db| {
            db.query_row(
                "SELECT id,json_extract(input_manifest_json,'$.compaction_report_token') FROM model_attempt WHERE turn_id=?1 AND kind='conversation' ORDER BY rowid LIMIT 1 OFFSET ?2",
                rusqlite::params![turn, ordinal],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    PreparedReport {
        owner: ReportOwner {
            attempt_id,
            row_turn_id: report_turn_id.to_owned(),
            input_tokens,
            request_token: request_token
                .filter(|token| !token.is_empty())
                .expect("saved report request token"),
        },
    }
}

async fn prepare_report_from_thread(
    control: &Arc<Control>,
    thread: &str,
    report_turn_id: &str,
    input_tokens: u64,
) -> PreparedReport {
    let details = request(
        control,
        Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
            thread_id: Some(thread.to_owned()),
            display: false,
        }),
    )
    .await;
    let report = &details["context_usage"]["last_successful_attempt"];
    assert_eq!(
        report["input_tokens"], input_tokens,
        "canonical preflight report usage is independently declared"
    );
    assert_eq!(
        report["next_request_pending"], false,
        "the captured preflight report is eligible before start"
    );
    let attempt_id = report["attempt_id"]
        .as_str()
        .expect("canonical report attempt id")
        .to_owned();
    let query_id = attempt_id.clone();
    let (turn,request_token):(String,Option<String>)=control.store.database.call(move|db|db.query_row(
        "SELECT turn_id,json_extract(input_manifest_json,'$.compaction_report_token') FROM model_attempt WHERE id=?1",
        [query_id],|row|Ok((row.get(0)?,row.get(1)?))
    ).map_err(Into::into)).await.unwrap();
    assert_eq!(
        turn, report_turn_id,
        "canonical completed report row owns the expected source Turn"
    );
    PreparedReport {
        owner: ReportOwner {
            attempt_id,
            row_turn_id: turn,
            input_tokens,
            request_token: request_token
                .filter(|token| !token.is_empty())
                .expect("saved report request token"),
        },
    }
}

#[derive(Clone, Debug)]
struct KnownFirstA {
    report_attempt_id: String,
    input_tokens: u64,
}

async fn known_first_a_from_reports(
    control: &Arc<Control>,
    thread: &str,
    candidates: &[&PreparedReport],
    input_tokens: u64,
) -> KnownFirstA {
    assert!(
        !candidates.is_empty(),
        "the source ledger names its possible first-A reports"
    );
    let thread = thread.to_owned();
    let (known,owner,token):(Option<u64>,Option<String>,Option<String>)=control.store.database.call(move|db|{
        let state:Option<String>=db.query_row("SELECT value FROM app_meta WHERE key=?1",[format!("compact.a_state.{thread}")],|row|row.get(0)).optional()?;
        let (known,owner)=state.map(|text|{
            let value:Value=serde_json::from_str(&text).expect("bounded #A state is valid JSON");
            (value["first"]["input_tokens"].as_u64(),value["first"]["attempt"].as_str().map(str::to_owned))
        }).unwrap_or((None,None));
        let token:Option<String>=if let Some(owner)=owner.as_deref(){db.query_row("SELECT json_extract(input_manifest_json,'$.compaction_report_token') FROM model_attempt WHERE id=?1",[owner],|row|row.get(0)).optional()?.flatten()}else{None};
        Ok((known,owner,token))
    }).await.unwrap();
    assert_eq!(
        known,
        Some(input_tokens),
        "source report establishes the prescribed first-known #A"
    );
    let report = candidates
        .iter()
        .copied()
        .find(|report| owner.as_deref() == Some(report.owner.attempt_id.as_str()))
        .expect("first-known report is one of the explicitly named bridge requests");
    assert_eq!(
        token.as_deref(),
        Some(report.owner.request_token.as_str()),
        "first-known report preserves its own manifest token"
    );
    KnownFirstA {
        report_attempt_id: report.owner.attempt_id.clone(),
        input_tokens,
    }
}

fn prepare_report_after_finish(
    finished: &FinishedReportBoundary,
    expected_outcome: &str,
    first_a: &KnownFirstA,
    expected_first_a: u64,
    prepared: PreparedReport,
) -> PreparedReport {
    assert_eq!(
        finished.outcome, expected_outcome,
        "fresh report follows the exact named terminal receipt"
    );
    assert_eq!(
        first_a.input_tokens, expected_first_a,
        "source transition establishes its prescribed first-A band"
    );
    assert_ne!(
        first_a.report_attempt_id, finished.boundary.report.attempt_id,
        "the consumed terminal report cannot be misidentified as the explicit first-A source"
    );
    assert_ne!(
        prepared.owner.attempt_id, finished.boundary.report.attempt_id,
        "fresh threshold eligibility names a report row distinct from the consumed FINISH"
    );
    prepared
}

async fn start_report_boundary(
    control: &Arc<Control>,
    event: &Value,
    prepared: PreparedReport,
    trigger: &MessageAccepted,
) -> ReportBoundary {
    let owner = prepared.owner;
    assert_eq!(
        event["attempt_id"], owner.attempt_id,
        "preflight start consumes the captured report identity"
    );
    assert_eq!(
        event["turn_id"], owner.row_turn_id,
        "event keeps original report Turn"
    );
    assert_eq!(event["trigger"]["input_tokens"], owner.input_tokens);
    let trigger_owner = TriggerOwner {
        run_id: trigger
            .run_id
            .as_deref()
            .expect("accepted trigger Run")
            .to_owned(),
        turn_id: trigger
            .turn_id
            .as_deref()
            .expect("accepted trigger Turn")
            .to_owned(),
    };
    let (usage,request_token):(Option<i64>,String)=control.store.database.call({
        let attempt=owner.attempt_id.clone();
        move|db|db.query_row("SELECT json_extract(usage_json,'$.input_tokens'),json_extract(input_manifest_json,'$.compaction_report_token') FROM model_attempt WHERE id=?1",[attempt],|row|Ok((row.get(0)?,row.get(1)?))).map_err(Into::into)
    }).await.unwrap();
    assert_eq!(
        usage,
        Some(owner.input_tokens as i64),
        "the named report row saved the expected response usage"
    );
    assert_eq!(
        request_token, owner.request_token,
        "the saved report request token remains with its original row"
    );
    let gc_token = automatic_token(control, &owner.attempt_id).await;
    ReportBoundary {
        report: owner,
        trigger: trigger_owner,
        gc_token,
    }
}

async fn admitted_summary_boundary(
    control: &Arc<Control>,
    reports: &[ReportOwner],
    trigger: TriggerOwner,
) -> ReportBoundary {
    let mut active = Vec::new();
    for report in reports {
        let id = report.attempt_id.clone();
        let thread: String = control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT thread_id FROM model_attempt WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        let Some(record) = compact_attempt(control, &thread, &report.attempt_id).await else {
            continue;
        };
        if record["state"] != "unknown" {
            continue;
        }
        assert_eq!(record["trigger"]["input_tokens"], report.input_tokens);
        let attempt_id = report.attempt_id.clone();
        let (usage, request_token): (Option<i64>, Option<String>) = control.store.database.call(move |db| {
            db.query_row(
                "SELECT json_extract(usage_json,'$.input_tokens'),json_extract(input_manifest_json,'$.compaction_report_token') FROM model_attempt WHERE id=?1",
                [attempt_id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(Into::into)
        }).await.unwrap();
        assert_eq!(
            usage,
            Some(report.input_tokens as i64),
            "admitted summary retains original report usage"
        );
        assert_eq!(
            request_token.as_deref(),
            Some(report.request_token.as_str()),
            "admitted summary retains original request token"
        );
        let gc_token = automatic_token(control, &report.attempt_id).await;
        active.push(ReportBoundary {
            report: report.clone(),
            trigger: trigger.clone(),
            gc_token,
        });
    }
    assert_eq!(
        active.len(),
        1,
        "the actual Summary request has exactly one active accepted source duty: {active:#?}"
    );
    active.pop().expect("one accepted source duty")
}

pub(super) async fn finish_proactive_trace(
    control: &Arc<Control>,
    receive: &mut ProxyInbox,
    trace: &mut ProactiveTrace,
    summary_reply: &(u16, Value),
) -> Vec<FinishedReportBoundary> {
    // Seed the wait from the second warmup report, even when its Summary
    // request has not reached the provider inbox yet.
    let original = trace
        .report_owners
        .last()
        .expect("second warmup report")
        .clone();
    let mut finished = Vec::new();
    let mut joined = std::collections::HashSet::new();
    while let Some(report) = std::iter::once(&original)
        .chain(
            trace
                .admitted_summaries
                .iter()
                .map(|boundary| &boundary.report),
        )
        .find(|report| !joined.contains(&report.attempt_id))
        .cloned()
    {
        let mut finish = Box::pin(automatic_report_finished(control, &report.attempt_id));
        let terminal = loop {
            tokio::select! {
                attempts = &mut finish => break attempts,
                incoming = receive.recv() => {
                    let incoming = incoming.expect("provider remains available while captured automatic duties FINISH");
                    let (request_run, request_turn) = request_scope(&incoming.body);
                    match incoming.kind() {
                        ProviderRequestKind::Summary => {
                            let admitted = admitted_summary_boundary(
                                control,
                                &trace.report_owners,
                                TriggerOwner { run_id: request_run, turn_id: request_turn.clone() },
                            ).await;
                            if !trace.admitted_summaries.iter().any(|known| known.report.attempt_id == admitted.report.attempt_id) {
                                trace.admitted_summaries.push(admitted);
                            }
                            trace.summary_requests += 1;
                            trace.summary_bodies.push(incoming.body.clone());
                            incoming.reply.send(summary_reply.clone()).unwrap();
                        }
                        ProviderRequestKind::Ordinary => {
                            let request_ordinal = trace.report_owners.iter().filter(|owner| owner.row_turn_id == request_turn).count() + 1;
                            let report = prepared_report_from_incoming(control, &request_turn, request_ordinal, &incoming, 10).await;
                            trace.report_owners.push(report.owner);
                            incoming.reply.send((200, answer_with_usage("drained ordinary request", 10))).unwrap();
                        }
                    }
                }
            }
        };
        joined.insert(report.attempt_id.clone());
        let record = automatic_attempt_record(control, report.attempt_id.clone()).await;
        // A saved automatic attempt can finish without any Summary POST.
        // Keep that FINISH as the count boundary, not as a serviced request.
        if let Some(boundary) = trace
            .admitted_summaries
            .iter()
            .find(|boundary| boundary.report.attempt_id == report.attempt_id)
        {
            finished.push(boundary.clone().finish(&terminal, &record));
        }
    }
    finished
}

impl ReportBoundary {
    fn finish(self, terminal: &ExpectedAutomaticAttempt, record: &Value) -> FinishedReportBoundary {
        assert_eq!(record["attempt_id"], terminal.id);
        assert_eq!(
            terminal.id, self.report.attempt_id,
            "FINISH retains report row"
        );
        assert_eq!(
            terminal.token, self.gc_token,
            "FINISH retains automatic GC token"
        );
        assert!(
            matches!(
                record["state"].as_str(),
                Some("adopted" | "failed" | "no_target" | "cancelled" | "run_stopped")
            ),
            "unexpected terminal: {record:#?}"
        );
        FinishedReportBoundary {
            boundary: self,
            outcome: record["state"].as_str().unwrap().to_owned(),
        }
    }
}

async fn automatic_report_finished(control: &Control, attempt: &str) -> ExpectedAutomaticAttempt {
    let mut changes = control.subscribe();
    crate::test_watchdog::watchdog("captured automatic report FINISH", async {
        loop {
            let id = attempt.to_owned();
            let row = control
                .store
                .database
                .call(move |db| {
                    db.query_row(
                        "SELECT token,outcome FROM automatic_compaction WHERE attempt_id=?1",
                        [id],
                        |row| {
                            Ok((
                                row.get::<_, Option<String>>(0)?,
                                row.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(Into::into)
                })
                .await
                .unwrap_or_else(|error| panic!("report {attempt} did not save FINISH: {error}"));
            if let Some((token, Some(outcome))) = row {
                assert!(
                    matches!(
                        outcome.as_str(),
                        "adopted" | "failed" | "no_target" | "cancelled"
                    ),
                    "unexpected terminal outcome {outcome} for {attempt}"
                );
                return ExpectedAutomaticAttempt {
                    id: attempt.to_owned(),
                    token: token
                        .filter(|token| !token.is_empty())
                        .expect("finished report has a nonempty token"),
                };
            }
            match changes.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("report {attempt} FINISH notification closed: {error}"),
            }
        }
    })
    .await
    .unwrap_or_else(|error| panic!("report {attempt} did not save FINISH: {error}"))
}

async fn automatic_attempts_finished_for_turns(
    control: &Control,
    turn_ids: &[String],
    expected_count: usize,
) -> Vec<ExpectedAutomaticAttempt> {
    assert!(
        expected_count > 0,
        "terminal wait requires an expected operation"
    );
    assert_eq!(
        turn_ids.len(),
        1,
        "this fixture contract names the exact triggering Turn"
    );
    let mut changes = control.subscribe();
    let turns = turn_ids.to_vec();
    crate::test_watchdog::watchdog("tests/runtime_features.rs: exact automatic FINISH", async {
        let attempts = loop {
            let turns = turns.clone();
            let rows = control.store.database.call(move |db| {
                let mut statement = db.prepare("SELECT a.id,(SELECT c.token FROM automatic_compaction c WHERE c.attempt_id=a.id) FROM model_attempt a WHERE a.turn_id=?1 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=a.id) ORDER BY a.rowid")?;
                statement.query_map([&turns[0]], |row| {
                    let token: Option<String> = row.get(1)?;
                    Ok((row.get::<_, String>(0)?, token))
                })?.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
            }).await.unwrap();
            assert!(rows.len() <= expected_count, "unexpected extra automatic attempts for the named Turn: {rows:?}");
            if rows.len() == expected_count {
                break rows.into_iter().map(|(id, token)| ExpectedAutomaticAttempt {
                    id,
                    token: token.filter(|token| !token.is_empty()).expect("registered attempt has a nonempty token"),
                }).collect::<Vec<_>>();
            }
            match changes.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                Err(error) => panic!("exact automatic attempt receipt did not arrive: {error}"),
            }
        };
        loop {
            let ids = attempts.iter().map(|attempt| attempt.id.clone()).collect::<Vec<_>>();
            let tokens = attempts.iter().map(|attempt| attempt.token.clone()).collect::<Vec<_>>();
            let pending = control.store.database.call(move |db| {
                let mut statement = db.prepare("SELECT id,(SELECT c.token FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id),(SELECT c.outcome IS NULL FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id),(SELECT COALESCE(c.outcome,'unknown') FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id) FROM model_attempt WHERE id=?1")?;
                for (id, token) in ids.iter().zip(tokens.iter()) {
                    let row = statement.query_row([id], |row| Ok((
                        row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?, row.get::<_, Option<String>>(3)?,
                    )))?;
                    assert_eq!(&row.0, id, "exact automatic attempt identity remains present");
                    assert_eq!(row.1.as_deref(), Some(token.as_str()), "exact attempt token remains bound");
                    let provisional = row.2.expect("FINISH fact has a typed provisional field");
                    let outcome = row.3.as_deref().expect("FINISH fact has a typed outcome field");
                    assert!(provisional == 0 || provisional == 1, "invalid provisional value {provisional}");
                    if provisional != 0 || outcome == "unknown" {
                        return Ok(true);
                    }
                    assert!(matches!(outcome, "adopted" | "failed" | "no_target" | "cancelled"),
                        "unexpected terminal outcome {outcome}");
                }
                Ok(false)
            }).await.unwrap();
            if !pending { return attempts; }
            match changes.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                Err(error) => panic!("exact automatic attempt FINISH notification closed: {error}"),
            }
        }
    }).await.unwrap()
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
    assert_eq!(listed["limit"], 50);
    assert_eq!(listed["filters"], json!({"thread_id": thread}));
    assert!(listed["more"].is_boolean());
    assert!(listed.get("next_cursor").is_none());
    listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["reason"] == "proactive_threshold")
        .cloned()
        .collect()
}
async fn attempt_count_for_run(control: &Control, run_id: &str) -> i64 {
    let run_id = run_id.to_owned();
    control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.run_id=?1",
                [run_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_closed_spill_then_cancelled_summary_preserves_adoption() {
    proactive_partial_spill_fixture(true, Some(false)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_active_spill_then_cancelled_summary_preserves_adoption() {
    proactive_partial_spill_fixture(true, Some(true)).await;
}

/// #303 automatic attempt over a Turn with two large older tool groups.
/// `oversized` adds a large protected request message; `cancel_active` stops
/// the Run at the summary HTTP boundary of the closed (`Some(false)`) or the
/// active (`Some(true)`) region. The first known provider report (#A, §7) is
/// 100 for the summary branch and, for the spill-only variant, exactly the
/// target 40 (`#A <= target`, §2.6).
async fn proactive_partial_spill_fixture(oversized: bool, cancel_active: Option<bool>) {
    // Ordering contract: tests observe a provider POST only after its model_attempt
    // admission transaction is committed (driver/request.rs:116-135, INSERT at :408;
    // driver.rs:390-399 sends only after prepare_attempt returns). No later DB state
    // transition is ordered against proxy parsing, so select admitted attempts by Turn
    // and row order, and wait for request arrival with recv under the fixture watchdog.
    let entered_phase = std::cell::RefCell::new("fixture setup".to_owned());
    let completed_phase = std::cell::RefCell::new("fixture setup".to_owned());
    let result=crate::test_watchdog::watchdog("proactive partial spill adoption", async {
let root = tempfile::tempdir().unwrap();
let (url, mut receive, server) = proxy().await;
let options = if oversized {
    "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n"
} else {
    "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n"
};
// Threshold 200 x default 20% = target 40.
let first_known_a = if oversized { 100 } else { 40 };
let owner = if cancel_active == Some(true) {
    // Use one source-defined transient Ordinary retry to release the live
    // boundary during backoff; no tool group or shell execution is added.
    let config = root.path().join("config.toml");
    std::fs::write(&config, format!(
        "default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=1\nresponse_retry_max_delay_ms=1000\n{options}"
    )).unwrap();
    DataOwner::start(Some(config), Some(root.path().join("data")), root.path().into()).await.unwrap()
} else {
    proactive_owner(root.path(), &url, options).await
};
let control = Control::new(&owner, root.path().into());
let target = thread(&control).await;
for text in ["hi", "yo"].into_iter().take(if cancel_active == Some(true) { 0 } else { 2 }) {
    *entered_phase.borrow_mut()=format!("starting earlier Turn {text}");
    send(&control, &target, text).await;
    *completed_phase.borrow_mut()=entered_phase.borrow().clone();
    *entered_phase.borrow_mut()=format!("awaiting earlier Turn {text} ordinary request");
    incoming(&control, &mut receive).await.reply.send((200, answer("ok"))).unwrap();
    *completed_phase.borrow_mut()=entered_phase.borrow().clone();
    *entered_phase.borrow_mut()=format!("settling earlier Turn {text}");
    settled(&control, &target).await;
    *completed_phase.borrow_mut()=entered_phase.borrow().clone();
}
let current = if oversized { "protected current ".repeat(70_000) } else { "long current turn".to_owned() };
*entered_phase.borrow_mut()="starting oversized/tool-loop current Turn".to_owned();
let current_accept=send_accepted(&control,&target,&current).await;
let report_turn_id=current_accept.turn_id.clone().expect("report-owning Turn id");
let mut trigger_accept=if cancel_active==Some(true){Some(current_accept.clone())}else{None};
*completed_phase.borrow_mut()=entered_phase.borrow().clone();
let tool_response = |index: u32, tokens: u32| json!({"id":format!("small-{index}"),"status":"completed","usage":{"input_tokens":tokens,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("small-{index}"),"name":"shell","arguments":json!({"command":"echo last"}).to_string()}]});
let large_command = if cfg!(windows) { "'0' * 50000" } else { "printf '%050000d' 0" };
for index in 0..2 {
    *entered_phase.borrow_mut()=format!("awaiting large tool-loop request index={index}");
    let current = incoming(&control, &mut receive).await;
    *completed_phase.borrow_mut()=entered_phase.borrow().clone();
    current.reply.send((200, json!({"id":format!("large-{index}"),"status":"completed", "usage":{"input_tokens":first_known_a,"output_tokens":1}, "output":[{
        "type":"function_call","call_id":format!("large-{index}"),"name":"shell",
        "arguments":json!({"command":large_command}).to_string()
    }]}))).unwrap();
}
*entered_phase.borrow_mut()="awaiting third tool-loop request/final known-A report".to_owned();
let before = incoming(&control, &mut receive).await;
*completed_phase.borrow_mut()=entered_phase.borrow().clone();
let before_size = before.body["input"].as_array().unwrap().iter().filter_map(|item| item.get("output").and_then(Value::as_str)).map(str::len).sum::<usize>();
assert!(before_size > 30_000, "fixture must contain both large tool outputs: {before_size}");
// Active cancellation triggers at this tool boundary; the other cases
// report the threshold on the final answer for a closed-Turn attempt.
let mut prepared_report=if cancel_active==Some(true) {
    Some(prepared_report_from_incoming(&control,&report_turn_id,3,&before,200).await)
}else{None};
let mut third_group = tool_response(2, if cancel_active == Some(true) { 200 } else { first_known_a });
if oversized && cancel_active.is_none() {
    // This group is protected at the first attempt. The retry's new group
    // displaces that protection, so do not give the older group reducible output.
    third_group["output"][0]["name"] = json!("fixture_unknown_tool");
    third_group["output"][0]["arguments"] = json!("{}");
}
before.reply.send((200, third_group)).unwrap();
if cancel_active != Some(true) {
    *entered_phase.borrow_mut()="awaiting final model report after tool completion".to_owned();
    let final_report=incoming_ordinary(&control,&mut receive).await;
    prepared_report=Some(prepared_report_from_incoming(&control,&report_turn_id,4,&final_report,200).await);
    final_report.reply.send((200, answer_with_usage("long turn done", 200))).unwrap();
    *completed_phase.borrow_mut()=entered_phase.borrow().clone();
    *entered_phase.borrow_mut()="settling report-producing foreground Turn".to_owned();
    settled(&control, &target).await;
    *completed_phase.borrow_mut()=entered_phase.borrow().clone();
}
if !oversized && cancel_active.is_none() {
    // Preflight consumes the completed report at the next request
    // boundary. This starts the real next ordinary Run; finish it
    // before waiting for the Thread-owned spill-only attempt.
    let spill_trigger=send_accepted(&control,&target,"next turn activates spill-only preflight").await;
    let boundary_request=incoming_ordinary(&control,&mut receive).await;
    boundary_request.reply.send((200,answer_with_usage("boundary complete",10))).unwrap();
    settled(&control,&target).await;
    // #A is exactly the target: this boundary is spill-only and makes
    // no summary HTTP request. Wait for the one source-identified
    // attempt after its foreground Turn has completed, then test the
    // adopted replay on the next real ordinary request.
    let events=proactive_events(&control,&target).await;
    assert_eq!(events.len(),1,"one predeclared threshold report owns one spill-only attempt: {events:#?}");
    assert_eq!(events[0]["trigger"]["input_tokens"],200);
    let boundary=start_report_boundary(&control,&events[0],prepared_report.clone().expect("completed spill report was captured before trigger Run"),&spill_trigger).await;
    let terminal=automatic_attempts_finished_for_turns(&control,std::slice::from_ref(&boundary.report.row_turn_id),1).await;
    assert_eq!(terminal.len(),1);
    let attempts=bx_attempts(&control,&target).await;
    assert_eq!(attempts.len(),1,"{attempts:#?}");
    let record=&attempts[0];
    boundary.finish(&terminal[0],record);
    assert_eq!(record["state"],"adopted");
    assert_eq!(record["trigger"]["input_tokens"],200);
    assert_eq!(selected_compaction_nodes(&control, &target, "spill").await.len(), 2);
    assert!(selected_compaction_nodes(&control, &target, "summary").await.is_empty());
    assert!(events[0]["metrics"]["summary_node_id"].is_null());
    send(&control,&target,"verify spill-only replay after FINISH").await;
    let resumed=incoming_ordinary(&control,&mut receive).await;
    let replay_size=resumed.body["input"].as_array().unwrap().iter()
        .filter_map(|item|item.get("output").and_then(Value::as_str)).map(str::len).sum::<usize>();
    assert!(replay_size<before_size,"the next request sees the adopted two-spill replay shrink: {before_size}->{replay_size}");
    resumed.reply.send((200,answer("finished"))).unwrap();
    settled(&control,&target).await;
    // A summary ledger row is committed by single_request.rs:599-602 before
    // the summary HTTP request is sent. Its absence proves no summary POST was
    // admitted for this exact spill-only trigger Run; do not snapshot the proxy queue.
    let spill_run = spill_trigger.run_id.clone().expect("accepted spill-only trigger Run");
    let summary_requests: i64 = control.store.database.call(move |db| {
        Ok(db.query_row(
            "SELECT count(*) FROM model_attempt a JOIN turn t ON a.turn_id=t.id WHERE a.kind='summary' AND t.run_id=?1",
            [spill_run],
            |row| row.get(0),
        )?)
    }).await.unwrap();
    assert_eq!(summary_requests, 0, "spill-only trigger Run has no admitted summary request");
    *entered_phase.borrow_mut()="shutting down partial-spill control/owner".to_owned();
control.shutdown().await.unwrap(); owner.shutdown().await.unwrap(); server.abort();
*completed_phase.borrow_mut()=entered_phase.borrow().clone();
    return;
}
if cancel_active!=Some(true) {
    *entered_phase.borrow_mut()="starting next Run at the saved report boundary".to_owned();
    trigger_accept=Some(send_accepted(&control,&target,"next turn").await);
    *completed_phase.borrow_mut()=entered_phase.borrow().clone();
}
*entered_phase.borrow_mut()="awaiting actual Summary HTTP request".to_owned();
let trigger_accept=trigger_accept.expect("the current report cut has an accepted trigger Run");
let mut ordinary_failures = 0;
let summary = if cancel_active == Some(true) {
    loop {
        let request = receive.recv().await.expect("provider remains available during active backoff");
        if request.kind() == ProviderRequestKind::Summary { break request; }
        assert_eq!(request.kind(), ProviderRequestKind::Ordinary);
        assert!(ordinary_failures < 2,
            "one configured transient retry permits at most the original request plus one retry");
        ordinary_failures += 1;
        request.reply.send((500, json!({
            "error":{"code":"server_error","message":"synthetic ordinary retry releases the Driver boundary"}
        }))).unwrap();
    }
} else {
    incoming_summary(&control, &mut receive).await
};
if cancel_active == Some(true) {
    assert!(ordinary_failures <= 2, "the source-configured retry bound is preserved");
    let live_run = request(&control, Command::RunGet(RunTarget {
        run_id: trigger_accept.run_id.clone().expect("saved explicit-Stop Run"),
    })).await;
    assert_eq!(live_run["state"], "live",
        "the Driver owner remains live during the retry backoff while active GC captures");
}
*completed_phase.borrow_mut()=entered_phase.borrow().clone();
let pending_events=proactive_events(&control,&target).await;
assert_eq!(pending_events.len(),1,"one exact report owner reaches this Summary boundary: {pending_events:#?}");
let boundary=start_report_boundary(&control,&pending_events[0],prepared_report.clone().expect("source report captured from its ordinary request before response"),&trigger_accept).await;
if let Some(active) = cancel_active {
    let summary_turn=boundary.report.row_turn_id.clone();
    let committed_summary=control.store.database.call(move |db| {
        Ok(db.query_row("SELECT count(*) FROM model_attempt WHERE turn_id=?1 AND kind='summary'", [summary_turn], |row| row.get::<_,i64>(0))?)
    }).await.unwrap();
    assert_eq!(committed_summary,1,"the Summary provider cannot receive the request before its attempt commits");
    assert_eq!(summary.body["tools"], json!([]), "cancel at the summary HTTP boundary, after spill commit");
    if active {
        let selected = summary.body["input"].to_string();
        assert!(selected.contains("large-0") && selected.contains("large-1"),
            "active R contains exactly the two independently named older source groups: {selected}");
        assert!(!selected.contains("small-2"),
            "the latest complete tool group stays protected at the captured active boundary: {selected}");
    }
    // incoming_summary may have deferred an Ordinary POST admitted before
    // Stop. Retain its typed custody separately from genuinely later arrivals.
    let pending_id=boundary.report.attempt_id.clone();
    let pending_record=automatic_attempt_record(&control,pending_id.clone()).await;
    assert_eq!(pending_record["state"],"unknown","RunStop targets an actually pending Thread-owned Summary");
    assert_eq!(boundary.trigger.run_id,trigger_accept.run_id.as_deref().unwrap());
    assert_eq!(boundary.trigger.turn_id,trigger_accept.turn_id.as_deref().unwrap());
    if active { assert_eq!(boundary.trigger.turn_id,boundary.report.row_turn_id); }
    else { assert_ne!(boundary.trigger.turn_id,boundary.report.row_turn_id); }
            // §3 summary branch: spill inside the selected region, then summarize it.
            let summary_stage = if active { "active_summary" } else { "closed_summary" };
            let adopted_ids = selected_compaction_nodes(&control, &target, "spill").await;
            assert_eq!(adopted_ids.len(), 2, "both older large groups are selected spills before cancelling summary");
            if active {
                let run = boundary.trigger.run_id.clone();
                let shell_executions: i64 = control.store.database.call(move |db| {
                    Ok(db.query_row("SELECT count(*) FROM shell_execution WHERE owner_run_id=?1", [run], |row| row.get(0))?)
                }).await.unwrap();
                assert_eq!(shell_executions, 3,
                    "the two large calls and existing small-2 call execute once; retry backoff adds no tool execution");
            }
            let trigger_run_id = boundary.trigger.run_id.clone();
            *entered_phase.borrow_mut()=format!("sending explicit RunStop for trigger Run {}, report attempt {}",trigger_run_id,boundary.report.attempt_id);
            request(&control, Command::RunStop(RunTarget { run_id: trigger_run_id.clone() })).await;
            // Admission is persisted before HTTP dispatch: src/control/driver/request.rs:116-135 awaits
            // attempt admission (model_attempt INSERT at line 408), and src/control/driver.rs:390-399
            // invokes stream_response only after prepare_attempt returns. The RunStop ACK
            // therefore fences any later attempt row from any post-Stop provider dispatch.
            let attempts_at_stop_ack = attempt_count_for_run(&control, &boundary.trigger.run_id).await;
            // Complete an already-admitted closed Summary after explicit Stop.
            // Its late success must not be adopted; this lets the producer and
            // the exact original FINISH unwind without a test-owned HTTP wait.
            let _late_summary_delivery = summary.reply.send((200, answer("late cancelled summary")));
            *completed_phase.borrow_mut()=entered_phase.borrow().clone();
            // One-shot diagnostic at the existing explicit-Stop cut: distinguish
            // a still-provisional exact report row from a completed CAS without
            // polling or extending the original outer watchdog.
            let diagnostic_attempt=boundary.report.attempt_id.clone();
            let diagnostic_token=boundary.gc_token.clone();
            let row=control.store.database.call(move|db|db.query_row(
                "SELECT state,(SELECT c.outcome IS NULL FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id),(SELECT c.token FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id),(SELECT COALESCE(c.outcome,'unknown') FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id),usage_json FROM model_attempt WHERE id=?1",
                [diagnostic_attempt],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<bool>>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?))
            ).map_err(Into::into)).await.unwrap();
            let stopped_run=request(&control,Command::RunGet(RunTarget{run_id:boundary.trigger.run_id.clone()})).await;
            eprintln!("post-Stop exact report diagnostic: report_turn={} attempt={} gc_token={} row_state={} provisional={:?} saved_gc_token={:?} terminal_outcome={:?} usage={:?} trigger_run={} trigger_run_state={}",boundary.report.row_turn_id,boundary.report.attempt_id,diagnostic_token,row.0,row.1,row.2,row.3,row.4,boundary.trigger.run_id,stopped_run["state"]);
            // Join the exact attempt and its original token; do not poll the
            // latest row or infer cancellation from a wait loop.
            *entered_phase.borrow_mut()=format!("joining exact cancelled FINISH reportTurn={}, reportAttempt={}",boundary.report.row_turn_id,boundary.report.attempt_id);
            let terminal=automatic_attempts_finished_for_turns(&control,std::slice::from_ref(&boundary.report.row_turn_id),1).await;
            *completed_phase.borrow_mut()=entered_phase.borrow().clone();
            assert_eq!(terminal[0].id,boundary.report.attempt_id);
            let record=automatic_attempt_record(&control,boundary.report.attempt_id.clone()).await;
            let finished=boundary.finish(&terminal[0],&record);
            // DB stopping and owner-token cancellation are separate from the
            // durable GC receipt; accept only the two correlated outcomes below.
            // Earlier independently committed spills remain adopted.
            match (
                record["state"].as_str(),
                record["compaction_outcome"]["outcome"]["reason"].as_str(),
            ) {
                (Some("cancelled"), Some("cancelled"))
                | (Some("failed"), Some("run_stopped")) => {}
                other => panic!("unexpected correlated RunStop outcome: {other:?}; {record:#?}"),
            }
            assert_eq!(record["compaction_outcome"]["phase"], summary_stage, "{record:#?}");
            assert_eq!(selected_compaction_nodes(&control, &target, "spill").await, adopted_ids);
            assert!(selected_compaction_nodes(&control, &target, "summary").await.is_empty());
            assert_eq!(record["metrics"]["summary_rejected"], true, "{record:#?}");
            assert_eq!(record["metrics"]["spilled_groups"], adopted_ids.len());
            assert!(record["metrics"]["source_nodes"].as_u64().unwrap() > 0);
            let path_target = target.clone();
            control.store.database.call(move |db| {
                let path = db.prepare("SELECT n.id,n.kind FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal")?.query_map([&path_target], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(path.iter().filter(|(_, kind)| kind == "spill").map(|(id, _)| id.clone()).collect::<Vec<_>>(), adopted_ids);
                assert!(!path.iter().any(|(_, kind)| kind == "summary"));
                let sources = db.prepare("SELECT s.source_node_id FROM thread_path p JOIN replacement r ON r.output_node_id=p.node_id JOIN replacement_source s ON s.replacement_id=r.id WHERE p.thread_id=?1 AND r.format_version='spill-v1' ORDER BY p.ordinal,s.ordinal")?.query_map([&path_target], |row| row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(record["metrics"]["source_nodes"], sources.len());
                assert_eq!(record["metrics"]["first_node_id"], json!(sources.first()));
                assert_eq!(record["metrics"]["last_node_id"], json!(sources.last()));
                // §9: no pending target, guard or activity witness is left behind.
                for kind in ["automatic_target", "automatic_guard", "automatic_active"] {
                    let key = format!("compact.{kind}.{path_target}");
                    let left: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key=?1", [key], |row| row.get(0))?;
                    assert_eq!(left, 0, "{kind}");
                }
                // §6.4: a RunStopped witness is transient, never a deterministic failure record.
                for region in ["closed", "active"] {
                    let key = format!("compact.deterministic_range_failure.{region}.{path_target}");
                    let left: i64 = db.query_row("SELECT count(*) FROM app_meta WHERE key=?1", [key], |row| row.get(0))?;
                    assert_eq!(left, 0, "{region}");
                }
                Ok(())
            }).await.unwrap();
            *entered_phase.borrow_mut()="settling explicitly stopped trigger Run".to_owned();
            settled(&control, &target).await;
            *completed_phase.borrow_mut()=entered_phase.borrow().clone();
            assert_eq!(request(&control, Command::RunGet(RunTarget { run_id: finished.boundary.trigger.run_id.clone() })).await["state"], "stopped");
            let stopped_attempts = attempt_count_for_run(&control, &trigger_run_id).await;
            assert_eq!(stopped_attempts, attempts_at_stop_ack);
            // §2.1/§2.3: the triggering report is consumed and pre-spill usage is
            // stale, so the resumed Turn starts with an ordinary request.
            let resume_text = "resume after cancelled summary";
            let resume_accept = send_accepted(&control, &target, resume_text).await;
            let resumed = crate::test_watchdog::watchdog(
                "runtime_features: receive the explicitly named resumed request",
                async {
                    loop {
                        let incoming = receive
                            .recv()
                            .await
                            .expect("provider inbox remains open for the resumed Turn");
                        if incoming.body.to_string().contains(resume_text) {
                            break incoming;
                        }
                        // These requests were already admitted before the resumed input.
                        drop(incoming.reply);
                    }
                },
            )
            .await
            .expect("the explicitly named resumed request did not reach the proxy");
            assert_eq!(resumed.kind(), ProviderRequestKind::Ordinary);
            assert_ne!(resumed.body["tools"], json!([]), "no summary retry from stale pre-spill usage");
            // This exact first ordinary request supplies fresh #A; retain its
            // attempt/token before responding, then use a later Run boundary.
            let prepared_retry=prepared_report_from_incoming(&control,resume_accept.turn_id.as_deref().expect("resumed report Turn"),1,&resumed,200).await;
            resumed.reply.send((200,answer_with_usage("fresh post-spill measurement",200))).unwrap();
            settled(&control,&target).await;
            let retry_trigger=send_accepted(&control,&target,"verify partial adoption").await;
            let retried = incoming_summary(&control, &mut receive).await;
            assert_eq!(retried.body["tools"], json!([]), "a cancelled summary is retried on the next eligible report");
            let retry_events=proactive_events(&control,&target).await;
            let retry_event=retry_events.iter().find(|event|event["attempt_id"]!=pending_id).expect("new report owns the retry attempt");
            let retry_boundary=start_report_boundary(&control,retry_event,prepared_retry.clone(),&retry_trigger).await;
            assert_ne!(retry_boundary.report.attempt_id,pending_id);
            retried.reply.send((200, answer("## Goal\nResumed work summarized"))).unwrap();
            let verified = incoming_ordinary(&control, &mut receive).await;
            assert_ne!(verified.body["tools"], json!([]));
            verified.reply.send((200, answer_with_usage("verified", 100))).unwrap();
            settled(&control, &target).await;
            let retry_terminal=automatic_attempts_finished_for_turns(&control,std::slice::from_ref(&retry_boundary.report.row_turn_id),1).await;
            let retry_record=automatic_attempt_record(&control,retry_boundary.report.attempt_id.clone()).await;
            let retry_finished=retry_boundary.finish(&retry_terminal[0],&retry_record);
            assert_eq!(retry_finished.outcome,"adopted");
            let events = proactive_events(&control, &target).await;
            assert_eq!(events.len(), 2, "{events:#?}");
            let retry = events.iter().find(|event| event["state"] == "adopted").expect("adopted retry");
            assert_eq!(retry["attempt_id"],retry_finished.boundary.report.attempt_id);
            read_summary(&control, retry).await;
            let path_target = target.clone();
            let spills = control.store.database.call(move |db| Ok(db.query_row("SELECT count(*) FROM node WHERE kind='spill' AND id IN (SELECT r.output_node_id FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id WHERE tr.thread_id=?1)", [path_target], |r| r.get::<_, i64>(0))?)).await.unwrap();
            assert_eq!(spills, 2, "the spill adopted before cancellation is kept, not redone");
            *entered_phase.borrow_mut()="shutting down partial-spill control/owner".to_owned();
control.shutdown().await.unwrap(); owner.shutdown().await.unwrap(); server.abort();
*completed_phase.borrow_mut()=entered_phase.borrow().clone();
            return;
        }
        assert_eq!(summary.body["tools"], json!([]));
        let first_summary_input = summary.body["input"].to_string();
        assert!(!first_summary_input.contains("large-0"), "earlier Turns first; the latest completed Turn stays");
        // An empty summary is a deterministic failure (§6.3), with one request.
        let mut rejected = answer(" ");
        rejected["usage"] = json!({"input_tokens":321,"output_tokens":123,"total_tokens":444});
        summary.reply.send((200, rejected)).unwrap();
        let after = incoming_ordinary(&control, &mut receive).await;
        assert_ne!(after.body["tools"], json!([]), "the measured request must be an ordinary conversation request, not another summary request");
        let context = current_context(&control, &boundary.trigger.turn_id).await;
        assert_current_context_once(&after.body, &context, "after proactive spill");
        let after_size = after.body["input"].as_array().unwrap().iter().filter_map(|item| item.get("output").and_then(Value::as_str)).map(str::len).sum::<usize>();
        assert!(after_size < before_size / 3, "actual replay input must shrink: {before_size} -> {after_size}");
        let omitted = after.body["input"].as_array().unwrap().iter()
            .filter_map(|item| item.get("output").and_then(Value::as_str))
            .filter_map(|output| serde_json::from_str::<Value>(output).ok())
            .filter(|output| output["omitted"] == true).count();
        assert!(omitted >= 2, "both tool output replacements must be replayed: {omitted}");
        let provisional_events = proactive_events(&control, &target).await;
        assert_eq!(provisional_events.len(),1,"the predeclared report has one GC attempt: {provisional_events:#?}");
        let first_attempt_id=provisional_events[0]["attempt_id"].as_str().expect("provider request attempt id").to_owned();
        let first_spills = selected_compaction_nodes(&control, &target, "spill").await;
        assert_eq!(first_spills.len(), 2);
        // A fresh report at the threshold in the same Turn: same closed range
        // and settings, so the deterministic failure is not repeated (§6.3).
        // Keep the Turn running without adding reducible shell output. An
        // echo result includes shell metadata and gives the next attempt a
        // legitimately new spill source even when the closed range is unchanged.
        let retry = prepared_report_from_incoming(&control, &boundary.trigger.turn_id, 1, &after, 205).await;
        let mut same_range = tool_response(3, 205);
        same_range["output"][0]["name"] = json!("fixture_unknown_tool");
        same_range["output"][0]["arguments"] = json!("{}");
        after.reply.send((200, same_range)).unwrap();
        let gated = incoming_ordinary(&control, &mut receive).await;
        assert_ne!(gated.body["tools"], json!([]), "the same sources must not trigger another summary");
        // Keep the active group protected until this exact retry settles.
        // Closing the Turn first permits a separate, legitimate closed spill.
        let mut changes = control.subscribe();
        loop {
            if let Some(item) = compact_attempt(&control, &target, &retry.owner.attempt_id).await
                && item["state"] != "unknown"
            {
                assert_eq!(item["trigger"]["input_tokens"], 205);
                break;
            }
            tokio::select! {
                change = changes.recv() => { change.unwrap(); }
                request = receive.recv() => panic!("unexpected provider request before same-range settlement: {:?}", request.unwrap().kind()),
            }
        }
        gated.reply.send((200, answer("finished"))).unwrap();
        settled(&control, &target).await;
        let observed_events=proactive_events(&control,&target).await;
        let mut turns=observed_events.iter().filter_map(|event|event["turn_id"].as_str()).map(str::to_owned).collect::<Vec<_>>();
        turns.sort();
        turns.dedup();
        for turn in turns {
            let expected_ids=observed_events.iter().filter(|event|event["turn_id"]==turn)
                .map(|event|event["attempt_id"].as_str().expect("saved attempt identity").to_owned()).collect::<Vec<_>>();
            let turn_ids=[turn];
            let terminal=automatic_attempts_finished_for_turns(&control,&turn_ids,expected_ids.len()).await;
            let mut actual_ids=terminal.into_iter().map(|attempt|attempt.id).collect::<Vec<_>>();
            let mut expected_ids=expected_ids;
            actual_ids.sort(); expected_ids.sort();
            assert_eq!(actual_ids,expected_ids,"exact terminal attempts after foreground Turns complete");
        }
        let events=proactive_events(&control,&target).await;
        let item = events.iter().find(|event| event["attempt_id"] == first_attempt_id).unwrap();
        assert_eq!(item["metrics"]["summary_rejected"],true,"{item:#?}");
        assert_eq!(item["metrics"]["summary_usage"]["requests"],1);
        assert!(events.iter().all(|event| event["metrics"]["summary_usage"]["requests"].as_u64().unwrap_or(0) == 0 || event["attempt_id"] == item["attempt_id"]), "the rejected summary must not be reissued: {events:#?}");
        assert_eq!(item["metrics"]["summary_rejected"], true);
        assert_eq!(item["metrics"]["spilled_groups"], 2, "{item:#?}");
        assert_eq!(item["metrics"]["summary_usage"]["input_tokens"]["known_sum"], 321);
        assert_eq!(item["metrics"]["summary_usage"]["output_tokens"]["known_sum"], 123);
        assert_eq!(item["state"], "failed", "{item:#?}");
        let retained_spills = selected_compaction_nodes(&control, &target, "spill").await;
        assert_eq!(first_spills.into_iter().collect::<std::collections::BTreeSet<_>>(), retained_spills.into_iter().collect(), "same-range rejection preserves the original spills and adds none");
        assert!(selected_compaction_nodes(&control, &target, "summary").await.is_empty());
        eprintln!("partial-spill replay bytes {before_size} -> {after_size}; summary input=321 output=123; events={events:#?}");
        *entered_phase.borrow_mut()="shutting down partial-spill control/owner".to_owned();
control.shutdown().await.unwrap(); owner.shutdown().await.unwrap(); server.abort();
*completed_phase.borrow_mut()=entered_phase.borrow().clone();
    }).await;
    if let Err(error) = result {
        panic!(
            "partial spill watchdog: entered_phase={} completed_phase={} error={error}",
            entered_phase.borrow(),
            completed_phase.borrow()
        );
    }
}

/// #303 §2.6: a first known report (#A) equal to the target selects the
/// spill-only branch: closed groups are spilled, no summary is requested.
#[tokio::test]
async fn proactive_first_known_a_at_target_spills_closed_groups_only() {
    proactive_partial_spill_fixture(false, None).await;
}

#[tokio::test]
async fn proactive_partial_spill_with_oversized_current_does_not_retry_summary() {
    proactive_partial_spill_fixture(true, None).await;
}

/// #248 / #303 §6.3: with an oversized current Turn and tool calls whose short
/// error output cannot shrink, an empty closed summary is a deterministic
/// failure. While the Turn keeps growing above the threshold, the closed range
/// (its original endpoints) and the settings stay the same, so the failed
/// summary is not requested again; the closed region keeps precedence (§4.1),
/// so no active summary is attempted either.
#[tokio::test]
async fn proactive_rejected_summaries_are_not_repeated_as_the_current_turn_grows() {
    let result = ax_growing_turn_summary_requests(|| (200, answer(" ")), 0).await;
    compaction_failure_memory::assert_scope_trace(&result);
    assert_eq!(
        result.summaries, 1,
        "one closed summary request: {:#?}",
        result.records
    );
    assert_eq!(
        result.initial_failure_event["metrics"]["summary_rejected"],
        true
    );
    assert_eq!(
        result.initial_failure_event["metrics"]["summary_usage"]["requests"],
        1
    );
    let records = &result.records;
    let failed: Vec<&Value> = records
        .iter()
        .filter(|record| record["metrics"]["summary_usage"]["requests"] == 1)
        .collect();
    assert_eq!(failed.len(), 1, "{records:#?}");
    assert_eq!(
        failed[0]["trigger"]["input_tokens"], 202,
        "the first admitted duty owns the rejection"
    );
    assert_eq!(failed[0]["state"], "failed", "{records:#?}");
    let suppressed = records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "retry_suppressed")
        .collect::<Vec<_>>();
    assert!(
        !suppressed.is_empty(),
        "a later admitted duty is required for nonvacuous suppression coverage"
    );
    assert_eq!(
        suppressed.len(),
        records.len() - 1,
        "all later admitted duties stay suppressed: {records:#?}"
    );
    for record in suppressed {
        let outcome = &record["compaction_outcome"]["outcome"];
        assert_eq!(outcome["origin"], "preparation");
        assert!(outcome.get("http_status").is_none());
    }
    assert!(
        result.selected_summaries.is_empty(),
        "no Summary joins the selected path"
    );
}

/// S2b retires the former per-report HTTP401 oracle: the veto remains a
/// provider failure with no halving or deterministic E write, while subsequent
/// reports in the same real owner/Run/Turn/generation scope are suppressed.
#[tokio::test]
async fn proactive_provider_rejection_of_summaries_is_not_repeated_either() {
    let result = ax_growing_turn_summary_requests(
        || {
            (
                401,
                json!({"error":{"code":"invalid_request_error","message":"synthetic rejection"}}),
            )
        },
        0,
    )
    .await;
    compaction_failure_memory::assert_scope_trace(&result);
    assert_eq!(
        result.failure_memory_slots, 1,
        "the actual provider failure is retained only as a bounded private slot"
    );
    compaction_failure_memory::assert_provider_scope_records(&result, 401, 1);
}

#[tokio::test]
async fn proactive_provider_500_is_suppressed_within_same_run_turn() {
    let result = ax_growing_turn_summary_requests(
        || {
            (
                500,
                json!({"error":{"code":"invalid_request_error","message":"synthetic 500"}}),
            )
        },
        0,
    )
    .await;
    compaction_failure_memory::assert_scope_trace(&result);
    assert_eq!(
        result.failure_memory_slots, 1,
        "provider digest retained in the closed slot"
    );
    compaction_failure_memory::assert_provider_scope_records(&result, 500, 1);
}

/// #303 variant of the growing-Turn fixture: an oversized protected request,
/// unknown-tool rounds whose short error output cannot shrink, the first
/// known report (#A) 100 over target 40, then 17 raw reports at or above the
/// threshold. Automatic outcomes are limited to duties actually admitted by
/// attempt_begin; an explicit compact remains independent of that memory.
struct AxFixtureResult {
    selected_summaries: Vec<String>,
    summaries: usize,
    records: Vec<Value>,
    owner_instance_id: String,
    // Scope is captured for all raw report rows; admitted duties are the subset below.
    report_scopes: Vec<(String, String, i64)>,
    expected_scope: (String, String, i64),
    expected_report_tokens: Vec<u64>,
    report_owners: Vec<ReportOwner>,
    report_attempt_ids: Vec<String>,
    unadmitted_report_attempt_ids: Vec<String>,
    initial_failure_event: Value,
    initial_path_ids: Vec<String>,
    final_path_ids: Vec<String>,
    failure_memory_slots: usize,
}

struct RawReportReadback {
    input_tokens: Option<i64>,
    request_token: Option<String>,
    scope: (String, String, i64),
}

async fn ax_growing_turn_summary_requests(
    summary_reply: fn() -> (u16, Value),
    retries: u8,
) -> AxFixtureResult {
    let phase = Arc::new(std::sync::Mutex::new("fixture setup".to_owned()));
    let phase_for_fixture = phase.clone();
    let result = crate::test_watchdog::watchdog("proactive growing Turn summary requests", async move {
        let set_phase = |label: &str| {
            *phase_for_fixture.lock().unwrap() = label.to_owned();
        };
        set_phase("creating owner and protected Turn");
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let options = "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n";
        let config = root.path().join("config.toml");
        // These provider-memory cases exercise one retained summary R across
        // repeated reports; automatic spills would legitimately consume that
        // R and turn later reports into no-target, a separate contract.
        std::fs::write(&config, format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms=10\nspill=false\n{options}")).unwrap();
        let owner = DataOwner::start(Some(config.clone()), Some(root.path().join("data")), root.path().into()).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        for text in ["hi", "yo"] {
            send(&control, &target, text).await;
            incoming(&control, &mut receive).await.reply.send((200, answer("ok"))).unwrap();
            settled(&control, &target).await;
        }
        let accepted = send_accepted(&control, &target, &"protected current ".repeat(70_000)).await;
        let expected_run = accepted.run_id.clone().expect("accepted triggering Run");
        let expected_turn = accepted.turn_id.clone().expect("accepted triggering Turn");
        let turn_id = expected_turn.clone();
        let expected_generation = control.store.database.call(move |db| {
            db.query_row("SELECT generation FROM turn WHERE id=?1", [turn_id], |row| row.get::<_,i64>(0))
                .map_err(Into::into)
        }).await.unwrap();
        let expected_scope = (expected_run, expected_turn.clone(), expected_generation);
        // Reports 202..=218 are the 17 known-usage boundaries; index 19 is
        // the final ordinary answer and intentionally carries no usage report.
        let expected_report_tokens = (202..=218).collect::<Vec<u64>>();
        let path_thread = target.clone();
        let initial_path_ids = control.store.database.call(move |db| {
            let mut statement = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?;
            let rows = statement.query_map([path_thread], |row| row.get::<_, String>(0))?;
            let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ids)
        }).await.unwrap();
        let tool = |index: u32, tokens: u32| json!({"id":format!("grow-{index}"),"status":"completed","usage":{"input_tokens":tokens,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("grow-{index}"),"name":"nope","arguments":"{}"}]});
        let mut summaries = 0;
        let mut report_owners = Vec::new();
        for index in 0..=19 {
            let ordinary = loop {
                set_phase(&format!("ordinary/summary request interleaving at input {index}"));
                let request = incoming(&control, &mut receive).await;
                if request.kind() == ProviderRequestKind::Ordinary { break request; }
                summaries += 1;
                assert!(summaries < 80, "unbounded automatic summary loop");
                request.reply.send(summary_reply()).unwrap();
            };
            let input_tokens = if index < 2 { 100 } else { 200 + index };
            if (2..=18).contains(&index) {
                set_phase("capturing original report ID/token before response");
                report_owners.push(
                    prepared_report_from_incoming(
                        &control,
                        &expected_turn,
                        index + 1,
                        &ordinary,
                        input_tokens as u64,
                    )
                    .await
                    .owner,
                );
            }
            if index == 19 { ordinary.reply.send((200, answer("finished"))).unwrap(); }
            else { ordinary.reply.send((200, tool(index as u32, input_tokens as u32))).unwrap(); }
        }
        set_phase("settling foreground Run after all raw report inputs");
        settled(&control, &target).await;
        assert_eq!(report_owners.len(), 17, "retain all predeclared known-usage ordinary rows");
        let first_report = report_owners
            .iter()
            .find(|owner| owner.input_tokens == 202)
            .expect("the first report row is retained");
        // The foreground workload has released the Thread boundary; now await
        // the original report-202 admission/FINISH while servicing queued Summary work.
        let mut changes = control.subscribe();
        set_phase("waiting for report-202 attempt_begin after foreground progress");
        let first_record = loop {
            if let Some(record) = compact_attempt(&control, &target, &first_report.attempt_id).await {
                set_phase("waiting for report-202 durable FINISH after attempt_begin");
                if record["state"] != "unknown" { break record; }
            }
            tokio::select! {
                changed = changes.recv() => match changed {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                    Err(error) => panic!("report-202 admission/FINISH notification stream closed: {error}"),
                },
                incoming = receive.recv() => {
                    let incoming = incoming.expect("provider remains available while report-202 admission/FINISH is pending");
                    assert_eq!(incoming.kind(), ProviderRequestKind::Summary,
                        "all ordinary responses were released before awaiting report-202");
                    summaries += 1;
                    assert!(summaries < 80, "unbounded automatic summary loop");
                    incoming.reply.send(summary_reply()).unwrap();
                }
            }
        };
        assert_eq!(first_record["trigger"]["input_tokens"], 202,
            "report 202 must be an accepted actual duty");
        let first_gc_token = automatic_token(&control, &first_report.attempt_id).await;
        assert_ne!(first_gc_token, first_report.request_token, "report and GC tokens are distinct");
        set_phase("reading report-202 CompactList event");
        let initial_failure_event = proactive_events(&control, &target)
            .await
            .into_iter()
            .find(|event| event["attempt_id"] == first_report.attempt_id)
            .expect("first admitted report remains visible through CompactList");
        set_phase("capturing pre-explicit path/history snapshot");
        let path_thread = target.clone();
        let final_path_ids = control.store.database.call(move |db| {
            let mut statement = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?;
            let rows = statement.query_map([path_thread], |row| row.get::<_, String>(0))?;
            let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ids)
        }).await.unwrap();
        let owner_instance_id = owner.instance_id.clone();
        set_phase("capturing pre-explicit failure-memory snapshot");
        let slot_thread = target.clone();
        let failure_memory_slots = control.store.database.call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM app_meta WHERE key IN (?1,?2)",
                rusqlite::params![
                    format!("compact.deterministic_range_failure.closed.{slot_thread}"),
                    format!("compact.deterministic_range_failure.active.{slot_thread}"),
                ],
                |row| row.get::<_, i64>(0).map(|count| count as usize),
            )?)
        }).await.unwrap();
        // The deterministic/provider failure is automatic-only: explicit compact remains available.
        set_phase("issuing explicit Compact");
        request(&control, Command::Compact(ThreadTarget { thread_id: Some(target.clone()) })).await;
        set_phase("servicing explicit Compact Summary");
        let explicit = incoming_summary(&control, &mut receive).await;
        assert_eq!(explicit.body["tools"], json!([]), "explicit compact is not blocked");
        let _ = explicit.reply.send((200, answer("explicit summary")));
        // This existing owner boundary closes admission and joins every retained producer before readback.
        set_phase("draining retained producers through Control.shutdown");
        control.shutdown().await.unwrap();
        // Summary ledger `begin` is committed before POST; this equals the fixture's serviced count.
        // Summary witness changes to model_attempt with #328.
        // Include the explicit Compact Summary admitted after the proactive workload.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &target).await, summaries as i64 + 1);

        set_phase("final 17-row admission/unadmitted partition readback after producer drain");
        owner.shutdown().await.unwrap();
        let owner = DataOwner::start(Some(config), Some(root.path().join("data")), root.path().into()).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let mut records = Vec::new();
        let mut raw_report_scopes = Vec::new();
        let mut report_attempt_ids = Vec::new();
        let mut unadmitted_report_attempt_ids = Vec::new();
        for owner in &report_owners {
            let attempt_id = owner.attempt_id.clone();
            let facts: RawReportReadback = control.store.database.call(move |db| {
                Ok(db.query_row(
                    "SELECT json_extract(a.usage_json,'$.input_tokens'),json_extract(a.input_manifest_json,'$.compaction_report_token'),r.id,t.id,t.generation FROM model_attempt a JOIN turn t ON t.id=a.turn_id JOIN run r ON r.id=t.run_id WHERE a.id=?1",
                    [attempt_id],
                    |row| Ok(RawReportReadback {
                        input_tokens: row.get(0)?,
                        request_token: row.get(1)?,
                        scope: (row.get(2)?, row.get(3)?, row.get(4)?),
                    }),
                )?)
            }).await.unwrap();
            assert_eq!(facts.input_tokens, Some(owner.input_tokens as i64), "saved known usage stays on its original attempt");
            assert_eq!(facts.request_token.as_deref(), Some(owner.request_token.as_str()), "original report token stays with its attempt");
            raw_report_scopes.push(facts.scope);
            if let Some(record) = compact_attempt(&control, &target, &owner.attempt_id).await {
                assert_eq!(record["trigger"]["input_tokens"], owner.input_tokens);
                report_attempt_ids.push(owner.attempt_id.clone());
                records.push(record);
            } else {
                unadmitted_report_attempt_ids.push(owner.attempt_id.clone());
            }
        }
        assert!(
            report_attempt_ids.len() >= 2,
            "nonvacuous same-scope suppression coverage: no later admitted attempt followed report 202; raw reports are not duties"
        );
        let mut accepted_ids = report_attempt_ids.clone();
        let mut unadmitted_ids = unadmitted_report_attempt_ids.clone();
        accepted_ids.sort_unstable();
        unadmitted_ids.sort_unstable();
        assert!(accepted_ids.iter().all(|id| !unadmitted_ids.contains(id)), "accepted/unadmitted partition overlaps");
        let mut partition_ids = accepted_ids.clone();
        partition_ids.extend(unadmitted_ids);
        partition_ids.sort_unstable();
        let mut raw_ids = report_owners.iter().map(|owner| owner.attempt_id.clone()).collect::<Vec<_>>();
        raw_ids.sort_unstable();
        assert_eq!(partition_ids, raw_ids, "drained admission partition covers exactly the 17 original raw rows");
        set_phase("checking exact terminal FINISH set for durable attempt_begin rows");
        let trigger_turns = [expected_turn.clone()];
        let terminal = automatic_attempts_finished_for_turns(
            &control,
            &trigger_turns,
            report_attempt_ids.len(),
        ).await;
        let mut terminal_ids = terminal.iter().map(|attempt| attempt.id.clone()).collect::<Vec<_>>();
        terminal_ids.sort_unstable();
        assert_eq!(terminal_ids, accepted_ids, "every durable attempt_begin row has exact terminal FINISH");
        for owner in &report_owners {
            let Some(index) = report_attempt_ids.iter().position(|id| id == &owner.attempt_id) else {
                continue;
            };
            let finished = terminal.iter().find(|attempt| attempt.id == owner.attempt_id)
                .expect("FINISH for admitted source attempt");
            assert_eq!(records[index]["attempt_id"], finished.id,
                "FINISH GC token remains on original source attempt");
            assert_ne!(records[index]["state"], "unknown", "all accepted duties are terminal after drain");
        }
        let durable_attempt_ids = control.store.database.call(move |db| {
            Ok(db.prepare("SELECT id FROM model_attempt WHERE turn_id=?1 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id) ORDER BY rowid")?
                .query_map([&expected_turn], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        }).await.unwrap();
        let mut durable_attempt_ids = durable_attempt_ids;
        durable_attempt_ids.sort_unstable();
        assert_eq!(durable_attempt_ids, accepted_ids, "no automatic attempt exists outside the drained raw-owner partition");
        eprintln!("raw_reports=17 admitted={} unadmitted={} summary_requests={summaries} records={records:#?}", report_attempt_ids.len(), unadmitted_report_attempt_ids.len());
        let selected_summaries = selected_compaction_nodes(&control, &target, "summary").await;
        set_phase("shutting down reopened Owner after durable automatic record readback");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap(); server.abort();
        AxFixtureResult {
            selected_summaries,
            summaries,
            records,
            owner_instance_id,
            report_scopes: raw_report_scopes,
            expected_scope,
            expected_report_tokens,
            report_owners,
            report_attempt_ids,
            unadmitted_report_attempt_ids,
            initial_path_ids,
            final_path_ids,
            failure_memory_slots,
            initial_failure_event,
        }
    }).await;
    match result {
        Ok(result) => result,
        Err(error) => panic!(
            "proactive growing Turn summary fixture watchdog at phase {:?}: {error}",
            *phase.lock().unwrap()
        ),
    }
}

/// Repeated Turns may produce summary requests even during warmup; trace those
/// separately from ordinary model requests so provider accounting is truthful.
#[derive(Default)]
pub(super) struct ProactiveTrace {
    summary_requests: usize,
    summary_bodies: Vec<Value>,
    report_owners: Vec<ReportOwner>,
    admitted_summaries: Vec<ReportBoundary>,
}

async fn proactive_third_turn(
    control: &Arc<Control>,
    receive: &mut ProxyInbox,
    thread: &str,
    second: u64,
    summary_reply: (u16, Value),
) -> usize {
    proactive_third_turn_trace(control, receive, thread, second, summary_reply)
        .await
        .summary_requests
}

async fn proactive_third_turn_trace(
    control: &Arc<Control>,
    receive: &mut ProxyInbox,
    thread: &str,
    second: u64,
    summary_reply: (u16, Value),
) -> ProactiveTrace {
    let mut trace = ProactiveTrace::default();
    for (n, tokens) in [(0, 50), (1, second)] {
        let text = format!("task {n}");
        let accepted = send_accepted(control, thread, &text).await;
        let run_id = accepted.run_id.clone().expect("accepted warmup Run");
        let turn_id = accepted.turn_id.clone().expect("accepted warmup Turn");
        loop {
            let next = incoming(control, receive).await;
            if next.kind() == ProviderRequestKind::Summary {
                let admitted = admitted_summary_boundary(
                    control,
                    &trace.report_owners,
                    TriggerOwner {
                        run_id: run_id.clone(),
                        turn_id: turn_id.clone(),
                    },
                )
                .await;
                if !trace
                    .admitted_summaries
                    .iter()
                    .any(|known| known.report.attempt_id == admitted.report.attempt_id)
                {
                    trace.admitted_summaries.push(admitted);
                }
                trace.summary_requests += 1;
                assert!(
                    trace.summary_requests <= 8,
                    "bounded warmup summary requests"
                );
                trace.summary_bodies.push(next.body.clone());
                next.reply.send(summary_reply.clone()).unwrap();
                continue;
            }
            if !next.body.to_string().contains(&text) {
                drop(next.reply);
                continue;
            }
            assert_current_context_once(
                &next.body,
                &current_context(control, &turn_id).await,
                "warmup ordinary request",
            );
            let report = prepared_report_from_incoming(control, &turn_id, 1, &next, tokens).await;
            trace.report_owners.push(report.owner);
            next.reply
                .send((200, answer_with_usage("saved answer", tokens)))
                .unwrap();
            break;
        }
        settled(control, thread).await;
    }
    let current_text = "current task";
    let current = send_accepted(control, thread, current_text).await;
    let current_run = current.run_id.clone().expect("accepted current Run");
    let current_turn = current.turn_id.clone().expect("accepted current Turn");
    loop {
        let next = incoming(control, receive).await;
        if next.kind() == ProviderRequestKind::Summary {
            let admitted = admitted_summary_boundary(
                control,
                &trace.report_owners,
                TriggerOwner {
                    run_id: current_run.clone(),
                    turn_id: current_turn.clone(),
                },
            )
            .await;
            if !trace
                .admitted_summaries
                .iter()
                .any(|known| known.report.attempt_id == admitted.report.attempt_id)
            {
                trace.admitted_summaries.push(admitted);
            }
            trace.summary_requests += 1;
            assert!(trace.summary_requests <= 8, "bounded summary requests");
            trace.summary_bodies.push(next.body.clone());
            next.reply.send(summary_reply.clone()).unwrap();
            continue;
        }
        if !next.body.to_string().contains(current_text) {
            drop(next.reply);
            continue;
        }
        assert_current_context_once(
            &next.body,
            &current_context(control, &current_turn).await,
            "after proactive summary",
        );
        next.reply
            .send((200, answer_with_usage("last answer", 10)))
            .unwrap();
        break;
    }
    settled(control, thread).await;
    trace
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_threshold_compacts_once_at_the_next_boundary() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let summary_reply = (200, {
        let mut reply = answer("## 目的\nSummarized earlier turns");
        reply["usage"] = json!({"input_tokens":120,"input_tokens_details":{"cached_tokens":30,"cache_creation_tokens":4},"output_tokens":6});
        reply
    });
    let mut summaries =
        proactive_third_turn(&control, &mut receive, &thread, 150, summary_reply.clone()).await;
    let initial_events = proactive_events(&control, &thread).await;
    assert_eq!(
        initial_events.len(),
        1,
        "one independently triggered threshold report: {initial_events:#?}"
    );
    let original_attempt_id = initial_events[0]["attempt_id"]
        .as_str()
        .expect("threshold report attempt id")
        .to_owned();
    let original_turn_id = initial_events[0]["turn_id"]
        .as_str()
        .expect("threshold report Turn id")
        .to_owned();
    let finish =
        automatic_attempts_finished_for_turns(&control, std::slice::from_ref(&original_turn_id), 1);
    tokio::pin!(finish);
    let terminal = loop {
        tokio::select! {
            attempts = &mut finish => break attempts,
            incoming = receive.recv() => {
                let incoming = incoming.expect("threshold provider remains available through original FINISH");
                assert_eq!(incoming.kind(), ProviderRequestKind::Summary,
                    "only a retained automatic Summary request may remain after foreground settlement");
                summaries += 1;
                incoming.reply.send(summary_reply.clone()).unwrap();
            }
        }
    };
    assert_eq!(terminal.len(), 1);
    assert_eq!(
        terminal[0].id, original_attempt_id,
        "the completed receipt is the original threshold report owner"
    );
    let finished_record = automatic_attempt_record(&control, original_attempt_id.clone()).await;
    assert_eq!(finished_record["trigger"]["input_tokens"], 150);
    assert_eq!(
        finished_record["attempt_id"], terminal[0].id,
        "original GC token survives exact FINISH"
    );
    assert_eq!(finished_record["state"], "adopted");
    assert_eq!(
        summaries, 1,
        "one request completed in the original threshold attempt"
    );
    // Summary ledger `begin` is committed before POST; this equals the serviced count.
    // Summary witness changes to model_attempt with #328.
    assert_eq!(
        summary_request_count_for_thread(&control, &thread).await,
        summaries as i64
    );
    let events = proactive_events(&control, &thread).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["state"], "adopted");
    assert_eq!(events[0]["kind"], "automatic_reduction");
    assert_eq!(events[0]["metrics"]["reason"], "proactive_threshold");
    assert_eq!(events[0]["trigger"]["auto_at_tokens"], 100);
    assert_eq!(events[0]["trigger"]["input_tokens"], 150);
    assert!(events[0]["metrics"]["summary_node_id"].is_string());
    assert_eq!(events[0]["metrics"]["summary_limit"], 20000);
    assert!(events[0]["metrics"].get("segment_chars").is_none());
    assert!(events[0]["metrics"].get("reduce_rounds").is_none());
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
            thread_id: Some(thread.clone().into_boxed_str()),
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
        // #303 §2.2: below the threshold (or disabled) nothing happens: no
        // attempt record on any report, so no #A boundary is consumed either.
        let records: i64 = control
            .store
            .database
            .call(|db| Ok(db.query_row("SELECT count(*) FROM model_attempt WHERE EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id)", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(records, 0, "{extra}");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
        assert!(
            !log.contains("\"event\":\"automatic_compaction_started\""),
            "{extra}"
        );
        assert!(!log.contains("saved answer"));
        assert!(!log.contains("current task"));
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_list_records_the_summary_parameters_in_effect() {
    // The typed Thread override replaces the connection output cap. The
    // automatic trigger remains independent of the summary request settings.
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(
        root.path(),
        &url,
        "summary_max_chars=50000\n[compaction]\nauto_at_tokens=100\n",
    )
    .await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs::summary_settings(
            thread.clone(),
            None,
            Some("6000".into()),
        ))),
    )
    .await;
    let summary_reply = (200, answer("## 目的\nSummarized earlier turns"));
    let mut trace =
        proactive_third_turn_trace(&control, &mut receive, &thread, 150, summary_reply.clone())
            .await;
    let attempts = finish_proactive_trace(&control, &mut receive, &mut trace, &summary_reply).await;
    assert_eq!(trace.summary_requests, 1);
    assert_eq!(attempts.len(), 1);
    let expected_attempt = &attempts[0].boundary.report.attempt_id;
    let events = proactive_events(&control, &thread).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["attempt_id"], expected_attempt.as_str());
    assert_eq!(events[0]["state"], "adopted", "{:?}", events[0]);
    assert_eq!(events[0]["trigger"]["auto_at_tokens"], 100);
    let metrics = &events[0]["metrics"];
    assert!(
        metrics["source_characters"].as_u64().is_some(),
        "existing CompactList row has no source metric: {}",
        events[0]
    );
    assert!(
        metrics["summary_characters"].as_u64().is_some(),
        "{metrics}"
    );
    assert_eq!(metrics["summary_limit"], 6000);
    assert_eq!(metrics["model_requests"], 1);
    assert_eq!(metrics["summary_usage"]["requests"], 1);
    assert_eq!(metrics["spilled_groups"], 0);
    assert!(metrics["summary_node_id"].is_string(), "{metrics}");
    assert!(events[0]["metrics"].get("segment_chars").is_none());
    assert!(events[0]["metrics"].get("reduce_rounds").is_none());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

async fn set_thread_auto_compact(control: &Arc<Control>, thread: &str, value: &str) {
    request(
        control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.into()),
            auto_compact_at: Some(value.into()),
            ..Default::default()
        })),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_auto_compact_override_replaces_the_connection_threshold_for_that_thread() {
    // Connection value 100 would compact; the Thread's own 0 turns it off, and
    // clearing returns to 100.
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    set_thread_auto_compact(&control, &thread, "0").await;
    for (n, tokens) in [(0, 50), (1, 150), (2, 150)] {
        send(&control, &thread, &format!("task {n}")).await;
        let next = incoming(&control, &mut receive).await;
        assert_ne!(
            next.body["tools"],
            json!([]),
            "disabled Thread never summarizes"
        );
        next.reply
            .send((200, answer_with_usage("saved answer", tokens)))
            .unwrap();
        settled(&control, &thread).await;
    }
    assert!(proactive_events(&control, &thread).await.is_empty());
    set_thread_auto_compact(&control, &thread, "").await;
    send(&control, &thread, "after clear").await;
    let mut summaries = 0;
    loop {
        let next = incoming(&control, &mut receive).await;
        if next.kind() == ProviderRequestKind::Summary {
            summaries += 1;
            assert!(summaries <= 4, "bounded summary requests");
            next.reply
                .send((200, answer("## 目的\nSummarized")))
                .unwrap();
            continue;
        }
        next.reply
            .send((200, answer_with_usage("done", 10)))
            .unwrap();
        break;
    }
    settled(&control, &thread).await;
    assert_eq!(summaries, 1, "clearing restores the connection threshold");
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_auto_compact_change_waits_for_the_next_boundary_and_never_alters_a_request_in_flight()
 {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "").await; // default 200000
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "task 0").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer_with_usage("saved answer", 50)))
        .unwrap();
    settled(&control, &thread).await;
    send(&control, &thread, "task 1").await;
    let in_flight = incoming(&control, &mut receive).await;
    // The request is already on the wire: lowering the threshold now changes nothing for it.
    set_thread_auto_compact(&control, &thread, "100").await;
    in_flight
        .reply
        .send((200, answer_with_usage("saved answer", 150)))
        .unwrap();
    settled(&control, &thread).await;
    assert!(
        proactive_events(&control, &thread).await.is_empty(),
        "in-flight request is unchanged"
    );
    send(&control, &thread, "current task").await;
    let mut summaries = 0;
    loop {
        let next = incoming(&control, &mut receive).await;
        if next.kind() == ProviderRequestKind::Summary {
            summaries += 1;
            assert!(summaries <= 4, "bounded summary requests");
            next.reply
                .send((200, answer("## 目的\nSummarized")))
                .unwrap();
            continue;
        }
        next.reply
            .send((200, answer_with_usage("last answer", 10)))
            .unwrap();
        break;
    }
    settled(&control, &thread).await;
    assert_eq!(summaries, 1, "the next boundary check uses the new value");
    let events = proactive_events(&control, &thread).await;
    assert_eq!(events[0]["trigger"]["auto_at_tokens"], 100);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

/// A deterministic closed-summary failure is followed by a stale high-token
/// tool report. Its report token predates the failed attempt's A-boundary reset,
/// so usage cannot become #A; the later preflight is spill-only and no-target.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_summary_failure_stale_report_is_no_target() {
    let (summaries, attempts, slots, stale_a_tokens) = bx_token_growth((200, answer(" "))).await;
    assert_eq!(
        summaries, 1,
        "the stale report cannot dispatch a second summary"
    );
    assert_eq!(
        attempts.len(),
        2,
        "one initial closed failure and one later preflight"
    );
    assert_eq!(attempts[0]["state"], "failed");
    assert_eq!(attempts[1]["state"], "no_target");
    assert_eq!(attempts[1]["trigger"]["input_tokens"], 9999);
    assert_ne!(
        stale_a_tokens.0, stale_a_tokens.1,
        "the stale 9999 report token differs from the post-FINISH A boundary"
    );
    assert!(
        slots.0.is_some(),
        "the initial closed deterministic failure remains recorded"
    );
    assert!(slots.1.is_none(), "no active failure slot was written");
}

/// Two completed Turns establish A, one active summary fails, then a later
/// 9999-token report was started before that failure's FINISH reset. Its saved
/// report token proves that it cannot become post-reset #A.
async fn bx_token_growth(
    summary_reply: (u16, Value),
) -> (
    u32,
    Vec<Value>,
    (Option<String>, Option<String>),
    (String, String),
) {
    crate::test_watchdog::watchdog("bx token growth", async move {
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
        let current_accept = send_accepted(&control, &thread, "current task").await;
        let current_turn = current_accept.turn_id.expect("accepted growing Turn");
        let mut summaries = 0;
        let mut ordinary = vec![
            answer_with_usage("done", 150),
            bx_tool("proactive-tool", 9999, "echo synthetic"),
        ];
        while let Some(reply) = ordinary.last().cloned() {
            let next = incoming(&control, &mut receive).await;
            if next.kind() == ProviderRequestKind::Summary {
                summaries += 1;
                assert!(summaries <= 4, "bounded summary requests");
                let input = next.body["input"].to_string();
                assert!(input.contains("task 0"), "closed range: the older Turn");
                assert!(!input.contains("task 1"), "the latest completed Turn stays");
                next.reply.send(summary_reply.clone()).unwrap();
                continue;
            }
            ordinary.pop();
            next.reply.send((200, reply)).unwrap();
        }
        settled(&control, &thread).await;
        // Summary ledger `begin` is committed before POST; no unserviced Summary remains.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &thread).await, summaries as i64);
        let attempts = bx_attempts(&control, &thread).await;
        assert_eq!(
            attempts.len(),
            2,
            "one attempt per eligible report: {attempts:#?}"
        );
        let triggers: Vec<_> = attempts.iter().map(|a| a["trigger"]["input_tokens"].clone()).collect();
        assert_eq!(triggers, [json!(150), json!(9999)]);
        let target=thread.clone();
        let (report_token,a_boundary):(String,String)=control.store.database.call(move|db|{
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt a WHERE thread_id=?1 AND kind='conversation' AND json_extract(a.usage_json,'$.input_tokens')=9999 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=a.id)",[&target],|r|r.get::<_,i64>(0))?,1,
                "the exact 9999-token report content identifies one model_attempt row");
            db.query_row(
                "SELECT json_extract(a.input_manifest_json,'$.compaction_report_token'),(SELECT json_extract(c.trigger_json,'$.a_boundary') FROM automatic_compaction c WHERE c.attempt_id=a.id) FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 AND json_extract(a.usage_json,'$.input_tokens')=9999 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=a.id)",
                [target],
                |row|Ok((row.get(0)?,row.get(1)?)),
            ).map_err(Into::into)
        }).await.unwrap();
        assert_ne!(report_token,a_boundary,
            "source report began before the previous FINISH advanced A boundary; completion cannot supply stale #A");
        assert_eq!(attempts[0]["state"],"failed");
        assert_eq!(attempts[1]["state"],"no_target");
        let events = proactive_events(&control, &thread).await;
        assert_eq!(events.len(), 2, "{events:?}");
        let first_event = events
            .iter()
            .find(|event| event["trigger"]["input_tokens"] == 150)
            .unwrap();
        let second_event = events
            .iter()
            .find(|event| event["trigger"]["input_tokens"] == 9999)
            .unwrap();
        assert_eq!(first_event["state"],"failed","{first_event:#?}");
        assert_eq!(second_event["state"],"no_target","{second_event:#?}");
        let first = first_event;
        let usage = &first["metrics"]["summary_usage"];
        assert_eq!(usage["requests"], 1, "{first:#?}");
        assert_eq!(usage["input_tokens"]["unknown_count"], 1);
        assert_eq!(usage["output_tokens"]["unknown_count"], 1);
        let (summary_nodes, turn_state) = control
            .store
            .database
            .call(move |db| {
                Ok((
                    db.query_row("SELECT count(*) FROM node WHERE kind='summary'", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    db.query_row(
                        "SELECT state FROM turn WHERE id=?1",
                        [&current_turn],
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
        let slots = bx_failure_slots(&control, &thread).await;
        let target=thread.clone();
        let stale_a_tokens:(String,String)=control.store.database.call(move|db|{
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt a WHERE thread_id=?1 AND kind='conversation' AND json_extract(a.usage_json,'$.input_tokens')=9999 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=a.id)",[&target],|r|r.get::<_,i64>(0))?,1,
                "the exact 9999-token report content identifies one model_attempt row");
            db.query_row(
                "SELECT json_extract(a.input_manifest_json,'$.compaction_report_token'),(SELECT json_extract(c.trigger_json,'$.a_boundary') FROM automatic_compaction c WHERE c.attempt_id=a.id) FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 AND json_extract(a.usage_json,'$.input_tokens')=9999 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=a.id)",
                [target],
                |row|Ok((row.get(0)?,row.get(1)?)),
            ).map_err(Into::into)
        }).await.unwrap();
        assert_ne!(stale_a_tokens.0,stale_a_tokens.1,
            "the input manifest token predates the terminal attempt's A-boundary reset");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
        (summaries, attempts, slots, stale_a_tokens)
    })
    .await
    .unwrap()
}

// #303 test helpers (bx_ prefix): automatic attempt records in triggering order,
// the two deterministic-failure slots (§6.3) and a tool response with usage.
async fn bx_attempts(control: &Arc<Control>, thread: &str) -> Vec<Value> {
    let mut items = proactive_events(control, thread).await;
    let thread = thread.to_owned();
    let ids: Vec<String> = control
        .store
        .database
        .call(move |db| {
            Ok(db
                .prepare("SELECT id FROM model_attempt WHERE thread_id=?1 ORDER BY rowid")?
                .query_map([thread], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    items.sort_by_key(|item| {
        ids.iter()
            .position(|id| item["attempt_id"] == *id)
            .expect("compact.list item belongs to a fixture attempt")
    });
    items
}
async fn bx_failure_slots(
    control: &Arc<Control>,
    thread: &str,
) -> (Option<String>, Option<String>) {
    let thread = thread.to_owned();
    control
        .store
        .database
        .call(move |db| {
            let slot = |kind: &str| {
                db.query_row(
                    "SELECT value FROM app_meta WHERE key=?1",
                    [format!(
                        "compact.deterministic_range_failure.{kind}.{thread}"
                    )],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            };
            Ok((slot("closed")?, slot("active")?))
        })
        .await
        .unwrap()
}
fn bx_tool(id: &str, tokens: u64, command: &str) -> Value {
    json!({"id":format!("response-{id}"),"status":"completed","usage":{"input_tokens":tokens,"output_tokens":1,"total_tokens":tokens+1},"output":[{"type":"function_call","status":"completed","call_id":id,"name":"shell","arguments":json!({"command":command}).to_string()}]})
}
/// Function-call outputs replayed in `body` that are spill replacements.
fn bx_omitted_outputs(body: &Value) -> usize {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.get("output").and_then(Value::as_str))
        .filter_map(|output| serde_json::from_str::<Value>(output).ok())
        .filter(|output| output["omitted"] == true)
        .count()
}

/// #303 §6.3: a deterministic closed failure blocks only its own original
/// endpoints. When a new older Turn becomes eligible, the closed range ends
/// later, so the next eligible report sends a summary again and adopts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_retries_when_a_new_older_turn_becomes_eligible() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let mut source_turns = Vec::new();
    for (n, tokens) in [(0, 50), (1, 150)] {
        let accepted = send_accepted(&control, &thread, &format!("task {n}")).await;
        source_turns.push(accepted.turn_id.expect("completed source Turn"));
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("saved answer", tokens)))
            .unwrap();
        settled(&control, &thread).await;
    }
    let prepared_old = prepare_report_from_thread(&control, &thread, &source_turns[1], 150).await;
    let trigger_first = send_accepted(&control, &thread, "third task").await;
    let first = incoming_summary(&control, &mut receive).await;
    let events_after_first = proactive_events(&control, &thread).await;
    assert_eq!(
        events_after_first.len(),
        1,
        "the predeclared old report creates one initial attempt"
    );
    let first_boundary = start_report_boundary(
        &control,
        &events_after_first[0],
        prepared_old,
        &trigger_first,
    )
    .await;
    assert_eq!(first.body["tools"], json!([]));
    let input = first.body["input"].to_string();
    assert!(input.contains("task 0") && !input.contains("task 1"));
    first.reply.send((200, answer(" "))).unwrap();
    let ordinary = incoming_ordinary(&control, &mut receive).await;
    assert_ne!(ordinary.body["tools"], json!([]));
    let prepared_after_summary = prepared_report_from_incoming(
        &control,
        trigger_first
            .turn_id
            .as_deref()
            .expect("third-task report Turn"),
        1,
        &ordinary,
        50,
    )
    .await;
    ordinary
        .reply
        .send((200, answer_with_usage("third answer bridge", 50)))
        .unwrap();
    settled(&control, &thread).await;
    let first_terminal = automatic_attempts_finished_for_turns(
        &control,
        std::slice::from_ref(&first_boundary.report.row_turn_id),
        1,
    )
    .await;
    let first_record =
        automatic_attempt_record(&control, first_boundary.report.attempt_id.clone()).await;
    let first_report_attempt = first_boundary.report.attempt_id.clone();
    let first_finished = first_boundary.finish(&first_terminal[0], &first_record);
    assert_eq!(first_finished.outcome, "failed");
    let (closed, active) = bx_failure_slots(&control, &thread).await;
    assert!(
        closed.is_some() && active.is_none(),
        "empty summary is deterministic"
    );
    // A below-threshold bridge records a first-known #A above the 40-token
    // target but below the 100-token trigger. Then a separate fresh measurement
    // Run supplies the report whose exact request starts the retry on the next
    // Run; its later 150 usage cannot replace the already-known #A=50.
    let bridge_accept = send_accepted(&control, &thread, "fourth task bridge").await;
    let bridge_request = incoming_ordinary(&control, &mut receive).await;
    let bridge_report = prepared_report_from_incoming(
        &control,
        bridge_accept.turn_id.as_deref().expect("bridge Turn"),
        1,
        &bridge_request,
        50,
    )
    .await;
    bridge_request
        .reply
        .send((200, answer_with_usage("bridge answer", 50)))
        .unwrap();
    settled(&control, &thread).await;
    let bridge_a = known_first_a_from_reports(
        &control,
        &thread,
        &[&prepared_after_summary, &bridge_report],
        50,
    )
    .await;
    assert_ne!(
        bridge_a.report_attempt_id, first_report_attempt,
        "failed report FINISH reset prior #A before this bridge"
    );

    let measurement_accept = send_accepted(&control, &thread, "fifth task fresh measurement").await;
    let fresh_request = incoming_ordinary(&control, &mut receive).await;
    let prepared_next = prepared_report_from_incoming(
        &control,
        measurement_accept
            .turn_id
            .as_deref()
            .expect("fresh measurement Turn"),
        1,
        &fresh_request,
        150,
    )
    .await;
    let prepared_next =
        prepare_report_after_finish(&first_finished, "failed", &bridge_a, 50, prepared_next);
    fresh_request
        .reply
        .send((200, answer_with_usage("fifth answer", 150)))
        .unwrap();
    settled(&control, &thread).await;
    let trigger_second =
        send_accepted(&control, &thread, "sixth task after fresh measurement").await;
    let second = incoming_summary(&control, &mut receive).await;
    let events_after_second = proactive_events(&control, &thread).await;
    assert_eq!(
        events_after_second.len(),
        2,
        "only the original failure and its explicit post-FINISH retry exist: {events_after_second:#?}"
    );
    let second_event = events_after_second
        .iter()
        .find(|event| event["attempt_id"] == prepared_next.owner.attempt_id)
        .expect("retry uses the explicit fifth-task post-FINISH report");
    assert_eq!(first_finished.outcome, "failed");
    assert_eq!(bridge_a.input_tokens, 50);
    assert_ne!(
        prepared_next.owner.request_token, first_finished.boundary.report.request_token,
        "fresh measurement report is separately captured after exact failed FINISH"
    );
    let second_boundary =
        start_report_boundary(&control, second_event, prepared_next, &trigger_second).await;
    assert_eq!(
        second.body["tools"],
        json!([]),
        "the newly unprotected older Turn changes the closed range"
    );
    let input = second.body["input"].to_string();
    assert!(input.contains("task 0") && input.contains("task 1"));
    assert!(
        input.contains("third task"),
        "the bridge Run is an older eligible Turn after the fifth-task measurement"
    );
    assert!(
        !input.contains("fifth task fresh measurement"),
        "the latest completed measurement Turn stays protected"
    );
    second.reply.send((200, answer("short summary"))).unwrap();
    let ordinary = incoming(&control, &mut receive).await;
    assert_ne!(ordinary.body["tools"], json!([]));
    ordinary
        .reply
        .send((200, answer_with_usage("finished", 10)))
        .unwrap();
    settled(&control, &thread).await;
    let second_terminal = automatic_attempts_finished_for_turns(
        &control,
        std::slice::from_ref(&second_boundary.report.row_turn_id),
        1,
    )
    .await;
    let second_record =
        automatic_attempt_record(&control, second_boundary.report.attempt_id.clone()).await;
    let second_finished = second_boundary.finish(&second_terminal[0], &second_record);
    assert_ne!(
        first_finished.boundary.report.row_turn_id, second_finished.boundary.report.row_turn_id,
        "closed retry uses a fresh source report, not the prior attempt's report Turn"
    );
    let attempts = bx_attempts(&control, &thread).await;
    let outcomes: Vec<_> = attempts.iter().map(|a| a["state"].clone()).collect();
    assert_eq!(
        outcomes,
        [json!("failed"), json!("adopted")],
        "{attempts:#?}"
    );
    let events = proactive_events(&control, &thread).await;
    assert_eq!(events.len(), 2, "{events:?}");
    for state in ["failed", "adopted"] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event["state"] == state)
                .count(),
            1,
            "{events:?}"
        );
    }
    let adopted = events
        .iter()
        .find(|event| event["state"] == "adopted")
        .unwrap();
    read_summary(&control, adopted).await;
    // Both Summary ledger `begin` calls commit before POST.
    // Summary witness changes to model_attempt with #328.
    assert_eq!(summary_request_count_for_thread(&control, &thread).await, 2);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

/// #303 §3/§4.1: in the summary branch (#A > target) the active region is
/// spilled and then summarized in the same attempt. An older complete tool
/// group of the open loop is spilled once a newer group exists; the active
/// summary then covers it through the latest round, which is kept as text.
async fn selected_completed_pair(control: &Arc<Control>, thread: &str) -> Vec<String> {
    let window = selected_history(control, thread).await;
    let mut nodes = window
        .items
        .into_iter()
        .rev()
        .filter(|node| matches!(node.kind.as_str(), "assistant_response" | "tool_result"));
    let result = nodes
        .next()
        .expect("result for the preceding provider call");
    let call = nodes
        .next()
        .expect("assistant response for the preceding provider call");
    assert_eq!(result.kind, "tool_result");
    assert_eq!(call.kind, "assistant_response");
    vec![call.node_id, result.node_id]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_resumes_when_an_older_complete_tool_group_becomes_reducible() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    // The dedicated no-target control covers an earlier report with an empty
    // current range. This caller owns only the active whole-group selection.
    let accepted = send_accepted(&control, &thread, "current task with two tool steps").await;
    let trigger_turn = accepted
        .turn_id
        .as_deref()
        .expect("accepted current tool-loop Turn")
        .to_owned();
    let command = if cfg!(windows) {
        "powershell -NoProfile -Command \"[Console]::Write(('x' * 50000))\""
    } else {
        "head -c 50000 /dev/zero | tr '\\0' x"
    };
    // First response is below threshold; the second exact in-flight request
    // supplies the sole 200-token report used by the active summary stage.
    let first = incoming_ordinary(&control, &mut receive).await;
    assert_ne!(first.body["tools"], json!([]));
    let prepared_first =
        prepared_report_from_incoming(&control, &trigger_turn, 1, &first, 50).await;
    first
        .reply
        .send((200, bx_tool("call-0", 50, command)))
        .unwrap();
    let second = incoming(&control, &mut receive).await;
    assert_ne!(
        second.body["tools"],
        json!([]),
        "below threshold nothing happens"
    );
    assert_strict_append(&first.body, &second.body, "below threshold");
    let mut eligible_pairs = std::collections::BTreeSet::new();
    assert!(eligible_pairs.insert(selected_completed_pair(&control, &thread).await));
    let prepared_active =
        prepared_report_from_incoming(&control, &trigger_turn, 2, &second, 200).await;
    second
        .reply
        .send((200, bx_tool("call-1", 200, command)))
        .unwrap();
    let first_a = known_first_a_from_reports(&control, &thread, &[&prepared_first], 50).await;
    assert_ne!(
        first_a.report_attempt_id, prepared_active.owner.attempt_id,
        "the 200-token active report cannot overwrite the first known 50-token A"
    );
    let mut next_call = 2;
    let mut summary_request = None;
    let mut foreground_finished = false;
    loop {
        let request = incoming(&control, &mut receive).await;
        match request.kind() {
            ProviderRequestKind::Summary => {
                summary_request = Some(request);
                break;
            }
            ProviderRequestKind::Ordinary if next_call < 4 => {
                let previous_call = format!("call-{}", next_call - 1);
                assert!(
                    request.body["input"].as_array().is_some_and(|items| items
                        .iter()
                        .any(|item| item["call_id"] == previous_call)),
                    "each natural continuation owns the previous completed call"
                );
                assert!(eligible_pairs.insert(selected_completed_pair(&control, &thread).await));
                request
                    .reply
                    .send((200, bx_tool(&format!("call-{next_call}"), 10, command)))
                    .unwrap();
                next_call += 1;
            }
            ProviderRequestKind::Ordinary => {
                assert!(
                    request.body["input"]
                        .as_array()
                        .is_some_and(|items| items.iter().any(|item| item["call_id"] == "call-3")),
                    "only the natural post-fourth-call request can finish foreground work"
                );
                request
                    .reply
                    .send((200, answer_with_usage("done", 10)))
                    .unwrap();
                foreground_finished = true;
                break;
            }
        }
    }

    let events_before_finish = proactive_events(&control, &thread).await;
    let active_event = events_before_finish
        .iter()
        .find(|event| event["attempt_id"] == prepared_active.owner.attempt_id)
        .expect("the predeclared current report owns exactly one automatic attempt");
    let active_boundary =
        start_report_boundary(&control, active_event, prepared_active, &accepted).await;
    let mut summary_includes_call1 = None;
    let mut summary_includes_call2 = None;
    if let Some(summary) = summary_request {
        let input = summary.body["input"].to_string();
        summary_includes_call1 = Some(input.contains("call-1"));
        summary_includes_call2 = Some(input.contains("call-2"));
        assert!(
            input.contains("call-0"),
            "the oldest complete group is selected"
        );
        assert!(
            !input.contains("call-3"),
            "the latest complete group stays protected"
        );
        assert!(
            !summary_includes_call2.unwrap() || summary_includes_call1.unwrap(),
            "the captured region is one predeclared whole-group prefix: 0, 0+1, or 0+1+2"
        );
        let append_thread = thread.clone();
        let append_turn = trigger_turn.clone();
        control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                shellboy::history::append_node(
                    &tx,
                    &append_thread,
                    "gc-append-during-provider",
                    Some(&append_turn),
                    "user",
                    &json!({"role":"user","content":"GC-APPENDED-TAIL"}),
                    "gc-append",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        summary
            .reply
            .send((200, answer("short middle summary")))
            .unwrap();
        while next_call < 4 {
            let continuation = incoming_ordinary(&control, &mut receive).await;
            let previous_call = format!("call-{}", next_call - 1);
            assert!(
                continuation.body["input"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["call_id"] == previous_call)),
                "the natural post-Summary continuation owns its preceding call"
            );
            assert!(eligible_pairs.insert(selected_completed_pair(&control, &thread).await));
            continuation
                .reply
                .send((200, bx_tool(&format!("call-{next_call}"), 10, command)))
                .unwrap();
            next_call += 1;
        }
        if !foreground_finished {
            let tail = incoming_ordinary(&control, &mut receive).await;
            assert!(
                tail.body["input"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["call_id"] == "call-3")),
                "the real fourth group completes before the Driver answers foreground work"
            );
            tail.reply
                .send((200, answer_with_usage("done", 10)))
                .unwrap();
            foreground_finished = true;
        }
    }
    assert_eq!(
        next_call, 4,
        "the four predeclared shell calls execute exactly once"
    );
    if foreground_finished {
        settled(&control, &thread).await;
    }
    let actual_calls = control.store.database.call(move |db| {
        db.prepare("SELECT c.call_id,count(*) FROM tool_call c JOIN shell_execution s ON s.operation_id=c.operation_id GROUP BY c.call_id ORDER BY c.call_id")?
            .query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }).await.unwrap();
    assert_eq!(
        actual_calls,
        vec![
            ("call-0".into(), 1),
            ("call-1".into(), 1),
            ("call-2".into(), 1),
            ("call-3".into(), 1)
        ],
        "the four independently named shell calls execute once each"
    );
    let finish_turn_id = active_boundary.report.row_turn_id.clone();
    let finish_wait =
        automatic_attempts_finished_for_turns(&control, std::slice::from_ref(&finish_turn_id), 1);
    tokio::pin!(finish_wait);
    let active_terminal = if foreground_finished && summary_includes_call1.is_none() {
        loop {
            tokio::select! {
                attempts = &mut finish_wait => break attempts,
                incoming = receive.recv() => {
                    let incoming = incoming.expect("provider remains available through the exact FINISH");
                    assert_eq!(incoming.kind(), ProviderRequestKind::Summary,
                        "after the final Ordinary response only the pending automatic Summary may remain");
                    let input = incoming.body["input"].to_string();
                    summary_includes_call1 = Some(input.contains("call-1"));
                    summary_includes_call2 = Some(input.contains("call-2"));
                    assert!(input.contains("call-0"));
                    assert!(!input.contains("call-3"));
                    assert!(!summary_includes_call2.unwrap() || summary_includes_call1.unwrap(),
                        "the captured region is one predeclared whole-group prefix: 0, 0+1, or 0+1+2");
                    let append_thread = thread.clone();
                    let append_turn = trigger_turn.clone();
                    control.store.database.call(move |db| {
                        let tx = db.transaction()?;
                        shellboy::history::append_node(
                            &tx, &append_thread, "gc-append-during-provider", Some(&append_turn),
                            "user", &json!({"role":"user","content":"GC-APPENDED-TAIL"}), "gc-append",
                        )?;
                        tx.commit()?;
                        Ok(())
                    }).await.unwrap();
                    incoming.reply.send((200, answer("short middle summary"))).unwrap();
                }
            }
        }
    } else {
        finish_wait.await
    };
    let active_record =
        automatic_attempt_record(&control, active_boundary.report.attempt_id.clone()).await;
    let spills = selected_compaction_nodes(&control, &thread, "spill").await;
    let summaries = selected_compaction_nodes(&control, &thread, "summary").await;
    assert_eq!(
        summaries.len(),
        usize::from(summary_includes_call1.is_some())
    );
    let expected_outcome = if !summaries.is_empty() || !spills.is_empty() {
        "adopted"
    } else {
        "no_target"
    };
    assert_eq!(
        eligible_pairs.len(),
        3,
        "fixture has three older complete tool groups"
    );
    for id in &spills {
        let args = serde_json::from_value(json!({"node_id": id})).unwrap();
        let spill = request(&control, Command::HistoryRead(args)).await;
        let sources: Vec<String> =
            serde_json::from_value(spill["source_node_ids"].clone()).unwrap();
        assert!(
            eligible_pairs.remove(&sources),
            "spill sources must be one unused older call/result pair: {sources:?}"
        );
    }
    let active_finished = active_boundary.finish(&active_terminal[0], &active_record);
    assert_eq!(active_finished.outcome, expected_outcome);
    let attempts = bx_attempts(&control, &thread).await;
    assert_eq!(
        attempts.len(),
        1,
        "the predeclared active report owns one attempt: {attempts:#?}"
    );
    let active_event = proactive_events(&control, &thread)
        .await
        .into_iter()
        .find(|event| event["attempt_id"] == active_finished.boundary.report.attempt_id)
        .expect("the one persisted terminal attempt is the exact named report event");
    assert_eq!(
        active_event["attempt_id"],
        active_finished.boundary.report.attempt_id
    );
    assert_eq!(attempts[0]["state"], expected_outcome);
    // A new request after exact original report/token FINISH is the canonical
    // path cut in either ordinary-first or summary-first scheduling.
    send(&control, &thread, "verify appended tail after FINISH").await;
    let verified = incoming_ordinary(&control, &mut receive).await;
    assert_ne!(verified.body["tools"], json!([]));
    let verified_input = verified.body["input"].to_string();
    assert!(
        verified_input.contains("call-3"),
        "the latest group remains protected after exact FINISH"
    );
    for (call, selected) in [
        ("call-1", summary_includes_call1),
        ("call-2", summary_includes_call2),
    ] {
        if selected == Some(true) {
            assert!(
                !verified_input.contains(call),
                "selected source {call} is replaced only after exact FINISH"
            );
        } else {
            assert!(
                verified_input.contains(call),
                "unselected/protected source {call} remains after exact FINISH"
            );
        }
    }
    assert_eq!(
        verified_input.matches("GC-APPENDED-TAIL").count(),
        usize::from(summary_includes_call1.is_some()),
        "the active provider-wait append is retained exactly once when its Summary was received"
    );
    verified
        .reply
        .send((200, answer_with_usage("verified appended tail", 10)))
        .unwrap();
    settled(&control, &thread).await;
    // Summary ledger `begin` commits before POST; the fixture tracks whether its Summary was received.
    // Summary witness changes to model_attempt with #328.
    assert_eq!(
        summary_request_count_for_thread(&control, &thread).await,
        i64::from(summary_includes_call1.is_some())
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_large_protected_turn_is_a_local_no_target_attempt() {
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
    next.reply
        .send((200, answer_with_usage("done", 10)))
        .unwrap();
    settled(&control, &thread).await;
    let events = proactive_events(&control, &thread).await;
    assert_eq!(
        events.len(),
        1,
        "the predeclared 150-token report has one no-target attempt: {events:#?}"
    );
    assert_eq!(events[0]["trigger"]["input_tokens"], 150);
    let trigger_turn = events[0]["turn_id"]
        .as_str()
        .expect("saved no-target Turn")
        .to_owned();
    let terminal = automatic_attempts_finished_for_turns(&control, &[trigger_turn], 1).await;
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].id, events[0]["attempt_id"]);
    let projected = thread_get_projection(&control, &thread).await;
    assert!(
        projected.snapshot.automatic_compaction.is_none(),
        "after the actual no-target FINISH, projection no longer shows compaction active"
    );
    let attempts = bx_attempts(&control, &thread).await;
    assert_eq!(attempts.len(), 1, "{attempts:#?}");
    assert_eq!(attempts[0]["state"], "no_target");
    assert_eq!(attempts[0]["trigger"]["input_tokens"], 150);
    assert!(
        selected_compaction_nodes(&control, &thread, "spill")
            .await
            .is_empty()
    );
    assert!(
        selected_compaction_nodes(&control, &thread, "summary")
            .await
            .is_empty()
    );
    // The Summary ledger `begin` commits before POST; no-target admits no Summary.
    // Summary witness changes to model_attempt with #328.
    assert_eq!(summary_request_count_for_thread(&control, &thread).await, 0);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

/// #303 §2.6/§3: #A <= target selects spill only, and that branch touches only
/// the closed region. With no closed tool group it ends `no_target` without a
/// model request, and the current Turn's older tool group stays unchanged.
/// Thread deletion removes #A state and both failure slots.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_spill_only_without_closed_candidates_leaves_current_turn() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=100\n").await;
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    send(&control, &thread, "only completed task").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer_with_usage("saved", 10)))
        .unwrap();
    settled(&control, &thread).await;
    send(&control, &thread, "current task").await;
    let command = if cfg!(windows) {
        "powershell -NoProfile -Command \"[Console]::Write(('x' * 50000))\""
    } else {
        "head -c 50000 /dev/zero | tr '\\0' x"
    };
    let first = incoming(&control, &mut receive).await;
    assert_ne!(first.body["tools"], json!([]));
    first
        .reply
        .send((200, bx_tool("older-tool", 50, command)))
        .unwrap();
    let second = incoming(&control, &mut receive).await;
    assert_ne!(second.body["tools"], json!([]));
    second
        .reply
        .send((200, bx_tool("single-tool", 9999, "echo synthetic")))
        .unwrap();
    let third = incoming(&control, &mut receive).await;
    assert_ne!(
        third.body["tools"],
        json!([]),
        "spill only sends no summary"
    );
    assert_strict_append(
        &second.body,
        &third.body,
        "the spill-only branch leaves the current Turn unchanged",
    );
    let projected = thread_get_projection(&control, &thread).await;
    assert!(
        projected.snapshot.automatic_compaction.is_none(),
        "finished work must not appear active"
    );
    third
        .reply
        .send((200, answer_with_usage("done", 10)))
        .unwrap();
    settled(&control, &thread).await;
    let attempts = bx_attempts(&control, &thread).await;
    assert_eq!(attempts.len(), 1, "{attempts:#?}");
    assert_eq!(attempts[0]["state"], "no_target");
    assert_eq!(attempts[0]["trigger"]["input_tokens"], 9999);
    assert!(
        selected_compaction_nodes(&control, &thread, "spill")
            .await
            .is_empty()
    );
    assert!(
        selected_compaction_nodes(&control, &thread, "summary")
            .await
            .is_empty()
    );
    assert_eq!(bx_failure_slots(&control, &thread).await, (None, None));
    // The Summary ledger `begin` commits before POST; this spill-only branch admits none.
    // Summary witness changes to model_attempt with #328.
    assert_eq!(summary_request_count_for_thread(&control, &thread).await, 0);
    let keys = [
        format!("compact.a_state.{thread}"),
        format!("compact.deterministic_range_failure.closed.{thread}"),
        format!("compact.deterministic_range_failure.active.{thread}"),
    ];
    let count_keys = |keys: [String; 3]| async {
        control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key IN (?1,?2,?3)",
                    [&keys[0], &keys[1], &keys[2]],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
    };
    assert_eq!(
        count_keys(keys.clone()).await,
        1,
        "#A state exists before deletion"
    );
    // Synthetic failure slots: deletion must remove both.
    let synthetic = keys.clone();
    control
        .store
        .database
        .call(move |db| {
            for key in &synthetic[1..3] {
                db.execute(
                    "INSERT INTO app_meta(key,value) VALUES(?1,'synthetic')",
                    [key],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(count_keys(keys.clone()).await, 3);
    request(
        &control,
        Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: thread.clone(),
            descendants: false,
            preview_marker: None,
        }),
    )
    .await;
    assert_eq!(
        count_keys(keys).await,
        0,
        "Thread deletion removes #A state and failure slots"
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
            let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(16);
            let listener = shellboy::ipc::LocalServer::bind(&owner, rpc_send)
                .await
                .unwrap();
            tokio::spawn(control.clone().serve(rpc_receive));
            for verdicts in [true, false] {
                let target = thread(&control).await;
                let accepted: MessageAccepted = serde_json::from_value(
                    request(
                        &control,
                        Command::MessageSend(MessageSendArgs {
                            max_model_requests: None,
                            resume_recovery: false,

                            human: false,
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
                assert_eq!(
                    model_attempt_count_for_thread(&control, &target).await,
                    1,
                    "the completed content_filter Turn adds no retry attempt"
                );
            }
            listener.shutdown().await.unwrap();
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
            server.abort();
        }
    }
}

// #183: the per-Turn `Shellboy context v1` item is part of the saved history,
// so each request of a Turn is a strict append of the previous request.
fn request_scope(body: &Value) -> (String, String) {
    let line = context_lines(body)
        .into_iter()
        .find(|line| line.starts_with("Shellboy context v1:"))
        .expect("provider request carries its own Run/Turn context");
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .expect("context identity field")
            .to_owned()
    };
    (field("run"), field("turn"))
}
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
/// The context line for the named Turn's current Thread settings.
async fn current_context(control: &Control, turn: &str) -> String {
    let turn = turn.to_owned();
    control.store.database.call(move |db| Ok(db.query_row(
        "SELECT t.id,u.run_id,u.id,t.cwd,t.profile_id,t.model_id,t.effort,COALESCE(s.source_path,t.agents_path) FROM turn u JOIN thread t ON t.id=u.thread_id LEFT JOIN instruction_snapshot s ON s.id=u.instruction_snapshot_id WHERE u.id=?1",
        [turn],
        |r| Ok(format!(
            "Shellboy context v1: thread={} run={} turn={} cwd={} connection={} model={} effort={:?}; AGENTS reference={}",
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
            r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, String>(7)?,
        )),
    )?)).await.unwrap()
}
/// The current settings appear once and are the latest saved context. Run/Turn
/// IDs identify the original observation, not the request carrying it.
#[track_caller]
fn assert_current_context_once(body: &Value, expected: &str, label: &str) {
    fn settings(line: &str) -> (&str, &str) {
        let (thread, ids) = line.split_once(" run=").unwrap();
        let (_, turn) = ids.split_once(" turn=").unwrap();
        let (_, rest) = turn.split_once(" cwd=").unwrap();
        (thread, rest)
    }
    let lines = context_lines(body);
    let expected_settings = settings(expected);
    assert_eq!(
        lines
            .iter()
            .filter(|line| settings(line) == expected_settings)
            .count(),
        1,
        "{label}: current settings must appear exactly once; expected {expected:?}; got {lines:#?}"
    );
    assert_eq!(
        lines.last().map(|line| settings(line)),
        Some(expected_settings),
        "{label}"
    );
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
    let first_accepted = send_accepted(&control, &thread, "append task").await;
    let first_turn = first_accepted.turn_id.expect("accepted append Turn");
    let first = incoming(&control, &mut receive).await;
    let context = current_context(&control, &first_turn).await;
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
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            cwd: Some(moved.to_str().unwrap().into()),
            effort: Some("high".into()),
            ..Default::default()
        })),
    )
    .await;
    second
        .reply
        .send((200, tool_call("two", "echo two")))
        .unwrap();
    let third = incoming(&control, &mut receive).await;
    assert_strict_append(&second.body, &third.body, "mid-Turn cwd/effort change");
    let changed = current_context(&control, &first_turn).await;
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
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            model: Some("fixture-next".into()),
            ..Default::default()
        })),
    )
    .await;
    third
        .reply
        .send((200, tool_call("three", "echo three")))
        .unwrap();
    let fourth = incoming(&control, &mut receive).await;
    assert_eq!(fourth.body["model"], "fixture-next");
    assert_strict_append(&third.body, &fourth.body, "mid-Turn model change");
    let model_changed = current_context(&control, &first_turn).await;
    assert!(
        model_changed.contains("model=fixture-next "),
        "{model_changed}"
    );
    assert_current_context_once(&fourth.body, &model_changed, "mid-Turn model change");
    fourth.reply.send((200, answer("done"))).unwrap();
    settled(&control, &thread).await;
    // The next Turn appends without re-announcing unchanged settings.
    send_accepted(&control, &thread, "next turn").await;
    let fifth = incoming(&control, &mut receive).await;
    assert_strict_append(&fourth.body, &fifth.body, "next Turn");
    assert_eq!(context_lines(&fifth.body), context_lines(&fourth.body));
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
    // Compare the exact new suffix at each of several unchanged Turn boundaries.
    let mut previous = retried.body;
    let mut suffix_sizes = Vec::new();
    for _ in 0..16 {
        send(&control, &thread, "same settings").await;
        let request = incoming(&control, &mut receive).await;
        assert_strict_append(&previous, &request.body, "unchanged Turn");
        assert_eq!(context_lines(&request.body), context_lines(&previous));
        let start = previous["input"].as_array().unwrap().len();
        let suffix = &request.body["input"].as_array().unwrap()[start..];
        let start_fact = suffix
            .iter()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter_map(|part| part["text"].as_str()?.strip_prefix("Shellboy event v1: "))
            .filter_map(|text| serde_json::from_str::<Value>(text).ok())
            .find(|fact| fact["kind"] == "turn_started")
            .expect("compact Turn start");
        assert!(start_fact.get("payload").is_none());
        // Normalize only clock-dependent fields; the added Turn record must
        // stay the same size regardless of how many records already replay.
        let mut record = start_fact;
        record["at"] = json!("timestamp");
        record["change"]["previous_turn_ended_at"] = json!("timestamp");
        suffix_sizes.push(serde_json::to_vec(&record).unwrap().len());
        request.reply.send((200, answer("finished"))).unwrap();
        settled(&control, &thread).await;
        previous = request.body;
    }
    assert!(
        suffix_sizes.windows(2).all(|pair| pair[0] == pair[1]),
        "{suffix_sizes:?}"
    );
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
    let accepted = send_accepted(&control, &thread, "omission task").await;
    let turn_id = accepted.turn_id.expect("accepted omission Turn");
    let mut current = incoming(&control, &mut receive).await;
    for call in ["one", "two"] {
        current
            .reply
            .send((200, tool_call(call, &format!("echo {call}"))))
            .unwrap();
        current = incoming(&control, &mut receive).await;
    }
    let context = current_context(&control, &turn_id).await;
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
            .contains("Note: intermediate work in the current Turn was also omitted."),
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

/// #271 under #303 §2.2/§3: below the threshold nothing happens, so every
/// request strictly appends. When a report recrosses it with #A <= target, the
/// spill-only branch spills every eligible closed tool group at once (no
/// stop at a target) and keeps the latest complete tool group.
#[tokio::test]
async fn issue_271_spill_shortfall_waits_until_threshold_recrossing() {
    crate::test_watchdog::watchdog("issue 271 repeated spill", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner =
            proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200000\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        send(&control, &target, "seed").await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("seed answer", 100)))
            .unwrap();
        settled(&control, &target).await;
        send(&control, &target, "synthetic repeated spill").await;
        let command = if cfg!(windows) {
            "[string]::new([char]48, 20000)"
        } else {
            "printf %020000d 0"
        };
        let mut previous: Option<Value> = None;
        let mut epochs = Vec::new();
        for n in 0..8 {
            let request = incoming(&control, &mut receive).await;
            assert_ne!(request.body["tools"], json!([]));
            if let Some(previous) = &previous {
                assert_strict_append(
                    previous,
                    &request.body,
                    "below-threshold requests are not rewritten",
                );
            }
            let id = target.clone();
            epochs.push(
                control
                    .store
                    .database
                    .call(move |db| {
                        Ok(db.query_row(
                            "SELECT reasoning_epoch FROM thread WHERE id=?1",
                            [id],
                            |r| r.get::<_, i64>(0),
                        )?)
                    })
                    .await
                    .unwrap(),
            );
            previous = Some(request.body.clone());
            let reply = if n < 7 {
                bx_tool(&format!("issue271-{n}"), 190_000, command)
            } else {
                answer_with_usage("crossing", 200_001)
            };
            request.reply.send((200, reply)).unwrap();
        }
        settled(&control, &target).await;
        assert!(
            epochs.windows(2).all(|pair| pair[0] == pair[1]),
            "{epochs:?}"
        );
        send(&control, &target, "after crossing").await;
        let after = incoming(&control, &mut receive).await;
        assert_ne!(
            after.body["tools"],
            json!([]),
            "#A <= target sends no summary"
        );
        assert_eq!(
            bx_omitted_outputs(&after.body),
            6,
            "every older closed tool group is spilled"
        );
        let latest = |body: &Value| {
            body["input"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["call_id"] == "issue271-6" && item.get("output").is_some())
                .cloned()
                .unwrap()
        };
        assert_eq!(
            latest(&after.body),
            latest(previous.as_ref().unwrap()),
            "the latest complete tool group stays"
        );
        after.reply.send((200, answer("finished"))).unwrap();
        settled(&control, &target).await;
        let attempts = bx_attempts(&control, &target).await;
        assert_eq!(attempts.len(), 1, "{attempts:#?}");
        assert_eq!(attempts[0]["state"], "adopted");
        assert_eq!(attempts[0]["trigger"]["input_tokens"], 200_001);
        assert_eq!(
            selected_compaction_nodes(&control, &target, "spill")
                .await
                .len(),
            6
        );
        assert!(
            selected_compaction_nodes(&control, &target, "summary")
                .await
                .is_empty()
        );
        // This spill branch admits no Summary POST; `begin` commits before send.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &target).await, 0);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

/// #303 §2.2 (replaces #209 target escalation): after a shallow closed
/// summary, a report below the threshold starts nothing, so the next request
/// strictly appends; the old target-unmet continuation is retired (§9).
#[tokio::test]
async fn proactive_shallow_summary_does_not_continue_below_threshold() {
    crate::test_watchdog::watchdog("proactive below threshold after summary", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner =
            proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200000\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        for n in 0..2 {
            send(&control, &target, &format!("old-{n} {}", "a".repeat(4000))).await;
            incoming(&control, &mut receive)
                .await
                .reply
                .send((200, answer_with_usage("old answer", 50_000)))
                .unwrap();
            settled(&control, &target).await;
        }
        let trigger_turn = send_accepted(&control, &target, "current work")
            .await
            .turn_id
            .unwrap();
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, bx_tool("step-0", 201_061, "echo STEP0")))
            .unwrap();
        let prefix = incoming_summary(&control, &mut receive).await;
        assert_eq!(prefix.body["tools"], json!([]));
        let input = prefix.body["input"].to_string();
        assert!(input.contains("old-0") && !input.contains("old-1"));
        prefix
            .reply
            .send((200, answer("## Goal\nOld work retained")))
            .unwrap();
        let first_after = incoming(&control, &mut receive).await;
        assert_ne!(first_after.body["tools"], json!([]));
        first_after
            .reply
            .send((200, bx_tool("step-1", 184_622, "echo STEP1")))
            .unwrap();
        let below = incoming(&control, &mut receive).await;
        assert_ne!(
            below.body["tools"],
            json!([]),
            "no summary below the threshold"
        );
        // The first ordinary request can use the pre-FINISH cut; the later
        // request must reflect the committed closed-summary replacement while
        // retaining this newly completed tool result.
        let before_projection = first_after.body["input"].to_string();
        let after_projection = below.body["input"].to_string();
        assert!(
            before_projection.contains("old-0"),
            "the earlier request uses its captured pre-adoption cut"
        );
        assert!(
            after_projection.contains("Old work retained"),
            "the later request contains the adopted summary"
        );
        assert!(
            !after_projection.contains("old-0"),
            "the replaced closed source is no longer replayed"
        );
        assert!(
            after_projection.contains("STEP1"),
            "the new ordinary tool result remains in history"
        );
        below
            .reply
            .send((200, answer_with_usage("done", 140_000)))
            .unwrap();
        settled(&control, &target).await;
        automatic_attempts_finished_for_turns(&control, &[trigger_turn], 1).await;
        let attempts = bx_attempts(&control, &target).await;
        assert_eq!(attempts.len(), 1, "{attempts:#?}");
        let attempt = &attempts[0];
        assert_eq!(attempt["state"], "adopted");
        assert_eq!(attempt["trigger"]["input_tokens"], 201_061);
        let events = proactive_events(&control, &target).await;
        assert_eq!(events.len(), 1);
        read_summary(&control, &events[0]).await;
        assert!(attempt.get("target_verification").is_none(), "{attempt:#?}");
        // The Summary ledger `begin` commits before POST; the closed-summary stage has one request.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &target).await, 1);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

/// #303 §2.2/§4.2 (replaces #209 prefix shortfall): a newly eligible older
/// Turn waits for the next report at the threshold; that closed summary then
/// summarizes the earlier summary together with the new older Turn.
#[tokio::test]
async fn proactive_next_closed_summary_resummarizes_prior_summary_with_new_older_turn() {
    crate::test_watchdog::watchdog("proactive closed resummary across turns", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner =
            proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200000\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        for n in 0..2 {
            send(&control, &target, &format!("old-{n} {}", "a".repeat(4000))).await;
            incoming(&control, &mut receive)
                .await
                .reply
                .send((200, answer_with_usage("old answer", 50_000)))
                .unwrap();
            settled(&control, &target).await;
        }
        let turn_a = send_accepted(
            &control,
            &target,
            &format!("TURN_A_MARKER {}", "b".repeat(4000)),
        )
        .await.turn_id.unwrap();
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, bx_tool("step-a", 201_061, "echo A")))
            .unwrap();
        let prefix = incoming_summary(&control, &mut receive).await;
        assert_eq!(prefix.body["tools"], json!([]));
        assert!(!prefix.body["input"].to_string().contains("old-1"));
        prefix
            .reply
            .send((200, answer("## Goal\nOld work retained")))
            .unwrap();
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("turn A finished", 184_622)))
            .unwrap();
        settled(&control, &target).await;
        automatic_attempts_finished_for_turns(&control, &[turn_a], 1).await;
        let turn_b = send_accepted(&control, &target, "TURN_B_MARKER").await.turn_id.unwrap();
        let first = incoming_ordinary(&control, &mut receive).await;
        assert_ne!(
            first.body["tools"],
            json!([]),
            "below the threshold nothing happens"
        );
        first
            .reply
            .send((200, bx_tool("step-b", 200_500, "echo B")))
            .unwrap();
        let resummary = incoming_summary(&control, &mut receive).await;
        assert_eq!(
            resummary.body["tools"],
            json!([]),
            "the next eligible report summarizes the closed region"
        );
        let input = resummary.body["input"].to_string();
        assert!(
            input.contains("Old work retained"),
            "the earlier summary is summarized again"
        );
        assert!(
            input.contains("old-1"),
            "the newly eligible older Turn is included"
        );
        assert!(
            !input.contains("TURN_A_MARKER"),
            "the latest completed Turn stays"
        );
        resummary
            .reply
            .send((200, answer("## Goal\nOlder work retained")))
            .unwrap();
        let next = incoming(&control, &mut receive).await;
        assert_ne!(next.body["tools"], json!([]), "one summary per attempt");
        next.reply
            .send((200, answer_with_usage("turn B finished", 140_000)))
            .unwrap();
        settled(&control, &target).await;
        automatic_attempts_finished_for_turns(&control, &[turn_b], 1).await;
        let attempts = bx_attempts(&control, &target).await;
        assert_eq!(attempts.len(), 2, "{attempts:#?}");
        for attempt in &attempts {
            assert_eq!(attempt["state"], "adopted", "{attempt:#?}");

        }
        let events = proactive_events(&control, &target).await;
        assert_eq!(events.len(), 2);
        for event in &events {
            assert_eq!(event["state"], "adopted");
            read_summary(&control, event).await;
        }
        assert_eq!(attempts[1]["trigger"]["input_tokens"],200_500);
        let summary_ledger_target = target.clone();
        let target=target.clone();
        let (stale_report_token,current_report_token,current_a_boundary):(String,String,String)=control.store.database.call(move|db|{
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt WHERE turn_id IN (SELECT id FROM turn WHERE thread_id=?1) AND json_extract(usage_json,'$.input_tokens')=184622", [&target], |row| row.get::<_,i64>(0))?, 1,
                "the response's exact 184622 usage identifies one report attempt");
            let stale:String=db.query_row(
                "SELECT json_extract(input_manifest_json,'$.compaction_report_token') FROM model_attempt WHERE turn_id IN (SELECT id FROM turn WHERE thread_id=?1) AND json_extract(usage_json,'$.input_tokens')=184622",
                [&target],|row|row.get(0))?;
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt WHERE turn_id IN (SELECT id FROM turn WHERE thread_id=?1) AND json_extract(usage_json,'$.input_tokens')=200500 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id)", [&target], |row| row.get::<_,i64>(0))?, 1,
                "the response's exact 200500 usage identifies one terminal report attempt");
            let current:(String,String)=db.query_row(
                "SELECT json_extract(input_manifest_json,'$.compaction_report_token'),(SELECT json_extract(c.trigger_json,'$.a_boundary') FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id) FROM model_attempt WHERE turn_id IN (SELECT id FROM turn WHERE thread_id=?1) AND json_extract(usage_json,'$.input_tokens')=200500 AND EXISTS(SELECT 1 FROM automatic_compaction c WHERE c.attempt_id=model_attempt.id)",
                [&target],|row|Ok((row.get(0)?,row.get(1)?)))?;
            Ok((stale,current.0,current.1))
        }).await.unwrap();
        assert_ne!(stale_report_token,current_a_boundary,
            "the 184622 response began before FINISH reset A and cannot populate the next boundary");
        assert_eq!(current_report_token,current_a_boundary,
            "the 200500 response manifest matches the accepted post-reset A boundary");
        // Both Summary ledger `begin` calls commit before POST.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &summary_ledger_target).await, 2);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

/// #303 §4.1/§4.2 (replaces #209 same-attempt widening): one attempt performs
/// one summary operation. The closed summary ends the attempt; the active
/// rounds are not summarized before the next ordinary request, and the stage
/// keeps the one request's actual usage.
#[tokio::test]
async fn proactive_attempt_runs_one_summary_operation_and_keeps_its_usage() {
    crate::test_watchdog::watchdog("proactive one summary per attempt", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner =
            proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200000\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        for n in 0..2 {
            let prior = request(
                &control,
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: Deliver::Next,
                    thread_id: Some(target.clone()),
                    text: format!("old-{n} {}", "a".repeat(4000)),
                }),
            )
            .await;
            let run_id = prior["run_id"].as_str().expect("prior Run ID").to_owned();
            incoming(&control, &mut receive)
                .await
                .reply
                .send((200, answer_with_usage("old answer", 50_000)))
                .unwrap();
            // A finished Turn can still belong to a live Run, which rejects a capped send.
            let ended = request(
                &control,
                Command::Wait(WaitArgs {
                    after: None,
                    conditions: vec![WaitCondition::RunEnded { run_id }],
                    timeout_ms: 5_000,
                }),
            )
            .await;
            assert_eq!(ended["status"], "satisfied", "{ended}");
        }
        let accepted: MessageAccepted = serde_json::from_value(
            request(
                &control,
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: std::num::NonZeroU32::new(5),
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: Deliver::Next,
                    thread_id: Some(target.clone()),
                    text: format!("current {}", "b".repeat(40_000)),
                }),
            )
            .await,
        )
        .unwrap();
        let run_id = accepted.run_id.unwrap();
        let trigger_turn = accepted.turn_id.expect("current request Turn");
        for (n, tokens) in [(0, 50_000), (1, 50_000), (2, 201_061)] {
            incoming(&control, &mut receive)
                .await
                .reply
                .send((
                    200,
                    bx_tool(&format!("widen-{n}"), tokens, &format!("echo WIDEN{n}")),
                ))
                .unwrap();
        }
        let prefix = incoming_summary(&control, &mut receive).await;
        assert_eq!(prefix.body["tools"], json!([]));
        assert!(
            !prefix.body["input"].to_string().contains("WIDEN0"),
            "closed region first"
        );
        prefix
            .reply
            .send((200, answer_with_usage("## Goal\nOld work", 11)))
            .unwrap();
        let after = incoming_ordinary(&control, &mut receive).await;
        assert_ne!(
            after.body["tools"],
            json!([]),
            "no second summary in the same attempt"
        );
        after
            .reply
            .send((200, answer_with_usage("done", 100_000)))
            .unwrap();
        settled(&control, &target).await;
        let run = request(&control, Command::RunGet(RunTarget { run_id })).await;
        assert_eq!(run["model_requests"], json!({"ceiling":5,"used":5}));
        let terminal = automatic_attempts_finished_for_turns(&control, &[trigger_turn], 1).await;
        assert_eq!(
            terminal.len(),
            1,
            "one expected attempt for the triggering Turn"
        );
        assert!(
            !terminal[0].token.is_empty(),
            "attempt token is bound before FINISH"
        );
        let attempts = bx_attempts(&control, &target).await;
        assert_eq!(attempts.len(), 1, "{attempts:#?}");
        assert_eq!(attempts[0]["state"], "adopted");
        let events = proactive_events(&control, &target).await;
        assert_eq!(events.len(), 1);
        read_summary(&control, &events[0]).await;
        let usage = &events[0]["metrics"]["summary_usage"];
        assert_eq!(usage["requests"], 1, "{events:#?}");
        assert_eq!(usage["input_tokens"]["known_sum"], 11);
        // The Summary ledger `begin` commits before POST; one completed Summary is recorded.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &target).await, 1);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_spill_then_cancelled_connection_wait_preserves_adoption() {
    crate::test_watchdog::watchdog("proactive shared cooldown cancellation", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = proactive_owner(root.path(), &url, "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        send(&control, &target, &"protected current ".repeat(70_000)).await;
        let large_command = if cfg!(windows) { "'0' * 50000" } else { "printf '%050000d' 0" };
        for index in 0..2 {
            incoming(&control, &mut receive).await.reply.send((200, json!({
                "id":format!("large-{index}"),"status":"completed","usage":{"input_tokens":100,"output_tokens":1},
                "output":[{"type":"function_call","call_id":format!("large-{index}"),"name":"shell","arguments":json!({"command":large_command}).to_string()}]
            }))).unwrap();
        }
        let before = incoming(&control, &mut receive).await;
        let other = thread(&control).await;
        send(&control, &other, "trigger shared rate limit").await;
        incoming(&control, &mut receive).await.reply.send((429, json!({"error":{"message":"synthetic"}}))).unwrap();
        settled(&control, &other).await;
        before.reply.send((200, json!({"id":"threshold","status":"completed","usage":{"input_tokens":200,"output_tokens":1},"output":[{"type":"function_call","call_id":"threshold","name":"shell","arguments":json!({"command":"echo last"}).to_string()}]}))).unwrap();
        let adopted = loop {
            let id = target.clone();
            let state = control.store.database.call(move |db| proactive_wait_witness(db, &id)).await.unwrap();
            if state.0.len() == 2 { break state; }
            tokio::task::yield_now().await;
        };
        assert_eq!(adopted.2.len(), 4, "both complete call/result groups retain provenance");
        crate::test_watchdog::watchdog("change model during proactive wait", request(&control,
            Command::ThreadSet(Box::new(ThreadSetArgs { thread_id: Some(target.clone()), model: Some("changed-model".into()), ..Default::default() }))
        )).await.unwrap();
        let id = target.clone();
        let changed = control.store.database.call(move |db| proactive_wait_witness(db, &id)).await.unwrap();
        assert_eq!(changed.0, adopted.0);
        assert_eq!(changed.2, adopted.2);
        assert_eq!(changed.1, adopted.1 + 1, "model change advances the reasoning epoch once");
        let run = request(&control, Command::ThreadGet(shellboy::protocol::ThreadGetArgs { thread_id: Some(target.clone()), display: false, })).await["run_id"].as_str().unwrap().to_owned();
        request(&control, Command::RunStop(RunTarget { run_id: run.clone() })).await;
        settled(&control, &target).await;
        assert_eq!(request(&control,Command::RunGet(RunTarget{run_id:run})).await["state"],"stopped");
        let id = target.clone();
        let after = control.store.database.call(move |db| proactive_wait_witness(db, &id)).await.unwrap();
        assert_eq!(after, changed, "cancelled admission preserves selected spills, the changed-model epoch, and ordered provenance");
        // Explicit RunStop cancels the in-flight summary and determines the
        // original attempt terminal outcome; the previously committed spill remains owned.
        let attempts = bx_attempts(&control, &target).await;
        assert_eq!(attempts.len(), 1, "{attempts:#?}");
        let record = &attempts[0];
        assert_eq!(record["state"], "cancelled", "{record:#?}");
        assert_eq!(record["compaction_outcome"]["outcome"]["reason"],"cancelled","{record:#?}");
        assert_eq!(selected_compaction_nodes(&control, &target, "spill").await, adopted.0);
        assert!(selected_compaction_nodes(&control, &target, "summary").await.is_empty());
        let slots=bx_failure_slots(&control,&target).await;
        assert!(slots.0.is_none()&&slots.1.is_none(),"source rejection is never failure memory: {slots:?}");
        // RunStop cancels this admission before Summary ledger `begin`, so no POST was dispatched.
        // Summary witness changes to model_attempt with #328.
        assert_eq!(summary_request_count_for_thread(&control, &target).await, 0);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }).await.unwrap();
}

fn proactive_wait_witness(
    db: &rusqlite::Connection,
    thread: &str,
) -> Result<(Vec<String>, i64, Vec<String>), shellboy::storage::StorageError> {
    let ids = db.prepare("SELECT n.id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='spill' ORDER BY p.ordinal")?.query_map([thread], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let epoch = db.query_row(
        "SELECT reasoning_epoch FROM thread WHERE id=?1",
        [thread],
        |r| r.get(0),
    )?;
    let sources = db.prepare("SELECT s.source_node_id FROM thread_path p JOIN replacement r ON r.output_node_id=p.node_id JOIN replacement_source s ON s.replacement_id=r.id JOIN node n ON n.id=s.source_node_id WHERE p.thread_id=?1 AND r.format_version='spill-v1' AND n.payload_json IS NOT NULL ORDER BY p.ordinal,s.ordinal")?.query_map([thread], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((ids, epoch, sources))
}

#[tokio::test]
async fn sender_label_reaches_the_request_only_when_present_and_not_human() {
    fn admission_preambles(body: &Value) -> Vec<String> {
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["role"] == "developer")
            .flat_map(|v| v["content"].as_array().into_iter().flatten())
            .filter_map(|v| v["text"].as_str())
            .filter(|t| t.starts_with("Shellboy received message: "))
            .map(str::to_owned)
            .collect()
    }
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let mut texts = Vec::new();
    for (label, human, text) in [
        (Some("app:todo"), false, "labelled"),
        (None, false, "plain"),
        (Some("typed"), true, "human"),
    ] {
        let created = request(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_thread_id: None,
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
                max_model_requests: None,
                resume_recovery: false,

                human,
                sender_label: label.map(str::to_owned),
                deliver: Deliver::Next,
                thread_id: Some(thread),
                text: text.into(),
            }),
        )
        .await;
        let sent = incoming(&control, &mut receive).await;
        texts.push((text, admission_preambles(&sent.body)));
        let input = sent.body["input"].as_array().unwrap();
        let position = input
            .iter()
            .position(|v| v["content"][0]["text"] == text)
            .expect("message text is unchanged");
        if label.is_some() && !human {
            let preamble = &input[position
                .checked_sub(1)
                .expect("receipt preamble precedes the body")];
            assert_eq!(preamble["role"], "developer");
            assert!(
                preamble["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Shellboy received message: ")
            );
        }
    }
    assert_eq!(texts[0].1.len(), 1, "{:?}", texts[0]);
    assert!(texts[0].1[0].contains("app:todo") && texts[0].1[0].contains("not verified"));
    assert!(texts[1].1.is_empty(), "no label, no preamble");
    assert!(texts[2].1.is_empty(), "human sender, no preamble");
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn proactive_provider_500_retries_for_a_new_active_range() {
    crate::test_watchdog::watchdog("provider500 across source-prescribed A epochs", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = proactive_owner(
            root.path(), &url,
            "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n",
        ).await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        let failure = (500, json!({"error":{"code":"invalid_request_error","message":"synthetic range-scoped 500"}}));
        let declared = [(202_u64, "source-range-first-202"), (205_u64, "source-range-next-205")];
        let mut report_owners = Vec::new();
        let mut finished_tokens = Vec::new();
        let mut captures = Vec::new();

        for (index, (input_tokens, prompt)) in declared.into_iter().enumerate() {
            let accepted = send_accepted(&control, &target, prompt).await;
            assert!(accepted.run_id.is_some(), "each report epoch is owned by a real Driver Run");
            let turn_id = accepted.turn_id.expect("declared report Turn");
            let seed = incoming_ordinary(&control, &mut receive).await;
            let seed_id = if index == 0 { "grow-first-seed" } else { "grow-5" };
            let seed_report = prepared_report_from_incoming(&control, &turn_id, 1, &seed, 100).await;
            seed.reply.send((200, json!({
                "id":seed_id, "status":"completed",
                "usage":{"input_tokens":100,"output_tokens":1},
                "output":[{"type":"function_call","call_id":seed_id,"name":"nope","arguments":"{}"}]
            }))).unwrap();
            let ordinary = incoming_ordinary(&control, &mut receive).await;
            let report = prepared_report_from_incoming(
                &control, &turn_id, 2, &ordinary, input_tokens,
            ).await;
            let manifest: Value = serde_json::from_str(&report.owner.request_token).unwrap();
            let meta_key = format!("compact.a_state.{target}");
            let seed_manifest: Value = serde_json::from_str(&seed_report.owner.request_token).unwrap();
            let a_state: Value = control.store.database.call(move |db| {
                let text: String = db.query_row("SELECT value FROM app_meta WHERE key=?1", [meta_key], |row| row.get(0))?;
                Ok(serde_json::from_str(&text).expect("live A-state"))
            }).await.unwrap();
            assert_eq!(manifest["boundary"], index,
                "new report owner starts only after the prior same-Thread FINISH reset: {manifest:#?}");
            assert_eq!(manifest["scope"], a_state["scope_generation"]);
            assert_eq!(manifest["boundary"], a_state["boundary_generation"]);
            assert_eq!(seed_manifest["scope"], manifest["scope"]);
            assert_eq!(seed_manifest["boundary"], manifest["boundary"]);
            assert_eq!(a_state["first"]["attempt"], seed_report.owner.attempt_id,
                "the independently identified seed response establishes A before the fresh high report");
            assert_eq!(a_state["first"]["input_tokens"], 100);
            if index == 1 {
                assert_eq!(a_state["last_reset_origin"], finished_tokens[0],
                    "the prior exact original GC FINISH causes this report's new boundary");
            }
            let tool_id = if index == 0 { "grow-first" } else { "grow-6" };
            ordinary.reply.send((200, json!({
                "id":format!("{tool_id}-owner"), "status":"completed",
                "usage":{"input_tokens":input_tokens,"output_tokens":1},
                "output":[{"type":"function_call","call_id":tool_id,"name":"nope","arguments":"{}"}]
            }))).unwrap();

            let turns = [turn_id.clone()];
            let finish_wait = automatic_attempts_finished_for_turns(&control, &turns, 1);
            let settle = settled(&control, &target);
            tokio::pin!(finish_wait);
            tokio::pin!(settle);
            let mut finished = None;
            let mut is_settled = false;
            let mut final_sent = false;
            let mut post_count = 0;
            while finished.is_none() || !is_settled {
                tokio::select! {
                    terminal = &mut finish_wait, if finished.is_none() => finished = Some(terminal),
                    _ = &mut settle, if !is_settled => is_settled = true,
                    incoming = receive.recv() => {
                        let incoming = incoming.expect("same-Thread provider remains available through exact report FINISH");
                        match incoming.kind() {
                            ProviderRequestKind::Summary => {
                                post_count += 1;
                                captures.push(incoming.body["input"].clone());
                                incoming.reply.send(failure.clone()).unwrap();
                            }
                            ProviderRequestKind::Ordinary => {
                                assert!(!final_sent, "one source-declared context-limit boundary per completed range");
                                final_sent = true;
                                incoming.reply.send((413, json!({"error":{
                                    "code":"context_length_exceeded","message":"join the exact active range before the next Run"
                                }}))).unwrap();
                            }
                        }
                    }
                }
            }
            assert!(final_sent, "the current ordinary Run reached its real context-limit join boundary");
            assert_eq!(post_count, 1, "one real HTTP500 POST per source-prescribed range");
            let terminal = finished.unwrap();
            assert_eq!(terminal.len(), 1);
            assert_eq!(terminal[0].id, report.owner.attempt_id);
            let record = automatic_attempt_record(&control, report.owner.attempt_id.clone()).await;
            assert_eq!(record["trigger"]["input_tokens"], input_tokens);
            assert_eq!(record["state"], "failed");
            assert_eq!(record["compaction_outcome"]["outcome"]["reason"], "provider_error");
            report_owners.push(report.owner);
            finished_tokens.push(terminal[0].token.clone());
            let run = accepted.run_id.expect("same saved report Run for cleanup");
            request(&control, Command::RunStop(RunTarget { run_id: run.clone() })).await;
            assert_eq!(request(&control, Command::RunGet(RunTarget { run_id: run })).await["state"], "stopped");
        }

        assert_eq!(report_owners.iter().map(|report| &report.attempt_id).collect::<std::collections::HashSet<_>>().len(), 2,
            "the two provider failures belong to independent actual model attempts");
        assert_ne!(report_owners[0].row_turn_id, report_owners[1].row_turn_id,
            "each range is a separately completed ordinary Turn, not an assumed within-Turn schedule");
        assert_ne!(captures[0], captures[1], "the two real provider R bodies are independently changed");
        let thread = target.clone();
        let (spills, shell_executions): (usize, i64) = control.store.database.call(move |db| {
            let spills = proactive_wait_witness(db, &thread)?.0.len();
            let calls: i64 = db.query_row("SELECT count(*) FROM shell_execution", [], |row| row.get(0))?;
            Ok((spills, calls))
        }).await.unwrap();
        assert_eq!(spills, 0, "provider error ranges authorize no spill");
        assert_eq!(shell_executions, 0, "provider error ranges execute no shell tools");
        control.shutdown().await.unwrap(); owner.shutdown().await.unwrap(); server.abort();
    }).await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_restart_runs_one_overdue_conditional_cli_message_then_advances() {
    crate::test_watchdog::watchdog("overdue schedule delivers through the ready owner", async {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let config = root.path().join("config.toml");
        let (url, mut requests, proxy_task) = proxy().await;
        std::fs::write(&config, format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
        let seed = HeadlessOwner::start(&data, &config).await;
        let thread = headless_cli(&data, &["thread", "create"]).await["thread_id"].as_str().unwrap().to_owned();
        headless_cli(&data, &["shutdown"]).await;
        seed.finish(true, &root.path().join("seed.jsonl")).await;

        std::fs::write(root.path().join("needs-review"), "").unwrap();
        let cli = serve_quote(std::path::Path::new(env!("CARGO_BIN_EXE_shellboy")));
        let text = "Review is needed from the overdue schedule";
        let command = if cfg!(windows) {
            format!("if (Test-Path needs-review) {{ & {cli} message send --thread \"$env:SHELLBOY_THREAD_ID\" --text '{text}'; exit $LASTEXITCODE }}")
        } else {
            format!("if test -f needs-review; then {cli} message send --thread \"$SHELLBOY_THREAD_ID\" --text '{text}'; fi")
        };
        {
            let db = rusqlite::Connection::open(data.join("shellboy.db")).unwrap();
            db.execute("INSERT INTO schedule(id,thread_id,cron,next_at,enabled,cwd,command) VALUES('overdue',?1,'0 12 1 1 *',1,1,?2,?3)",rusqlite::params![thread,root.path().to_str().unwrap(),command]).unwrap();
        }
        let owner = HeadlessOwner::start(&data, &config).await;
        let request = requests.recv().await.unwrap();
        assert!(request.body["input"].to_string().contains(text));
        request.reply.send((200, answer("Scheduled message received"))).unwrap();
        loop {
            let list = headless_cli(&data, &["schedule", "list", "--thread", &thread]).await;
            let saved = &list["items"][0]["latest"];
            if saved["state"] == "exited" {
                assert_eq!(saved["exit_code"], 0, "{list}");
                break;
            }
            assert!(!matches!(saved["state"].as_str(), Some("failed" | "stopped")), "{list}");
            tokio::task::yield_now().await;
        }
        headless_cli(&data, &["shutdown"]).await;
        owner.finish(true, &root.path().join("restart.jsonl")).await;
        let db = headless_db(&data);
        let (count, next) = db.query_row("SELECT count(x.id),q.next_at FROM schedule q LEFT JOIN shell_execution x ON x.schedule_id=q.id WHERE q.id='overdue' GROUP BY q.id",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).unwrap();
        assert_eq!(count, 1);
        assert!(next > chrono::Utc::now().timestamp());
        proxy_task.abort();
    }).await.unwrap();
}
