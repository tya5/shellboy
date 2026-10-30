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
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
const WATCHDOG: Duration = Duration::from_secs(90);
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
        let (rpc, receive) = mpsc::channel(16);
        let server = LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
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
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"));
        command
            .arg("--data-dir")
            .arg(self.root.path().join("data"))
            .arg("exec")
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
        timeout(WATCHDOG,async{tokio::select!{request=self.incoming.recv()=>request.expect("mock receiver"),out=&mut *child=>panic!("exec exited before its request: {:?}",out.unwrap())}}).await.unwrap()
    }
    async fn close(self) {
        timeout(WATCHDOG, self.server.shutdown())
            .await
            .unwrap()
            .unwrap();
        timeout(WATCHDOG, self.control.shutdown())
            .await
            .unwrap()
            .unwrap();
        timeout(WATCHDOG, self.owner.shutdown())
            .await
            .unwrap()
            .unwrap();
        self.stop.cancel();
        timeout(WATCHDOG, self.mock).await.unwrap().unwrap();
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
                    sender_label: None,
                    now: false,
                    thread_id: Some(id),
                    text: "initial work".into(),
                }),
            ))
            .await;
        assert!(accepted.ok, "{:?}", accepted.error);
        let accepted = serde_json::from_value(accepted.result.unwrap()).unwrap();
        let request = timeout(WATCHDOG, self.incoming.recv())
            .await
            .unwrap()
            .unwrap();
        (accepted, request)
    }
    async fn queued_exec(&self, thread: &str) {
        let mut events = self.control.subscribe();
        let target = thread.to_owned();
        timeout(WATCHDOG,async {loop {let id=target.clone();let count=self.owner.db.call(move|db|Ok(db.query_row("SELECT count(*) FROM receipt WHERE target_thread_id=?1 AND json_extract(sender_json,'$.label')='shellboy exec'",[id],|r|r.get::<_,i64>(0))?)).await.unwrap();if count>0{break;}loop {match events.recv().await {Ok(EngineEvent::Changed{thread_id,..})if thread_id==target=>break,Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>break,Err(e)=>panic!("{e}"),_=>{}}}}}).await.unwrap();
    }
}
fn spawn(mut command: tokio::process::Command) -> JoinHandle<Output> {
    tokio::spawn(async move {
        timeout(WATCHDOG, command.spawn().unwrap().wait_with_output())
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
    let output = timeout(WATCHDOG, child).await.unwrap().unwrap();
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
    let original = timeout(
        WATCHDOG,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .arg("--data-dir")
            .arg(fixture.root.path().join("data"))
            .args(["history", "get", "--node"])
            .arg(diag["answer_node_id"].as_str().unwrap())
            .arg("--final-answer-only")
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
    let output = timeout(WATCHDOG, child).await.unwrap().unwrap();
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
    let output = timeout(WATCHDOG, child).await.unwrap().unwrap();
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
    let output = timeout(WATCHDOG, child).await.unwrap().unwrap();
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
    let output = timeout(WATCHDOG, child).await.unwrap().unwrap();
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
    let output = timeout(WATCHDOG, child).await.unwrap().unwrap();
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
            .arg("--schema")
            .arg(schema)
            .arg("work");
        if let Some(extra) = extra {
            command.arg(extra);
        }
        timeout(WATCHDOG, command.output()).await.unwrap().unwrap()
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
    let output = timeout(WATCHDOG, command.output()).await.unwrap().unwrap();
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
    let output = timeout(WATCHDOG, child).await.unwrap().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "9007199254740993.0\n"
    );
    fixture.close().await;
}
