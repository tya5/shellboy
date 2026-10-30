use super::*;
use crate::{
    control::db::{DraftSendOptions, DraftSubmission, Store},
    protocol::*,
    storage::DbWorker,
};
use std::sync::Arc;

async fn store() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("pending.db"))
        .await
        .unwrap();
    let config=Config::parse("default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let store = Store::new(db, Arc::new(config), root.path().into(), "fixture".into());
    store.database.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','fixture','synthetic','0'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','0');")?;
        Ok(())
    }).await.unwrap();
    (root, store)
}
fn origin(thread: &str) -> Origin {
    Origin {
        instance_id: "fixture".into(),
        thread_id: thread.into(),
        run_id: "source-run".into(),
        turn_id: "source-turn".into(),
        execution_id: "source-shell".into(),
        token: "private-origin-token".into(),
    }
}
fn input(db: &Connection, id: &str, sender: Option<&Origin>) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','user','0',?2)",
        params![
            id,
            json!({"role":"user","content":[{"type":"input_text","text":id}]}).to_string()
        ],
    )?;
    db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,result_json)VALUES(?1,?1,?2,'t',?1,'r','u','accepted','0','{}')",params![id,serde_json::to_string(&sender.map(Origin::identity)).unwrap()])?;
    Ok(())
}
fn request(id: &str, ids: &[&str]) -> Request {
    Request::new(
        format!("fixture:{id}"),
        Command::MessageCancel(MessageCancelArgs {
            message_ids: ids.iter().map(|id| (*id).into()).collect(),
        }),
    )
}
#[tokio::test]
async fn cancellation_is_partial_authorized_archived_idempotent_and_never_wakes_models() {
    let (_root, store) = store().await;
    store.database.call(|db| {
        input(db,"own",Some(&origin("sender")))?; input(db,"human",None)?;
        input(db,"rewound",Some(&origin("sender")))?; input(db,"selected",Some(&origin("sender")))?;
        input(db,"missing",Some(&origin("sender")))?; input(db,"uncertain",Some(&origin("sender")))?;
        db.execute("UPDATE receipt SET state='held',path_admission='unknown',hold_reason='uncertain_path_admission' WHERE id='uncertain'",[])?;
        db.execute_batch("UPDATE receipt SET path_admission='admitted' WHERE id='rewound'; INSERT INTO thread_path VALUES('t',0,'selected'); UPDATE thread SET cursor_node_id='selected' WHERE id='t'; UPDATE node SET payload_json=NULL,deleted_reason='collected' WHERE id='missing';")?; Ok(())
    }).await.unwrap();
    let req = request(
        "cancel-partial",
        &[
            "own",
            "human",
            "rewound",
            "selected",
            "missing",
            "absent",
            "uncertain",
        ],
    );
    let outcome = store
        .execute(req.clone(), Some(origin("sender")))
        .await
        .unwrap();
    assert!(outcome.wake_threads.is_empty() && outcome.quiet_wake_threads.is_empty());
    assert_eq!(outcome.changed_threads, ["t"]);
    let cancelled: MessageCancelResult = serde_json::from_value(outcome.result.clone()).unwrap();
    assert_eq!(
        cancelled
            .items
            .iter()
            .map(|i| i.outcome)
            .collect::<Vec<_>>(),
        [
            MessageCancelOutcome::Cancelled,
            MessageCancelOutcome::Forbidden,
            MessageCancelOutcome::AlreadyAdmitted,
            MessageCancelOutcome::AlreadyAdmitted,
            MessageCancelOutcome::Unavailable,
            MessageCancelOutcome::NotFound,
            MessageCancelOutcome::Cancelled
        ]
    );
    assert!(cancelled.items[1].thread_id.is_none());
    let replay = store.execute(req, Some(origin("sender"))).await.unwrap();
    assert_eq!(replay.result, outcome.result);
    assert!(replay.changed_threads.is_empty());
    let already = store
        .execute(request("cancel-again", &["own"]), Some(origin("sender")))
        .await
        .unwrap();
    assert_eq!(already.result["items"][0]["outcome"], "already_cancelled");
    let human = store
        .execute(request("human-cancel", &["human"]), None)
        .await
        .unwrap();
    assert_eq!(human.result["items"][0]["outcome"], "cancelled");
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='archived'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                3
            );
            assert_eq!(
                db.query_row(
                    "SELECT run_id,turn_id,path_admission FROM receipt WHERE id='own'",
                    [],
                    |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?
                    ))
                )?,
                ("r".into(), "u".into(), "unadmitted".into())
            );
            let body: String = db.query_row(
                "SELECT body_json FROM draft WHERE id='cancelled-message:own'",
                [],
                |r| r.get(0),
            )?;
            assert!(!body.contains("private-origin-token"));
            assert!(!body.contains("payload_json"));
            assert_eq!(
                db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))?,
                0,
                "cancellation has no model notification"
            );
            let root: bool = db.query_row(
                &format!(
                    "SELECT {} FROM node n WHERE n.id='own'",
                    crate::history::reachability::ROOT_PREDICATE
                ),
                [],
                |r| r.get(0),
            )?;
            assert!(
                root,
                "the live-target receipt already protects archived source history"
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancellation_and_admission_commit_boundary_prevents_retraction_or_partial_storage_success()
{
    let (_root, store) = store().await;
    store
        .database
        .call(|db| {
            input(db, "first", None)?;
            input(db, "second", None)?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .execute(request("cancel-first", &["first"]), None)
        .await
        .unwrap();
    store
        .database
        .call(|db| {
            let tx = db.transaction()?;
            let pending: Vec<String> = tx
                .prepare(
                    "SELECT message_node_id FROM receipt WHERE state='accepted' ORDER BY rowid",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(pending, ["second"]);
            crate::history::append_existing_nodes(&tx, "t", 0, &pending, "admit")?;
            crate::control::message_delivery::mark_admitted_tx(&tx, "t", &pending)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .execute(request("too-late", &["second"]), None)
            .await
            .unwrap()
            .result["items"][0]["outcome"],
        "already_admitted"
    );
    store.database.call(|db| {
        input(db,"third",None)?; input(db,"fourth",None)?;
        db.execute_batch("CREATE TRIGGER fail_cancel_archive BEFORE INSERT ON draft WHEN new.id='cancelled-message:fourth' BEGIN SELECT RAISE(ABORT,'synthetic archive failure'); END;")?; Ok(())
    }).await.unwrap();
    assert!(
        store
            .execute(request("storage-fail", &["third", "fourth"]), None)
            .await
            .is_err()
    );
    store.database.call(|db| {
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE id IN ('third','fourth') AND state='accepted'",[],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(db.query_row("SELECT count(*) FROM draft WHERE id='cancelled-message:third'",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn only_explicit_unadmitted_now_input_releases_foreground_wait() {
    let (_root, store) = store().await;
    let queued = store
        .execute(
            Request::new(
                "fixture:queued",
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some("t".into()),
                    text: "ordinary".into(),
                    now: false,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(queued.quiet_wake_threads, ["t"]);
    assert!(queued.wake_threads.is_empty());
    store
        .database
        .call(|db| {
            assert!(!has_immediate_pending_tx(db, "u")?);
            Ok(())
        })
        .await
        .unwrap();
    let now = store
        .execute(
            Request::new(
                "fixture:now",
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some("t".into()),
                    text: "urgent".into(),
                    now: true,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(now.wake_threads, ["t"]);
    assert!(now.quiet_wake_threads.is_empty());
    assert_eq!(now.result["now"], true);
    let id = now.result["message_id"].as_str().unwrap().to_owned();
    store
        .database
        .call(move |db| {
            assert!(has_immediate_pending_tx(db, "u")?);
            let tx = db.transaction()?;
            crate::history::append_existing_nodes(&tx, "t", 0, std::slice::from_ref(&id), "admit")?;
            crate::control::message_delivery::mark_admitted_tx(
                &tx,
                "t",
                std::slice::from_ref(&id),
            )?;
            assert!(
                !has_immediate_pending_tx(&tx, "u")?,
                "accepted but admitted now input cannot release a later call"
            );
            tx.execute("DELETE FROM thread_path WHERE node_id=?1", [id])?;
            assert!(
                !has_immediate_pending_tx(&tx, "u")?,
                "rewinding cannot re-arm an immediate send"
            );
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
fn submission(op: &str, source: &str, revision: u64, text: &str) -> DraftSubmission {
    DraftSubmission {
        draft_id: DraftSubmission::id_for_operation(op),
        source_draft_id: source.into(),
        revision,
        snapshot: json!({"draft_id":source,"revision":revision,"text":text,"cursor":text.len(),"attachments":[],"folds":[],"literal_paste":false,"edit":null}),
    }
}
#[tokio::test]
async fn frozen_submission_survives_precommit_failure_and_accepts_atomically_without_touching_newer_editable_draft()
 {
    let (root, store) = store().await;
    let frozen = submission("fixture:frozen", "editable", 7, "frozen send");
    store
        .prepare_draft_submission("fixture:frozen", Some("t"), &frozen)
        .await
        .unwrap();
    store
        .prepare_draft_submission("fixture:frozen", Some("t"), &frozen)
        .await
        .unwrap();
    let conflicting = submission("fixture:frozen", "editable", 7, "changed content");
    assert_eq!(
        store
            .prepare_draft_submission("fixture:frozen", Some("t"), &conflicting)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    store.database.call(|db| {
        db.execute("INSERT INTO draft VALUES('editable','t',8,?1,'editing','later')",[json!({"draft_id":"editable","revision":8,"text":"newer unsent edit","attachments":[]}).to_string()])?;
        db.execute_batch("CREATE TRIGGER fail_frozen_send BEFORE INSERT ON receipt WHEN new.client_operation_id='fixture:frozen' BEGIN SELECT RAISE(ABORT,'synthetic precommit failure'); END;")?; Ok(())
    }).await.unwrap();
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    };
    let options = DraftSendOptions {
        now: true,
        submission: Some(frozen.clone()),
        expected_selection_revision: Some(0),
        ..Default::default()
    };
    assert!(
        store
            .send_draft_content(
                "fixture:frozen".into(),
                args.clone(),
                Some("t".into()),
                crate::attachments::DraftContent::appended("frozen send".into(), 0),
                vec![],
                options.clone()
            )
            .await
            .is_err()
    );
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='submitting'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                0
            );
            db.execute_batch("DROP TRIGGER fail_frozen_send;")?;
            Ok(())
        })
        .await
        .unwrap();
    // A different operation has a different frozen identity; the failed operation remains immutable.
    let frozen = submission("fixture:accepted", "editable", 7, "frozen send");
    store
        .prepare_draft_submission("fixture:accepted", Some("t"), &frozen)
        .await
        .unwrap();
    let options = DraftSendOptions {
        now: true,
        submission: Some(frozen.clone()),
        expected_selection_revision: Some(0),
        ..Default::default()
    };
    let accepted = store
        .send_draft_content(
            "fixture:accepted".into(),
            args.clone(),
            Some("t".into()),
            crate::attachments::DraftContent::appended("frozen send".into(), 0),
            vec![],
            options.clone(),
        )
        .await
        .unwrap();
    let replay = store
        .send_draft_content(
            "fixture:accepted".into(),
            args,
            Some("t".into()),
            crate::attachments::DraftContent::appended("frozen send".into(), 0),
            vec![],
            options,
        )
        .await
        .unwrap();
    assert_eq!(replay.result, accepted.result);
    let frozen_id = frozen.draft_id.clone();
    let expected = accepted.result["message_id"].clone();
    store
        .database
        .call(move |db| {
            let (body, state): (String, String) = db.query_row(
                "SELECT body_json,state FROM draft WHERE id=?1",
                [frozen_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(state, "submitted");
            assert_eq!(body["_submission"]["message_id"], expected);
            assert_eq!(body["text"], "frozen send");
            assert_eq!(
                db.query_row(
                    "SELECT state,revision FROM draft WHERE id='editable'",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                )?,
                ("editing".into(), 8)
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
    let reopened = DbWorker::open(root.path().join("pending.db"))
        .await
        .unwrap();
    reopened
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='submitted'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn frozen_first_send_survives_without_thread_and_binds_only_at_acceptance() {
    let (_root, store) = store().await;
    let frozen = submission("fixture:first-thread", "new-composer", 3, "first request");
    store
        .prepare_draft_submission("fixture:first-thread", None, &frozen)
        .await
        .unwrap();
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='submitting' AND thread_id IS NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    let accepted = store
        .send_draft_content(
            "fixture:first-thread".into(),
            ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            },
            None,
            crate::attachments::DraftContent::appended("first request".into(), 0),
            vec![],
            DraftSendOptions {
                submission: Some(frozen.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let thread = accepted.result["thread_id"].as_str().unwrap().to_owned();
    store
        .database
        .call(move |db| {
            let (saved, state): (String, String) = db.query_row(
                "SELECT thread_id,state FROM draft WHERE id=?1",
                [frozen.draft_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(saved, thread);
            assert_eq!(state, "submitted");
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_gc_protects_frozen_and_archived_sources_until_their_thread_is_deleted() {
    let (root, store) = store().await;
    let directory = root.path().join("managed/clipboard");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("managed.png");
    std::fs::write(&file, b"fixture").unwrap();
    let file_text = file.to_string_lossy().into_owned();
    store.database.call(move|db| {
        db.execute("INSERT INTO managed_file(id,absolute_path,state,raw_size,created_at)VALUES('managed',?1,'ready',7,'2000-01-01')",[&file_text])?;
        db.execute("INSERT INTO draft VALUES('frozen','t',1,?1,'submitting','0')",[json!({"attachments":[{"id":"managed","path":file_text}]}).to_string()])?;Ok(())
    }).await.unwrap();
    let attachments = crate::attachments::AttachmentService::new(
        store.database.clone(),
        &store.config,
        root.path().into(),
        "fixture".into(),
    );
    for state in ["submitting", "submitted"] {
        store
            .database
            .call(move |db| {
                db.execute("UPDATE draft SET state=?1 WHERE id='frozen'", [state])?;
                Ok(())
            })
            .await
            .unwrap();
        let report = attachments
            .collect_managed(chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(report.protected_bytes, 7);
        assert!(report.deleted.is_empty());
        assert!(file.exists());
    }
    let file_text = file.to_string_lossy().into_owned();
    store.database.call(move|db| {
        db.execute("DELETE FROM draft WHERE id='frozen'",[])?; input(db,"archive-source",None)?;
        db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('reference','archive-source',?1,'image','managed_clipboard','0')",[file_text])?;Ok(())
    }).await.unwrap();
    store
        .execute(request("archive-source", &["archive-source"]), None)
        .await
        .unwrap();
    let report = attachments
        .collect_managed(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report.protected_bytes, 7);
    assert!(report.deleted.is_empty());
    assert!(file.exists());
    store
        .database
        .call(|db| {
            db.execute("UPDATE thread SET deleted_at='deleted' WHERE id='t'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let report = attachments
        .collect_managed(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report.protected_bytes, 0);
    assert_eq!(report.deleted, ["managed"]);
    assert!(!file.exists());
    store.database.shutdown().await.unwrap();
}
#[test]
fn cancel_ids_are_explicit_bounded_unique_and_immediate_defaults_to_false() {
    for ids in [vec![], vec!["a".into(); 129], vec!["a".into(), "a".into()]] {
        assert!(
            Command::MessageCancel(MessageCancelArgs { message_ids: ids })
                .validate()
                .is_err()
        );
    }
    assert!(
        Command::MessageCancel(MessageCancelArgs {
            message_ids: (0..128).map(|n| format!("message-{n}")).collect()
        })
        .validate()
        .is_ok()
    );
    let default: MessageSendArgs =
        serde_json::from_value(json!({"thread_id":"t","text":"queued"})).unwrap();
    assert!(!default.now);
}

#[tokio::test]
async fn actual_control_draft_replay_precedes_missing_files_and_old_owner_checks() {
    use crate::{app::DataOwner, control::Control};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\nresponse_max_retries=0\n[limits]\nattachment_file_bytes=1024\nattachment_request_bytes=1024\n").unwrap();
    let data = root.path().join("data");
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let operation = format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4());
    let old_instance = control.store.instance_id.clone();
    let file = root.path().join("original.txt");
    std::fs::write(&file, b"fixture").unwrap();
    let files = vec![crate::attachments::AttachmentInput {
        path: file.clone(),
        kind: AttachmentKind::File,
    }];
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    };
    let mut frozen = submission(&operation, "composer", 3, "saved request\u{fffc}");
    frozen.snapshot["attachments"] = json!([{"id":"original-chip","path":file,"kind":"file","state":"active","position":"saved request".len()}]);
    let options = DraftSendOptions {
        submission: Some(frozen.clone()),
        ..Default::default()
    };
    let accepted = control
        .send_ordered_draft(
            operation.clone(),
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options.clone(),
        )
        .await
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    let replay = control
        .send_ordered_draft(
            operation.clone(),
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay, accepted);
    // Changed file bytes do not change the identity of the original request.
    std::fs::write(&file, vec![0; 2048]).unwrap();
    let replay = control
        .send_ordered_draft(
            operation.clone(),
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay, accepted);
    for field in ["now", "text", "snapshot", "files", "args", "thread"] {
        let mut changed = options.clone();
        let mut changed_args = args.clone();
        let mut changed_files = files.clone();
        let mut changed_thread = None;
        let text = if field == "text" {
            "different request"
        } else {
            "saved request"
        };
        match field {
            "now" => changed.now = true,
            "snapshot" => {
                changed.submission.as_mut().unwrap().snapshot["text"] =
                    json!("different frozen source")
            }
            "files" => changed_files[0].path = root.path().join("different.txt"),
            "args" => changed_args.model = Some("different-model".into()),
            "thread" => changed_thread = Some("different-thread".into()),
            _ => {}
        }
        let error = control
            .send_ordered_draft(
                operation.clone(),
                changed_args,
                changed_thread,
                crate::attachments::DraftContent::appended(text.into(), 1),
                changed_files,
                changed,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::OperationConflict,
            "changed {field} cannot return a saved different request"
        );
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    assert_ne!(control.store.instance_id, old_instance);
    let before = owner
        .db
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM run", [], |r| r.get::<_, i64>(0))?,
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    let replay = control
        .send_ordered_draft(
            operation,
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options,
        )
        .await
        .unwrap();
    assert_eq!(
        replay, accepted,
        "known old-owner operation replays before frozen owner validation"
    );
    assert!(
        control.index_workers.get().is_none(),
        "saved replay does not start background workers"
    );
    let after = owner
        .db
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM run", [], |r| r.get::<_, i64>(0))?,
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(after, before, "saved replay has no mutation/driver effects");
    let expired = control
        .send_ordered_draft(
            format!("{old_instance}:unknown"),
            args,
            None,
            crate::attachments::DraftContent::appended("unknown".into(), 1),
            files,
            DraftSendOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(expired.code, ErrorCode::OperationExpired);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn frozen_budget_preserves_tiny_raw_text_limits_and_json_escaping_with_separate_metadata_bound()
 {
    let (root, base) = store().await;
    let mut config = (*base.config).clone();
    config.limits.draft_bytes = 128;
    let store = Store::new(
        base.database.clone(),
        Arc::new(config),
        root.path().into(),
        "fixture".into(),
    );
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    };
    for (n, text) in [
        "hi".into(),
        "a".repeat(128),
        "\u{0001}".repeat(128),
        "\"".repeat(128),
    ]
    .into_iter()
    .enumerate()
    {
        let operation = format!("fixture:raw-budget-{n}");
        let frozen = submission(&operation, &format!("source-{n}"), 1, &text);
        store
            .prepare_draft_submission(&operation, Some("t"), &frozen)
            .await
            .unwrap();
        let accepted = store
            .send_draft_content(
                operation,
                args.clone(),
                Some("t".into()),
                crate::attachments::DraftContent::appended(text, 0),
                vec![],
                DraftSendOptions {
                    submission: Some(frozen.clone()),
                    expected_selection_revision: Some(0),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(accepted.result["message_id"].is_string());
        store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row(
                        "SELECT state FROM draft WHERE id=?1",
                        [&frozen.draft_id],
                        |r| r.get::<_, String>(0)
                    )?,
                    "submitted"
                );
                let serialized: String = db.query_row(
                    "SELECT body_json FROM draft WHERE id=?1",
                    [&frozen.draft_id],
                    |row| row.get(0),
                )?;
                assert!(
                    serialized.len() > 128,
                    "saved metadata and escaping use a separate budget"
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    let too_much_text = submission("fixture:raw-too-long", "source-long", 1, &"a".repeat(129));
    let error = store
        .prepare_draft_submission("fixture:raw-too-long", Some("t"), &too_much_text)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert!(error.message.contains("text"));
    let mut too_much_metadata = submission("fixture:metadata-too-long", "source-metadata", 1, "a");
    too_much_metadata.snapshot["extra_metadata"] = json!("z".repeat(MAX_FRAME_BYTES + 4096));
    let error = store
        .prepare_draft_submission("fixture:metadata-too-long", Some("t"), &too_much_metadata)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert!(error.message.contains("metadata"));
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelling_source_reference_is_independent_of_the_current_raw_draft_text_limit() {
    let (root, base) = store().await;
    base.database
        .call(|db| {
            input(db, "cancel-with-small-budget", None)?;
            Ok(())
        })
        .await
        .unwrap();
    let mut config = (*base.config).clone();
    config.limits.draft_bytes = 1;
    let store = Store::new(
        base.database.clone(),
        Arc::new(config),
        root.path().into(),
        "fixture".into(),
    );
    let result = store
        .execute(request("tiny-cancel", &["cancel-with-small-budget"]), None)
        .await
        .unwrap();
    assert_eq!(result.result["items"][0]["outcome"], "cancelled");
    store.database.call(|db| {
        let bytes:i64=db.query_row("SELECT octet_length(body_json) FROM draft WHERE id='cancelled-message:cancel-with-small-budget'",[],|r|r.get(0))?;
        assert!(bytes>1 && bytes<=MAX_FRAME_BYTES as i64);Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}
