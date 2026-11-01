//! Real IPC, a held WAL writer, and a counted loopback endpoint; no user data.
use serde_json::json;
use shellboy::{app::DataOwner, control::Control, ipc, protocol::*};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::oneshot};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readonly_preview_works_while_writer_is_held_and_never_calls_provider_or_queues() {
    crate::test_watchdog::watchdog("compact preview read-only IPC",async {
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let requests=Arc::new(AtomicUsize::new(0));let observed=requests.clone();let (stop,mut stopped)=oneshot::channel();
        let provider=tokio::spawn(async move {
            loop {tokio::select! {_=&mut stopped=>break,connection=listener.accept()=>{
                let (mut stream,_)=connection.unwrap();observed.fetch_add(1,Ordering::SeqCst);
                let _=stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            }}}
        });
        let root=tempfile::tempdir().unwrap();let config=root.path().join("config.toml");let data=root.path().join("data");
        std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='mock'\n[connections.fixture]\nbase_url='http://{address}/v1'\nresponse_max_retries=0\n[indexing]\nauto_history=false\n[ui]\ncopy_backend='osc52'\n")).unwrap();
        let owner=DataOwner::start(Some(config),Some(data.clone()),root.path().into()).await.unwrap();
        owner.db.call(|db|{
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','fixture','mock','now')",[])?;
            for (i,(id,role,text)) in [("a","user","alpha"),("b","assistant","beta"),("c","user","current")].into_iter().enumerate(){
                let payload=json!({"role":role,"content":text}).to_string();
                db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'message','now',?2)",rusqlite::params![id,payload])?;
                db.execute("INSERT INTO thread_path VALUES('t',?1,?2)",rusqlite::params![i as i64,id])?;
            }
            Ok(())
        }).await.unwrap();
        let control=Control::new(&owner,root.path().into());
        control.start().await.unwrap();
        let (send,receive)=tokio::sync::mpsc::channel(16);
        let server=ipc::LocalServer::bind(&owner,send).await.unwrap();let serve=tokio::spawn(control.clone().serve(receive));
        let before=owner.db.call(|db| {
            ["node","replacement","replacement_source","thread_path","app_meta","operation_receipt","model_attempt"].iter()
                .map(|table|db.query_row(&format!("SELECT count(*) FROM {table}"),[],|row|row.get::<_,i64>(0))).collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await.unwrap();
        let (ready,prepared)=oneshot::channel();let (release,wait)=std::sync::mpsc::channel::<()>();let database=owner.db.clone();
        let writer=tokio::spawn(async move {database.call(move|db| {
            let tx=db.transaction()?;tx.execute("UPDATE thread SET title='uncommitted' WHERE id='t'",[])?;
            let _=ready.send(());let _=wait.recv();tx.rollback()?;Ok(())
        }).await.unwrap()});
        prepared.await.unwrap();
        let requests_args=[
            CompactPreviewArgs{thread_id:Some("t".into()),..Default::default()},
            CompactPreviewArgs{thread_id:Some("t".into()),action:Some(ContextEditAction::Summary),from_node_id:Some("a".into()),to_node_id:Some("b".into()),..Default::default()},
            CompactPreviewArgs{thread_id:Some("t".into()),action:Some(ContextEditAction::Spill),from_node_id:Some("a".into()),to_node_id:Some("b".into()),..Default::default()},
            CompactPreviewArgs{thread_id:Some("t".into()),action:Some(ContextEditAction::Trim),to_node_id:Some("b".into()),..Default::default()},
            CompactPreviewArgs{thread_id:Some("t".into()),action:Some(ContextEditAction::Omit),from_node_id:Some("a".into()),to_node_id:Some("b".into()),..Default::default()},
            CompactPreviewArgs{thread_id:Some("t".into()),action:Some(ContextEditAction::Maximal),..Default::default()},
        ];
        for args in requests_args {
            let result=ipc::call(&data,Command::CompactPreview(Box::new(args)),None,None).await.unwrap();
            assert!(result.ok,"{:?}",result.error);let result=result.result.unwrap();assert_eq!(result["dry_run"],true);
            assert!(result["predicted_failure"].is_null(),"{result}");assert_eq!(result["provider_requests"],0);
        }
        drop(release);writer.await.unwrap();
        let after=owner.db.call(|db| {
            assert_eq!(db.query_row("SELECT title FROM thread WHERE id='t'",[],|r|r.get::<_,String>(0))?,"");
            ["node","replacement","replacement_source","thread_path","app_meta","operation_receipt","model_attempt"].iter()
                .map(|table|db.query_row(&format!("SELECT count(*) FROM {table}"),[],|row|row.get::<_,i64>(0))).collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await.unwrap();
        assert_eq!(after,before);assert_eq!(requests.load(Ordering::SeqCst),0);
        stop.send(()).unwrap();provider.await.unwrap();server.shutdown().await.unwrap();serve.await.unwrap();
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}
