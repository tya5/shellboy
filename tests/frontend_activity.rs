use shellboy::{
    frontend::activity::{self, Projection},
    storage::DbWorker,
    ui::activity::ActivityPhase,
};

async fn projection(db: &DbWorker) -> Projection {
    db.call(|db| activity::project(db, "t")).await.unwrap()
}

#[tokio::test]
async fn automatic_reduction_summary_survives_later_failures_without_crossing_generation() {
    use serde_json::{Value, json};
    use shellboy::{
        protocol::{ApiError, ErrorCode},
        ui::{UiState, UiUpdate},
    };
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    change(&db,"INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES('u','t','r','recovery_wait',2,'now');").await;
    let original=json!({
        "raw_response":{"opaque":"x".repeat(1024*1024)},
        "automatic_trim":{"before_characters":90000,"after_characters":40000,"omitted_nodes":7,"compact":{"spilled_groups":3,"no_target":false}},
    }).to_string();
    let saved = original.clone();
    let reason = serde_json::to_string(&ApiError::new(
        ErrorCode::IoFailed,
        "connection failed again",
    ))
    .unwrap();
    db.call(move|db| {
        db.execute("UPDATE turn SET recovery_reason_json=?1 WHERE id='u'",[reason])?;
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('trimmed','u',2,0,'p','m',0,'failed','{}',?1,'now')",[saved])?;
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('latest','u',2,0,'p','m',0,'incomplete','{}','{}','now')",[])?;
        Ok(())
    }).await.unwrap();
    let observed = projection(&db).await;
    let summary = observed.snapshot.context_reduction.clone().unwrap();
    assert!(summary.contains("spill 3組"));
    assert!(summary.contains("省略 7件"));
    assert!(summary.contains("90000→40000"));
    assert!(
        summary.len() < 256,
        "only bounded numeric facts are projected"
    );
    assert_eq!(
        observed.snapshot.recovery.as_ref().unwrap().message,
        "connection failed again"
    );
    let mut ui = UiState::new(&shellboy::config::Config::default(), root.path().into());
    ui.select_thread("t".into());
    ui.apply(UiUpdate::Activity {
        thread_id: "t".into(),
        generation: ui.view().generation,
        epoch: 1,
        snapshot: observed.snapshot,
        observed_at: std::time::Instant::now(),
    });
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| shellboy::ui::draw(frame, &mut ui))
        .unwrap();
    use unicode_width::UnicodeWidthStr;
    let buffer = terminal.backend().buffer();
    let mut shown = String::new();
    for y in 0..30 {
        let mut x = 0;
        while x < 140 {
            let symbol = buffer[(x, y)].symbol();
            shown.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        shown.push('\n');
    }
    assert!(shown.contains(&summary), "{shown}");
    // The current attempt has no automatic_trim; earlier evidence remains in
    // the activity summary while the next retry wait is running too.
    change(&db,"UPDATE turn SET state='running',recovery_reason_json=NULL; UPDATE model_attempt SET state='failed',error_json='{\"automatic_retry\":{\"number\":1,\"limit\":8,\"delay_ms\":1000,\"partial_response_discarded\":false}}' WHERE id='latest';").await;
    let retry = projection(&db).await;
    assert_eq!(retry.snapshot.phase, ActivityPhase::Retrying);
    assert_eq!(
        retry.snapshot.context_reduction.as_deref(),
        Some(summary.as_str())
    );
    change(&db, "UPDATE turn SET generation=3;").await;
    assert!(projection(&db).await.snapshot.context_reduction.is_none());
    let retained = db
        .call(|db| {
            Ok(db.query_row(
                "SELECT error_json FROM model_attempt WHERE id='trimmed'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        retained, original,
        "display projection never edits paid source evidence"
    );
    change(&db, "UPDATE turn SET generation=2;").await;
    let base: Value = serde_json::from_str(&original).unwrap();
    for invalid in [
        json!(-1),
        json!(true),
        json!(1.5),
        json!("x".repeat(128 * 1024)),
        json!(u64::MAX),
    ] {
        let mut body = base.clone();
        body["automatic_trim"]["omitted_nodes"] = invalid;
        let body = body.to_string();
        db.call(move |db| {
            db.execute(
                "UPDATE model_attempt SET error_json=?1 WHERE id='trimmed'",
                [body],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(
            projection(&db).await.snapshot.context_reduction.is_none(),
            "invalid counts must not become confident display facts"
        );
    }
    db.shutdown().await.unwrap();
}
async fn change(db: &DbWorker, sql: &str) {
    let sql = sql.to_owned();
    db.call(move |db| {
        db.execute_batch(&sql)?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn activity_uses_recorded_run_turn_attempt_priority_and_remains_read_only() {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    change(&db,"INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now');").await;
    assert_eq!(projection(&db).await.snapshot.phase, ActivityPhase::Idle);
    change(&db,"INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2026-10-25T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','preparing','2026-10-25T00:00:00Z');").await;
    let prepared = projection(&db).await;
    assert_eq!(prepared.snapshot.phase, ActivityPhase::Preparing);
    assert_eq!(prepared.snapshot.run_id.as_deref(), Some("r"));
    assert_eq!(prepared.snapshot.turn_id.as_deref(), Some("u"));
    assert!(prepared.poll_running);
    change(&db,"UPDATE turn SET state='running'; INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'inflight','{}','2026-10-25T00:00:00Z'); INSERT INTO tool_group VALUES('g','a','u','running','2026-10-25T00:00:00Z');").await;
    let waiting = projection(&db).await;
    assert_eq!(
        waiting.snapshot.phase,
        ActivityPhase::AwaitingModel,
        "inflight attempt wins over a background tool group"
    );
    assert_eq!(waiting.snapshot.attempt_id.as_deref(), Some("a"));
    change(
        &db,
        "UPDATE model_attempt SET state='completed' WHERE id='a';",
    )
    .await;
    assert_eq!(projection(&db).await.snapshot.phase, ActivityPhase::Tools);
    change(&db, "UPDATE tool_group SET state='completed';").await;
    assert_eq!(projection(&db).await.snapshot.phase, ActivityPhase::Running);
    change(&db, "UPDATE turn SET state='waiting';").await;
    assert_eq!(projection(&db).await.snapshot.phase, ActivityPhase::Waiting);
    change(
        &db,
        "UPDATE turn SET state='recovery_wait'; UPDATE model_attempt SET state='inflight';",
    )
    .await;
    assert_eq!(
        projection(&db).await.snapshot.phase,
        ActivityPhase::Recovery,
        "recovery must not show model activity"
    );
    change(&db, "UPDATE run SET state='stopping';").await;
    assert_eq!(
        projection(&db).await.snapshot.phase,
        ActivityPhase::Stopping
    );
    change(&db, "UPDATE run SET state='stopped';").await;
    assert_eq!(projection(&db).await.snapshot.phase, ActivityPhase::Stopped);
    assert!(!projection(&db).await.poll_running);
    change(&db, "UPDATE run SET state='failed';").await;
    assert_eq!(projection(&db).await.snapshot.phase, ActivityPhase::Failed);
    for (saved, expected) in [
        ("failed", ActivityPhase::Failed),
        ("stopped", ActivityPhase::Stopped),
    ] {
        change(
            &db,
            &format!("UPDATE run SET state='live'; UPDATE turn SET state='{saved}';"),
        )
        .await;
        let saved_terminal_turn = projection(&db).await;
        assert_eq!(saved_terminal_turn.snapshot.phase, expected);
        assert!(!saved_terminal_turn.snapshot.turn_in_progress);
        assert!(!saved_terminal_turn.poll_running);
    }
    change(&db,"UPDATE run SET state='live'; UPDATE turn SET state='completed',ended_at='2026-10-25T00:00:02Z'; UPDATE model_attempt SET state='completed'; INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('s','r','u','s','running','/','{\"spawned\":true}','UTF-8','UTF-8','closed','now'),('not-spawned','r','u','not-spawned','failed','/','{}','UTF-8','UTF-8','closed','now'); INSERT INTO tool_call VALUES('g','cancelled',0,'shell','{}','cancelled',NULL); INSERT INTO node(id,kind,created_at,payload_json)VALUES('cancel-result','tool_result','now','{}'); INSERT INTO tool_result VALUES('g','cancelled','cancel-result','cancelled');").await;
    let background = projection(&db).await;
    assert_eq!(background.snapshot.phase, ActivityPhase::Background);
    assert!(!background.snapshot.turn_in_progress);
    assert_eq!(background.tool_calls, 1);
    assert_eq!(background.cancelled_calls, 1);
    change(&db, "UPDATE shell_execution SET state='exited';").await;
    let continuing = projection(&db).await;
    assert_eq!(continuing.snapshot.phase, ActivityPhase::Continuing);
    assert!(
        !continuing.poll_running,
        "local pulse must not add periodic database targets"
    );
    change(&db,"INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('child','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES('child-run','child','r','live','now');").await;
    assert_eq!(
        projection(&db).await.snapshot.phase,
        ActivityPhase::Background,
        "owned child Run is recorded background work"
    );
    change(&db,"UPDATE run SET state='completed' WHERE id='r'; INSERT INTO run(id,thread_id,state,created_at)VALUES('new-run','t','stopped','now');").await;
    let latest = projection(&db).await;
    assert_eq!(latest.snapshot.run_id.as_deref(), Some("new-run"));
    assert_eq!(latest.snapshot.turn_id, None);
    assert_eq!(latest.snapshot.attempt_id, None);
    assert_eq!(
        latest.snapshot.phase,
        ActivityPhase::Stopped,
        "old Turn/attempt must not animate the newer Run"
    );
    db.call(|db| {
        let before = db.total_changes();
        let _ = activity::project(db, "t")?;
        assert_eq!(db.total_changes(), before);
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn activity_delivery_rejects_a_previous_thread_after_a_slow_database_read() {
    use shellboy::{
        app::DataOwner,
        control::Control,
        frontend,
        ipc::LocalServer,
        ui::{UiAction, UiUpdate},
    };
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    owner.db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('a','/','/AGENTS.md','p','m','now'),('b','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('ra','a','live','2026-10-25T00:00:00Z'),('rb','b','completed','2026-10-25T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('ua','a','ra','preparing','2026-10-25T00:00:00Z'),('ub','b','rb','completed','2026-10-25T00:00:00Z');")?;Ok(())}).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (rpc, receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc).await.unwrap();
    tokio::spawn(control.clone().serve(receive));
    let db = owner.db.clone();
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    send.send(UiAction::SelectThread {
        preview_sources: vec![],
        thread_id: "a".into(),
        generation: 1,
        view_revision: 0,
        known_view: None,
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(UiUpdate::Activity {
                thread_id,
                snapshot,
                ..
            }) = incoming.try_recv()
            {
                assert_eq!(thread_id, "a");
                assert_eq!(snapshot.phase, ActivityPhase::Preparing);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let blocked = tokio::spawn(async move {
        db.call(move |_| {
            let _ = entered.send(());
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
        .await
    });
    entered_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(550)).await; // existing 500ms progress request is now blocked
    while incoming.try_recv().is_ok() {}
    send.send(UiAction::SelectThread {
        preview_sources: vec![],
        thread_id: "b".into(),
        generation: 2,
        view_revision: 0,
        known_view: None,
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    release.send(()).unwrap();
    blocked.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match incoming.try_recv() {
                Ok(UiUpdate::Activity {
                    thread_id,
                    generation,
                    snapshot,
                    ..
                }) => {
                    assert_eq!(
                        thread_id, "b",
                        "late activity escaped the frontend generation gate"
                    );
                    assert_eq!(generation, 2);
                    assert_eq!(snapshot.phase, ActivityPhase::Completed);
                    break;
                }
                Ok(UiUpdate::Progress { thread_id, .. }) => assert_ne!(
                    thread_id, "a",
                    "legacy progress must use the same delivery gate"
                ),
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn wall_clock_changes_do_not_change_ui_observation_time_or_saved_shell_measurement() {
    use ratatui::{Terminal, backend::TestBackend};
    use shellboy::{
        config::Config,
        history::text_of_payload,
        presentation,
        ui::{self, UiNode, UiState, UiUpdate},
    };
    use std::time::{Duration, Instant};
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    change(&db,"INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2000-01-01T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','2000-01-01T00:00:00Z'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,exit_code,output_complete)VALUES('s','r','u','s','exited','/','{\"spawned\":true,\"completion\":{\"elapsed_ms\":1240}}','UTF-8','UTF-8','closed','2000-01-01T00:00:00Z','2100-01-01T00:00:00Z',0,1);").await;
    db.call(|db| {
        let elapsed:i64=db.query_row("SELECT json_extract(shell_json,'$.completion.elapsed_ms') FROM shell_execution WHERE id='s'",[],|r|r.get(0))?;
        let payload=serde_json::json!({"item":{"type":"function_call_output","call_id":"saved","output":serde_json::json!({"status":"exited","exit_code":0,"elapsed_ms":elapsed,"output":"measured output"}).to_string()}});
        db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('saved-tool','tool_result','now',?1)",[payload.to_string()])?;
        Ok(())
    }).await.unwrap();
    let mut state = UiState::new(&Config::default(), root.path().into());
    state.select_thread("t".into());
    let generation = state.view().generation;
    let at = Instant::now();
    let apply = |state: &mut UiState, projection: Projection, observed_at| {
        state.apply(projection.progress_update("t"));
        state.apply(UiUpdate::Activity {
            thread_id: "t".into(),
            generation,
            epoch: 1,
            snapshot: projection.snapshot,
            observed_at,
        });
    };
    apply(&mut state, projection(&db).await, at);
    let node = db
        .call(|db| {
            let body: String = db.query_row(
                "SELECT payload_json FROM node WHERE id='saved-tool'",
                [],
                |r| r.get(0),
            )?;
            let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
            let text = text_of_payload(&payload);
            Ok(UiNode {
                node_id: "saved-tool".into(),
                role: "tool".into(),
                text: text.clone(),
                base_char_offset: 0,
                total_chars: text.chars().count(),
                reasoning_ranges: vec![],
                presentation: presentation::project("tool_result", &payload, &text, 0),
            })
        })
        .await
        .unwrap();
    state.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "t".into(),
        generation,
        nodes: vec![node],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
        total_nodes: 1,
    });
    change(&db,"UPDATE turn SET created_at='2100-01-01T00:00:00Z',ended_at=NULL; UPDATE shell_execution SET started_at='2100-01-01T00:00:00Z',ended_at='1900-01-01T00:00:00Z';").await;
    apply(
        &mut state,
        projection(&db).await,
        at + Duration::from_secs(133),
    );
    fn screen(state: &mut UiState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, state)).unwrap();
        use unicode_width::UnicodeWidthStr;
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in buffer.area.y..buffer.area.bottom() {
            let mut x = buffer.area.x;
            while x < buffer.area.right() {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            text.push('\n');
        }
        text
    }
    let text = screen(&mut state);
    assert!(text.contains("133s"), "{text}");
    assert!(
        text.contains("1.2秒"),
        "S6 saved shell measurement stays independent: {text}"
    );
    change(&db,"UPDATE turn SET state='completed',ended_at='1900-01-01T00:00:00Z'; INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('background','r','u','background','running','/','{\"spawned\":true}','UTF-8','UTF-8','closed','1900-01-01T00:00:00Z');").await;
    let background = projection(&db).await;
    assert_eq!(background.snapshot.phase, ActivityPhase::Background);
    assert!(!background.snapshot.turn_in_progress);
    apply(&mut state, background, at + Duration::from_secs(133));
    assert!(
        !screen(&mut state).contains("133s"),
        "a stopped Turn clock must not label background-work age"
    );
    // No locally observed active Turn means no invented duration after selection.
    state.select_thread("t".into());
    let new_generation = state.view().generation;
    let background = projection(&db).await;
    state.apply(UiUpdate::Activity {
        thread_id: "t".into(),
        generation: new_generation,
        epoch: 2,
        snapshot: background.snapshot,
        observed_at: at + Duration::from_secs(9000),
    });
    assert!(!screen(&mut state).contains("133s"));
    db.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_activity_read_delivers_observation_loss_through_the_real_frontend() {
    use shellboy::{
        app::DataOwner,
        control::Control,
        frontend,
        ipc::LocalServer,
        ui::{UiAction, UiUpdate},
    };
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    owner.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('a','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','a','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','a','r','preparing','now');")?;
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (rpc, receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc).await.unwrap();
    tokio::spawn(control.clone().serve(receive));
    let db = owner.db.clone();
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    send.send(UiAction::SelectThread {
        preview_sources: vec![],
        thread_id: "a".into(),
        generation: 1,
        view_revision: 0,
        known_view: None,
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(UiUpdate::Activity { snapshot, .. }) = incoming.try_recv() {
                assert!(snapshot.turn_in_progress);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Temporarily hide one required table in this disposable DB. The real
    // activity worker must return its read failure, not retain a live clock.
    db.call(|db| {
        db.execute_batch("ALTER TABLE model_attempt RENAME TO hidden_attempt")?;
        Ok(())
    })
    .await
    .unwrap();
    let lost = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(UiUpdate::ActivityUnavailable {
                thread_id,
                generation,
                ..
            }) = incoming.try_recv()
            {
                break (thread_id, generation);
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    db.call(|db| {
        db.execute_batch("ALTER TABLE hidden_attempt RENAME TO model_attempt")?;
        Ok(())
    })
    .await
    .unwrap();
    send.send(UiAction::Quit).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), bridge)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lost.unwrap(), ("a".into(), 1));
    assert!(ended.unwrap_err().is_storage_failure());
}

#[tokio::test]
async fn human_model_failure_message_keeps_saved_error_and_does_not_guess_prefixes() {
    use shellboy::{
        model::ModelError,
        protocol::{ApiError, ErrorCode},
    };
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    let model = ModelError::new("provider", "上流から応答を取得できませんでした");
    let error = ApiError::new(ErrorCode::InvalidState, model.to_string());
    let saved = serde_json::to_string(&error).unwrap();
    let saved_model = serde_json::to_string(&serde_json::json!({"error": &model})).unwrap();
    let (receipt, attempt) = (saved.clone(), saved_model.clone());
    db.call(move |db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','recovery_wait','now');")?;
        db.execute("UPDATE turn SET recovery_reason_json=?1",[receipt])?;
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('a','u',0,0,'p','m',0,'incomplete','{}',?1,'now')",[attempt])?;
        Ok(())
    }).await.unwrap();
    let recovery = projection(&db).await.snapshot.recovery.unwrap();
    assert_eq!(recovery.stage, "モデル要求");
    assert_eq!(recovery.message, model.message);
    let mut ui =
        shellboy::ui::UiState::new(&shellboy::config::Config::default(), root.path().into());
    ui.select_thread("t".into());
    let snapshot = projection(&db).await.snapshot;
    ui.apply(shellboy::ui::UiUpdate::Activity {
        thread_id: "t".into(),
        generation: ui.view().generation,
        epoch: 1,
        observed_at: std::time::Instant::now(),
        snapshot,
    });
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| shellboy::ui::draw(frame, &mut ui))
        .unwrap();
    let shown = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    for meaning in ["上流から応答", "依頼は保存済み", "/retry"] {
        // Wide cell continuation blanks are absent in actual terminal text.
        assert!(
            shown.replace(' ', "").contains(&meaning.replace(' ', "")),
            "{meaning}: {shown}"
        );
    }
    assert!(!shown.contains("provider:"));
    let original = db
        .call(|db| {
            Ok((
                db.query_row(
                    "SELECT recovery_reason_json FROM turn WHERE id='u'",
                    [],
                    |r| r.get::<_, String>(0),
                )?,
                db.query_row(
                    "SELECT error_json FROM model_attempt WHERE id='a'",
                    [],
                    |r| r.get::<_, String>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(original, (saved, saved_model));
    let literal = ApiError::new(
        ErrorCode::InvalidState,
        "provider: unrelated literal reason",
    );
    let literal = serde_json::to_string(&literal).unwrap();
    db.call(move |db| {
        db.execute("UPDATE turn SET recovery_reason_json=?1", [literal])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        projection(&db).await.snapshot.recovery.unwrap().message,
        "provider: unrelated literal reason"
    );
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn retry_projection_requires_current_failed_attempt_and_live_running_turn() {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    change(&db, r#"
        INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/AGENTS.md','p','m','now');
        INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','live','now');
        INSERT INTO turn(id,thread_id,run_id,state,generation,created_at) VALUES('u','t','r','running',2,'now');
        INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at,error_json)
        VALUES('a','u',2,0,'p','m',0,'failed','{}','now','{"automatic_retry":{"number":1,"limit":2,"delay_ms":1000,"partial_response_discarded":false,"possible_duplicate_charge":true}}');
    "#).await;
    let retry = projection(&db).await;
    assert_eq!(retry.snapshot.phase, ActivityPhase::Retrying);
    let counts = retry.snapshot.automatic_retry.unwrap();
    assert_eq!((counts.number, counts.limit), (1, 2));
    assert_eq!(counts.delay_ms, 1000);
    assert!(!counts.partial_response_discarded);
    assert!(retry.snapshot.turn_in_progress);
    assert!(retry.poll_running);
    // Old-generation metadata cannot revive a cancelled/retried Turn.
    change(&db, "UPDATE model_attempt SET generation=1;").await;
    assert_eq!(projection(&db).await.snapshot.phase, ActivityPhase::Running);
    assert!(projection(&db).await.snapshot.automatic_retry.is_none());
    change(&db, "UPDATE model_attempt SET generation=2;").await;
    for (run, turn, expected) in [
        ("stopping", "running", ActivityPhase::Stopping),
        ("live", "recovery_wait", ActivityPhase::Recovery),
        ("completed", "completed", ActivityPhase::Completed),
        ("failed", "failed", ActivityPhase::Failed),
        ("stopped", "stopped", ActivityPhase::Stopped),
    ] {
        change(
            &db,
            &format!("UPDATE run SET state='{run}'; UPDATE turn SET state='{turn}';"),
        )
        .await;
        let saved = projection(&db).await.snapshot;
        assert_eq!(saved.phase, expected);
        assert!(saved.automatic_retry.is_none());
    }
    change(
        &db,
        "UPDATE run SET state='live'; UPDATE turn SET state='running';",
    )
    .await;
    // No metadata, malformed metadata, and an impossible count remain ordinary activity.
    for body in [
        "{}",
        "not-json",
        r#"{"automatic_retry":{"number":0,"limit":2,"delay_ms":1000,"partial_response_discarded":false}}"#,
        r#"{"automatic_retry":{"number":3,"limit":2,"delay_ms":1000,"partial_response_discarded":false}}"#,
        r#"{"automatic_retry":{"number":1}}"#,
        r#"{"automatic_retry":{"number":1,"limit":2,"partial_response_discarded":false}}"#,
        r#"{"automatic_retry":{"number":1,"limit":2,"delay_ms":1000}}"#,
    ] {
        let body = body.to_owned();
        db.call(move |db| {
            db.execute("UPDATE model_attempt SET error_json=?1", [body])?;
            Ok(())
        })
        .await
        .unwrap();
        let saved = projection(&db).await.snapshot;
        assert_eq!(saved.phase, ActivityPhase::Running);
        assert!(saved.automatic_retry.is_none());
    }
    change(&db, r#"UPDATE model_attempt SET error_json='{"automatic_retry":{"number":2,"limit":2,"delay_ms":1000,"partial_response_discarded":false}}';
        INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)
        VALUES('b','u',2,0,'p','m',0,'inflight','{}','now');"#).await;
    let next = projection(&db).await.snapshot;
    assert_eq!(next.phase, ActivityPhase::AwaitingModel);
    assert_eq!(next.attempt_id.as_deref(), Some("b"));
    assert!(next.automatic_retry.is_none());
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn exhausted_retry_recovery_stage_requires_exact_saved_model_and_counts() {
    use serde_json::json;
    use shellboy::{
        model::ModelError,
        protocol::{ApiError, ErrorCode},
    };
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    change(&db, "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES('u','t','r','recovery_wait',2,'now'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',2,0,'p','m',0,'incomplete','{}','now');").await;
    let model = ModelError::new("provider", "rate limited; original provider message");
    let suffix = " (automatic retries exhausted: 2/2)";
    let message = format!("{model}{suffix}");
    // Only the complete typed diagnostic, with current-generation incomplete
    // ownership, is proof that the recovery failure came from this model.
    for (metadata, reason, generation, state, known) in [
        (
            Some(json!({"retries":2,"limit":2})),
            message.clone(),
            2,
            "incomplete",
            true,
        ),
        (None, message.clone(), 2, "incomplete", false),
        (
            Some(json!({"retries":1,"limit":2})),
            message.clone(),
            2,
            "incomplete",
            false,
        ),
        (
            Some(json!({"retries":2,"limit":1})),
            message.clone(),
            2,
            "incomplete",
            false,
        ),
        (
            Some(json!({"retries":0,"limit":0})),
            message.clone(),
            2,
            "incomplete",
            false,
        ),
        (
            Some(json!({"retries":6,"limit":6})),
            message.clone(),
            2,
            "incomplete",
            false,
        ),
        (
            Some(json!({"retries":"2","limit":2})),
            message.clone(),
            2,
            "incomplete",
            false,
        ),
        (Some(json!(2)), message.clone(), 2, "incomplete", false),
        (
            Some(json!({"retries":2,"limit":2})),
            format!("provider: forged reason{suffix}"),
            2,
            "incomplete",
            false,
        ),
        (
            Some(json!({"retries":2,"limit":2})),
            message.clone(),
            1,
            "incomplete",
            false,
        ),
        (
            Some(json!({"retries":2,"limit":2})),
            message.clone(),
            2,
            "failed",
            false,
        ),
    ] {
        let mut saved = json!({"error": &model});
        if let Some(metadata) = metadata {
            saved["automatic_retry_exhausted"] = metadata;
        }
        let saved = saved.to_string();
        let recovery = serde_json::to_string(&ApiError::new(ErrorCode::IoFailed, &reason)).unwrap();
        let original = (recovery.clone(), saved.clone());
        db.call(move |db| {
            db.execute("UPDATE turn SET recovery_reason_json=?1", [recovery])?;
            db.execute(
                "UPDATE model_attempt SET error_json=?1,generation=?2,state=?3",
                rusqlite::params![saved, generation, state],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let recovery = projection(&db).await.snapshot.recovery.unwrap();
        if known {
            assert_eq!(recovery.stage, "モデル要求");
            assert_eq!(recovery.message, format!("{}{suffix}", model.message));
        } else {
            assert_eq!(recovery.stage, "処理（段階の記録なし）");
            assert_eq!(recovery.message, reason);
        }
        let unchanged = db
            .call(|db| {
                Ok((
                    db.query_row(
                        "SELECT recovery_reason_json FROM turn WHERE id='u'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?,
                    db.query_row(
                        "SELECT error_json FROM model_attempt WHERE id='a'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(
            unchanged, original,
            "projection must not rewrite provider diagnostics or counts"
        );
    }
    db.shutdown().await.unwrap();
}
