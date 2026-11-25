use super::{UiAction, UiState};
use crate::{cli::Cli, config::Config, control::db::Store, protocol::*, storage::DbWorker};
use clap::Parser;
use std::sync::Arc;

#[tokio::test]
async fn ask_slash_dispatches_the_cli_ask_command() {
    for input in [
        "--thread recipient --deliver complete --sender-label 'Test caller' --text 'Choose A or B'",
        "--thread recipient --text 'Choose A or B'",
        "--to fixture --text 'Choose A or B'",
        "--text 'Choose A or B'",
    ] {
        let mut argv = vec!["shellboy".into(), "message".into(), "ask".into()];
        argv.extend(shlex::split(input).unwrap());
        let cwd = std::env::temp_dir();
        let expected = Cli::try_parse_from(argv)
            .unwrap()
            .into_command(&cwd)
            .await
            .unwrap()
            .unwrap();
        let mut ui = UiState::new(&Config::default(), cwd);
        let actions = ui.slash(&format!("/ask {input}"));
        let [UiAction::Manage(actual)] = actions.as_slice() else {
            panic!("{input}: {}", ui.view().status)
        };
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
}

#[test]
fn ask_slash_refuses_send_flags_and_piped_input_without_dispatch() {
    for input in [
        "--thread recipient --text question --max-model-requests 1",
        "--thread recipient --text question --resume-recovery",
        "--thread recipient --to fixture --text question",
        "--to fixture --deliver now --text question",
        "--thread recipient --text ''",
        "--thread recipient --text 'unclosed",
        "--thread recipient --text-stdin",
    ] {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        assert!(ui.slash(&format!("/ask {input}")).is_empty(), "{input}");
        assert!(ui.command_rejected, "{input}");
        if input.ends_with("--text-stdin") {
            assert!(
                ui.view()
                    .status
                    .contains("shellboy message ask --text-stdin")
            );
        }
    }
}

#[tokio::test]
async fn thread_ask_slash_opens_one_duty_and_send_opens_none() {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("reply.db")).await.unwrap();
    db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('recipient','/','/a','p','m','0');")?;
        Ok(())
    }).await.unwrap();
    let store = Store::new(
        db,
        Arc::new(Config::default()),
        root.path().into(),
        "fixture".into(),
    );
    let request = |id: &str, command| Request::new(format!("fixture:{id}"), command);
    let count = || {
        store.database.call(|db| {
            Ok(
                db.query_row("SELECT count(*) FROM pending_replies", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
    };
    let mut ui = UiState::new(&Config::default(), root.path().into());
    let send = Command::MessageSend(MessageSendArgs {
        thread_id: Some("recipient".into()),
        sender_label: None,
        human: false,
        text: "Ordinary message".into(),
        deliver: Deliver::Next,
        max_model_requests: None,
        resume_recovery: false,
    });
    store.execute(request("send", send), None).await.unwrap();
    assert_eq!(count().await.unwrap(), 0);
    let actions = ui.slash("/ask --to claude --text 'Named question'");
    let [UiAction::Manage(named)] = actions.as_slice() else {
        panic!("expected named Ask")
    };
    let error = store
        .execute(request("named", named.clone()), None)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "named questions require the current model Thread"
    );
    assert_eq!(count().await.unwrap(), 0);
    let actions = ui.slash("/ask --thread recipient --text 'Thread question'");
    let [UiAction::Manage(ask)] = actions.as_slice() else {
        panic!("expected Thread Ask")
    };
    store
        .execute(request("ask", ask.clone()), None)
        .await
        .unwrap();
    assert_eq!(count().await.unwrap(), 1);
}
