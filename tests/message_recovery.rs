//! Interrupted delivery uses original nodes/receipts, never an automatic resend.
use rusqlite::params;
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, ipc, protocol::*};
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

async fn bounded<T>(label: &str, future: impl std::future::Future<Output = T>) -> T {
    timeout(Duration::from_secs(60), future)
        .await
        .unwrap_or_else(|_| panic!("{label}: 60-second watchdog expired"))
}
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
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (sender, incoming) = mpsc::channel(8);
        let task = tokio::spawn(async move {
            let mut first = true;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                socket.set_nodelay(true).unwrap();
                let mut bytes = Vec::new();
                let body = loop {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "fixture request ended before its body");
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(
                        bytes.len() <= 8 * 1024 * 1024,
                        "fixture request exceeded bound"
                    );
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let length = String::from_utf8_lossy(&bytes[..at])
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= at + 4 + length {
                            break serde_json::from_slice::<Value>(&bytes[at + 4..at + 4 + length])
                                .unwrap();
                        }
                    }
                };
                sender.send(body).await.unwrap();
                if first {
                    first = false;
                    // No header is sent. Death or RunStop must cancel this real
                    // socket before the proxy accepts the explicit resume.
                    let mut byte = [0];
                    match socket.read(&mut byte).await {
                        Ok(0) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::BrokenPipe
                            ) => {}
                        other => {
                            panic!("held socket did not close after owner death/stop: {other:?}")
                        }
                    }
                    continue;
                }
                let response = json!({"id":"resumed-answer","status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"RECOVERY_FIXTURE_ANSWER"}]}]}).to_string();
                let reply = format!(
                    "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        Self {
            url,
            incoming,
            task,
        }
    }
    async fn next(&mut self) -> Value {
        bounded("model request", self.incoming.recv())
            .await
            .expect("fixture proxy stopped")
    }
    fn no_pending_request(&mut self) {
        // Observe captured requests now; durable terminal-owner/Turn and
        // unchanged attempt/receipt counts prove startup has no replay work.
        assert!(
            matches!(
                self.incoming.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "unexpected captured model request"
        );
    }
}
fn config(root: &Path, url: &str) {
    std::fs::write(root.join("config.toml"), format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\nresponse_header_timeout_ms=60000\n")).unwrap();
}
async fn open(root: &Path) -> DataOwner {
    bounded(
        "owner startup",
        DataOwner::start(
            Some(root.join("config.toml")),
            Some(root.join("data")),
            root.into(),
        ),
    )
    .await
    .unwrap()
}
fn request(control: &Control, command: Command) -> Request {
    Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        command,
    )
}
async fn rpc(control: &Arc<Control>, command: Command) -> Value {
    let response = bounded(
        "Control request",
        control.request(request(control, command)),
    )
    .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
fn create(parent_id: Option<String>) -> Command {
    Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id,
    })
}
fn send(thread: &str, text: &str) -> Command {
    Command::MessageSend(MessageSendArgs {
        sender_label: None,
        now: false,
        thread_id: Some(thread.into()),
        text: text.into(),
    })
}
async fn message(control: &Arc<Control>, id: &str) -> Value {
    rpc(
        control,
        Command::MessageGet(MessageTarget {
            message_id: id.into(),
        }),
    )
    .await
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Saved {
    receipt: String,
    run: Option<String>,
    turn: Option<String>,
    node_turn: Option<String>,
    body: String,
    state: String,
    admission: String,
    reason: Option<String>,
    updated: String,
    on_path: bool,
}
fn saved(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<Saved> {
    db.query_row("SELECT r.id,r.run_id,r.turn_id,n.turn_id,n.payload_json,r.state,r.path_admission,r.hold_reason,r.updated_at,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=n.id) FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE n.id=?1", [id], |r| Ok(Saved { receipt:r.get(0)?,run:r.get(1)?,turn:r.get(2)?,node_turn:r.get(3)?,body:r.get(4)?,state:r.get(5)?,admission:r.get(6)?,reason:r.get(7)?,updated:r.get(8)?,on_path:r.get(9)? }))
}
async fn snapshot(owner: &DataOwner, id: &str) -> Saved {
    let id = id.to_owned();
    owner.db.call(move |db| Ok(saved(db, &id)?)).await.unwrap()
}
fn assert_provenance(before: &Saved, after: &Saved) {
    assert_eq!(before.receipt, after.receipt);
    assert_eq!(before.run, after.run);
    assert_eq!(before.turn, after.turn);
    assert_eq!(before.node_turn, after.node_turn);
    assert_eq!(before.body, after.body);
}
async fn complete(owner: &DataOwner, turn: &str) {
    let turn = turn.to_owned();
    bounded("resumed Turn completion", async {
        loop {
            let id = turn.clone();
            let state: String = owner
                .db
                .call(move |db| {
                    Ok(db.query_row("SELECT state FROM turn WHERE id=?1", [id], |r| r.get(0))?)
                })
                .await
                .unwrap();
            assert!(
                !matches!(state.as_str(), "recovery_wait" | "failed" | "stopped"),
                "unexpected resumed Turn state: {state}"
            );
            if state == "completed" {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
}
async fn assert_terminal_delivery_snapshot(owner: &DataOwner, attempts: i64, receipts: i64) {
    owner.db.call(move |db| {
        let counts: (i64,i64,i64,i64,i64) = db.query_row("SELECT (SELECT count(*) FROM run WHERE state IN ('live','stopping')),(SELECT count(*) FROM turn WHERE state IN ('preparing','running','waiting','recovery_wait')),(SELECT count(*) FROM model_attempt),(SELECT count(*) FROM receipt),(SELECT count(*) FROM receipt WHERE state='accepted')",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
        assert_eq!(counts,(0,0,attempts,receipts,0),"startup/stop must not create new owners, attempts or accepted work");
        Ok(())
    }).await.unwrap();
}
fn assert_input_order(body: &Value, markers: &[&str]) {
    let items = body["input"].as_array().unwrap();
    let mut previous = None;
    for marker in markers {
        let matches = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item["role"] == "user" && item.to_string().contains(marker))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "original instruction must occur exactly once: {marker}; {body}"
        );
        if let Some(index) = previous {
            assert!(
                index < matches[0],
                "recovered instructions must precede the new send: {body}"
            );
        }
        previous = Some(matches[0]);
    }
}

#[test]
#[ignore = "isolated child entry point for actual owner-process death"]
fn recovery_owner_process_child() {
    let root = PathBuf::from(
        std::env::var_os("SHELLBOY_MESSAGE_RECOVERY_ROOT").expect("fixture child only"),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let owner = open(&root).await;
        let control = Control::new(&owner, root.clone());
        control.start().await.unwrap();
        let (send, receive) = mpsc::channel(16);
        let _server = ipc::LocalServer::bind(&owner, send).await.unwrap();
        tokio::spawn(control.serve(receive));
        std::fs::write(root.join("owner-ready"), b"ready").unwrap();
        std::future::pending::<()>().await;
    });
}
async fn ipc_rpc(data: &Path, command: Command) -> Value {
    let response = bounded("child-owner IPC", ipc::call(data, command, None, None))
        .await
        .unwrap();
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_owner_distinguishes_admitted_input_from_held_and_recovers_once_on_human_send() {
    let root = tempfile::tempdir().unwrap();
    let mut proxy = Proxy::new().await;
    config(root.path(), &proxy.url);
    // module_path includes the aggregate crate name, while libtest filters do
    // not. Keep the exact nested test name instead of assuming a Cargo target.
    let module = module_path!()
        .split_once("::")
        .map(|(_, module)| module)
        .unwrap_or(module_path!());
    let filter = format!("{module}::recovery_owner_process_child");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &filter, "--ignored", "--nocapture"])
        .env("SHELLBOY_MESSAGE_RECOVERY_ROOT", root.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    bounded("child owner readiness", async {
        while !root.path().join("owner-ready").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before ready (filter {filter})"
            );
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let data = root.path().join("data");
    let thread = ipc_rpc(&data, create(None)).await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first: MessageAccepted =
        serde_json::from_value(ipc_rpc(&data, send(&thread, "FIRST_ADMITTED_MARKER")).await)
            .unwrap();
    assert_input_order(&proxy.next().await, &["FIRST_ADMITTED_MARKER"]);
    let second: MessageAccepted =
        serde_json::from_value(ipc_rpc(&data, send(&thread, "SECOND_INTERRUPTED_MARKER")).await)
            .unwrap();
    assert_eq!(first.turn_id, second.turn_id);
    let db = rusqlite::Connection::open_with_flags(
        data.join("shellboy.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let before_first = saved(&db, &first.message_id).unwrap();
    let before_second = saved(&db, &second.message_id).unwrap();
    assert!(before_first.on_path && before_first.admission == "admitted");
    assert!(!before_second.on_path && before_second.admission == "unadmitted");
    drop(db);
    bounded("kill actual owner process", child.kill())
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().await.unwrap().signal(), Some(libc::SIGKILL));
    }
    let owner = open(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    let repaired_first = snapshot(&owner, &first.message_id).await;
    let held = snapshot(&owner, &second.message_id).await;
    assert_provenance(&before_first, &repaired_first);
    assert_provenance(&before_second, &held);
    assert_eq!(repaired_first.state, "applied");
    assert_eq!(
        repaired_first.reason.as_deref(),
        Some("terminal_after_path_admission")
    );
    assert_eq!(held.state, "held");
    assert_eq!(
        held.reason.as_deref(),
        Some("terminal_before_path_admission")
    );
    assert!(!held.on_path);
    let first_details = message(&control, &first.message_id).await;
    assert!(
        first_details.get("answer_node_id").is_none(),
        "applied is not answered"
    );
    assert_eq!(
        message(&control, &second.message_id).await["context"]["hold_reason"],
        "terminal_before_path_admission"
    );
    assert_terminal_delivery_snapshot(&owner, 1, 2).await;
    proxy.no_pending_request();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = open(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    assert_eq!(
        snapshot(&owner, &second.message_id).await,
        held,
        "repeated startup preserves held facts/reason/time"
    );
    assert_terminal_delivery_snapshot(&owner, 1, 2).await;
    proxy.no_pending_request();
    let resume = request(&control, send(&thread, "THIRD_EXPLICIT_RESUME_MARKER"));
    let response = bounded("explicit resume", control.request(resume.clone())).await;
    assert!(response.ok, "{:?}", response.error);
    let third: MessageAccepted = serde_json::from_value(response.result.clone().unwrap()).unwrap();
    assert_ne!(third.run_id, second.run_id);
    let body = proxy.next().await;
    assert_input_order(
        &body,
        &[
            "FIRST_ADMITTED_MARKER",
            "SECOND_INTERRUPTED_MARKER",
            "THIRD_EXPLICIT_RESUME_MARKER",
        ],
    );
    complete(&owner, third.turn_id.as_deref().unwrap()).await;
    let recovered = snapshot(&owner, &second.message_id).await;
    assert_provenance(&before_second, &recovered);
    assert_eq!(recovered.state, "applied");
    assert_eq!(recovered.admission, "admitted");
    assert_eq!(
        recovered.reason.as_deref(),
        Some("recovered_at_explicit_resume")
    );
    let first_id = first.message_id.clone();
    let second_id = second.message_id.clone();
    let third_id = third.message_id.clone();
    owner.db.call(move|db|{
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE message_node_id IN (?1,?2,?3)",params![first_id,second_id,third_id],|r|r.get::<_,i64>(0))?,3,"no duplicate recovery receipts");
        assert_eq!(db.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='interrupted_messages_recovered'",[],|r|r.get::<_,i64>(0))?,1,"one provenance observation");
        Ok(())
    }).await.unwrap();
    let replay = bounded("same-operation replay", control.request(resume)).await;
    assert_eq!(replay.result, response.result);
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |row| row
                    .get::<_, i64>(0))?,
                2,
                "operation replay creates no additional model attempt"
            );
            Ok(())
        })
        .await
        .unwrap();
    proxy.no_pending_request();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

async fn store_rpc(control: &Control, command: Command, origin: Option<&Origin>) -> Value {
    bounded(
        "transaction fixture",
        control
            .store
            .execute(request(control, command), origin.cloned()),
    )
    .await
    .unwrap()
    .result
}
async fn terminal_fixture(owner: &DataOwner, accepted: &MessageAccepted) {
    let run = accepted.run_id.clone().unwrap();
    let turn = accepted.turn_id.clone().unwrap();
    owner
        .db
        .call(move |db| {
            db.execute(
                "UPDATE turn SET state='stopped',ended_at='2026-10-30T00:00:00Z' WHERE id=?1",
                [turn],
            )?;
            db.execute(
                "UPDATE run SET state='stopped',ended_at='2026-10-30T00:00:00Z' WHERE id=?1",
                [run],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn only_human_or_live_direct_parent_restart_authorizes_old_unadmitted_input() {
    // Store receives an already authenticated Origin. This fixture exercises
    // its authority boundary, without pretending a synthetic token passed IPC.
    for own_parent in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut proxy = Proxy::new().await;
        config(root.path(), &proxy.url);
        let owner = open(root.path()).await;
        let control = Control::new(&owner, root.path().into());
        let parent = store_rpc(&control, create(None), None).await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let unrelated = store_rpc(&control, create(None), None).await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let sender = if own_parent { &parent } else { &unrelated };
        let sender_message: MessageAccepted = serde_json::from_value(
            store_rpc(&control, send(sender, "PARENT_OR_OTHER_RUN"), None).await,
        )
        .unwrap();
        let child = store_rpc(&control, create(Some(parent.clone())), None).await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let old: MessageAccepted = serde_json::from_value(
            store_rpc(
                &control,
                send(&child, "OLD_UNADMITTED_CHILD_INSTRUCTION"),
                None,
            )
            .await,
        )
        .unwrap();
        terminal_fixture(&owner, &old).await;
        let before = snapshot(&owner, &old.message_id).await;
        let origin = Origin {
            instance_id: owner.instance_id.clone(),
            thread_id: sender.clone(),
            run_id: sender_message.run_id.unwrap(),
            turn_id: sender_message.turn_id.unwrap(),
            execution_id: "already-authenticated-fixture".into(),
            token: "not-an-ipc-credential".into(),
        };
        let req = request(&control, send(&child, "EXPLICIT_CHILD_RESTART"));
        let first = control
            .store
            .execute(req.clone(), Some(origin.clone()))
            .await
            .unwrap();
        let current = snapshot(&owner, &old.message_id).await;
        assert_provenance(&before, &current);
        assert_eq!(
            current.on_path, own_parent,
            "unrelated machine send cannot recover older instructions"
        );
        if own_parent {
            assert_eq!(current.state, "applied");
            assert_eq!(
                current.reason.as_deref(),
                Some("recovered_at_explicit_resume")
            );
            let replay = control
                .store
                .execute(req, Some(origin.clone()))
                .await
                .unwrap();
            assert_eq!(replay.result, first.result);
            assert_eq!(snapshot(&owner, &old.message_id).await, current);
        } else {
            assert_eq!(current.admission, "unadmitted");
        }
        proxy.no_pending_request();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn pre_marker_history_without_admission_evidence_is_not_resurrected_on_new_send() {
    let root = tempfile::tempdir().unwrap();
    let mut proxy = Proxy::new().await;
    config(root.path(), &proxy.url);
    let owner = open(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    let thread = store_rpc(&control, create(None), None).await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let prior: MessageAccepted = serde_json::from_value(
        store_rpc(&control, send(&thread, "PRIOR_ADMITTED_BUT_REMOVED"), None).await,
    )
    .unwrap();
    let uncertain: MessageAccepted = serde_json::from_value(
        store_rpc(&control, send(&thread, "UNCERTAIN_HISTORICAL_INPUT"), None).await,
    )
    .unwrap();
    terminal_fixture(&owner, &prior).await;
    let old_prior = snapshot(&owner, &prior.message_id).await;
    let old_uncertain = snapshot(&owner, &uncertain.message_id).await;
    let turn = prior.turn_id.clone().unwrap();
    let receipt = old_prior.receipt.clone();
    owner.db.call(move|db|{
        // A persisted model-attempt manifest proves prior admission even if
        // the selected path no longer contains that node after user editing.
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('historical-attempt',?1,0,0,'fixture','fixture',0,'completed',?2,'2026-10-30T00:00:00Z')",params![turn,json!({"receipts":[receipt]}).to_string()])?;
        Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    // Reconstruct the actual pre-marker format, then let production startup
    // migrate it. No read-time compatibility fallback is exercised or added.
    let path = root.path().join("data").join("shellboy.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DROP INDEX IF EXISTS shell_started_readback; DROP INDEX IF EXISTS shell_owner_started_readback; DROP INDEX IF EXISTS compact_operation_thread; DROP INDEX IF EXISTS automatic_compaction_turn; DROP INDEX IF EXISTS wait_run_state; DROP INDEX IF EXISTS wait_recovery_thread; DROP INDEX IF EXISTS thread_answer_latest; DROP INDEX IF EXISTS receipt_thread_order; DELETE FROM schema_migration WHERE version>=13; PRAGMA user_version=12; COMMIT;").unwrap();
    drop(db);
    let owner = open(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    let admitted = snapshot(&owner, &prior.message_id).await;
    let held = snapshot(&owner, &uncertain.message_id).await;
    assert_provenance(&old_prior, &admitted);
    assert_provenance(&old_uncertain, &held);
    assert_eq!(admitted.admission, "admitted");
    assert_eq!(admitted.state, "applied");
    assert!(
        !admitted.on_path,
        "positive old evidence must not reintroduce removed history"
    );
    assert_eq!(held.admission, "unknown");
    assert_eq!(held.state, "held");
    assert_eq!(held.reason.as_deref(), Some("uncertain_path_admission"));
    let new: MessageAccepted = serde_json::from_value(
        store_rpc(&control, send(&thread, "NEW_EXPLICIT_HUMAN_INPUT"), None).await,
    )
    .unwrap();
    let after = snapshot(&owner, &uncertain.message_id).await;
    assert_eq!(
        after, held,
        "new send must not resurrect uncertain historical instructions"
    );
    assert!(!snapshot(&owner, &prior.message_id).await.on_path);
    assert_ne!(new.run_id, prior.run_id);
    proxy.no_pending_request();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_stop_classifies_admitted_and_queued_input_before_restart_and_explicit_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut proxy = Proxy::new().await;
    config(root.path(), &proxy.url);
    let owner = open(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    let thread = rpc(&control, create(None)).await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first: MessageAccepted =
        serde_json::from_value(rpc(&control, send(&thread, "STOP_FIRST_ADMITTED")).await).unwrap();
    proxy.next().await;
    let second: MessageAccepted =
        serde_json::from_value(rpc(&control, send(&thread, "STOP_SECOND_QUEUED")).await).unwrap();
    let before = snapshot(&owner, &second.message_id).await;
    assert!(!before.on_path && before.admission == "unadmitted");
    rpc(
        &control,
        Command::RunStop(RunTarget {
            run_id: first.run_id.clone().unwrap(),
        }),
    )
    .await;
    bounded("RunStop terminal repair", async {
        loop {
            let current = snapshot(&owner, &second.message_id).await;
            let run = first.run_id.clone().unwrap();
            let terminal = owner
                .db
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT state='stopped' FROM run WHERE id=?1",
                        [run],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if terminal
                && current.state == "held"
                && current.reason.as_deref() == Some("terminal_before_path_admission")
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let stopped = snapshot(&owner, &second.message_id).await;
    assert_provenance(&before, &stopped);
    assert_eq!(snapshot(&owner, &first.message_id).await.state, "applied");
    assert!(
        message(&control, &first.message_id)
            .await
            .get("answer_node_id")
            .is_none()
    );
    assert_terminal_delivery_snapshot(&owner, 1, 2).await;
    proxy.no_pending_request();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = open(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    assert_eq!(snapshot(&owner, &second.message_id).await, stopped);
    assert_terminal_delivery_snapshot(&owner, 1, 2).await;
    proxy.no_pending_request();
    let third: MessageAccepted =
        serde_json::from_value(rpc(&control, send(&thread, "STOP_THIRD_EXPLICIT_RESUME")).await)
            .unwrap();
    assert_input_order(
        &proxy.next().await,
        &[
            "STOP_FIRST_ADMITTED",
            "STOP_SECOND_QUEUED",
            "STOP_THIRD_EXPLICIT_RESUME",
        ],
    );
    complete(&owner, third.turn_id.as_deref().unwrap()).await;
    assert_provenance(&before, &snapshot(&owner, &second.message_id).await);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
