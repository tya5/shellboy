//! Cross-service operation boundaries using a real owner and loopback proxy.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, attachments::AttachmentInput, control::Control, protocol::*};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::timeout,
};
struct Incoming {
    body: Value,
    reply: oneshot::Sender<(u16, Value)>,
}
async fn proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
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
                let body = loop {
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
                            break serde_json::from_slice(&bytes[i + 4..i + 4 + length]).unwrap();
                        }
                    }
                };
                let (reply, result) = oneshot::channel();
                if send.send(Incoming { body, reply }).await.is_err() {
                    return;
                }
                let Ok((status, body)) = result.await else {
                    return;
                };
                let body = body.to_string();
                let _=stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
        }
    });
    (format!("http://{address}/v1"), receive, task)
}
async fn owner(root: &std::path::Path, url: &str) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
fn answer(text: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]})
}
#[track_caller]
fn incoming<'a>(
    control: &'a Control,
    thread: &'a str,
    receive: &'a mut mpsc::Receiver<Incoming>,
) -> impl std::future::Future<Output = Incoming> + 'a {
    let caller = std::panic::Location::caller();
    async move {
        match timeout(Duration::from_secs(60), receive.recv()).await {
            Ok(Some(value)) => value,
            result => {
                let diagnostic = timeout(Duration::from_secs(5), progress(control, thread)).await;
                panic!(
                    "loopback request watchdog at {}:{}; receive={}; persisted phase after timeout={diagnostic:?}",
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
// Capture state only after a watchdog failure. The polling cadence observes a
// persisted condition; it does not establish the test's concurrency ordering.
async fn progress(control: &Control, thread: &str) -> Value {
    let thread = thread.to_owned();
    control.store.database.call(move |db| {
        let mut state = serde_json::Map::new();
        for (name, query) in [
            ("runs", "SELECT json_object('id',id,'state',state,'stop_reason',substr(stop_reason,1,512)) FROM run WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 8"),
            ("turns", "SELECT json_object('id',id,'state',state,'generation',generation,'instruction_snapshot_present',instruction_snapshot_id IS NOT NULL,'reason',substr(recovery_reason_json,1,2048)) FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 8"),
            ("attempts", "SELECT json_object('id',a.id,'state',a.state,'reason',substr(a.error_json,1,2048)) FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 ORDER BY a.rowid DESC LIMIT 8"),
            ("shells", "SELECT json_object('id',s.id,'state',s.state,'exit',s.exit_code,'complete',s.output_complete) FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE r.thread_id=?1 ORDER BY s.rowid DESC LIMIT 8"),
        ] {
            let rows = db.prepare(query)?.query_map([&thread], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            state.insert(name.into(), json!(rows));
        }
        Ok(Value::Object(state))
    }).await.unwrap()
}
async fn settled(control: &Control, thread: &str) {
    let target = thread.to_owned();
    let result = timeout(Duration::from_secs(5), async {
        loop {
            let target = target.clone();
            if control.store.database.call(move |db| Ok(db.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting'))",
                [target], |row| row.get::<_, bool>(0),
            )?)).await.unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await;
    if result.is_err() {
        let diagnostic = timeout(Duration::from_secs(1), progress(control, thread)).await;
        panic!(
            "Thread did not settle within the fixture watchdog; observed after timeout: {diagnostic:?}"
        );
    }
}
async fn send(control: &Arc<Control>, thread: &str, text: &str) {
    request(
        control,
        Command::MessageSend(MessageSendArgs {
            thread_id: Some(thread.into()),
            text: text.into(),
        }),
    )
    .await;
}

fn defaults() -> ThreadCreateArgs {
    ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }
}
fn operation(control: &Control) -> String {
    format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4())
}
async fn manifest(control: &Control, thread: &str) -> Value {
    let thread = thread.to_owned();
    control.store.database.call(move|db| {
        let body:String=db.query_row("SELECT a.input_manifest_json FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 ORDER BY a.started_at DESC,a.rowid DESC LIMIT 1",[thread],|r|r.get(0))?;
        Ok(serde_json::from_str(&body).unwrap())
    }).await.unwrap()
}
fn attachments(manifest: &Value) -> &[Value] {
    manifest["attachments"]
        .as_array()
        .expect("persisted attachment provenance manifest")
}
fn assert_position(body: &Value, item: &Value, before: &str, after: Option<&str>, kind: &str) {
    let index = item["api_item_index"].as_u64().unwrap() as usize;
    let offset = item["api_content_index"].as_u64().unwrap() as usize;
    let count = item["api_content_count"].as_u64().unwrap() as usize;
    let content = body["input"][index]["content"].as_array().unwrap();
    assert_eq!(content[0]["text"], before);
    let metadata: Value = serde_json::from_str(
        content[offset]["text"]
            .as_str()
            .unwrap()
            .strip_prefix("[Shellboy attachment metadata] ")
            .unwrap(),
    )
    .unwrap();
    for key in [
        "attachment_id",
        "source_node_id",
        "source_element_index",
        "first_inclusion",
        "content_hash",
        "state",
    ] {
        assert_eq!(
            metadata[key], item[key],
            "wire metadata must agree with persisted manifest: {key}"
        );
    }
    assert!(
        content[offset..offset + count]
            .iter()
            .any(|v| v["type"] == kind),
        "{content:?}"
    );
    if let Some(after) = after {
        assert_eq!(content[offset + count]["text"], after);
    }
    assert!(
        !body.to_string().contains("\"type\":\"attachment_ref\""),
        "internal references cannot escape onto the wire"
    );
}
async fn finish(control: &Control, thread: &str, value: Incoming) {
    value.reply.send((200, answer("synthetic answer"))).unwrap();
    settled(control, thread).await;
}
/// Build a real accepted human draft, pause it before HTTP, then construct the
/// ordered-content fixture unsupported by today's text-only composer. This is
/// synthetic history, not a mutation of user data or a claimed composer test.
async fn ordered_draft(
    control: &Arc<Control>,
    root: &std::path::Path,
    receive: &mut mpsc::Receiver<Incoming>,
) -> (String, Value) {
    let file = root.join("image-a.png");
    std::fs::write(&file, b"synthetic-image-A").unwrap();
    std::fs::write(root.join("AGENTS.md"), [0xff, 0xfe]).unwrap();
    let accepted = control
        .send_draft(
            operation(control),
            defaults(),
            None,
            "before A".into(),
            vec![AttachmentInput {
                path: file,
                kind: AttachmentKind::Image,
            }],
        )
        .await
        .unwrap();
    let thread = accepted["thread_id"].as_str().unwrap().to_owned();
    let turn = accepted["turn_id"].as_str().unwrap().to_owned();
    // Observe this accepted Turn's actual preparation rejection, rather than
    // treating an absence of active Turns as proof that the intended pause ran.
    let paused = timeout(Duration::from_secs(5), async {
        loop {
            let target = turn.clone();
            let state = control.store.database.call(move |db| {
                db.query_row("SELECT state,recovery_reason_json,instruction_snapshot_id,(SELECT count(*) FROM model_attempt WHERE turn_id=turn.id) FROM turn WHERE id=?1", [target],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, i64>(3)?)))
                    .map_err(Into::into)
            }).await.unwrap();
            if state.0 == "recovery_wait" { break state; }
            assert!(matches!(state.0.as_str(), "preparing" | "running" | "waiting"), "unexpected ordered-fixture preparation phase: {state:?}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await;
    let (_, reason, snapshot, attempts) = match paused {
        Ok(state) => state,
        Err(_) => {
            let diagnostic = timeout(Duration::from_secs(1), progress(control, &thread)).await;
            panic!(
                "ordered fixture did not reach its explicit invalid-AGENTS preparation pause within the watchdog; observed={diagnostic:?}"
            );
        }
    };
    let reason: Value = serde_json::from_str(reason.as_deref().unwrap()).unwrap();
    assert!(
        reason["message"]
            .as_str()
            .unwrap()
            .contains("AGENTS.md must contain UTF-8 text"),
        "preparation failed for an unintended reason: {reason}"
    );
    assert!(
        snapshot.is_none(),
        "invalid instructions cannot create a reusable snapshot"
    );
    assert_eq!(
        attempts, 0,
        "preparation pause must precede attempt admission"
    );
    assert!(
        receive.try_recv().is_err(),
        "fixture preparation pause precedes HTTP"
    );
    let id = accepted["message_id"].as_str().unwrap().to_owned();
    control.store.database.call(move|db|{
        let text:String=db.query_row("SELECT payload_json FROM node WHERE id=?1",[&id],|r|r.get(0))?;
        let mut body:Value=serde_json::from_str(&text).unwrap();
        let content=body["content"].as_array_mut().unwrap();
        assert!(content.iter().any(|v|v["type"]=="attachment_ref"));
        content.push(json!({"type":"input_text","text":"after A"}));
        let text=body.to_string();
        db.execute("UPDATE node SET payload_json=?2,payload_bytes=?3,payload_revision=payload_revision+1 WHERE id=?1",rusqlite::params![id,text,text.len() as i64])?;
        Ok(())
    }).await.unwrap();
    std::fs::write(
        root.join("AGENTS.md"),
        "Synthetic attachment provenance fixture.",
    )
    .unwrap();
    request(
        control,
        Command::RunRetry(RunTarget {
            run_id: accepted["run_id"].as_str().unwrap().into(),
        }),
    )
    .await;
    (thread, accepted)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_order_survives_multiple_turns_and_manifest_matches_real_http_positions() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let (thread, accepted) = ordered_draft(&control, root.path(), &mut receive).await;
    let first = incoming(&control, &thread, &mut receive).await;
    let initial = manifest(&control, &thread).await;
    let a = &attachments(&initial)[0];
    assert_eq!(a["source_node_id"], accepted["message_id"]);
    assert_eq!(a["source_element_index"], 1);
    assert_eq!(a["first_inclusion"], true);
    assert_position(&first.body, a, "before A", Some("after A"), "input_image");
    finish(&control, &thread, first).await;
    for text in ["next without attachment", "another without attachment"] {
        send(&control, &thread, text).await;
        let next = incoming(&control, &thread, &mut receive).await;
        let current = manifest(&control, &thread).await;
        let replay = &attachments(&current)[0];
        assert_eq!(replay["attachment_id"], a["attachment_id"]);
        assert_eq!(replay["source_node_id"], a["source_node_id"]);
        assert_eq!(replay["first_inclusion"], false);
        assert_position(
            &next.body,
            replay,
            "before A",
            Some("after A"),
            "input_image",
        );
        let latest = next.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["content"][0]["text"] == text)
            .unwrap();
        assert_eq!(latest["content"].as_array().unwrap().len(), 1);
        assert_eq!(next.body.to_string().matches("input_image").count(), 1);
        finish(&control, &thread, next).await;
    }
    assert!(control.replay_cache_stats().hits + control.replay_cache_stats().append_hits > 0);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn separate_sources_same_path_model_switch_fork_and_owner_restart_preserve_order() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let first_owner = owner(root.path(), &url).await;
    let control = Control::new(&first_owner, root.path().into());
    let (thread, _) = ordered_draft(&control, root.path(), &mut receive).await;
    let first = incoming(&control, &thread, &mut receive).await;
    finish(&control, &thread, first).await;
    let second = control
        .send_draft(
            operation(&control),
            defaults(),
            Some(thread.clone()),
            "before B".into(),
            vec![AttachmentInput {
                path: root.path().join("image-a.png"),
                kind: AttachmentKind::Image,
            }],
        )
        .await
        .unwrap();
    let next = incoming(&control, &thread, &mut receive).await;
    let both = manifest(&control, &thread).await;
    assert_eq!(attachments(&both).len(), 2);
    assert_ne!(
        attachments(&both)[0]["attachment_id"],
        attachments(&both)[1]["attachment_id"]
    );
    assert_ne!(
        attachments(&both)[0]["source_node_id"],
        attachments(&both)[1]["source_node_id"]
    );
    assert_eq!(
        attachments(&both)[1]["source_node_id"],
        second["message_id"]
    );
    assert_position(
        &next.body,
        &attachments(&both)[0],
        "before A",
        Some("after A"),
        "input_image",
    );
    assert_position(
        &next.body,
        &attachments(&both)[1],
        "before B",
        None,
        "input_image",
    );
    finish(&control, &thread, next).await;
    request(
        &control,
        Command::ThreadSet(ThreadSetArgs {
            agents: None,
            connection: None,
            thread_id: Some(thread.clone()),
            cwd: None,
            model: Some("other-model".into()),
            effort: None,
            title: None,
        }),
    )
    .await;
    send(&control, &thread, "model changed").await;
    let next = incoming(&control, &thread, &mut receive).await;
    assert_eq!(next.body["model"], "other-model");
    let changed = manifest(&control, &thread).await;
    assert_eq!(
        attachments(&changed)
            .iter()
            .map(|v| &v["source_node_id"])
            .collect::<Vec<_>>(),
        attachments(&both)
            .iter()
            .map(|v| &v["source_node_id"])
            .collect::<Vec<_>>()
    );
    finish(&control, &thread, next).await;
    let target = thread.clone();
    let head = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT cursor_node_id FROM thread WHERE id=?1",
                [target],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    let fork = request(
        &control,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: thread.clone(),
            node_id: head,
            cwd: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&control, &fork, "fork replay").await;
    let next = incoming(&control, &fork, &mut receive).await;
    let forked = manifest(&control, &fork).await;
    assert_eq!(attachments(&forked).len(), 2);
    for (actual, expected) in attachments(&forked).iter().zip(attachments(&both)) {
        assert_eq!(actual["attachment_id"], expected["attachment_id"]);
        assert_eq!(actual["source_node_id"], expected["source_node_id"]);
        assert_eq!(actual["first_inclusion"], false);
    }
    finish(&control, &fork, next).await;
    control.shutdown().await.unwrap();
    first_owner.shutdown().await.unwrap();
    drop(control);
    // A new owner has no old transformed-history or Base64 cache to rely on.
    let restarted = owner(root.path(), &url).await;
    let control = Control::new(&restarted, root.path().into());
    send(&control, &fork, "restart replay").await;
    let next = incoming(&control, &fork, &mut receive).await;
    let rebuilt = manifest(&control, &fork).await;
    for (actual, expected) in attachments(&rebuilt).iter().zip(attachments(&both)) {
        assert_eq!(actual["source_node_id"], expected["source_node_id"]);
        assert_eq!(actual["first_inclusion"], false);
        assert_position(
            &next.body,
            actual,
            if actual["source_node_id"] == attachments(&both)[0]["source_node_id"] {
                "before A"
            } else {
                "before B"
            },
            None,
            "input_image",
        );
    }
    assert_eq!(next.body.to_string().matches("input_image").count(), 2);
    finish(&control, &fork, next).await;
    control.shutdown().await.unwrap();
    restarted.shutdown().await.unwrap();
    server.abort();
}
fn updates(body: &Value) -> Vec<Value> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["content"].as_array().into_iter().flatten())
        .filter_map(|part| {
            part["text"]
                .as_str()
                .and_then(|s| s.strip_prefix("Shellboy attachment updates v1: "))
        })
        .flat_map(|s| serde_json::from_str::<Vec<Value>>(s).unwrap())
        .collect()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_file_retry_keeps_update_identity_and_success_advances_only_that_attachment_baseline()
 {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let initial_owner = owner(root.path(), &url).await;
    let control = Control::new(&initial_owner, root.path().into());
    let (thread, _) = ordered_draft(&control, root.path(), &mut receive).await;
    let first = incoming(&control, &thread, &mut receive).await;
    let original = manifest(&control, &thread).await;
    assert!(
        updates(&first.body).is_empty(),
        "no baseline must not be described as a change"
    );
    finish(&control, &thread, first).await;
    std::fs::write(
        root.path().join("image-a.png"),
        b"changed-image-version-with-different-size",
    )
    .unwrap();
    let accepted = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            thread_id: Some(thread.clone()),
            text: "check changed version".into(),
        }),
    )
    .await;
    let changed = incoming(&control, &thread, &mut receive).await;
    let changed_manifest = manifest(&control, &thread).await;
    let entry = &attachments(&changed_manifest)[0];
    assert_eq!(entry["changed"], true);
    assert_eq!(
        entry["previous_hash"],
        attachments(&original)[0]["content_hash"]
    );
    assert_ne!(entry["content_hash"], entry["previous_hash"]);
    assert_position(
        &changed.body,
        entry,
        "before A",
        Some("after A"),
        "input_image",
    );
    let notification = updates(&changed.body);
    assert_eq!(notification.len(), 1);
    assert_eq!(notification[0]["attachment_id"], entry["attachment_id"]);
    assert_eq!(notification[0]["source_node_id"], entry["source_node_id"]);
    assert_eq!(notification[0]["previous_hash"], entry["previous_hash"]);
    assert_eq!(notification[0]["content_hash"], entry["content_hash"]);
    changed
        .reply
        .send((500, json!({"error":{"message":"synthetic failure"}})))
        .unwrap();
    settled(&control, &thread).await;
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: accepted["run_id"].as_str().unwrap().into(),
        }),
    )
    .await;
    let retry = incoming(&control, &thread, &mut receive).await;
    assert_eq!(
        updates(&retry.body),
        notification,
        "retry is the same pending update, not a new attachment"
    );
    let retried = manifest(&control, &thread).await;
    let attempt_thread = thread.clone();
    let completed_update_attempt: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT a.id FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE t.thread_id=?1 ORDER BY a.started_at DESC,a.rowid DESC LIMIT 1",
                [attempt_thread],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        attachments(&retried)[0]["attachment_id"],
        entry["attachment_id"]
    );
    finish(&control, &thread, retry).await;
    let completed_manifest = manifest(&control, &thread).await;
    let mut completed_request = completed_manifest.clone();
    let transport = completed_request
        .as_object_mut()
        .unwrap()
        .remove("http_observation")
        .expect("successful attempt must retain header observations");
    assert!(transport["header_elapsed_ms"].is_u64());
    assert_eq!(transport["phase"], "body_receive");
    assert_eq!(
        transport["request_body_bytes"],
        transport["body_source_bytes_consumed"]
    );
    assert_eq!(
        completed_request, retried,
        "completion only adds transport observations; the sent request/attachment evidence is immutable"
    );
    send(&control, &thread, "same new version again").await;
    let next = incoming(&control, &thread, &mut receive).await;
    assert!(
        updates(&next.body).is_empty(),
        "normal completion acknowledges the update"
    );
    let current = manifest(&control, &thread).await;
    assert_eq!(attachments(&current)[0]["changed"], false);
    assert_eq!(
        attachments(&current)[0]["previous_hash"],
        entry["content_hash"]
    );
    assert_eq!(attachments(&current)[0]["first_inclusion"], false);
    finish(&control, &thread, next).await;
    control.shutdown().await.unwrap();
    initial_owner.shutdown().await.unwrap();
    drop(control);
    let restarted = owner(root.path(), &url).await;
    let control = Control::new(&restarted, root.path().into());
    let saved: Value = control
        .store
        .database
        .call(move |db| {
            let (state, body): (String, String) = db.query_row(
                "SELECT state,input_manifest_json FROM model_attempt WHERE id=?1",
                [completed_update_attempt],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(state, "completed");
            Ok(serde_json::from_str(&body).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(
        saved, completed_manifest,
        "the completed update and transport evidence survives later requests and owner restart"
    );
    assert_eq!(attachments(&saved)[0]["changed"], true);
    send(&control, &thread, "same version after restart").await;
    let replay = incoming(&control, &thread, &mut receive).await;
    assert!(updates(&replay.body).is_empty());
    let replayed = manifest(&control, &thread).await;
    assert_eq!(attachments(&replayed)[0]["changed"], false);
    assert_eq!(
        attachments(&replayed)[0]["content_hash"],
        entry["content_hash"]
    );
    assert_eq!(
        attachments(&replayed)[0]["source_node_id"],
        entry["source_node_id"]
    );
    finish(&control, &thread, replay).await;
    control.shutdown().await.unwrap();
    restarted.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_and_excluded_states_stay_at_their_original_source_without_stale_entities() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let (thread, _) = ordered_draft(&control, root.path(), &mut receive).await;
    let first = incoming(&control, &thread, &mut receive).await;
    let original = manifest(&control, &thread).await;
    finish(&control, &thread, first).await;
    std::fs::remove_file(root.path().join("image-a.png")).unwrap();
    send(&control, &thread, "file is missing").await;
    let missing = incoming(&control, &thread, &mut receive).await;
    let missing_manifest = manifest(&control, &thread).await;
    let entry = &attachments(&missing_manifest)[0];
    assert_eq!(entry["state"], "unavailable");
    assert_eq!(
        entry["source_node_id"],
        attachments(&original)[0]["source_node_id"]
    );
    assert_position(
        &missing.body,
        entry,
        "before A",
        Some("after A"),
        "input_text",
    );
    assert!(!missing.body.to_string().contains("input_image"));
    finish(&control, &thread, missing).await;
    request(
        &control,
        Command::AttachmentRemove(AttachmentRemoveArgs {
            thread_id: Some(thread.clone()),
            attachment_id: entry["attachment_id"].as_str().unwrap().into(),
        }),
    )
    .await;
    send(&control, &thread, "file excluded").await;
    let excluded = incoming(&control, &thread, &mut receive).await;
    let excluded_manifest = manifest(&control, &thread).await;
    let entry = &attachments(&excluded_manifest)[0];
    assert_eq!(entry["state"], "excluded");
    assert_position(
        &excluded.body,
        entry,
        "before A",
        Some("after A"),
        "input_text",
    );
    assert!(!excluded.body.to_string().contains("input_image"));
    finish(&control, &thread, excluded).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_attachment_source_or_duplicate_reference_fails_before_any_http_request() {
    for duplicate in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = owner(root.path(), &url).await;
        let control = Control::new(&owner, root.path().into());
        let (thread, accepted) = ordered_draft(&control, root.path(), &mut receive).await;
        let first = incoming(&control, &thread, &mut receive).await;
        finish(&control, &thread, first).await;
        let source = accepted["message_id"].as_str().unwrap().to_owned();
        control.store.database.call(move|db|{
            let body:String=db.query_row("SELECT payload_json FROM node WHERE id=?1",[&source],|r|r.get(0))?;
            let mut body:Value=serde_json::from_str(&body).unwrap();
            let content=body["content"].as_array_mut().unwrap();
            let index=content.iter().position(|v|v["type"]=="attachment_ref").unwrap();
            if duplicate {content.push(content[index].clone());} else {content.remove(index);}
            let body=body.to_string();
            db.execute("UPDATE node SET payload_json=?2,payload_bytes=?3,payload_revision=payload_revision+1 WHERE id=?1",rusqlite::params![source,body,body.len() as i64])?;
            Ok(())
        }).await.unwrap();
        send(&control, &thread, "must not repair by attaching to me").await;
        settled(&control, &thread).await;
        assert!(
            receive.try_recv().is_err(),
            "invalid source provenance must fail before HTTP (duplicate={duplicate})"
        );
        let target = thread.clone();
        control.store.database.call(move|db|{
            let(state,reason):(String,String)=db.query_row("SELECT state,recovery_reason_json FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",[target],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(state,"recovery_wait");
            assert!(!reason.is_empty());
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_shell_attachment_is_replayed_once_after_every_result_in_its_tool_group() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    control.store.database.call(|db|{
        db.execute_batch("CREATE TRIGGER immutable_payload_guard BEFORE UPDATE OF payload_json,payload_bytes,body_hash ON node WHEN OLD.payload_json IS NOT NULL AND NEW.payload_json IS NOT NULL BEGIN SELECT RAISE(ABORT,'source payload must be final at INSERT'); END;")?;
        Ok(())
    }).await.unwrap();
    let (rpc_send, rpc_receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, rpc_send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(rpc_receive));
    let thread = request(&control, Command::ThreadCreate(defaults())).await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let file = root.path().join("tool-image.png");
    std::fs::write(&file, b"synthetic-tool-image").unwrap();
    send(&control, &thread, "human request without an attachment").await;
    let first = incoming(&control, &thread, &mut receive).await;
    let op = operation(&control);
    let command = format!(
        "'{}' --operation-id '{}' attachment add --path '{}' --kind image",
        env!("CARGO_BIN_EXE_shellboy"),
        op,
        file.display()
    );
    // Repeat the exact operation in the same authenticated process context.
    let command = format!("{command}; {command}");
    first.reply.send((200,json!({"id":"synthetic-tool-response","status":"completed","output":[
        {"type":"function_call","call_id":"attach-call","name":"shell","arguments":json!({"command":command,"background":false}).to_string()},
        {"type":"function_call","call_id":"tail-call","name":"shell","arguments":json!({"command":"printf TOOL_GROUP_TAIL","background":false}).to_string()}
    ]}))).unwrap();
    let next = match timeout(Duration::from_secs(5), receive.recv()).await {
        Ok(Some(next)) => next,
        other => {
            let diagnostic=control.store.database.call(|db|{
                let mut result=serde_json::Map::new();
                for (name,query) in [
                    ("turns","SELECT json_object('id',id,'state',state,'reason',recovery_reason_json) FROM turn"),
                    ("attempts","SELECT json_object('id',id,'state',state,'error',error_json) FROM model_attempt"),
                    ("shells","SELECT json_object('id',id,'state',state,'exit',exit_code,'complete',output_complete) FROM shell_execution"),
                    ("raw","SELECT json_object('id',execution_id,'stream',stream,'text',substr(CAST(bytes AS TEXT),1,2048)) FROM output_chunk"),
                    ("delivery","SELECT json_object('node',node_id,'status',status) FROM attachment_delivery"),
                    ("operations","SELECT json_object('id',operation_id,'state',state,'result',result_ref_json) FROM operation_receipt"),
                ] {
                    let rows=db.prepare(query)?.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    result.insert(name.into(),json!(rows));
                }
                Ok(Value::Object(result))
            }).await;
            control.shutdown().await.unwrap();
            panic!(
                "post-tool request unavailable (timed_out={}): diagnostic={diagnostic:?}",
                other.is_err()
            );
        }
    };
    let current = manifest(&control, &thread).await;
    assert_eq!(
        attachments(&current).len(),
        1,
        "captured API: {}",
        next.body
    );
    let entry = &attachments(&current)[0];
    let position = entry["api_item_index"].as_u64().unwrap() as usize;
    let input = next.body["input"].as_array().unwrap();
    let outputs: Vec<usize> = input
        .iter()
        .enumerate()
        .filter(|(_, v)| v["type"] == "function_call_output")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(outputs.len(), 2);
    let tail = input
        .iter()
        .find(|v| v["type"] == "function_call_output" && v["call_id"] == "tail-call")
        .unwrap();
    let tail: Value = serde_json::from_str(tail["output"].as_str().unwrap()).unwrap();
    assert_eq!(
        tail["status"], "exited",
        "attachment acceptance must not interrupt its tool group: {tail}"
    );
    assert!(tail["output"].as_str().unwrap().contains("TOOL_GROUP_TAIL"));
    assert!(
        outputs.iter().all(|i| *i < position),
        "attachment event must follow all corresponding tool results"
    );
    let original = input
        .iter()
        .find(|v| v["content"][0]["text"] == "human request without an attachment")
        .unwrap();
    assert_eq!(original["content"].as_array().unwrap().len(), 1);
    assert!(
        input[position]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Shellboy attachment")
    );
    assert_eq!(next.body.to_string().matches("input_image").count(), 1);
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM attachment_ref", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM attachment_delivery", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    finish(&control, &thread, next).await;
    local.shutdown().await.unwrap();
    serving.abort();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compacted_attachment_source_is_replaced_without_sending_its_file_entity() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let (thread, accepted) = ordered_draft(&control, root.path(), &mut receive).await;
    let first = incoming(&control, &thread, &mut receive).await;
    finish(&control, &thread, first).await;
    for text in ["newer completed turn", "retain latest completed turn"] {
        send(&control, &thread, text).await;
        let next = incoming(&control, &thread, &mut receive).await;
        finish(&control, &thread, next).await;
    }
    let selected = thread.clone();
    let source = accepted["message_id"].as_str().unwrap().to_owned();
    let source_copy = source.clone();
    let (original_path, original_source) = control
        .store
        .database
        .call(move |db| {
            let path = db
                .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
                .query_map([selected], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let source = db.query_row(
                "SELECT payload_json,payload_revision,body_hash FROM node WHERE id=?1",
                [source_copy],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )?;
            Ok((path, source))
        })
        .await
        .unwrap();
    let fork = request(
        &control,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: thread.clone(),
            node_id: original_path.last().unwrap().clone(),
            cwd: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let compact = request(
        &control,
        Command::Compact(ThreadTarget {
            thread_id: Some(thread.clone()),
        }),
    )
    .await;
    let summary = incoming(&control, &thread, &mut receive).await;
    assert_eq!(summary.body["tools"], json!([]));
    summary
        .reply
        .send((
            200,
            answer("## 目的\nSynthetic attachment source summarized."),
        ))
        .unwrap();
    let mut compact_state = Value::Null;
    let compact_finished = timeout(Duration::from_secs(5), async {
        loop {
            let state = request(
                &control,
                Command::OperationGet(OperationTarget {
                    operation_id: compact["operation_id"].as_str().unwrap().into(),
                }),
            )
            .await;
            let complete = state["downstream"]["outcome"]["result"]["summary_node_id"].is_string();
            compact_state = state;
            if complete {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        compact_finished.is_ok(),
        "compaction did not publish its summary within the fixture watchdog; last operation state: {compact_state}"
    );
    let resumed = request(
        &control,
        Command::MessageSend(MessageSendArgs {
            thread_id: Some(thread.clone()),
            text: "after compaction".into(),
        }),
    )
    .await;
    let next = incoming(&control, &thread, &mut receive).await;
    assert!(!next.body.to_string().contains("input_image"));
    assert!(!next.body.to_string().contains("attachment_ref"));
    assert!(next.body.to_string().contains("context_replacement"));
    assert!(next.body.to_string().contains("image-a.png"));
    let current = manifest(&control, &thread).await;
    assert_eq!(attachments(&current).len(), 1);
    let notice = &attachments(&current)[0];
    assert_eq!(notice["state"], "excluded");
    assert_eq!(notice["source_node_id"], source);
    assert!(notice["content_hash"].is_null());
    let item = notice["api_item_index"].as_u64().unwrap() as usize;
    assert_eq!(next.body["input"][item]["role"], "developer");
    next.reply
        .send((500, json!({"error":{"message":"synthetic notice failure"}})))
        .unwrap();
    settled(&control, &thread).await;
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: resumed["run_id"].as_str().unwrap().into(),
        }),
    )
    .await;
    let retry = incoming(&control, &thread, &mut receive).await;
    assert!(
        retry.body.to_string().contains("context_replacement"),
        "failed request does not consume the notice"
    );
    finish(&control, &thread, retry).await;
    send(&control, &thread, "notice already consumed").await;
    let once = incoming(&control, &thread, &mut receive).await;
    assert!(!once.body.to_string().contains("context_replacement"));
    assert!(attachments(&manifest(&control, &thread).await).is_empty());
    finish(&control, &thread, once).await;
    send(&control, &fork, "fork retained its active attachment").await;
    let forked = incoming(&control, &fork, &mut receive).await;
    assert!(forked.body.to_string().contains("input_image"));
    assert_eq!(
        attachments(&manifest(&control, &fork).await)[0]["state"],
        "ready"
    );
    finish(&control, &fork, forked).await;
    let selected = thread.clone();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let revision: i64 = tx.query_row(
                "SELECT selection_revision FROM thread WHERE id=?1",
                [&selected],
                |r| r.get(0),
            )?;
            shellboy::history::select_path(
                &tx,
                &selected,
                revision as u64,
                &original_path,
                "fixture restore",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    send(&control, &thread, "explicit original path selection").await;
    let restored = incoming(&control, &thread, &mut receive).await;
    assert!(
        !restored.body.to_string().contains("input_image"),
        "path selection does not reactivate excluded binaries"
    );
    let restored_manifest = manifest(&control, &thread).await;
    let restored_entry = &attachments(&restored_manifest)[0];
    assert_eq!(restored_entry["state"], "excluded");
    assert_position(
        &restored.body,
        restored_entry,
        "before A",
        Some("after A"),
        "input_text",
    );
    finish(&control, &thread, restored).await;
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT payload_json,payload_revision,body_hash FROM node WHERE id=?1",
                    [source],
                    |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?
                    ))
                )?,
                original_source
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
async fn unavailable_new_attachment_is_consumed_by_success_without_a_fabricated_sent_hash() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    let accepted = control
        .send_draft(
            operation(&control),
            defaults(),
            None,
            "inspect missing file".into(),
            vec![AttachmentInput {
                path: root.path().join("not-created.txt"),
                kind: AttachmentKind::File,
            }],
        )
        .await
        .unwrap();
    let thread = accepted["thread_id"].as_str().unwrap().to_owned();
    let first = incoming(&control, &thread, &mut receive).await;
    let failed_manifest = manifest(&control, &thread).await;
    assert_eq!(attachments(&failed_manifest)[0]["state"], "unavailable");
    assert!(attachments(&failed_manifest)[0]["content_hash"].is_null());
    let id = attachments(&failed_manifest)[0]["attachment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    first
        .reply
        .send((500, json!({"error":{"message":"synthetic failure"}})))
        .unwrap();
    settled(&control, &thread).await;
    let copy = id.clone();
    control
        .store
        .database
        .call(move |db| {
            assert!(db.query_row(
                "SELECT protected FROM attachment_selection WHERE attachment_id=?1",
                [copy],
                |r| r.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .await
        .unwrap();
    request(
        &control,
        Command::RunRetry(RunTarget {
            run_id: accepted["run_id"].as_str().unwrap().into(),
        }),
    )
    .await;
    let retry = incoming(&control, &thread, &mut receive).await;
    assert!(retry.body.to_string().contains("Attachment unavailable"));
    finish(&control, &thread, retry).await;
    control.store.database.call(move|db|{
        assert!(!db.query_row("SELECT protected FROM attachment_selection WHERE attachment_id=?1",[&id],|r|r.get::<_,bool>(0))?);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM attachment_send WHERE attachment_id=?1 AND (state='sent' OR content_hash IS NOT NULL)",[id],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
