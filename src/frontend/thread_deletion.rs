//! Bounded frontend deletion observations; Core remains the mutation authority.
use super::*;
use crate::config::UiLanguage;
use futures_util::FutureExt;

/// Keep request ownership intact if the operation future panics. The existing
/// worker still delivers a terminal reply and releases its admission permit;
/// an uncertain mutation must be checked again, never automatically replayed.
pub(super) async fn guard<T>(
    work: impl std::future::Future<Output = Result<T, ApiError>>,
    language: UiLanguage,
) -> Result<T, ApiError> {
    std::panic::AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            let message = match language {
                UiLanguage::En => "Deletion stopped unexpectedly. The outcome is unknown. Refresh the list and review the deletion impact before trying again.",
                UiLanguage::Ja => "削除処理が予期せず停止しました。結果は未確認です。一覧を再取得し、削除影響を確認してから再操作してください",
            };
            Err(ApiError::new(
                ErrorCode::IoFailed,
                message,
            ).details(serde_json::json!({"deletion_outcome_uncertain": true})))
        })
}

pub(super) enum Outcome {
    Preview(Value, chrono::DateTime<chrono::Utc>),
    Applied(Vec<String>),
}
pub(super) async fn run(
    owner: native_history::Owner<'_>,
    args: ThreadDeleteArgs,
    execute: bool,
) -> Result<Outcome, ApiError> {
    if !execute {
        let value = owner.call(Command::ThreadDeletePreview(args)).await?;
        return Ok(Outcome::Preview(value, chrono::Utc::now()));
    }
    let target = args.thread_id.clone();
    let result: crate::protocol::ThreadDeleteResult =
        owner.call(Command::ThreadDelete(args)).await?;
    if !result.accepted || !result.deleted_thread_ids.contains(&target) {
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "Thread deletion receipt did not identify its committed targets",
        )
        .details(serde_json::json!({"deletion_outcome_uncertain": true})));
    }
    Ok(Outcome::Applied(result.deleted_thread_ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn accept_preview(
        sender: &crate::ui::actions::ActionSender,
        incoming: &std::sync::mpsc::Receiver<UiUpdate>,
        request_id: u64,
    ) {
        sender
            .try_send(UiAction::ThreadDeletion {
                request_id,
                args: ThreadDeleteArgs {
                    preview_marker: None,
                    thread_id: "thread".into(),
                    descendants: false,
                },
                execute: false,
            })
            .unwrap();
        crate::test_watchdog::watchdog("deletion fixture preview", async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    if let UiUpdate::ThreadDeletion {
                        request_id: id,
                        execute: false,
                        result,
                        ..
                    } = update
                        && id == request_id
                    {
                        assert!(matches!(result, Ok(ThreadDeletionReply::Preview(..))));
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn panicked_delete_returns_uncertain_outcome_without_claiming_not_applied() {
        for (language, expected) in [
            (UiLanguage::En, "The outcome is unknown"),
            (UiLanguage::Ja, "結果は未確認"),
        ] {
            let result: Result<Outcome, ApiError> = guard(
                async {
                    tokio::task::yield_now().await;
                    panic!("fixture deletion panic");
                },
                language,
            )
            .await;
            let error = result.err().expect("panic must become a terminal error");
            assert_eq!(error.code, ErrorCode::IoFailed);
            assert!(error.message.contains(expected));
            assert!(!error.is_storage_failure());
            assert_eq!(
                error.details.as_ref().unwrap()["deletion_outcome_uncertain"],
                true
            );
        }
        assert!(matches!(
            guard(async { Ok(Outcome::Applied(vec![])) }, UiLanguage::Ja).await,
            Ok(Outcome::Applied(ids)) if ids.is_empty()
        ));
    }

    #[tokio::test]
    async fn delete_projects_known_descendants_and_late_autosave_cannot_recreate_deleted_drafts() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let child = request(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                parent_thread_id: Some("thread".into()),
                cwd: None,
                connection: None,
                model: None,
                effort: None,
            }),
        )
        .await
        .unwrap()["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let draft = DraftSnapshot {
            draft_id: "deleted-draft".into(),
            text: "saved before deletion".into(),
            ..Default::default()
        };
        save_draft(
            &selected_owner(&control),
            "thread".into(),
            draft.clone(),
            ViewAnchor::default(),
            true,
        )
        .await
        .unwrap();
        let mut args = ThreadDeleteArgs {
            preview_marker: None,
            thread_id: "thread".into(),
            descendants: true,
        };
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE run SET state='completed' WHERE id='target-run'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let Outcome::Preview(value, _) = run(selected_owner(&control), args.clone(), false)
            .await
            .unwrap()
        else {
            panic!("expected preview")
        };
        assert_eq!(value["deletable_now"], true);
        args.preview_marker =
            Some(serde_json::from_value(value["preview_marker"].clone()).unwrap());
        // The preview is only a snapshot: a Run starting afterwards must
        // still prevent the actual mutation.
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE run SET state='live' WHERE id='target-run'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            run(selected_owner(&control), args.clone(), true)
                .await
                .is_err(),
            "live Run must remain the Core rejection boundary"
        );
        control
            .store
            .database
            .call(|db| {
                db.execute(
                    "UPDATE turn SET state='completed' WHERE id='target-run'",
                    [],
                )?;
                db.execute("UPDATE run SET state='completed' WHERE id='target-run'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let Outcome::Applied(deleted) = run(selected_owner(&control), args, true).await.unwrap()
        else {
            panic!("expected commit")
        };
        assert!(deleted.contains(&"thread".to_owned()));
        assert!(deleted.contains(&child));
        assert_eq!(deleted.len(), 2);
        // A newer autosave queued before deletion cannot resurrect the draft.
        let mut late = draft;
        late.revision += 1;
        late.text = "must never return".into();
        assert_eq!(
            save_draft(
                &selected_owner(&control),
                "thread".into(),
                late,
                ViewAnchor::default(),
                true
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::TargetUnavailable
        );
        control
            .store
            .database
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM draft WHERE thread_id='thread'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM view_state WHERE thread_id='thread'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn frontend_delete_worker_returns_typed_outcome_and_cleans_up_without_popup_ownership() {
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        control
            .store
            .database
            .call(|db| {
                db.execute(
                    "UPDATE turn SET state='completed' WHERE id='target-run'",
                    [],
                )?;
                db.execute("UPDATE run SET state='completed' WHERE id='target-run'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let (sender, receiver) = crate::ui::actions::channel(32);
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        let bridge = tokio::spawn(crate::frontend::run(
            control.clone(),
            receiver,
            updates,
            root.path().into(),
            owner,
            server,
        ));
        sender
            .try_send(UiAction::SelectThread {
                thread_id: "thread".into(),
                generation: 0,
                view_revision: 0,
                known_view: None,
            })
            .unwrap();
        crate::test_watchdog::watchdog("src/frontend/thread_deletion.rs: frontend_delete_worker_returns_typed_outcome_and_cleans_up_without_popup_ownership", async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    if matches!(update, UiUpdate::History { ref thread_id, .. } if thread_id == "thread") {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        let live = control.fixture_live_text("thread");
        assert!(live.record(0, "pending output", false));
        live.received(std::time::Instant::now());
        crate::test_watchdog::watchdog("native preview owned before deletion", async {
            loop {
                if matches!(incoming.try_recv(), Ok(UiUpdate::ModelReceive { thread_id, attempt_id, .. })
                    if thread_id == "thread" && attempt_id == "fixture-attempt") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let draft = DraftSnapshot {
            draft_id: "worker-draft".into(),
            text: "queued autosave".into(),
            ..Default::default()
        };
        sender
            .try_send(UiAction::SaveDraft {
                thread_id: "thread".into(),
                draft: draft.clone(),
                anchor: ViewAnchor::default(),
                follow: true,
            })
            .unwrap();
        accept_preview(&sender, &incoming, 98).await;
        sender
            .try_send(UiAction::ThreadDeletion {
                request_id: 99,
                args: ThreadDeleteArgs {
                    preview_marker: None,
                    thread_id: "thread".into(),
                    descendants: false,
                },
                execute: true,
            })
            .unwrap();
        let deleted = crate::test_watchdog::watchdog("src/frontend/thread_deletion.rs: frontend_delete_worker_returns_typed_outcome_and_cleans_up_without_popup_ownership", async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    if let UiUpdate::ThreadDeletion {
                        request_id: 99,
                        execute: true,
                        result,
                        ..
                    } = update
                    {
                        return result.unwrap();
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(deleted, ThreadDeletionReply::Deleted(ids) if ids == ["thread"]));
        // After the deletion result is delivered, stale stream/receive events
        // cannot keep the deleted selection alive or repopulate its display.
        let stale = control.fixture_live_text("thread");
        assert!(stale.record(0, "late output", false));
        stale.received(std::time::Instant::now());
        let late = super::super::saved_events::wait_scoped(
            &root.path().join("data"),
            &control.store.instance_id,
            None,
            false,
            Some("thread".into()),
        )
        .await
        .unwrap();
        assert!(
            late.live_text
                .unwrap()
                .iter()
                .any(|attempt| attempt.receive_age_ms.is_some())
        );
        // A native timing update for the new selection fences the old wait.
        let independent = control.fixture_live_text("independent");
        independent.received(std::time::Instant::now());
        sender
            .try_send(UiAction::SelectThread {
                thread_id: "independent".into(),
                generation: 1,
                view_revision: 0,
                known_view: None,
            })
            .unwrap();
        crate::test_watchdog::watchdog("src/frontend/thread_deletion.rs: frontend_delete_worker_returns_typed_outcome_and_cleans_up_without_popup_ownership", async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    match update {
                        UiUpdate::ModelReceive { thread_id, generation: 1, .. }
                            if thread_id == "independent" => return,
                        UiUpdate::Stream { thread_id, .. }
                        | UiUpdate::ModelReceive { thread_id, .. }
                        | UiUpdate::PreviewGap { thread_id, .. } => {
                            assert_ne!(
                                thread_id, "thread",
                                "deleted selection must release stream ownership"
                            );
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        sender
            .try_send(UiAction::SaveDraft {
                thread_id: "thread".into(),
                draft,
                anchor: ViewAnchor::default(),
                follow: true,
            })
            .unwrap();
        sender.try_send(UiAction::Quit).unwrap();
        crate::frontend::control_tests::finish_bridge(bridge, &incoming).await;
        // The graceful shutdown closes the actor; inspect its durable result.
        let db = rusqlite::Connection::open(root.path().join("data/shellboy.db")).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM draft WHERE thread_id='thread'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM view_state WHERE thread_id='thread'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn failed_saved_view_cleanup_rolls_back_thread_deletion_and_draft_cleanup() {
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let client = selected_owner(&control);
        save_draft(
            &client,
            "thread".into(),
            DraftSnapshot {
                draft_id: "preserved-draft".into(),
                text: "saved input".into(),
                ..Default::default()
            },
            ViewAnchor::default(),
            true,
        )
        .await
        .unwrap();
        let draft_get = || {
            Command::DraftGet(crate::protocol::DraftGetArgs {
                thread_id: Some("thread".into()),
                operation_id: None,
            })
        };
        let before: crate::protocol::DraftState = client.call(draft_get()).await.unwrap();
        assert_eq!(before.draft.as_ref().unwrap().text, "saved input");
        assert!(before.view.as_ref().unwrap().follow_tail);
        owner.db.call(|db| {
            db.execute_batch("UPDATE run SET state='completed' WHERE id='target-run';
                CREATE TRIGGER fail_view_delete BEFORE DELETE ON view_state
                WHEN OLD.thread_id='thread' BEGIN SELECT RAISE(ABORT,'fixture view cleanup failure'); END;")?;
            Ok(())
        }).await.unwrap();
        let args = ThreadDeleteArgs {
            preview_marker: None,
            thread_id: "thread".into(),
            descendants: false,
        };
        let error = run(client, args, true)
            .await
            .err()
            .expect("delete must fail");
        assert_eq!(error.code, ErrorCode::SaveFailed);
        let refused = client
            .call::<crate::protocol::DraftState>(draft_get())
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidState);
        server.shutdown().await.unwrap();
        assert_eq!(
            control.shutdown().await.unwrap_err().code,
            ErrorCode::SaveFailed
        );
        owner.shutdown().await.unwrap();

        let owner = crate::app::DataOwner::start(
            Some(root.path().join("config.toml")),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = crate::control::Control::new(&owner, root.path().into());
        let (rpc, receive) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
        let after: crate::protocol::DraftState =
            selected_owner(&control).call(draft_get()).await.unwrap();
        assert_eq!(after.draft, before.draft);
        assert_eq!(after.view, before.view);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn parent_only_delete_preserves_child() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let child = request(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                parent_thread_id: Some("thread".into()),
                cwd: None,
                connection: None,
                model: None,
                effort: None,
            }),
        )
        .await
        .unwrap()["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE run SET state='completed' WHERE id='target-run'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let outcome = run(
            selected_owner(&control),
            ThreadDeleteArgs {
                preview_marker: None,
                thread_id: "thread".into(),
                descendants: false,
            },
            true,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Applied(deleted) if deleted == ["thread"]));
        assert!(
            request(
                &control,
                Command::ThreadGet(crate::protocol::ThreadGetArgs {
                    thread_id: Some(child),
                    display: false,
                })
            )
            .await
            .is_ok()
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    struct FailedClipboard;
    impl ClipboardBackend for FailedClipboard {
        fn capture(&self, _: u64, _: CancellationToken) -> ClipboardFuture<'_, ClipboardPixels> {
            Box::pin(async {
                Err(ApiError::new(
                    ErrorCode::IoFailed,
                    "fixture capture failure",
                ))
            })
        }
        fn copy(
            &self,
            _: String,
            _: CopyBackend,
            _: usize,
            _: u64,
            _: CancellationToken,
        ) -> ClipboardFuture<'_, CopyOutcome> {
            Box::pin(async { unreachable!("copy not requested") })
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submitted_image_failure_is_reported_once_while_unsubmitted_failure_keeps_draft_notice()
    {
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let (sender, receiver) = crate::ui::actions::channel(16);
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        let bridge = tokio::spawn(run_with_clipboard_slots(
            control,
            receiver,
            updates,
            root.path().into(),
            owner,
            server,
            Arc::new(FailedClipboard),
            Arc::new(Semaphore::new(2)),
        ));
        sender
            .try_send(UiAction::SubmittedCommand {
                submission: CommandSubmission {
                    draft_id: "slash".into(),
                    revision: 1,
                },
                action: Box::new(UiAction::PasteImage {
                    pin: Default::default(),
                    draft_id: "slash".into(),
                    revision: 1,
                    position: 0,
                    thread_id: None,
                }),
            })
            .unwrap();
        sender
            .try_send(UiAction::PasteImage {
                pin: Default::default(),
                draft_id: "bare".into(),
                revision: 1,
                position: 0,
                thread_id: None,
            })
            .unwrap();
        let mut command_errors = 0;
        let mut bare_errors = 0;
        let mut duplicate_errors = 0;
        crate::test_watchdog::watchdog("src/frontend/thread_deletion.rs: submitted_image_failure_is_reported_once_while_unsubmitted_failure_keeps_draft_notice", async {
            while command_errors == 0 || bare_errors == 0 {
                while let Ok(update) = incoming.try_recv() {
                    match update {
                        UiUpdate::CommandOutcome {
                            submission,
                            result: Err(error),
                        } if submission.draft_id == "slash" => {
                            assert!(error.message.contains("fixture capture failure"));
                            command_errors += 1;
                        }
                        UiUpdate::DraftStatus { draft_id, text }
                            if text.contains("fixture capture failure") =>
                        {
                            if draft_id == "bare" {
                                bare_errors += 1;
                            } else {
                                duplicate_errors += 1;
                            }
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // Both captures completed; give the result channel a bounded observation
        // window for an erroneously duplicated slash DraftStatus.
        tokio::time::sleep(Duration::from_millis(50)).await;
        while let Ok(update) = incoming.try_recv() {
            if matches!(update,UiUpdate::DraftStatus{draft_id,text} if draft_id=="slash"&&text.contains("fixture capture failure"))
            {
                duplicate_errors += 1;
            }
        }
        assert_eq!(command_errors, 1);
        assert_eq!(bare_errors, 1);
        assert_eq!(duplicate_errors, 0);
        sender.try_send(UiAction::Quit).unwrap();
        crate::frontend::control_tests::finish_bridge(bridge, &incoming).await;
    }
}
