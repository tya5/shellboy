//! Registered frontend/real local IPC list observations, synthetic saved facts.
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    attachments::clipboard::{ClipboardPixels, CopyOutcome},
    config::CopyBackend,
    control::Control,
    frontend,
    protocol::*,
    ui::{InspectionSnapshot, InspectionTarget, ManagementContext, UiAction, UiUpdate},
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct NoClipboard;
impl frontend::ClipboardBackend for NoClipboard {
    fn capture(
        &self,
        _: u64,
        _: CancellationToken,
    ) -> frontend::ClipboardFuture<'_, ClipboardPixels> {
        Box::pin(async { panic!("list inspection must not access clipboard") })
    }
    fn copy(
        &self,
        _: String,
        _: CopyBackend,
        _: usize,
        _: u64,
        _: CancellationToken,
    ) -> frontend::ClipboardFuture<'_, CopyOutcome> {
        Box::pin(async { panic!("list inspection must not copy") })
    }
}
fn snapshot(update: UiUpdate) -> Option<InspectionSnapshot> {
    match update {
        UiUpdate::ScopedManagement { update, .. } => snapshot(*update),
        UiUpdate::InspectionSnapshot { snapshot, .. } => Some(snapshot),
        _ => None,
    }
}
fn started(update: &UiUpdate) -> Option<InspectionTarget> {
    match update {
        UiUpdate::ScopedManagement { update, .. } => started(update),
        UiUpdate::RetainedInspectionStarted { target, .. } => Some(target.clone()),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_captured_list_routes_observe_eight_ipc_changed_commits_without_retargeting() {
    for route in ["status", "thread", "shell", "compact"] {
        crate::test_watchdog::watchdog("registered list observation IPC route", async {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join("config.toml");
            let data = root.path().join("data");
            std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
            let owner = DataOwner::start(Some(config),Some(data.clone()),root.path().into()).await.unwrap();
            let control = Control::new(&owner,root.path().into());
            control.start().await.unwrap();
            let cwd = root.path().to_string_lossy().into_owned();
            control.store.database.call(move |db| {
                db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('scope','initial',?1,'','fixture','fixture','2026-11-01T00:00:00Z')",[&cwd])?;
                db.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','scope','completed','2026-11-01T00:00:00Z','2026-11-01T00:00:01Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('t','scope','r','completed','2026-11-01T00:00:00Z','2026-11-01T00:00:01Z');")?;
                db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('s','r','t','shell-op','exited',?1,'{\"command\":\"printf synthetic\"}','utf-8','utf-8','closed','2026-11-01T00:00:00Z')",[cwd])?;
                db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('compact-op','fixture','{}','hash','completed',?1,'2026-11-01T00:00:00Z')",[json!({"result":{"thread_id":"scope","status":"queued","context_edit":{"action":"restore"}}}).to_string()])?;
                db.execute("INSERT INTO app_meta(key,value)VALUES('compact.result.compact-op',?1)",[json!({"state":"completed","ended_at":"2026-11-01T00:00:01Z","outcome":{"result":{"omitted_nodes":0}}}).to_string()])?;
                Ok(())
            }).await.unwrap();
            let (rpc, requests) = mpsc::channel(16);
            let server = shellboy::ipc::LocalServer::bind(&owner,rpc).await.unwrap();
            let serving = tokio::spawn(control.clone().serve(requests));
            let (actions, receive) = mpsc::channel(64);
            let (updates, ui_receive) = std::sync::mpsc::sync_channel(128);
            let (forward, mut incoming) = mpsc::channel(128);
            let forwarder = std::thread::spawn(move || {
                while let Ok(update) = ui_receive.recv() {
                    if forward.blocking_send(update).is_err() { break; }
                }
            });
            let bridge = tokio::spawn(frontend::run_with_clipboard(control.clone(),receive,updates,root.path().into(),owner,server,Arc::new(NoClipboard)));
            let command = match route {
                "status" => Command::Status(serde_json::from_value(json!({"thread_id":"scope","all":true,"limit":20})).unwrap()),
                "thread" => Command::ThreadList(serde_json::from_value(json!({"limit":20})).unwrap()),
                "shell" => Command::ShellList(serde_json::from_value(json!({"thread_id":"scope","limit":20})).unwrap()),
                _ => Command::CompactList(serde_json::from_value(json!({"thread_id":"scope","limit":20})).unwrap()),
            };
            let context = ManagementContext { epoch:1, owner_draft:"viewer-draft".into(), owner_thread:None };
            actions.send(UiAction::ManagementIntent { context:context.clone(), action:Box::new(UiAction::Command(command)) }).await.unwrap();
            let target = loop {
                let update = incoming.recv().await.expect("frontend closed before retained target");
                if let Some(target) = started(&update) { break target; }
            };
            assert!(matches!(target,InspectionTarget::Lists(_)));
            actions.send(UiAction::ObserveInspection { context:context.clone(),target:target.clone(),visible:true,revision:1 }).await.unwrap();
            let mut last_sequence = loop {
                if let Some(s) = snapshot(incoming.recv().await.unwrap()) {
                    assert_eq!(s.target,target); break s.metadata_sequence;
                }
            };
            let witness = root.path().join("ipc-observations.jsonl");
            std::fs::write(&witness,"").unwrap();
            for (round,state) in [(0,"failed"),(1,"stopped")] {
                let current = state.to_string();
                // Arrange native retained facts, then deliver the notification through real IPC.
                control.store.database.call(move |db| {
                    db.execute("UPDATE run SET state=?1 WHERE id='r'",[&current])?;
                    db.execute("UPDATE shell_execution SET state=?1 WHERE id='s'",[&current])?;
                    db.execute("UPDATE app_meta SET value=json_set(value,'$.state',?1) WHERE key='compact.result.compact-op'",[current])?;
                    Ok(())
                }).await.unwrap();
                let title = format!("round-{round}");
                let response = shellboy::ipc::call(&data,Command::ThreadSet(serde_json::from_value(json!({"thread_id":"scope","title":title})).unwrap()),None,None).await.unwrap();
                assert!(response.ok,"{response:?}");
                let expected = if route=="thread" {title.as_str()} else {state};
                let s = loop {
                    if let Some(s) = snapshot(incoming.recv().await.unwrap()) {
                        assert_eq!(s.target,target,"retained scope changed");
                        if s.metadata_sequence>last_sequence && s.fields.iter().any(|f|f.value.contains(expected)) { break s; }
                    }
                };
                last_sequence=s.metadata_sequence;
                use std::io::Write;
                writeln!(std::fs::OpenOptions::new().append(true).open(&witness).unwrap(),"{}",json!({"route":route,"round":round,"sequence":last_sequence,"fact":expected})).unwrap();
            }
            assert_eq!(std::fs::read_to_string(&witness).unwrap().lines().count(),2);
            actions.send(UiAction::HideInspection {revision:2}).await.unwrap();
            actions.send(UiAction::Quit).await.unwrap();
            bridge.await.unwrap().unwrap();
            drop(incoming);
            forwarder.join().unwrap();
            serving.await.unwrap();
        }).await.unwrap();
    }
}
