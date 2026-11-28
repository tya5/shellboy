use super::*;
use crate::control::driver::preparation_tests::{close, fixture, ready};
use crate::test_watchdog::watchdog;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn exchange(listener: &TcpListener, status: u16, reply: Value) -> Vec<u8> {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    let mut chunk = [0; 8192];
    let body = loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert_ne!(n, 0);
        request.extend_from_slice(&chunk[..n]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            if request.len() >= end + 4 + length {
                break request[end + 4..end + 4 + length].to_vec();
            }
        }
    };
    let reply = reply.to_string();
    socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.unwrap();
    body
}

fn completed(output: Value) -> Value {
    json!({"id":"fixture","status":"completed","output":output})
}

async fn define(control: &Control, thread: &str, name: &str, command: &str, timeout_ms: u32) {
    let thread = thread.to_owned();
    let args = crate::protocol::ContextAddArgs {
        thread_id: None,
        name: name.into(),
        command: command.into(),
        timeout_ms,
    };
    control
        .store
        .database
        .call(move |db| {
            super::super::super::context_command::add(db, &thread, &args)?;
            Ok(())
        })
        .await
        .unwrap();
}

fn result_at(item: &Value) -> Option<Value> {
    let text = item["content"].as_array()?.first()?.get("text")?.as_str()?;
    let value: Value = serde_json::from_str(text).ok()?;
    (value["type"] == "context_commands").then_some(value)
}

async fn inspect_only_attempt(
    root: &std::path::Path,
    control: &Arc<Control>,
    thread_id: String,
) -> Value {
    use crate::protocol::{Command, Request, StatusArgs};
    let status = control
        .request(Request::new(
            format!(
                "{}:inspect-context-status",
                control.store.operation_epoch.current()
            ),
            Command::Status(StatusArgs {
                thread_id: Some(thread_id.clone()),
                all: true,
                ..Default::default()
            }),
        ))
        .await;
    assert!(status.ok, "{:?}", status.error);
    let page = status.result.unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{page}");
    let inspected = crate::request_inspect::execute(
        crate::request_inspect::RequestOptions {
            thread_id,
            attempt_id: Some(items[0]["id"].as_str().unwrap().into()),
            compare_attempt_id: None,
            body: true,
        },
        Some(root.join("config.toml")),
        Some(root.join("data")),
        root,
    )
    .await;
    assert_eq!(inspected["ok"], true, "{inspected}");
    inspected["result"].clone()
}

#[tokio::test]
async fn retry_reuses_tail_next_step_replaces_it_and_inspect_does_not_execute() {
    watchdog("context continuation and retry", async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (root, owner, control, accepted) = fixture(&format!("http://{}/v1",listener.local_addr().unwrap())).await;
        define(&control,&accepted.thread_id,"probe", "printf x >> context-count; cat context-count", 10_000).await;
        define(&control,&accepted.thread_id,"failure", "exit 7", 10_000).await;
        control.kick_quiet(accepted.thread_id.clone()).await;
        let first = exchange(&listener, 503, json!({"error":{"code":"server_error","message":"fixture retry"}})).await;
        let retry = exchange(&listener, 200, completed(json!([{"type":"function_call","name":"shell","call_id":"next-step","arguments":"{\"command\":\"printf tool\"}"}]))).await;
        assert_eq!(retry, first, "the retry must send the identical body");
        let first: Value = serde_json::from_slice(&first).unwrap();
        let next = exchange(&listener, 200, completed(json!([]))).await;
        let next: Value = serde_json::from_slice(&next).unwrap();
        let first_tail = first["input"].as_array().unwrap().last().unwrap();
        let first_result = result_at(first_tail).unwrap();
        assert_eq!(first_result["results"][0]["stdout"], "x");
        assert_eq!(first_result["results"][1]["outcome"], "failed");
        let input = next["input"].as_array().unwrap();
        assert!(input.iter().any(|item| item["type"] == "function_call_output"));
        let last = input.last().unwrap();
        assert_eq!(result_at(last).unwrap()["results"][0]["stdout"], "xx");
        assert_eq!(input.iter().filter(|item| result_at(item).is_some()).count(), 1);
        let attempt: String = control.store.database.call(|db| Ok(db.query_row("SELECT id FROM model_attempt WHERE json_extract(input_manifest_json,'$.context_tail.item') IS NOT NULL ORDER BY rowid LIMIT 1",[],|r| r.get(0))?)).await.unwrap();
        let inspected = crate::request_inspect::execute(crate::request_inspect::RequestOptions {thread_id:accepted.thread_id, attempt_id:Some(attempt), compare_attempt_id:None, body:true},Some(root.path().join("config.toml")),Some(root.path().join("data")),root.path()).await;
        assert_eq!(inspected["ok"], true, "{inspected}");
        assert_eq!(&inspected["result"]["saved"]["context_tail"]["item"],first_tail);
        assert_eq!(std::fs::read(root.path().join("context-count")).unwrap(), b"xx");
        close(control,owner).await;
    }).await.unwrap();
}

#[tokio::test]
async fn no_definitions_add_no_context_on_wire_or_inspection() {
    watchdog("context-free request inspection", async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (root, owner, control, accepted) =
            fixture(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
        control.kick_quiet(accepted.thread_id.clone()).await;
        let sent = exchange(&listener, 200, completed(json!([]))).await;
        let parsed: Value = serde_json::from_slice(&sent).unwrap();
        assert!(
            parsed["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| result_at(item).is_none())
        );
        let inspected = inspect_only_attempt(root.path(), &control, accepted.thread_id).await;
        assert_eq!(inspected["unknown"], json!([]));
        assert!(inspected["saved"]["context_tail"].is_null());
        assert!(
            inspected["body"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| result_at(item).is_none())
        );
        close(control, owner).await;
    })
    .await
    .unwrap();
}

async fn gated_child(interrupt: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (root, owner, control, accepted) =
        fixture(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let snapshot = ready(&control, &accepted).await;
    define(&control,&accepted.thread_id,"held", "trap 'printf stopped > context-stopped; exit' TERM; printf '%s' \"$$\" > context-started; while :; do sleep 1; done", 30_000).await;
    control.kick_quiet(accepted.thread_id.clone()).await;
    let started = root.path().join("context-started");
    let pid = loop {
        if let Ok(value) = std::fs::read_to_string(&started)
            && let Ok(pid) = value.parse::<i32>()
        {
            break pid;
        }
        tokio::task::yield_now().await;
    };
    if interrupt {
        assert_eq!(
            control.interrupt_turn(snapshot.turn.clone()).await.unwrap()["requested"],
            true
        );
    } else {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        let sent: Value =
            serde_json::from_slice(&exchange(&listener, 200, completed(json!([]))).await).unwrap();
        let result = result_at(sent["input"].as_array().unwrap().last().unwrap()).unwrap();
        assert_eq!(result["results"][0]["outcome"], "timed_out");
    }
    while control
        .live
        .test_drivers()
        .lock()
        .await
        .contains_key(&accepted.thread_id)
    {
        tokio::task::yield_now().await;
    }
    // The owned driver must join the process before retiring; a signal request is not enough.
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success()
    );
    let attempts: i64 = control
        .store
        .database
        .call(|db| Ok(db.query_row("SELECT count(*) FROM model_attempt", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(attempts, i64::from(!interrupt));
    close(control, owner).await;
}

#[tokio::test]
async fn timed_out_context_is_a_fact_and_the_request_proceeds() {
    watchdog("context deadline", gated_child(false))
        .await
        .unwrap();
}

#[tokio::test]
async fn interrupting_the_turn_reaps_context_before_admission() {
    watchdog("context interrupt", gated_child(true))
        .await
        .unwrap();
}

#[tokio::test]
async fn closed_turn_gate_cannot_create_a_native_context_process() {
    watchdog("interruption before context launch", async {
        let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let snapshot = ready(&control, &accepted).await;
        define(
            &control,
            &accepted.thread_id,
            "held",
            "printf launched > launch-marker",
            30_000,
        )
        .await;
        let gate = Arc::new(Mutex::new(true));
        // In the interruption path the gate closes before the token is notified.
        collect(&control, &snapshot, &CancellationToken::new(), gate)
            .await
            .unwrap();
        assert!(!root.path().join("launch-marker").exists());
        close(control, owner).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_budget_that_only_fits_the_skip_fact_never_launches() {
    watchdog("context skip request", async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (root, owner, control, accepted) =
            crate::control::driver::preparation_tests::fixture_with_config(
                &format!("http://{}/v1", listener.local_addr().unwrap()),
                "[limits]\ntool_excerpt_bytes=138\n",
            )
            .await;
        define(
            &control,
            &accepted.thread_id,
            "side-effect",
            "printf launched > launch-marker",
            10_000,
        )
        .await;
        control.kick_quiet(accepted.thread_id.clone()).await;
        let sent: Value =
            serde_json::from_slice(&exchange(&listener, 200, completed(json!([]))).await).unwrap();
        let tail = sent["input"].as_array().unwrap().last().unwrap();
        let fact: Value =
            serde_json::from_str(tail["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            fact,
            json!({"context":"skipped","reason":"budget_too_small"})
        );
        assert!(serde_json::to_vec(tail).unwrap().len() <= 138);
        let inspected = inspect_only_attempt(root.path(), &control, accepted.thread_id).await;
        assert_eq!(&inspected["saved"]["context_tail"]["item"], tail);
        assert_eq!(
            inspected["body"]["input"].as_array().unwrap().last(),
            Some(tail)
        );
        assert!(!root.path().join("launch-marker").exists());
        close(control, owner).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn expired_launch_admission_refuses_without_starting() {
    watchdog("expired context start gate", async {
        let (root, owner, control, accepted) = fixture("http://127.0.0.1:1").await;
        let snapshot = ready(&control, &accepted).await;
        let gate = Arc::new(Mutex::new(false));
        let guard = gate.lock().await;
        let cancel = CancellationToken::new();
        tokio::time::pause();
        let result = {
            let future = run(
                &control,
                &snapshot,
                &cancel,
                gate.clone(),
                super::super::super::context_command::Definition {
                    name: "expired".into(),
                    command: "printf launched > launch-marker".into(),
                    timeout_ms: 10,
                },
                128,
            );
            tokio::pin!(future);
            assert!(futures_util::poll!(&mut future).is_pending());
            tokio::time::advance(Duration::from_millis(11)).await;
            drop(guard);
            serde_json::to_value(future.await).unwrap()
        };
        tokio::time::resume();
        assert_eq!(result["outcome"], "timed_out");
        assert_eq!(result["started"], false);
        assert!(!root.path().join("launch-marker").exists());
        close(control, owner).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_context_commands_fill_a_equal_shares_in_definition_order() {
    watchdog("concurrent context commands and wire budget", async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (root, owner, control, accepted) =
            fixture(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.path().join("handoff"))
                .status()
                .unwrap()
                .success()
        );
        define(
            &control,
            &accepted.thread_id,
            "first",
            "IFS= read -r signal < handoff && printf first && head -c 16384 /dev/zero",
            30_000,
        )
        .await;
        define(
            &control,
            &accepted.thread_id,
            "second",
            "printf 'ready\\n' > handoff && printf second",
            30_000,
        )
        .await;
        control.kick_quiet(accepted.thread_id).await;
        let sent: Value =
            serde_json::from_slice(&exchange(&listener, 200, completed(json!([]))).await).unwrap();
        let tail = sent["input"].as_array().unwrap().last().unwrap();
        assert!(
            serde_json::to_vec(tail).unwrap().len() as u64
                <= control.store.config.limits.tool_excerpt_bytes
        );
        let result = result_at(tail).unwrap();
        assert_eq!(result["results"][0]["name"], "first");
        assert_eq!(
            result["results"][0]["outcome"], "ok",
            "first needs the second process to unblock"
        );
        assert!(
            result["results"][0]["stdout"]
                .as_str()
                .unwrap()
                .starts_with("first")
        );
        assert_eq!(result["results"][0]["truncated"], true);
        assert_eq!(result["outcome"], "truncated");
        assert_eq!(result["results"][1]["stdout"], "second");
        assert_eq!(result["results"][1]["truncated"], false);
        close(control, owner).await;
    })
    .await
    .unwrap();
}
