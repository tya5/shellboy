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
    fn incomplete(text: &str) -> Self {
        Self {
            status: 200,
            kind: "text/event-stream",
            body: format!(
                "event: response.output_text.delta\ndata: {}\n\n",
                json!({"type":"response.output_text.delta","delta":text})
            )
            .into_bytes(),
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
async fn runtime(url: &str) -> (tempfile::TempDir, DataOwner, Arc<Control>) {
    runtime_with_io(url, "").await
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
fn final_answer(text: &str) -> Reply {
    Reply::json(
        json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"id":uuid::Uuid::new_v4().to_string(),"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]}),
    )
}
fn tool(command: &str, background: bool) -> Reply {
    Reply::json(
        json!({"id":"fixture-tool-response","status":"completed","output":[{"type":"function_call","id":"fixture-tool-item","call_id":"fixture-call","name":"shell","arguments":json!({"command":command,"background":background}).to_string()}]}),
    )
}
async fn state(control: &Arc<Control>, table: &str, id: &str, wanted: &str) {
    let table = table.to_owned();
    let id = id.to_owned();
    let wanted = wanted.to_owned();
    let result = timeout(Duration::from_secs(8), async {
        loop {
            let table = table.clone();
            let id = id.clone();
            let value = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        &format!("SELECT state FROM {table} WHERE id=?1"),
                        [id],
                        |row| row.get::<_, String>(0),
                    )?)
                })
                .await
                .unwrap();
            if value == wanted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    if result.is_err() {
        let diagnostic = timeout(Duration::from_millis(250), control.store.database.call(|db| {
            let turns = db.prepare("SELECT id,state,generation FROM turn ORDER BY rowid")?
                .query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let shells = db.prepare("SELECT id,state,json_extract(shell_json,'$.spawned'),saved_sequence FROM shell_execution ORDER BY rowid")?
                .query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<bool>>(2)?,row.get::<_,i64>(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let waits = db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",[],|row|row.get::<_,i64>(0))?;
            Ok((turns,shells,waits))
        })).await;
        panic!("{table} {id} did not become {wanted}: {diagnostic:?}");
    }
}
async fn close(control: Arc<Control>, owner: DataOwner, server: JoinHandle<()>) {
    timeout(Duration::from_secs(8), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_read_keeps_target_raw_separate_from_message_send_and_observer_output() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let other_thread = command(
        &control,
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
    let accepted = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(tool("sleep 2; printf SHELLBOY_BG_READ_V1", true))
        .ok()
        .unwrap();
    let continuation = incoming(&mut requests).await;
    let result = continuation.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    let result: Value = serde_json::from_str(result["output"].as_str().unwrap()).unwrap();
    let target = result["execution_id"].as_str().unwrap().to_owned();
    assert_eq!(result["command"], "sleep 2; printf SHELLBOY_BG_READ_V1");
    assert_eq!(result["thread_id"], accepted.thread_id);
    assert_eq!(result["run_id"], accepted.run_id.as_deref().unwrap());
    assert_eq!(result["turn_id"], accepted.turn_id.as_deref().unwrap());
    assert!(result["cwd"].as_str().unwrap().starts_with('/'));
    let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
    // A second genuine model tool call observes the first one over IPC. Its stdout
    // includes a message receipt, while the target must retain only its own token.
    let script = format!(
        "test -n \"$SHELLBOY_SOCKET\" || exit 91\n\
         '{executable}' message send --thread '{}' --text 'unrelated receipt fixture'\n\
         '{executable}' wait --conditions-json '[{{\"type\":\"shell_ended\",\"execution_id\":\"{target}\"}}]' --timeout-ms 30000\n\
         '{executable}' shell get --execution '{target}'\n\
         '{executable}' shell read --execution '{target}' --encoding UTF-8 --limit-bytes 65536",
        other_thread["thread_id"].as_str().unwrap()
    );
    continuation.reply.send(Reply::json(json!({
        "id":"observer-response","status":"completed","output":[{
            "type":"function_call","id":"observer-item","call_id":"observer-call","name":"shell",
            "arguments":json!({"command":script,"background":false}).to_string()
        }]
    }))).ok().unwrap();
    let observed = loop {
        let request = incoming(&mut requests).await;
        let output = request.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "observer-call"
            });
        if let Some(output) = output {
            let result: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
            request
                .reply
                .send(final_answer("observer checked"))
                .ok()
                .unwrap();
            break result;
        }
        request
            .reply
            .send(final_answer("unrelated message handled"))
            .ok()
            .unwrap();
    };
    let observer = observed["execution_id"].as_str().unwrap();
    assert_ne!(observer, target);
    assert_eq!(observed["command"], script);
    assert_eq!(observed["thread_id"], result["thread_id"]);
    assert_eq!(observed["turn_id"], result["turn_id"]);
    assert_eq!(observed["status"], "exited");
    let replies: Vec<Value> = observed["output"]
        .as_str()
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 4);
    for reply in &replies {
        assert_eq!(reply["ok"], true, "{reply}");
    }
    assert_eq!(replies[0]["result"]["delivery_status"], "accepted");
    assert_eq!(replies[1]["result"]["status"], "satisfied");
    assert_eq!(replies[2]["result"]["state"], "exited");
    assert_eq!(replies[2]["result"]["exit_code"], 0);
    for field in ["command", "cwd", "thread_id", "run_id", "turn_id"] {
        assert_eq!(replies[2]["result"][field], result[field], "{field}");
    }
    let read = &replies[3]["result"];
    assert_eq!(read["execution_id"], target);
    assert_eq!(read["text"], "SHELLBOY_BG_READ_V1");
    assert_eq!(read["raw_bytes"], b"SHELLBOY_BG_READ_V1".len());
    assert_eq!(read["encoding"], "UTF-8");
    assert_eq!(read["decode_errors"], false);
    assert_eq!(read["output_complete"], true);
    let observer_read = command(
        &control,
        Command::ShellRead(ShellReadArgs {
            execution_id: observer.into(),
            cursor: None,
            encoding: Some("UTF-8".into()),
            limit_bytes: 65536,
        }),
    )
    .await;
    assert_eq!(observer_read["text"], observed["output"]);
    control
        .store
        .database
        .call(move |db| {
            let raw = db
                .prepare("SELECT bytes FROM output_chunk WHERE execution_id=?1 ORDER BY sequence")?
                .query_map([target], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .concat();
            assert_eq!(raw, b"SHELLBOY_BG_READ_V1");
            // Genuine inherited shell credentials crossed the local IPC boundary:
            // sender provenance and both read/wait runtime intents must omit them.
            for sql in [
                "SELECT count(*) FROM operation_receipt WHERE json_type(origin_json,'$.token') IS NOT NULL",
                "SELECT count(*) FROM receipt WHERE json_type(sender_json,'$.token') IS NOT NULL",
            ] {
                assert_eq!(db.query_row(sql, [], |r| r.get::<_, i64>(0))?, 0);
            }
            assert!(db.query_row("SELECT count(*) FROM receipt WHERE json_extract(sender_json,'$.execution_id') IS NOT NULL", [], |r| r.get::<_, i64>(0))? > 0);
            Ok(())
        })
        .await
        .unwrap();
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    local.shutdown().await.unwrap();
    serving.abort();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_message_proxy_shell_final_and_reopen_preserve_original() {
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    let first = incoming(&mut requests).await;
    assert_eq!(first.body["store"], false);
    assert!(first.body.to_string().contains("fixture instruction"));
    first
        .reply
        .send(tool("printf 'runtime fixture 日本語'", false))
        .ok()
        .unwrap();
    let second = incoming(&mut requests).await;
    let result = second.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    let result: Value = serde_json::from_str(result["output"].as_str().unwrap()).unwrap();
    let started =
        chrono::DateTime::parse_from_rfc3339(result["started_at"].as_str().unwrap()).unwrap();
    let ended = chrono::DateTime::parse_from_rfc3339(result["ended_at"].as_str().unwrap()).unwrap();
    assert!(ended >= started);
    // Epoch timestamps remain facts, while duration comes from the supervisor's
    // monotonic measurement rather than from their wall-clock difference.
    let details = command(
        &control,
        Command::ShellGet(ExecutionTarget {
            execution_id: result["execution_id"].as_str().unwrap().into(),
        }),
    )
    .await;
    assert!(result["elapsed_ms"].as_u64().is_some());
    assert_eq!(result["elapsed_ms"], details["elapsed_ms"]);
    assert!(
        result["output"]
            .as_str()
            .unwrap()
            .contains("runtime fixture 日本語")
    );
    second
        .reply
        .send(final_answer("finished fixture"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        accepted.turn_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    control
        .store
        .database
        .call(|db| {
            let raw: Vec<u8> = db
                .prepare("SELECT bytes FROM output_chunk ORDER BY sequence")?
                .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .concat();
            assert_eq!(String::from_utf8(raw).unwrap(), "runtime fixture 日本語");
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM model_attempt WHERE state='completed'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                2
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM delivery WHERE state='pending'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0,
                "foreground completion is consumed by its tool result"
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(75), requests.recv())
            .await
            .is_err(),
        "no spurious completion Turn"
    );
    close(control, owner, server).await;
    let reopened = shellboy::storage::DbWorker::open(directory.path().join("data/shellboy.db"))
        .await
        .unwrap();
    reopened
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM receipt WHERE state='applied'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_additional_input_is_accepted_while_shell_lives_and_stop_reaps_before_terminal()
{
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(tool("sleep 10; printf unexpected", false))
        .ok()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            let running = control
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM shell_execution WHERE state='running')",
                        [],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let additional = timeout(
        Duration::from_millis(500),
        command(
            &control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                thread_id: Some(accepted.thread_id.clone()),
                text: "while foreground waits".into(),
                now: true,
            }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        additional["turn_id"],
        accepted.turn_id.as_ref().unwrap().as_str()
    );
    let second = incoming(&mut requests).await;
    assert!(second.body.to_string().contains("while foreground waits"));
    assert!(second.body.to_string().contains("additional_input"));
    let released = second.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    let released: Value = serde_json::from_str(released["output"].as_str().unwrap()).unwrap();
    assert_eq!(released["command"], "sleep 10; printf unexpected");
    assert_eq!(released["thread_id"], accepted.thread_id);
    assert_eq!(released["turn_id"], accepted.turn_id.as_deref().unwrap());
    second
        .reply
        .send(final_answer("background remains"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        accepted.turn_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    let run_id = accepted.run_id.unwrap();
    command(
        &control,
        Command::RunStop(RunTarget {
            run_id: run_id.clone(),
        }),
    )
    .await;
    state(&control, "run", &run_id, "stopped").await;
    control.store.database.call(|db|{
        assert_eq!(db.query_row("SELECT count(*) FROM shell_execution WHERE state IN ('starting','running','stopping')",[],|row|row.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incomplete_response_created_after_gc_roots_survives_sweep() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    let request = incoming(&mut requests).await;
    // Finish the initial maintenance pass before introducing this explicit job.
    control
        .gc
        .collect(StorageCategory::History, chrono::Utc::now())
        .await
        .unwrap();
    let deleted = command(
        &control,
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
    let deleted_thread = deleted["thread_id"].as_str().unwrap().to_owned();
    let fixture_thread = deleted_thread.clone();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            shellboy::history::append_node(
                &tx,
                &fixture_thread,
                "unreachable",
                None,
                "message",
                &json!({"text":"collect me"}),
                "now",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    command(
        &control,
        Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: deleted_thread,
            descendants: false,
        }),
    )
    .await;
    let job = control.gc.start_history().await.unwrap().unwrap();
    loop {
        if control.gc.history_step(&job).await.unwrap().stage == "sweep" {
            break;
        }
    }
    let marked_revision = control
        .store
        .database
        .call({
            let job = job.clone();
            move |db| {
                Ok(
                    db.query_row("SELECT root_revision FROM gc_job WHERE id=?1", [job], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            }
        })
        .await
        .unwrap();
    // The mock holds EOF until after Roots/Provenance, so this is not a timing race.
    request
        .reply
        .send(Reply::incomplete("audit partial after roots"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        accepted.turn_id.as_deref().unwrap(),
        "recovery_wait",
    )
    .await;
    let before = control.store.database.call(|db| {
        Ok(db.query_row("SELECT id,payload_json,payload_revision,turn_id FROM node WHERE kind='incomplete_response'", [], |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,i64>(2)?, row.get::<_,String>(3)?)))?)
    }).await.unwrap();
    let step = control.gc.history_step(&job).await.unwrap();
    assert!(
        !step.restarted,
        "bounded GC progress must survive new roots"
    );
    assert!(
        step.root_revision > marked_revision,
        "recover must still publish its newly rooted audit node"
    );
    loop {
        if control.gc.history_step(&job).await.unwrap().complete {
            break;
        }
    }
    control.store.database.call(move |db| {
        let after = db.query_row("SELECT id,payload_json,payload_revision,turn_id FROM node WHERE kind='incomplete_response'", [], |row| Ok((row.get::<_,String>(0)?, row.get::<_,Option<String>>(1)?, row.get::<_,i64>(2)?, row.get::<_,String>(3)?)))?;
        assert_eq!(after, (before.0, Some(before.1), before.2, before.3), "the original audit body and version must survive");
        let deleted: (Option<String>,String) = db.query_row("SELECT payload_json,deleted_reason FROM node WHERE id='unreachable'", [], |row| Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!(deleted, (None,"thread_deleted_unreachable".into()));
        let audit_on_path: i64 = db.query_row("SELECT COUNT(*) FROM thread_path WHERE node_id=?1", [&after.0], |row| row.get(0))?;
        assert_eq!(audit_on_path, 0, "preserving audit data must not select it for replay");
        Ok(())
    }).await.unwrap();
    close(control, owner, server).await;
}

fn foreground_pair() -> Reply {
    Reply::json(
        json!({"id":"foreground-pair","status":"completed","output":[
            {"type":"function_call","id":"pair-first","call_id":"pair-first-call","name":"shell","arguments":json!({"command":"printf FIRST","background":false}).to_string()},
            {"type":"function_call","id":"pair-second","call_id":"pair-second-call","name":"shell","arguments":json!({"command":"printf SECOND","background":false}).to_string()}
        ]}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_executes_both_foreground_calls_without_a_spurious_interrupt() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(Reply::incomplete("retry fixture"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        accepted.turn_id.as_deref().unwrap(),
        "recovery_wait",
    )
    .await;
    command(
        &control,
        Command::RunRetry(RunTarget {
            run_id: accepted.run_id.clone().unwrap(),
        }),
    )
    .await;
    incoming(&mut requests)
        .await
        .reply
        .send(foreground_pair())
        .ok()
        .unwrap();
    let next = incoming(&mut requests).await;
    let results: Vec<Value> = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(results.len(), 2);
    for (result, output) in results.iter().zip(["FIRST", "SECOND"]) {
        assert_eq!(result["status"], "exited");
        assert_eq!(result["output"], output);
    }
    next.reply
        .send(final_answer("both calls complete"))
        .ok()
        .unwrap();
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_accepted_during_model_request_still_interrupts_the_next_foreground_call() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    let model_request = incoming(&mut requests).await;
    command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            thread_id: Some(accepted.thread_id.clone()),
            text: "new input before shell starts".into(),
            now: true,
        }),
    )
    .await;
    model_request.reply.send(Reply::json(json!({"id":"early-input-group","status":"completed","output":[
        {"type":"function_call","id":"first","call_id":"early-first-call","name":"shell","arguments":json!({"command":"cat","stdin_open":true,"background":false}).to_string()},
        {"type":"function_call","id":"second","call_id":"early-second-call","name":"shell","arguments":json!({"command":"printf must-not-run","background":false}).to_string()}
    ]}))).ok().unwrap();
    let next = incoming(&mut requests).await;
    let results: Vec<Value> = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["reason"], "additional_input");
    assert_eq!(results[1]["status"], "cancelled");
    assert!(
        next.body
            .to_string()
            .contains("new input before shell starts")
    );
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM shell_execution", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incomplete_attempt_is_preserved_retry_reuses_turn_instruction_snapshot_and_tools_do_not_repeat()
 {
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    std::fs::write(
        directory.path().join("AGENTS.md"),
        "first fixed instruction",
    )
    .unwrap();
    let accepted = start(&control).await;
    let first = incoming(&mut requests).await;
    assert!(first.body.to_string().contains("first fixed instruction"));
    first.reply.send(tool("printf once", false)).ok().unwrap();
    let after_tool = incoming(&mut requests).await;
    after_tool
        .reply
        .send(Reply::incomplete("partial preserved answer"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        accepted.turn_id.as_deref().unwrap(),
        "recovery_wait",
    )
    .await;
    std::fs::write(
        directory.path().join("AGENTS.md"),
        "changed instruction must wait for NEXT Turn",
    )
    .unwrap();
    command(
        &control,
        Command::RunRetry(RunTarget {
            run_id: accepted.run_id.clone().unwrap(),
        }),
    )
    .await;
    let retry = incoming(&mut requests).await;
    assert!(retry.body.to_string().contains("first fixed instruction"));
    assert!(!retry.body.to_string().contains("changed instruction"));
    assert!(
        !retry.body.to_string().contains("partial preserved answer"),
        "incomplete text is audit-only"
    );
    retry
        .reply
        .send(final_answer("retry complete"))
        .ok()
        .unwrap();
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |row| row
                    .get::<_, i64>(0))?,
                1,
                "retry must not reexecute saved shell calls"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM instruction_snapshot", [], |row| row
                    .get::<_, i64>(
                    0
                ))?,
                1
            );
            let saved: String = db.query_row(
                "SELECT payload_json FROM node WHERE kind='incomplete_response'",
                [],
                |row| row.get(0),
            )?;
            assert!(saved.contains("partial preserved answer"));
            Ok(())
        })
        .await
        .unwrap();
    let next = command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some(accepted.thread_id.clone()),
            text: "next turn".into(),
        }),
    )
    .await;
    let next_request = incoming(&mut requests).await;
    assert!(
        next_request
            .body
            .to_string()
            .contains("changed instruction must wait for NEXT Turn")
    );
    let instructions: Vec<&Value> = next_request.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| {
            item["role"] == "developer"
                && item
                    .to_string()
                    .contains("changed instruction must wait for NEXT Turn")
        })
        .collect();
    assert_eq!(
        instructions.len(),
        1,
        "new instructions are loaded once at the new Turn"
    );
    next_request
        .reply
        .send(final_answer("next complete"))
        .ok()
        .unwrap();
    state(
        &control,
        "run",
        next["run_id"].as_str().unwrap(),
        "completed",
    )
    .await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_wait_without_live_driver_can_be_stopped() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(Reply::incomplete("saved"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        accepted.turn_id.as_deref().unwrap(),
        "recovery_wait",
    )
    .await;
    // The HTTP task is terminal and the per-Thread driver exits after recovery.
    // Additional DB roundtrips allow its cleanup to finish without timing sleeps.
    for _ in 0..3 {
        control.store.database.call(|_| Ok(())).await.unwrap();
        tokio::task::yield_now().await;
    }
    command(
        &control,
        Command::RunStop(RunTarget {
            run_id: accepted.run_id.clone().unwrap(),
        }),
    )
    .await;
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "stopped",
    )
    .await;
    assert!(
        timeout(Duration::from_millis(75), requests.recv())
            .await
            .is_err()
    );
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_completion_before_answer_is_durable_and_reaches_a_model_boundary_once() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(tool("printf background-complete", true))
        .ok()
        .unwrap();
    let second = incoming(&mut requests).await;
    let has_event = |body: &Value| {
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["role"] == "developer" && item.to_string().contains("shell_ended"))
    };
    let mut delivered = usize::from(has_event(&second.body));
    timeout(Duration::from_secs(5), async {
        loop {
            let done = control
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM shell_execution WHERE state='exited')",
                        [],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    second
        .reply
        .send(final_answer("initial answer"))
        .ok()
        .unwrap();
    if delivered == 0 {
        let third = incoming(&mut requests).await;
        delivered += usize::from(has_event(&third.body));
        third
            .reply
            .send(final_answer("background observed"))
            .ok()
            .unwrap();
    }
    assert_eq!(delivered, 1);
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    assert!(
        timeout(Duration::from_millis(75), requests.recv())
            .await
            .is_err(),
        "completion event must not generate duplicate Turns"
    );
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raw_save_failure_stops_admission_reaps_shell_and_reports_failed_shutdown() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let mut events = control.subscribe();
    let accepted = start(&control).await;
    let first = incoming(&mut requests).await;
    control.store.database.call(|db|{db.execute_batch("CREATE TRIGGER fixture_raw_save_failure BEFORE INSERT ON output_chunk BEGIN SELECT RAISE(ABORT,'fixture raw source save failure'); END;")?;Ok(())}).await.unwrap();
    first
        .reply
        .send(tool("while :; do printf flood; sleep 0.01; done", false))
        .ok()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if let shellboy::control::EngineEvent::Error { error, .. } =
                events.recv().await.unwrap()
                && error.code == ErrorCode::SaveFailed
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "stopped",
    )
    .await;
    let reply = control
        .request(Request::new(
            format!("{}:after-save-failure", control.store.instance_id),
            Command::ThreadList(ThreadListArgs::default()),
        ))
        .await;
    assert_eq!(reply.error.unwrap().code, ErrorCode::InvalidState);
    assert!(
        timeout(Duration::from_millis(75), requests.recv())
            .await
            .is_err(),
        "storage failure must not initiate another model request"
    );
    let shutdown = timeout(Duration::from_secs(8), control.shutdown())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        shutdown.code,
        ErrorCode::SaveFailed,
        "normal and failed-save shutdown remain distinguishable"
    );
    owner.shutdown().await.unwrap();
    server.abort();
}

// Profile hooks run on the fixture DB worker, after each explicit COMMIT. This
// models a fixed durable-write cost without changing production dependencies.
unsafe extern "C" fn slow_fixture_commits(
    mask: u32,
    _context: *mut std::ffi::c_void,
    statement: *mut std::ffi::c_void,
    _elapsed: *mut std::ffi::c_void,
) -> i32 {
    if mask == rusqlite::ffi::SQLITE_TRACE_PROFILE && !statement.is_null() {
        // SQLite guarantees a live sqlite3_stmt pointer during PROFILE.
        let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement.cast()) };
        if !sql.is_null()
            && unsafe { std::ffi::CStr::from_ptr(sql) }
                .to_bytes()
                .eq_ignore_ascii_case(b"COMMIT")
        {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_database_batches_binary_shell_output_without_losing_natural_exit_tail() {
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime_with_io(
        &url,
        "output_batch_bytes=32768\noutput_batch_ms=10\noutput_buffer_bytes=524288\n",
    )
    .await;
    let stdout: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
    let stderr: Vec<u8> = (0..1024 * 1024).map(|i| (255 - i % 256) as u8).collect();
    let out = directory.path().join("stdout.bin");
    let err = directory.path().join("stderr.bin");
    std::fs::write(&out, &stdout).unwrap();
    std::fs::write(&err, &stderr).unwrap();
    let accepted = start(&control).await;
    let request = incoming(&mut requests).await;
    control.store.database.call(|db| {
        db.execute_batch("CREATE TABLE fixture_output_batch(chunks INTEGER,bytes INTEGER); CREATE TRIGGER fixture_saved_batch AFTER UPDATE OF saved_sequence ON shell_execution WHEN NEW.saved_sequence>OLD.saved_sequence BEGIN INSERT INTO fixture_output_batch SELECT COUNT(*),SUM(length(bytes)) FROM output_chunk WHERE execution_id=NEW.id AND sequence>=OLD.saved_sequence AND sequence<NEW.saved_sequence; END;")?;
        // No callback state/pointers escape the connection. It is removed below.
        assert_eq!(unsafe { rusqlite::ffi::sqlite3_trace_v2(db.handle(), rusqlite::ffi::SQLITE_TRACE_PROFILE, Some(slow_fixture_commits), std::ptr::null_mut()) }, rusqlite::ffi::SQLITE_OK);
        Ok(())
    }).await.unwrap();
    request
        .reply
        .send(tool(
            &format!("cat '{}'; cat '{}' >&2", out.display(), err.display()),
            false,
        ))
        .ok()
        .unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            let saved = control
                .store
                .database
                .call(|db| {
                    Ok(
                        db.query_row("SELECT COUNT(*) FROM fixture_output_batch", [], |row| {
                            row.get::<_, i64>(0)
                        })?,
                    )
                })
                .await
                .unwrap();
            if saved > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Control handles another operation before the foreground shell completes.
    timeout(
        Duration::from_secs(1),
        command(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        ),
    )
    .await
    .unwrap();
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT state FROM shell_execution", [], |row| row
                    .get::<_, String>(0))?,
                "running"
            );
            Ok(())
        })
        .await
        .unwrap();
    let next = match timeout(Duration::from_secs(5), requests.recv()).await {
        Ok(Some(request)) => request,
        outcome => {
            let snapshot = control.store.database.call(|db| {
                let mut rows = Vec::new();
                for sql in [
                    "SELECT json_object('table','turn','id',id,'state',state,'reason',recovery_reason_json) FROM turn",
                    "SELECT json_object('table','shell','id',id,'state',state,'sequence',saved_sequence,'reason',end_reason) FROM shell_execution",
                    "SELECT json_object('table','raw','chunks',COUNT(*),'bytes',COALESCE(SUM(length(bytes)),0)) FROM output_chunk",
                    "SELECT json_object('table','group','id',id,'state',state) FROM tool_group",
                    "SELECT json_object('table','call','state',state,'execution',operation_id) FROM tool_call",
                    "SELECT json_object('table','result','status',status,'node',node_id) FROM tool_result",
                    "SELECT json_object('table','attempt','id',id,'state',state,'start',started_at,'end',ended_at) FROM model_attempt",
                    "SELECT json_object('table','pin','kind',resource_kind,'resource',resource_id) FROM runtime_pin",
                    "SELECT json_object('table','node','kind',kind,'bytes',payload_bytes) FROM node ORDER BY rowid DESC LIMIT 10",
                    "SELECT json_object('table','delivery','kind',e.kind,'state',d.state) FROM delivery d JOIN event e ON d.event_id=e.id",
                ] {
                    rows.extend(db.prepare(sql)?.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>,_>>()?);
                }
                Ok(rows)
            }).await;
            panic!(
                "missing post-shell model request: timeout={}, snapshot={snapshot:?}",
                outcome.is_err()
            );
        }
    };
    let result: Value = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
        .unwrap();
    control.store.database.call(move |db| {
        assert_eq!(unsafe { rusqlite::ffi::sqlite3_trace_v2(db.handle(), 0, None, std::ptr::null_mut()) }, rusqlite::ffi::SQLITE_OK);
        let rows = db.prepare("SELECT sequence,stream,stream_offset,bytes FROM output_chunk ORDER BY sequence")?.query_map([], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,String>(1)?, row.get::<_,i64>(2)?, row.get::<_,Vec<u8>>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
        let (mut out,mut err)=(Vec::new(),Vec::new());
        for (i,(sequence,stream,offset,bytes)) in rows.iter().enumerate() {
            assert_eq!(*sequence, i as i64);
            let body=if stream=="stdout" {&mut out} else {assert_eq!(stream,"stderr"); &mut err};
            assert_eq!(*offset, body.len() as i64);
            body.extend_from_slice(bytes);
        }
        assert_eq!(out.len(), stdout.len(), "stdout byte count");
        assert!(out==stdout, "stdout including its final binary bytes");
        assert_eq!(err.len(), stderr.len(), "stderr byte count");
        assert!(err==stderr, "stderr including its final binary bytes");
        let (batches,max_bytes,max_chunks):(i64,i64,i64)=db.query_row("SELECT COUNT(*),MAX(bytes),MAX(chunks) FROM fixture_output_batch", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        assert!(max_bytes<=32768, "a saved batch exceeded configured bytes: {max_bytes}");
        assert!(max_chunks>1 && batches<(rows.len()/2) as i64, "batching did not reduce durable commits: {batches} / {} chunks", rows.len());
        assert_eq!(db.query_row("SELECT saved_sequence FROM shell_execution", [], |row| row.get::<_,i64>(0))?, rows.len() as i64);
        Ok(())
    }).await.unwrap();
    assert_eq!(result["status"], "exited");
    assert_eq!(result["output_complete"], true);
    next.reply
        .send(final_answer("binary output complete"))
        .ok()
        .unwrap();
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_output_wait_wakes_after_commit_while_shell_and_model_remain_blocked() {
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    let gate = directory.path().join("output-gate");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&gate)
            .status()
            .unwrap()
            .success()
    );
    let shell_command = format!(
        "read -r release < '{}'; printf READY; read -r finish",
        gate.to_string_lossy().replace('\'', "'\\''")
    );
    start(&control).await;
    incoming(&mut requests).await.reply.send(Reply::json(json!({
        "id":"output-wait-response","status":"completed","output":[{
            "type":"function_call","id":"output-wait-item","call_id":"output-wait-call","name":"shell",
            "arguments":json!({"command":shell_command,"background":true,"stdin_open":true}).to_string()
        }]
    }))).ok().unwrap();
    // All model/tool transitions are past; hold the next HTTP response throughout
    // the observation so it cannot provide an unrelated change notification.
    let _continuation = incoming(&mut requests).await;
    let execution = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row("SELECT id FROM shell_execution", [], |row| {
                row.get::<_, String>(0)
            })?)
        })
        .await
        .unwrap();
    // Read directly through the production reader: an ordinary CLI runtime
    // request completion also wakes observers and would hide this missing wake.
    let reader = shellboy::control::output::ShellOutputReader::new(
        control.store.database.clone(),
        control.store.instance_id.clone(),
    );
    let empty = reader
        .read(ShellReadArgs {
            execution_id: execution.clone(),
            cursor: None,
            encoding: Some("UTF-8".into()),
            limit_bytes: 100,
        })
        .await
        .unwrap();
    assert_eq!(empty.raw_bytes, 0);
    let operation = format!("{}:output-wait", control.store.instance_id);
    let request = Request::new(
        operation.clone(),
        Command::Wait(WaitArgs {
            conditions: vec![WaitCondition::ShellOutput {
                execution_id: execution.clone(),
                cursor: empty.cursor,
            }],
            timeout_ms: 30_000,
        }),
    );
    let observer = control.clone();
    let mut waiting = tokio::spawn(async move { observer.request(request).await });
    timeout(Duration::from_secs(3), async {
        loop {
            let key = format!("wait.pending.{operation}");
            let registered = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                        [key],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if registered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Release the FIFO from outside Control: neither a CLI completion nor shell
    // exit can wake the observer. The second read keeps the shell alive.
    tokio::task::spawn_blocking(move || std::fs::write(gate, b"release\n"))
        .await
        .unwrap()
        .unwrap();
    let observed = timeout(Duration::from_secs(2), &mut waiting).await;
    waiting.abort();
    let saved = control
        .store
        .database
        .call(move |db| {
            let state: String = db.query_row(
                "SELECT state FROM shell_execution WHERE id=?1",
                [&execution],
                |row| row.get(0),
            )?;
            let raw = db
                .prepare("SELECT bytes FROM output_chunk WHERE execution_id=?1 ORDER BY sequence")?
                .query_map([execution], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .concat();
            Ok((state, raw))
        })
        .await
        .unwrap();
    close(control, owner, server).await;
    assert_eq!(saved, ("running".into(), b"READY".to_vec()));
    let response = observed
        .expect("saved output must wake wait before its 30-second deadline")
        .unwrap();
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.result.unwrap()["status"], "satisfied");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdin_operation_retransmission_writes_once_and_wait_any_observes_completion() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    let first = incoming(&mut requests).await;
    first.reply.send(Reply::json(json!({"id":"stdin-response","status":"completed","output":[{"type":"function_call","id":"stdin-item","call_id":"stdin-call","name":"shell","arguments":json!({"command":"cat","background":true,"stdin_open":true}).to_string()}]}))).ok().unwrap();
    let second = incoming(&mut requests).await;
    let execution = timeout(Duration::from_secs(3), async {
        loop {
            let id = control
                .store
                .database
                .call(|db| {
                    use rusqlite::OptionalExtension;
                    Ok(db
                        .query_row(
                            "SELECT id FROM shell_execution WHERE state='running'",
                            [],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?)
                })
                .await
                .unwrap();
            if let Some(id) = id {
                break id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let write = Request::new(
        format!("{}:write-once", control.store.instance_id),
        Command::ShellWrite(ShellWriteArgs {
            execution_id: execution.clone(),
            text: "once\n".into(),
            encoding: "UTF-8".into(),
        }),
    );
    let (a, b) = tokio::join!(control.request(write.clone()), control.request(write));
    assert!(a.ok && b.ok);
    assert_eq!(a.result, b.result);
    command(
        &control,
        Command::ShellEof(ExecutionTarget {
            execution_id: execution.clone(),
        }),
    )
    .await;
    let waited = command(
        &control,
        Command::Wait(WaitArgs {
            conditions: vec![WaitCondition::ShellEnded {
                execution_id: execution.clone(),
            }],
            timeout_ms: 5000,
        }),
    )
    .await;
    assert_eq!(waited["status"], "satisfied");
    let execution_copy = execution.clone();
    control
        .store
        .database
        .call(move |db| {
            let raw = db
                .prepare("SELECT bytes FROM output_chunk WHERE execution_id=?1 ORDER BY sequence")?
                .query_map([execution_copy], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .concat();
            assert_eq!(raw, b"once\n");
            Ok(())
        })
        .await
        .unwrap();
    let event_in_second = second.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["role"] == "developer" && item.to_string().contains("shell_ended"));
    second
        .reply
        .send(final_answer("stdin observed"))
        .ok()
        .unwrap();
    if !event_in_second {
        incoming(&mut requests)
            .await
            .reply
            .send(final_answer("completion observed"))
            .ok()
            .unwrap();
    }
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    let completed = command(
        &control,
        Command::Wait(WaitArgs {
            conditions: vec![
                WaitCondition::TurnCompleted {
                    turn_id: accepted.turn_id.unwrap(),
                },
                WaitCondition::RunEnded {
                    run_id: accepted.run_id.unwrap(),
                },
            ],
            timeout_ms: 0,
        }),
    )
    .await;
    assert_eq!(completed["matched"].as_array().unwrap().len(), 2);
    // EOF is an idempotent close request, including after the process and its
    // live handle have retired. It never creates a new execution or new bytes.
    let repeat = Request::new(
        format!("{}:terminal-eof", control.store.instance_id),
        Command::ShellEof(ExecutionTarget {
            execution_id: execution.clone(),
        }),
    );
    let first = control.request(repeat.clone()).await;
    assert!(first.ok, "{:?}", first.error);
    assert_eq!(first.result.as_ref().unwrap()["state"], "exited");
    assert_eq!(first.result.as_ref().unwrap()["execution_id"], execution);
    assert_eq!(control.request(repeat).await, first);
    let fresh = command(
        &control,
        Command::ShellEof(ExecutionTarget {
            execution_id: execution.clone(),
        }),
    )
    .await;
    assert_eq!(fresh["state"], "exited");
    assert_eq!(fresh["execution_id"], execution);
    let missing = control
        .request(Request::new(
            format!("{}:unknown-eof", control.store.instance_id),
            Command::ShellEof(ExecutionTarget {
                execution_id: "unknown-execution".into(),
            }),
        ))
        .await;
    assert_eq!(missing.error.unwrap().code, ErrorCode::TargetUnavailable);
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            let raw = db
                .prepare("SELECT bytes FROM output_chunk WHERE execution_id=?1 ORDER BY sequence")?
                .query_map([execution], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .concat();
            assert_eq!(raw, b"once\n");
            Ok(())
        })
        .await
        .unwrap();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_shell_cli_cannot_mutate_an_independent_runs_live_shell() {
    async fn snapshot(
        control: &Control,
        execution: &str,
    ) -> (String, String, i64, Vec<u8>, String) {
        let execution = execution.to_owned();
        control.store.database.call(move |db| {
            let (state, stdin, sequence, run) = db.query_row(
                "SELECT s.state,s.stdin_state,s.saved_sequence,r.state FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE s.id=?1",
                [&execution], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            let raw = db.prepare("SELECT bytes FROM output_chunk WHERE execution_id=?1 ORDER BY sequence")?
                .query_map([execution], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?.concat();
            Ok((state, stdin, sequence, raw, run))
        }).await.unwrap()
    }
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let victim = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(Reply::json(json!({
            "id":"victim-response","status":"completed","output":[{
                "type":"function_call","id":"victim-item","call_id":"victim-call","name":"shell",
                "arguments":json!({"command":"cat","background":true,"stdin_open":true}).to_string()
            }]
        })))
        .ok()
        .unwrap();
    // Hold the victim's HTTP continuation while another independent Run executes real CLI calls.
    let victim_continuation = incoming(&mut requests).await;
    let victim_run = victim.run_id.unwrap();
    let execution = control
        .store
        .database
        .call({
            let run = victim_run.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT id FROM shell_execution WHERE owner_run_id=?1 AND state='running'",
                    [run],
                    |row| row.get::<_, String>(0),
                )?)
            }
        })
        .await
        .unwrap();
    command(
        &control,
        Command::ShellWrite(ShellWriteArgs {
            execution_id: execution.clone(),
            text: "baseline\n".into(),
            encoding: "UTF-8".into(),
        }),
    )
    .await;
    let before = timeout(Duration::from_secs(3), async {
        loop {
            let saved = snapshot(&control, &execution).await;
            if saved.3 == b"baseline\n" {
                break saved;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(before.0, "running");
    assert_eq!(before.4, "live");

    let attacker = start(&control).await;
    let attacker_request = incoming(&mut requests).await;
    control
        .store
        .database
        .call({
            let victim = victim_run.clone();
            let attacker = attacker.run_id.clone().unwrap();
            move |db| {
                let independent: i64 = db.query_row(
                    "SELECT count(*) FROM run WHERE id IN (?1,?2) AND owner_run_id IS NULL",
                    [victim, attacker],
                    |row| row.get(0),
                )?;
                assert_eq!(independent, 2);
                Ok(())
            }
        })
        .await
        .unwrap();
    let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
    // Each command gets the genuine shell-issued Origin through inherited environment + IPC.
    // Deliberately omit `set -e`: every denied command must execute independently.
    let script = format!(
        "'{executable}' shell write --execution '{execution}' --text 'unauthorized-write'\n\
         '{executable}' shell eof --execution '{execution}'\n\
         '{executable}' shell stop --execution '{execution}'\n\
         '{executable}' run stop --run '{victim_run}'"
    );
    attacker_request
        .reply
        .send(tool(&script, false))
        .ok()
        .unwrap();
    let continuation = incoming(&mut requests).await;
    let output = continuation.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    let output: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
    let errors: Vec<Value> = output["output"]
        .as_str()
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(errors.len(), 4);
    for error in errors {
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "invalid_state");
        assert_eq!(
            error["error"]["message"],
            "Only a human or an owning Run may change this execution"
        );
    }
    assert_eq!(snapshot(&control, &execution).await, before);
    // A permitted human write after all four denials proves the original cat and stdin remain live.
    command(
        &control,
        Command::ShellWrite(ShellWriteArgs {
            execution_id: execution.clone(),
            text: "still-live\n".into(),
            encoding: "UTF-8".into(),
        }),
    )
    .await;
    timeout(Duration::from_secs(3), async {
        loop {
            let saved = snapshot(&control, &execution).await;
            if saved.3 == b"baseline\nstill-live\n" {
                assert_eq!(saved.0, "running");
                assert_eq!(saved.4, "live");
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    continuation
        .reply
        .send(final_answer("ownership boundary observed"))
        .ok()
        .unwrap();
    state(
        &control,
        "run",
        attacker.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    command(
        &control,
        Command::RunStop(RunTarget {
            run_id: victim_run.clone(),
        }),
    )
    .await;
    state(&control, "run", &victim_run, "stopped").await;
    drop(victim_continuation);
    local.shutdown().await.unwrap();
    serving.abort();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_shell_cli_starts_owned_child_and_parent_receives_answer_and_run_end_separately() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let parent = start(&control).await;
    let parent_request = incoming(&mut requests).await;
    let child = command(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_id: Some(parent.thread_id.clone()),
        }),
    )
    .await;
    let child_id = child["thread_id"].as_str().unwrap().to_owned();
    let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
    parent_request
        .reply
        .send(tool(
            &format!("'{executable}' message send --thread '{child_id}' --text 'child fixture'"),
            false,
        ))
        .ok()
        .unwrap();
    let is_child = |body: &Value| {
        body["input"].as_array().unwrap().iter().any(|item| {
            item["role"] == "developer" && item.to_string().contains(&format!("thread={child_id}"))
        })
    };
    let mut child_request = None;
    let mut parent_continuation = None;
    for _ in 0..2 {
        let request = incoming(&mut requests).await;
        if is_child(&request.body) {
            child_request = Some(request);
        } else {
            parent_continuation = Some(request);
        }
    }
    parent_continuation
        .unwrap()
        .reply
        .send(final_answer("child is still running"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        parent.turn_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    let child_thread = child_id.clone();
    let parent_run = parent.run_id.clone().unwrap();
    control
        .store
        .database
        .call(move |db| {
            let owner: String = db.query_row(
                "SELECT owner_run_id FROM run WHERE thread_id=?1",
                [child_thread],
                |row| row.get(0),
            )?;
            assert_eq!(
                owner, parent_run,
                "shell CLI origin creates execution ownership, not just Thread lineage"
            );
            Ok(())
        })
        .await
        .unwrap();
    child_request
        .unwrap()
        .reply
        .send(final_answer("child final answer"))
        .ok()
        .unwrap();
    let run = parent.run_id.clone().unwrap();
    for _ in 0..4 {
        let done = control
            .store
            .database
            .call({
                let run = run.clone();
                move |db| {
                    Ok(db.query_row(
                        "SELECT state='completed' FROM run WHERE id=?1",
                        [run],
                        |row| row.get::<_, bool>(0),
                    )?)
                }
            })
            .await
            .unwrap();
        if done {
            break;
        }
        tokio::select! {
            request=requests.recv()=>{
                let request=request.unwrap();assert!(!is_child(&request.body));
                assert!(request.body.to_string().contains("child_turn_completed")||request.body.to_string().contains("child_run_ended"));
                request.reply.send(final_answer("child status observed")).ok().unwrap();
            },
            _=state(&control,"run",&run,"completed")=>break,
        }
    }
    state(&control, "run", &run, "completed").await;
    control
        .store
        .database
        .call(move |db| {
            let turn_events: i64 = db.query_row(
                "SELECT count(*) FROM event WHERE target_run_id=?1 AND kind='child_turn_completed'",
                [&run],
                |row| row.get(0),
            )?;
            let run_events: i64 = db.query_row(
                "SELECT count(*) FROM event WHERE target_run_id=?1 AND kind='child_run_ended'",
                [run],
                |row| row.get(0),
            )?;
            assert_eq!((turn_events, run_events), (1, 1));
            Ok(())
        })
        .await
        .unwrap();
    local.shutdown().await.unwrap();
    serving.abort();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_background_keeps_the_first_shell_alive_and_cancels_remaining_calls_with_notice() {
    assert_human_background_notice(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_background_of_a_single_shell_notifies_ui_and_model_without_cancelled_calls() {
    assert_human_background_notice(false).await;
}

async fn assert_human_background_notice(has_remaining_call: bool) {
    let (url, mut requests, proxy_server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let mut events = control.subscribe();
    let (rpc_send, rpc_receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, rpc_send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(rpc_receive));
    let (actions, actions_receive) = mpsc::channel(16);
    let (updates, updates_receive) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(shellboy::frontend::run(
        control.clone(),
        actions_receive,
        updates,
        _directory.path().into(),
        owner,
        local,
    ));
    let accepted = start(&control).await;
    let mut ui = shellboy::ui::UiState::new(&control.store.config, _directory.path().into());
    for action in ui.select_thread(accepted.thread_id.clone()) {
        actions.send(action).await.unwrap();
    }
    let mut calls = vec![
        json!({"type":"function_call","id":"first","call_id":"first-call","name":"shell","arguments":json!({"command":"cat","background":false,"stdin_open":true}).to_string()}),
        json!({"type":"function_call","id":"second","call_id":"second-call","name":"shell","arguments":json!({"command":"printf must-not-run","background":false}).to_string()}),
    ];
    if !has_remaining_call {
        calls.pop();
    }
    incoming(&mut requests)
        .await
        .reply
        .send(Reply::json(
            json!({"id":"group","status":"completed","output":calls}),
        ))
        .ok()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if control.background(&accepted.thread_id).await.is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let next = incoming(&mut requests).await;
    let results: Vec<Value> = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        next.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["role"] == "user")
            .count(),
        1,
        "commentless backgrounding must not fabricate a user message"
    );
    let cancelled_count = usize::from(has_remaining_call);
    assert_eq!(results.len(), 1 + cancelled_count);
    assert_eq!(results[0]["reason"], "human_backgrounded");
    if has_remaining_call {
        assert_eq!(results[1]["status"], "cancelled");
    }
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT state FROM shell_execution", [], |r| r
                    .get::<_, String>(0))?,
                "running"
            );
            Ok(())
        })
        .await
        .unwrap();
    // This next model boundary is reached only after all group results and
    // the completion notice were published. No sleep can turn absence into success.
    let mut notices = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let shellboy::control::EngineEvent::Notice {
            thread_id,
            foreground_release,
            cancelled_calls,
        } = event
        {
            assert_eq!(thread_id, accepted.thread_id);
            assert_eq!(cancelled_calls, cancelled_count);
            assert_eq!(
                foreground_release,
                Some((
                    results[0]["execution_id"].as_str().unwrap().to_owned(),
                    shellboy::control::ForegroundRelease::Background
                ))
            );
            let remaining = if cancelled_calls == 0 {
                "後続の未実行呼出しはありません".to_owned()
            } else {
                format!("未実行のツール呼出しを{cancelled_calls}件取り消しました")
            };
            notices.push(format!(
                "人間の操作でshell {}の待機を解除し背景へ移しました。{remaining}",
                results[0]["execution_id"].as_str().unwrap()
            ));
        }
    }
    assert_eq!(
        notices.len(),
        1,
        "background release must notify even without later calls"
    );
    assert!(notices[0].contains("背景"));
    assert!(notices[0].contains(results[0]["execution_id"].as_str().unwrap()));
    if has_remaining_call {
        assert!(notices[0].contains("1件"));
    } else {
        assert!(notices[0].contains("後続の未実行呼出しはありません"));
        assert!(!notices[0].contains("取消") && !notices[0].contains("取り消"));
    }
    let ui_notice = timeout(Duration::from_secs(5), async {
        loop {
            match updates_receive.try_recv() {
                Ok(update @ shellboy::ui::UiUpdate::Status { .. }) => {
                    if let shellboy::ui::UiUpdate::Status {
                        thread_id: Some(ref target),
                        ref text,
                    } = update
                        && target == &accepted.thread_id
                        && text == &notices[0]
                    {
                        break update;
                    }
                }
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(error) => panic!("UI bridge stopped before background notice: {error}"),
            }
        }
    })
    .await
    .unwrap();
    ui.apply(ui_notice);
    assert_eq!(ui.view().status, notices[0]);
    next.reply
        .send(final_answer("background running"))
        .ok()
        .unwrap();
    state(
        &control,
        "turn",
        accepted.turn_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    command(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            agents: None,
            title: None,
            thread_id: Some(accepted.thread_id.clone()),
            cwd: None,
            connection: None,
            model: None,
            effort: Some("low".into()),
        }),
    )
    .await;
    assert!(
        timeout(Duration::from_millis(100), requests.recv())
            .await
            .is_err(),
        "settings must not wake background-only waiting"
    );
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM turn", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    actions
        .send(shellboy::ui::UiAction::Cancel {
            thread_id: accepted.thread_id.clone(),
        })
        .await
        .unwrap();
    state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "stopped",
    )
    .await;
    let stopped = timeout(Duration::from_secs(5), async {
        loop {
            match updates_receive.try_recv() {
                Ok(update @ shellboy::ui::UiUpdate::Progress { .. }) => {
                    if let shellboy::ui::UiUpdate::Progress {
                        ref thread_id,
                        ref state,
                        ..
                    } = update
                        && thread_id == &accepted.thread_id
                        && state == "中断済み"
                    {
                        break update;
                    }
                }
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(error) => panic!("UI bridge stopped before saved interruption: {error}"),
            }
        }
    })
    .await
    .unwrap();
    ui.apply(stopped);
    assert_eq!(ui.view().state, "中断済み");
    actions.send(shellboy::ui::UiAction::Quit).await.unwrap();
    timeout(Duration::from_secs(8), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(8), serving)
        .await
        .unwrap()
        .unwrap();
    proxy_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_attachment_and_deleted_draft_targets_leave_other_work_alive() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime(&url).await;
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let accepted = start(&control).await;
    let first = incoming(&mut requests).await;
    first.reply.send(tool("sleep 30", true)).ok().unwrap();
    // The next model request proves that the background shell has been admitted.
    let next = incoming(&mut requests).await;
    let execution: String = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT id FROM shell_execution WHERE state='running'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let response = control
        .request(Request::new(
            format!("{}:missing-attachment-target", control.store.instance_id),
            Command::AttachmentAdd(AttachmentAddArgs {
                thread_id: Some("typo-thread".into()),
                path: "fixture.txt".into(),
                kind: AttachmentKind::File,
            }),
        ))
        .await;
    assert_eq!(response.error.unwrap().code, ErrorCode::ThreadNotFound);
    let cli = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .arg("--data-dir")
        .arg(&owner.loaded.data_dir)
        .args([
            "attachment",
            "add",
            "--thread",
            "typo-thread",
            "--path",
            "fixture.txt",
            "--kind",
            "file",
        ])
        .output()
        .await
        .unwrap();
    assert!(!cli.status.success());
    let reply: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(reply["error"]["code"], "thread_not_found");
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    };
    let target = command(&control, Command::ThreadCreate(args.clone())).await;
    let target = target["thread_id"].as_str().unwrap().to_owned();
    command(
        &control,
        Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: target.clone(),
            descendants: false,
        }),
    )
    .await;
    let error = control
        .send_draft(
            format!("{}:deleted-draft-target", control.store.instance_id),
            args,
            Some(target.clone()),
            "retained draft".into(),
            vec![],
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ThreadNotFound);
    let error = control
        .edit_request(
            format!("{}:deleted-edit-target", control.store.instance_id),
            target,
            "old-node".into(),
            None,
            "retained edit".into(),
            vec![],
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ThreadNotFound);
    let shell = command(
        &control,
        Command::ShellGet(ExecutionTarget {
            execution_id: execution,
        }),
    )
    .await;
    assert_eq!(shell["state"], "running");
    let run = command(
        &control,
        Command::RunGet(RunTarget {
            run_id: accepted.run_id.unwrap(),
        }),
    )
    .await;
    assert_eq!(run["state"], "live");
    command(&control, Command::ThreadList(ThreadListArgs::default())).await;
    next.reply.send(final_answer("still alive")).ok().unwrap();
    local.shutdown().await.unwrap();
    serving.abort();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_shutdown_captures_real_shell_wait_once_and_replays_context_after_gc_restart() {
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(Reply::json(json!({"id":"shutdown-shell-response","status":"completed","output":[{"type":"function_call","call_id":"shutdown-shell","name":"shell","arguments":json!({"command":"printf SHUTDOWN_READY; read -r finish","background":false,"stdin_open":true}).to_string()}]})))
        .ok()
        .unwrap();
    let execution=timeout(Duration::from_secs(5),async { loop {
        let row=control.store.database.call(|db|{
            use rusqlite::OptionalExtension;
            Ok(db.query_row("SELECT s.id FROM shell_execution s WHERE s.state='running' AND EXISTS(SELECT 1 FROM output_chunk o WHERE o.execution_id=s.id)",[],|r|r.get::<_,String>(0)).optional()?)
        }).await.unwrap();
        if let Some(id)=row {break id;}tokio::task::yield_now().await;
    }}).await.unwrap();
    let operation = format!("{}:graceful-real-wait", control.store.instance_id);
    let wait = tokio::spawn({
        let control = control.clone();
        let execution = execution.clone();
        let operation = operation.clone();
        async move {
            control
                .request(Request::new(
                    operation,
                    Command::Wait(WaitArgs {
                        conditions: vec![WaitCondition::ShellEnded {
                            execution_id: execution,
                        }],
                        timeout_ms: 30_000,
                    }),
                ))
                .await
        }
    });
    let registered = timeout(Duration::from_secs(3), async {
        loop {
            let key = format!("wait.pending.{operation}");
            if control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                        [key],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if registered.is_err() {
        if wait.is_finished() {
            panic!("wait ended before registration: {:?}", wait.await.unwrap());
        }
        panic!("wait did not register before shutdown");
    }
    let old_root = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    let (first, second) = tokio::join!(control.prepare_shutdown(), control.prepare_shutdown());
    first.unwrap();
    second.unwrap();
    timeout(Duration::from_secs(8), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(wait.await.unwrap().ok);
    let target = accepted.thread_id.clone();
    let run = accepted.run_id.clone().unwrap();
    let op = operation.clone();
    let snapshot_nodes=control.store.database.call(move|db|{
        let root:i64=db.query_row("SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'",[],|r|r.get(0))?;assert!(root>old_root);
        assert_eq!(db.query_row("SELECT stop_reason FROM run WHERE id=?1",[run],|r|r.get::<_,String>(0))?,"app_shutdown");
        let body:String=db.query_row("SELECT payload_ref_json FROM event WHERE kind='context_changed' AND json_extract(payload_ref_json,'$.change.reason')='app_shutdown' AND target_thread_id=?1",[target],|r|r.get(0))?;
        let snapshot:Value=serde_json::from_str(&body).unwrap();assert_eq!(snapshot["actor"]["source"],"shutdown");assert_eq!(snapshot["change"]["shells"]["items"][0]["previous_state"],"running");assert_eq!(snapshot["change"]["shells"]["items"][0]["planned_state"],"stopped");assert_eq!(snapshot["change"]["cancelled_waits"]["items"][0]["operation_id"],op);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM event WHERE json_extract(payload_ref_json,'$.change.reason')='app_shutdown'",[],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM shell_execution WHERE state IN ('starting','running','stopping')",[],|r|r.get::<_,i64>(0))?,0);
        let raw:String=db.query_row("SELECT CAST(bytes AS TEXT) FROM output_chunk WHERE stream='stdout' ORDER BY sequence LIMIT 1",[],|r|r.get(0))?;assert!(raw.contains("SHUTDOWN_READY"));
        let nodes=db.prepare("SELECT id,payload_json FROM node WHERE kind IN ('interruption_context','interruption_details','interrupted_wait') ORDER BY id")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(nodes)
    }).await.unwrap();
    let compactor = shellboy::compaction::Compactor::new(
        control.store.database.clone(),
        control.store.config.clone(),
    );
    let spill = compactor.spill(&accepted.thread_id, true).await.unwrap();
    assert_eq!(
        spill.spilled_groups, 1,
        "audit nodes must not split one call/result group"
    );
    assert!(
        spill
            .original_node_ids
            .iter()
            .any(|id| snapshot_nodes.iter().any(|(source, _)| source == id)),
        "interleaved audit sources remain reachable through replacement provenance"
    );
    let job = "shutdown-gc".to_owned();
    control.store.database.call(|db| {db.execute("INSERT INTO gc_job(id,kind,root_revision,state)VALUES('shutdown-gc','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),'pending')",[])?;Ok(())}).await.unwrap();
    control.gc.start_history().await.unwrap().unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(
        Some(directory.path().join("config.toml")),
        Some(directory.path().join("data")),
        directory.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, directory.path().into());
    // Resume a saved GC job after restart; captured sources remain rooted,
    // and startup must not duplicate the completed graceful capture.
    control.gc.history_step(&job).await.unwrap();
    control
        .gc
        .collect(StorageCategory::History, chrono::Utc::now())
        .await
        .unwrap();
    control.store.database.call(move|db|{for (id,body) in snapshot_nodes {assert_eq!(db.query_row("SELECT payload_json FROM node WHERE id=?1",[id],|r|r.get::<_,String>(0))?,body);}assert_eq!(db.query_row("SELECT COUNT(*) FROM event WHERE json_extract(payload_ref_json,'$.change.reason')='app_shutdown'",[],|r|r.get::<_,i64>(0))?,1);Ok(())}).await.unwrap();
    command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some(accepted.thread_id.clone()),
            text: "explicit next input after shutdown".into(),
        }),
    )
    .await;
    let next = incoming(&mut requests).await;
    let input = next.body.to_string();
    assert!(input.contains("app_shutdown") && input.contains(&operation));
    assert!(input.contains(&execution));
    next.reply
        .send(final_answer("context restored"))
        .ok()
        .unwrap();
    timeout(Duration::from_secs(5),async{loop {let target=accepted.thread_id.clone();if control.store.database.call(move|db|Ok(db.query_row("SELECT NOT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state IN ('live','stopping'))",[target],|r|r.get::<_,bool>(0))?)).await.unwrap(){break;}tokio::task::yield_now().await;}}).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_context_revision_does_not_adopt_a_late_model_response() {
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    let accepted = start(&control).await;
    // The real driver has admitted an inflight attempt and reached HTTP. Hold
    // the response until capture changes the selected path and cancels it.
    let pending = incoming(&mut requests).await;
    control.prepare_shutdown().await.unwrap();
    let _ = pending.reply.send(final_answer("LATE_SHUTDOWN_ANSWER"));
    timeout(Duration::from_secs(8), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    let turn = accepted.turn_id.clone().unwrap();
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT state FROM model_attempt WHERE turn_id=?1",
                    [&turn],
                    |r| r.get::<_, String>(0)
                )?,
                "cancelled"
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM node WHERE payload_json LIKE '%LATE_SHUTDOWN_ANSWER%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row("SELECT state FROM turn WHERE id=?1", [turn], |r| r
                    .get::<_, String>(0))?,
                "stopped"
            );
            let body: String = db.query_row(
                "SELECT payload_json FROM node WHERE kind='interruption_context'",
                [],
                |r| r.get(0),
            )?;
            let wrapper: Value = serde_json::from_str(&body).unwrap();
            let value: Value = serde_json::from_str(wrapper["text"].as_str().unwrap()).unwrap();
            assert_eq!(value["change"]["result_uncertain"], true);
            assert_eq!(value["change"]["automatic_replay"], false);
            Ok(())
        })
        .await
        .unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(
        Some(directory.path().join("config.toml")),
        Some(directory.path().join("data")),
        directory.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, directory.path().into());
    assert!(
        requests.try_recv().is_err(),
        "restart must not replay the interrupted request"
    );
    command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some(accepted.thread_id.clone()),
            text: "new explicit input".into(),
        }),
    )
    .await;
    let next = incoming(&mut requests).await;
    let input = next.body.to_string();
    assert!(input.contains("app_shutdown") && input.contains("result_uncertain"));
    assert!(!input.contains("LATE_SHUTDOWN_ANSWER"));
    next.reply.send(final_answer("fresh answer")).ok().unwrap();
    state(
        &control,
        "run",
        &control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT id FROM run WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",
                    [accepted.thread_id],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap(),
        "completed",
    )
    .await;
    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_foreground_wait_is_durable_and_human_backgrounding_releases_only_the_turn() {
    for background in [false, true] {
        let (url, mut requests, server) = proxy().await;
        let (_directory, owner, control) = runtime(&url).await;
        let (send, receive) = mpsc::channel(32);
        let local = shellboy::ipc::LocalServer::bind(&owner, send)
            .await
            .unwrap();
        let serving = tokio::spawn(control.clone().serve(receive));
        let accepted = start(&control).await;
        incoming(&mut requests).await.reply.send(Reply::json(json!({
            "id":"target-response","status":"completed","output":[{
                "type":"function_call","id":"target-item","call_id":"target-call","name":"shell",
                "arguments":json!({"command":"read -r finish; printf TARGET_DONE","background":true,"stdin_open":true}).to_string()
            }]
        }))).ok().unwrap();
        let continuation = incoming(&mut requests).await;
        let target: Value = continuation.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
            .unwrap();
        let target = target["execution_id"].as_str().unwrap().to_owned();
        let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
        let script = format!(
            "'{executable}' wait --conditions-json '[{{\"type\":\"shell_ended\",\"execution_id\":\"{target}\"}}]' --timeout-ms 30000; printf WAIT_RELEASED; read -r finish"
        );
        continuation.reply.send(Reply::json(json!({
            "id":"wait-response","status":"completed","output":[{
                "type":"function_call","id":"wait-item","call_id":"wait-call","name":"shell",
                "arguments":json!({"command":script,"background":false,"stdin_open":true}).to_string()
            }]
        }))).ok().unwrap();
        let turn = accepted.turn_id.as_deref().unwrap();
        state(&control, "turn", turn, "waiting").await;
        let execution: String = control
            .store
            .database
            .call(|db| {
                Ok(db.query_row(
                    "SELECT operation_id FROM tool_call WHERE call_id='wait-call'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        control.store.database.call(|db| {
            assert_eq!(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%' AND json_type(value,'$.turn_wait_generation')='integer'",[],|r|r.get::<_,i64>(0))?,1);
            Ok(())
        }).await.unwrap();
        if background {
            control.background(&accepted.thread_id).await.unwrap();
            let next = incoming(&mut requests).await;
            state(&control, "turn", turn, "running").await;
            control
                .store
                .database
                .call(|db| {
                    assert_eq!(
                        db.query_row(
                            "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                            [],
                            |r| r.get::<_, i64>(0)
                        )?,
                        1
                    );
                    Ok(())
                })
                .await
                .unwrap();
            next.reply
                .send(final_answer("waiting shell continues in background"))
                .ok()
                .unwrap();
            state(&control, "turn", turn, "completed").await;
            command(
                &control,
                Command::RunStop(RunTarget {
                    run_id: accepted.run_id.clone().unwrap(),
                }),
            )
            .await;
            state(
                &control,
                "run",
                accepted.run_id.as_deref().unwrap(),
                "stopped",
            )
            .await;
        } else {
            command(
                &control,
                Command::ShellWrite(ShellWriteArgs {
                    execution_id: target,
                    text: "finish\n".into(),
                    encoding: "UTF-8".into(),
                }),
            )
            .await;
            state(&control, "turn", turn, "running").await;
            command(
                &control,
                Command::ShellWrite(ShellWriteArgs {
                    execution_id: execution,
                    text: "finish\n".into(),
                    encoding: "UTF-8".into(),
                }),
            )
            .await;
            incoming(&mut requests)
                .await
                .reply
                .send(final_answer("wait completed"))
                .ok()
                .unwrap();
            state(
                &control,
                "run",
                accepted.run_id.as_deref().unwrap(),
                "completed",
            )
            .await;
        }
        control
            .store
            .database
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        local.shutdown().await.unwrap();
        serving.await.unwrap();
        close(control, owner, server).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_stream_enters_durable_recovery_and_preserves_received_text_without_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (closed, disconnected) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _request = read_request(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let event = format!(
            "data: {}\n\n",
            json!({"type":"response.output_text.delta","delta":"timeout text must remain available"})
        );
        stream.write_all(event.as_bytes()).await.unwrap();
        // Keep the HTTP response open: recovery must come from receive
        // inactivity, rather than EOF or an explicit incomplete event.
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        let _ = closed.send(());
    });
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(&config, format!("default_connection='test'\ndefault_model='fixture'\n[connections.test]\nbase_url='{url}'\nresponse_retry_partial=false\nstream_idle_timeout_ms=1000\n")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(directory.path().join("data")),
        directory.path().to_path_buf(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, directory.path().to_path_buf());
    let accepted = start(&control).await;
    let turn = accepted.turn_id.clone().unwrap();
    state(&control, "turn", &turn, "recovery_wait").await;
    timeout(Duration::from_secs(3), disconnected)
        .await
        .unwrap()
        .unwrap();
    control
        .store
        .database
        .call(move |db| {
            let reason: String = db.query_row(
                "SELECT recovery_reason_json FROM turn WHERE id=?1",
                [&turn],
                |row| row.get(0),
            )?;
            let reason: Value = serde_json::from_str(&reason).unwrap();
            assert!(reason.to_string().contains("timeout"));
            assert!(
                reason
                    .to_string()
                    .contains("connections.test.stream_idle_timeout_ms")
            );
            let (attempt_state, error): (String, String) = db.query_row(
                "SELECT state,error_json FROM model_attempt WHERE turn_id=?1",
                [&turn],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert!(matches!(attempt_state.as_str(), "failed" | "incomplete"));
            assert!(error.contains("timeout text must remain available"));
            assert!(error.contains("timeout"));
            let received: String = db.query_row(
                "SELECT payload_json FROM node WHERE turn_id=?1 AND kind='incomplete_response'",
                [&turn],
                |row| row.get(0),
            )?;
            assert!(received.contains("timeout text must remain available"));
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM model_attempt WHERE turn_id=?1",
                    [&turn],
                    |row| row.get::<_, i64>(0)
                )?,
                1,
                "timeout must not automatically resend the conversation"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    close(control, owner, server).await;
}
