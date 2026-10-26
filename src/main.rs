use clap::{CommandFactory, Parser};
use shellboy::{
    cli::{Cli, CliCommand},
    config::{PathEnvironment, resolve_cli_data_dir},
    ipc,
    protocol::{ApiError, ErrorCode, Origin, Response},
};
use std::{io::IsTerminal, path::Path, process::ExitCode};

fn print_error(error: ApiError, operation: Option<String>) -> ExitCode {
    let operation = operation.filter(|id| !id.trim().is_empty()).or_else(|| {
        error
            .details
            .as_ref()?
            .get("operation_id")?
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
    });
    // Before handshake an owner operation may not exist. The CLI contract
    // allows omitting its ID; owner wire responses still require a real ID.
    let known_operation = operation.is_some();
    let response = Response::<serde_json::Value>::failure(operation.unwrap_or_default(), error);
    let mut value = serde_json::to_value(response).expect("error JSON");
    if !known_operation {
        value
            .as_object_mut()
            .expect("error object")
            .remove("operation_id");
    }
    println!("{}", serde_json::to_string(&value).expect("error JSON"));
    ExitCode::FAILURE
}
fn cli_parse_error(error: &clap::Error) -> ApiError {
    use clap::error::{ContextKind, ContextValue};
    use std::collections::BTreeSet;
    fn names(command: &clap::Command, output: &mut BTreeSet<String>) {
        for arg in command.get_arguments() {
            if let Some(long) = arg.get_long() {
                output.insert(format!("--{long}"));
            }
            if let Some(short) = arg.get_short() {
                output.insert(format!("-{short}"));
            }
        }
        for child in command.get_subcommands() {
            names(child, output);
        }
    }
    let mut known = BTreeSet::new();
    names(&Cli::command(), &mut known);
    let candidates: Vec<&str> = match error.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(value)) => vec![value.as_str()],
        Some(ContextValue::Strings(values)) => values.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    let arguments: BTreeSet<&str> = candidates
        .iter()
        .filter_map(|value| value.split_whitespace().next())
        .filter(|name| known.contains(*name))
        .collect();
    // Value-parser errors do not always carry command-specific usage. Fall
    // back to the declared root command, never to a message containing input.
    let usage = error
        .get(ContextKind::Usage)
        .map(ToString::to_string)
        .or_else(|| Some(Cli::command().render_usage().to_string()));
    let kind = format!("{:?}", error.kind());
    let hint = match &usage {
        Some(usage) => format!("{usage}\nUse shellboy <command> --help for available arguments"),
        None => "Run shellboy --help or shellboy <command> --help for syntax".into(),
    };
    ApiError::new(
        ErrorCode::InvalidArgument,
        format!("Invalid command line ({kind})"),
    )
    .hint(hint)
    .details(serde_json::json!({"parser_kind":kind,"arguments":arguments,"usage":usage}))
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
        Err(error) => return print_error(cli_parse_error(&error), None),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(value) => value,
        Err(_) => {
            return print_error(
                ApiError::new(ErrorCode::IoFailed, "Cannot start async runtime"),
                cli.operation_id,
            );
        }
    };
    let cwd = if cli.command.is_none() {
        std::path::PathBuf::new()
    } else {
        match std::env::current_dir() {
            Ok(value) => value,
            Err(_) => {
                return print_error(
                    ApiError::new(ErrorCode::IoFailed, "Cannot determine startup cwd"),
                    cli.operation_id,
                );
            }
        }
    };
    let mut cli = cli;
    if cli.operation_id.is_some()
        && matches!(
            cli.command,
            Some(CliCommand::Docs { .. } | CliCommand::Rag(_))
        )
    {
        return print_error(
            ApiError::new(
                ErrorCode::InvalidArgument,
                "--operation-id applies to owner RPCs; docs and rag are standalone commands",
            ),
            cli.operation_id,
        );
    }
    match cli.command.take() {
        Some(CliCommand::Docs { command }) => standalone_response(docs_response(command)),
        Some(CliCommand::Rag(options)) => standalone_response(runtime.block_on(
            shellboy::rag::execute(options, cli.config, cli.data_dir, &cwd),
        )),
        command @ Some(_) => {
            cli.command = command;
            runtime.block_on(run_owner(cli, &cwd))
        }
        None => run_tui(cli, runtime),
    }
}
fn docs_response(command: shellboy::docs::DocsCommand) -> serde_json::Value {
    match shellboy::docs::execute(command) {
        Ok(result) => {
            serde_json::json!({"protocol_version":shellboy::protocol::PROTOCOL_VERSION,"ok":true,"result":result})
        }
        Err(message) => {
            let (code, detail) = message
                .split_once(": ")
                .unwrap_or(("invalid_argument", &message));
            serde_json::json!({"protocol_version":shellboy::protocol::PROTOCOL_VERSION,"ok":false,"error":{"code":code,"message":detail}})
        }
    }
}
fn standalone_response(response: serde_json::Value) -> ExitCode {
    println!(
        "{}",
        serde_json::to_string(&response).expect("local response JSON")
    );
    if response["ok"].as_bool() == Some(true) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
async fn run_owner(cli: Cli, cwd: &Path) -> ExitCode {
    let result = async {
        let command = cli
            .clone()
            .into_command(cwd)
            .await?
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidArgument, "Missing command"))?;
        if matches!(&cli.command, Some(CliCommand::Status(options)) if options.operation_id.is_some()) {
            eprintln!("status --operation is deprecated and will be removed after v0.1.2; use shellboy operation get --operation ID");
        }
        let data_dir =
            resolve_cli_data_dir(cli.data_dir.as_deref(), &PathEnvironment::current(), cwd)
                .map_err(|e| ApiError::new(ErrorCode::ConfigInvalid, e.to_string()))?;
        ipc::call(
            &data_dir,
            command,
            cli.operation_id.clone(),
            origin(&data_dir),
        )
        .await
    }
    .await;
    match result {
        Ok(response) => {
            let code = if response.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
            println!(
                "{}",
                serde_json::to_string(&response).expect("response JSON")
            );
            code
        }
        Err(error) => print_error(error, cli.operation_id),
    }
}
fn run_tui(cli: Cli, runtime: tokio::runtime::Runtime) -> ExitCode {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return print_error(
            ApiError::new(
                ErrorCode::InvalidState,
                "The TUI needs an interactive terminal",
            )
            .hint("Use a subcommand to connect to an already running owner"),
            None,
        );
    }
    let mut session = match shellboy::ui::TerminalSession::enter() {
        Ok(session) => session,
        Err(error) => {
            eprintln!("Terminal error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let (progress, stages) =
        tokio::sync::watch::channel(shellboy::app::StartupStage::Configuration);
    let (send, mut pending) = std::sync::mpsc::sync_channel(1);
    let token = cancel.clone();
    let observations = progress.clone();
    let mut initializer = runtime.spawn(async move {
        let result =
            shellboy::frontend::startup::initialize(cli.config, cli.data_dir, token, observations)
                .await;
        shellboy::frontend::startup::deliver(result, send).await;
    });
    let prepared = loop {
        match shellboy::ui::startup::wait(&mut session, &stages, &pending, &cancel) {
            Ok(Ok(Some(prepared))) if cancel.is_cancelled() => {
                // Cancellation may race the single ready delivery. Never
                // hand that owner to the conversation UI; keep drawing cleanup.
                progress.send_replace(shellboy::app::StartupStage::Closing);
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                pending = receive;
                initializer = runtime.spawn(async move {
                    let result = prepared.shutdown().await.map(|_| None);
                    shellboy::frontend::startup::deliver(result, send).await;
                });
            }
            Ok(result) => break result,
            Err(error) => {
                // The terminal UI has ended before waiting for finalization.
                drop(session);
                cancel.cancel();
                runtime.block_on(async {
                    let _ = initializer.await;
                    if let Ok(Ok(Some(prepared))) = pending.try_recv() {
                        let _ = prepared.shutdown().await;
                    }
                });
                eprintln!("Terminal error: {error}");
                runtime.shutdown_background();
                return ExitCode::FAILURE;
            }
        }
    };
    let prepared = match prepared {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            drop(session);
            runtime.shutdown_background();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            drop(session);
            runtime.shutdown_background();
            return print_error(error, None);
        }
    };
    let shellboy::frontend::startup::Prepared {
        owner,
        control,
        server,
        requests: rpc_receive,
        cwd,
    } = prepared;
    let config = owner.loaded.config.clone();
    runtime.spawn(control.clone().serve(rpc_receive));
    let (actions_send, actions_receive) = shellboy::ui::actions::channel(128);
    let (updates_send, updates_receive) = std::sync::mpsc::sync_channel(128);
    let bridge = runtime.spawn(shellboy::frontend::run(
        control,
        actions_receive,
        updates_send,
        cwd,
        owner,
        server,
    ));
    let terminal_result =
        shellboy::ui::run_in_session(&mut session, &config, actions_send.clone(), updates_receive);
    drop(session);
    if matches!(&terminal_result, Ok(shellboy::ui::UiExitReason::Forced)) {
        eprintln!(
            "Forced exit: conversation saving and owned process cleanup may be incomplete. Restart to inspect interrupted work."
        );
        std::process::exit(1);
    }
    if terminal_result.is_err() {
        let _ = actions_send.try_send(shellboy::ui::UiAction::Quit);
    }
    let result = runtime.block_on(bridge);
    if let Err(error) = terminal_result {
        eprintln!("Terminal error: {error}");
        runtime.shutdown_background();
        return ExitCode::FAILURE;
    }
    let exit_code = match result {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(error)) => {
            eprintln!("Shutdown error: {error}");
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("Runtime adapter failed");
            ExitCode::FAILURE
        }
    };
    runtime.shutdown_background();
    exit_code
}

fn origin(data_dir: &Path) -> Option<Origin> {
    if std::env::var_os("SHELLBOY_DATA_DIR")
        .as_deref()
        .map(Path::new)
        != Some(data_dir)
    {
        return None;
    }
    Some(Origin {
        instance_id: std::env::var("SHELLBOY_INSTANCE_ID").ok()?,
        thread_id: std::env::var("SHELLBOY_THREAD_ID").ok()?,
        run_id: std::env::var("SHELLBOY_RUN_ID").ok()?,
        turn_id: std::env::var("SHELLBOY_TURN_ID").ok()?,
        execution_id: std::env::var("SHELLBOY_EXECUTION_ID").ok()?,
        token: std::env::var("SHELLBOY_ORIGIN_TOKEN").ok()?,
    })
}
