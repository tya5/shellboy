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
};

static FIXTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
async fn owner_fixture(url: &str) -> (tempfile::TempDir, DataOwner) {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(&config,format!("default_connection='test'\ndefault_model='fixture'\n[connections.test]\nbase_url='{url}'\n[io]\nterm_grace_ms=100\ndrain_timeout_ms=100\nself_reply_grace_ms=10\nshutdown_grace_ms=2000\n")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(directory.path().join("data")),
        directory.path().to_path_buf(),
    )
    .await
    .unwrap();
    (directory, owner)
}
async fn runtime(url: &str) -> (tempfile::TempDir, DataOwner, Arc<Control>) {
    let (directory, owner) = owner_fixture(url).await;
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
    crate::test_watchdog::watchdog("tests/runtime_hierarchy.rs: incoming", receiver.recv())
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
        json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"function_call","id":uuid::Uuid::new_v4().to_string(),"call_id":uuid::Uuid::new_v4().to_string(),"name":"shell","arguments":json!({"command":command,"background":background}).to_string()}]}),
    )
}
async fn state(control: &Arc<Control>, table: &str, id: &str, wanted: &str) {
    let table = table.to_owned();
    let id = id.to_owned();
    let wanted = wanted.to_owned();
    crate::test_watchdog::watchdog("tests/runtime_hierarchy.rs: state", async {
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
    .await
    .unwrap();
}
async fn close(control: Arc<Control>, owner: DataOwner, server: JoinHandle<()>) {
    crate::test_watchdog::watchdog("tests/runtime_hierarchy.rs: close", control.shutdown())
        .await
        .unwrap()
        .unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

async fn full_history(owner: &DataOwner, node: &str) -> String {
    let mut cursor = None;
    let mut text = String::new();
    loop {
        let body = shellboy::history::History::new(owner.db.clone())
            .get(HistoryGetArgs {
                final_answer_only: false,
                node_id: node.into(),
                limit_bytes: 65536,
                cursor,
                encoding: "UTF-8".into(),
            })
            .await
            .unwrap();
        text.push_str(&body.text);
        cursor = body.next_cursor;
        if cursor.is_none() {
            return text;
        }
    }
}
fn request_thread(body: &Value) -> String {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .find_map(|part| {
            part["text"]
                .as_str()
                .and_then(|text| text.strip_prefix("Shellboy context v1: thread="))
                .map(|text| text.split_whitespace().next().unwrap().to_owned())
        })
        .unwrap()
}
async fn child_thread(control: &Arc<Control>, parent: &str) -> String {
    command(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_id: Some(parent.into()),
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn process_started(path: &std::path::Path) -> i32 {
    crate::test_watchdog::watchdog("tests/runtime_hierarchy.rs: process_started", async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_parent_reaps_real_grandchildren_but_keeps_independent_run_and_rejects_late_answers()
 {
    let _fixture = FIXTURE.lock().await;
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let parent = start(&control).await;
    let first = incoming(&mut requests).await;
    let child = child_thread(&control, &parent.thread_id).await;
    let grandchild = child_thread(&control, &child).await;
    let independent = start(&control).await;
    let independent_first = incoming(&mut requests).await;
    let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
    let path = |name: &str| directory.path().join(format!("{name}.pid"));
    let sleep = |name: &str| format!("printf '%s' $$ > '{}'; exec sleep 60", path(name).display());
    independent_first
        .reply
        .send(tool(&sleep("independent"), true))
        .ok()
        .unwrap();
    first
        .reply
        .send(tool(
            &format!(
                "'{executable}' message send --thread '{child}' --text 'owned child'; {}",
                sleep("parent")
            ),
            true,
        ))
        .ok()
        .unwrap();
    let mut held = std::collections::HashMap::new();
    while held.len() < 4 {
        let request = incoming(&mut requests).await;
        let thread = request_thread(&request.body);
        if thread == child && !held.contains_key(&child) && !path("child").exists() {
            request.reply.send(tool(&format!("'{executable}' message send --thread '{grandchild}' --text 'owned grandchild'; {}",sleep("child")),true)).ok().unwrap();
        } else if thread == grandchild && !path("grandchild").exists() {
            request
                .reply
                .send(tool(&sleep("grandchild"), true))
                .ok()
                .unwrap();
        } else {
            assert!(held.insert(thread, request).is_none());
        }
    }
    let mut pids = Vec::new();
    for name in ["parent", "child", "grandchild", "independent"] {
        pids.push(process_started(&path(name)).await);
    }
    let child_query = child.clone();
    let grand_query = grandchild.clone();
    let (child_run, grand_run, grand_owner, child_owner): (String, String, String, String) =
        control
            .store
            .database
            .call(move |db| {
                Ok((
                    db.query_row(
                        "SELECT id FROM run WHERE thread_id=?1",
                        [&child_query],
                        |r| r.get(0),
                    )?,
                    db.query_row(
                        "SELECT id FROM run WHERE thread_id=?1",
                        [&grand_query],
                        |r| r.get(0),
                    )?,
                    db.query_row(
                        "SELECT owner_run_id FROM run WHERE thread_id=?1",
                        [grand_query],
                        |r| r.get(0),
                    )?,
                    db.query_row(
                        "SELECT owner_run_id FROM run WHERE thread_id=?1",
                        [child_query],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap();
    assert_eq!(child_owner, parent.run_id.as_deref().unwrap());
    assert_eq!(grand_owner, child_run);
    command(
        &control,
        Command::RunStop(RunTarget {
            run_id: parent.run_id.clone().unwrap(),
        }),
    )
    .await;
    for run in [parent.run_id.as_ref().unwrap(), &child_run, &grand_run] {
        state(&control, "run", run, "stopped").await;
    }
    for pid in &pids[..3] {
        assert!(!alive(*pid), "Run terminal before process {pid} ended");
    }
    assert!(alive(pids[3]));
    state(
        &control,
        "run",
        independent.run_id.as_ref().unwrap(),
        "live",
    )
    .await;
    for thread in [&parent.thread_id, &child, &grandchild] {
        let _ = held
            .remove(thread)
            .unwrap()
            .reply
            .send(final_answer("late answer must not be adopted"));
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    let adopted:i64=control.store.database.call(|db|Ok(db.query_row("SELECT count(*) FROM node WHERE payload_json LIKE '%late answer must not be adopted%'",[],|r|r.get(0))?)).await.unwrap();
    assert_eq!(adopted, 0);
    command(
        &control,
        Command::RunStop(RunTarget {
            run_id: independent.run_id.unwrap(),
        }),
    )
    .await;
    crate::test_watchdog::watchdog("tests/runtime_hierarchy.rs: stopping_parent_reaps_real_grandchildren_but_keeps_independent_run_and_rejects_late_answers", async {
        while alive(pids[3]) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(held);
    local.shutdown().await.unwrap();
    serving.abort();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_exposes_interrupted_waits_and_assignments_only_on_explicit_new_turn() {
    let _fixture = FIXTURE.lock().await;
    let (url, mut requests, server) = proxy().await;
    let (directory, owner) = owner_fixture(&url).await;
    owner.db.call(|db|{
        let tx=db.transaction()?;
        for (thread,run,turn,parent) in [("parent","old-parent","parent-turn",None),("child","old-child","child-turn",Some("old-parent")),("grandchild","old-grandchild","grandchild-turn",Some("old-child"))] {
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/AGENTS.md','test','fixture','now')",[thread])?;
            tx.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES(?1,?2,?3,'live','now')",rusqlite::params![run,thread,parent])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'running','now')",rusqlite::params![turn,thread,run])?;
        }
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('old-shell','old-parent','parent-turn','old-shell-op','running','/','{}','UTF-8','UTF-8','open','now')",[])?;
        tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('old-request','parent-turn','user','now','{\"role\":\"user\",\"content\":[]}',28)",[])?;
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at)VALUES('old-message','old-message-op','{}','parent','old-request','old-parent','parent-turn','accepted','now')",[])?;
        tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at)VALUES('legacy-op','legacy','{\"thread_id\":\"parent\",\"token\":\"secret-legacy-token\"}','hash','pending','now')",[])?;
        let conditions=(0..70).map(|_|json!({"kind":"run_ended","run_id":"old-child"})).collect::<Vec<_>>();
        for number in 0..66 {
            let shell=format!("old-shell-{number:03}");
            tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,'old-parent','parent-turn',?1,'running','/','{}','UTF-8','UTF-8','open','now')",[shell])?;
            tx.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at,ended_at)VALUES(?1,'child','old-parent','completed','now','now')",[format!("old-completed-child-{number:03}")])?;
            let operation=format!("old-wait-{number:03}");
            let body=json!({"operation_id":operation,"caller":{"thread_id":"parent","run_id":"old-parent","turn_id":"parent-turn","execution_id":"old-shell","token":"secret-must-not-survive"},"conditions":conditions,"timeout_ms":30000,"accepted_at":"now"}).to_string();
            tx.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2)",rusqlite::params![format!("wait.pending.{operation}"),body])?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(
        Some(directory.path().join("config.toml")),
        Some(directory.path().join("data")),
        directory.path().to_path_buf(),
    )
    .await
    .unwrap();
    let (events,wait_nodes,pending,runs):(Vec<Value>,Vec<String>,i64,i64)=owner.db.call(|db|{
        let events=db.prepare("SELECT payload_ref_json FROM event WHERE kind='context_changed' ORDER BY target_thread_id")?.query_map([],|r|r.get::<_,String>(0))?.map(|r|serde_json::from_str(&r?).map_err(|e|shellboy::storage::StorageError::Schema(e.to_string()))).collect::<Result<Vec<Value>,_>>()?;
        let nodes=db.prepare("SELECT id FROM node WHERE kind='interrupted_wait' ORDER BY rowid")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        Ok((events,nodes,db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",[],|r|r.get(0))?,db.query_row("SELECT count(*) FROM run WHERE state IN ('live','stopping')",[],|r|r.get(0))?))
    }).await.unwrap();
    assert_eq!(events.len(), 3);
    for event in &events {
        let snapshot = full_history(&owner, event["snapshot_node_id"].as_str().unwrap()).await;
        assert_eq!(serde_json::from_str::<Value>(&snapshot).unwrap(), *event);
    }
    assert_eq!(wait_nodes.len(), 66);
    assert_eq!(pending, 0);
    assert_eq!(runs, 0);
    let parent = events
        .iter()
        .find(|event| event["change"]["runs"]["items"][0]["run_id"] == "old-parent")
        .unwrap();
    for (section, last_id) in [
        ("shells", "old-shell-065"),
        ("owned_children", "old-completed-child-065"),
    ] {
        assert_eq!(parent["change"][section]["count"], 67);
        assert_eq!(
            parent["change"][section]["items"].as_array().unwrap().len(),
            64
        );
        let mut originals = Vec::new();
        for node in parent["change"][section]["detail_node_ids"]
            .as_array()
            .unwrap()
        {
            let body: Value =
                serde_json::from_str(&full_history(&owner, node.as_str().unwrap()).await).unwrap();
            originals.extend(body["items"].as_array().unwrap().iter().cloned());
        }
        assert_eq!(originals.len(), 67);
        assert!(serde_json::to_string(&originals).unwrap().contains(last_id));
    }
    assert_eq!(parent["change"]["cancelled_waits"]["count"], 66);
    assert_eq!(
        parent["change"]["cancelled_waits"]["items"]
            .as_array()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        parent["change"]["cancelled_waits"]["items"][0]["conditions_count"],
        70
    );
    assert_eq!(
        parent["change"]["cancelled_waits"]["items"][0]["conditions"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        parent["change"]["pending_operations_with_unknown_targets"]["items"][0]["wait_targets"],
        "unknown"
    );
    let original = shellboy::history::History::new(owner.db.clone())
        .get(HistoryGetArgs {
            final_answer_only: false,
            node_id: wait_nodes.last().unwrap().clone(),
            limit_bytes: 65536,
            cursor: None,
            encoding: "UTF-8".into(),
        })
        .await
        .unwrap();
    let saved: Value = serde_json::from_str(&original.text).unwrap();
    assert_eq!(saved["conditions"].as_array().unwrap().len(), 70);
    assert!(!original.text.contains("secret"));
    assert!(!serde_json::to_string(&events).unwrap().contains("secret"));
    assert!(
        requests.try_recv().is_err(),
        "startup must not call the model"
    );
    owner.db.call(|db| {
        assert_eq!(db.query_row("SELECT state,path_admission,EXISTS(SELECT 1 FROM thread_path WHERE thread_id='parent' AND node_id='old-request') FROM receipt WHERE id='old-message'", [], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?)))?,
            ("held".into(),"unadmitted".into(),false), "startup preserves unadmitted input without automatically replaying it");
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, directory.path().to_path_buf());
    let accepted = command(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some("parent".into()),
            text: "explicit resume instruction".into(),
        }),
    )
    .await;
    assert_ne!(accepted["run_id"], "old-parent");
    assert_ne!(accepted["turn_id"], "parent-turn");
    let request = incoming(&mut requests).await;
    let text = request.body.to_string();
    for expected in [
        "previous_instance_interrupted",
        "old-parent",
        "old-child",
        "old-shell",
        "old-request",
        "old-message",
        "legacy-op",
        "interrupted_wait",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert!(
        !text.contains("old-wait-065"),
        "full saved waits should be retrieved by reference"
    );
    request
        .reply
        .send(final_answer("resumed explicitly"))
        .ok()
        .unwrap();
    state(
        &control,
        "run",
        accepted["run_id"].as_str().unwrap(),
        "completed",
    )
    .await;
    let new_message = accepted["message_id"].as_str().unwrap().to_owned();
    owner
        .db
        .call(move |db| {
            let assignment: (String, String, String) = db.query_row(
                "SELECT run_id,turn_id,state FROM receipt WHERE id='old-message'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(
                assignment,
                ("old-parent".into(), "parent-turn".into(), "applied".into())
            );
            assert_eq!(db.query_row("SELECT n.turn_id,r.path_admission,r.hold_reason FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.id='old-message'", [], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?,
                ("parent-turn".into(),"admitted".into(),"recovered_at_explicit_resume".into()));
            let positions: (i64,i64) = db.query_row("SELECT (SELECT ordinal FROM thread_path WHERE thread_id='parent' AND node_id='old-request'),(SELECT ordinal FROM thread_path WHERE thread_id='parent' AND node_id=?1)", [new_message], |r|Ok((r.get(0)?,r.get(1)?)))?;
            assert!(positions.0 < positions.1, "recovered instruction precedes explicit new send");
            assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE message_node_id='old-request'", [], |r|r.get::<_,i64>(0))?,1, "recovery preserves one original receipt");
            assert_eq!(
                db.query_row("SELECT state FROM run WHERE id='old-child'", [], |r| r
                    .get::<_, String>(
                    0
                ))?,
                "stopped"
            );
            Ok(())
        })
        .await
        .unwrap();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_cli_cannot_stop_or_write_to_an_independent_live_run() {
    let _fixture = FIXTURE.lock().await;
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let independent = start(&control).await;
    let first = incoming(&mut requests).await;
    let pidfile = directory.path().join("independent.pid");
    first
        .reply
        .send(tool(
            &format!("printf '%s' $$ > '{}'; exec sleep 60", pidfile.display()),
            true,
        ))
        .ok()
        .unwrap();
    let independent_held = incoming(&mut requests).await;
    let pid = process_started(&pidfile).await;
    let target_run = independent.run_id.clone().unwrap();
    let query_run = target_run.clone();
    let execution: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT id FROM shell_execution WHERE owner_run_id=?1",
                [query_run],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let source = start(&control).await;
    let request = incoming(&mut requests).await;
    let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
    let output = directory.path().join("ownership-errors.jsonl");
    let script = format!(
        "set +e; '{executable}' run stop --run '{target_run}' > '{output}'; '{executable}' shell stop --execution '{execution}' >> '{output}'; '{executable}' shell eof --execution '{execution}' >> '{output}'; '{executable}' shell write --execution '{execution}' --text 'forbidden' >> '{output}'; true",
        output = output.display()
    );
    request.reply.send(tool(&script, false)).ok().unwrap();
    let continuation = incoming(&mut requests).await;
    let errors = tokio::fs::read_to_string(&output).await.unwrap();
    let responses = errors
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 4, "{errors}");
    for response in responses {
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "invalid_state");
    }
    assert!(alive(pid));
    state(&control, "run", &target_run, "live").await;
    continuation
        .reply
        .send(final_answer("independent ownership preserved"))
        .ok()
        .unwrap();
    state(
        &control,
        "run",
        source.run_id.as_ref().unwrap(),
        "completed",
    )
    .await;
    command(&control, Command::RunStop(RunTarget { run_id: target_run })).await;
    crate::test_watchdog::watchdog(
        "tests/runtime_hierarchy.rs: model_cli_cannot_stop_or_write_to_an_independent_live_run",
        async {
            while alive(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        },
    )
    .await
    .unwrap();
    drop(independent_held);
    local.shutdown().await.unwrap();
    serving.abort();
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_parent_cli_holds_ordinary_recovery_message_and_retries_only_direct_child() {
    let _fixture = FIXTURE.lock().await;
    let (url, mut requests, server) = proxy().await;
    let (directory, owner, control) = runtime(&url).await;
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let parent = start(&control).await;
    let first = incoming(&mut requests).await;
    let child = child_thread(&control, &parent.thread_id).await;
    let grandchild = child_thread(&control, &child).await;
    let parent_run = parent.run_id.clone().unwrap();
    let child_query = child.clone();
    owner.db.call(move|db|{
        let tx=db.transaction()?;
        for (thread,run,turn,owner) in [(&child_query,"recovery-child","recovery-child-turn",parent_run.as_str()),(&grandchild,"recovery-grandchild","recovery-grandchild-turn","recovery-child")] {
            tx.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES(?1,?2,?3,'live','now')",rusqlite::params![run,thread,owner])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES(?1,?2,?3,'recovery_wait','now','{\"reason\":\"fixture_proxy_failure\"}')",rusqlite::params![turn,thread,run])?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
    let output = directory.path().join("recovery-results.jsonl");
    first.reply.send(tool(&format!("set +e; '{executable}' message send --thread '{child}' --text 'ordinary machine note' > '{output}'; '{executable}' run retry --run recovery-grandchild >> '{output}'; '{executable}' run retry --run recovery-child >> '{output}'; true",output=output.display()),false)).ok().unwrap();
    let mut parent_continuation = None;
    let mut child_retry = None;
    for _ in 0..2 {
        let request = incoming(&mut requests).await;
        if request_thread(&request.body) == child {
            child_retry = Some(request);
        } else {
            parent_continuation = Some(request);
        }
    }
    let body = tokio::fs::read_to_string(&output).await.unwrap();
    let responses = body
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3, "{body}");
    assert_eq!(responses[0]["result"]["delivery_status"], "held");
    assert_eq!(responses[1]["error"]["code"], "invalid_state");
    assert_eq!(responses[2]["ok"], true);
    assert!(
        child_retry
            .as_ref()
            .unwrap()
            .body
            .to_string()
            .contains("ordinary machine note")
    );
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT generation FROM turn WHERE id='recovery-child-turn'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT state FROM turn WHERE id='recovery-grandchild-turn'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "recovery_wait"
            );
            Ok(())
        })
        .await
        .unwrap();
    child_retry
        .unwrap()
        .reply
        .send(final_answer("direct child retry completed"))
        .ok()
        .unwrap();
    state(&control, "turn", "recovery-child-turn", "completed").await;
    command(
        &control,
        Command::RunStop(RunTarget {
            run_id: parent.run_id.unwrap(),
        }),
    )
    .await;
    state(&control, "run", "recovery-grandchild", "stopped").await;
    drop(parent_continuation);
    local.shutdown().await.unwrap();
    serving.abort();
    close(control, owner, server).await;
}
