use super::{UiAction, UiState};
use crate::{
    cli::Cli, client_operation::ClientOperation, config::Config, config_setup::ConfigCommand,
};
use clap::Parser;

#[tokio::test]
async fn config_slash_get_set_dispatch_the_cli_commands() {
    let cwd = std::env::temp_dir();
    for text in [
        "config get",
        r#"config set --expected-hash aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --set 'default_model=fixture model' --set ui.bell=false --unset 'connections."my.proxy".default_effort'"#,
    ] {
        let words = shlex::split(text).unwrap();
        let cli = Cli::try_parse_from(
            std::iter::once("shellboy").chain(words.iter().map(String::as_str)),
        )
        .unwrap();
        let expected = cli.into_command(&cwd).await.unwrap().unwrap();
        let mut ui = UiState::new(&Config::default(), cwd.clone());
        let actions = ui.slash(&format!("/{text}"));
        let [UiAction::Manage(actual)] = actions.as_slice() else {
            panic!("{text} did not dispatch: {}", ui.view().status);
        };
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
}

#[test]
fn config_slash_check_dispatches_existing_client_check() {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    let actions = ui.slash("/config check");
    assert!(
        matches!(actions.as_slice(), [UiAction::ClientOperation(operation)] if matches!(operation.as_ref(), ClientOperation::Config(ConfigCommand::Check)))
    );
}

#[test]
fn config_slash_rejects_invalid_input_without_dispatch() {
    for text in [
        "/config init",
        "/config get --data-dir other-owner",
        "/config check --config other-file",
        "/config set --set ui.bell=false",
        "/config set --expected-hash invalid --set ui.bell=false",
        "/config set --expected-hash hash --set ui.bell",
        "/config set --expected-hash hash --set 'ui.bell=false",
    ] {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        assert!(ui.slash(text).is_empty(), "{text}");
        assert!(ui.command_rejected, "{text}");
    }
}
