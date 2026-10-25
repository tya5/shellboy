use super::shell_lifecycle_tests::{
    answer, fixture, fixture_with_settings, incoming, rpc, start, tool,
};
use super::*;
use tokio::time::{Duration, timeout};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_attachment_before_snapshot_recovers_once_without_driver_restart_storm() {
    for active_turn in [true, false] {
        let (_root, owner, control, mut requests, server) = fixture().await;
        let thread = rpc(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                connection: None,
                model: None,
                effort: None,
                cwd: None,
                parent_id: None,
            }),
        )
        .await;
        let thread = thread["thread_id"].as_str().unwrap().to_owned();
        let target = thread.clone();
        control.store.database.call(move|db| {
            db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('damaged-run',?1,'live','now')",[&target])?;
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('existing-turn',?1,'damaged-run',?2,'now')",rusqlite::params![target,if active_turn{"preparing"}else{"completed"}])?;
            db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('retained-message','user','now','{\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"keep accepted input\"}]}',93);")?;
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at)VALUES('retained-receipt','retained-operation','{}',?1,'retained-message','damaged-run',?2,'accepted','now')",rusqlite::params![target,active_turn.then_some("existing-turn")])?;
            db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,deleted_reason)VALUES('missing-attachment-source','event','now',NULL,0,'fixture_source_missing');")?;
            db.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at,target_run_id)VALUES('missing-attachment-source',?1,'accepted','damaged-attachment','{\"kind\":\"external_cli\"}','now','damaged-run')",[target])?;
            Ok(())
        }).await.unwrap();
        let mut events = control.subscribe();
        control.kick_quiet(thread).await;
        let first = timeout(Duration::from_secs(3), async {
            loop {
                if let EngineEvent::Error { error, .. } = events.recv().await.unwrap() {
                    break error;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(first.code, ErrorCode::InvalidState);
        let observed=control.store.database.call(move |db| {
            let state:String=db.query_row("SELECT state FROM turn WHERE run_id='damaged-run' ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0))?;
            let delivery:String=db.query_row("SELECT status FROM attachment_delivery WHERE node_id='missing-attachment-source'",[],|r|r.get(0))?;
            assert!(db.query_row("SELECT r.state='accepted' AND json_extract(n.payload_json,'$.content[0].text')='keep accepted input' FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.id='retained-receipt'",[],|r|r.get::<_,bool>(0))?);
            assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE node_id='retained-message'",[],|r|r.get::<_,i64>(0))?,0,"failed adoption transaction must roll back preceding input adoption");
            if !active_turn { assert_eq!(db.query_row("SELECT state FROM turn WHERE id='existing-turn'",[],|r|r.get::<_,String>(0))?,"completed"); }
            Ok((state,delivery))
        }).await.unwrap();
        // Finish the real driver even on the red baseline before asserting.
        // The malformed source itself remains untouched; no OS/API is needed.
        control.begin_shutdown();
        control.shutdown().await.unwrap();
        let mut errors = 1;
        while let Ok(event) = events.try_recv() {
            if matches!(event, EngineEvent::Error { .. }) {
                errors += 1;
            }
        }
        assert!(requests.try_recv().is_err());
        assert!(!control.storage_failed.load(Ordering::Acquire));
        control.store.database.call(|db| {assert!(db.query_row("SELECT payload_json IS NULL AND deleted_reason='fixture_source_missing' FROM node WHERE id='missing-attachment-source'",[],|r|r.get::<_,bool>(0))?);Ok(())}).await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
        assert_eq!(
            observed,
            ("recovery_wait".into(), "held".into()),
            "permanent pre-snapshot failure must enter explicit recovery"
        );
        assert_eq!(
            errors, 1,
            "the same persisted failure must not automatically restart"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_shell_cannot_be_backgrounded_while_excerpt_is_loading() {
    let (_root, owner, control, mut requests, server) =
        fixture_with_settings("[limits]\ntool_excerpt_bytes=2097152\n").await;
    let thread = start(&control).await;
    let (_, reply) = incoming(&mut requests).await;
    // The real reader pins its immutable output before paged excerpt reads.
    // Stop the DB actor exactly at that persisted boundary, not on a timer.
    let database = control.store.database.clone();
    let (pinned, observed_pin) = tokio::sync::oneshot::channel();
    let (release_db, wait_release) = std::sync::mpsc::channel();
    let polling = tokio::spawn(async move {
        let mut pinned = Some(pinned);
        let mut wait_release = Some(wait_release);
        loop {
            let notice = pinned.take().unwrap();
            let release = wait_release.take().unwrap();
            let (seen, notice, release) = database.call(move |db| {
                let seen: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_pin p JOIN shell_execution s ON s.id=p.resource_id WHERE p.resource_kind='shell_output' AND s.state='exited')",
                    [], |row| row.get(0),
                )?;
                if seen {
                    let _ = notice.send(());
                    release.recv().unwrap();
                    Ok((true, None, None))
                } else {
                    Ok((false, Some(notice), Some(release)))
                }
            }).await.unwrap();
            if seen {
                break;
            }
            pinned = notice;
            wait_release = release;
            tokio::task::yield_now().await;
        }
    });
    reply
        .send(tool("head -c 2097152 /dev/zero | tr '\\000' x"))
        .unwrap();
    let boundary = timeout(Duration::from_secs(5), observed_pin).await;
    // Always release the actor before asserting or awaiting shutdown.
    let background = if boundary.is_ok() {
        Some(control.background(&thread).await)
    } else {
        None
    };
    let _ = release_db.send(());
    if boundary.is_err() {
        polling.abort();
    } else {
        polling.await.unwrap();
    }
    if boundary.is_ok() {
        let (_, reply) = incoming(&mut requests).await;
        reply.send(answer()).unwrap();
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
    boundary.unwrap().unwrap();
    assert!(
        background.unwrap().is_err(),
        "completion already won: excerpt loading is not a foreground shell wait"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_process_edit_does_not_wait_for_a_shell_caller_reply_grace() {
    let (_root, owner, control, mut requests, server) = fixture().await;
    assert_eq!(control.store.config.io.self_reply_grace_ms, 60_000);
    let thread = start(&control).await;
    // The actual driver is inside a model HTTP request. No shell caller exists
    // that needs a response before this UI-only edit stops its old Run.
    let (_, old_reply) = incoming(&mut requests).await;
    let target = thread.clone();
    let (run,node) = control.store.database.call(move|db| {
        Ok(db.query_row("SELECT r.id,n.id FROM run r JOIN receipt x ON x.run_id=r.id JOIN node n ON n.id=x.message_node_id WHERE r.thread_id=?1 AND r.state='live' ORDER BY x.rowid LIMIT 1",[target],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?)
    }).await.unwrap();
    let edited = control.clone();
    let operation = format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4());
    let mut pending = tokio::spawn(async move {
        edited
            .edit_ordered_request(
                operation,
                thread,
                node,
                Some(run),
                crate::attachments::DraftContent::appended("edited retained input".into(), 0),
                vec![],
                ThreadCreateArgs {
                    connection: None,
                    model: None,
                    effort: None,
                    cwd: None,
                    parent_id: None,
                },
            )
            .await
    });
    let completed = timeout(Duration::from_secs(3), &mut pending).await;
    // Red-baseline cleanup closes admission to interrupt the 60s grace; this
    // occurs only after measuring whether the edit finished by itself.
    let success = match completed {
        Ok(value) => Some(value.unwrap()),
        Err(_) => {
            control.prepare_shutdown().await.unwrap();
            let _ = pending.await.unwrap();
            None
        }
    };
    drop(old_reply);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
    assert!(
        success.is_some(),
        "in-process edit must not need shell self-reply grace"
    );
    assert!(success.unwrap().is_ok());
}
