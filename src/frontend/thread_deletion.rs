//! Bounded frontend deletion observations; Core remains the mutation authority.
use super::*;
use futures_util::FutureExt;

/// Keep request ownership intact if the operation future panics. The existing
/// worker still delivers a terminal reply and releases its admission permit;
/// an uncertain mutation must be checked again, never automatically replayed.
pub(super) async fn guard<T>(
    work: impl std::future::Future<Output = Result<T, ApiError>>,
) -> Result<T, ApiError> {
    std::panic::AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Err(ApiError::new(
                ErrorCode::IoFailed,
                "削除処理が予期せず停止しました。結果は未確認です。一覧を再取得し、削除影響を確認してから再操作してください",
            ).details(serde_json::json!({"deletion_outcome_uncertain": true})))
        })
}

pub(super) const MAX_KNOWN_THREADS: usize = 4_096;
/// Selection generations are stale-response fences, not owned view IDs. They
/// deliberately do not contribute to the finite deletion observation set.
pub(super) fn candidates(
    mut known: Vec<String>,
    selection: &selection::Selection,
    owned: impl Iterator<Item = String>,
) -> Vec<String> {
    known.extend(selection.active_thread.iter().cloned());
    known.extend(owned);
    known.sort();
    known.dedup();
    known
}
pub(super) enum Outcome {
    Preview(Value),
    Applied,
}
pub(super) async fn run(
    control: &Arc<Control>,
    args: ThreadDeleteArgs,
    execute: bool,
) -> Result<Outcome, ApiError> {
    let command = if execute {
        Command::ThreadDelete(args)
    } else {
        Command::ThreadDeletePreview(args)
    };
    let value = request(control, command).await?;
    if !execute {
        return Ok(Outcome::Preview(value));
    }
    if value.get("accepted").and_then(Value::as_bool) != Some(true) {
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "Thread deletion was not accepted",
        ));
    }
    Ok(Outcome::Applied)
}

/// Observe only frontend-known IDs. Never traverse an unbounded descendant tree
/// or assume that preview results describe the later mutation's actual outcome.
pub(super) async fn cleanup(
    control: &Control,
    candidates: Vec<String>,
) -> Result<(Vec<String>, Vec<String>), ApiError> {
    let candidates: HashSet<_> = candidates.into_iter().collect();
    if candidates.len() > MAX_KNOWN_THREADS {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Too many cached Thread IDs for deletion cleanup",
        ));
    }
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let mut deleted = Vec::new();
            let mut drafts = Vec::new();
            for thread in candidates {
                let live: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                    [&thread],
                    |row| row.get(0),
                )?;
                if live {
                    continue;
                }
                let mut statement = tx.prepare(
                    "SELECT id FROM draft WHERE thread_id=?1 AND state='editing' LIMIT 4096",
                )?;
                drafts.extend(
                    statement
                        .query_map([&thread], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
                drop(statement);
                tx.execute("DELETE FROM draft WHERE thread_id=?1", [&thread])?;
                tx.execute("DELETE FROM view_state WHERE thread_id=?1", [&thread])?;
                deleted.push(thread);
            }
            deleted.sort();
            drafts.sort();
            drafts.dedup();
            tx.commit()?;
            Ok((deleted, drafts))
        })
        .await
        .map_err(storage_error)
}

/// Core success is irreversible even when local saved-view cleanup fails.
/// Always return the confirmed root; descendants require a successful DB observation.
pub(super) async fn finish_cleanup(
    control: &Control,
    root: &str,
    candidates: Vec<String>,
) -> (Vec<String>, Vec<String>, Option<String>) {
    let observed_candidates = candidates.clone();
    let cleanup_result = cleanup(control, candidates).await;
    match checked_storage(control, cleanup_result).await {
        Ok((mut deleted, drafts)) => {
            if !deleted.iter().any(|id| id == root) {
                deleted.push(root.to_owned());
            }
            (deleted, drafts, None)
        }
        Err(error) => {
            // A failed write transaction can still permit read-only tombstone
            // observation, so an active deleted child also leaves its view.
            let observed = if observed_candidates.len() <= MAX_KNOWN_THREADS {
                control.store.database.call(move |db| {
                    let mut deleted = Vec::new();
                    for id in observed_candidates {
                        let dead: bool = db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NOT NULL)",
                            [&id], |row| row.get(0))?;
                        if dead { deleted.push(id); }
                    }
                    Ok(deleted)
                }).await.ok()
            } else {
                None
            };
            let mut deleted = observed.unwrap_or_default();
            deleted.push(root.to_owned());
            deleted.sort();
            deleted.dedup();
            let warning = if error.is_storage_failure() {
                format!("正本DBの保存障害により新規実行を停止し資源を回収しています: {error}")
            } else {
                error.to_string()
            };
            (deleted, vec![], Some(warning))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn panicked_delete_returns_uncertain_outcome_without_claiming_not_applied() {
        let result: Result<Outcome, ApiError> = guard(async {
            tokio::task::yield_now().await;
            panic!("fixture deletion panic");
        })
        .await;
        let error = result.err().expect("panic must become a terminal error");
        assert_eq!(error.code, ErrorCode::IoFailed);
        assert!(error.message.contains("結果は未確認"));
        assert!(error.message.contains("一覧を再取得"));
        assert!(!error.is_storage_failure());
        assert_eq!(
            error.details.as_ref().unwrap()["deletion_outcome_uncertain"],
            true
        );
        assert!(matches!(
            guard(async { Ok(Outcome::Applied) }).await,
            Ok(Outcome::Applied)
        ));
    }

    #[tokio::test]
    async fn frontend_cleanup_save_failed_warns_new_draft_and_stops_admission() {
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        control.store.database.call(|db| {
            db.execute("UPDATE turn SET state='completed' WHERE id='target-run'", [])?;
            db.execute("UPDATE run SET state='completed' WHERE id='target-run'", [])?;
            db.execute("INSERT INTO view_state(thread_id,follow_tail) VALUES('thread',1)", [])?;
            db.execute_batch("CREATE TRIGGER fixture_cleanup_failure BEFORE DELETE ON view_state BEGIN SELECT RAISE(ABORT,'fixture frontend cleanup failure'); END;")?;
            Ok(())
        }).await.unwrap();
        let mut ui = crate::ui::UiState::new(&control.store.config, root.path().into());
        let (sender, receiver) = crate::ui::actions::channel(32);
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        let mut bridge = tokio::spawn(crate::frontend::run(
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
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    let loaded = matches!(&update, UiUpdate::History { thread_id, .. } if thread_id == "thread");
                    ui.apply(update);
                    if loaded { return; }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        let old_draft = ui.view().draft.draft_id.clone();
        sender
            .try_send(UiAction::ThreadDeletion {
                request_id: 144,
                args: ThreadDeleteArgs {
                    thread_id: "thread".into(),
                    descendants: false,
                },
                execute: true,
                known_thread_ids: vec!["thread".into()],
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    let deleted = matches!(
                        &update,
                        UiUpdate::ThreadDeletion {
                            request_id: 144,
                            result: Ok(ThreadDeletionReply::DeletedWithWarning { .. }),
                            ..
                        }
                    );
                    ui.apply(update);
                    if deleted {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(ui.view().thread_id.is_none());
        assert_ne!(ui.view().draft.draft_id, old_draft);
        assert!(
            ui.view()
                .status
                .contains("削除しました。後片付けに失敗しました")
        );
        assert!(ui.view().status.contains("新規実行を停止"));
        assert!(
            ui.view()
                .status
                .contains("fixture frontend cleanup failure")
        );
        ui.insert("retained fresh draft", false);
        assert_eq!(ui.view().draft.text, "retained fresh draft");
        assert!(
            request(
                &control,
                Command::ThreadCreate(ThreadCreateArgs {
                    parent_id: None,
                    cwd: None,
                    connection: None,
                    model: None,
                    effort: None,
                })
            )
            .await
            .is_err(),
            "cleanup failure must close owner admission before delivering the warning"
        );
        sender.try_send(UiAction::Quit).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(if cfg!(windows) { 30 } else { 15 }),
            async {
                loop {
                    while incoming.try_recv().is_ok() {}
                    tokio::select! {
                        result = &mut bridge => return result,
                        _ = tokio::time::sleep(Duration::from_millis(5)) => {},
                    }
                }
            },
        )
        .await
        .unwrap()
        .expect("frontend task must not panic");
        assert_eq!(result.unwrap_err().code, ErrorCode::SaveFailed);
    }

    #[tokio::test]
    async fn delete_projects_known_descendants_and_late_autosave_cannot_recreate_deleted_drafts() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let child = request(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                parent_id: Some("thread".into()),
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
            &control,
            "thread".into(),
            draft.clone(),
            ViewAnchor::default(),
            true,
        )
        .await
        .unwrap();
        let args = ThreadDeleteArgs {
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
        assert!(matches!(
            run(&control, args.clone(), false).await.unwrap(),
            Outcome::Preview(value) if value["deletable_now"] == true
        ));
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
            run(&control, args.clone(), true).await.is_err(),
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
        assert!(matches!(
            run(&control, args, true).await.unwrap(),
            Outcome::Applied
        ));
        let (deleted, drafts) = cleanup(
            &control,
            vec!["thread".into(), child.clone(), "independent".into()],
        )
        .await
        .unwrap();
        assert!(deleted.contains(&"thread".to_owned()));
        assert!(deleted.contains(&child));
        assert!(!deleted.contains(&"independent".to_owned()));
        assert_eq!(drafts, ["deleted-draft"]);
        // This models an already queued/inflight autosave arriving after the
        // deletion transaction and cleanup, with a newer revision than the old one.
        let mut late = draft;
        late.revision += 1;
        late.text = "must never return".into();
        save_draft(&control, "thread".into(), late, ViewAnchor::default(), true)
            .await
            .unwrap();
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
                preview_sources: vec![],
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    if matches!(update, UiUpdate::History { ref thread_id, .. } if thread_id == "thread") {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        control.emit_test_event(EngineEvent::Stream {
            thread_id: "thread".into(),
            node_id: "deleted-attempt".into(),
            text: "pending output".into(),
            reasoning: false,
            stream_char_offset: 0,
        });
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
        sender
            .try_send(UiAction::ThreadDeletion {
                request_id: 99,
                args: ThreadDeleteArgs {
                    thread_id: "thread".into(),
                    descendants: false,
                },
                execute: true,
                known_thread_ids: vec!["thread".into(), "independent".into()],
            })
            .unwrap();
        let deleted = tokio::time::timeout(Duration::from_secs(10), async {
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
        control.emit_test_event(EngineEvent::ModelReceive {
            thread_id: "thread".into(),
            attempt_id: "after-deletion".into(),
            observed_at: std::time::Instant::now(),
        });
        control.emit_test_event(EngineEvent::Stream {
            thread_id: "thread".into(),
            node_id: "deleted-attempt".into(),
            text: "late output".into(),
            reasoning: false,
            stream_char_offset: 14,
        });
        control.emit_test_event(EngineEvent::Error {
            thread_id: "independent".into(),
            error: ApiError::new(ErrorCode::IoFailed, "post-deletion-event-fence"),
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    match update {
                        UiUpdate::Stream { thread_id, .. }
                        | UiUpdate::ModelReceive { thread_id, .. } => {
                            assert_ne!(
                                thread_id, "thread",
                                "deleted selection must release stream ownership"
                            );
                        }
                        UiUpdate::Status { text, .. }
                            if text.contains("post-deletion-event-fence") =>
                        {
                            return;
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
    async fn successful_core_delete_remains_success_when_saved_view_cleanup_fails() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE run SET state='completed' WHERE id='target-run'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let args = ThreadDeleteArgs {
            thread_id: "thread".into(),
            descendants: false,
        };
        assert!(matches!(
            run(&control, args, true).await.unwrap(),
            Outcome::Applied
        ));
        // A failure after Core commit must never become a deletion failure.
        control.store.database.call(|db| {
            db.execute_batch("CREATE TRIGGER fixture_cleanup_failure BEFORE DELETE ON view_state BEGIN SELECT RAISE(ABORT,'fixture cleanup failure'); END;")?;
            db.execute("INSERT OR IGNORE INTO view_state(thread_id,follow_tail) VALUES('thread',1)", [])?;
            Ok(())
        }).await.unwrap();
        let (deleted, _, warning) = finish_cleanup(&control, "thread", vec!["thread".into()]).await;
        assert_eq!(deleted, ["thread"]);
        let warning = warning.unwrap();
        assert!(warning.contains("fixture cleanup failure"));
        assert!(warning.contains("新規実行を停止"));
        assert!(
            request(
                &control,
                Command::ThreadCreate(ThreadCreateArgs {
                    parent_id: None,
                    cwd: None,
                    connection: None,
                    model: None,
                    effort: None,
                })
            )
            .await
            .is_err(),
            "stopped owner must reject new mutations"
        );
        server.shutdown().await.unwrap();
        assert_eq!(
            control.shutdown().await.unwrap_err().code,
            ErrorCode::SaveFailed
        );
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn bounded_cleanup_observation_error_preserves_root_without_stopping_owner() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let (deleted, _, warning) = finish_cleanup(
            &control,
            "confirmed-root",
            (0..MAX_KNOWN_THREADS + 1)
                .map(|n| format!("t{n}"))
                .collect(),
        )
        .await;
        assert_eq!(deleted, ["confirmed-root"]);
        let warning = warning.unwrap();
        assert!(warning.contains("Too many cached Thread IDs"));
        assert!(!warning.contains("新規実行を停止"));
        assert!(
            request(
                &control,
                Command::ThreadCreate(ThreadCreateArgs {
                    parent_id: None,
                    cwd: None,
                    connection: None,
                    model: None,
                    effort: None,
                })
            )
            .await
            .is_ok()
        );
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
                parent_id: Some("thread".into()),
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
        run(
            &control,
            ThreadDeleteArgs {
                thread_id: "thread".into(),
                descendants: false,
            },
            true,
        )
        .await
        .unwrap();
        let (deleted, _, warning) =
            finish_cleanup(&control, "thread", vec!["thread".into(), child.clone()]).await;
        assert_eq!(deleted, ["thread"]);
        assert!(warning.is_none());
        assert!(
            request(
                &control,
                Command::ThreadGet(ThreadTarget {
                    thread_id: Some(child)
                })
            )
            .await
            .is_ok()
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn historical_selection_generations_do_not_exhaust_deletion_observation_budget() {
        let mut selection = selection::Selection::default();
        for n in 0..MAX_KNOWN_THREADS + 100 {
            selection.select_thread(format!("old{n}"), n as u64);
        }
        selection.select_thread("active".into(), 1);
        let result = candidates(
            vec!["visible".into()],
            &selection,
            ["saved".into(), "image".into()].into_iter(),
        );
        assert_eq!(result, ["active", "image", "saved", "visible"]);
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
                    draft_id: "slash".into(),
                    revision: 1,
                    position: 0,
                    thread_id: None,
                }),
            })
            .unwrap();
        sender
            .try_send(UiAction::PasteImage {
                draft_id: "bare".into(),
                revision: 1,
                position: 0,
                thread_id: None,
            })
            .unwrap();
        let mut command_errors = 0;
        let mut bare_errors = 0;
        let mut duplicate_errors = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            while command_errors == 0 || bare_errors == 0 {
                while let Ok(update) = incoming.try_recv() {
                    match update {
                        UiUpdate::CommandOutcome {
                            submission,
                            result: Err(error),
                        } if submission.draft_id == "slash" => {
                            assert!(error.contains("fixture capture failure"));
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
