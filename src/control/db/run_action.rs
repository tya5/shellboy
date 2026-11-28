//! Exact saved Run selection fences for stop and retry.
use super::*;

pub(super) fn check_selection(
    tx: &Transaction<'_>,
    run: &str,
    expected: Option<&ExpectedRunSelection>,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let actual: Option<(String, Option<String>, Option<i64>)> = tx.query_row(
        "SELECT r.id,u.id,u.generation FROM thread t JOIN run r ON r.id=(SELECT id FROM run WHERE thread_id=t.id ORDER BY created_at DESC,id DESC LIMIT 1) LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1) WHERE t.id=?1 AND t.deleted_at IS NULL",
        [&expected.thread_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(sql_error)?;
    let (id, generation) = match &expected.turn {
        Some(turn) => (
            Some(turn.turn_id.as_str()),
            i64::try_from(turn.generation).ok(),
        ),
        None => (None, None),
    };
    if actual
        .as_ref()
        .map(|(run, id, generation)| (run.as_str(), id.as_deref(), *generation))
        != Some((run, id, generation))
    {
        return Err(error(
            ErrorCode::InvalidState,
            "The displayed Run or Turn changed; refresh before acting",
        )
        .details(json!({"reason":"expected_turn_changed","request_may_have_been_applied":false})));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let database = DbWorker::open(root.path().join("run-action.db"))
            .await
            .unwrap();
        database.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','fixture','synthetic','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2020-01-01T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES('u','t','r','recovery_wait',4,'2020-01-01T00:00:00Z');")?;Ok(())}).await.unwrap();
        let store = Store::new(
            database,
            Arc::new(Config::default()),
            root.path().into(),
            "fixture".into(),
        );
        (root, store)
    }
    struct ExpectedRun {
        thread_id: String,
        run_id: String,
        turn_id: Option<String>,
        turn_generation: Option<u64>,
    }
    enum RunAction {
        Stop,
        Retry,
    }
    fn expected() -> ExpectedRun {
        ExpectedRun {
            thread_id: "t".into(),
            run_id: "r".into(),
            turn_id: Some("u".into()),
            turn_generation: Some(4),
        }
    }
    async fn execute(
        store: &Store,
        operation: String,
        expected: ExpectedRun,
        action: RunAction,
    ) -> Result<MutationResult> {
        let turn = match (expected.turn_id, expected.turn_generation) {
            (Some(turn_id), Some(generation)) => Some(ExpectedTurn {
                turn_id,
                generation,
            }),
            (None, None) => None,
            _ => return Err(error(ErrorCode::InvalidArgument, "Incomplete test witness")),
        };
        let expected_selection = Some(ExpectedRunSelection {
            thread_id: expected.thread_id,
            turn,
        });
        let run_id = expected.run_id;
        let command = match action {
            RunAction::Stop => Command::RunStop(RunStopTarget::Run {
                run_id,
                expected_selection,
            }),
            RunAction::Retry => Command::RunRetry(RunRetryArgs {
                run_id,
                expected_turn: None,
                expected_selection,
            }),
        };
        store.execute(Request::new(operation, command), None).await
    }
    async fn state(store: &Store) -> (String, String, i64) {
        store.database.call(|db|Ok(db.query_row("SELECT r.state,u.state,u.generation FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id='r'",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)).await.unwrap()
    }
    fn no_effects(result: &MutationResult) {
        assert!(
            result.stop_runs.is_empty()
                && result.quiet_wake_threads.is_empty()
                && result.changed_threads.is_empty()
                && result.compact_threads.is_empty()
        );
    }
    #[tokio::test]
    async fn guarded_stop_reuses_canonical_effects_and_replay_has_no_repeated_effects() {
        crate::test_watchdog::watchdog("guarded Run mutation and cleanup", async {
            let (_root, store) = fixture().await;
            let first = execute(&store, "fixture.0:stop".into(), expected(), RunAction::Stop)
                .await
                .unwrap();
            assert_eq!(first.stop_runs, ["r"]);
            assert_eq!(
                state(&store).await,
                ("stopping".into(), "recovery_wait".into(), 5)
            );
            let replay = execute(&store, "fixture.0:stop".into(), expected(), RunAction::Stop)
                .await
                .unwrap();
            assert_eq!(replay.result, first.result);
            no_effects(&replay);
            let mut changed = expected();
            changed.turn_generation = Some(5);
            assert_eq!(
                execute(&store, "fixture.0:stop".into(), changed, RunAction::Stop)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::OperationConflict
            );
            store.database.shutdown().await.unwrap();
        })
        .await
        .expect("guarded Run action watchdog");
    }
    #[tokio::test]
    async fn replacement_run_generation_aba_deleted_thread_and_invalid_pairs_cannot_mutate() {
        crate::test_watchdog::watchdog("guarded Run mutation and cleanup", async {
        for case in 0..5 {
            let (_root, store) = fixture().await;
            let mut guard = expected();
            match case {
                0 => guard.turn_generation = Some(3),
                1 => {
                    store.database.call(|db|{db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('replacement','t','completed','2021-01-01T00:00:00Z')",[])?;Ok(())}).await.unwrap();
                }
                2 => {
                    store
                        .database
                        .call(|db| {
                            db.execute("UPDATE thread SET deleted_at='now' WHERE id='t'", [])?;
                            Ok(())
                        })
                        .await
                        .unwrap();
                }
                3 => guard.turn_id = None,
                _ => guard.turn_generation = None,
            }
            let before = state(&store).await;
            assert!(
                execute(&store, format!("fixture.0:bad-{case}"), guard, RunAction::Stop)
                    .await
                    .is_err()
            );
            assert_eq!(state(&store).await, before);
            store.database.shutdown().await.unwrap();
        }
        }).await.expect("guarded Run action watchdog");
    }
    #[tokio::test]
    async fn retry_requires_exact_recovery_generation_and_uses_existing_quiet_wake() {
        crate::test_watchdog::watchdog("guarded Run mutation and cleanup", async {
            let (_root, store) = fixture().await;
            let first = execute(
                &store,
                "fixture.0:retry".into(),
                expected(),
                RunAction::Retry,
            )
            .await
            .unwrap();
            assert_eq!(first.quiet_wake_threads, ["t"]);
            assert!(first.stop_runs.is_empty());
            assert_ne!(state(&store).await.1, "recovery_wait");
            let replay = execute(
                &store,
                "fixture.0:retry".into(),
                expected(),
                RunAction::Retry,
            )
            .await
            .unwrap();
            no_effects(&replay);
            let mut current = expected();
            current.turn_generation = Some(5);
            assert!(
                execute(
                    &store,
                    "fixture.0:retry-again".into(),
                    current,
                    RunAction::Retry
                )
                .await
                .is_err(),
                "exact identity alone cannot retry a non-recovery Turn"
            );
            store.database.shutdown().await.unwrap();
        })
        .await
        .expect("guarded Run action watchdog");
    }
    #[tokio::test]
    async fn absent_turn_witness_matches_only_an_actually_absent_turn() {
        crate::test_watchdog::watchdog("guarded Run mutation and cleanup", async {
            let (_root, store) = fixture().await;
            let mut guard = expected();
            guard.turn_id = None;
            guard.turn_generation = None;
            assert!(
                execute(
                    &store,
                    "fixture.0:unexpected-turn".into(),
                    ExpectedRun {
                        thread_id: guard.thread_id.clone(),
                        run_id: guard.run_id.clone(),
                        turn_id: None,
                        turn_generation: None
                    },
                    RunAction::Stop
                )
                .await
                .is_err()
            );
            store
                .database
                .call(|db| {
                    db.execute("DELETE FROM turn WHERE id='u'", [])?;
                    Ok(())
                })
                .await
                .unwrap();
            let stopped = execute(&store, "fixture.0:no-turn".into(), guard, RunAction::Stop)
                .await
                .unwrap();
            assert_eq!(stopped.stop_runs, ["r"]);
            store.database.shutdown().await.unwrap();
        })
        .await
        .expect("guarded Run action watchdog");
    }
    #[tokio::test]
    async fn failed_reducer_transaction_leaves_original_state_and_no_success_receipt() {
        crate::test_watchdog::watchdog("guarded Run mutation and cleanup", async {
        let (_root, store) = fixture().await;
        store.database.call(|db|{db.execute_batch("CREATE TRIGGER reject_guarded_stop BEFORE UPDATE OF state ON run BEGIN SELECT RAISE(ABORT,'synthetic rollback'); END;")?;Ok(())}).await.unwrap();
        assert!(
            execute(&store, "fixture.0:rollback".into(), expected(), RunAction::Stop)
                .await
                .is_err()
        );
        assert_eq!(
            state(&store).await,
            ("live".into(), "recovery_wait".into(), 4)
        );
        store
            .database
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM operation_receipt WHERE state='completed'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        store.database.shutdown().await.unwrap();
        }).await.expect("guarded Run action watchdog");
    }
}
