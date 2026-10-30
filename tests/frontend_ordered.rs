//! Real UI -> frontend -> receipt/source -> HTTP ordered human inputs.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, frontend, ipc::LocalServer, protocol::*, ui::*};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::timeout,
};
struct Incoming {
    body: Value,
    reply: oneshot::Sender<()>,
}
async fn proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (send, receive) = mpsc::channel(8);
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                let body = loop {
                    let n = stream.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(at) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                        let size: usize = String::from_utf8_lossy(&bytes[..at])
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|s| s.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= at + 4 + size {
                            break serde_json::from_slice(&bytes[at + 4..at + 4 + size]).unwrap();
                        }
                    }
                };
                let (reply, done) = oneshot::channel();
                if send.send(Incoming { body, reply }).await.is_err() {
                    return;
                }
                if done.await.is_err() {
                    return;
                }
                let body=json!({"id":"answer","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"saved"}]}]}).to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
        }
    });
    (url, receive, server)
}
async fn incoming(receive: &mut mpsc::Receiver<Incoming>) -> Incoming {
    timeout(Duration::from_secs(5), receive.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn update<T>(
    receive: &std::sync::mpsc::Receiver<UiUpdate>,
    mut take: impl FnMut(UiUpdate) -> Option<T>,
) -> T {
    timeout(Duration::from_secs(5), async {
        loop {
            match receive.try_recv() {
                Ok(UiUpdate::SavedMessage {
                    draft_id,
                    revision,
                    thread_id,
                    status,
                    history,
                    ..
                }) => {
                    // Observe the receipt fact and the paired history carried by the atomic ACK.
                    if let Some(value) = take(UiUpdate::Receipt {
                        draft_id,
                        revision,
                        thread_id: Some(thread_id),
                        status,
                        message: None,
                    }) {
                        break value;
                    }
                    if let Some(value) = take(*history) {
                        break value;
                    }
                }
                Ok(value) => {
                    if let Some(value) = take(value) {
                        break value;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(2)).await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap()
}
async fn settled(control: &Control, thread: &str) {
    let thread = thread.to_owned();
    timeout(Duration::from_secs(5),async {loop {let thread=thread.clone();if control.store.database.call(move|db|Ok(db.query_row("SELECT NOT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state IN ('live','stopping'))",[thread],|r|r.get::<_,bool>(0))?)).await.unwrap(){break;}tokio::time::sleep(Duration::from_millis(2)).await;}}).await.unwrap();
}
async fn send(control: &Arc<Control>, thread: &str, text: &str) {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            Command::MessageSend(MessageSendArgs {
                now: false,
                thread_id: Some(thread.into()),
                text: text.into(),
            }),
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
}
fn ordered_wire(body: &Value) -> [Value; 2] {
    let content = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|item| {
            item["content"]
                .as_array()
                .filter(|content| content.first().is_some_and(|v| v["text"] == "前"))
        })
        .unwrap();
    assert_eq!(
        content
            .iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "input_text",
            "input_text",
            "input_image",
            "input_text",
            "input_text",
            "input_file",
            "input_text"
        ]
    );
    assert_eq!(content[3]["text"], "中");
    assert_eq!(content[6]["text"], "後");
    assert!(!body.to_string().contains('\u{fffc}'));
    assert!(!body.to_string().contains("attachment_ref"));
    [1, 4].map(|index| {
        serde_json::from_str(
            content[index]["text"]
                .as_str()
                .unwrap()
                .strip_prefix("[Shellboy attachment metadata] ")
                .unwrap(),
        )
        .unwrap()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn human_chips_keep_source_positions_through_followups_edit_restore_and_owner_restart() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let root = tempfile::tempdir().unwrap();
    let (url, mut requests, server) = proxy().await;
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='{url}'\n")).unwrap();
    let image = root.path().join("a.png");
    let file = root.path().join("b.txt");
    std::fs::write(&image, b"synthetic image").unwrap();
    std::fs::write(&file, b"synthetic file").unwrap();
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    control.store.database.call(|db|{
        db.execute_batch("CREATE TRIGGER immutable_payload_guard BEFORE UPDATE OF payload_json,payload_bytes,body_hash ON node WHEN OLD.payload_json IS NOT NULL AND NEW.payload_json IS NOT NULL BEGIN SELECT RAISE(ABORT,'source payload must be final at INSERT'); END;")?;
        Ok(())
    }).await.unwrap();
    let observer = control.clone();
    let (rpc, rpcs) = mpsc::channel(16);
    let local = LocalServer::bind(&owner, rpc).await.unwrap();
    tokio::spawn(control.clone().serve(rpcs));
    let (actions, receiver) = mpsc::channel(16);
    let (updates, events) = std::sync::mpsc::sync_channel(256);
    let bridge = tokio::spawn(frontend::run(
        control,
        receiver,
        updates,
        root.path().into(),
        owner,
        local,
    ));
    let mut ui = UiState::new(&shellboy::config::Config::default(), root.path().into());
    ui.insert("前", false);
    for (id, path, kind, text) in [
        ("image", image, "image", "中"),
        ("file", file, "file", "後"),
    ] {
        ui.apply(UiUpdate::ClipboardAttachment {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            attachment: UiAttachment {
                id: id.into(),
                path,
                kind: kind.into(),
                state: "ready".into(),
                position: None,
            },
        });
        ui.insert(text, false);
    }
    let snapshot = ui.view().draft.clone();
    assert_eq!(snapshot.text, "前\u{fffc}中\u{fffc}後");
    for action in ui.event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))) {
        actions.send(action).await.unwrap();
    }
    let first = incoming(&mut requests).await;
    ordered_wire(&first.body);
    let thread = update(&events, |value| match value {
        UiUpdate::Receipt {
            status: ReceiptStatus::Accepted,
            thread_id: Some(thread),
            ..
        } => Some(thread),
        _ => None,
    })
    .await;
    let target = thread.clone();
    let (source,original,ids)=observer.store.database.call(move|db|{
        let (source,body):(String,String)=db.query_row("SELECT n.id,n.payload_json FROM node n JOIN receipt r ON r.message_node_id=n.id WHERE r.target_thread_id=?1",[target],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let payload:Value=serde_json::from_str(&body).unwrap();let c=payload["content"].as_array().unwrap();assert_eq!(c.len(),5);assert_eq!(c[0]["text"],"前");assert_eq!(c[2]["text"],"中");assert_eq!(c[4]["text"],"後");assert!(!body.contains('\u{fffc}'));
        let ids=vec![c[1]["attachment_id"].as_str().unwrap().to_owned(),c[3]["attachment_id"].as_str().unwrap().to_owned()];
        let manifest:String=db.query_row("SELECT input_manifest_json FROM model_attempt ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0))?;let manifest:Value=serde_json::from_str(&manifest).unwrap();
        for (entry,index) in manifest["attachments"].as_array().unwrap().iter().zip([1,3]) {assert_eq!(entry["source_node_id"],source);assert_eq!(entry["source_element_index"],index);assert_eq!(entry["first_inclusion"],true);}
        Ok((source,body,ids))
    }).await.unwrap();
    first.reply.send(()).unwrap();
    settled(&observer, &thread).await;
    for text in ["次1", "次2"] {
        send(&observer, &thread, text).await;
        let next = incoming(&mut requests).await;
        let metadata = ordered_wire(&next.body);
        assert!(
            metadata
                .iter()
                .all(|value| value["first_inclusion"] == false)
        );
        assert!(
            next.body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["content"][0]["text"] == text)
        );
        next.reply.send(()).unwrap();
        settled(&observer, &thread).await;
    }
    actions
        .send(UiAction::RestoreSnapshot {
            draft_id: snapshot.draft_id.clone(),
            revision: snapshot.revision,
            node_id: source.clone(),
            expected_revision: 99,
        })
        .await
        .unwrap();
    let (restored, restored_refs) = update(&events, |v| match v {
        UiUpdate::RestoredSnapshot {
            text, attachments, ..
        } => Some((text, attachments)),
        _ => None,
    })
    .await;
    assert_eq!(restored, snapshot.text);
    assert_eq!(
        restored_refs.iter().map(|a| a.position).collect::<Vec<_>>(),
        snapshot
            .attachments
            .iter()
            .map(|a| a.position)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        restored_refs
            .iter()
            .map(|a| a.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    actions
        .send(UiAction::LoadEditRequest {
            thread_id: thread.clone(),
            node_id: source.clone(),
            generation: 0,
            draft_revision: 0,
        })
        .await
        .unwrap();
    let (text, attachments) = update(&events, |v| match v {
        UiUpdate::EditRequestLoaded {
            text,
            attachments,
            run_id,
            ..
        } => {
            assert!(run_id.is_none());
            Some((text, attachments))
        }
        _ => None,
    })
    .await;
    assert_eq!(text, snapshot.text);
    actions
        .send(UiAction::SendEdited {
            generation: 0,
            now: false,
            snapshot: None,
            draft_id: "edit".into(),
            revision: 1,
            source_thread_id: thread.clone(),
            source_node_id: source.clone(),
            cwd: root.path().into(),
            text,
            attachments,
            connection: None,
            model: None,
            effort: None,
            confirmed_run_id: None,
        })
        .await
        .unwrap();
    let edited = incoming(&mut requests).await;
    let metadata = ordered_wire(&edited.body);
    assert!(
        metadata
            .iter()
            .all(|value| value["first_inclusion"] == true)
    );
    assert!(ids.iter().all(|id| !edited.body.to_string().contains(id)));
    edited.reply.send(()).unwrap();
    let edited_thread = update(&events, |v| match v {
        UiUpdate::Receipt {
            draft_id,
            thread_id: Some(thread),
            status: ReceiptStatus::Accepted,
            ..
        } if draft_id == "edit" => Some(thread),
        _ => None,
    })
    .await;
    settled(&observer, &edited_thread).await;
    let source_copy = source.clone();
    observer
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT payload_json FROM node WHERE id=?1",
                    [source_copy],
                    |r| r.get::<_, String>(0)
                )?,
                original
            );
            Ok(())
        })
        .await
        .unwrap();
    // This is a new unsent edit of the original content. Its already submitted
    // revision must not be resurrected by a late autosave.
    let mut unsent = snapshot.clone();
    unsent.revision += 1;
    actions
        .send(UiAction::SaveDraft {
            thread_id: thread.clone(),
            draft: unsent.clone(),
            anchor: ViewAnchor::default(),
            follow: false,
        })
        .await
        .unwrap();
    actions.send(UiAction::Quit).await.unwrap();
    timeout(Duration::from_secs(8), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(observer);
    let owner = DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let target = thread.clone();
    let saved: DraftSnapshot = control
        .store
        .database
        .call(move |db| {
            let body: String = db.query_row(
                "SELECT body_json FROM draft WHERE thread_id=?1 AND state='editing'",
                [target],
                |r| r.get(0),
            )?;
            Ok(serde_json::from_str(&body).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(saved, unsent);
    assert_eq!(saved.text, snapshot.text);
    assert_eq!(saved.cursor, snapshot.cursor);
    assert_eq!(saved.folds, snapshot.folds);
    assert_eq!(saved.attachments, snapshot.attachments);
    send(&control, &thread, "再起動後").await;
    let rebuilt = incoming(&mut requests).await;
    assert!(
        ordered_wire(&rebuilt.body)
            .iter()
            .all(|value| value["first_inclusion"] == false)
    );
    rebuilt.reply.send(()).unwrap();
    settled(&control, &thread).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
