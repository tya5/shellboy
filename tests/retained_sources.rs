//! Synthetic UI reads plus real local IPC mutations. No model/provider request.
use crate::test_watchdog::{Deadline, watchdog};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use shellboy::{
    app::DataOwner,
    control::Control,
    frontend, ipc,
    protocol::*,
    ui::{self, InspectionSnapshot, ManagementContext, UiAction, UiState, UiUpdate},
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc::Receiver},
};
struct Fixture {
    _root: tempfile::TempDir,
    data: PathBuf,
    control: Arc<Control>,
    ui: UiState,
    send: ui::actions::ActionSender,
    incoming: Receiver<UiUpdate>,
    bridge: tokio::task::JoinHandle<Result<(), ApiError>>,
}
impl Fixture {
    async fn start(shell: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        let data = root.path().join("data");
        std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n[indexing]\nauto_history=false\n[ui]\ncopy_backend='osc52'\n").unwrap();
        let owner = DataOwner::start(Some(config), Some(data.clone()), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        owner.db.call(move|db|{
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','fixture','fixture','now');
              INSERT INTO node(id,kind,created_at,payload_json,payload_revision,body_hash)VALUES('n','user','now','{\"role\":\"user\",\"content\":\"original n\"}',3,'hash-n'),('tail','assistant_response','now','{\"text\":\"original tail\"}',7,'hash-tail');
              INSERT INTO thread_path VALUES('t',0,'n'),('t',1,'tail');INSERT INTO edge(src,dst)VALUES('n','tail');UPDATE thread SET cursor_node_id='tail' WHERE id='t';")?;
            if shell {db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');
              INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,retention_generation,started_at)VALUES('e','r','u','op','running','/','{}','UTF-8','UTF-8','closed',1,2,'now');INSERT INTO output_chunk VALUES('e',0,'stdout',0,x'616263','now');")?;}Ok(())
        }).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let server = ipc::LocalServer::bind(&owner, tx).await.unwrap();
        tokio::spawn(control.clone().serve(rx));
        let mut state = UiState::new(&owner.loaded.config, root.path().into());
        let (send, receive) = ui::actions::channel(64);
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        let bridge = tokio::spawn(frontend::run(
            control.clone(),
            receive,
            updates,
            root.path().into(),
            owner,
            server,
        ));
        for action in state.select_thread("t".into()) {
            send.try_send(action).unwrap();
        }
        let mut f = Self {
            _root: root,
            data,
            control,
            ui: state,
            send,
            incoming,
            bridge,
        };
        let _ = f.until(|ui, _| !ui.view().loading).await;
        f
    }
    fn dispatch(&self, actions: Vec<UiAction>) {
        assert!(self.send.try_send_batch(actions).is_empty());
    }
    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let actions = self.ui.event(Event::Key(KeyEvent::new(code, modifiers)));
        self.dispatch(actions);
    }
    fn paste(&mut self, text: &str) {
        let actions = self.ui.event(Event::Paste(text.into()));
        self.dispatch(actions);
    }
    fn draw(&mut self) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.ui))
            .unwrap();
        let actions = self.ui.take_deferred_actions();
        self.dispatch(actions);
    }
    async fn until(
        &mut self,
        mut ready: impl FnMut(&UiState, Option<&(ManagementContext, u64, InspectionSnapshot)>) -> bool,
    ) -> Option<(ManagementContext, u64, InspectionSnapshot)> {
        let deadline = Deadline::new("retained source integration event");
        let mut proof = None;
        loop {
            self.draw();
            if ready(&self.ui, proof.as_ref()) {
                return proof;
            }
            let update = tokio::task::block_in_place(|| deadline.receive(&self.incoming))
                .expect("retained source update");
            if let UiUpdate::ScopedManagement { context, update } = &update
                && let UiUpdate::InspectionSnapshot {
                    request_id,
                    snapshot,
                } = &**update
            {
                proof = Some((context.clone(), *request_id, snapshot.clone()));
            }
            self.ui.apply(update);
        }
    }
    async fn changed(&self, title: &str) {
        let response = ipc::call(
            &self.data,
            Command::ThreadSet(ThreadSetArgs {
                thread_id: Some("t".into()),
                title: Some(title.into()),
                ..Default::default()
            }),
            Some(format!(
                "{}:{}",
                self.control.store.instance_id,
                uuid::Uuid::new_v4()
            )),
            None,
        )
        .await
        .unwrap();
        assert!(response.ok, "{response:?}");
    }
    async fn detail(
        &mut self,
        proof: &(ManagementContext, u64, InspectionSnapshot),
        pointer: &str,
    ) -> String {
        self.send
            .try_send(UiAction::ManagementIntent {
                context: proof.0.clone(),
                action: Box::new(UiAction::LoadManagementDetail {
                    request_id: proof.1,
                    detail_request_id: 91,
                    pointer: pointer.into(),
                    cursor: None,
                }),
            })
            .unwrap();
        let deadline = Deadline::new("retained original detail");
        loop {
            let update = tokio::task::block_in_place(|| deadline.receive(&self.incoming)).unwrap();
            let text = if let UiUpdate::ScopedManagement { update, .. } = &update {
                match &**update {
                    UiUpdate::ManagementDetail {
                        pointer: p, text, ..
                    } if p == pointer => Some(text.clone()),
                    UiUpdate::ManagementDetailFailure { error, .. } => panic!("{error}"),
                    _ => None,
                }
            } else {
                None
            };
            self.ui.apply(update);
            if let Some(text) = text {
                return text;
            }
        }
    }
    async fn close(mut self) {
        self.send.try_send(UiAction::Quit).unwrap();
        watchdog("retained source bridge shutdown", async {
            while !self.bridge.is_finished() {
                while let Ok(update) = self.incoming.try_recv() {
                    self.ui.apply(update);
                }
                tokio::task::yield_now().await;
            }
            self.bridge.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}
fn field<'a>(
    proof: &'a (ManagementContext, u64, InspectionSnapshot),
    key: &str,
) -> Option<&'a str> {
    proof
        .2
        .fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.as_str())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_relations_keep_original_revision_after_synthetic_gc_and_real_ipc_change() {
    watchdog("retained relations IPC",async {
        let mut f=Fixture::start(false).await;
        f.key(KeyCode::F(8),KeyModifiers::NONE);f.paste("history relations");f.key(KeyCode::Enter,KeyModifiers::NONE);
        f.key(KeyCode::Char('u'),KeyModifiers::CONTROL);f.paste("n");f.key(KeyCode::Tab,KeyModifiers::NONE);f.key(KeyCode::Right,KeyModifiers::NONE);f.key(KeyCode::Enter,KeyModifiers::NONE);
        let first=f.until(|_,p|p.is_some_and(|p|field(p,"/current_source/sources/1/body_available")==Some("true"))).await.unwrap();
        assert_ne!(field(&first,"/current_source/sources/1/selected"),Some("true"),"viewer Thread must not become a selection scope");
        let before=f.detail(&first,"/items/0/provenance/source_revision").await;assert!(before.contains('7'));
        f.control.store.database.call(|db|{db.execute("UPDATE node SET payload_json=NULL,payload_revision=8,deleted_reason='synthetic collection' WHERE id='tail'",[])?;Ok(())}).await.unwrap();
        f.changed("actual IPC notification after synthetic GC").await;
        let next=f.until(|_,p|p.is_some_and(|p|field(p,"/current_source/sources/1/body_available")==Some("false"))).await.unwrap();
        assert_eq!(next.2.target,first.2.target);assert!(next.2.metadata_sequence>first.2.metadata_sequence);
        assert_eq!(f.detail(&next,"/items/0/provenance/source_revision").await,before);
        assert_eq!(f.ui.view().thread_id.as_deref(),Some("t"));f.close().await;
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_shell_page_keeps_bytes_and_cursor_across_growth_terminal_and_tombstone() {
    watchdog("retained shell cursor IPC",async {
        let mut f=Fixture::start(true).await;
        f.key(KeyCode::F(8),KeyModifiers::NONE);f.paste("shell read");f.key(KeyCode::Enter,KeyModifiers::NONE);
        f.key(KeyCode::Char('u'),KeyModifiers::CONTROL);f.paste("e");f.key(KeyCode::Enter,KeyModifiers::NONE);
        let first=f.until(|_,p|p.is_some_and(|p|field(p,"/current_source/sources/0/saved_sequence")==Some("1"))).await.unwrap();
        let original=f.detail(&first,"/text").await;let original_cursor=f.detail(&first,"/cursor").await;assert!(original.contains("abc"));
        f.control.store.database.call(|db|{db.execute_batch("INSERT INTO output_chunk VALUES('e',1,'stdout',3,x'646566','now');UPDATE shell_execution SET saved_sequence=2,state='exited',exit_code=0,ended_at='now' WHERE id='e';")?;Ok(())}).await.unwrap();
        f.changed("actual IPC notification after output growth").await;
        let next=f.until(|_,p|p.is_some_and(|p|field(p,"/current_source/sources/0/current_state")==Some("exited")&&field(p,"/current_source/sources/0/saved_sequence")==Some("2"))).await.unwrap();
        assert_eq!(f.detail(&next,"/text").await,original);assert_eq!(f.detail(&next,"/cursor").await,original_cursor);
        // This is an EXPLICIT test-side repeat, not notification refresh.
        let token=serde_json::from_str::<String>(&original_cursor).unwrap_or_else(|_|original_cursor.clone());
        let read=ipc::call(&f.data,Command::ShellRead(ShellReadArgs{execution_id:"e".into(),cursor:Some(token),encoding:None,limit_bytes:1024}),Some(format!("{}:{}", f.control.store.instance_id, uuid::Uuid::new_v4())),None).await.unwrap();
        assert!(read.ok,"{read:?}");assert!(read.result.unwrap()["text"].as_str().unwrap().contains("def"));
        f.control.store.database.call(|db|{db.execute_batch("INSERT INTO app_meta(key,value)VALUES('raw_tombstone.e','{\"reason\":\"synthetic collection\"}');DELETE FROM output_chunk WHERE execution_id='e';UPDATE shell_execution SET retention_generation=3 WHERE id='e';")?;Ok(())}).await.unwrap();
        f.changed("actual IPC notification after raw loss").await;
        let gone=f.until(|_,p|p.is_some_and(|p|field(p,"/current_source/sources/0/raw_tombstone")==Some("true"))).await.unwrap();
        assert_eq!(f.detail(&gone,"/text").await,original);assert_eq!(f.detail(&gone,"/cursor").await,original_cursor);
        assert_eq!(gone.2.target,first.2.target);f.close().await;
    }).await.unwrap();
}
