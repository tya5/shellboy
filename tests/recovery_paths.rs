//! Recovery guidance uses recorded IDs and read-only inspection before side effects.
use clap::{CommandFactory, Parser};
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    cli::Cli,
    ipc::{self, LocalServer},
    protocol::{ApiError, Command, ErrorCode, MessageSendArgs, Response},
};

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(120), future)
        .await
        .expect("finite recovery fixture deadline")
}

#[tokio::test]
async fn help_examples_parse_and_name_the_next_observation() {
    let mut help = Cli::command();
    let text = help.render_long_help().to_string();
    assert!(text.contains("shellboy docs list"));
    for (path, expected) in [
        (
            vec!["message", "send"],
            "shellboy message get --message MESSAGE_ID",
        ),
        (vec!["thread", "create"], "shellboy/delegation"),
        (vec!["shell", "get"], "WORK_EXECUTION_ID"),
        (vec!["shell", "read"], "result.cursor"),
        (vec!["operation", "get"], "details.operation_id"),
        (vec!["turn", "get"], "ANSWER_NODE_ID"),
        (vec!["wait"], "timeout leaves the target running"),
    ] {
        let mut command = Cli::command();
        for component in path {
            command = command.find_subcommand_mut(component).unwrap().clone();
        }
        assert!(
            command.render_long_help().to_string().contains(expected),
            "{expected}"
        );
    }
    let cases = [
        vec!["thread", "create", "--parent", "THREAD_ID"],
        vec![
            "message",
            "send",
            "--thread",
            "CHILD_THREAD_ID",
            "--text",
            "Read-only audit; return evidence",
        ],
        vec!["message", "get", "--message", "MESSAGE_ID"],
        vec!["turn", "get", "--turn", "TURN_ID"],
        vec!["history", "get", "--node", "ANSWER_NODE_ID"],
        vec!["shell", "get", "--execution", "WORK_EXECUTION_ID"],
        vec![
            "shell",
            "read",
            "--execution",
            "WORK_EXECUTION_ID",
            "--limit-bytes",
            "65536",
        ],
        vec![
            "shell",
            "read",
            "--execution",
            "WORK_EXECUTION_ID",
            "--cursor",
            "CURSOR",
            "--limit-bytes",
            "65536",
        ],
        vec!["operation", "get", "--operation", "OPERATION_ID"],
        vec![
            "wait",
            "--conditions-json",
            r#"[{"type":"shell_ended","execution_id":"WORK_EXECUTION_ID"}]"#,
            "--timeout-ms",
            "30000",
        ],
        vec![
            "wait",
            "--conditions-json",
            r#"[{"type":"turn_completed","turn_id":"CHILD_TURN_ID"}]"#,
            "--timeout-ms",
            "30000",
        ],
    ];
    for args in cases {
        let cli = Cli::try_parse_from(std::iter::once("shellboy").chain(args)).unwrap();
        cli.into_command(std::path::Path::new("/"))
            .await
            .unwrap()
            .unwrap()
            .validate()
            .unwrap();
    }
}

#[test]
fn generic_recovery_preserves_specific_hints_and_never_promises_safe_reexecution() {
    for code in [
        ErrorCode::OperationConflict,
        ErrorCode::OperationNotFound,
        ErrorCode::OperationExpired,
        ErrorCode::IoFailed,
        ErrorCode::InvalidState,
        ErrorCode::WaitCycle,
        ErrorCode::SaveFailed,
        ErrorCode::OutputExpired,
    ] {
        let response =
            Response::<Value>::failure("receipt", ApiError::new(code, "anonymous failure"));
        assert!(response.error.unwrap().hint.is_some());
    }
    let error = ApiError::new(ErrorCode::CursorExpired, "cursor gone")
        .hint("specific target hint")
        .with_recovery_hint();
    assert_eq!(error.hint.as_deref(), Some("specific target hint"));
    let error = ApiError::new(ErrorCode::OperationNotFound, "receipt gone").with_recovery_hint();
    assert!(error.hint.unwrap().contains("does not prove"));
}

async fn owner(root: &std::path::Path) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config, "").unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}

#[tokio::test]
async fn connection_loss_preserves_auto_operation_id_and_does_not_resend() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, mut receive) = tokio::sync::mpsc::channel(4);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let directory = owner.loaded.data_dir.clone();
    let client = tokio::spawn(async move {
        ipc::call(
            &directory,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some("synthetic-thread".into()),
                text: "one instruction".into(),
            }),
            None,
            None,
        )
        .await
    });
    let invocation = bounded(receive.recv()).await.unwrap();
    let id = invocation.request.operation_id.clone();
    drop(invocation);
    let error = bounded(client).await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.details.as_ref().unwrap()["operation_id"], id);
    assert_eq!(
        error.details.as_ref().unwrap()["next_operation"]["command"],
        "operation.get"
    );
    assert!(
        matches!(serde_json::from_value::<Command>(error.details.as_ref().unwrap()["next_operation"].clone()).unwrap(),Command::OperationGet(target) if target.operation_id==id)
    );
    assert_eq!(
        error.details.as_ref().unwrap()["request_may_have_been_applied"],
        true
    );
    assert!(error.hint.unwrap().contains("do not resubmit"));
    assert!(receive.try_recv().is_err());
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_status_reply_guides_a_fresh_read_without_a_nonexistent_receipt() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, mut receive) = tokio::sync::mpsc::channel(4);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let directory = owner.loaded.data_dir.clone();
    let client = tokio::spawn(async move {
        ipc::call(&directory, Command::Status(Default::default()), None, None).await
    });
    let invocation = bounded(receive.recv()).await.unwrap();
    drop(invocation);
    let error = bounded(client).await.unwrap().unwrap_err();
    assert!(error.hint.unwrap().contains("Repeat shellboy status"));
    let details = error.details.unwrap();
    assert_eq!(details["read_only"], true);
    assert_eq!(details["request_may_have_been_applied"], false);
    assert!(details.get("next_operation").is_none());
    assert!(receive.try_recv().is_err());
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mismatched_reply_keeps_the_requested_id_for_receipt_lookup() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, mut receive) = tokio::sync::mpsc::channel(4);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let directory = owner.loaded.data_dir.clone();
    let id = format!("{}:explicit-not-a-command-$()", owner.instance_id);
    let request_id = id.clone();
    let client = tokio::spawn(async move {
        ipc::call(
            &directory,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some("synthetic-thread".into()),
                text: "one instruction".into(),
            }),
            Some(request_id),
            None,
        )
        .await
    });
    let invocation = bounded(receive.recv()).await.unwrap();
    invocation
        .reply
        .send(Response::success("unrelated", json!({})))
        .unwrap();
    let error = bounded(client).await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::ProtocolMismatch);
    assert_eq!(error.details.as_ref().unwrap()["operation_id"], id);
    assert!(!error.hint.unwrap().contains("$()"));
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_json_exposes_generated_operation_after_unknown_result() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, mut receive) = tokio::sync::mpsc::channel(4);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let directory = owner.loaded.data_dir.clone();
    let client = tokio::spawn(async move {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .kill_on_drop(true)
            .arg("--data-dir")
            .arg(directory)
            .args([
                "message",
                "send",
                "--thread",
                "synthetic-thread",
                "--text",
                "one instruction",
            ])
            .output()
            .await
            .unwrap()
    });
    let invocation = bounded(receive.recv()).await.unwrap();
    let id = invocation.request.operation_id.clone();
    drop(invocation);
    let output = bounded(client).await.unwrap();
    assert!(!output.status.success());
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["operation_id"], id);
    assert_eq!(reply["error"]["details"]["operation_id"], id);
    assert!(receive.try_recv().is_err());
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_shutdown_preserves_wait_children_shell_and_recovery_context_after_restart() {
    use shellboy::{
        control::Control,
        protocol::{Request, WaitArgs, WaitCondition},
    };
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    owner.db.call(|db| {
        let tx=db.transaction()?;
        for (thread,run,turn,parent,state) in [("parent","r-parent","u-parent",None,"running"),("child","r-child","u-child",Some("r-parent"),"running"),("recovering","r-recovery","u-recovery",None,"recovery_wait")] {
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/AGENTS.md','','','now')",[thread])?;
            tx.execute("INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES(?1,?2,?3,'live','now')",rusqlite::params![run,thread,parent])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,recovery_reason_json,created_at)VALUES(?1,?2,?3,?4,?5,'now')",rusqlite::params![turn,thread,run,state,if state=="recovery_wait" {Some(r#"{"code":"config_invalid","message":"fixture recovery reason"}"#)}else{None}])?;
        }
        // Durable shell metadata is fixture data; no fake PID/OS process is inspected.
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('s-parent','r-parent','u-parent','shell-op','running','/','{}','UTF-8','UTF-8','open','now')",[])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    let operation = format!("{}:shutdown-observation", owner.instance_id);
    let wait = tokio::spawn({
        let control = control.clone();
        let operation = operation.clone();
        async move {
            control
                .request(Request::new(
                    operation,
                    Command::Wait(WaitArgs {
                        conditions: vec![WaitCondition::RunEnded {
                            run_id: "r-parent".into(),
                        }],
                        timeout_ms: 30000,
                    }),
                ))
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let key = format!("wait.pending.{operation}");
            if owner
                .db
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                        [key],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    control.shutdown().await.unwrap();
    assert!(wait.await.unwrap().ok);
    owner.shutdown().await.unwrap();
    let restarted = self::owner(root.path()).await;
    let contexts = restarted.db.call(|db| {
        let rows=db.prepare("SELECT payload_ref_json FROM event WHERE kind='context_changed' ORDER BY target_thread_id")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        assert_eq!(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM run WHERE state IN ('live','stopping')",[],|r|r.get::<_,i64>(0))?,0);
        Ok(rows)
    }).await.unwrap();
    restarted.shutdown().await.unwrap();
    assert_eq!(
        contexts.len(),
        3,
        "each pre-stop Thread context survives exactly once: {contexts:?}"
    );
    let contexts: Vec<Value> = contexts
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    let parent = contexts
        .iter()
        .find(|v| v["change"]["runs"]["items"][0]["run_id"] == "r-parent")
        .unwrap();
    assert_eq!(parent["change"]["reason"], "app_shutdown");
    assert_eq!(parent["change"]["cancelled_waits"]["count"], 1);
    assert_eq!(
        parent["change"]["cancelled_waits"]["items"][0]["operation_id"],
        operation
    );
    assert_eq!(
        parent["change"]["owned_children"]["items"][0]["run_id"],
        "r-child"
    );
    assert_eq!(
        parent["change"]["shells"]["items"][0]["execution_id"],
        "s-parent"
    );
    assert!(!parent.to_string().contains("previous_instance_interrupted"));
    let recovery = contexts
        .iter()
        .find(|v| v["change"]["runs"]["items"][0]["run_id"] == "r-recovery")
        .unwrap();
    assert_eq!(
        recovery["change"]["turns"]["items"][0]["recovery_reason"]["message"],
        "fixture recovery reason"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_context_save_failure_is_cached_but_wait_and_run_still_drain() {
    use shellboy::{
        control::Control,
        protocol::{ErrorCode, Request, WaitArgs, WaitCondition},
    };
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','','','now'); INSERT INTO run(id,thread_id,state,stop_reason,created_at)VALUES('r','t','live','explicit_stop','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now'); CREATE TRIGGER reject_shutdown_context BEFORE INSERT ON node WHEN NEW.kind='interruption_context' BEGIN SELECT RAISE(ABORT,'fixture capture failed'); END;")?;Ok(())}).await.unwrap();
    let operation = format!("{}:failed-context-wait", owner.instance_id);
    let wait = tokio::spawn({
        let control = control.clone();
        let operation = operation.clone();
        async move {
            control
                .request(Request::new(
                    operation,
                    Command::Wait(WaitArgs {
                        conditions: vec![WaitCondition::RunEnded { run_id: "r".into() }],
                        timeout_ms: 30000,
                    }),
                ))
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let key = format!("wait.pending.{operation}");
            if owner
                .db
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                        [key],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let failure = control.prepare_shutdown().await.unwrap_err();
    assert_eq!(failure.code, ErrorCode::SaveFailed);
    owner
        .db
        .call(|db| {
            db.execute_batch("DROP TRIGGER reject_shutdown_context")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(control.prepare_shutdown().await.unwrap_err()).unwrap(),
        serde_json::to_value(&failure).unwrap(),
        "the failed capture is not retried after cancellation removed its evidence"
    );
    let shutdown = tokio::time::timeout(std::time::Duration::from_secs(5), control.shutdown())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        serde_json::to_value(shutdown).unwrap(),
        serde_json::to_value(failure).unwrap()
    );
    assert!(wait.await.unwrap().ok);
    owner.db.call(|db|{
        assert_eq!(db.query_row("SELECT state,stop_reason FROM run WHERE id='r'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?,("stopped".into(),"explicit_stop".into()));
        assert_eq!(db.query_row("SELECT COUNT(*) FROM node WHERE kind IN ('interruption_context','interruption_details','interrupted_wait')",[],|r|r.get::<_,i64>(0))?,0,"all source nodes roll back with capture failure");
        assert_eq!(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",[],|r|r.get::<_,i64>(0))?,0);Ok(())
    }).await.unwrap();
    owner.shutdown().await.unwrap();
}
