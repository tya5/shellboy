use crate::ui::{DraftSnapshot, UiAction, ViewAnchor, actions};

#[test]
fn unexpected_no_rows_is_not_reclassified_as_a_missing_ui_target() {
    let error = super::storage_error(crate::storage::StorageError::Sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ));
    assert_eq!(error.code, crate::protocol::ErrorCode::SaveFailed);
}

#[tokio::test]
async fn missing_history_targets_are_local_errors_and_do_not_stop_other_runs() {
    let (_root, owner, control, server) = fixture().await;
    let (updates, incoming) = std::sync::mpsc::sync_channel(32);
    let revision = owner
        .db
        .call(|db| {
            Ok(
                crate::history::selection_revision(db, "thread", crate::history::ThreadScope::Live)?
                    as u64,
            )
        })
        .await
        .unwrap();
    let cursor = crate::protocol::PageCursor {
        version: 1,
        scope: "ui_history".into(),
        filter: "thread".into(),
        last_sort_key: "10".into(),
        watermark: 0,
        revision,
        encoding: None,
    }
    .encode()
    .unwrap();
    let reader = super::HistoryReader::new(&control, &updates, 80);
    reader
        .page(&super::Reply::new("thread", 7), Some(cursor.clone()), None)
        .await
        .unwrap();
    assert!(
        matches!(incoming.try_recv(),Ok(crate::ui::UiUpdate::History {thread_id,generation:7,append:true,..}) if thread_id=="thread")
    );
    for target in ["missing", "thread"] {
        if target == "thread" {
            owner
                .db
                .call(|db| {
                    db.execute("UPDATE thread SET deleted_at='now' WHERE id='thread'", [])?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        let reader = super::HistoryReader::new(&control, &updates, 80);
        for before in [None, Some(cursor.clone())] {
            let error = reader
                .page(&super::Reply::new(target, 0), before, None)
                .await
                .unwrap_err();
            assert_eq!(error.code, crate::protocol::ErrorCode::TargetUnavailable);
        }
        let reply = super::WindowReply::new(target, 0, 0);
        let error = reader
            .rebase(&reply, ViewAnchor::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, crate::protocol::ErrorCode::TargetUnavailable);
        let error = reader
            .window(&reply, super::WindowRequest::new(0, 0, false, 0))
            .await
            .unwrap_err();
        assert_eq!(error.code, crate::protocol::ErrorCode::TargetUnavailable);
    }
    let state = owner
        .db
        .call(|db| {
            Ok(db.query_row(
                "SELECT state FROM run WHERE id='independent-run'",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(state, "live");
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

fn save(text: &str, revision: u64) -> UiAction {
    UiAction::SaveDraft {
        thread_id: "thread".into(),
        draft: DraftSnapshot {
            text: text.into(),
            revision,
            ..DraftSnapshot::default()
        },
        anchor: ViewAnchor::default(),
        follow: true,
    }
}

#[tokio::test]
async fn cancel_has_a_reserved_bounded_coalesced_mailbox_and_closed_receiver_rejects() {
    let (sender, mut receiver) = actions::channel(1);
    sender
        .try_send(UiAction::RenderWidth { width: 10 })
        .unwrap();
    for _ in 0..1000 {
        sender
            .try_send(UiAction::Cancel {
                thread_id: "first".into(),
            })
            .unwrap();
    }
    assert!(
        matches!(receiver.recv().await,Some(UiAction::Cancel{thread_id}) if thread_id=="first")
    );
    // The in-flight target also occupies its one reserved entry.
    for i in 0..31 {
        sender
            .try_send(UiAction::Cancel {
                thread_id: format!("pending-{i}"),
            })
            .unwrap();
    }
    assert!(matches!(
        sender.try_send(UiAction::Cancel {
            thread_id: "overflow".into()
        }),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_))
    ));
    assert!(matches!(
        receiver.recv().await,
        Some(UiAction::RenderWidth { .. })
    ));
    receiver.complete_cancel();
    assert!(matches!(
        receiver.recv().await,
        Some(UiAction::Cancel { .. })
    ));
    drop(receiver);
    assert!(matches!(
        sender.try_send(UiAction::Quit),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
    ));
}

#[tokio::test]
async fn quit_bypasses_full_normal_queue_but_preserves_accepted_send_and_drafts_in_order() {
    let (sender, mut receiver) = actions::channel(3);
    sender
        .try_send(UiAction::Send {
            generation: 0,
            now: false,
            snapshot: None,
            draft_id: "send".into(),
            revision: 0,
            thread_id: Some("thread".into()),
            cwd: "/tmp".into(),
            text: "accepted send".into(),
            attachments: vec![],
            connection: None,
            model: None,
            effort: None,
        })
        .unwrap();
    sender.try_send(save("accepted older draft", 1)).unwrap();
    sender
        .try_send(UiAction::RenderWidth { width: 10 })
        .unwrap();
    assert!(
        sender
            .try_send_batch(vec![save("latest active draft", 2), UiAction::Quit])
            .is_empty()
    );
    assert!(
        sender
            .try_send_batch(vec![save("duplicate must not replace", 3), UiAction::Quit])
            .is_empty()
    );
    assert!(matches!(
        sender.try_send(UiAction::RenderWidth { width: 20 }),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
    ));
    assert!(matches!(receiver.recv().await,Some(UiAction::Send{text,..}) if text=="accepted send"));
    assert!(receiver.is_closing());
    assert!(matches!(
        sender.try_send(UiAction::RenderWidth { width: 20 }),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
    ));
    assert!(
        matches!(receiver.recv().await,Some(UiAction::SaveDraft{draft,..}) if draft.text=="accepted older draft")
    );
    assert!(
        matches!(receiver.recv().await,Some(UiAction::SaveDraft{draft,..}) if draft.text=="latest active draft")
    );
    assert!(matches!(receiver.recv().await, Some(UiAction::Quit)));
    for _ in 0..1000 {
        sender.try_send(UiAction::WaitForShutdown).unwrap();
        sender.try_send(UiAction::ForceQuit).unwrap();
    }
    assert!(matches!(
        receiver.recv().await,
        Some(UiAction::WaitForShutdown)
    ));
    assert!(matches!(receiver.recv().await, Some(UiAction::ForceQuit)));
}

pub(crate) async fn fixture() -> (
    tempfile::TempDir,
    crate::app::DataOwner,
    std::sync::Arc<crate::control::Control>,
    crate::ipc::LocalServer,
) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:9/v1\"\n").unwrap();
    let owner = crate::app::DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = crate::control::Control::new(&owner, root.path().into());
    let cwd = root.path().to_string_lossy().to_string();
    owner.db.call(move |db|{
        for id in ["thread","independent"] {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?1,?2,?3,'fixture','fixture-model','now')",rusqlite::params![id,cwd,format!("{cwd}/AGENTS.md")])?;
        }
        for (run,thread) in [("target-run","thread"),("independent-run","independent")] {
            db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,'live','now')",rusqlite::params![run,thread])?;
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?1,'recovery_wait','now')",rusqlite::params![run,thread])?;
        }
        db.execute("UPDATE turn SET state='running' WHERE id='independent-run'", [])?;
        Ok(())
    }).await.unwrap();
    let (rpc, receive) = tokio::sync::mpsc::channel(16);
    let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
    tokio::spawn(control.clone().serve(receive));
    (root, owner, control, server)
}
fn drain(updates: &std::sync::mpsc::Receiver<crate::ui::UiUpdate>) {
    while updates.try_recv().is_ok() {}
}

// These tests assert eventual cleanup, not the UI's shutdown grace. Await the
// task directly while draining its bounded update channel, instead of keeping
// a test runtime busy with yield_now polling during filesystem/IPC shutdown.
pub(super) async fn finish_bridge(
    mut bridge: tokio::task::JoinHandle<Result<(), crate::protocol::ApiError>>,
    incoming: &std::sync::mpsc::Receiver<crate::ui::UiUpdate>,
) {
    let watchdog_seconds = if cfg!(windows) { 30 } else { 15 };
    let mut last_progress = None;
    let completed = tokio::time::timeout(std::time::Duration::from_secs(watchdog_seconds), async {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            while let Ok(update) = incoming.try_recv() {
                if let crate::ui::UiUpdate::ShutdownProgress {
                    elapsed_ms,
                    detail,
                    force_available,
                } = update
                {
                    last_progress = Some((elapsed_ms, detail, force_available));
                }
            }
            tokio::select! {
                result = &mut bridge => return result,
                _ = tick.tick() => {},
            }
        }
    })
    .await;
    let joined = completed.unwrap_or_else(|_| {
        panic!("frontend cleanup watchdog expired; last progress: {last_progress:?}")
    });
    joined
        .expect("frontend task panicked during cleanup")
        .expect("frontend graceful cleanup failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_its_thread_while_all_ordinary_command_slots_are_waiting() {
    use crate::protocol::{Command, WaitArgs, WaitCondition};
    let (root, owner, control, server) = fixture().await;
    let (sender, receiver) = actions::channel(128);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
    let bridge = tokio::spawn(super::run_with_clipboard_slots(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        std::sync::Arc::new(super::ClipboardAdapter::new().unwrap()),
        slots.clone(),
    ));
    for _ in 0..2 {
        sender
            .try_send(UiAction::Manage(Command::Wait(WaitArgs {
                conditions: vec![WaitCondition::RunEnded {
                    run_id: "independent-run".into(),
                }],
                timeout_ms: 60000,
            })))
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            drain(&incoming);
            let count: i64 = control
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                        [],
                        |row| row.get(0),
                    )?)
                })
                .await
                .unwrap();
            if count == 2 && slots.available_permits() == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    sender
        .try_send(UiAction::Cancel {
            thread_id: "thread".into(),
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            drain(&incoming);
            let state: String = control
                .store
                .database
                .call(|db| {
                    Ok(
                        db.query_row("SELECT state FROM run WHERE id='target-run'", [], |row| {
                            row.get(0)
                        })?,
                    )
                })
                .await
                .unwrap();
            if state == "stopped" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let other: String = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT state FROM run WHERE id='independent-run'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(other, "live");
    sender.try_send(UiAction::Quit).unwrap();
    finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saturated_queue_quit_persists_prior_send_and_latest_snapshot_without_starting_management_waits()
 {
    use crate::protocol::{Command, WaitArgs, WaitCondition};
    let (root, owner, control, server) = fixture().await;
    // This case exercises a fresh Run rather than explicit recovery of the seed.
    control
        .store
        .database
        .call(|db| {
            db.execute("UPDATE turn SET state='stopped'", [])?;
            db.execute("UPDATE run SET state='stopped'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let (sender, receiver) = actions::channel(8);
    sender
        .try_send(UiAction::Send {
            generation: 0,
            now: false,
            snapshot: None,
            draft_id: "accepted-send".into(),
            revision: 1,
            thread_id: Some("thread".into()),
            cwd: root.path().into(),
            text: "preserved queued message".into(),
            attachments: vec![],
            connection: None,
            model: None,
            effort: None,
        })
        .unwrap();
    sender.try_send(save("older saved snapshot", 1)).unwrap();
    for _ in 0..6 {
        sender
            .try_send(UiAction::Manage(Command::Wait(WaitArgs {
                conditions: vec![WaitCondition::RunEnded {
                    run_id: "independent-run".into(),
                }],
                timeout_ms: 60000,
            })))
            .unwrap();
    }
    assert!(matches!(
        sender.try_send(UiAction::RenderWidth { width: 20 }),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_))
    ));
    assert!(
        sender
            .try_send_batch(vec![save("latest quit snapshot", 2), UiAction::Quit])
            .is_empty()
    );
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(crate::frontend::run(
        control,
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    finish_bridge(bridge, &incoming).await;
    let db = rusqlite::Connection::open(root.path().join("data/shellboy.db")).unwrap();
    let count:i64=db.query_row("SELECT COUNT(*) FROM node WHERE kind='user' AND payload_json LIKE '%preserved queued message%'",[],|row|row.get(0)).unwrap();
    assert_eq!(count, 1);
    let body: String = db
        .query_row(
            "SELECT body_json FROM draft WHERE thread_id='thread' ORDER BY revision DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<DraftSnapshot>(&body).unwrap().text,
        "latest quit snapshot"
    );
    let waits: i64 = db
        .query_row("SELECT COUNT(*) FROM operation_receipt", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        waits <= 1,
        "management requests must not start ahead of Quit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserved_shutdown_choices_keep_the_existing_grace_wait_and_force_boundaries() {
    use crate::ui::UiUpdate;
    let (sender, mut receiver) = actions::channel(1);
    sender
        .try_send(UiAction::RenderWidth { width: 20 })
        .unwrap();
    sender.try_send(UiAction::Quit).unwrap();
    assert!(matches!(receiver.recv().await, Some(UiAction::Quit)));
    // An early force is ignored just as it is on the ordinary receiver.
    sender.try_send(UiAction::ForceQuit).unwrap();
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let shutdown = tokio::spawn(async move {
        crate::frontend::wait_for_shutdown(
            &mut receiver,
            &updates,
            std::time::Duration::from_millis(20),
            std::future::pending(),
        )
        .await
    });
    async fn progress(incoming: &std::sync::mpsc::Receiver<UiUpdate>, wanted: bool) {
        tokio::time::timeout(std::time::Duration::from_secs(2),async{
            loop {
                while let Ok(update)=incoming.try_recv(){if matches!(update,UiUpdate::ShutdownProgress{force_available,..} if force_available==wanted){return;}}
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.unwrap();
    }
    progress(&incoming, true).await;
    assert!(!shutdown.is_finished());
    drain(&incoming);
    sender.try_send(UiAction::WaitForShutdown).unwrap();
    progress(&incoming, false).await;
    assert!(!shutdown.is_finished());
    progress(&incoming, true).await;
    sender.try_send(UiAction::ForceQuit).unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.details.unwrap()["forced_shutdown"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_remains_available_at_full_receipt_budget_without_creating_a_receipt() {
    use crate::protocol::{Command, ErrorCode, ThreadTarget, WaitArgs, WaitCondition};
    let (root, owner, control, server) = fixture().await;
    let (sender, receiver) = actions::channel(4);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
    let bridge = tokio::spawn(super::run_with_clipboard_slots(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        std::sync::Arc::new(super::ClipboardAdapter::new().unwrap()),
        slots.clone(),
    ));
    for _ in 0..2 {
        sender
            .try_send(UiAction::Manage(Command::Wait(WaitArgs {
                conditions: vec![WaitCondition::RunEnded {
                    run_id: "independent-run".into(),
                }],
                timeout_ms: 60000,
            })))
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            drain(&incoming);
            let count: i64 = control
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                        [],
                        |row| row.get(0),
                    )?)
                })
                .await
                .unwrap();
            if count == 2 && slots.available_permits() == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let instance = control.store.instance_id.clone();
    control.store.database.call(move |db| {
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes)VALUES(?1,?2,'null','synthetic-pending','pending','2000-01-01',268435456)",rusqlite::params![format!("{instance}:full"),instance])?;
        Ok(())
    }).await.unwrap();
    let receipt_budget = |db: &mut rusqlite::Connection| {
        Ok((
            db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |row| {
                row.get::<_, i64>(0)
            })?,
            db.query_row(
                "SELECT logical_bytes FROM quota_counter WHERE category='operation_receipt'",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            db.query_row(
                "SELECT COALESCE(SUM(reserved_bytes),0) FROM operation_receipt",
                [],
                |row| row.get::<_, i64>(0),
            )?,
        ))
    };
    let before = control.store.database.call(receipt_budget).await.unwrap();
    assert!(
        before.2 >= 256 * 1024 * 1024,
        "pending intent keeps mutation admission full"
    );
    let observed = super::request(
        &control,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some("thread".into()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(observed["thread_id"], "thread");
    assert_eq!(
        control.store.database.call(receipt_budget).await.unwrap(),
        before,
        "full-budget observation allocates neither a receipt nor a reservation"
    );
    assert_eq!(
        control
            .cancel_thread("missing".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::ThreadNotFound
    );
    sender
        .try_send(UiAction::Cancel {
            thread_id: "thread".into(),
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            drain(&incoming);
            let state: String = control
                .store
                .database
                .call(|db| {
                    Ok(
                        db.query_row("SELECT state FROM run WHERE id='target-run'", [], |row| {
                            row.get(0)
                        })?,
                    )
                })
                .await
                .unwrap();
            if state == "stopped" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        slots.available_permits(),
        0,
        "ordinary slots remain occupied throughout stop"
    );
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT state FROM run WHERE id='independent-run'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "live"
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |row| row
                    .get::<_, i64>(0))?,
                3,
                "only the two waits and the synthetic full record exist"
            );
            db.execute(
                "INSERT INTO app_meta(key,value)VALUES('source-still-writable','yes')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    sender.try_send(UiAction::Quit).unwrap();
    finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_queue_rejection_and_domain_error_recover_input_but_success_releases_it() {
    use crate::ui::{UiState, UiUpdate};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let (root, owner, control, server) = fixture().await;
    let (sender, receiver) = actions::channel(16);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let mut saturation = Some(slots.clone().acquire_owned().await.unwrap());
    let bridge = tokio::spawn(super::run_with_clipboard_slots(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        std::sync::Arc::new(super::ClipboardAdapter::new().unwrap()),
        slots.clone(),
    ));
    for (command, successful) in [
        ("/compact".to_owned(), false),
        ("/fork missing-node".into(), false),
        (format!("/cwd {}", root.path().display()), true),
    ] {
        let mut ui = UiState::new(&control.store.config, root.path().into());
        ui.select_thread("thread".into());
        ui.insert(&command, false);
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let UiAction::SubmittedCommand { submission, .. } = &actions[0] else {
            panic!("{actions:?}")
        };
        let revision = submission.revision;
        sender.try_send(actions[0].clone()).unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    if let UiUpdate::CommandOutcome { submission, result } = update {
                        assert_eq!(result.is_ok(), successful, "{command}: {result:?}");
                        return UiUpdate::CommandOutcome { submission, result };
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        ui.apply(outcome);
        if successful {
            assert!(ui.view().draft.text.is_empty());
            assert!(!ui.view().pending.contains_key(&revision));
        } else {
            assert_eq!(ui.view().draft.text, command);
            assert_eq!(
                ui.view().pending[&revision].status,
                crate::ui::ReceiptStatus::Failed
            );
        }
        // First rejection is deterministic frontend admission, not a core reply.
        // The following operations enter the real dispatcher with a free slot.
        if command == "/compact" {
            drop(saturation.take());
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while slots.available_permits() == 0 {
                drain(&incoming);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    sender.try_send(UiAction::Quit).unwrap();
    finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_preparation_failure_keeps_newer_input_and_the_original_snapshot() {
    use crate::ui::{ReceiptStatus, UiAttachment, UiState, UiUpdate};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let (root, owner, control, server) = fixture().await;
    let (sender, receiver) = actions::channel(16);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard_slots(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        std::sync::Arc::new(super::ClipboardAdapter::new().unwrap()),
        std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
    ));
    let mut ui = UiState::new(&control.store.config, root.path().into());
    ui.select_thread("thread".into());
    ui.insert("original", false);
    let attachment_position = ui.view().draft.text.len();
    ui.insert(crate::ui::draft::MARKER, false);
    ui.view_mut().draft.attachments.push(UiAttachment {
        id: "invalid-kind".into(),
        path: root.path().join("not-opened"),
        kind: "invalid".into(),
        state: "ready".into(),
        position: Some(attachment_position),
    });
    let actions = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let UiAction::Send { revision, .. } = &actions[0] else {
        panic!()
    };
    let revision = *revision;
    ui.insert(" latest", false);
    let newer = ui.view().draft.clone();
    sender.try_send(actions[0].clone()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            while let Ok(update) = incoming.try_recv() {
                let failed = matches!(&update, UiUpdate::Receipt { revision: r, status: ReceiptStatus::Failed, .. } if *r == revision);
                ui.apply(update);
                if failed { return; }
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(ui.view().draft, newer);
    assert_eq!(
        ui.view().pending[&revision].snapshot.text,
        format!("original{}", crate::ui::draft::MARKER)
    );
    assert!(ui.view().status.contains(&format!("/restore {revision}")));
    sender.try_send(UiAction::Quit).unwrap();
    finish_bridge(bridge, &incoming).await;
}

#[tokio::test]
async fn recovery_projection_uses_saved_failure_and_freezes_time_without_guessing_the_stage() {
    let (_root, owner, control, server) = fixture().await;
    let projection = control.store.database.call(|db| {
        let reason = serde_json::json!({"code":"io_failed","message":"provider: fixture failure"});
        let model = serde_json::json!({"error":{"kind":"provider","message":"fixture failure","status":500}});
        db.execute("UPDATE turn SET created_at='2026-10-25T00:00:00Z',recovery_reason_json=?1 WHERE id='target-run'",[reason.to_string()])?;
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at) VALUES('a','target-run',0,0,'fixture','fixture-model',0,'incomplete','{}',?1,'2026-10-25T00:00:00Z','2026-10-25T00:00:02Z')",[model.to_string()])?;
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES('pending','message','now','{}',2),('result','tool_result','now','{}',2)",[])?;
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at) VALUES('pending','op','{}','thread','pending','target-run','target-run','held','now')",[])?;
        db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','target-run','completed','now')",[])?;
        db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g','c',0,'shell','{}','completed')",[])?;
        db.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','c','result','completed')",[])?;
        super::activity::project(db,"thread")
    }).await.unwrap();
    assert_eq!(
        projection.snapshot.phase,
        crate::ui::activity::ActivityPhase::Recovery
    );
    assert!(!projection.snapshot.turn_in_progress);
    let recovery = projection.snapshot.recovery.unwrap();
    assert_eq!(recovery.stage, "モデル要求");
    assert_eq!(recovery.message, "fixture failure");
    assert_eq!(recovery.pending_messages, 1);
    assert_eq!(recovery.saved_tool_results, 1);
    let context_projection=control.store.database.call(|db| {
        db.execute("UPDATE model_attempt SET error_json=json_set(error_json,'$.error.provider_code','context_length_exceeded') WHERE id='a'",[])?;
        super::activity::project(db,"thread")
    }).await.unwrap();
    assert_eq!(
        context_projection.snapshot.recovery.unwrap().stage,
        "モデル要求（入力容量超過）"
    );
    // An automatic reduction may fail after a model attempt was marked failed.
    // Matching error text alone is not proof that recovery was caused by it.
    let ambiguous = control
        .store
        .database
        .call(|db| {
            db.execute("UPDATE model_attempt SET state='failed' WHERE id='a'", [])?;
            super::activity::project(db, "thread")
        })
        .await
        .unwrap();
    assert!(
        ambiguous
            .snapshot
            .recovery
            .unwrap()
            .stage
            .contains("記録なし")
    );
    // The saved attempt belongs to a previous generation; it cannot explain
    // failure timing or stage in the new generation.
    let projection = control
        .store
        .database
        .call(|db| {
            db.execute(
                "UPDATE model_attempt SET state='incomplete' WHERE id='a'",
                [],
            )?;
            db.execute("UPDATE turn SET generation=1 WHERE id='target-run'", [])?;
            super::activity::project(db, "thread")
        })
        .await
        .unwrap();
    assert!(!projection.snapshot.turn_in_progress);
    assert!(
        projection
            .snapshot
            .recovery
            .as_ref()
            .unwrap()
            .stage
            .contains("記録なし")
    );
    let projection = control.store.database.call(|db| {
        db.execute("UPDATE run SET state='stopped',stop_reason='explicit_stop' WHERE id='target-run'",[])?;
        db.execute("UPDATE turn SET state='stopped',ended_at='2026-10-25T00:00:03Z' WHERE id='target-run'",[])?;
        super::activity::project(db,"thread")
    }).await.unwrap();
    assert_eq!(
        projection.snapshot.phase,
        crate::ui::activity::ActivityPhase::Stopped
    );
    assert_eq!(
        projection
            .snapshot
            .recovery
            .as_ref()
            .unwrap()
            .stop_reason
            .as_deref(),
        Some("explicit_stop")
    );
    // Startup saves a generic interruption reason for every interrupted Turn;
    // it is not proof of a previous recovery_wait state.
    let interrupted = control.store.database.call(|db| {
        db.execute("UPDATE turn SET recovery_reason_json=?1 WHERE id='target-run'",[serde_json::json!({"reason":"previous_instance_interrupted","result_uncertain":true}).to_string()])?;
        super::activity::project(db,"thread")
    }).await.unwrap();
    assert_eq!(
        interrupted.snapshot.phase,
        crate::ui::activity::ActivityPhase::Stopped
    );
    assert!(interrupted.snapshot.recovery.is_none());
    // A real recovery_wait still has a recovery entry when a legacy/unknown
    // reason cannot establish a more specific stage or cause.
    let unknown = control
        .store
        .database
        .call(|db| {
            db.execute(
                "UPDATE run SET state='live',stop_reason=NULL WHERE id='target-run'",
                [],
            )?;
            db.execute(
                "UPDATE turn SET state='recovery_wait' WHERE id='target-run'",
                [],
            )?;
            super::activity::project(db, "thread")
        })
        .await
        .unwrap();
    assert!(
        unknown
            .snapshot
            .recovery
            .unwrap()
            .stage
            .contains("記録なし")
    );
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_refreshes_a_pinned_tool_outside_the_latest_fifty_nodes_without_reloading_its_body()
{
    use crate::{protocol::Command, ui::UiState};
    let (root, owner, control, server) = fixture().await;
    control.store.database.call(|db|{
        let tx=db.transaction()?;
        tx.execute_batch("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','target-run',0,0,'fixture','fixture-model',0,'completed','{}','now'); INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','target-run','completed','now'); INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g','call',0,'shell','{\"command\":\"sleep 30\"}','completed','execution'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('execution','target-run','target-run','shell-operation','running','/','{}','UTF-8','UTF-8','closed','now');")?;
        crate::history::append_node(&tx,"thread","call-node",Some("target-run"),"assistant_response",&serde_json::json!({"group_id":"g","output":[{"type":"function_call","name":"shell","call_id":"call","arguments":"{\"command\":\"sleep 30\"}"}]}),"now")?;
        for i in 0..60 {crate::history::append_node(&tx,"thread",&format!("speech-{i}"),Some("target-run"),"assistant_response",&serde_json::json!({"text":format!("saved answer {i}")}),"now")?;}
        tx.commit()?;Ok(())
    }).await.unwrap();
    let mut ui = UiState::new(&control.store.config, root.path().into());
    let (sender, receiver) = actions::channel(32);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    for action in ui.select_thread("thread".into()) {
        sender.try_send(action).unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            while let Ok(update) = incoming.try_recv() {
                ui.apply(update);
                for action in ui.take_deferred_actions() {
                    sender.try_send(action).unwrap();
                }
            }
            if ui.view().total_nodes == 61 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let generation = ui.view().generation;
    sender
        .try_send(UiAction::LoadHistoryWindow {
            thread_id: "thread".into(),
            generation,
            view_revision: ui.view().view_revision,
            ordinal: 0,
            char_offset: 0,
            from_end: false,
            selection_revision: ui.view().selection_revision,
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            while let Ok(update) = incoming.try_recv() {
                ui.apply(update);
                for action in ui.take_deferred_actions() {
                    sender.try_send(action).unwrap();
                }
            }
            if ui
                .view()
                .nodes()
                .iter()
                .any(|node| node.node_id == "call-node")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    ui.view_mut().follow = false;
    ui.view_mut().anchor = ViewAnchor {
        node_id: Some("call-node".into()),
        char_offset: 0,
    };
    let original = ui
        .view()
        .nodes()
        .iter()
        .find(|node| node.node_id == "call-node")
        .unwrap()
        .clone();
    assert_eq!(
        original.presentation.blocks[0]
            .tool
            .as_ref()
            .unwrap()
            .current_state
            .as_deref(),
        Some("running_current")
    );
    control
        .store
        .database
        .call(|db| {
            db.execute(
                "UPDATE shell_execution SET state='exited',exit_code=0 WHERE id='execution'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // A real RunStop completion emits canonical Changed. No history reload
    // is requested after it; the already-exited shell remains terminal.
    let response = control
        .request(super::operation(
            &control,
            Command::RunStop(crate::protocol::RunTarget {
                run_id: "target-run".into(),
            }),
        ))
        .await;
    assert!(response.ok, "{response:?}");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            while let Ok(update) = incoming.try_recv() {
                ui.apply(update);
                for action in ui.take_deferred_actions() {
                    sender.try_send(action).unwrap();
                }
            }
            if ui
                .view()
                .nodes()
                .iter()
                .find(|node| node.node_id == "call-node")
                .unwrap()
                .presentation
                .blocks[0]
                .tool
                .as_ref()
                .unwrap()
                .current_state
                .as_deref()
                == Some("exited")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let refreshed = ui
        .view()
        .nodes()
        .iter()
        .find(|node| node.node_id == "call-node")
        .unwrap();
    assert_eq!(refreshed.text, original.text);
    assert_eq!(refreshed.base_char_offset, original.base_char_offset);
    assert_eq!(ui.view().anchor.node_id.as_deref(), Some("call-node"));
    sender.try_send(UiAction::Quit).unwrap();
    finish_bridge(bridge, &incoming).await;
}

mod management_scope;

mod thread_settings;

mod answer_details;

struct UnusedClipboard;
impl super::ClipboardBackend for UnusedClipboard {
    fn capture(
        &self,
        _: u64,
        _: tokio_util::sync::CancellationToken,
    ) -> super::ClipboardFuture<'_, super::ClipboardPixels> {
        panic!("management fixtures must not access the host clipboard")
    }
    fn copy(
        &self,
        _: String,
        _: super::CopyBackend,
        _: usize,
        _: u64,
        _: tokio_util::sync::CancellationToken,
    ) -> super::ClipboardFuture<'_, super::CopyOutcome> {
        panic!("management fixtures must not access the host clipboard")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_mutation_report_retains_authoritative_outcome_on_its_ui_owner() {
    use crate::{
        protocol::*,
        ui::{ManagementContext, UiState, UiUpdate},
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    fn request(ui: &mut UiState) -> (ManagementContext, UiAction) {
        fn context(action: &UiAction) -> ManagementContext {
            match action {
                UiAction::SubmittedCommand { action, .. } => context(action),
                UiAction::ManagementIntent { context, .. } => context.clone(),
                _ => panic!("expected scoped command"),
            }
        }
        ui.insert("/status", false);
        let action = ui
            .event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .remove(0);
        (context(&action), action)
    }
    for (command, success, stop) in [
        (
            Command::ThreadDelete(ThreadDeleteArgs {
                thread_id: "deletable".into(),
                descendants: false,
            }),
            true,
            false,
        ),
        (
            Command::ThreadDelete(ThreadDeleteArgs {
                thread_id: "missing".into(),
                descendants: false,
            }),
            false,
            false,
        ),
        (
            Command::RunStop(RunTarget {
                run_id: "target-run".into(),
            }),
            true,
            true,
        ),
        (
            Command::StorageCollect(StorageArgs {
                category: Some(StorageCategory::Raw),
            }),
            true,
            false,
        ),
    ] {
        let (root, owner, control, server) = fixture().await;
        owner.db.call(|db| {
            db.execute("UPDATE thread SET updated_at='2026-01-01T00:00:00Z'", [])?;
            db.execute("UPDATE run SET created_at='2026-01-01T00:00:00Z'", [])?;
            db.execute("UPDATE turn SET created_at='2026-01-01T00:00:00Z'", [])?;
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) SELECT 'deletable','delete',cwd,agents_path,profile_id,model_id,updated_at FROM thread WHERE id='thread'", [])?;
            Ok(())
        }).await.unwrap();
        let mut ui = UiState::new(&control.store.config, root.path().into());
        ui.select_thread("thread".into());
        let (old, _) = request(&mut ui);
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        let (_, latest) = request(&mut ui);
        let (sender, receiver) = actions::channel(16);
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
        // Both requests occupy their worker slot before the DB gate opens. No
        // time-based scheduling assumption can let the old report win the epoch.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let db = owner.db.clone();
        let gate = tokio::spawn(async move {
            db.call(move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
        });
        started_rx.await.unwrap();
        let bridge = tokio::spawn(super::run_with_clipboard_slots(
            control.clone(),
            receiver,
            updates,
            root.path().into(),
            owner,
            server,
            std::sync::Arc::new(UnusedClipboard),
            slots.clone(),
        ));
        sender
            .try_send(UiAction::ManagementIntent {
                context: old,
                action: Box::new(UiAction::Manage(command)),
            })
            .unwrap();
        sender.try_send(latest).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while slots.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        release_tx.send(()).unwrap();
        gate.await.unwrap();
        let mut notices = Vec::new();
        let mut reports = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    match &update {
                        UiUpdate::Status {
                            thread_id: Some(thread),
                            text,
                        } if text.contains("照会ID") => {
                            assert_eq!(
                                thread, "thread",
                                "notification owner must not become the referenced target"
                            );
                            notices.push(text.clone());
                        }
                        UiUpdate::ScopedManagement { update, .. } => {
                            if let UiUpdate::StructuredManagement { title, .. } = update.as_ref() {
                                reports.push(title.clone());
                            }
                        }
                        _ => {}
                    }
                    ui.apply(update);
                }
                if !notices.is_empty() && !reports.is_empty() && slots.available_permits() == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            reports,
            ["作業状態"],
            "the stale report body is never emitted"
        );
        assert_eq!(notices.len(), 1);
        let (id, response): (String, String) = control.store.database.call(|db| {
            Ok(db.query_row("SELECT operation_id,result_ref_json FROM operation_receipt WHERE result_ref_json IS NOT NULL", [], |r| Ok((r.get(0)?, r.get(1)?)))?)
        }).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["error"].is_null(), success, "{response}");
        assert_eq!(response["result"].is_null(), !success, "{response}");
        assert!(notices[0].ends_with(&id), "{}", notices[0]);
        assert!(notices[0].starts_with(if stop {
            "停止要求を受け付けました"
        } else if success {
            "受付結果: 成功"
        } else {
            "受付結果: 失敗"
        }));
        assert_eq!(
            ui.view().status,
            notices[0],
            "new read reports must not consume the outcome notice"
        );
        assert_eq!(ui.management.as_ref().unwrap().title, "作業状態");
        sender.try_send(UiAction::Quit).unwrap();
        finish_bridge(bridge, &incoming).await;
    }
}

#[derive(Default)]
struct ScopeClipboard {
    calls: std::sync::Mutex<Vec<(String, usize)>>,
}
impl super::ClipboardBackend for ScopeClipboard {
    fn capture(
        &self,
        _: u64,
        _: tokio_util::sync::CancellationToken,
    ) -> super::ClipboardFuture<'_, super::ClipboardPixels> {
        panic!("copy scope fixture must not capture the host clipboard")
    }
    fn copy(
        &self,
        text: String,
        _: super::CopyBackend,
        budget: usize,
        _: u64,
        _: tokio_util::sync::CancellationToken,
    ) -> super::ClipboardFuture<'_, super::CopyOutcome> {
        let mut calls = self.calls.lock().unwrap();
        let outcome = calls.len() % 3;
        calls.push((text, budget));
        Box::pin(async move {
            match outcome {
                0 => Ok(super::CopyOutcome::OsCopied),
                1 => Ok(super::CopyOutcome::TerminalRequest(
                    b"fixture OSC52 request".to_vec(),
                )),
                _ => Err(crate::protocol::ApiError::new(
                    crate::protocol::ErrorCode::IoFailed,
                    "fixture copy failure",
                )),
            }
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_worker_preserves_raw_scope_budget_and_distinguishes_os_completion_from_terminal_request()
 {
    use crate::ui::{CopyScope, UiUpdate};
    let (root, owner, control, server) = fixture().await;
    let clipboard = std::sync::Arc::new(ScopeClipboard::default());
    let (sender, receiver) = actions::channel(16);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run_with_clipboard_slots(
        control,
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
        clipboard.clone(),
        std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
    ));
    let raw = "raw\u{1b}[31m 日本語\n";
    for scope in [
        CopyScope::OriginalPage { has_more: true },
        CopyScope::OriginalPage { has_more: false },
        CopyScope::ManagementPage { has_more: true },
        CopyScope::ManagementPage { has_more: false },
        CopyScope::Value,
        CopyScope::Body,
        CopyScope::CodeBlock,
    ] {
        for outcome in 0..3 {
            sender
                .try_send(UiAction::CopyText {
                    text: raw.into(),
                    scope,
                })
                .unwrap();
            let (mut terminal, mut result) = (Vec::new(), None);
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while result.is_none() {
                    while let Ok(update) = incoming.try_recv() {
                        match update {
                            UiUpdate::TerminalRequest { bytes } => terminal.push(bytes),
                            UiUpdate::Status { text, .. } => result = Some(text),
                            _ => {}
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let text = result.unwrap();
            match outcome {
                0 => {
                    assert_eq!(text, format!("{}をコピーしました", scope.label()));
                    assert!(terminal.is_empty());
                }
                1 => {
                    assert_eq!(
                        text,
                        format!(
                            "OSC52: {}のコピー要求を端末へ送りました（完了未確認）",
                            scope.label()
                        )
                    );
                    assert_eq!(terminal, [b"fixture OSC52 request".to_vec()]);
                    assert!(!text.contains("コピーしました"));
                }
                _ => {
                    assert!(text.contains("fixture copy failure"));
                    assert!(terminal.is_empty());
                    assert!(!text.contains("コピーしました"));
                }
            }
        }
    }
    {
        let calls = clipboard.calls.lock().unwrap();
        assert_eq!(calls.len(), 21);
        assert!(
            calls
                .iter()
                .all(|(text, budget)| text == raw && *budget == 65536)
        );
    }
    sender.try_send(UiAction::Quit).unwrap();
    finish_bridge(bridge, &incoming).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_preserves_rejected_inactive_and_active_drafts_at_saturated_admission() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let (root, owner, control, server) = fixture().await;
    let mut ui = crate::ui::UiState::new(&owner.loaded.config, root.path().into());
    ui.select_thread("thread".into());
    ui.insert("inactive latest 日本語", false);
    let rejected = ui.select_thread("independent".into()).remove(0);
    ui.insert("active latest 😀", false);
    let (sender, receiver) = actions::channel(1);
    sender
        .try_send(UiAction::RenderWidth { width: 20 })
        .unwrap();
    ui.action_rejected(&sender.try_send(rejected).unwrap_err().into_inner());
    let quit = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(
        quit.iter()
            .filter(|a| matches!(a, UiAction::SaveDraft { .. }))
            .count(),
        2
    );
    assert!(sender.try_send_batch(quit).is_empty());
    // A duplicate shutdown request cannot replace the accepted final snapshots.
    assert!(
        sender
            .try_send_batch(vec![
                save("must not replace accepted bundle", 99),
                UiAction::Quit
            ])
            .is_empty()
    );
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run(
        control,
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    finish_bridge(bridge, &incoming).await;
    let db = rusqlite::Connection::open(root.path().join("data/shellboy.db")).unwrap();
    for (thread, expected) in [
        ("thread", "inactive latest 日本語"),
        ("independent", "active latest 😀"),
    ] {
        let raw: String = db
            .query_row(
                "SELECT body_json FROM draft WHERE thread_id=?1",
                [thread],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<DraftSnapshot>(&raw).unwrap().text,
            expected
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_before_initial_draft_restore_cannot_mask_an_existing_saved_draft() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let (root, owner, control, server) = fixture().await;
    super::save_draft(
        &control,
        "thread".into(),
        DraftSnapshot {
            draft_id: "persisted".into(),
            revision: 7,
            text: "saved before startup".into(),
            ..Default::default()
        },
        ViewAnchor::default(),
        true,
    )
    .await
    .unwrap();
    let mut ui = crate::ui::UiState::new(&owner.loaded.config, root.path().into());
    ui.select_thread("thread".into()); // Deliberately do not deliver Thread restore yet.
    let (sender, receiver) = actions::channel(1);
    let quit = ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(
        quit.iter()
            .filter(|a| matches!(a, UiAction::SaveDraft { .. }))
            .count(),
        0
    );
    assert!(sender.try_send_batch(quit).is_empty());
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(super::run(
        control,
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    finish_bridge(bridge, &incoming).await;
    let db = rusqlite::Connection::open(root.path().join("data/shellboy.db")).unwrap();
    let (count, raw): (i64, String) = db
        .query_row(
            "SELECT count(*),body_json FROM draft WHERE thread_id='thread'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        serde_json::from_str::<DraftSnapshot>(&raw).unwrap().text,
        "saved before startup"
    );
}
