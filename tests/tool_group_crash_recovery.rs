//! Real process death and durable fault fixtures; no provider or default data.
use rusqlite::params;
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, history, protocol::*};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    time::{sleep, timeout},
};

struct Proxy {
    url: String,
    incoming: mpsc::Receiver<Value>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Proxy {
    async fn new(first: Option<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (send, incoming) = mpsc::channel(8);
        let task = tokio::spawn(async move {
            let mut first = first;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                socket.set_nodelay(true).unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 65536];
                let body = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "request ended before body");
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() < 8 * 1024 * 1024);
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let len = String::from_utf8_lossy(&bytes[..at])
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= at + 4 + len {
                            break serde_json::from_slice::<Value>(&bytes[at + 4..at + 4 + len])
                                .unwrap();
                        }
                    }
                };
                send.send(body).await.unwrap();
                let response=first.take().unwrap_or_else(||json!({"id":"repair-answer","status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"continued after interruption"}]}]})).to_string();
                socket.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
            }
        });
        Self {
            url,
            incoming,
            task,
        }
    }
    async fn next(&mut self) -> Value {
        timeout(Duration::from_secs(20), self.incoming.recv())
            .await
            .expect("model request watchdog")
            .expect("proxy stopped")
    }
}
fn config(root: &Path, url: &str) -> PathBuf {
    let path = root.join("config.toml");
    std::fs::write(&path,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\nresponse_header_timeout_ms=10000\n")).unwrap();
    path
}
async fn open(root: &Path) -> DataOwner {
    DataOwner::start(
        Some(root.join("config.toml")),
        Some(root.join("data")),
        root.into(),
    )
    .await
    .unwrap()
}
async fn rpc(control: &Arc<Control>, command: Command) -> Value {
    let response = timeout(
        Duration::from_secs(20),
        control.request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        )),
    )
    .await
    .expect("Control request watchdog");
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
async fn send_and_complete(control: &Arc<Control>, thread: &str, marker: &str) {
    let accepted: MessageAccepted = serde_json::from_value(
        rpc(
            control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                now: false,
                thread_id: Some(thread.into()),
                text: marker.into(),
            }),
        )
        .await,
    )
    .unwrap();
    let turn = accepted.turn_id.unwrap();
    timeout(Duration::from_secs(20), async {
        loop {
            let turn = turn.clone();
            let state: String = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row("SELECT state FROM turn WHERE id=?1", [turn], |r| r.get(0))?)
                })
                .await
                .unwrap();
            assert_ne!(state, "recovery_wait", "repaired tool pairs were rejected");
            if state == "completed" {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("new Turn did not complete");
}
fn assert_pairs(request: &Value, expected: &[&str], new_marker: &str) {
    let input = request["input"].as_array().unwrap();
    let calls = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item["type"] == "function_call")
        .collect::<Vec<_>>();
    assert_eq!(
        calls
            .iter()
            .map(|(_, item)| item["call_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected.to_vec()
    );
    let start = calls.last().unwrap().0 + 1;
    for (offset, call) in expected.iter().enumerate() {
        assert_eq!(
            input[start + offset]["type"],
            "function_call_output",
            "all outputs must be contiguous immediately after the call block: {input:?}"
        );
        assert_eq!(input[start + offset]["call_id"], *call);
        assert_eq!(
            input
                .iter()
                .filter(|item| item["type"] == "function_call_output" && item["call_id"] == *call)
                .count(),
            1
        );
    }
    let newest = input
        .iter()
        .position(|item| item["role"] == "user" && item.to_string().contains(new_marker))
        .expect("new request missing");
    assert!(
        newest >= start + expected.len(),
        "new user message precedes repaired outputs"
    );
}
async fn result_snapshot(owner: &DataOwner) -> Vec<(String, String, String)> {
    owner.db.call(|db|Ok(db.prepare("SELECT r.call_id,r.node_id,n.payload_json FROM tool_result r JOIN node n ON n.id=r.node_id ORDER BY r.call_id")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_repairs_pending_partial_old_terminal_and_child_groups_idempotently() {
    let root = tempfile::tempdir().unwrap();
    let mut proxy = Proxy::new(None).await;
    config(root.path(), &proxy.url);
    let owner = open(root.path()).await;
    let cwd = root.path().to_string_lossy().into_owned();
    owner.db.call(move|db| {
        let tx=db.transaction()?;
        tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent',?1,'','fixture','fixture','now')",[&cwd])?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','now')",[])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('parent-turn','parent','parent-run','waiting','now')",[])?;
        for (thread,count,terminal,child) in [("pending",1,false,false),("partial",2,false,false),("terminal",1,true,false),("child",1,false,true)] {
            let run=format!("run-{thread}");let turn=format!("turn-{thread}");let attempt=format!("attempt-{thread}");let group=format!("group-{thread}");let source=format!("source-{thread}");
            tx.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,?3,'','fixture','fixture','now')",params![thread,child.then_some("parent"),cwd])?;
            tx.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES(?1,?2,?3,?4,'now')",params![run,thread,child.then_some("parent-run"),if terminal{"stopped"}else{"live"}])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,?4,'now')",params![turn,thread,run,if terminal{"stopped"}else{"running"}])?;
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,?2,0,0,'fixture','fixture',0,'completed','{}','now')",params![attempt,turn])?;
            tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,?3,'running','now')",params![group,attempt,turn])?;
            let calls=(0..count).map(|i|json!({"type":"function_call","call_id":format!("call-{thread}-{i}"),"name":"shell","arguments":"{\"command\":\"fixture never executes\"}"})).collect::<Vec<_>>();
            history::append_node(&tx,thread,&source,Some(&turn),"assistant_response",&json!({"output":calls,"group_id":group,"reasoning_epoch":0}),"now")?;
            for (i,item) in calls.iter().enumerate() {
                let call=item["call_id"].as_str().unwrap();let completed=thread=="partial"&&i==0;let running=thread=="partial"&&i==1;
                tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,?2,'function_call','completed',?3,?4)",params![attempt,i as i64,item.to_string(),source])?;
                tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES(?1,?2,?3,'shell',?4,?5,?6)",params![group,call,i as i64,item["arguments"].as_str().unwrap(),if completed{"completed"}else if running{"running"}else{"pending"},running.then_some("execution-partial")])?;
                if completed {let payload=json!({"item":{"type":"function_call_output","call_id":call,"output":"{\"status\":\"exited\",\"text\":\"KEEP_EXISTING\",\"exit_code\":0}"},"group_id":group});history::append_node(&tx,thread,"saved-result",Some(&turn),"tool_result",&payload,"now")?;tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?2,'saved-result','exited')",params![group,call])?;}
                if running {tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('execution-partial',?1,?2,'execution-partial','running',?3,'{\"command\":\"synthetic saved running metadata\"}','UTF-8','UTF-8','open','now')",params![run,turn,cwd])?;}
            }
            if terminal {history::append_node(&tx,thread,"old-audit",None,"interruption_context",&json!({"role":"developer","content":[{"type":"input_text","text":"LEGACY_AUDIT_AFTER_UNPAIRED_CALL"}]}),"now")?;}
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    let before = result_snapshot(&owner).await;
    assert_eq!(before.len(), 1);
    owner.shutdown().await.unwrap();
    let owner = open(root.path()).await;
    let repaired = result_snapshot(&owner).await;
    assert_eq!(repaired.len(), 5);
    assert!(
        repaired.contains(&before[0]),
        "previous result ID and payload were rewritten"
    );
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM tool_group WHERE state!='completed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM tool_call WHERE state!='completed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    for (call, _, payload) in &repaired {
        if call == "call-partial-0" {
            continue;
        }
        let payload: Value = serde_json::from_str(payload).unwrap();
        let output: Value =
            serde_json::from_str(payload["item"]["output"].as_str().unwrap()).unwrap();
        assert_eq!(output["reason"], "previous_instance_interrupted");
        assert_eq!(output["output_complete"], false);
        if call == "call-partial-1" {
            assert_eq!(output["status"], "interrupted");
            assert_eq!(output["execution_id"], "execution-partial");
            assert_eq!(output["result_uncertain"], true);
        } else {
            assert_eq!(output["status"], "not_started");
            assert_eq!(output["result_uncertain"], false);
            assert!(output.get("execution_id").is_none());
        }
    }
    owner.shutdown().await.unwrap();
    let owner = open(root.path()).await;
    assert_eq!(
        result_snapshot(&owner).await,
        repaired,
        "second restart duplicated or rewrote repair results"
    );
    let control = Control::new(&owner, root.path().into());
    for (thread, calls) in [
        ("pending", vec!["call-pending-0"]),
        ("partial", vec!["call-partial-0", "call-partial-1"]),
        ("terminal", vec!["call-terminal-0"]),
        ("child", vec!["call-child-0"]),
    ] {
        let marker = format!("AFTER_RESTART_{thread}");
        send_and_complete(&control, thread, &marker).await;
        let body = proxy.next().await;
        assert_pairs(&body, &calls, &marker);
        if thread == "terminal" {
            let input = body["input"].as_array().unwrap();
            let audit = input
                .iter()
                .position(|item| {
                    item.to_string()
                        .contains("LEGACY_AUDIT_AFTER_UNPAIRED_CALL")
                })
                .unwrap();
            let output = input
                .iter()
                .position(|item| item["type"] == "function_call_output")
                .unwrap();
            assert!(
                output < audit,
                "legacy interruption audit split call/result ordering"
            );
        }
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_result_write_failure_is_repaired_after_storage_is_restored() {
    let root = tempfile::tempdir().unwrap();
    let tool = json!({"id":"storage-tool","status":"completed","output":[{"type":"function_call","call_id":"call-storage","name":"shell","arguments":"{\"command\":\"echo STORAGE_TOOL_FINISHED\"}"}]});
    let mut proxy = Proxy::new(Some(tool)).await;
    config(root.path(), &proxy.url);
    let owner = open(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    let mut events = control.subscribe();
    let thread = rpc(
        &control,
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
        .to_owned();
    owner.db.call(|db|{db.execute_batch("CREATE TRIGGER refuse_tool_result BEFORE INSERT ON tool_result BEGIN SELECT RAISE(ABORT,'synthetic result persistence failure'); END;")?;Ok(())}).await.unwrap();
    rpc(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some(thread.clone()),
            text: "run one synthetic shell".into(),
        }),
    )
    .await;
    let _initial = proxy.next().await;
    timeout(Duration::from_secs(20), async {
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
    .expect("tool-result trigger did not stop the real Driver with SaveFailed");
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM tool_result", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            db.execute_batch("DROP TRIGGER refuse_tool_result")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        control.shutdown().await.unwrap_err().code,
        ErrorCode::SaveFailed
    );
    owner.shutdown().await.unwrap();
    let owner = open(root.path()).await;
    let repaired = result_snapshot(&owner).await;
    assert_eq!(repaired.len(), 1);
    assert_eq!(repaired[0].0, "call-storage");
    let control = Control::new(&owner, root.path().into());
    send_and_complete(&control, &thread, "AFTER_STORAGE_REPAIR").await;
    assert_pairs(
        &proxy.next().await,
        &["call-storage"],
        "AFTER_STORAGE_REPAIR",
    );
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                1,
                "startup must not execute the old tool again"
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess entry point for the real SIGKILL owner regression"]
fn crash_owner_process_child() {
    let root = PathBuf::from(
        std::env::var_os("SHELLBOY_CRASH_FIXTURE_ROOT")
            .expect("only invoke as the isolated fixture child"),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let owner = open(&root).await;
        let control = Control::new(&owner, root.clone());
        let (send, receive) = mpsc::channel(16);
        let _server = shellboy::ipc::LocalServer::bind(&owner, send)
            .await
            .unwrap();
        tokio::spawn(control.serve(receive));
        std::fs::write(root.join("owner-ready"), b"ready").unwrap();
        std::future::pending::<()>().await;
    });
}

#[cfg(unix)]
struct KilledOwner {
    child: std::process::Child,
    shell_group: Option<i32>,
    active: bool,
}
#[cfg(unix)]
impl KilledOwner {
    fn kill(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let owner_group = i32::try_from(self.child.id()).unwrap();
        // SAFETY: these are process groups created and owned by this fixture.
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(-owner_group, libc::SIGKILL);
            }
        }
        if let Some(shell_group) = self.shell_group.take() {
            assert!(shell_group > 1 && shell_group != owner_group);
            // The production shell intentionally starts in a separate process
            // group. Killing the owner group alone must not leave this fixture's
            // waiting shell behind, even when an assertion unwinds.
            unsafe {
                libc::kill(-shell_group, libc::SIGKILL);
            }
        }
        let _ = self.child.wait();
    }
}
#[cfg(unix)]
impl Drop for KilledOwner {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigkill_real_owner_during_native_shell_then_first_post_has_adjacent_pairs() {
    use std::os::unix::process::CommandExt;
    let root = tempfile::tempdir().unwrap();
    let pidfile = root.path().join("native-shell-pid");
    let quoted = format!("'{}'", pidfile.to_string_lossy().replace('\'', "'\\''"));
    let command =
        format!("printf '%s\n' \"$$\" > {quoted} && printf CRASH_READY && read fixture_line");
    let tool = json!({"id":"crash-tool","status":"completed","output":[{"type":"function_call","call_id":"call-crash","name":"shell","arguments":json!({"command":command,"stdin_open":true}).to_string()}]});
    let mut proxy = Proxy::new(Some(tool)).await;
    config(root.path(), &proxy.url);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_owner_process_child",
            "--ignored",
            "--nocapture",
        ])
        .env("SHELLBOY_CRASH_FIXTURE_ROOT", root.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut killed = KilledOwner {
        child,
        shell_group: None,
        active: true,
    };
    timeout(Duration::from_secs(20), async {
        while !root.path().join("owner-ready").exists() {
            assert!(
                killed.child.try_wait().unwrap().is_none(),
                "owner child exited before ready"
            );
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owner subprocess startup watchdog");
    let directory = root.path().join("data");
    let response = shellboy::ipc::call(
        &directory,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(response.ok, "{:?}", response.error);
    let thread = response.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = shellboy::ipc::call(
        &directory,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some(thread.clone()),
            text: "start the actual native shell".into(),
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(response.ok, "{:?}", response.error);
    let _initial = proxy.next().await;
    timeout(Duration::from_secs(20),async {loop {
        if let Ok(text)=std::fs::read_to_string(&pidfile) && text.ends_with('\n') && let Ok(pid)=text.trim().parse::<i32>() {
            assert!(pid>1);
            // SAFETY: getpgid only observes the PID written by our shell.
            assert_eq!(unsafe {libc::getpgid(pid)},pid);
            killed.shell_group=Some(pid);
            let db=rusqlite::Connection::open_with_flags(directory.join("shellboy.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let ready:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution s JOIN output_chunk o ON o.execution_id=s.id WHERE s.state='running' AND o.bytes=CAST('CRASH_READY' AS BLOB))",[],|r|r.get(0)).unwrap();
            if ready {break;}
        }
        assert!(killed.child.try_wait().unwrap().is_none());sleep(Duration::from_millis(5)).await;
    }}).await.expect("actual shell never reached running plus saved output");
    killed.kill();
    let owner = open(root.path()).await;
    let rows = result_snapshot(&owner).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "call-crash");
    let saved: Value = serde_json::from_str(&rows[0].2).unwrap();
    let output: Value = serde_json::from_str(saved["item"]["output"].as_str().unwrap()).unwrap();
    assert_eq!(output["status"], "interrupted");
    assert_eq!(output["result_uncertain"], true);
    assert_eq!(output["output_complete"], false);
    assert_eq!(output["saved_execution"]["state"], "stopped");
    assert_eq!(output["saved_execution"]["output_complete"], false);
    let control = Control::new(&owner, root.path().into());
    send_and_complete(&control, &thread, "AFTER_SIGKILL").await;
    assert_pairs(&proxy.next().await, &["call-crash"], "AFTER_SIGKILL");
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                1,
                "old shell was replayed"
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[path = "support/watchdog.rs"]
pub(crate) mod test_watchdog;
