use super::*;
use std::time::Duration;

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
    tokio::time::timeout(Duration::from_secs(10), async {
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
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(incoming.try_recv(), Ok(UiUpdate::RecoveryBell)) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
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
    sender.try_send(UiAction::Quit).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !bridge.is_finished() {
            while incoming.try_recv().is_ok() {}
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    bridge.await.unwrap().unwrap();
    serving.await.unwrap();
}
