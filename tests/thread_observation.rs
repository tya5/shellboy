//! Real owner/loopback response observations; no live provider or user data.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

async fn call(control: &Arc<Control>, command: Command) -> Value {
    let result = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(result.ok, "{:?}", result.error);
    result.result.unwrap()
}
fn settled(ids: Vec<String>) -> Command {
    Command::Wait(WaitArgs {
        conditions: vec![WaitCondition::ThreadSettled {
            thread_ids: ids,
            states: vec![
                "completed".into(),
                "failed".into(),
                "recovery_wait".into(),
                "stopped".into(),
            ],
        }],
        timeout_ms: crate::test_watchdog::WATCHDOG.as_millis() as u64,
        after: None,
    })
}
#[tokio::test]
async fn multiple_threads_and_last_answer_input_stay_distinct_during_a_new_request() {
    crate::test_watchdog::watchdog("loopback Thread observations",async {
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let (send,mut incoming)=mpsc::channel::<oneshot::Sender<()>>(2);
        let server=tokio::spawn(async move {
            for text in ["first answer","second answer"] {
                let (mut stream,_)=listener.accept().await.unwrap();let mut bytes=Vec::new();let mut chunk=[0;8192];
                loop {
                    let n=stream.read(&mut chunk).await.unwrap();assert!(n>0);bytes.extend_from_slice(&chunk[..n]);assert!(bytes.len()<1_048_576);
                    if let Some(i)=bytes.windows(4).position(|v|v==b"\r\n\r\n") {
                        let head=String::from_utf8_lossy(&bytes[..i]);
                        let count:usize=head.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap();
                        if bytes.len()>=i+4+count {let body:Value=serde_json::from_slice(&bytes[i+4..i+4+count]).unwrap();assert_eq!(body["stream"],true);break;}
                    }
                }
                let (release,gate)=oneshot::channel();send.send(release).await.unwrap();gate.await.unwrap();
                let body=json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]}).to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let root=tempfile::tempdir().unwrap();let config=root.path().join("config.toml");
        std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\n")).unwrap();
        let owner=DataOwner::start(Some(config),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());
        let first=call(&control,Command::ThreadCreate(ThreadCreateArgs { agents:None, connection:None, cwd:None, model:None, effort:None, parent_id:None })).await;
        let other=call(&control,Command::ThreadCreate(ThreadCreateArgs { agents:None, connection:None, cwd:None, model:None, effort:None, parent_id:None })).await;
        let thread=first["thread_id"].as_str().unwrap().to_owned();let other=other["thread_id"].as_str().unwrap().to_owned();
        call(&control,Command::MessageSend(MessageSendArgs{thread_id:Some(thread.clone()),text:"first input".into(),sender_label:Some("initial".into()),now:false})).await;
        incoming.recv().await.unwrap().send(()).unwrap();
        let done=call(&control,settled(vec![thread.clone(),other])).await;
        assert_eq!(done["status"],"satisfied");assert_eq!(done["snapshot"]["changes"][0]["thread_id"],thread);
        call(&control,Command::MessageSend(MessageSendArgs{thread_id:Some(thread.clone()),text:"new input".into(),sender_label:Some("self-asserted-reviewer".into()),now:false})).await;
        let release=incoming.recv().await.unwrap();
        let details=call(&control,Command::ThreadGet(ThreadTarget{thread_id:Some(thread.clone())})).await;
        assert_eq!(details["last_answer"]["excerpt"],"first answer");
        assert!(details["last_answer"]["turn_id"].is_string());assert!(details["last_answer"]["at"].is_string());
        assert_eq!(details["last_input"]["excerpt"],"new input");
        assert_eq!(details["last_input"]["sender"]["kind"],"external_cli");
        assert_eq!(details["last_input"]["sender"]["label"],"self-asserted-reviewer");
        let page=call(&control,Command::ThreadList(ThreadListArgs{query:Some(thread.clone()),..Default::default()})).await;
        assert_eq!(page["items"][0]["last_answer"],details["last_answer"]);
        assert_eq!(page["items"][0]["last_input"],details["last_input"]);
        release.send(()).unwrap();call(&control,settled(vec![thread.clone()])).await;
        let latest=call(&control,Command::ThreadGet(ThreadTarget{thread_id:Some(thread)})).await;
        assert_eq!(latest["last_answer"]["excerpt"],"second answer");
        assert_eq!(latest["last_input"]["excerpt"],"new input");
        server.await.unwrap();control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}
