//! Recorded work only: no user data, child process, or external API is used.
use serde_json::{Value, json};
use shellboy::{
    config::Config, control::db::Store, protocol::*, status::StatusSnapshot, storage::DbWorker,
};
use std::sync::Arc;

struct Fixture {
    _temp: tempfile::TempDir,
    db: DbWorker,
    store: Store,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("fixture.db"))
            .await
            .unwrap();
        db.call(|db|{db.execute_batch("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('a',NULL,'Alpha','/fixture','/fixture/AGENTS.md','current-profile','current-model','2026-10-25T08:00:00Z'),('b','a','Thread child','/fixture','/fixture/AGENTS.md','current-profile','current-model','2026-10-25T08:00:00Z'),('c',NULL,'Owned work','/fixture','/fixture/AGENTS.md','current-profile','current-model','2026-10-25T08:00:00Z');
        INSERT INTO run(id,thread_id,owner_run_id,state,created_at,ended_at)VALUES('root','a',NULL,'live','2026-10-25T07:00:00Z',NULL),('owned','c','root','live','2026-10-25T07:01:00Z',NULL),('unrelated','b',NULL,'completed','2026-10-25T06:00:00Z','2026-10-25T07:30:00Z');
        INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('turn-root','a','root','running','2026-10-25T07:02:00Z',NULL),('turn-owned','c','owned','waiting','2026-10-25T07:03:00Z',NULL),('turn-other','b','unrelated','completed','2026-10-25T06:05:00Z','2026-10-25T07:30:00Z');
        INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('held-node',NULL,'user','2026-10-25T07:04:00Z','\"PRIVATE_BODY\"',14),('answer','turn-other','assistant','2026-10-25T07:30:00Z','\"PRIVATE_ANSWER\"',16),('applied-node','turn-other','user','2026-10-25T06:05:00Z','\"PRIVATE_BODY\"',14);
        UPDATE turn SET answer_node_id='answer' WHERE id='turn-other';
        INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','root','turn-root','exec','running','/saved-shell-cwd','{\"command\":\"PRIVATE_COMMAND\",\"env\":{\"SECRET\":\"PRIVATE_ENV\"}}','UTF-8','UTF-8','open','2026-10-25T07:05:00Z');
        INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,updated_at,hold_reason)VALUES('held-receipt','fixture:held','{\"token\":\"PRIVATE_TOKEN\"}','c','held-node','held','2026-10-25T07:04:00Z','2026-10-25T07:06:00Z','stopping');
        INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at)VALUES('applied-receipt','fixture:applied','{\"token\":\"PRIVATE_TOKEN\"}','b','applied-node','unrelated','turn-other','applied','2026-10-25T06:05:00Z','2026-10-25T07:30:00Z');
        INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','turn-root',0,0,'actual-profile','actual-model',0,'inflight','{\"token\":\"PRIVATE_MANIFEST\"}','2026-10-25T07:07:00Z');
        INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('fixture:held','fixture','{\"token\":\"PRIVATE_TOKEN\"}','hash','completed','{\"result\":{\"message_id\":\"held-node\"}}','2026-10-25T07:04:00Z');")?;Ok(())}).await.unwrap();
        let store = Store::new(
            db.clone(),
            Arc::new(Config::default()),
            temp.path().into(),
            "fixture".into(),
        );
        Self {
            _temp: temp,
            db,
            store,
        }
    }
    async fn status(&self, args: StatusArgs) -> Result<StatusSnapshot, ApiError> {
        let request = Request {
            protocol_version: 1,
            operation_id: "fixture:status".into(),
            command: Command::Status(args),
            origin: None,
        };
        self.store
            .execute(request, None)
            .await
            .map(|v| serde_json::from_value(v.result).unwrap())
    }
    async fn close(self) {
        self.db.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn one_projection_exposes_native_ownership_models_unassigned_messages_and_safe_targets_without_writes()
 {
    let f = Fixture::new().await;
    let before =
        f.db.call(|db| {
            Ok((
                db.total_changes(),
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            ))
        })
        .await
        .unwrap();
    let status = f.status(StatusArgs::default()).await.unwrap();
    assert_eq!(status.scope, "durable_only");
    assert_eq!(status.consistency, "single_sqlite_read_transaction");
    assert_eq!(status.items.len(), 7);
    let owned = status
        .items
        .iter()
        .find(|i| i.kind == "run" && i.id == "owned")
        .unwrap();
    assert_eq!(owned.owner_run_id.as_deref(), Some("root"));
    assert_eq!(owned.parent_thread_id, None);
    let attempt = status.items.iter().find(|i| i.kind == "attempt").unwrap();
    assert_eq!(attempt.current_model, "current-model");
    assert_eq!(attempt.attempt_model.as_deref(), Some("actual-model"));
    let held = status.items.iter().find(|i| i.kind == "message").unwrap();
    assert_eq!(held.run_id, None);
    assert_eq!(held.message_id.as_deref(), Some("held-node"));
    let shell = status.items.iter().find(|i| i.kind == "shell").unwrap();
    assert!(matches!(&shell.targets[0],Command::ShellGet(a)if a.execution_id=="exec"));
    assert!(matches!(&shell.targets[1],Command::ShellRead(a)if a.execution_id=="exec"));
    assert!(shell.operation_id.is_none());
    assert!(
        !shell
            .targets
            .iter()
            .any(|c| matches!(c, Command::OperationGet(_)))
    );
    let raw = serde_json::to_string(&status).unwrap();
    for private in [
        "PRIVATE_BODY",
        "PRIVATE_ANSWER",
        "PRIVATE_COMMAND",
        "PRIVATE_ENV",
        "PRIVATE_TOKEN",
        "PRIVATE_MANIFEST",
    ] {
        assert!(!raw.contains(private), "{private}");
    }
    let after =
        f.db.call(|db| {
            Ok((
                db.total_changes(),
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(before, after);
    f.close().await;
}

#[tokio::test]
async fn exact_scope_state_and_absolute_time_filters_keep_request_result_links() {
    let f = Fixture::new().await;
    let thread = f
        .status(StatusArgs {
            thread_id: Some("b".into()),
            all: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(thread.items.iter().all(|i| i.thread_id == "b"));
    assert!(
        thread
            .items
            .iter()
            .any(|i| i.parent_thread_id.as_deref() == Some("a") && i.owner_run_id.is_none())
    );
    let run = f
        .status(StatusArgs {
            run_id: Some("root".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        run.items
            .iter()
            .all(|i| i.run_id.as_deref() == Some("root"))
    );
    assert!(!run.items.iter().any(|i| i.id == "owned"));
    let held = f
        .status(StatusArgs {
            state: vec!["held".into()],
            since: Some("2026-10-25T16:06:00+09:00".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(held.items.len(), 1);
    assert_eq!(held.items[0].id, "held-node");
    let applied = f
        .status(StatusArgs {
            state: vec!["applied".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(applied.items[0].answer_node_id.as_deref(), Some("answer"));
    assert!(
        applied.items[0]
            .targets
            .iter()
            .any(|c| matches!(c,Command::HistoryGet(a)if a.node_id=="answer"))
    );
    let bad = f
        .status(StatusArgs {
            thread_id: Some("a".into()),
            run_id: Some("owned".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidArgument);
    f.close().await;
}

#[tokio::test]
async fn pagination_continues_across_state_changes_but_rejects_filter_changes() {
    let f = Fixture::new().await;
    let args = StatusArgs {
        all: true,
        limit: 2,
        ..Default::default()
    };
    let first = f.status(args.clone()).await.unwrap();
    assert_eq!(first.items.len(), 2);
    let cursor = first.next_cursor.clone().unwrap();
    let mut page = first;
    let mut ids = Vec::new();
    loop {
        ids.extend(page.items.iter().map(|i| (i.kind.clone(), i.id.clone())));
        let Some(cursor) = page.next_cursor else {
            break;
        };
        page = f
            .status(StatusArgs {
                cursor: Some(cursor),
                ..args.clone()
            })
            .await
            .unwrap();
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 10);
    let wrong = f
        .status(StatusArgs {
            cursor: Some(cursor.clone()),
            thread_id: Some("a".into()),
            ..args.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(wrong.code, ErrorCode::CursorExpired);
    f.db.call(|db| {
        db.execute(
            "UPDATE shell_execution SET state='stopping' WHERE id='exec'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let continued = f
        .status(StatusArgs {
            cursor: Some(cursor),
            limit: 200,
            ..args
        })
        .await
        .unwrap();
    assert!(
        continued
            .items
            .iter()
            .any(|item| item.id == "exec" && item.state == "stopping")
    );
    f.close().await;
}

#[tokio::test]
async fn model_origin_defaults_to_own_thread_without_reinterpreting_all_as_global_scope() {
    let f = Fixture::new().await;
    let caller = Origin {
        instance_id: "fixture".into(),
        thread_id: "a".into(),
        run_id: "root".into(),
        turn_id: "turn-root".into(),
        execution_id: "exec".into(),
        token: "private".into(),
    };
    let request = Request {
        protocol_version: 1,
        operation_id: "fixture:status".into(),
        command: Command::Status(StatusArgs {
            all: true,
            ..Default::default()
        }),
        origin: None,
    };
    let value = f
        .store
        .execute(request, Some(caller.clone()))
        .await
        .unwrap()
        .result;
    assert_eq!(value["filters"]["thread_id"], "a");
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["thread_id"] == "a")
    );
    let request = Request {
        protocol_version: 1,
        operation_id: "fixture:status".into(),
        command: Command::Status(StatusArgs {
            run_id: Some("owned".into()),
            ..Default::default()
        }),
        origin: None,
    };
    let value = f.store.execute(request, Some(caller)).await.unwrap().result;
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["thread_id"] == "c")
    );
    assert_eq!(value["filters"]["thread_id"], Value::Null);
    f.close().await;
}

#[tokio::test]
async fn malformed_or_other_instance_cursors_are_rejected_and_large_lists_stay_bounded() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let f = Fixture::new().await;
    let first = f
        .status(StatusArgs {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    let mut parsed: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor).unwrap()).unwrap();
    parsed["instance"] = json!("another-owner");
    let other = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&parsed).unwrap());
    assert_eq!(
        f.status(StatusArgs {
            cursor: Some(other),
            ..Default::default()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::CursorExpired
    );
    parsed["instance"] = json!("fixture");
    parsed["last_activity"] = json!("not-a-date");
    let malformed = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&parsed).unwrap());
    assert_eq!(
        f.status(StatusArgs {
            cursor: Some(malformed),
            ..Default::default()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::CursorExpired
    );
    f.db.call(|db|{let tx=db.transaction()?;for i in 0..210{tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason)VALUES(?1,'a','stopped','2026-10-25T07:00:00Z','2026-10-25T08:00:00Z',?2)",rusqlite::params![format!("old-{i:03}"),"x".repeat(5000)])?;}tx.execute("UPDATE thread SET title=?1 WHERE id='a'",["y".repeat(5000)])?;tx.commit()?;Ok(())}).await.unwrap();
    let result = f
        .status(StatusArgs {
            all: true,
            limit: 200,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.items.len(), 200);
    assert!(result.next_cursor.is_some());
    assert!(
        result
            .items
            .iter()
            .all(|i| i.thread_title.chars().count() <= 80
                && i.reason.as_ref().is_none_or(|r| r.chars().count() <= 512))
    );
    assert!(serde_json::to_vec(&result).unwrap().len() < MAX_FRAME_BYTES);
    f.close().await;
}

#[test]
fn status_request_defaults_and_bounds_are_part_of_the_wire_contract() {
    let value: Command = serde_json::from_value(json!({"command":"status","args":{}})).unwrap();
    assert!(matches!(
        value,
        Command::Status(StatusArgs { limit: 50, .. })
    ));
    for args in [
        StatusArgs {
            limit: 0,
            ..Default::default()
        },
        StatusArgs {
            limit: 201,
            ..Default::default()
        },
        StatusArgs {
            since: Some("2026-10-25T07:00:00".into()),
            ..Default::default()
        },
        StatusArgs {
            state: vec!["live".into(); 33],
            ..Default::default()
        },
    ] {
        assert_eq!(
            Command::Status(args).validate().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
}

#[tokio::test]
async fn receipt_lookup_is_opt_in_read_only_and_never_returns_saved_secret_outcomes() {
    let f = Fixture::new().await;
    let before = f.db.call(|db| Ok(db.total_changes())).await.unwrap();
    let lookup = f
        .status(StatusArgs {
            operation_id: Some("fixture:held".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(lookup.scope, "operation");
    assert!(lookup.items.is_empty());
    let value = serde_json::to_value(&lookup).unwrap();
    assert_eq!(value["operation"]["state"], "completed");
    assert_eq!(value["operation"]["outcome_included"], false);
    assert!(!value.to_string().contains("PRIVATE_TOKEN"));
    assert_eq!(value["operation"]["targets"]["message_id"], "held-node");
    assert!(value["operation"].get("outcome").is_none());
    assert_eq!(
        f.status(StatusArgs {
            operation_id: Some("missing".into()),
            ..Default::default()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::OperationNotFound
    );
    assert_eq!(
        f.status(StatusArgs {
            operation_id: Some("fixture:held".into()),
            thread_id: Some("a".into()),
            ..Default::default()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.db.call(|db| Ok(db.total_changes())).await.unwrap(),
        before
    );
    use clap::Parser;
    let parsed =
        shellboy::cli::Cli::try_parse_from(["shellboy", "status", "--operation", "fixture:held"])
            .unwrap();
    let command = parsed
        .into_command_with_stdin(std::path::Path::new("/fixture"), &mut tokio::io::empty())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(command,Command::Status(StatusArgs{operation_id:Some(id),..}) if id=="fixture:held")
    );
    f.close().await;
}

#[tokio::test]
async fn continuation_keeps_initial_birth_watermark_skips_deleted_anchor_and_reads_current_metadata()
 {
    let f = Fixture::new().await;
    let args = StatusArgs {
        all: true,
        limit: 1,
        ..Default::default()
    };
    let first = f.status(args.clone()).await.unwrap();
    // Initial top item is the applied message. Delete its source and update an
    // existing shell without changing its birth: neither invalidates paging.
    assert_eq!(first.items[0].id, "applied-node");
    f.db.call(|db| {
        db.execute_batch("DELETE FROM receipt WHERE id='applied-receipt';
        UPDATE shell_execution SET state='exited',exit_code=7,ended_at='2026-10-25T07:20:00Z',end_reason='user_stop' WHERE id='exec';
        UPDATE thread SET title='Renamed live metadata' WHERE id='a';
        INSERT INTO run(id,thread_id,state,created_at) VALUES('new-run','a','completed','2026-10-25T09:00:00Z');
        INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('new-turn','a','new-run','completed','2026-10-25T09:01:00Z');")?;
        Ok(())
    }).await.unwrap();
    let mut cursor = first.next_cursor;
    let mut remaining = Vec::new();
    while let Some(next) = cursor {
        let page = f
            .status(StatusArgs {
                cursor: Some(next),
                limit: 3,
                ..args.clone()
            })
            .await
            .unwrap();
        assert_ne!(page.fingerprint, first.fingerprint);
        assert!(page.items.len() <= 3);
        remaining.extend(page.items);
        cursor = page.next_cursor;
    }
    assert_eq!(remaining.len(), 9);
    assert!(
        remaining
            .iter()
            .all(|item| item.id != "new-run" && item.id != "new-turn")
    );
    let shell = remaining.iter().find(|item| item.id == "exec").unwrap();
    assert_eq!(shell.state, "exited");
    assert_eq!(shell.activity_at, "2026-10-25T07:20:00Z");
    assert_eq!(shell.thread_title, "Renamed live metadata");
    assert_eq!(shell.reason.as_deref(), Some("user_stop"));
    let fresh = f.status(StatusArgs { limit: 200, ..args }).await.unwrap();
    assert!(fresh.items.iter().any(|item| item.id == "new-run"));
    assert!(fresh.items.iter().any(|item| item.id == "new-turn"));
    f.close().await;
}

#[tokio::test]
async fn cursor_binding_covers_scope_version_kind_watermark_and_normalized_conditions() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let f = Fixture::new().await;
    let args = StatusArgs {
        all: true,
        state: vec!["live".into(), "running".into(), "live".into()],
        since: Some("2026-10-25T07:00:00Z".into()),
        limit: 1,
        ..Default::default()
    };
    let first = f.status(args.clone()).await.unwrap();
    let cursor = first.next_cursor.unwrap();
    let normalized = f
        .status(StatusArgs {
            state: vec!["running".into(), "live".into()],
            since: Some("2026-10-25T16:00:00+09:00".into()),
            limit: 200,
            cursor: Some(cursor.clone()),
            ..args.clone()
        })
        .await
        .unwrap();
    assert!(!normalized.items.is_empty());
    let parsed: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&cursor).unwrap()).unwrap();
    for (field, value) in [
        ("scope", json!("operation")),
        ("version", json!(1)),
        ("watermark", json!("invalid")),
        ("last_activity", json!("invalid")),
        ("last", json!(["unknown_kind", "id"])),
        ("last", json!(["run", ""])),
    ] {
        let mut wrong = parsed.clone();
        wrong[field] = value;
        let token = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wrong).unwrap());
        let error = f
            .status(StatusArgs {
                cursor: Some(token),
                ..args.clone()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CursorExpired, "{field}");
    }
    for changed in [
        StatusArgs {
            state: vec!["running".into()],
            ..args.clone()
        },
        StatusArgs {
            since: Some("2026-10-25T07:01:00Z".into()),
            ..args.clone()
        },
        StatusArgs {
            all: false,
            ..args.clone()
        },
        StatusArgs {
            run_id: Some("root".into()),
            ..args.clone()
        },
    ] {
        assert_eq!(
            f.status(StatusArgs {
                cursor: Some(cursor.clone()),
                ..changed
            })
            .await
            .unwrap_err()
            .code,
            ErrorCode::CursorExpired
        );
    }
    f.close().await;
}

#[tokio::test]
async fn updated_last_item_may_move_out_of_page_without_forcing_restart() {
    let f = Fixture::new().await;
    let args = StatusArgs {
        limit: 1,
        ..Default::default()
    };
    let first = f.status(args.clone()).await.unwrap();
    assert_eq!(first.items[0].id, "attempt");
    f.db.call(|db| { db.execute("UPDATE model_attempt SET state='completed',ended_at='2026-10-25T09:00:00Z' WHERE id='attempt'", [])?; Ok(()) }).await.unwrap();
    let continued = f
        .status(StatusArgs {
            cursor: first.next_cursor,
            limit: 200,
            ..args
        })
        .await
        .unwrap();
    assert_eq!(continued.items.len(), 6);
    assert!(continued.items.iter().all(|item| item.id != "attempt"));
    f.close().await;
}

#[tokio::test]
async fn maximal_valid_escaped_filters_still_produce_reusable_bounded_cursors() {
    let f = Fixture::new().await;
    let mut states = vec!["live".into()];
    states.extend((0..31).map(|index| format!("{}{index:02}", "\0".repeat(60))));
    let args = StatusArgs {
        state: states,
        limit: 1,
        ..Default::default()
    };
    Command::Status(args.clone()).validate().unwrap();
    let first = f.status(args.clone()).await.unwrap();
    let cursor = first.next_cursor.unwrap();
    assert!(cursor.len() <= 8192);
    let next = f
        .status(StatusArgs {
            cursor: Some(cursor),
            ..args
        })
        .await
        .unwrap();
    assert_eq!(next.items.len(), 1);
    assert_ne!(first.items[0].id, next.items[0].id);
    f.close().await;
}
