//! Actual UI form -> frontend -> committed Control notification -> UI projection.
//! Synthetic local fixtures; mutations cross real local IPC (no subprocess CLI or native-latency proof).
use super::*;
use crate::ui::{
    InspectionSnapshot, InspectionTarget, ManagementContext, UiAction, UiState, UiUpdate,
    actions::{self, ActionSender},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc::Receiver;

// Use the public local transport with the exact generated operation/origin.
// Fixture SQL only prepares synthetic starting data.
async fn external_request(control: &Control, request: Request) -> Response<Value> {
    let data_dir = control.store.database.path().parent().unwrap();
    let operation_id = request.operation_id;
    let response = crate::ipc::call(
        data_dir,
        request.command,
        Some(operation_id.clone()),
        request.origin,
    )
    .await
    .expect("fixture external IPC request must reach the bound owner");
    assert_eq!(response.operation_id, operation_id);
    response
}

struct NoClipboard;
impl ClipboardBackend for NoClipboard {
    fn capture(
        &self,
        _timeout: u64,
        _cancel: CancellationToken,
    ) -> ClipboardFuture<'_, ClipboardPixels> {
        Box::pin(async { panic!("inspection must not access the clipboard") })
    }
    fn copy(
        &self,
        _text: String,
        _backend: CopyBackend,
        _max: usize,
        _timeout: u64,
        _cancel: CancellationToken,
    ) -> ClipboardFuture<'_, CopyOutcome> {
        Box::pin(async { panic!("inspection must not access the clipboard") })
    }
}
#[derive(Clone)]
struct Proof {
    context: ManagementContext,
    request_id: u64,
    snapshot: InspectionSnapshot,
}
fn proof(update: &UiUpdate) -> Option<Proof> {
    if let UiUpdate::ScopedManagement { context, update } = update
        && let UiUpdate::InspectionSnapshot {
            request_id,
            snapshot,
        } = &**update
    {
        return Some(Proof {
            context: context.clone(),
            request_id: *request_id,
            snapshot: snapshot.clone(),
        });
    }
    None
}
fn field<'a>(snapshot: &'a InspectionSnapshot, pointer: &str) -> Option<&'a str> {
    snapshot
        .fields
        .iter()
        .find(|field| field.key == pointer)
        .map(|field| field.value.as_str())
}
fn dispatch(sender: &ActionSender, actions: Vec<UiAction>) {
    let rejected = sender.try_send_batch(actions);
    assert!(
        rejected.is_empty(),
        "fixture action queue rejected {} actions",
        rejected.len()
    );
}
fn key(ui: &mut UiState, sender: &ActionSender, code: KeyCode, modifiers: KeyModifiers) {
    dispatch(sender, ui.event(Event::Key(KeyEvent::new(code, modifiers))));
}
fn draw(ui: &mut UiState) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, ui)).unwrap();
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
async fn pump_until(
    ui: &mut UiState,
    sender: &ActionSender,
    incoming: &Receiver<UiUpdate>,
    proofs: &mut Vec<Proof>,
    mut ready: impl FnMut(&UiState, &[Proof]) -> bool,
) {
    let deadline = crate::test_watchdog::Deadline::new("saved inspection fixture event");
    loop {
        let _ = draw(ui);
        dispatch(sender, ui.take_deferred_actions());
        if ready(ui, proofs) {
            return;
        }
        let update = tokio::task::block_in_place(|| deadline.receive(incoming))
            .unwrap_or_else(|error| panic!("inspection fixture event: {error}\n{}", draw(ui)));
        if let Some(value) = proof(&update) {
            if let Some(previous) = proofs.last() {
                assert_eq!(
                    value.snapshot.target, previous.snapshot.target,
                    "inspection retargeted"
                );
                assert!(
                    value.snapshot.metadata_sequence >= previous.snapshot.metadata_sequence,
                    "metadata watermark regressed"
                );
                if let (Some(before), Some(after)) = (
                    previous.snapshot.usage_sequence,
                    value.snapshot.usage_sequence,
                ) {
                    assert!(after >= before, "usage watermark regressed");
                }
            }
            proofs.push(value);
        }
        ui.apply(update);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_thread_latest_message_limits_keep_source_available_and_read_while_writer_is_held()
{
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    owner.db.call(|db| {
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('bounded-answer','independent-run','assistant_response','now',?1)",
            [serde_json::json!({"content":"retained answer","opaque":"x".repeat(2*1024*1024)}).to_string()])?;
        db.execute("UPDATE turn SET answer_node_id='bounded-answer',state='completed' WHERE id='independent-run'",[])?;
        db.execute("UPDATE run SET state='completed' WHERE id='independent-run'",[])?;
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('foreign-restore','fixture','{}','hash','completed',?1,'2026-10-31T00:00:00Z')",
            [serde_json::json!({"result":{"thread_id":"independent","status":"queued","context_edit":{"action":"restore"}}}).to_string()])?;
        db.execute("INSERT INTO app_meta(key,value)VALUES('compact.result.foreign-restore',?1)",
            [serde_json::json!({"state":"completed","ended_at":"2026-10-31T00:02:00Z","outcome":{"result":{"marker_node_id":"bounded-answer","omitted_nodes":0}}}).to_string()])?;
        Ok(())
    }).await.unwrap();
    let path = control.store.database.path().to_path_buf();
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    let writer = rusqlite::Connection::open(path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("thread get".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
    dispatch(&sender, ui.event(Event::Paste("independent".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, proofs| {
        proofs.last().is_some_and(|proof| {
            field(&proof.snapshot, "/last_answer/body_available") == Some("true")
                && proof.snapshot.fields.iter().any(|field| {
                    field.key.starts_with("/view_limits/") && field.value == "last_answer.excerpt"
                })
        })
    })
    .await;
    let result = &proofs.last().unwrap().snapshot;
    assert_eq!(
        result.target,
        InspectionTarget::Thread("independent".into())
    );
    assert_eq!(
        field(result, "/last_answer/node_id"),
        Some("bounded-answer")
    );
    assert_eq!(field(result, "/last_compaction/kind"), Some("restore"));
    assert_eq!(
        field(result, "/last_compaction/event_id"),
        Some("operation:foreign-restore")
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    let canonical = control
        .store
        .database
        .call(|db| Ok(crate::thread_observation::last_answer(db, "independent")?))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(canonical["excerpt"], "retained answer");
    assert_eq!(canonical["body_available"], true);
    control.store.database.call(|db| {
        db.execute("UPDATE app_meta SET value=?1 WHERE key='compact.result.foreign-restore'",[serde_json::json!({"state":"completed","ended_at":"2026-10-31T00:02:00Z","outcome":{"result":{"omitted_nodes":0,"opaque":"x".repeat(1024*1024)}}}).to_string()])?;
        Ok(())
    }).await.unwrap();
    let changed = external_request(
        &control,
        super::operation(
            &control,
            Command::ThreadSet(ThreadSetArgs {
                thread_id: Some("independent".into()),
                title: Some("updated under oversized compaction".into()),
                ..Default::default()
            }),
        ),
    )
    .await;
    assert!(changed.ok, "{:?}", changed.error);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, proofs| {
        proofs.last().is_some_and(|proof| {
            field(&proof.snapshot, "/title") == Some("updated under oversized compaction")
                && proof.snapshot.fields.iter().any(|field| {
                    field.key.starts_with("/view_limits/") && field.value == "last_compaction"
                })
        })
    })
    .await;
    let latest = &proofs.last().unwrap().snapshot;
    assert_eq!(field(latest, "/last_answer/body_available"), Some("true"));
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    let full = control
        .store
        .database
        .call(|db| {
            Ok(crate::readback::compactions(
                db,
                "independent",
                &CompactListArgs {
                    thread_id: Some("independent".into()),
                    since: None,
                    until: None,
                    limit: 1,
                },
            )?)
        })
        .await
        .unwrap();
    assert_eq!(full["items"][0]["kind"], "restore");
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleared_f8_thread_target_is_not_replaced_by_reply_owner_context() {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control,
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("thread get".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.management
            .as_ref()
            .is_some_and(|view| view.title == "会話 | 詳細")
    })
    .await;
    let screen = draw(&mut ui);
    assert!(
        screen.contains("external operations must specify --thread"),
        "{screen}"
    );
    assert!(
        proofs.is_empty(),
        "reply ownership must not authorize a saved target read"
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_foreign_run_inspection_tracks_real_stop_without_switching_conversation_or_rolling_back_watermarks()
 {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    // Use the real F8/filter/form editing route. No handcrafted Manage request
    // or emitted EngineEvent can accidentally bypass UI target ownership.
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("run get".into())));
    assert!(draw(&mut ui).contains("1/1件"));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
    dispatch(&sender, ui.event(Event::Paste("independent-run".into())));
    let form = draw(&mut ui);
    assert!(form.contains("independent-run") && form.contains("Enter 表示"));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, proofs| {
        proofs.last().is_some_and(|proof| {
            field(&proof.snapshot, "/state") == Some("live")
                && proof.snapshot.usage_sequence.is_some()
        })
    })
    .await;
    let initial = proofs.last().unwrap().clone();
    assert_eq!(
        initial.snapshot.target,
        InspectionTarget::Run("independent-run".into())
    );
    assert_eq!(initial.context.owner_thread.as_deref(), Some("thread"));
    assert_eq!(field(&initial.snapshot, "/thread_id"), Some("independent"));
    let old_metadata = initial.snapshot.metadata_sequence;
    let old_usage = initial.snapshot.usage_sequence.unwrap();
    // The real public Control reducer commits the foreign state and publishes
    // its actual Changed event. No synthetic notification or observer.changed.
    let stopped = external_request(
        &control,
        super::operation(
            &control,
            Command::RunStop(RunTarget {
                run_id: "independent-run".into(),
            }),
        ),
    )
    .await;
    assert!(stopped.ok, "actual stop failed: {:?}", stopped.error);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, proofs| {
        proofs.last().is_some_and(|proof| {
            field(&proof.snapshot, "/state") == Some("stopped")
                && proof.snapshot.metadata_sequence > old_metadata
        })
    })
    .await;
    let metadata = proofs.last().unwrap().clone();
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    let screen = draw(&mut ui);
    assert!(
        screen.contains("independent-run") && screen.contains("停止済み"),
        "{screen}"
    );
    // Totals refresh has an independent3s cadence. Its later reply must retain
    // the already current stopped metadata, rather than repainting a stale Run.
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, proofs| {
        proofs.last().is_some_and(|proof| {
            proof
                .snapshot
                .usage_sequence
                .is_some_and(|sequence| sequence > old_usage)
        })
    })
    .await;
    let latest = proofs.last().unwrap().clone();
    assert_eq!(field(&latest.snapshot, "/state"), Some("stopped"));
    assert!(latest.snapshot.metadata_sequence >= metadata.snapshot.metadata_sequence);
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    // A delayed previously valid scoped report is rejected by the actual UI,
    // including its old metadata/usage times and field values.
    let before = draw(&mut ui);
    ui.apply(UiUpdate::ScopedManagement {
        context: initial.context.clone(),
        update: Box::new(UiUpdate::InspectionSnapshot {
            request_id: initial.request_id,
            snapshot: initial.snapshot.clone(),
        }),
    });
    assert_eq!(
        draw(&mut ui),
        before,
        "stale scoped snapshot rolled displayed metadata/usage back"
    );
    // Hold metadata at the current watermark and independently regress only
    // the usage watermark/time. This exercises the usage fence rather than
    // letting the older metadata guard mask a missing usage guard.
    let mut old_usage_only = latest.snapshot.clone();
    old_usage_only.usage_sequence = initial.snapshot.usage_sequence;
    old_usage_only.usage_observed_at = initial.snapshot.usage_observed_at.clone();
    ui.apply(UiUpdate::ScopedManagement {
        context: latest.context.clone(),
        update: Box::new(UiUpdate::InspectionSnapshot {
            request_id: latest.request_id,
            snapshot: old_usage_only,
        }),
    });
    assert_eq!(
        draw(&mut ui),
        before,
        "older usage watermark rolled current inspection time/state back"
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    key(&mut ui, &sender, KeyCode::Esc, KeyModifiers::NONE);
    let deferred = ui.take_deferred_actions();
    assert!(deferred.iter().any(|action|matches!(action,UiAction::ObserveInspection{target:InspectionTarget::Run(id),visible:false,..}if id=="independent-run")),"closing actual management must retire its observation");
    dispatch(&sender, deferred);
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

fn open_form(ui: &mut UiState, sender: &ActionSender, query: &str) {
    key(ui, sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(sender, ui.event(Event::Paste(query.into())));
    assert!(
        draw(ui).contains("1/1件"),
        "form filter was not unique: {query}"
    );
    key(ui, sender, KeyCode::Enter, KeyModifiers::NONE);
    key(ui, sender, KeyCode::Enter, KeyModifiers::NONE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_storage_separates_saved_metadata_from_physical_samples_and_refreshes_real_commits() {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    open_form(&mut ui, &sender, "storage status");
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| field(&p.snapshot, "/physical_observation/available") == Some("true"))
    })
    .await;
    let initial = proofs.last().unwrap().snapshot.clone();
    assert_eq!(initial.target, InspectionTarget::Storage);
    assert!(field(&initial, "/observed_at").is_some());
    assert!(
        field(&initial, "/logical/bytes").is_some(),
        "saved logical facts must accompany the independently timed physical sample"
    );
    assert!(field(&initial, "/physical_observation/sample/observed_at").is_some());
    assert!(field(&initial, "/physical_observation/sample/started_at").is_some());
    let physical = field(&initial, "/physical_observation/physical_sequence")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let changed = external_request(
        &control,
        super::operation(
            &control,
            Command::ThreadSet(ThreadSetArgs {
                thread_id: Some("independent".into()),
                title: Some("synthetic changed title".into()),
                ..Default::default()
            }),
        ),
    )
    .await;
    assert!(
        changed.ok,
        "foreign metadata commit failed: {:?}",
        changed.error
    );
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| p.snapshot.metadata_sequence > initial.metadata_sequence)
    })
    .await;
    let after_metadata = proofs.last().unwrap().snapshot.metadata_sequence;
    let collected = external_request(
        &control,
        super::operation(
            &control,
            Command::StorageCollect(StorageArgs {
                category: Some(StorageCategory::Raw),
            }),
        ),
    )
    .await;
    assert!(
        collected.ok,
        "actual collection failed: {:?}",
        collected.error
    );
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > after_metadata
                && field(&p.snapshot, "/physical_observation/physical_sequence")
                    .and_then(|v| v.parse::<u64>().ok())
                    .is_some_and(|n| n > physical)
        })
    })
    .await;
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    let attempts: i64 = control
        .store
        .database
        .call(|db| Ok(db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(
        attempts, 0,
        "saved/physical observations must not start model attempts"
    );
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_delete_preview_refreshes_live_blocker_after_actual_stop_without_deleting() {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    open_form(&mut ui, &sender, "thread delete-preview");
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| field(&p.snapshot, "/deletable_now") == Some("false"))
    })
    .await;
    let initial = proofs.last().unwrap().snapshot.clone();
    assert!(
        matches!(initial.target,InspectionTarget::Deletion(ref args) if args.thread_id=="thread" && !args.descendants)
    );
    assert_eq!(field(&initial, "/blockers/live_run_count"), Some("1"));
    let stopped = external_request(
        &control,
        super::operation(
            &control,
            Command::RunStop(RunTarget {
                run_id: "target-run".into(),
            }),
        ),
    )
    .await;
    assert!(stopped.ok, "actual stop failed: {:?}", stopped.error);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > initial.metadata_sequence
                && field(&p.snapshot, "/deletable_now") == Some("true")
        })
    })
    .await;
    let final_snapshot = &proofs.last().unwrap().snapshot;
    assert_eq!(field(final_snapshot, "/blockers/live_run_count"), Some("0"));
    assert_eq!(
        field(final_snapshot, "/observation/selected_scope_complete"),
        Some("true")
    );
    assert_eq!(
        field(final_snapshot, "/observation/references_complete"),
        Some("true")
    );
    let facts: (Option<String>, String, i64) = control
        .store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT deleted_at FROM thread WHERE id='thread'", [], |r| {
                    r.get(0)
                })?,
                db.query_row("SELECT state FROM run WHERE id='target-run'", [], |r| {
                    r.get(0)
                })?,
                db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(facts, (None, "stopped".into(), 0));
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

/// Real transport, held after the complete request has arrived. The listener
/// continues accepting while held, so an unauthorized second request is counted
/// rather than hidden behind the first socket's barrier.
struct HeldEmbedding {
    url: String,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    unexpected: Arc<std::sync::atomic::AtomicUsize>,
    release: Arc<tokio::sync::Semaphore>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for HeldEmbedding {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl HeldEmbedding {
    async fn new() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let unexpected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (calls, bad, gate) = (requests.clone(), unexpected.clone(), release.clone());
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{
                        let (mut socket,_)=accepted.unwrap();
                        let (calls,bad,gate)=(calls.clone(),bad.clone(),gate.clone());
                        handlers.spawn(async move {
                            let mut bytes=Vec::new();
                            let request=loop {
                                let mut part=[0;4096];
                                let n=socket.read(&mut part).await.unwrap();
                                assert!(n>0,"fixture transport ended before request");
                                bytes.extend_from_slice(&part[..n]);
                                assert!(bytes.len()<=64*1024);
                                if let Some(at)=bytes.windows(4).position(|b|b==b"\r\n\r\n") {
                                    let headers=String::from_utf8_lossy(&bytes[..at]);
                                    let length=headers.lines().find_map(|line|line.to_ascii_lowercase()
                                        .strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap();
                                    if bytes.len()>=at+4+length {
                                        break (headers.lines().next().unwrap().to_owned(),serde_json::from_slice::<serde_json::Value>(&bytes[at+4..at+4+length]).unwrap());
                                    }
                                }
                            };
                            let number=calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
                            let valid=request.0=="POST /v1/embeddings HTTP/1.1"
                                && request.1["input"].as_array().is_some_and(|v|v.len()==1) && number==1;
                            if !valid {bad.fetch_add(1,std::sync::atomic::Ordering::SeqCst);}
                            if valid {gate.acquire().await.unwrap().forget();}
                            let (status,body)=if valid {(200,r#"{"data":[{"index":0,"embedding":[1.0,2.0]}]}"#)}
                                else {(503,r#"{"error":{"message":"unexpected synthetic request"}}"#)};
                            socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                        });
                    },
                    result=handlers.join_next(),if !handlers.is_empty()=>{result.unwrap().unwrap();}
                }
            }
        });
        Self {
            url,
            requests,
            unexpected,
            release,
            task,
        }
    }
    fn count(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
}

async fn index_fixture(
    url: &str,
) -> (
    tempfile::TempDir,
    crate::app::DataOwner,
    Arc<Control>,
    crate::ipc::LocalServer,
) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[ui]\nlanguage=\"ja\"\n[connections.fixture]\nbase_url=\"{url}\"\n[embedding]\nconnection=\"fixture\"\nmodel=\"synthetic\"\n[indexing]\nauto_history=true\nretries=0\nretry_delays_ms=[]\n")).unwrap();
    let owner = crate::app::DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let cwd = root.path().to_string_lossy().to_string();
    owner.db.call(move|db|{
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','synthetic',?1,'','fixture','fixture-model','2026-01-01T00:00:00Z')",[cwd])?;
        for ordinal in 0..40 {
            let id=format!("source-{ordinal:04}");
            let payload=serde_json::json!({"role":"user","content":[{"type":"input_text","text":format!("synthetic source {ordinal}")}]}).to_string();
            db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'user','2026-01-01T00:00:00Z',?2,?3,?4)",rusqlite::params![id,payload,payload.len()as i64,crate::digest::sha256_hex(payload.as_bytes())])?;
            db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',?1,?2)",rusqlite::params![ordinal,id])?;
        }
        Ok(())
    }).await.unwrap();
    let (rpc, receive) = tokio::sync::mpsc::channel(16);
    let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
    tokio::spawn(control.clone().serve(receive));
    // Production starts the owner before entering the frontend loop. Unlike
    // the metadata-only fixture, this test must run real Index workers.
    control.start().await.unwrap();
    (root, owner, control, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_index_pages_saved_sources_preserves_window_on_enqueue_and_only_posts_authorized_input()
{
    let mock = HeldEmbedding::new().await;
    let (root, owner, control, server) = index_fixture(&mock.url).await;
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    open_form(&mut ui, &sender, "index status");
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            field(
                &p.snapshot,
                "/management/coverage_window/inspected_memberships",
            ) == Some("32")
        })
    })
    .await;
    let first = proofs.last().unwrap().snapshot.clone();
    assert!(matches!(first.target, InspectionTarget::Index(_)));
    assert_ne!(
        field(&first, "/management/coverage_window/next_cursor"),
        Some("null")
    );
    assert_eq!(
        field(&first, "/management/coverage_window/scope_exhausted"),
        Some("false")
    );
    assert_eq!(
        mock.count(),
        0,
        "seeding and observing sources authorizes no embedding request"
    );
    let next = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('n'),
        KeyModifiers::NONE,
    )));
    assert!(
        next.iter()
            .any(|a| matches!(a,UiAction::NextInspectionWindow{target,..}if target==&first.target)),
        "actual n must request the same target's next window"
    );
    dispatch(&sender, next);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            field(
                &p.snapshot,
                "/management/coverage_window/inspected_memberships",
            ) == Some("8")
                && field(&p.snapshot, "/management/coverage_window/scope_exhausted") == Some("true")
        })
    })
    .await;
    let second = proofs.last().unwrap().snapshot.clone();
    assert_eq!(
        mock.count(),
        0,
        "paging is an observation, never an authorization"
    );
    let queued = control
        .index
        .enqueue_new(&["source-0039".into()])
        .await
        .unwrap();
    assert_eq!(queued.segments, 1);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        mock.count() == 1
            && p.last().is_some_and(|p| {
                p.snapshot.metadata_sequence > second.metadata_sequence
                    && field(&p.snapshot, "/management/status/counts/running")
                        == Some("1（確認済み）")
            })
    })
    .await;
    let running = proofs.last().unwrap().snapshot.clone();
    assert_eq!(
        field(
            &running,
            "/management/coverage_window/inspected_memberships"
        ),
        Some("8"),
        "ordinary saved progress must preserve the user's current coverage cursor"
    );
    assert_eq!(
        field(&running, "/management/coverage_window/scope_exhausted"),
        Some("true")
    );
    assert_eq!(
        mock.count(),
        1,
        "only explicit enqueue may authorize this held POST"
    );
    mock.release.add_permits(1);
    // Space bootstrap invalidates this coverage cursor, but that domain error
    // must not freeze unrelated saved lifecycle/status facts. No refresh action
    // is sent before this automatic done observation arrives.
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            field(&p.snapshot, "/management/status/counts/done") == Some("1（確認済み）")
                && field(&p.snapshot, "/management/status/counts/running") == Some("0（確認済み）")
                && field(&p.snapshot, "/management/coverage_window/current") == Some("false")
                && field(&p.snapshot, "/management/coverage_window/stale") == Some("true")
        })
    })
    .await;
    let stale = proofs.last().unwrap().snapshot.clone();
    assert!(stale.metadata_sequence > running.metadata_sequence);
    assert!(
        field(&stale, "/management/coverage_window/next_cursor").is_none(),
        "an old cursor must never remain a current navigation action"
    );
    assert_eq!(
        mock.count(),
        1,
        "automatic status refresh must not authorize another embedding"
    );
    // Open the actual retained last-observation field through its UI row. The
    // list intentionally shows a stale label rather than flattening old values;
    // the real deferred detail reply is the evidence for the retained8 window.
    let row = stale
        .fields
        .iter()
        .position(|f| f.key == "/management/coverage_window/last_observation")
        .unwrap();
    for _ in 0..stale.fields.len() {
        key(&mut ui, &sender, KeyCode::Up, KeyModifiers::NONE);
    }
    for _ in 0..row {
        key(&mut ui, &sender, KeyCode::Down, KeyModifiers::NONE);
    }
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    let detail_deadline = crate::test_watchdog::Deadline::new("retained coverage detail event");
    let text = loop {
        let _ = draw(&mut ui);
        dispatch(&sender, ui.take_deferred_actions());
        let update = tokio::task::block_in_place(|| detail_deadline.receive(&incoming)).unwrap();
        let detail = match &update {
            UiUpdate::ScopedManagement { update, .. } => match &**update {
                UiUpdate::ManagementDetail {
                    pointer,
                    text,
                    next_cursor,
                    ..
                } if pointer == "/management/coverage_window/last_observation" => {
                    assert!(
                        next_cursor.is_none(),
                        "small retained coverage fits one detail page"
                    );
                    Some(text.clone())
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(value) = proof(&update) {
            proofs.push(value);
        }
        ui.apply(update);
        if let Some(text) = detail {
            let _ = draw(&mut ui);
            break text;
        }
    };
    assert!(
        text.contains("/inspected_memberships: 8"),
        "retained detail lost the user's8-member window: {text}"
    );
    assert!(text.contains("/scope_exhausted: true"), "{text}");
    // Only an explicit refresh starts the first window in the newly selected
    // space. It must preserve the already current done acknowledgement.
    key(&mut ui, &sender, KeyCode::Char('r'), KeyModifiers::CONTROL);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > stale.metadata_sequence
                && field(&p.snapshot, "/management/status/counts/done") == Some("1（確認済み）")
                && field(
                    &p.snapshot,
                    "/management/coverage_window/inspected_memberships",
                ) == Some("32")
        })
    })
    .await;
    assert_eq!(
        mock.count(),
        1,
        "explicit saved refresh must not resubmit embeddings"
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    assert_eq!(
        mock.count(),
        1,
        "authorized worker1; automatic observation/paging/refresh0"
    );
    assert_eq!(mock.unexpected.load(std::sync::atomic::Ordering::SeqCst), 0);
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
    assert_eq!(mock.count(), 1, "shutdown must not add requests");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn visible_storage_observes_real_pending_runtime_receipt_before_held_probe_completes() {
    let mock = HeldEmbedding::new().await;
    let (root, owner, control, server) = index_fixture(&mock.url).await;
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    open_form(&mut ui, &sender, "storage status");
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| field(&p.snapshot, "/logical/bytes").is_some())
    })
    .await;
    let initial = proofs.last().unwrap().snapshot.clone();
    let initial_bytes = field(&initial, "/logical/bytes")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(
        mock.count(),
        0,
        "Storage observations authorize no model request"
    );
    let request = super::operation(&control, Command::IndexProbe(IndexProbeArgs::default()));
    let operation = request.operation_id.clone();
    let hint = control.saved_observation_hint().notified();
    tokio::pin!(hint);
    hint.as_mut().enable();
    let requester = control.clone();
    let execution = tokio::spawn(async move { external_request(&requester, request).await });
    crate::test_watchdog::watchdog("saved inspection fixture", hint.as_mut())
        .await
        .unwrap();
    // Notify is advisory: readonly request teardown also wakes it. Require both
    // an actual held HTTP request and saved pending-row bytes in the UI instead
    // of treating an arbitrary wake as evidence of a committed reservation.
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        mock.count() == 1
            && p.last().is_some_and(|p| {
                p.snapshot.metadata_sequence > initial.metadata_sequence
                    && field(&p.snapshot, "/logical/bytes")
                        .and_then(|v| v.parse::<u64>().ok())
                        .is_some_and(|bytes| bytes > initial_bytes)
            })
    })
    .await;
    let pending = proofs.last().unwrap().snapshot.clone();
    let lookup = operation.clone();
    let state: (String, Option<String>) = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT state,result_ref_json FROM operation_receipt WHERE operation_id=?1",
                [lookup],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        state,
        ("pending".into(), None),
        "the new row must be committed while the action is still held"
    );
    assert!(
        !execution.is_finished(),
        "the visible pending update must precede runtime completion"
    );
    assert_eq!(
        mock.count(),
        1,
        "one explicit probe was authorized; observations authorize zero"
    );
    mock.release.add_permits(1);
    let result = crate::test_watchdog::watchdog("saved inspection fixture", execution)
        .await
        .unwrap()
        .unwrap();
    assert!(result.ok, "synthetic probe failed: {:?}", result.error);
    let pending_bytes = field(&pending, "/logical/bytes")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > pending.metadata_sequence
                && field(&p.snapshot, "/logical/bytes")
                    .and_then(|v| v.parse::<u64>().ok())
                    .is_some_and(|bytes| bytes > pending_bytes)
        })
    })
    .await;
    let state: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT state FROM operation_receipt WHERE operation_id=?1",
                [operation],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(state, "completed");
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    assert_eq!(mock.count(), 1);
    assert_eq!(mock.unexpected.load(std::sync::atomic::Ordering::SeqCst), 0);
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_standalone_usage_reads_foreign_run_root_and_follows_real_saved_stop() {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("usage get".into())));
    assert!(draw(&mut ui).contains("1/1件"));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    // Clear current Thread and pass attempt/since/until fields to Run.
    key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
    for _ in 0..4 {
        key(&mut ui, &sender, KeyCode::Tab, KeyModifiers::NONE);
    }
    dispatch(&sender, ui.event(Event::Paste("independent-run".into())));
    assert!(draw(&mut ui).contains("independent-run"));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| field(&p.snapshot, "/scope_id") == Some("independent-run"))
    })
    .await;
    let first = proofs.last().unwrap().clone();
    assert_eq!(
        first.snapshot.target,
        InspectionTarget::Usage(crate::protocol::UsageGetArgs {
            run_id: Some("independent-run".into()),
            ..Default::default()
        })
    );
    assert_eq!(field(&first.snapshot, "/thread_id"), Some("independent"));
    assert_eq!(field(&first.snapshot, "/scope"), Some("run"));
    assert_eq!(
        first.snapshot.usage_sequence,
        Some(first.snapshot.metadata_sequence)
    );
    assert!(
        !first
            .snapshot
            .fields
            .iter()
            .any(|f| f.key.starts_with("/usage_totals") || f.key.starts_with("/context_usage"))
    );
    assert_eq!(first.context.owner_thread.as_deref(), Some("thread"));
    let stopped = external_request(
        &control,
        super::operation(
            &control,
            Command::RunStop(RunTarget {
                run_id: "independent-run".into(),
            }),
        ),
    )
    .await;
    assert!(stopped.ok, "{:?}", stopped.error);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| p.snapshot.usage_sequence > first.snapshot.usage_sequence)
    })
    .await;
    let next = proofs.last().unwrap();
    assert_eq!(next.snapshot.target, first.snapshot.target);
    assert_eq!(
        next.snapshot.metadata_sequence,
        next.snapshot.usage_sequence.unwrap()
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    assert!(
        control
            .store
            .database
            .call(
                |db| Ok(db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?)
            )
            .await
            .unwrap()
            == 0
    );
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[test]
fn explicit_saved_usage_reference_enters_the_same_inspection_without_viewer_retargeting() {
    let args = crate::protocol::UsageGetArgs {
        turn_id: Some("foreign-turn".into()),
        ..Default::default()
    };
    let value = serde_json::json!({"reference":{"targets":[Command::UsageGet(args.clone())]}});
    let command =
        management::follow(&value, "/reference", crate::ui::ManagementFollow::Inspect).unwrap();
    assert_eq!(
        inspection_target(&command),
        Some(InspectionTarget::Usage(args))
    );
}

#[tokio::test]
async fn standalone_usage_reads_canonical_run_turn_and_children_scope_without_watcher_replay() {
    let (root, owner, control, _server) = super::control_tests::fixture_with_language("ja").await;
    let diagnostics = control.diagnostics().clone();
    owner.db.call(|db| {
        // A live grandchild behind a deleted bridge is intentionally outside
        // canonical Usage, unlike Watch's descendant traversal.
        for (id,parent,deleted) in [("child","thread",None),("bridge","thread",Some("deleted")),("grandchild","bridge",None)] {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,parent_thread_id,deleted_at,updated_at) VALUES(?1,?1,'','','fixture','fixture-model',?2,?3,'now')",rusqlite::params![id,parent,deleted])?;
        }
        Ok(())
    }).await.unwrap();
    for (args, expected_thread, selected) in [
        (
            crate::protocol::UsageGetArgs {
                run_id: Some("independent-run".into()),
                ..Default::default()
            },
            "independent",
            1,
        ),
        (
            crate::protocol::UsageGetArgs {
                turn_id: Some("independent-run".into()),
                ..Default::default()
            },
            "independent",
            1,
        ),
        (
            crate::protocol::UsageGetArgs {
                thread_id: Some("thread".into()),
                include_children: true,
                ..Default::default()
            },
            "thread",
            2,
        ),
    ] {
        let context = ManagementContext {
            epoch: 1,
            owner_draft: "input".into(),
            owner_thread: Some("viewer".into()),
        };
        let target = InspectionTarget::Usage(args);
        let mut observer = inspection_state::Observation::default();
        observer.install(1, context.clone(), target.clone(), None);
        observer.observe(&context, &target, true);
        let ticket = observer
            .take_usage(tokio::time::Instant::now(), diagnostics.clone())
            .unwrap();
        let raw = snapshot::read(
            owner.db.path().to_path_buf(),
            CancellationToken::new(),
            move |db| inspection_state::read_usage_tx(db, &ticket, None),
        )
        .await
        .unwrap();
        assert_eq!(raw.value["thread_id"], expected_thread);
        assert_eq!(raw.value["selected_threads"], selected);
        assert_eq!(raw.value["coverage"]["scope_complete"], true);
        assert!(raw.context_usage.is_none());
    }
    drop(control);
    drop(owner);
    drop(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_foreign_attachment_list_tracks_saved_remove_with_exact_thread_and_no_file_reads() {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    // Fixture-only file creation; observation never stats/decodes this image.
    let managed_path = owner.loaded.data_dir.join("managed/clipboard/example.png");
    std::fs::create_dir_all(managed_path.parent().unwrap()).unwrap();
    std::fs::write(&managed_path, b"synthetic managed GC fixture").unwrap();
    let managed_path_sql = managed_path.to_string_lossy().into_owned();
    let current_instance = control.store.instance_id.clone();
    owner.db.call(move |db| {
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('attachment-source','user','now','{}');
        INSERT INTO attachment_ref VALUES('saved-attachment','attachment-source','/does-not-exist-g5-observation','file','external','now');
        INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('independent','saved-attachment','active','now',0);")?;
        db.execute("INSERT INTO attachment_ref VALUES('managed-attachment','attachment-source',?1,'image','managed_clipboard','now')", [&managed_path_sql])?;
        db.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,reason,selected_at,selected_order)VALUES('independent','managed-attachment','excluded','fixture exclusion','now',1)", [])?;
        db.execute("INSERT INTO managed_file(id,absolute_path,attachment_id,state,raw_size,created_at)VALUES('example',?1,'managed-attachment','ready',27,'2000-01-01T00:00:00Z')", [&managed_path_sql])?;
        db.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES('attachment-managed-gc-pin',?1,'managed_file','example',0,'now')", [&current_instance])?;
        Ok(())
    }).await.unwrap();
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("attachment list".into())));
    assert!(draw(&mut ui).contains("1/1件"));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
    dispatch(&sender, ui.event(Event::Paste("independent".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| field(&p.snapshot, "/attachments/0/selected") == Some("true"))
    })
    .await;
    let first = proofs.last().unwrap().clone();
    assert_eq!(
        first.snapshot.target,
        InspectionTarget::Attachments {
            thread_id: "independent".into(),
            attachment_id: None
        }
    );
    assert_eq!(first.context.owner_thread.as_deref(), Some("thread"));
    assert!(first.snapshot.usage_sequence.is_none());
    let removed = external_request(
        &control,
        super::operation(
            &control,
            Command::AttachmentRemove(crate::protocol::AttachmentRemoveArgs {
                thread_id: Some("independent".into()),
                attachment_id: "saved-attachment".into(),
            }),
        ),
    )
    .await;
    assert!(removed.ok, "{:?}", removed.error);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > first.snapshot.metadata_sequence
                && field(&p.snapshot, "/attachments/0/selected") == Some("false")
        })
    })
    .await;
    let latest = proofs.last().unwrap().clone();
    assert_eq!(
        field(&latest.snapshot, "/attachments/0/unavailable_reason"),
        Some("explicitly removed")
    );
    assert_eq!(
        field(&latest.snapshot, "/attachments/0/path"),
        Some("/does-not-exist-g5-observation")
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    assert_eq!(
        field(&latest.snapshot, "/attachments/1/attachment_id"),
        Some("managed-attachment")
    );
    assert_eq!(
        field(
            &latest.snapshot,
            "/attachments/1/saved_observation/managed_state"
        ),
        Some("ready")
    );
    // Ordinary mutation budget maintenance may collect eligible files too.
    // Release the fixture pin only now, so this explicit GC owns the transition.
    control
        .store
        .database
        .call(|db| {
            db.execute(
                "DELETE FROM runtime_pin WHERE id='attachment-managed-gc-pin'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let managed = control
        .gc
        .collect(
            crate::protocol::StorageCategory::ManagedImages,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(managed.managed_deleted_count, 1);
    assert!(
        !managed_path.exists(),
        "actual managed collection removed only the fixture file"
    );
    // GC publishes availability after its own commits, with no destination
    // Thread Changed producer. The visible saved inspection must consume it.
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > latest.snapshot.metadata_sequence
                && field(
                    &p.snapshot,
                    "/attachments/1/saved_observation/managed_state",
                ) == Some("deleted")
        })
    })
    .await;
    let after_managed = proofs.last().unwrap().clone();
    assert_eq!(
        field(&after_managed.snapshot, "/attachments/1/attachment_id"),
        Some("managed-attachment")
    );
    assert_eq!(
        field(&after_managed.snapshot, "/attachments/1/unavailable_reason"),
        // Canonical saved managed-file unavailability takes precedence over
        // the selection's exclusion reason after GC (the G8 reader contract).
        Some("clipboard retention (deleted)")
    );
    assert_eq!(
        field(
            &after_managed.snapshot,
            "/attachments/0/saved_observation/source_payload_available"
        ),
        Some("true")
    );
    control.store.database.call(|db| {
        db.execute("INSERT INTO gc_job(id,kind,root_revision,cursor_json,state,error_json)VALUES('attachment-source-gc','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL)", [])?;
        Ok(())
    }).await.unwrap();
    // The source has no Turn/path/pending-delivery/pin root. Excluded saved
    // attachment references survive history payload collection.
    let history = control
        .gc
        .collect(
            crate::protocol::StorageCategory::History,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(history.nodes_tombstoned, 1);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > after_managed.snapshot.metadata_sequence
                && field(
                    &p.snapshot,
                    "/attachments/0/saved_observation/source_payload_available",
                ) == Some("false")
        })
    })
    .await;
    let after_history = proofs.last().unwrap();
    assert_eq!(
        field(&after_history.snapshot, "/attachments/0/attachment_id"),
        Some("saved-attachment")
    );
    assert_eq!(
        field(&after_history.snapshot, "/attachments/0/source_node_id"),
        Some("attachment-source")
    );
    assert_eq!(
        field(
            &after_history.snapshot,
            "/attachments/0/saved_observation/source_deleted_reason"
        ),
        Some("thread_deleted_unreachable")
    );
    assert_eq!(
        field(&after_history.snapshot, "/attachments/1/attachment_id"),
        Some("managed-attachment")
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    let attempts: i64 = control
        .store
        .database
        .call(|db| Ok(db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(
        attempts, 0,
        "saved observation and local GC never issue provider requests"
    );
    key(&mut ui, &sender, KeyCode::Esc, KeyModifiers::NONE);
    let deferred = ui.take_deferred_actions();
    assert!(deferred.iter().any(|a|matches!(a,UiAction::ObserveInspection {target:InspectionTarget::Attachments {thread_id:id,..},visible:false,..}if id=="independent")));
    dispatch(&sender, deferred);
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

/// Event-driven bounded fixture pump: no arrival-delay assertions or polling sleep.
fn pump_operation_until(
    ui: &mut UiState,
    sender: &ActionSender,
    incoming: &Receiver<UiUpdate>,
    proofs: &mut Vec<Proof>,
    mut ready: impl FnMut(&UiState, &[Proof]) -> bool,
) {
    let deadline = crate::test_watchdog::Deadline::new("operation fixture event");
    loop {
        let _ = draw(ui);
        dispatch(sender, ui.take_deferred_actions());
        if ready(ui, proofs) {
            return;
        }
        let update = deadline.receive(incoming).unwrap_or_else(|error| {
            panic!("operation fixture event watchdog: {error}\n{}", draw(ui))
        });
        if let Some(value) = proof(&update) {
            proofs.push(value);
        }
        ui.apply(update);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_operation_and_saved_reference_react_to_actual_message_cancel_without_viewer_retargeting()
 {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    owner.db.call(|db| {
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('op-message','user','now','{\"role\":\"user\",\"content\":[]}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,hold_reason)VALUES('op-receipt','old:f8-message','{\"kind\":\"human\"}','independent','op-message','held','now','test-held'); INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('old:f8-message','old-owner','null','hash','completed','{\"result\":{\"message_id\":\"op-message\"}}','now');")?;Ok(())
    }).await.unwrap();
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_operation_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    });
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("operation get".into())));
    assert!(draw(&mut ui).contains("1/1件"));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
    dispatch(&sender, ui.event(Event::Paste("old:f8-message".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_operation_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| field(&p.snapshot, "/downstream/state") == Some("held"))
    });
    let initial = proofs.last().unwrap().clone();
    assert_eq!(
        initial.snapshot.target,
        InspectionTarget::Operation("old:f8-message".into())
    );
    assert_eq!(field(&initial.snapshot, "/current_instance"), Some("false"));
    assert_eq!(initial.context.owner_thread.as_deref(), Some("thread"));
    assert!(initial.snapshot.usage_sequence.is_none());
    // Actual retained-reference route: resolve the saved current management
    // Value and follow its exact operation ID through the frontend, not replay.
    let operation_row = initial
        .snapshot
        .fields
        .iter()
        .position(|field| field.key == "/operation_id")
        .expect("exact saved operation reference");
    // Management list navigation guarantees Up saturation; Home is a reader
    // scroll key and does not itself select the first structured field.
    for _ in 0..initial.snapshot.fields.len() {
        key(&mut ui, &sender, KeyCode::Up, KeyModifiers::NONE);
    }
    for _ in 0..operation_row {
        key(&mut ui, &sender, KeyCode::Down, KeyModifiers::NONE);
    }
    key(&mut ui, &sender, KeyCode::Char('o'), KeyModifiers::NONE);
    pump_operation_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.request_id > initial.request_id
                && field(&p.snapshot, "/downstream/state") == Some("held")
        })
    });
    let followed = proofs.last().unwrap();
    assert_eq!(followed.snapshot.target, initial.snapshot.target);
    assert_ne!(
        followed.context.epoch, initial.context.epoch,
        "real saved-reference input owns a new UI scope"
    );
    assert_eq!(followed.context.owner_thread.as_deref(), Some("thread"));
    let before = followed.snapshot.metadata_sequence;
    let cancelled = external_request(
        &control,
        super::operation(
            &control,
            Command::MessageCancel(crate::protocol::MessageCancelArgs {
                message_ids: vec!["op-message".into()],
            }),
        ),
    )
    .await;
    assert!(cancelled.ok, "{:?}", cancelled.error);
    assert_eq!(
        cancelled.result.unwrap()["items"][0]["outcome"],
        "cancelled"
    );
    pump_operation_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            p.snapshot.metadata_sequence > before
                && field(&p.snapshot, "/downstream/state") == Some("cancelled")
        })
    });
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    let attempted = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row("SELECT count(*) FROM model_attempt", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(attempted, 0);
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_history_body_retains_original_detail_while_current_source_facts_change() {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    owner.db.call(|db|{db.execute_batch("INSERT INTO node(id,kind,created_at,payload_revision,payload_json)VALUES('retained-history','user','now',7,'{\"role\":\"user\",\"content\":\"retained history original\"}');")?;Ok(())}).await.unwrap();
    let path = control.store.database.path().to_path_buf();
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    let writer = rusqlite::Connection::open(path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("history get".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("retained-history".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            field(&p.snapshot, "/current_source/nodes/0/body_available") == Some("true")
        })
    })
    .await;
    let initial = proofs.last().unwrap().clone();
    writer.execute_batch("ROLLBACK").unwrap();
    let InspectionTarget::HistorySources(scope) = &initial.snapshot.target else {
        panic!("wrong target")
    };
    assert_eq!(scope.nodes, vec![("retained-history".into(), Some(7))]);
    assert!(scope.thread_id.is_none());
    control.store.database.call(|db|{db.execute_batch("UPDATE node SET payload_json=NULL,payload_revision=8,deleted_reason='synthetic collection' WHERE id='retained-history';")?;Ok(())}).await.unwrap();
    let changed = external_request(
        &control,
        super::operation(
            &control,
            Command::ThreadSet(ThreadSetArgs {
                thread_id: Some("independent".into()),
                title: Some("source observation fixture change".into()),
                ..Default::default()
            }),
        ),
    )
    .await;
    assert!(changed.ok, "{:?}", changed.error);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            field(&p.snapshot, "/current_source/nodes/0/body_available") == Some("false")
                && p.snapshot.metadata_sequence > initial.snapshot.metadata_sequence
        })
    })
    .await;
    let current = proofs.last().unwrap();
    assert_eq!(
        field(
            &current.snapshot,
            "/current_source/nodes/0/revision_matches"
        ),
        Some("false")
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    sender
        .try_send(UiAction::ManagementIntent {
            context: current.context.clone(),
            action: Box::new(UiAction::LoadManagementDetail {
                request_id: current.request_id,
                detail_request_id: 99,
                pointer: "/text".into(),
                cursor: None,
            }),
        })
        .unwrap();
    let deadline = crate::test_watchdog::Deadline::new("retained history original detail");
    loop {
        let update = tokio::task::block_in_place(|| deadline.receive(&incoming)).unwrap();
        let text = match &update {
            UiUpdate::ScopedManagement { update, .. } => match &**update {
                UiUpdate::ManagementDetail { pointer, text, .. } if pointer == "/text" => {
                    Some(text.clone())
                }
                UiUpdate::ManagementDetailFailure { error, .. } => panic!("{error}"),
                _ => None,
            },
            _ => None,
        };
        ui.apply(update);
        if let Some(text) = text {
            assert!(text.contains("retained history original"), "{text}");
            break;
        }
    }
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_history_list_keeps_captured_foreign_page_when_selected_path_changes() {
    let (root, owner, control, server) = super::control_tests::fixture_with_language("ja").await;
    owner.db.call(|db|{db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('list-first','user','now','{\"role\":\"user\",\"content\":\"retained first\"}'),('list-second','user','now','{\"role\":\"user\",\"content\":\"retained second\"}');INSERT INTO thread_path VALUES('independent',0,'list-first'),('independent',1,'list-second');")?;Ok(())}).await.unwrap();
    let mut ui = UiState::new(&owner.loaded.config, root.path().into());
    let (sender, receiver) = actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    dispatch(&sender, ui.select_thread("thread".into()));
    let mut proofs = Vec::new();
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
        ui.view().thread_id.as_deref() == Some("thread") && !ui.view().navigation.is_loading()
    })
    .await;
    key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
    dispatch(&sender, ui.event(Event::Paste("history list".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
    dispatch(&sender, ui.event(Event::Paste("independent".into())));
    key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last()
            .is_some_and(|p| field(&p.snapshot, "/current_source/nodes/1/selected") == Some("true"))
    })
    .await;
    let initial = proofs.last().unwrap().clone();
    let InspectionTarget::HistorySources(scope) = &initial.snapshot.target else {
        panic!("wrong target")
    };
    assert_eq!(scope.thread_id.as_deref(), Some("independent"));
    assert_eq!(scope.nodes.len(), 2);
    control
        .store
        .database
        .call(|db| {
            let tx = db.transaction()?;
            crate::history::select_path(&tx, "independent", 0, &["list-first".into()], "now")?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let changed = external_request(
        &control,
        super::operation(
            &control,
            Command::ThreadSet(ThreadSetArgs {
                thread_id: Some("independent".into()),
                title: Some("changed path fixture".into()),
                ..Default::default()
            }),
        ),
    )
    .await;
    assert!(changed.ok, "{:?}", changed.error);
    pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
        p.last().is_some_and(|p| {
            field(&p.snapshot, "/current_source/nodes/1/selected") == Some("false")
                && p.snapshot.metadata_sequence > initial.snapshot.metadata_sequence
        })
    })
    .await;
    let current = proofs.last().unwrap();
    assert_eq!(
        field(&current.snapshot, "/current_source/nodes/0/selected"),
        Some("true")
    );
    assert_eq!(
        field(
            &current.snapshot,
            "/current_source/selection_scope/selection_revision"
        ),
        Some("1")
    );
    assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    sender
        .try_send(UiAction::ManagementIntent {
            context: current.context.clone(),
            action: Box::new(UiAction::LoadManagementDetail {
                request_id: current.request_id,
                detail_request_id: 99,
                pointer: "/items/1/excerpt".into(),
                cursor: None,
            }),
        })
        .unwrap();
    let deadline = crate::test_watchdog::Deadline::new("retained foreign history list detail");
    loop {
        let update = tokio::task::block_in_place(|| deadline.receive(&incoming)).unwrap();
        let text = match &update {
            UiUpdate::ScopedManagement { update, .. } => match &**update {
                UiUpdate::ManagementDetail { pointer, text, .. }
                    if pointer == "/items/1/excerpt" =>
                {
                    Some(text.clone())
                }
                UiUpdate::ManagementDetailFailure { error, .. } => panic!("{error}"),
                _ => None,
            },
            _ => None,
        };
        ui.apply(update);
        if let Some(text) = text {
            assert!(text.contains("retained second"), "{text}");
            break;
        }
    }
    sender.try_send(UiAction::Quit).unwrap();
    super::control_tests::finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f8_history_repeat_hint_uses_service_language_and_renders_at_80_columns() {
    let ja = "aから同じ履歴照会と引数を指定して再取得。現在状態の更新は元の結果を取り直しません";
    let en = "Use a to enter the same query/args; refresh does not rerun.";
    for language in ["en", "ja"] {
        for command in ["history get", "history list"] {
            let (root, owner, control, server) =
                super::control_tests::fixture_with_language(language).await;
            owner.db.call(|db| {
                db.execute_batch("INSERT INTO node(id,kind,created_at,payload_revision,payload_json) VALUES('hint-node','user','now',7,'{\"role\":\"user\",\"content\":\"retained original\"}'); INSERT INTO thread_path VALUES('independent',0,'hint-node');")?;
                Ok(())
            }).await.unwrap();
            let mut ui = UiState::new(&owner.loaded.config, root.path().into());
            let (sender, receiver) = actions::channel(64);
            let (updates, incoming) = std::sync::mpsc::sync_channel(128);
            let bridge = tokio::spawn(super::run_with_clipboard(
                control,
                receiver,
                updates,
                root.path().into(),
                owner,
                server,
                Arc::new(NoClipboard),
            ));
            dispatch(&sender, ui.select_thread("thread".into()));
            let mut proofs = Vec::new();
            pump_until(&mut ui, &sender, &incoming, &mut proofs, |ui, _| {
                ui.view().thread_id.as_deref() == Some("thread")
                    && !ui.view().navigation.is_loading()
            })
            .await;
            key(&mut ui, &sender, KeyCode::F(8), KeyModifiers::NONE);
            dispatch(&sender, ui.event(Event::Paste(command.into())));
            key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
            if command == "history list" {
                key(&mut ui, &sender, KeyCode::Char('u'), KeyModifiers::CONTROL);
                dispatch(&sender, ui.event(Event::Paste("independent".into())));
            } else {
                dispatch(&sender, ui.event(Event::Paste("hint-node".into())));
            }
            key(&mut ui, &sender, KeyCode::Enter, KeyModifiers::NONE);
            pump_until(&mut ui, &sender, &incoming, &mut proofs, |_, p| {
                p.last()
                    .is_some_and(|p| field(&p.snapshot, "/current_source/repeat_result").is_some())
            })
            .await;
            let snapshot = &proofs.last().unwrap().snapshot;
            let expected = if language == "ja" { ja } else { en };
            assert_eq!(
                field(snapshot, "/current_source/repeat_result"),
                Some(expected)
            );
            assert_eq!(
                field(snapshot, "/current_source/nodes/0/node_id"),
                Some("hint-node")
            );
            if language == "en" {
                let index = snapshot
                    .fields
                    .iter()
                    .position(|f| f.key == "/current_source/repeat_result")
                    .unwrap();
                for _ in 0..index {
                    key(&mut ui, &sender, KeyCode::Down, KeyModifiers::NONE);
                }
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40)).unwrap();
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut ui))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let mut screen = String::new();
                for y in 0..40 {
                    for x in 0..80 {
                        screen.push_str(buffer[(x, y)].symbol());
                    }
                    screen.push('\n');
                }
                let words = screen.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(screen.contains("/current_source/repeat_result"), "{screen}");
                assert!(words.contains("Use a to enter the same"), "{screen}");
                assert!(words.contains("query/args;"), "{screen}");
                assert!(words.contains("refresh does not rerun."), "{screen}");
            }
            sender.try_send(UiAction::Quit).unwrap();
            super::control_tests::finish_bridge(bridge, &incoming).await;
        }
    }
}
