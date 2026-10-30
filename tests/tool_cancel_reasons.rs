#![cfg(unix)]
//! Real local HTTP + SQLite + shell vertical paths; no production credentials.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

struct Reply {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            kind: "application/json",
            body: value.to_string().into_bytes(),
        }
    }
}
struct Incoming {
    body: Value,
    reply: oneshot::Sender<Reply>,
}
async fn read_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let split = loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break split + 4;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..split]);
    let length = head
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap();
    while bytes.len() < split + length {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[split..split + length]).unwrap()
}
async fn proxy() -> (String, mpsc::UnboundedReceiver<Incoming>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (send, receive) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let send = send.clone();
            tokio::spawn(async move {
                let body = read_request(&mut stream).await;
                let (reply, receive) = oneshot::channel();
                if send.send(Incoming { body, reply }).is_err() {
                    return;
                }
                let Ok(reply) = receive.await else {
                    return;
                };
                let head = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    reply.kind,
                    reply.body.len()
                );
                if stream.write_all(head.as_bytes()).await.is_ok() {
                    let _ = stream.write_all(&reply.body).await;
                }
            });
        }
    });
    (url, receive, task)
}
async fn runtime_with_io(url: &str, io: &str) -> (tempfile::TempDir, DataOwner, Arc<Control>) {
    // These vertical paths exercise explicit recovery/retry and its saved facts.
    // Default automatic partial retries are covered by response_retries instead.
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(&config,format!("default_connection='test'\ndefault_model='fixture'\n[connections.test]\nbase_url='{url}'\nresponse_retry_partial=false\n[io]\nterm_grace_ms=100\ndrain_timeout_ms=100\nself_reply_grace_ms=10\nshutdown_grace_ms=2000\n{io}")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(directory.path().join("data")),
        directory.path().to_path_buf(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, directory.path().to_path_buf());
    (directory, owner, control)
}
async fn command(control: &Arc<Control>, command: Command) -> Value {
    let request = Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        command,
    );
    let response = control.request(request).await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
async fn start(control: &Arc<Control>) -> MessageAccepted {
    let thread = command(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_id: None,
        }),
    )
    .await;
    serde_json::from_value(
        command(
            control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                now: false,
                thread_id: Some(thread["thread_id"].as_str().unwrap().into()),
                text: "fixture instruction".into(),
            }),
        )
        .await,
    )
    .unwrap()
}
async fn incoming(receiver: &mut mpsc::UnboundedReceiver<Incoming>) -> Incoming {
    timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[derive(Clone, Copy)]
enum Scenario {
    Failed,
    NewInput,
    HumanBackground,
}

async fn check(scenario: Scenario, expected: &str) {
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime_with_io(&url, "").await;
    let accepted = start(&control).await;
    let first = incoming(&mut requests).await;
    let shell = match scenario {
        Scenario::Failed => "exit 7",
        Scenario::NewInput | Scenario::HumanBackground => "cat",
    };
    first.reply.send(Reply::json(json!({"id":"group","status":"completed","output":[
        {"type":"function_call","id":"first","call_id":"first-call","name":"shell","arguments":json!({"command":shell,"background":false,"stdin_open":matches!(scenario,Scenario::NewInput|Scenario::HumanBackground)}).to_string()},
        {"type":"function_call","id":"second","call_id":"second-call","name":"shell","arguments":json!({"command":"printf must-not-run","background":false}).to_string()},
        {"type":"function_call","id":"third","call_id":"third-call","name":"shell","arguments":json!({"command":"printf also-must-not-run","background":false}).to_string()}
    ]}))).ok().unwrap();
    match scenario {
        Scenario::HumanBackground => timeout(Duration::from_secs(5), async {
            loop {
                if control.background(&accepted.thread_id).await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap(),
        Scenario::NewInput => {
            timeout(Duration::from_secs(5),async {
                loop {
                    let running=control.store.database.call(|db| Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE state='running')",[],|r|r.get::<_,bool>(0))?)).await.unwrap();
                    if running {break;} tokio::task::yield_now().await;
                }
            }).await.unwrap();
            command(
                &control,
                Command::MessageSend(MessageSendArgs {
                    sender_label: None,
                    thread_id: Some(accepted.thread_id.clone()),
                    text: "additional instruction".into(),
                    now: true,
                }),
            )
            .await;
        }
        _ => {}
    }
    let next = incoming(&mut requests).await;
    let results = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| {
            (
                item["call_id"].as_str().unwrap().to_owned(),
                serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 3);
    let prerequisite = &results[0].1;
    for (call, result) in &results[1..] {
        assert!(call == "second-call" || call == "third-call");
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["executed"], false);
        assert_eq!(result["reason"], expected);
        assert!(!result["message"].as_str().unwrap().is_empty());
        assert_eq!(result["prerequisite"]["call_id"], "first-call");
        assert_eq!(
            result["prerequisite"]["execution_id"],
            prerequisite["execution_id"]
        );
        assert_eq!(result["prerequisite"]["state"], prerequisite["status"]);
    }
    if matches!(scenario, Scenario::Failed) {
        assert_eq!(prerequisite["status"], "exited");
        assert_eq!(prerequisite["exit_code"], 7);
        assert_eq!(results[1].1["prerequisite"]["exit_code"], 7);
        assert_eq!(results[1].1["prerequisite"]["output_complete"], true);
    } else {
        assert_eq!(prerequisite["status"], "running");
        assert!(results[1].1["prerequisite"].get("exit_code").is_none());
    }
    let saved=control.store.database.call(|db|{
        assert_eq!(db.query_row("SELECT COUNT(*) FROM shell_execution",[],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM tool_call WHERE call_id IN ('second-call','third-call') AND operation_id IS NOT NULL",[],|r|r.get::<_,i64>(0))?,0);
        let mut q=db.prepare("SELECT payload_json FROM node WHERE kind='tool_result' ORDER BY rowid")?;
        Ok(q.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap();
    let saved = saved
        .iter()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    for (call, result) in &results[1..] {
        let node = saved
            .iter()
            .find(|node| node["item"]["call_id"] == *call)
            .unwrap();
        let actual: Value = serde_json::from_str(node["item"]["output"].as_str().unwrap()).unwrap();
        assert_eq!(
            &actual, result,
            "model replay must contain the committed cancellation facts"
        );
    }
    timeout(Duration::from_secs(8), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
    drop(next);
    drop(directory);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_prerequisite_saves_exit_facts_without_starting_remaining_calls() {
    check(Scenario::Failed, "previous_call_failed").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_input_release_preserves_running_execution_without_duplicate_calls() {
    check(Scenario::NewInput, "previous_wait_released_by_new_input").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_background_release_has_its_own_saved_cancellation_reason() {
    check(
        Scenario::HumanBackground,
        "previous_wait_released_by_human_background",
    )
    .await;
}
