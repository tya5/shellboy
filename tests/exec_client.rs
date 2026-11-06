//! Native CLI orchestration through a real owner and synthetic loopback Responses.
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    control::{Control, EngineEvent},
    ipc::LocalServer,
    protocol::*,
};
use std::{
    path::Path,
    process::{Output, Stdio},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

struct Incoming {
    body: Value,
    reply: oneshot::Sender<String>,
}
struct Fixture {
    root: tempfile::TempDir,
    owner: DataOwner,
    control: Arc<Control>,
    server: LocalServer,
    incoming: mpsc::Receiver<Incoming>,
    stop: CancellationToken,
    mock: JoinHandle<()>,
}
async fn read_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 8192];
        let n = stream.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
        assert!(bytes.len() < 4 * 1024 * 1024);
        if let Some(split) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..split]);
            let length = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .and_then(|v| v.parse::<usize>().ok())
                })
                .unwrap();
            if bytes.len() >= split + 4 + length {
                return serde_json::from_slice(&bytes[split + 4..split + 4 + length]).unwrap();
            }
        }
    }
}
fn http(status: u16, body: Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn answer(text: &str) -> String {
    http(
        200,
        json!({"id":"exec-fixture","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}),
    )
}
impl Fixture {
    async fn new() -> Self {
        Self::with_final_read_rejection(None).await
    }
    // false: reject the preflight as an old owner; true: lose support after send.
    async fn with_final_read_rejection(after_send: Option<bool>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let (sender, incoming) = mpsc::channel(16);
        let mock = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _=token.cancelled()=>break,
                    accepted=listener.accept()=>{
                        let(mut stream,_)=accepted.unwrap();let sender=sender.clone();let stop=token.clone();
                        connections.spawn(async move {let body=read_request(&mut stream).await;let(reply,received)=oneshot::channel();if sender.send(Incoming{body,reply}).await.is_err(){return;}tokio::select!{_=stop.cancelled()=>{},response=received=>{if let Ok(response)=response {let _=stream.write_all(response.as_bytes()).await;}}}});
                    }
                    result=connections.join_next(),if !connections.is_empty()=>{result.unwrap().unwrap();}
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        let config = root.path().join("config.toml");
        std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='http://{address}/v1'\nresponse_header_timeout_ms=60000\nstream_idle_timeout_ms=60000\nresponse_max_retries=0\n")).unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, mut receive) = mpsc::channel::<shellboy::ipc::Invocation>(16);
        let server = LocalServer::bind(&owner, rpc).await.unwrap();
        let (forward, requests) = mpsc::channel(16);
        tokio::spawn(control.clone().serve(requests));
        tokio::spawn(async move {
            let mut sent = false;
            while let Some(invocation) = receive.recv().await {
                if matches!(&invocation.request.command, Command::MessageSend(_)) {
                    sent = true;
                }
                let reject = after_send.is_some_and(|late| !late || sent)
                    && matches!(&invocation.request.command,Command::HistoryGet(args)if args.end_message_only);
                if reject {
                    let _ = invocation.reply.send(Response::failure(
                        invocation.request.operation_id.clone(),
                        ApiError::new(
                            ErrorCode::InvalidArgument,
                            "unknown field `end_message_only` (synthetic old owner)",
                        ),
                    ));
                } else if forward.send(invocation).await.is_err() {
                    break;
                }
            }
        });
        Self {
            root,
            owner,
            control,
            server,
            incoming,
            stop,
            mock,
        }
    }
    fn schema(&self) -> std::path::PathBuf {
        let path = self.root.path().join("schema.json");
        std::fs::write(&path,br#"{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}"#).unwrap();
        path
    }
    fn command(&self, schema: &Path) -> tokio::process::Command {
        self.command_mode(schema, "turn-end")
    }
    fn command_mode(&self, schema: &Path, mode: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"));
        command
            .arg("--data-dir")
            .arg(self.root.path().join("data"))
            .arg("exec")
            .arg("--result")
            .arg(mode)
            .arg("--schema")
            .arg(schema)
            .current_dir(self.root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
    async fn next(&mut self, child: &mut JoinHandle<Output>) -> Incoming {
        crate::test_watchdog::watchdog("tests/exec_client.rs: next",async{tokio::select!{request=self.incoming.recv()=>request.expect("mock receiver"),out=&mut *child=>panic!("exec exited before its request: {:?}",out.unwrap())}}).await.unwrap()
    }
    async fn close(self) {
        crate::test_watchdog::watchdog("tests/exec_client.rs: close", self.server.shutdown())
            .await
            .unwrap()
            .unwrap();
        crate::test_watchdog::watchdog("tests/exec_client.rs: close", self.control.shutdown())
            .await
            .unwrap()
            .unwrap();
        crate::test_watchdog::watchdog("tests/exec_client.rs: close", self.owner.shutdown())
            .await
            .unwrap()
            .unwrap();
        self.stop.cancel();
        crate::test_watchdog::watchdog("tests/exec_client.rs: close", self.mock)
            .await
            .unwrap()
            .unwrap();
    }
    async fn active(&mut self) -> (MessageAccepted, Incoming) {
        let thread = self
            .control
            .request(Request::new(
                format!("{}:{}", self.owner.instance_id, uuid::Uuid::new_v4()),
                Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    cwd: None,
                    model: None,
                    effort: None,
                    parent_id: None,
                }),
            ))
            .await;
        assert!(thread.ok, "{:?}", thread.error);
        let id = thread.result.unwrap()["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let accepted = self
            .control
            .request(Request::new(
                format!("{}:{}", self.owner.instance_id, uuid::Uuid::new_v4()),
                Command::MessageSend(MessageSendArgs {
                    human: false,
                    expect_reply: false,
                    sender_label: None,
                    deliver: Deliver::Next,
                    thread_id: Some(id),
                    text: "initial work".into(),
                }),
            ))
            .await;
        assert!(accepted.ok, "{:?}", accepted.error);
        let accepted = serde_json::from_value(accepted.result.unwrap()).unwrap();
        let request =
            crate::test_watchdog::watchdog("tests/exec_client.rs: active", self.incoming.recv())
                .await
                .unwrap()
                .unwrap();
        (accepted, request)
    }
    async fn queued_chat(&self, thread: &str) {
        let target = thread.to_owned();
        crate::test_watchdog::watchdog("tests/exec_client.rs: queued_chat", async {
            loop {
                let id = target.clone();
                let count = self.owner.db.call(move |db| Ok(db.query_row("SELECT count(*) FROM receipt WHERE target_thread_id=?1 AND json_extract(sender_json,'$.kind')='external_cli'", [id], |r| r.get::<_, i64>(0))?)).await.unwrap();
                if count > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn queued_exec(&self, thread: &str) {
        let mut events = self.control.subscribe();
        let target = thread.to_owned();
        crate::test_watchdog::watchdog("tests/exec_client.rs: queued_exec",async {loop {let id=target.clone();let count=self.owner.db.call(move|db|Ok(db.query_row("SELECT count(*) FROM receipt WHERE target_thread_id=?1 AND json_extract(sender_json,'$.label')='shellboy exec'",[id],|r|r.get::<_,i64>(0))?)).await.unwrap();if count>0{break;}loop {match events.recv().await {Ok(EngineEvent::Changed{thread_id,..})if thread_id==target=>break,Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>break,Err(e)=>panic!("{e}"),_=>{}}}}}).await.unwrap();
    }
}
fn spawn(mut command: tokio::process::Command) -> JoinHandle<Output> {
    tokio::spawn(async move {
        crate::test_watchdog::watchdog(
            "tests/exec_client.rs: spawn",
            command.spawn().unwrap().wait_with_output(),
        )
        .await
        .unwrap()
        .unwrap()
    })
}
fn diagnostic(output: &Output) -> Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!(
            "invalid stderr JSON: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_stdout_is_only_validated_utf8_final_json_and_sender_is_recorded() {
    let mut fixture = Fixture::new().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("--text-stdin").stdin(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("work \u{65e5}\u{672c}".as_bytes())
        .await
        .unwrap();
    let mut child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    let incoming = fixture.next(&mut child).await;
    assert!(incoming.body.to_string().contains("Final answer contract"));
    assert!(incoming.body.to_string().contains("work"));
    let value = json!({"value":format!("{}\u{65e5}\u{672c}\u{8a9e}","x".repeat(70_000))});
    incoming.reply.send(http(200,json!({"id":"with-reasoning","status":"completed","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"This is reasoning, not the JSON answer."}]},{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":value.to_string()}]}]}))).unwrap();
    let output = crate::test_watchdog::watchdog("tests/exec_client.rs: native_stdout_is_only_validated_utf8_final_json_and_sender_is_recorded", child).await.unwrap().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        value
    );
    assert!(!output.stdout.starts_with(&[0xef, 0xbb, 0xbf]));
    let diag = diagnostic(&output);
    assert_eq!(diag["repairs"], 0);
    assert!(
        output.stdout.iter().any(|byte| *byte >= 128),
        "the result must exercise real UTF-8 bytes"
    );
    assert_eq!(diag["turn_usage"].as_array().unwrap().len(), 1);
    assert_eq!(diag["turn_usage"][0]["turn_id"], diag["turn_id"]);
    let original = crate::test_watchdog::watchdog("tests/exec_client.rs: native_stdout_is_only_validated_utf8_final_json_and_sender_is_recorded",
        tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(fixture.root.path().join("data"))
            .args(["history", "get", "--node"])
            .arg(diag["end_message_node_id"].as_str().unwrap())
            .arg("--end-message-only")
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(original.status.success(), "{original:?}");
    let original: Value = serde_json::from_slice(&original.stdout).unwrap();
    assert!(
        value
            .to_string()
            .starts_with(original["result"]["text"].as_str().unwrap())
    );
    assert!(
        original["result"]["next_cursor"].is_string(),
        "ordinary CLI history reads remain paged"
    );
    let thread = diag["thread_id"].as_str().unwrap().to_owned();
    fixture
        .owner
        .db
        .call(move |db| {
            let sender: String = db.query_row(
                "SELECT sender_json FROM receipt WHERE target_thread_id=?1",
                [thread],
                |r| r.get(0),
            )?;
            let sender: Value = serde_json::from_str(&sender).unwrap();
            assert_eq!(sender["kind"], "external_cli");
            assert_eq!(sender["label"], "shellboy exec");
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounded_repairs_stay_in_history_and_report_raw_answer_on_schema_failure() {
    let mut fixture = Fixture::new().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("--max-repairs").arg("2").arg("report a value");
    let mut child = spawn(command);
    for n in 0..3 {
        let incoming = fixture.next(&mut child).await;
        if n > 0 {
            assert!(incoming.body.to_string().contains("Correct ONLY"));
            assert!(
                incoming
                    .body
                    .to_string()
                    .contains("do not repeat completed side effects")
            );
        }
        incoming
            .reply
            .send(answer(if n == 0 {
                "not JSON"
            } else {
                "{\"value\":123}"
            }))
            .unwrap();
    }
    let output = crate::test_watchdog::watchdog("tests/exec_client.rs: bounded_repairs_stay_in_history_and_report_raw_answer_on_schema_failure", child).await.unwrap().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let diag = diagnostic(&output);
    assert_eq!(diag["repairs"], 2);
    assert_eq!(diag["raw_answer"], "{\"value\":123}");
    assert!(!diag["validation_errors"].as_array().unwrap().is_empty());
    let thread = diag["thread_id"].as_str().unwrap().to_owned();
    fixture
        .owner
        .db
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM receipt WHERE target_thread_id=?1",
                    [thread],
                    |r| r.get::<_, i64>(0)
                )?,
                3
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_corrective_message_can_produce_a_valid_result() {
    let mut fixture = Fixture::new().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("report value");
    let mut child = spawn(command);
    fixture
        .next(&mut child)
        .await
        .reply
        .send(answer("{}"))
        .unwrap();
    fixture
        .next(&mut child)
        .await
        .reply
        .send(answer("{\"value\":\"corrected\"}"))
        .unwrap();
    let output = crate::test_watchdog::watchdog(
        "tests/exec_client.rs: one_corrective_message_can_produce_a_valid_result",
        child,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"value":"corrected"})
    );
    assert_eq!(diagnostic(&output)["repairs"], 1);
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_wait_is_not_an_implicit_retry_or_success() {
    let mut fixture = Fixture::new().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("report value");
    let mut child = spawn(command);
    fixture
        .next(&mut child)
        .await
        .reply
        .send(http(
            400,
            json!({"error":{"message":"synthetic permanent provider failure"}}),
        ))
        .unwrap();
    let output = crate::test_watchdog::watchdog(
        "tests/exec_client.rs: recovery_wait_is_not_an_implicit_retry_or_success",
        child,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let diag = diagnostic(&output);
    assert_eq!(diag["status"], "recovery_wait");
    assert!(diag["run_id"].is_string());
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_leaves_the_existing_run_and_queued_input_alive() {
    let mut fixture = Fixture::new().await;
    let (accepted, held) = fixture.active().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command
        .arg("--thread")
        .arg(&accepted.thread_id)
        .arg("--timeout-ms")
        .arg("5000")
        .arg("queued work");
    let child = spawn(command);
    fixture.queued_exec(&accepted.thread_id).await;
    let output = crate::test_watchdog::watchdog(
        "tests/exec_client.rs: timeout_leaves_the_existing_run_and_queued_input_alive",
        child,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stdout.is_empty());
    let diag = diagnostic(&output);
    assert_eq!(diag["thread_id"], accepted.thread_id);
    assert!(diag["operation_id"].is_string());
    let run = accepted.run_id.unwrap();
    fixture
        .owner
        .db
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT state FROM run WHERE id=?1", [run], |r| r
                    .get::<_, String>(0))?,
                "live"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    drop(held);
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_added_to_an_active_turn_waits_for_its_real_answer() {
    let mut fixture = Fixture::new().await;
    let (accepted, first) = fixture.active().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command
        .arg("--thread")
        .arg(&accepted.thread_id)
        .arg("report appended value");
    let mut child = spawn(command);
    fixture.queued_exec(&accepted.thread_id).await;
    first.reply.send(http(200,json!({"id":"tool-boundary","status":"completed","output":[{"type":"function_call","call_id":"fixture-call","name":"shell","arguments":json!({"command":"echo exec-fixture"}).to_string()}]}))).unwrap();
    let next = fixture.next(&mut child).await;
    assert!(next.body.to_string().contains("report appended value"));
    next.reply.send(answer("{\"value\":\"appended\"}")).unwrap();
    let output = crate::test_watchdog::watchdog(
        "tests/exec_client.rs: input_added_to_an_active_turn_waits_for_its_real_answer",
        child,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"value":"appended"})
    );
    assert_eq!(diagnostic(&output)["run_id"], accepted.run_id.unwrap());
    fixture.close().await;
}
#[tokio::test]
async fn absent_owner_and_unsupported_schema_have_distinct_stderr_only_failures() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("no-owner");
    let schema = root.path().join("schema.json");
    std::fs::write(&schema, br#"{"type":"object"}"#).unwrap();
    async fn invoke(data: &Path, schema: &Path, extra: Option<&str>) -> Output {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"));
        command
            .arg("--data-dir")
            .arg(data)
            .arg("exec")
            .arg("--result")
            .arg("turn-end")
            .arg("--schema")
            .arg(schema)
            .arg("work");
        if let Some(extra) = extra {
            command.arg(extra);
        }
        crate::test_watchdog::watchdog("tests/exec_client.rs: invoke", command.output())
            .await
            .unwrap()
            .unwrap()
    }
    let output = invoke(&data, &schema, None).await;
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    assert_eq!(diagnostic(&output)["status"], "owner_error");
    assert!(!data.exists());
    std::fs::write(&schema, br#"{"$ref":"https://example.invalid/no-network"}"#).unwrap();
    let output = invoke(&data, &schema, None).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(diagnostic(&output)["status"], "input_error");
    assert!(!data.exists());
    let output = invoke(&data, &schema, Some("--not-an-exec-flag")).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_thread_is_input_error_not_a_provider_call() {
    let fixture = Fixture::new().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.args(["--thread", "does-not-exist", "report value"]);
    let output = crate::test_watchdog::watchdog(
        "tests/exec_client.rs: missing_thread_is_input_error_not_a_provider_call",
        command.output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(diagnostic(&output)["status"], "input_error");
    fixture
        .owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validated_numeric_json_spelling_is_not_rounded_by_the_client() {
    let mut fixture = Fixture::new().await;
    let schema = fixture.schema();
    std::fs::write(&schema, br#"{"type":"number"}"#).unwrap();
    let mut command = fixture.command(&schema);
    command.arg("report a numeric value");
    let mut child = spawn(command);
    fixture
        .next(&mut child)
        .await
        .reply
        .send(answer("9007199254740993.0"))
        .unwrap();
    let output = crate::test_watchdog::watchdog(
        "tests/exec_client.rs: validated_numeric_json_spelling_is_not_rounded_by_the_client",
        child,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "9007199254740993.0\n"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_owner_is_rejected_before_creating_or_sending_work() {
    let fixture = Fixture::with_final_read_rejection(Some(false)).await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("do not execute on an incompatible owner");
    let output = crate::test_watchdog::watchdog(
        "tests/exec_client.rs: old_owner_is_rejected_before_creating_or_sending_work",
        command.output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    let diagnostic = diagnostic(&output);
    assert_eq!(diagnostic["status"], "owner_version_mismatch");
    assert_eq!(diagnostic["stage"], "capability");
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("history get")
    );
    fixture
        .owner
        .db
        .call(|db| {
            for table in ["thread", "receipt", "model_attempt"] {
                assert_eq!(
                    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn final_read_refusal_after_work_is_owner_error_with_answer_ids_and_no_resend() {
    let mut fixture = Fixture::with_final_read_rejection(Some(true)).await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("execute once and retain the answer");
    let mut child = spawn(command);
    fixture
        .next(&mut child)
        .await
        .reply
        .send(answer("{\"value\":\"already done\"}"))
        .unwrap();
    let output = crate::test_watchdog::watchdog("tests/exec_client.rs: final_read_refusal_after_work_is_owner_error_with_answer_ids_and_no_resend", child).await.unwrap().unwrap();
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    let diagnostic = diagnostic(&output);
    assert_eq!(diagnostic["status"], "owner_version_mismatch");
    assert_eq!(diagnostic["stage"], "answer");
    assert_eq!(diagnostic["repairs"], 0);
    for key in [
        "thread_id",
        "message_id",
        "run_id",
        "turn_id",
        "end_message_node_id",
    ] {
        assert!(diagnostic[key].is_string(), "{diagnostic}");
    }
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("do not resend")
    );
    let original = crate::test_watchdog::watchdog("tests/exec_client.rs: final_read_refusal_after_work_is_owner_error_with_answer_ids_and_no_resend",
        tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(fixture.root.path().join("data"))
            .args(["history", "get", "--node"])
            .arg(diagnostic["end_message_node_id"].as_str().unwrap())
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(original.status.success());
    let original: Value = serde_json::from_slice(&original.stdout).unwrap();
    assert_eq!(original["result"]["text"], "{\"value\":\"already done\"}");
    fixture
        .owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_end_message_succeeds_even_with_schema() {
    let mut fixture = Fixture::new().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("work");
    let child = command.spawn().unwrap();
    let mut child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    let incoming = fixture.next(&mut child).await;
    incoming.reply.send(http(200, json!({"id":"empty", "status":"completed", "output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"internal"}]}]}))).unwrap();
    let output = crate::test_watchdog::watchdog("empty end message", child)
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"\n");
    assert_eq!(diagnostic(&output)["repairs"], 0);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_end_returns_assigned_runs_last_end_message() {
    let mut fixture = Fixture::new().await;
    let schema = fixture.schema();
    let mut command = fixture.command_mode(&schema, "run-end");
    command.arg("work");
    let mut child = spawn(command);
    let incoming = fixture.next(&mut child).await;
    incoming.reply.send(answer("{\"value\":\"last\"}")).unwrap();
    let output = crate::test_watchdog::watchdog("run end result", child)
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"value":"last"})
    );
    assert!(diagnostic(&output)["run_id"].is_string());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_end_selects_last_turn_after_processing_turn() {
    let mut fixture = Fixture::new().await;
    let (accepted, first) = fixture.active().await;
    let schema = fixture.schema();
    let mut command = fixture.command_mode(&schema, "run-end");
    command.arg("--thread").arg(&accepted.thread_id).arg("work");
    let mut child = spawn(command);
    fixture.queued_exec(&accepted.thread_id).await;
    first.reply.send(http(200,json!({"id":"boundary","status":"completed","output":[{"type":"function_call","call_id":"fixture-call","name":"shell","arguments":json!({"command":"echo fixture"}).to_string()}]}))).unwrap();
    let processing = fixture.next(&mut child).await;
    let response = fixture
        .control
        .request(Request::new(
            format!("{}:{}", fixture.owner.instance_id, uuid::Uuid::new_v4()),
            Command::MessageSend(MessageSendArgs {
                human: false,
                expect_reply: false,
                thread_id: Some(accepted.thread_id.clone()),
                text: "later turn".into(),
                deliver: Deliver::Complete,
                sender_label: None,
            }),
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    processing
        .reply
        .send(answer("{\"value\":\"processing\"}"))
        .unwrap();
    let last = fixture.next(&mut child).await;
    last.reply.send(answer("{\"value\":\"last\"}")).unwrap();
    let output = crate::test_watchdog::watchdog("multi-turn run end", child)
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"value":"last"})
    );
    assert_eq!(diagnostic(&output)["turn_id"], accepted.turn_id.unwrap());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn esc_ended_processing_turn_exits_four() {
    let mut fixture = Fixture::new().await;
    let (accepted, first) = fixture.active().await;
    let schema = fixture.schema();
    let mut command = fixture.command(&schema);
    command.arg("--thread").arg(&accepted.thread_id).arg("work");
    let mut child = spawn(command);
    fixture.queued_exec(&accepted.thread_id).await;
    first.reply.send(http(200,json!({"id":"boundary","status":"completed","output":[{"type":"function_call","call_id":"fixture-call","name":"shell","arguments":json!({"command":"echo fixture"}).to_string()}]}))).unwrap();
    let pending = fixture.next(&mut child).await;
    fixture
        .control
        .interrupt_turn(accepted.thread_id.clone(), accepted.turn_id.unwrap())
        .await
        .unwrap();
    drop(pending);
    let output = crate::test_watchdog::watchdog("esc turn result", child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert_eq!(diagnostic(&output)["status"], "execution_unavailable");
    assert!(output.stdout.is_empty());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reply_mode_returns_explicit_reply_not_end_message() {
    let mut fixture = Fixture::new().await;
    let (accepted, first) = fixture.active().await;
    let schema = fixture.schema();
    let mut command = fixture.command_mode(&schema, "reply");
    command.arg("--thread").arg(&accepted.thread_id).arg("work");
    let mut child = spawn(command);
    fixture.queued_exec(&accepted.thread_id).await;
    first.reply.send(http(200,json!({"id":"boundary","status":"completed","output":[{"type":"function_call","call_id":"fixture-call","name":"shell","arguments":json!({"command":"echo fixture"}).to_string()}]}))).unwrap();
    let processing = fixture.next(&mut child).await;
    let thread = accepted.thread_id.clone();
    let id: String = fixture.owner.db.call(move |db| Ok(db.query_row("SELECT message_node_id FROM receipt WHERE target_thread_id=?1 AND json_extract(sender_json,'$.label')='shellboy exec' ",[thread],|r|r.get(0))?)).await.unwrap();
    let admitted = id.clone();
    fixture.owner.db.call(move |db| { db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&admitted])?; Ok(()) }).await.unwrap();
    let reply = fixture
        .control
        .request(Request::new(
            format!("{}:{}", fixture.owner.instance_id, uuid::Uuid::new_v4()),
            Command::MessageReply(MessageReplyArgs {
                message_id: id,
                text: "{\"value\":\"reply\"}".into(),
                sender_label: None,
            }),
        ))
        .await;
    assert!(reply.ok, "{:?}", reply.error);
    processing
        .reply
        .send(answer("{\"value\":\"end\"}"))
        .unwrap();
    let output = crate::test_watchdog::watchdog("reply result", child)
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"value":"reply"})
    );
    fixture.close().await;
}

// ---- shellboy chat: thin client over the same owner RPCs ----

fn chat_command(fixture: &Fixture, args: &[&str]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"));
    command
        .arg("--data-dir")
        .arg(fixture.root.path().join("data"))
        .arg("chat")
        .args(args)
        .current_dir(fixture.root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}
fn tool_call(command: &str) -> String {
    http(
        200,
        json!({"id":"chat-tool","status":"completed","output":[{"type":"function_call","call_id":"chat-call","name":"shell","arguments":json!({"command":command}).to_string()}]}),
    )
}
fn json_lines(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|_| panic!("not JSON: {l}")))
        .collect()
}
async fn senders(fixture: &Fixture) -> Vec<String> {
    fixture
        .owner
        .db
        .call(|db| {
            let mut stmt = db
                .prepare("SELECT json_extract(sender_json,'$.kind') FROM receipt ORDER BY rowid")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}
fn chat_options(tty_json: bool) -> shellboy::chat_client::ChatOptions {
    shellboy::chat_client::ChatOptions {
        thread_id: None,
        create: shellboy::cli::ThreadCreateOptions {
            agents: None,
            cwd: None,
            connection: None,
            model: None,
            effort: None,
            parent_id: None,
        },
        json: tty_json,
        until: shellboy::exec_client::ResultMode::TurnEnd,
        timeout_ms: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_piped_json_session_with_tool_progress_and_multiline_input() {
    let mut fixture = Fixture::new().await;
    let mut command = chat_command(&fixture, &["--json"]);
    let mut child = command.spawn().unwrap();
    // Message 1 is one line; message 2 continues over a trailing backslash; /exit ignores the rest.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"first question\nsecond \\\nline two\n/exit\nignored\n")
        .await
        .unwrap();
    let mut child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    let first = fixture.next(&mut child).await;
    assert!(first.body.to_string().contains("first question"));
    first
        .reply
        .send(tool_call("echo chat-tool-output"))
        .unwrap();
    let after_tool = fixture.next(&mut child).await;
    after_tool.reply.send(answer("answer one")).unwrap();
    let second = fixture.next(&mut child).await;
    assert!(
        second.body.to_string().contains("second \\nline two"),
        "{}",
        second.body
    );
    second.reply.send(answer("answer two")).unwrap();
    let output = crate::test_watchdog::watchdog("chat json", child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let lines = json_lines(&output);
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    let pos = |t: &str| {
        types
            .iter()
            .position(|x| *x == t)
            .unwrap_or_else(|| panic!("{t} missing in {types:?}"))
    };
    assert!(pos("thread") < pos("message_accepted"));
    assert!(pos("tool_started") < pos("tool_finished"));
    assert!(pos("tool_finished") < pos("end_message"));
    let ends: Vec<&Value> = lines
        .iter()
        .filter(|l| l["type"] == "end_message")
        .collect();
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0]["text"], "answer one");
    assert_eq!(ends[1]["text"], "answer two");
    let finished = lines.iter().find(|l| l["type"] == "tool_finished").unwrap();
    assert_eq!(finished["exit_code"], 0);
    assert!(
        finished["summary"]
            .as_str()
            .unwrap()
            .contains("echo chat-tool-output")
    );
    for line in &lines {
        for key in [
            "thread_id",
            "run_id",
            "turn_id",
            "message_id",
            "execution_id",
            "node_id",
            "state",
            "summary",
            "exit_code",
            "at",
            "text",
        ] {
            assert!(line.get(key).is_some(), "{key} missing in {line}");
        }
    }
    assert_eq!(ends[0]["thread_id"], lines[0]["thread_id"]);
    assert!(ends[0]["node_id"].is_string() && ends[0]["turn_id"].is_string());
    // Piped stdin is not a terminal: external_cli.
    assert_eq!(
        senders(&fixture).await,
        vec!["external_cli", "external_cli"]
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_terminal_input_records_human_and_prints_progress_and_plain_end_message() {
    let mut fixture = Fixture::new().await;
    let (_signal, interrupts) = mpsc::channel(4);
    let data = fixture.root.path().join("data");
    let cwd = fixture.root.path().to_path_buf();
    let task = tokio::spawn(async move {
        let mut out: Vec<u8> = Vec::new();
        let result = shellboy::chat_client::run(
            chat_options(false),
            &data,
            &cwd,
            None,
            tokio::io::BufReader::new(&b"hello\n"[..]),
            &mut out,
            shellboy::chat_client::ChatEnv {
                stdin_is_tty: true,
                interrupts,
            },
        )
        .await;
        (result, out)
    });
    let first = crate::test_watchdog::watchdog("chat human first", fixture.incoming.recv())
        .await
        .unwrap()
        .unwrap();
    first.reply.send(tool_call("echo human-tool")).unwrap();
    let second = crate::test_watchdog::watchdog("chat human second", fixture.incoming.recv())
        .await
        .unwrap()
        .unwrap();
    second
        .reply
        .send(answer("**plain** reply\x1b[31m"))
        .unwrap();
    let (result, out) = crate::test_watchdog::watchdog("chat human", task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap(), 0);
    let out = String::from_utf8(out).unwrap();
    println!("--- sample chat session (terminal) ---\n{out}--- end ---");
    assert!(out.starts_with("> "), "{out}");
    assert!(out.contains("[thread] "));
    assert!(out.contains("[turn] completed"));
    assert!(out.contains("[tool] echo human-tool"), "{out}");
    assert!(out.contains("[tool] exited, exit 0"), "{out}");
    assert!(
        out.contains("**plain** reply"),
        "Markdown stays unrendered: {out}"
    );
    assert!(!out.contains('\x1b'), "no escape sequences are printed");
    assert_eq!(senders(&fixture).await, vec!["human"]);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_ctrl_c_stops_the_run_then_second_ctrl_c_leaves() {
    let mut fixture = Fixture::new().await;
    let (signal, interrupts) = mpsc::channel(4);
    let data = fixture.root.path().join("data");
    let cwd = fixture.root.path().to_path_buf();
    let task = tokio::spawn(async move {
        let mut out: Vec<u8> = Vec::new();
        let result = shellboy::chat_client::run(
            chat_options(false),
            &data,
            &cwd,
            None,
            tokio::io::BufReader::new(&b"long work\n"[..]),
            &mut out,
            shellboy::chat_client::ChatEnv {
                stdin_is_tty: false,
                interrupts,
            },
        )
        .await;
        (result, out)
    });
    // The model request stays unanswered, so the Run is live.
    let pending = crate::test_watchdog::watchdog("chat ctrl-c request", fixture.incoming.recv())
        .await
        .unwrap()
        .unwrap();
    signal.send(()).await.unwrap();
    let (result, out) = crate::test_watchdog::watchdog("chat ctrl-c", task)
        .await
        .unwrap()
        .unwrap();
    let out = String::from_utf8(out).unwrap();
    println!("--- sample chat session (Ctrl+C) ---\n{out}--- end ---");
    assert_eq!(
        result.unwrap(),
        4,
        "a stopped Run is an abnormal end: {out}"
    );
    assert!(out.contains("[stop] stopping the Run"), "{out}");
    assert!(
        out.contains("ended as stopped") || out.contains("[turn] stopped"),
        "{out}"
    );
    drop(pending);
    // Ctrl+C at the prompt leaves chat without sending anything.
    let (signal, interrupts) = mpsc::channel(4);
    let data = fixture.root.path().join("data");
    let cwd = fixture.root.path().to_path_buf();
    signal.send(()).await.unwrap();
    signal.send(()).await.unwrap();
    let (result, out) = crate::test_watchdog::watchdog("chat double ctrl-c", async move {
        let mut out: Vec<u8> = Vec::new();
        let result = shellboy::chat_client::run(
            chat_options(false),
            &data,
            &cwd,
            None,
            tokio::io::BufReader::new(&b"more work\n"[..]),
            &mut out,
            shellboy::chat_client::ChatEnv {
                stdin_is_tty: false,
                interrupts,
            },
        )
        .await;
        (result, out)
    })
    .await
    .unwrap();
    // Interrupts queued before any prompt read exit the client at the prompt.
    assert_eq!(result.unwrap(), 0);
    assert!(
        out.is_empty(),
        "an interrupt at the prompt leaves before sending"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_timeout_exits_five_without_stopping_work() {
    let mut fixture = Fixture::new().await;
    let mut command = chat_command(&fixture, &["--json", "--timeout-ms", "500"]);
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"slow work\n")
        .await
        .unwrap();
    let mut child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    let pending = fixture.next(&mut child).await;
    let output = crate::test_watchdog::watchdog("chat timeout", child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    let lines = json_lines(&output);
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "notice" && l["state"] == "timeout")
    );
    drop(pending);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_run_end_mode_waits_for_the_run_and_reports_abnormal_end_as_four() {
    let mut fixture = Fixture::new().await;
    let mut command = chat_command(&fixture, &["--json", "--until", "run-end"]);
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"work\n")
        .await
        .unwrap();
    let mut child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    let first = fixture.next(&mut child).await;
    first.reply.send(answer("run done")).unwrap();
    let output = crate::test_watchdog::watchdog("chat run-end", child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let lines = json_lines(&output);
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "end_message" && l["text"] == "run done")
    );
    // A provider failure leaves the Turn without an end message: exit 4.
    let mut command = chat_command(
        &fixture,
        &[
            "--json",
            "--thread",
            lines[0]["thread_id"].as_str().unwrap(),
        ],
    );
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"again\n")
        .await
        .unwrap();
    let mut child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    let second = fixture.next(&mut child).await;
    second
        .reply
        .send(http(
            400,
            json!({"error":{"message":"bad request","type":"invalid_request_error"}}),
        ))
        .unwrap();
    let output = crate::test_watchdog::watchdog("chat failed turn", child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(json_lines(&output).iter().any(|l| l["type"] == "notice"));
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_without_owner_exits_six_and_suggests_serve() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("data")).unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .arg("--data-dir")
        .arg(root.path().join("data"))
        .arg("chat")
        .current_dir(root.path())
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(6), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("shellboy serve"));
}

#[derive(Clone, Default)]
struct SharedOut(Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for SharedOut {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl SharedOut {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
    async fn until_contains(&self, needle: &str) {
        crate::test_watchdog::watchdog("chat output", async {
            while !self.text().contains(needle) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_run_end_provider_failure_and_recovery_wait_end_with_four_not_a_polling_loop() {
    for status in [400u16, 500] {
        let mut fixture = Fixture::new().await;
        let mut command = chat_command(&fixture, &["--json", "--until", "run-end"]);
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"work\n")
            .await
            .unwrap();
        let mut child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
        let request = fixture.next(&mut child).await;
        request
            .reply
            .send(http(
                status,
                json!({"error":{"message":"synthetic","type":"server_error"}}),
            ))
            .unwrap();
        let output = crate::test_watchdog::watchdog("chat run-end failure", child)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(4), "{status}: {output:?}");
        let lines = json_lines(&output);
        let notices: Vec<&Value> = lines.iter().filter(|l| l["type"] == "notice").collect();
        println!(
            "status {status}: {:?}",
            notices
                .iter()
                .map(|n| n["state"].clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(notices.len(), 1, "exactly one notice: {lines:?}");
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_existing_turn_message_that_is_not_applied_is_a_held_notice() {
    let mut fixture = Fixture::new().await;
    let (accepted, initial) = fixture.active().await;
    let mut command = chat_command(&fixture, &["--json", "--thread", &accepted.thread_id]);
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"added\n")
        .await
        .unwrap();
    let child = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    // Once the added message is saved, keep its receipt held while the Run proceeds; the
    // finished Turn then is not the answer to that message.
    fixture.queued_chat(&accepted.thread_id).await;
    let db = fixture.owner.db.clone();
    let thread = accepted.thread_id.clone();
    let holder = tokio::spawn(async move {
        loop {
            let thread = thread.clone();
            let _ = db
                .call(move |db| {
                    db.execute(
                        "UPDATE receipt SET state='held' WHERE target_thread_id=?1 AND json_extract(sender_json,'$.kind')='external_cli'",
                        [thread],
                    )?;
                    Ok(())
                })
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });
    initial.reply.send(answer("initial answer")).unwrap();
    let responder = tokio::spawn({
        let mut incoming = std::mem::replace(&mut fixture.incoming, mpsc::channel(1).1);
        async move {
            while let Some(request) = incoming.recv().await {
                let _ = request.reply.send(answer("later answer"));
            }
        }
    });
    let output = crate::test_watchdog::watchdog("chat held", child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let lines = json_lines(&output);
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "notice" && l["state"] == "held"),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l["type"] == "end_message"),
        "{lines:?}"
    );
    holder.abort();
    responder.abort();
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_ctrl_c_at_the_prompt_stops_a_run_left_live_by_a_timeout_then_second_leaves() {
    let mut fixture = Fixture::new().await;
    let (signal, interrupts) = mpsc::channel(4);
    let (mut feed, reader) = tokio::io::duplex(1024);
    let data = fixture.root.path().join("data");
    let cwd = fixture.root.path().to_path_buf();
    let out = SharedOut::default();
    let mut writer = out.clone();
    let mut options = chat_options(false);
    options.timeout_ms = Some(300);
    let task = tokio::spawn(async move {
        shellboy::chat_client::run(
            options,
            &data,
            &cwd,
            None,
            tokio::io::BufReader::new(reader),
            &mut writer,
            shellboy::chat_client::ChatEnv {
                stdin_is_tty: false,
                interrupts,
            },
        )
        .await
    });
    feed.write_all(b"slow\n").await.unwrap();
    let _pending = crate::test_watchdog::watchdog("chat live request", fixture.incoming.recv())
        .await
        .unwrap()
        .unwrap();
    out.until_contains("timed out").await;
    signal.send(()).await.unwrap();
    out.until_contains("[stop] stopping the Run that timed out")
        .await;
    signal.send(()).await.unwrap();
    let code = crate::test_watchdog::watchdog("chat leave", task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(code, 5);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_second_ctrl_c_during_follow_leaves_without_further_stops() {
    let mut fixture = Fixture::new().await;
    let (signal, interrupts) = mpsc::channel(4);
    let (mut feed, reader) = tokio::io::duplex(1024);
    let data = fixture.root.path().join("data");
    let cwd = fixture.root.path().to_path_buf();
    let out = SharedOut::default();
    let mut writer = out.clone();
    let task = tokio::spawn(async move {
        shellboy::chat_client::run(
            chat_options(false),
            &data,
            &cwd,
            None,
            tokio::io::BufReader::new(reader),
            &mut writer,
            shellboy::chat_client::ChatEnv {
                stdin_is_tty: false,
                interrupts,
            },
        )
        .await
    });
    feed.write_all(b"work\n").await.unwrap();
    let _pending = crate::test_watchdog::watchdog("chat live request", fixture.incoming.recv())
        .await
        .unwrap()
        .unwrap();
    signal.send(()).await.unwrap();
    signal.send(()).await.unwrap();
    let code = crate::test_watchdog::watchdog("chat double", task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = out.text();
    println!("{text}");
    assert!(text.contains("[stop] stopping the Run"));
    assert!(code == 0 || code == 4, "{code}");
    drop(feed);
    fixture.close().await;
}
