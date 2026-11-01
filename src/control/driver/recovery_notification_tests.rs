use super::*;

pub(super) fn assert_reason_and_event(
    db: &rusqlite::Connection,
    turn: &str,
    generation: i64,
) -> serde_json::Value {
    let (raw, count): (String, i64) = db.query_row(
        "SELECT recovery_reason_json, (SELECT count(*) FROM json_each(recovery_reason_json, '$.details') WHERE key='last_failure_at') FROM turn WHERE id=?1 AND state='recovery_wait'",
        [turn], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(count, 1);
    let reason: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let timestamp = reason
        .pointer("/details/last_failure_at")
        .and_then(|v| v.as_str())
        .unwrap();
    chrono::DateTime::parse_from_rfc3339(timestamp).expect("RFC3339 transition time");
    let event: String = db
        .query_row(
            "SELECT payload_ref_json FROM event WHERE id=?1 AND kind='child_recovery_wait'",
            [format!("child-recovery-{turn}-{generation}")],
            |r| r.get(0),
        )
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&event).unwrap();
    assert_eq!(payload.get("error"), Some(&reason));
    reason
}

#[tokio::test]
async fn recovery_attention_is_committed_once_per_transition_and_silent_for_stale_or_stopped_turns()
{
    let (_root, owner, control, accepted) =
        preparation_tests::fixture("http://127.0.0.1:9/v1").await;
    let mut snapshot = preparation_tests::ready(&control, &accepted).await;
    let mut events = control.subscribe();
    assert!(
        events.try_recv().is_err(),
        "existing state is never replayed as a notification"
    );
    let error = ApiError::new(ErrorCode::InvalidState, "synthetic recovery");
    recover(&control, &snapshot, error.clone(), None)
        .await
        .unwrap();
    assert!(
        matches!(events.try_recv(), Ok(super::super::EngineEvent::Changed { recovery: true, thread_id }) if thread_id == snapshot.thread)
    );
    let turn = snapshot.turn.clone();
    let state: String = owner
        .db
        .call(move |db| {
            Ok(db.query_row("SELECT state FROM turn WHERE id=?1", [turn], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(
        state, "recovery_wait",
        "the fact is emitted only after the commit"
    );
    recover(&control, &snapshot, error.clone(), None)
        .await
        .unwrap();
    assert!(matches!(
        events.try_recv(),
        Ok(super::super::EngineEvent::Changed {
            recovery: false,
            ..
        })
    ));

    // Same Turn can enter recovery again after an explicit resume. No per-Turn
    // cache may suppress its second committed transition.
    let turn = snapshot.turn.clone();
    owner
        .db
        .call(move |db| {
            db.execute(
                "UPDATE turn SET state='running',generation=generation+1 WHERE id=?1",
                [turn],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    recover(&control, &snapshot, error.clone(), None)
        .await
        .unwrap();
    assert!(
        matches!(
            events.try_recv(),
            Ok(super::super::EngineEvent::Changed {
                recovery: false,
                ..
            })
        ),
        "late old-generation failures must remain silent"
    );
    snapshot.generation += 1;
    recover(&control, &snapshot, error.clone(), None)
        .await
        .unwrap();
    assert!(matches!(
        events.try_recv(),
        Ok(super::super::EngineEvent::Changed { recovery: true, .. })
    ));
    let turn = snapshot.turn.clone();
    owner
        .db
        .call(move |db| {
            db.execute("UPDATE turn SET state='stopped' WHERE id=?1", [turn])?;
            Ok(())
        })
        .await
        .unwrap();
    recover(&control, &snapshot, error, None).await.unwrap();
    assert!(
        matches!(
            events.try_recv(),
            Ok(super::super::EngineEvent::Changed {
                recovery: false,
                ..
            })
        ),
        "a stopped Turn must not notify"
    );
    preparation_tests::close(control, owner).await;
}

#[tokio::test]
async fn preparation_recovery_also_publishes_only_the_new_transition() {
    let (_root, owner, control, accepted) =
        preparation_tests::fixture("http://127.0.0.1:9/v1").await;
    let mut events = control.subscribe();
    let error = ApiError::new(ErrorCode::InvalidArgument, "synthetic preparation failure");
    recover_preparation(
        &control,
        accepted.thread_id.clone(),
        accepted.run_id.clone().unwrap(),
        error.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        events.try_recv(),
        Ok(super::super::EngineEvent::Changed { recovery: true, .. })
    ));
    let (raw, count) = owner
        .db
        .call(|db| {
            let (turn, generation): (String, i64) =
                db.query_row("SELECT id,generation FROM turn", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            assert_reason_and_event(db, &turn, generation);
            Ok((
                db.query_row("SELECT recovery_reason_json FROM turn", [], |r| {
                    r.get::<_, String>(0)
                })?,
                db.query_row(
                    "SELECT count(*) FROM event WHERE kind='child_recovery_wait'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    recover_preparation(
        &control,
        accepted.thread_id,
        accepted.run_id.unwrap(),
        error,
    )
    .await
    .unwrap();
    assert!(
        events.try_recv().is_err(),
        "already waiting is not a new transition"
    );
    owner
        .db
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT recovery_reason_json FROM turn", [], |r| r
                    .get::<_, String>(0))?,
                raw
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM event WHERE kind='child_recovery_wait'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                count
            );
            Ok(())
        })
        .await
        .unwrap();
    preparation_tests::close(control, owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frontend_delivers_background_and_child_recovery_without_selecting_them() {
    use crate::ui::{UiAction, UiUpdate, actions};
    let (root, owner, control, accepted) =
        preparation_tests::fixture("http://127.0.0.1:9/v1").await;
    let snapshot = preparation_tests::ready(&control, &accepted).await;
    let (rpc, requests) = tokio::sync::mpsc::channel(16);
    let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(requests));
    let (sender, receiver) = actions::channel(16);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(crate::frontend::run(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    // Initialize confirms subscription. Leave the UI on its empty draft: the
    // recovering Thread has no frontend selection/generation or pending send.
    crate::test_watchdog::watchdog("src/control/driver/recovery_notification_tests.rs: frontend_delivers_background_and_child_recovery_without_selecting_them", async {
        loop {
            if matches!(incoming.try_recv(), Ok(UiUpdate::Initialize { .. })) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let error = ApiError::new(ErrorCode::InvalidState, "synthetic background failure");
    recover(&control, &snapshot, error.clone(), None)
        .await
        .unwrap();
    async fn expect_bell(incoming: &std::sync::mpsc::Receiver<UiUpdate>) {
        crate::test_watchdog::watchdog(
            "src/control/driver/recovery_notification_tests.rs: expect_bell",
            async {
                loop {
                    if matches!(incoming.try_recv(), Ok(UiUpdate::RecoveryBell)) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            },
        )
        .await
        .unwrap();
    }
    expect_bell(&incoming).await;
    let thread = snapshot.thread.clone();
    let run = snapshot.run.clone();
    let turn = snapshot.turn.clone();
    control.store.database.call(move |db| {
        // A real parent/child ownership relation exercises owner_event too.
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) SELECT 'bell-parent','parent',cwd,agents_path,profile_id,model_id,updated_at FROM thread WHERE id=?1", [&thread])?;
        db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('bell-parent-run','bell-parent','live','now')", [])?;
        db.execute("UPDATE thread SET parent_thread_id='bell-parent' WHERE id=?1", [&thread])?;
        db.execute("UPDATE run SET owner_run_id='bell-parent-run' WHERE id=?1", [&run])?;
        db.execute("UPDATE turn SET state='running',generation=generation+1 WHERE id=?1", [&turn])?;
        Ok(())
    }).await.unwrap();
    let mut child = snapshot;
    child.generation += 1;
    recover(&control, &child, error, None).await.unwrap();
    expect_bell(&incoming).await;
    let child_turn = child.turn.clone();
    let child_generation = child.generation;
    control
        .store
        .database
        .call(move |db| {
            assert_reason_and_event(db, &child_turn, child_generation);
            Ok(())
        })
        .await
        .unwrap();
    sender.try_send(UiAction::Quit).unwrap();
    crate::test_watchdog::watchdog(
        "src/control/driver/recovery_notification_tests.rs: expect_bell",
        async {
            while !bridge.is_finished() {
                while incoming.try_recv().is_ok() {}
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
    bridge.await.unwrap().unwrap();
    serving.await.unwrap();
}

#[tokio::test]
async fn recovery_timestamp_is_atomic_fenced_and_preserves_details() {
    let (_root, owner, control, accepted) =
        preparation_tests::fixture("http://127.0.0.1:9/v1").await;
    let snapshot = preparation_tests::ready(&control, &accepted).await;
    let turn = snapshot.turn.clone();
    let thread = snapshot.thread.clone();
    let run = snapshot.run.clone();
    owner
        .db
        .call(move |db| {
            for (generation, state, details) in [
                (10, "preparing", None),
                (
                    11,
                    "running",
                    Some(json!({"kept":42,"last_failure_at":"old"})),
                ),
                (12, "waiting", Some(json!(["preserved", 7]))),
                (13, "running", Some(json!(null))),
                (14, "running", Some(json!("original"))),
            ] {
                db.execute(
                    "UPDATE turn SET state=?2,generation=?3 WHERE id=?1",
                    params![turn, state, generation],
                )?;
                let mut error =
                    ApiError::new(ErrorCode::InvalidArgument, "fixture").hint("keep hint");
                error.details = details.clone();
                let original = error.clone();
                let before = chrono::Utc::now();
                let tx = db.transaction()?;
                assert!(recover_turn_tx(
                    &tx, &thread, &run, &turn, generation, &error
                )?);
                tx.commit()?;
                let after = chrono::Utc::now();
                assert_eq!(error, original, "the caller's error is unchanged");
                let reason = assert_reason_and_event(db, &turn, generation);
                let at = chrono::DateTime::parse_from_rfc3339(
                    reason["details"]["last_failure_at"].as_str().unwrap(),
                )
                .unwrap();
                assert!(at >= before && at <= after);
                assert_eq!(reason["message"], "fixture");
                assert_eq!(reason["hint"], "keep hint");
                match details {
                    Some(Value::Object(_)) => assert_eq!(reason["details"]["kept"], 42),
                    Some(value) => assert_eq!(reason["details"]["original_details"], value),
                    None => assert!(reason["details"].get("original_details").is_none()),
                }
            }
            let original: String = db.query_row(
                "SELECT recovery_reason_json FROM turn WHERE id=?1",
                [&turn],
                |r| r.get(0),
            )?;
            let count: i64 = db.query_row(
                "SELECT count(*) FROM event WHERE kind='child_recovery_wait'",
                [],
                |r| r.get(0),
            )?;
            let error = ApiError::new(ErrorCode::InvalidState, "late");
            for (state, generation) in [
                ("running", 99),
                ("stopped", 14),
                ("completed", 14),
                ("recovery_wait", 14),
            ] {
                db.execute("UPDATE turn SET state=?2 WHERE id=?1", params![turn, state])?;
                let tx = db.transaction()?;
                assert!(!recover_turn_tx(
                    &tx, &thread, &run, &turn, generation, &error
                )?);
                tx.commit()?;
                assert_eq!(
                    db.query_row(
                        "SELECT recovery_reason_json FROM turn WHERE id=?1",
                        [&turn],
                        |r| r.get::<_, String>(0)
                    )?,
                    original
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM event WHERE kind='child_recovery_wait'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    count
                );
            }
            db.execute(
                "UPDATE turn SET state='running',generation=15 WHERE id=?1",
                [&turn],
            )?;
            {
                let tx = db.transaction()?;
                assert!(recover_turn_tx(&tx, &thread, &run, &turn, 15, &error)?);
            }
            assert_eq!(
                db.query_row(
                    "SELECT recovery_reason_json FROM turn WHERE id=?1",
                    [&turn],
                    |r| r.get::<_, String>(0)
                )?,
                original
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM event WHERE kind='child_recovery_wait'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                count
            );
            Ok(())
        })
        .await
        .unwrap();
    preparation_tests::close(control, owner).await;
}
#[tokio::test]
async fn terminal_provider_failure_through_retry_persists_one_transition_time() {
    let (_root, owner, control, accepted) =
        preparation_tests::fixture("http://127.0.0.1:9/v1").await;
    let snapshot = preparation_tests::ready(&control, &accepted).await;
    let turn = snapshot.turn.clone();
    let generation = snapshot.generation;
    let selection = snapshot.selection;
    let profile = snapshot.profile.clone();
    let model = snapshot.model.clone();
    owner.db.call({let turn = turn.clone(); move |db| {
        db.execute("UPDATE turn SET state='running' WHERE id=?1", [&turn])?;
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('synthetic-attempt',?1,?2,?3,?4,?5,0,'inflight','{}','now')", params![turn,generation,selection,profile,model])?;
        Ok(())
    }}).await.unwrap();
    let slot = DriverSlot {
        id: id(),
        run_id: snapshot.run.clone(),
        stop: CancellationToken::new(),
        nudge: Arc::new(Notify::new()),
        foreground: Arc::new(ForegroundSlot::default()),
        turn: Arc::new(tokio::sync::Mutex::new(None)),
    };
    let mut failure: model::http::AttemptFailure =
        model::ModelError::new("provider", "synthetic permanent failure").into();
    failure.facts.status = Some(400);
    let mutex = tokio::sync::Mutex::new(());
    let returned = retry::retry_or_recover(
        &control,
        &snapshot,
        &slot,
        "synthetic-attempt".into(),
        failure,
        &mut retry::RetryBudget::default(),
        &mut OverflowBudget::default(),
        mutex.lock().await,
    )
    .await
    .unwrap_err();
    assert_eq!(returned.code, ErrorCode::IoFailed);
    assert_eq!(
        returned.details.as_ref().unwrap()["failure_kind"],
        "provider"
    );
    owner
        .db
        .call(move |db| {
            let reason = assert_reason_and_event(db, &turn, generation);
            assert_eq!(reason["details"]["failure_kind"], "provider");
            assert_eq!(reason["details"]["automatic_retries"], 0);
            assert_eq!(
                db.query_row(
                    "SELECT state FROM model_attempt WHERE id='synthetic-attempt'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "incomplete"
            );
            Ok(())
        })
        .await
        .unwrap();
    preparation_tests::close(control, owner).await;
}
#[tokio::test]
async fn capacity_failure_through_drive_persists_one_transition_time() {
    let (root, mut owner, old_control, accepted) =
        preparation_tests::fixture("http://127.0.0.1:9/v1").await;
    drop(old_control);
    // Mutate only this disposable owner's in-memory configuration after setup.
    owner.loaded.config.limits.data_max_bytes = 0;
    let control = Control::new(&owner, root.path().into());
    let slot = DriverSlot {
        id: id(),
        run_id: accepted.run_id.unwrap(),
        stop: CancellationToken::new(),
        nudge: Arc::new(Notify::new()),
        foreground: Arc::new(ForegroundSlot::default()),
        turn: Arc::new(tokio::sync::Mutex::new(None)),
    };
    let error = drive(control.clone(), accepted.thread_id, slot)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StorageLimit);
    owner
        .db
        .call(|db| {
            let (turn, generation): (String, i64) =
                db.query_row("SELECT id,generation FROM turn", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let reason = assert_reason_and_event(db, &turn, generation);
            assert_eq!(reason["details"]["limit_bytes"], 0);
            assert!(reason["details"]["logical_live_bytes"].is_u64());
            Ok(())
        })
        .await
        .unwrap();
    preparation_tests::close(control, owner).await;
}
