//! Bounded frontend deletion observations; Core remains the mutation authority.
use super::*;

pub(super) const MAX_KNOWN_THREADS: usize = 4_096;
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
