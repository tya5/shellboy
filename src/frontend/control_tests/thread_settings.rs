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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsaved_agents_round_trip_adopts_origin_draft_and_next_send_uses_same_thread() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    for switch_away in [false, true] {
        let (root, owner, control, server) = super::fixture().await;
        std::fs::write(root.path().join("role.md"), "SELECTED_DRAFT_ROLE").unwrap();
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
        // Process startup before issuing a draft-bound selection.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(update) = incoming.try_recv() {
                    let initialized = matches!(update, UiUpdate::Initialize { .. });
                    ui.apply(update);
                    if initialized {
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let origin = ui.view().draft.draft_id.clone();
        sender
            .try_send(UiAction::ActivateDraft {
                draft_id: origin.clone(),
            })
            .unwrap();
        ui.insert("/agents role.md", false);
        for action in ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))) {
            sender.try_send(action).unwrap();
        }
        ui.insert("kept while reference is selected", false);
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )))
            .is_empty(),
            "normal send cannot race unsaved Thread creation"
        );
        let selected = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(update) = incoming.try_recv() {
                    if matches!(update, UiUpdate::AgentsSelected { .. }) {
                        break update;
                    }
                    ui.apply(update);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let thread = match &selected {
            UiUpdate::AgentsSelected {
                draft_id,
                result: Ok(details),
                ..
            } => {
                assert_eq!(draft_id, &origin);
                details.thread_id.clone()
            }
            other => panic!("{other:?}"),
        };
        if switch_away {
            for action in ui.new_chat() {
                sender.try_send(action).unwrap();
            }
            ui.insert("unrelated draft", false);
        }
        ui.apply(selected);
        for action in ui.take_deferred_actions() {
            sender.try_send(action).unwrap();
        }
        if switch_away {
            assert!(ui.view().thread_id.is_none());
            assert_eq!(ui.view().draft.text, "unrelated draft");
            for action in ui.select_thread(format!("draft:{origin}")) {
                sender.try_send(action).unwrap();
            }
        }
        assert_eq!(ui.view().thread_id.as_deref(), Some(thread.as_str()));
        assert_eq!(
            ui.view().agents_path,
            root.path().join("role.md").to_str().unwrap()
        );
        assert_eq!(ui.view().draft.text, "kept while reference is selected");
        let send = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(
            send.iter()
                .any(|a| matches!(a,UiAction::Send{thread_id:Some(id),..} if id==&thread))
        );
        for action in send {
            sender.try_send(action).unwrap();
        }
        let target = thread.clone();
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let target=target.clone();
                if control.store.database.call(move|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 AND n.payload_json LIKE '%kept while reference%')",[target],|r|r.get::<_,bool>(0))?)).await.unwrap(){break;}
                while let Ok(update)=incoming.try_recv(){ui.apply(update);}
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let count: i64 = control
            .store
            .database
            .call(|db| Ok(db.query_row("SELECT count(*) FROM thread", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(
            count, 3,
            "first send must reuse the selected Thread, not create a default one"
        );
        sender.try_send(UiAction::Quit).unwrap();
        super::finish_bridge(bridge, &incoming).await;
    }
}
