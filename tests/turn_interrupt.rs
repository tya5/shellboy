//! Real local HTTP + SQLite + shell vertical paths; no production credentials.
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    control::{Control, EngineEvent},
    protocol::*,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};

struct Reply {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
    keep_open: bool,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            kind: "application/json",
            body: value.to_string().into_bytes(),
            keep_open: false,
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
                let head = if reply.keep_open {
                    format!(
                        "HTTP/1.1 {} Fixture\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                        reply.status, reply.kind
                    )
                } else {
                    format!(
                        "HTTP/1.1 {} Fixture\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.status,
                        reply.kind,
                        reply.body.len()
                    )
                };
                if stream.write_all(head.as_bytes()).await.is_ok() {
                    let _ = stream.write_all(&reply.body).await;
                    if reply.keep_open {
                        let mut byte = [0u8; 1];
                        let _ = stream.read(&mut byte).await;
                    }
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
                thread_id: Some(thread["thread_id"].as_str().unwrap().into()),
                text: "fixture instruction".into(),
                now: false,
            }),
        )
        .await,
    )
    .unwrap()
}
async fn incoming(receiver: &mut mpsc::UnboundedReceiver<Incoming>) -> Incoming {
    crate::test_watchdog::watchdog("tests/turn_interrupt.rs: incoming", receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

// Every runtime state observed below publishes Changed after its DB commit.
// Subscribe before reading so an update between the read and wait is retained.
async fn changed(events: &mut broadcast::Receiver<EngineEvent>) {
    loop {
        match events.recv().await {
            Ok(EngineEvent::Changed { .. }) | Err(broadcast::error::RecvError::Lagged(_)) => return,
            Ok(_) => {}
            Err(broadcast::error::RecvError::Closed) => {
                panic!("owner event stream closed before expected state")
            }
        }
    }
}

async fn saved_state(control: &Control, table: &str, id: &str, wanted: &str) {
    let table = table.to_owned();
    let id = id.to_owned();
    let mut changes = control.subscribe();
    crate::test_watchdog::watchdog("tests/turn_interrupt.rs: saved_state", async {
        loop {
            let table = table.clone();
            let id = id.clone();
            let state = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        &format!("SELECT state FROM {table} WHERE id=?1"),
                        [id],
                        |r| r.get::<_, String>(0),
                    )?)
                })
                .await
                .unwrap();
            if state == wanted {
                break;
            }
            changed(&mut changes).await;
        }
    })
    .await
    .unwrap();
}
async fn close(control: Arc<Control>, owner: DataOwner, server: JoinHandle<()>) {
    crate::test_watchdog::watchdog("tests/turn_interrupt.rs: close", control.shutdown())
        .await
        .unwrap()
        .unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_stream_is_saved_without_retry_and_empty_run_completes() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime_with_io(&url, "").await;
    let mut events = control.subscribe();
    let accepted = start(&control).await;
    incoming(&mut requests)
        .await
        .reply
        .send(Reply {
            status: 200,
            kind: "text/event-stream",
            body: format!(
                "data: {}\n\n",
                json!({"type":"response.output_text.delta","delta":"saved partial output"})
            )
            .into_bytes(),
            keep_open: true,
        })
        .ok()
        .unwrap();
    crate::test_watchdog::watchdog("tests/turn_interrupt.rs: interrupted_stream_is_saved_without_retry_and_empty_run_completes", async {
        loop {
            if let shellboy::control::EngineEvent::Stream { text, .. } =
                events.recv().await.unwrap()
                && text.contains("saved partial")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let result = control
        .interrupt_turn(
            accepted.thread_id.clone(),
            accepted.turn_id.clone().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result["requested"], true);
    saved_state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    let turn = accepted.turn_id.unwrap();
    control.store.database.call(move|db|{
        let(state,generation,at):(String,i64,Option<String>)=db.query_row("SELECT state,generation,interrupt_requested_at FROM turn WHERE id=?1",[&turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(state,"stopped");assert_eq!(generation,1);assert!(at.is_some());
        assert_eq!(db.query_row("SELECT COUNT(*) FROM model_attempt WHERE turn_id=?1 AND state='cancelled'",[&turn],|r|r.get::<_,i64>(0))?,1);
        let audit:String=db.query_row("SELECT payload_json FROM node WHERE turn_id=?1 AND kind='incomplete_response'",[&turn],|r|r.get(0))?;
        assert!(audit.contains("saved partial output"));
        assert_eq!(db.query_row("SELECT COUNT(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE n.kind='incomplete_response'",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM event WHERE id=?1",[format!("turn-interrupted-{turn}")],|r|r.get::<_,i64>(0))?,1);
        Ok(())
    }).await.unwrap();
    assert!(
        requests.try_recv().is_err(),
        "human interruption must not retry or wake from its marker alone"
    );
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_foreground_keeps_owned_work_and_carries_queued_input_in_same_run() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime_with_io(&url, "").await;
    let accepted = start(&control).await;
    incoming(&mut requests).await.reply.send(Reply::json(json!({"id":"group","status":"completed","output":[
        {"type":"function_call","id":"background","call_id":"background-call","name":"shell","arguments":json!({"command":if cfg!(windows){"[Console]::In.ReadToEnd()"}else{"cat"},"background":true,"stdin_open":true}).to_string()},
        {"type":"function_call","id":"foreground","call_id":"foreground-call","name":"shell","arguments":json!({"command":if cfg!(windows){"[Console]::In.ReadToEnd()"}else{"cat"},"background":false,"stdin_open":true}).to_string()},
        {"type":"function_call","id":"later","call_id":"later-call","name":"shell","arguments":json!({"command":if cfg!(windows){"[Console]::Out.Write('MUST_NOT_RUN')"}else{"printf MUST_NOT_RUN"}}).to_string()}
    ]}))).ok().unwrap();
    let mut changes = control.subscribe();
    crate::test_watchdog::watchdog("tests/turn_interrupt.rs: interrupted_foreground_keeps_owned_work_and_carries_queued_input_in_same_run", async {
        loop {
            let count = control
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT count(*) FROM shell_execution WHERE state='running'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap();
            if count == 2 {
                break;
            }
            changed(&mut changes).await;
        }
    })
    .await
    .unwrap();
    let old_run = accepted.run_id.clone().unwrap();
    let child_owner = old_run.clone();
    // A live owned child record is an explicit synthetic ownership fixture;
    // this does not claim a native child process was exercised.
    control.store.database.call(move|db|{
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('owned-child','/','/AGENTS.md','test','fixture','now')",[])?;
        db.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES('owned-child-run','owned-child',?1,'live','now')",[child_owner])?;Ok(())
    }).await.unwrap();
    command(
        &control,
        Command::RevisitSet(RevisitSetArgs {
            run_id: Some(old_run.clone()),
            text: "must not fire".into(),
        }),
    )
    .await;
    let queued = command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            thread_id: Some(accepted.thread_id.clone()),
            text: "new direction after interruption".into(),
            now: false,
        }),
    )
    .await;
    let old_turn = accepted.turn_id.clone().unwrap();
    let result = control
        .interrupt_turn(accepted.thread_id.clone(), old_turn.clone())
        .await
        .unwrap();
    assert_eq!(result["requested"], true);
    let next = incoming(&mut requests).await;
    let input = next.body["input"].as_array().unwrap();
    let outputs = input
        .iter()
        .filter(|i| i["type"] == "function_call_output")
        .map(|i| {
            (
                i["call_id"].as_str().unwrap(),
                serde_json::from_str::<Value>(i["output"].as_str().unwrap()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    let foreground = &outputs
        .iter()
        .find(|(call, _)| *call == "foreground-call")
        .unwrap()
        .1;
    assert_eq!(foreground["status"], "running");
    assert_eq!(foreground["reason"], "human_turn_interrupted");
    let later = &outputs
        .iter()
        .find(|(call, _)| *call == "later-call")
        .unwrap()
        .1;
    assert_eq!(later["status"], "cancelled");
    assert_eq!(later["executed"], false);
    assert_eq!(later["reason"], "previous_turn_interrupted");
    assert_eq!(
        later["prerequisite"]["execution_id"],
        foreground["execution_id"]
    );
    let marker = input
        .iter()
        .position(|i| i["role"] == "developer" && i.to_string().contains("turn_interrupted"))
        .unwrap();
    let direction = input
        .iter()
        .position(|i| {
            i["role"] == "user" && i.to_string().contains("new direction after interruption")
        })
        .unwrap();
    assert!(
        marker < direction,
        "the interruption fact must precede carried input"
    );
    let queued_id = queued["message_id"].as_str().unwrap().to_owned();
    control.store.database.call(move|db|{
        assert_eq!(db.query_row("SELECT count(*) FROM shell_execution WHERE owner_run_id=?1 AND state='running'",[&old_run],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(db.query_row("SELECT state FROM run WHERE id='owned-child-run'",[],|r|r.get::<_,String>(0))?,"live");
        assert_eq!(db.query_row("SELECT count(*) FROM tool_call WHERE call_id='later-call' AND operation_id IS NOT NULL",[],|r|r.get::<_,i64>(0))?,0);
        let current:(String,String)=db.query_row("SELECT id,run_id FROM turn WHERE state='running' AND run_id=?1",[&old_run],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_ne!(current.0,old_turn);assert_eq!(current.1,old_run);
        let receipt:(String,String,String)=db.query_row("SELECT run_id,turn_id,state FROM receipt WHERE message_node_id=?1",[&queued_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(receipt,(old_run.clone(),old_turn.clone(),"applied".into()));
        let payload:String=db.query_row("SELECT payload_ref_json FROM event WHERE id=?1",[format!("turn-interrupted-{old_turn}")],|r|r.get(0))?;
        let marker:Value=serde_json::from_str(&payload).unwrap();
        assert_eq!(marker["change"]["new_turn_id"],current.0);
        assert_eq!(marker["change"]["carried_count"],1);
        assert_eq!(marker["change"]["carried_sources_truncated"],false);
        assert_eq!(marker["change"]["carried_sources"][0]["message_id"],queued_id);
        assert_eq!(marker["change"]["carried_sources"][0]["original_turn_id"],old_turn);
        let delivery:String=db.query_row("SELECT result_json FROM receipt WHERE message_node_id=?1",[&queued_id],|r|r.get(0))?;
        assert_eq!(serde_json::from_str::<Value>(&delivery).unwrap()["delivery_context"]["turn_id"],current.0);
        let revisit:Option<String>=db.query_row("SELECT value FROM app_meta WHERE key=?1",[format!("revisit.{old_run}")],|r|r.get(0)).optional()?;
        assert!(revisit.is_none_or(|v|serde_json::from_str::<Value>(&v).unwrap()["pending"].is_null()));
        Ok(())
    }).await.unwrap();
    close(control, owner, server).await;
    drop(next);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_turn_interrupt_is_idempotent_and_stale_target_cannot_stop_replacement() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime_with_io(&url, "").await;
    let accepted = start(&control).await;
    let mut refusal =
        Reply::json(json!({"error":{"code":"invalid_request","message":"synthetic refusal"}}));
    refusal.status = 400;
    incoming(&mut requests)
        .await
        .reply
        .send(refusal)
        .ok()
        .unwrap();
    let old_turn = accepted.turn_id.clone().unwrap();
    saved_state(&control, "turn", &old_turn, "recovery_wait").await;
    let first = control
        .interrupt_turn(accepted.thread_id.clone(), old_turn.clone())
        .await
        .unwrap();
    assert_eq!(first["requested"], true);
    let _ = control
        .interrupt_turn(accepted.thread_id.clone(), old_turn.clone())
        .await
        .unwrap();
    saved_state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            thread_id: Some(accepted.thread_id.clone()),
            text: "a replacement turn".into(),
            now: false,
        }),
    )
    .await;
    let next = incoming(&mut requests).await;
    let stale = control
        .interrupt_turn(accepted.thread_id.clone(), old_turn.clone())
        .await
        .unwrap();
    assert_eq!(stale["requested"], false);
    let thread = accepted.thread_id;
    control.store.database.call(move|db|{
        assert_eq!(db.query_row("SELECT generation FROM turn WHERE id=?1",[old_turn],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT count(*) FROM turn WHERE thread_id=?1 AND state='running' AND interrupt_requested_at IS NULL",[thread],|r|r.get::<_,i64>(0))?,1);Ok(())
    }).await.unwrap();
    close(control, owner, server).await;
    drop(next);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn later_stream_interruption_is_not_misclassified_by_an_earlier_completed_tool_group() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime_with_io(&url, "").await;
    let mut events = control.subscribe();
    let accepted = start(&control).await;
    incoming(&mut requests).await.reply.send(Reply::json(json!({
        "id":"earlier-group", "status":"completed", "output":[{
            "type":"function_call", "id":"earlier-item", "call_id":"earlier-call", "name":"shell",
            "arguments":json!({"command":"exit 0","background":false}).to_string()
        }]
    }))).ok().unwrap();
    let next = incoming(&mut requests).await;
    let real_result = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "earlier-call")
        .unwrap();
    let real_result: Value = serde_json::from_str(real_result["output"].as_str().unwrap()).unwrap();
    assert_eq!(real_result["status"], "exited");
    assert_eq!(real_result["exit_code"], 0);
    next.reply
        .send(Reply {
            status: 200,
            kind: "text/event-stream",
            keep_open: true,
            body: format!(
                "data: {}\n\n",
                json!({"type":"response.output_text.delta","delta":"later response partial"})
            )
            .into_bytes(),
        })
        .ok()
        .unwrap();
    crate::test_watchdog::watchdog("tests/turn_interrupt.rs: later_stream_interruption_is_not_misclassified_by_an_earlier_completed_tool_group", async {
        loop {
            if let shellboy::control::EngineEvent::Stream { text, .. } =
                events.recv().await.unwrap()
                && text.contains("later response partial")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    control
        .interrupt_turn(
            accepted.thread_id.clone(),
            accepted.turn_id.clone().unwrap(),
        )
        .await
        .unwrap();
    saved_state(
        &control,
        "run",
        accepted.run_id.as_deref().unwrap(),
        "completed",
    )
    .await;
    let turn = accepted.turn_id.unwrap();
    control.store.database.call(move |db| {
        let marker:String=db.query_row("SELECT payload_ref_json FROM event WHERE id=?1",[format!("turn-interrupted-{turn}")],|r|r.get(0))?;
        let marker:Value=serde_json::from_str(&marker).unwrap();
        assert_eq!(marker["change"]["phase"],"model_attempt_interrupted");
        assert_eq!(marker["change"]["had_adopted_groups"],true);
        let attempt=marker["change"]["interrupted_attempt_id"].as_str().unwrap();
        assert_eq!(db.query_row("SELECT state FROM model_attempt WHERE id=?1",[attempt],|r|r.get::<_,String>(0))?,"cancelled");
        assert!(marker["change"]["meaning"].as_str().unwrap().contains("absent from model history"));
        let audit:String=db.query_row("SELECT payload_json FROM node WHERE turn_id=?1 AND kind='incomplete_response'",[turn],|r|r.get(0))?;
        assert!(audit.contains("later response partial"));
        let prior:String=db.query_row("SELECT n.payload_json FROM tool_result r JOIN node n ON n.id=r.node_id WHERE r.call_id='earlier-call'",[],|r|r.get(0))?;
        let prior:Value=serde_json::from_str(&prior).unwrap();
        let prior:Value=serde_json::from_str(prior["item"]["output"].as_str().unwrap()).unwrap();
        assert_eq!(prior["status"],"exited");assert_eq!(prior["exit_code"],0);
        Ok(())
    }).await.unwrap();
    assert!(
        requests.try_recv().is_err(),
        "a past tool group and the marker must not restart interrupted generation"
    );
    close(control, owner, server).await;
}

fn completed_answer() -> Reply {
    Reply::json(json!({"id":"final-answer","status":"completed","output":[{
        "id":"final-item","type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":"work completed"}]
    }]}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_completion_after_interruption_starts_new_turn_and_completes_same_run() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime_with_io(&url, "").await;
    let mut events = control.subscribe();
    let accepted = start(&control).await;
    incoming(&mut requests).await.reply.send(Reply::json(json!({
        "id":"background-group","status":"completed","output":[{
            "type":"function_call","id":"background-item","call_id":"background-call","name":"shell",
            "arguments":json!({"command":if cfg!(windows){"[Console]::In.ReadToEnd()"}else{"cat"},"background":true,"stdin_open":true}).to_string()
        }]
    }))).ok().unwrap();
    let stream = incoming(&mut requests).await;
    let launched = stream.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    let launched: Value = serde_json::from_str(launched["output"].as_str().unwrap()).unwrap();
    let execution = launched["execution_id"].as_str().unwrap().to_owned();
    assert_eq!(launched["status"], "running");
    stream.reply.send(Reply{status:200,kind:"text/event-stream",keep_open:true,
        body:format!("data: {}\n\n",json!({"type":"response.output_text.delta","delta":"interrupted while background work continues"})).into_bytes(),
    }).ok().unwrap();
    crate::test_watchdog::watchdog("tests/turn_interrupt.rs: background_completion_after_interruption_starts_new_turn_and_completes_same_run", async {
        loop {
            if let shellboy::control::EngineEvent::Stream { text, .. } =
                events.recv().await.unwrap()
                && text.contains("background work continues")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let run = accepted.run_id.clone().unwrap();
    let old_turn = accepted.turn_id.clone().unwrap();
    command(
        &control,
        Command::RevisitSet(RevisitSetArgs {
            run_id: Some(run.clone()),
            text: "must not automatically resume".into(),
        }),
    )
    .await;
    control
        .interrupt_turn(accepted.thread_id.clone(), old_turn.clone())
        .await
        .unwrap();
    saved_state(&control, "turn", &old_turn, "stopped").await;
    let saved_run = run.clone();
    let saved_execution = execution.clone();
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT state FROM run WHERE id=?1", [saved_run], |r| r
                    .get::<_, String>(
                    0
                ))?,
                "live"
            );
            assert_eq!(
                db.query_row(
                    "SELECT state FROM shell_execution WHERE id=?1",
                    [saved_execution],
                    |r| r.get::<_, String>(0)
                )?,
                "running"
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        requests.try_recv().is_err(),
        "the interruption marker alone cannot start a model request"
    );
    command(
        &control,
        Command::ShellEof(ExecutionTarget {
            execution_id: execution.clone(),
        }),
    )
    .await;
    let completion = incoming(&mut requests).await;
    let input = completion.body["input"].as_array().unwrap();
    assert!(
        input.iter().any(|item| item["role"] == "developer"
            && item.to_string().contains("shell_ended")
            && item.to_string().contains(&execution)),
        "the real background completion must cause the next request"
    );
    assert_eq!(
        input
            .iter()
            .filter(
                |item| item["role"] == "developer" && item.to_string().contains("turn_interrupted")
            )
            .count(),
        1
    );
    let saved_run = run.clone();
    let saved_old = old_turn.clone();
    control.store.database.call(move|db|{
        let current:String=db.query_row("SELECT id FROM turn WHERE run_id=?1 AND state='running'",[&saved_run],|r|r.get(0))?;
        assert_ne!(current,saved_old);
        assert_eq!(db.query_row("SELECT count(*) FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.run_id=?1",[saved_run],|r|r.get::<_,i64>(0))?,3);
        Ok(())
    }).await.unwrap();
    completion.reply.send(completed_answer()).ok().unwrap();
    saved_state(&control, "run", &run, "completed").await;
    saved_state(&control, "shell_execution", &execution, "exited").await;
    control.store.database.call(move|db|{
        assert_eq!(db.query_row("SELECT count(*) FROM turn WHERE run_id=?1",[&run],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(db.query_row("SELECT count(*) FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.run_id=?1",[&run],|r|r.get::<_,i64>(0))?,3);
        assert_eq!(db.query_row("SELECT count(*) FROM event WHERE kind='revisit' AND target_run_id=?1",[run],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    assert!(requests.try_recv().is_err());
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_queued_input_keeps_foreground_group_intact_without_extra_model_requests() {
    let (url, mut requests, server) = proxy().await;
    let (_directory, owner, control) = runtime_with_io(&url, "").await;
    let accepted = start(&control).await;
    incoming(&mut requests).await.reply.send(Reply::json(json!({
        "id":"serial-group","status":"completed","output":[
            {"type":"function_call","id":"first-item","call_id":"first-call","name":"shell","arguments":json!({"command":if cfg!(windows){"[Console]::In.ReadToEnd()"}else{"cat"},"background":false,"stdin_open":true}).to_string()},
            {"type":"function_call","id":"second-item","call_id":"second-call","name":"shell","arguments":json!({"command":if cfg!(windows){"[Console]::Out.Write('SECOND')"}else{"printf SECOND"},"background":false}).to_string()}
        ]
    }))).ok().unwrap();
    let mut changes = control.subscribe();
    let execution=crate::test_watchdog::watchdog("tests/turn_interrupt.rs: ordinary_queued_input_keeps_foreground_group_intact_without_extra_model_requests",async{loop{
        let execution=control.store.database.call(|db|Ok(db.query_row("SELECT id FROM shell_execution WHERE state='running' ORDER BY rowid LIMIT 1",[],|r|r.get::<_,String>(0)).optional()?)).await.unwrap();
        if let Some(execution)=execution{break execution;}changed(&mut changes).await;
    }}).await.unwrap();
    let queued = command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            thread_id: Some(accepted.thread_id.clone()),
            text: "ordinary report for the next boundary".into(),
            now: false,
        }),
    )
    .await;
    let message = queued["message_id"].as_str().unwrap().to_owned();
    let saved_message = message.clone();
    control.store.database.call(move|db|{
        let facts:(String,String,bool)=db.query_row("SELECT r.state,r.path_admission,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id) FROM receipt r WHERE r.message_node_id=?1",[saved_message],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(facts,("accepted".into(),"unadmitted".into(),false));
        assert_eq!(db.query_row("SELECT count(*) FROM tool_result",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT count(*) FROM model_attempt",[],|r|r.get::<_,i64>(0))?,1);
        Ok(())
    }).await.unwrap();
    assert!(
        requests.try_recv().is_err(),
        "ordinary reports do not release the foreground wait"
    );
    command(
        &control,
        Command::ShellEof(ExecutionTarget {
            execution_id: execution,
        }),
    )
    .await;
    let next = incoming(&mut requests).await;
    let input = next.body["input"].as_array().unwrap();
    let outputs = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    for result in &outputs {
        assert_eq!(result["status"], "exited");
        assert_eq!(result["exit_code"], 0);
        assert!(result.get("cancel_remaining_calls").is_none());
    }
    assert_eq!(outputs[1]["output"], "SECOND");
    let report = input
        .iter()
        .position(|item| {
            item["role"] == "user"
                && item
                    .to_string()
                    .contains("ordinary report for the next boundary")
        })
        .unwrap();
    let last_result = input
        .iter()
        .rposition(|item| item["type"] == "function_call_output")
        .unwrap();
    assert!(
        last_result < report,
        "queued report is admitted after the complete tool group"
    );
    next.reply.send(completed_answer()).ok().unwrap();
    let run = accepted.run_id.unwrap();
    saved_state(&control, "run", &run, "completed").await;
    control.store.database.call(move|db|{
        assert_eq!(db.query_row("SELECT count(*) FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.run_id=?1",[&run],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(db.query_row("SELECT count(*) FROM shell_execution WHERE owner_run_id=?1",[run],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(db.query_row("SELECT state FROM receipt WHERE message_node_id=?1",[message],|r|r.get::<_,String>(0))?,"applied");
        Ok(())
    }).await.unwrap();
    assert!(requests.try_recv().is_err());
    close(control, owner, server).await;
}
