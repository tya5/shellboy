//! Native pipe -> committed output -> exact in-process marker -> Notify ordering.
use super::*;
use crate::{app::DataOwner, control::LiveShell, shell::ShellSupervisor};
use futures_util::FutureExt;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

const WATCHDOG: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_output_commit_marks_only_its_registered_execution_before_notification() {
    marker_case(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rolled_back_output_batch_has_no_marker_or_normal_output_notification() {
    marker_case(true).await;
}

async fn marker_case(rollback: bool) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "[io]\noutput_batch_ms=1\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(move |db| {
        let tx=db.transaction()?;
        for index in 0..3 {
            let thread=format!("t{index}");let run=format!("r{index}");let turn=format!("u{index}");let execution=format!("s{index}");
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','','p','m','now')",[&thread])?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,'live','now')",params![run,thread])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'running','now')",params![turn,thread,run])?;
            tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,?3,?1,'running','/','{}','UTF-8','UTF-8','open','now')",params![execution,run,turn])?;
        }
        if rollback {
            tx.execute_batch("CREATE TRIGGER reject_marker_output BEFORE INSERT ON output_chunk WHEN NEW.execution_id='s0' BEGIN SELECT RAISE(ABORT,'synthetic output rollback'); END;")?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    let hidden_marker = Instant::now();
    let supervisor = ShellSupervisor::new(&owner.loaded.config);
    let mut handles = Vec::new();
    let mut collectors = Vec::new();
    for index in 0..3 {
        let execution = format!("s{index}");
        let args=serde_json::from_value(json!({"command":"read -r begin && printf COMMITTED && read -r finish","stdin_open":true})).unwrap();
        let shell = supervisor
            .spawn(execution.clone(), root.path().into(), args, BTreeMap::new())
            .await
            .unwrap();
        let handle = shell.handle.clone();
        control
            .live
            .register_shell(
                execution.clone(),
                LiveShell {
                    run_id: Some(format!("r{index}")),
                    handle: handle.clone(),
                    last_committed_output: (index == 2).then_some(hidden_marker),
                },
            )
            .await;
        handles.push(handle);
        let mut shell_owner = ShellOwner::Run {
            thread: format!("t{index}"),
            run: format!("r{index}"),
            turn: format!("u{index}"),
        };
        let collecting = Arc::clone(&control);
        collectors.push(tokio::spawn(async move {
            collect_shell(&collecting, &mut shell_owner, &execution, shell).await
        }));
    }
    assert!(
        control
            .committed_output_markers(&[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        control
            .committed_output_markers(&["absent".into()])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        control
            .committed_output_markers(&["s0".into(), "s1".into(), "s2".into(), "absent".into()])
            .await
            .unwrap_err()
            .code,
        ErrorCode::InputLimit
    );
    let ids = ["s0".into(), "s1".into(), "s2".into()];
    assert_eq!(
        control.committed_output_markers(&ids).await.unwrap(),
        vec![("s2".into(), hidden_marker)]
    );
    let subscription = control.display_output_subscription();
    let notified = subscription.notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let output_started = Instant::now();
    handles[0].write("go\n", "UTF-8").await.unwrap();
    if rollback {
        let failed = tokio::time::timeout(WATCHDOG, collectors.remove(0))
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(failed.code, ErrorCode::SaveFailed);
        assert!(
            notified.as_mut().now_or_never().is_none(),
            "rolled-back output never emits the normal output Notify"
        );
        assert_eq!(
            control.committed_output_markers(&ids).await.unwrap(),
            vec![("s2".into(), hidden_marker)]
        );
    } else {
        tokio::time::timeout(WATCHDOG, notified.as_mut())
            .await
            .unwrap();
        // Deliberately no DB observation before this getter: the notification
        // itself must not race ahead of recording the committed marker.
        let markers = control.committed_output_markers(&ids).await.unwrap();
        let committed = markers.iter().find(|(id, _)| id == "s0").unwrap().1;
        assert!(committed >= output_started);
        assert_eq!(markers.len(), 2);
        assert_eq!(
            markers.iter().find(|(id, _)| id == "s2").unwrap().1,
            hidden_marker
        );
        assert!(!markers.iter().any(|(id, _)| id == "s1"));
        assert!(
            !handles[0].is_finished(),
            "stdin gate keeps output observation separate from process completion"
        );
    }
    owner
        .db
        .call(move |db| {
            let count = db.query_row(
                "SELECT count(*) FROM output_chunk WHERE execution_id='s0'",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            let sequence = db.query_row(
                "SELECT saved_sequence FROM shell_execution WHERE id='s0'",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            if rollback {
                assert_eq!((count, sequence), (0, 0));
            } else {
                assert!(count > 0 && sequence > 0);
            }
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM output_chunk WHERE execution_id IN ('s1','s2')",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    for handle in &handles {
        handle.stop();
    }
    for collector in collectors {
        tokio::time::timeout(WATCHDOG, collector)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    for execution in ["s0", "s1", "s2"] {
        control.live.remove_shell(execution).await;
    }
    assert!(
        control
            .committed_output_markers(&ids)
            .await
            .unwrap()
            .is_empty()
    );
    let shutdown = control.shutdown().await;
    if rollback {
        assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
    } else {
        shutdown.unwrap();
    }
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn service_endpoint_retires_even_when_output_cannot_be_saved() {
    for obstructed in [false, true] {
        let (root, owner, control, accepted) =
            crate::control::driver::preparation_tests::fixture("http://127.0.0.1:1/v1").await;
        let execution = uuid::Uuid::new_v4().to_string();
        let saved = execution.clone();
        let thread = accepted.thread_id;
        let saved_thread = thread.clone();
        owner.db.call(move |db| {
            db.execute("INSERT INTO schedule(id,thread_id,kind,enabled,cwd,command) VALUES('service',?1,'service',1,'.','echo fixture')", [&saved_thread])?;
            db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES(?1,'service',?1,'starting','.','{}','utf-8','utf-8','closed','2020-01-01T00:00:00Z')", [&saved])?;
            db.execute_batch("CREATE TRIGGER reject_service_output BEFORE INSERT ON output_chunk BEGIN SELECT RAISE(ABORT,'synthetic output rollback'); END;")?;
            Ok(())
        }).await.unwrap();
        let publication = Arc::new(
            crate::control::service_endpoint::Publication::create(&control, &execution)
                .await
                .unwrap(),
        );
        let file = std::path::PathBuf::from(publication.env_value());
        assert!(file.is_file());
        if obstructed {
            std::fs::remove_file(&file).unwrap();
            std::fs::create_dir(&file).unwrap();
        }
        let shell = ShellSupervisor::new(&owner.loaded.config)
            .spawn(
                execution.clone(),
                root.path().into(),
                serde_json::from_value(json!({"command":"printf fixture; exec sleep 600"}))
                    .unwrap(),
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let (stop, _) = control
            .shell_start_intent(&execution, true, Some(publication.clone()))
            .await
            .unwrap();
        assert!(!stop);
        let mut shell_owner = ShellOwner::Schedule {
            thread,
            endpoint: Some(publication),
        };
        let result = crate::test_watchdog::watchdog(
            "post-reap endpoint retirement",
            collect_shell(&control, &mut shell_owner, &execution, shell),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().code, ErrorCode::SaveFailed);
        let saved = crate::control::output::ShellOutputReader::new(
            owner.db.clone(),
            owner.instance_id.clone(),
        )
        .get(&execution)
        .await
        .unwrap();
        assert_eq!(saved.state, ShellState::Stopped);
        assert!(!saved.output_complete);
        assert_eq!(
            saved.settlement_detail,
            obstructed.then_some(ShellSettlementDetail::ServiceEndpointRetirementFailed)
        );
        if obstructed {
            assert!(file.is_dir());
            std::fs::remove_dir(&file).unwrap();
        } else {
            assert!(!file.exists());
        }
        assert_eq!(
            control.shutdown().await.unwrap_err().code,
            ErrorCode::SaveFailed
        );
        owner.shutdown().await.unwrap();
    }
}
