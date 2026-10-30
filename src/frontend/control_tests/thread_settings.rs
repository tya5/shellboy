use crate::{
    control::EngineEvent,
    frontend,
    protocol::{Command, ThreadSetArgs},
    ui::{UiAction, UiState, UiUpdate, actions},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_settings_notify_after_commit_and_refresh_metadata_without_resuming_recovery() {
    let (root, owner, control, server) = super::fixture().await;
    let cwd = root.path().join("next");
    std::fs::create_dir(&cwd).unwrap();
    let mut ui = UiState::new(&control.store.config, root.path().into());
    let (sender, receiver) = actions::channel(32);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
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
    ui.insert("keep the current draft", false);
    let draft_revision = ui.view().draft.revision;
    let mut events = control.subscribe();
    let settings = |title: Option<&str>, model: Option<&str>, cwd: Option<String>| ThreadSetArgs {
        agents: None,
        thread_id: Some("thread".into()),
        title: title.map(str::to_owned),
        connection: None,
        model: model.map(str::to_owned),
        cwd,
        effort: None,
    };
    // Exercise both the title-only fast path and full settings path through
    // external requests, never the frontend's local ThreadSet success shortcut.
    for args in [
        settings(Some("renamed"), None, None),
        settings(None, Some("next-model"), Some(cwd.to_string_lossy().into())),
    ] {
        let request = frontend::operation(&control, Command::ThreadSet(args.clone()));
        let response = control.request(request.clone()).await;
        assert!(response.ok, "{response:?}");
        assert!(
            matches!(events.try_recv(), Ok(EngineEvent::Changed {thread_id, recovery: false}) if thread_id == "thread")
        );
        let details = response.result.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                while let Ok(update) = incoming.try_recv() {
                    let found = matches!(&update, UiUpdate::Thread {details: observed,..}
                        if observed.title == details["title"].as_str().unwrap()
                        && observed.model == details["model"].as_str().unwrap()
                        && observed.cwd == details["cwd"].as_str().unwrap());
                    ui.apply(update);
                    for action in ui.take_deferred_actions() {
                        sender.try_send(action).unwrap();
                    }
                    if found {
                        return;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(ui.view().draft.text, "keep the current draft");
        assert_eq!(ui.view().draft.revision, draft_revision);
        // A saved receipt replay has no new commit/effect, even after refresh.
        assert!(control.request(request).await.ok);
        assert!(
            events.try_recv().is_err(),
            "receipt replay must not re-publish effects"
        );
    }
    // Existing no-op revision behavior remains a commit and notification.
    assert!(
        control
            .request(frontend::operation(
                &control,
                Command::ThreadSet(settings(Some("renamed"), None, None))
            ))
            .await
            .ok
    );
    assert!(matches!(events.try_recv(), Ok(EngineEvent::Changed { .. })));
    // Domain rejection cannot publish settings or Changed.
    let mut invalid = settings(None, None, None);
    invalid.connection = Some("missing".into());
    assert!(
        !control
            .request(frontend::operation(&control, Command::ThreadSet(invalid)))
            .await
            .ok
    );
    assert!(events.try_recv().is_err());
    control.store.database.call(|db| {
        assert_eq!(db.query_row("SELECT revision FROM thread WHERE id='thread'", [], |r|r.get::<_,i64>(0))?, 3, "same-value settings retain the existing revision increment; replay/rejection do not");
        assert_eq!(db.query_row("SELECT count(*) FROM run", [], |r|r.get::<_,i64>(0))?, 2);
        assert_eq!(db.query_row("SELECT count(*) FROM turn", [], |r|r.get::<_,i64>(0))?, 2);
        assert_eq!(db.query_row("SELECT state FROM turn WHERE id='target-run'", [], |r|r.get::<_,String>(0))?, "recovery_wait");
        assert_eq!(db.query_row("SELECT state FROM turn WHERE id='independent-run'", [], |r|r.get::<_,String>(0))?, "running");
        assert_eq!(db.query_row("SELECT count(*) FROM model_attempt", [], |r|r.get::<_,i64>(0))?, 0);
        db.execute_batch("CREATE TRIGGER reject_settings BEFORE UPDATE ON thread BEGIN SELECT RAISE(ABORT,'fixture settings failure'); END;")?;
        Ok(())
    }).await.unwrap();
    // Real SQLite failure rolls back and cannot return a notification effect.
    // Call the Store boundary directly: Control's fatal-save policy is tested
    // elsewhere and deliberately stops runs, unlike a settings notification.
    let failed = control
        .store
        .execute(
            frontend::operation(
                &control,
                Command::ThreadSet(settings(Some("must not persist"), None, None)),
            ),
            None,
        )
        .await;
    assert_eq!(
        failed.unwrap_err().code,
        crate::protocol::ErrorCode::SaveFailed
    );
    assert!(events.try_recv().is_err());
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT title FROM thread WHERE id='thread'", [], |r| r
                    .get::<_, String>(
                    0
                ))?,
                "renamed"
            );
            assert_eq!(
                db.query_row("SELECT revision FROM thread WHERE id='thread'", [], |r| r
                    .get::<_, i64>(
                    0
                ))?,
                3,
                "failed settings must roll back the revision too"
            );
            db.execute_batch("DROP TRIGGER reject_settings")?;
            Ok(())
        })
        .await
        .unwrap();
    sender.try_send(UiAction::Quit).unwrap();
    super::finish_bridge(bridge, &incoming).await;
}
