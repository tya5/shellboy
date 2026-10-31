//! Opt-in compatibility check against the user's local LiteLLM proxy.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use shellboy::{app::DataOwner, attachments::AttachmentInput, control::Control, protocol::*};
use std::{sync::Arc, time::Duration};

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
async fn completed(control: &Control, thread: &str) {
    let thread = thread.to_owned();
    crate::test_watchdog::watchdog("tests/attachment_proxy.rs: completed",async {
        loop {
            let target=thread.clone();
            let state=control.store.database.call(move|db| {
                Ok(db.query_row("SELECT state,recovery_reason_json FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",[target],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?)))?)
            }).await.unwrap();
            if state.0=="completed" { break; }
            assert_ne!(state.0,"recovery_wait","{:?}",state.1);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in local LiteLLM 127.0.0.1:4000; two actual luna image requests"]
async fn historical_image_replays_at_its_source_with_local_luna() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection='local'\ndefault_model='gpt-5.6-luna'\n[connections.local]\nbase_url='http://127.0.0.1:4000/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let file = root.path().join("synthetic-blue.png");
    std::fs::write(&file,STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAeklEQVR4nO3PUQkAIBTAwBfCYGY3kSH8OITBAtxm7fN1wwUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWPHYBtHVhHjkcFvgAAAAASUVORK5CYII=").unwrap()).unwrap();
    let accepted=control.send_draft(
        format!("{}:{}",control.store.instance_id,uuid::Uuid::new_v4()),
        ThreadCreateArgs{ agents: None,connection:None,cwd:None,model:None,effort:None,parent_id:None},None,
        "This is a synthetic attachment compatibility check. Do not call tools. Briefly describe the image color.".into(),
        vec![AttachmentInput{path:file,kind:AttachmentKind::Image}],
    ).await.unwrap();
    let thread = accepted["thread_id"].as_str().unwrap();
    completed(&control, thread).await;
    request(
        &control,
        Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some(thread.into()),
            text: "This turn has no new attachment. Do not call tools. Reply briefly with OK."
                .into(),
        }),
    )
    .await;
    completed(&control, thread).await;
    let thread = thread.to_owned();
    control.store.database.call(move|db| {
        let rows=db.prepare("SELECT a.input_manifest_json FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND a.state='completed' ORDER BY a.rowid")?.query_map([thread],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(rows.len(),2);
        let first:Value=serde_json::from_str(&rows[0]).unwrap();
        let next:Value=serde_json::from_str(&rows[1]).unwrap();
        let a=&first["attachments"][0]; let b=&next["attachments"][0];
        assert_eq!(a["source_node_id"],b["source_node_id"]);
        assert_eq!(a["api_item_index"],b["api_item_index"]);
        assert_eq!(a["api_content_index"],b["api_content_index"]);
        assert_eq!(a["content_hash"],b["content_hash"]);
        assert_eq!(a["first_inclusion"],true); assert_eq!(b["first_inclusion"],false);
        assert_eq!(b["changed"],false);
        assert_eq!(db.query_row("SELECT count(*) FROM attachment_send WHERE state='sent'",[],|row|row.get::<_,i64>(0))?,2);
        Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
