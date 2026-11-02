//! Registered frontend/real local IPC list observations, synthetic saved facts.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
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
fn draw(ui: &mut shellboy::ui::UiState) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
    terminal
        .draw(|frame| shellboy::ui::draw(frame, ui))
        .unwrap();
    let mut text = String::new();
    for y in 0..40 {
        let mut x = 0;
        while x < 140 {
            let symbol = terminal.backend().buffer()[(x, y)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push('\n');
    }
    text
}
fn context(action: &UiAction) -> Option<ManagementContext> {
    match action {
        UiAction::SubmittedCommand { action, .. } => context(action),
        UiAction::ManagementIntent { context, .. } => Some(context.clone()),
        _ => None,
    }
}
fn gesture(ui: &mut shellboy::ui::UiState, code: KeyCode, modifiers: KeyModifiers) {
    assert!(
        ui.event(Event::Key(KeyEvent::new(code, modifiers)))
            .is_empty()
    );
}
fn paste(ui: &mut shellboy::ui::UiState, text: &str) {
    assert!(ui.event(Event::Paste(text.into())).is_empty());
}
fn submit_list(ui: &mut shellboy::ui::UiState, route: &str) -> (Vec<UiAction>, ManagementContext) {
    gesture(ui, KeyCode::F(8), KeyModifiers::NONE);
    paste(
        ui,
        match route {
            "status" => "/status",
            "thread" => "thread list",
            "shell" => "shell list",
            _ => "compact list",
        },
    );
    gesture(ui, KeyCode::Enter, KeyModifiers::NONE);
    if route == "thread" {
        gesture(ui, KeyCode::Char('u'), KeyModifiers::CONTROL);
        paste(ui, "1");
    } else if route == "status" {
        gesture(ui, KeyCode::Tab, KeyModifiers::NONE);
        paste(ui, "scope");
        for _ in 0..4 {
            gesture(ui, KeyCode::Tab, KeyModifiers::NONE);
        }
        gesture(ui, KeyCode::Right, KeyModifiers::NONE);
        gesture(ui, KeyCode::Tab, KeyModifiers::NONE);
        gesture(ui, KeyCode::Char('u'), KeyModifiers::CONTROL);
        paste(ui, "20");
    } else {
        paste(ui, "scope");
    }
    let pending = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let owner = pending
        .iter()
        .find_map(context)
        .expect("real F8 form must scope the submitted command");
    (pending, owner)
}
async fn dispatch(actions: &mpsc::Sender<UiAction>, pending: Vec<UiAction>) {
    for action in pending {
        actions.send(action).await.unwrap();
    }
}
async fn paint_changed(
    ui: &mut shellboy::ui::UiState,
    actions: &mpsc::Sender<UiAction>,
    expected: &str,
) {
    // The UI caps current plus retained fields at 256. Home does not move this
    // list, so use its supported Up key before checking every possible viewport.
    for _ in 0..256 {
        gesture(ui, KeyCode::Up, KeyModifiers::NONE);
    }
    for _ in 0..256 {
        let screen = draw(ui);
        dispatch(actions, ui.take_deferred_actions()).await;
        if screen.contains(expected) {
            return;
        }
        gesture(ui, KeyCode::Down, KeyModifiers::NONE);
    }
    panic!("changed metadata not painted: {expected}\n{}", draw(ui));
}
fn apply(ui: &mut shellboy::ui::UiState, update: UiUpdate) -> Option<InspectionSnapshot> {
    fn saved(update: &UiUpdate) -> Option<InspectionSnapshot> {
        match update {
            UiUpdate::ScopedManagement { update, .. } => saved(update),
            UiUpdate::InspectionSnapshot { snapshot, .. } => Some(snapshot.clone()),
            _ => None,
        }
    }
    let proof = saved(&update);
    ui.apply(update);
    proof
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
    for (route, language) in ["status", "thread", "shell", "compact"]
        .into_iter()
        .flat_map(|route| [(route, "ja"), (route, "en")])
    {
        crate::test_watchdog::watchdog("registered list observation IPC route", async {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join("config.toml");
            let data = root.path().join("data");
            std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[ui]\nlanguage='{language}'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n")).unwrap();
            let owner = DataOwner::start(Some(config),Some(data.clone()),root.path().into()).await.unwrap();
            let control = Control::new(&owner,root.path().into());
            control.start().await.unwrap();
            let cwd = root.path().to_string_lossy().into_owned();
            control.store.database.call(move |db| {
                db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('scope','initial',?1,'','fixture','fixture','2026-11-01T00:00:00Z')",[&cwd])?;
                db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('other','older',?1,'','fixture','fixture','2026-10-31T00:00:00Z')",[&cwd])?;
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
            let mut ui = shellboy::ui::UiState::new(&owner.loaded.config,root.path().into());
            let bridge = tokio::spawn(frontend::run_with_clipboard(control.clone(),receive,updates,root.path().into(),owner,server,Arc::new(NoClipboard)));
            let (pending, context) = submit_list(&mut ui,route);
            dispatch(&actions,pending).await;
            let target = loop {
                let update = incoming.recv().await.expect("frontend closed before retained target");
                let target = started(&update);
                ui.apply(update);
                if let Some(target) = target { break target; }
            };
            assert!(matches!(target,InspectionTarget::Lists(_)));
            let _ = draw(&mut ui);
            dispatch(&actions,ui.take_deferred_actions()).await;
            let mut last_sequence = loop {
                if let Some(s) = apply(&mut ui,incoming.recv().await.unwrap()) {
                    assert_eq!(s.target,target); break s.metadata_sequence;
                }
            };
            let witness = root.path().join("ipc-observations.jsonl");
            std::fs::write(&witness,"").unwrap();
            for (round,state) in [(0,"failed"),(1,"stopped")] {
                let current = state.to_string();
                // Arrange native retained facts, then deliver the notification through real IPC.
                control.store.database.call(move |db| {
                    assert_eq!(db.execute("UPDATE run SET state=?1 WHERE id='r'",[&current])?,1);
                    assert_eq!(db.execute("UPDATE shell_execution SET state=?1 WHERE id='s'",[&current])?,1);
                    assert_eq!(db.execute("UPDATE app_meta SET value=json_set(value,'$.state',?1) WHERE key='compact.result.compact-op'",[current])?,1);
                    Ok(())
                }).await.unwrap();
                let title = format!("round-{round}");
                let mutation = Command::ThreadSet(serde_json::from_value(json!({"thread_id":"scope","title":title})).unwrap());
                let response = shellboy::ipc::call(&data,mutation.clone(),None,None).await.unwrap();
                assert!(response.ok,"{response:?}");
                let expected = if route=="thread" {title.as_str()} else {state};
                let pointer = match route {
                    "status" => "/current_source/current/items/0/current/state",
                    "thread" => "/current_source/current/items/0/title",
                    "shell" => "/current_source/current/rows/0/state",
                    _ => "/current_source/current/rows/0/downstream_state",
                };
                let s = loop {
                    if let Some(s) = apply(&mut ui,incoming.recv().await.unwrap()) {
                        assert_eq!(s.target,target,"retained scope changed");
                        // Missing/clipped current evidence must fail separately from paint.
                        let fact = s.fields.iter().find(|f|f.key==pointer)
                            .unwrap_or_else(||panic!("current field omitted before paint: {pointer}; fields={:?}",s.fields));
                        if s.metadata_sequence>last_sequence && fact.value==expected { break s; }
                    }
                };
                last_sequence=s.metadata_sequence;
                let fact = s.fields.iter().find(|f|f.key==pointer).unwrap();
                assert_eq!(fact.value,expected,"native fixture/current fact mismatch");
                // Common decoration localizes state, but not downstream_state.
                let displayed = match (route,state) {
                    ("status" | "shell","failed") => "\u{5931}\u{6557}",
                    ("status" | "shell","stopped") => "\u{505c}\u{6b62}\u{6e08}\u{307f}",
                    _ => expected,
                };
                assert_eq!(fact.displayed(),displayed,"unexpected display mapping at {pointer}");
                let painted = match (route, state, language) {
                    ("status" | "shell", "failed", "en") => "Failed",
                    ("status" | "shell", "stopped", "en") => "Stopped",
                    _ => displayed,
                };
                paint_changed(&mut ui,&actions,painted).await;
                let before = control.store.database.call(|db| Ok(db.query_row("SELECT revision,title FROM thread WHERE id='scope'",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?)).await.unwrap();
                let replay = shellboy::ipc::call(&data,mutation,Some(response.operation_id.clone()),None).await.unwrap();
                assert!(replay.ok);
                assert_eq!(replay.result,response.result,"saved same-ID result must replay");
                let read = shellboy::ipc::call(&data,Command::ThreadGet(serde_json::from_value(json!({"thread_id":"scope"})).unwrap()),None,None).await.unwrap();
                assert!(read.ok);
                let rollback = shellboy::ipc::call(&data,Command::ThreadSet(serde_json::from_value(json!({"thread_id":"scope","title":"SHOULD_NOT_COMMIT","connection":"missing-profile"})).unwrap()),None,None).await.unwrap();
                assert!(!rollback.ok);
                let after = control.store.database.call(|db| Ok(db.query_row("SELECT revision,title FROM thread WHERE id='scope'",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?)).await.unwrap();
                assert_eq!(before,after,"replay/read/rejected mutation changed saved row");
                if route == "thread" {
                    let reordered = shellboy::ipc::call(&data,Command::ThreadSet(serde_json::from_value(json!({"thread_id":"other","title":format!("newer-{round}")})).unwrap()),None,None).await.unwrap();
                    assert!(reordered.ok);
                    let current_page = shellboy::ipc::call(&data,Command::ThreadList(serde_json::from_value(json!({"limit":1})).unwrap()),None,None).await.unwrap();
                    assert_eq!(current_page.result.unwrap()["items"][0]["thread_id"],"other");
                    // The open target/page must remain the original identity/cursor.
                    actions.send(UiAction::ObserveInspection {context:context.clone(),target:target.clone(),visible:true,revision:1}).await.unwrap();
                }
                use std::io::Write;
                writeln!(std::fs::OpenOptions::new().append(true).open(&witness).unwrap(),"{}",json!({"route":route,"round":round,"sequence":last_sequence,"fact":expected})).unwrap();
            }
            assert_eq!(std::fs::read_to_string(&witness).unwrap().lines().count(),2);
            gesture(&mut ui,KeyCode::Esc,KeyModifiers::NONE);
            let _ = draw(&mut ui);
            dispatch(&actions,ui.take_deferred_actions()).await;
            assert!(ui.management.is_none(),"Esc must hide the inspected list");
            actions.send(UiAction::Quit).await.unwrap();
            bridge.await.unwrap().unwrap();
            drop(incoming);
            forwarder.join().unwrap();
            serving.await.unwrap();
        }).await.unwrap();
    }
}
