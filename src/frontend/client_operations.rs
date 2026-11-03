//! Typed standalone operations on existing workers, never terminal input or subprocesses.
use crate::{
    client_operation::ClientOperation,
    protocol::{ApiError, ErrorCode},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct ClientContext {
    pub config_path: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub cwd: PathBuf,
}
fn envelope(result: Result<Value, ApiError>) -> Value {
    match result {
        Ok(value) => {
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":true,"result":value})
        }
        Err(error) => {
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"error":error})
        }
    }
}
fn docs(command: crate::docs::DocsCommand) -> Value {
    match crate::docs::execute(command) {
        Ok(value) => {
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":true,"result":value})
        }
        Err(message) => {
            let (code, detail) = message
                .split_once(": ")
                .unwrap_or(("invalid_argument", &message));
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"error":{"code":code,"message":detail}})
        }
    }
}
fn exec_response(outcome: crate::exec_client::Outcome) -> Value {
    json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":outcome.code==0,"exit_code":outcome.code,"result":outcome.result,"diagnostics":outcome.diagnostics})
}
fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        cwd.join(path)
    }
}
fn exec_options(
    mut options: crate::exec_client::ExecOptions,
    cwd: &Path,
) -> Result<crate::exec_client::ExecOptions, crate::exec_client::Outcome> {
    if options.text_stdin {
        return Err(crate::exec_client::Outcome::input(
            "The UI never reads terminal stdin; enter the work request in the form",
        ));
    }
    if options.text.is_none() {
        return Err(crate::exec_client::Outcome::input(
            "Enter a nonempty work request in the form",
        ));
    }
    options.schema = absolute(&options.schema, cwd);
    options.create.cwd = options.create.cwd.map(|path| absolute(&path, cwd));
    let effective = options.create.cwd.as_deref().unwrap_or(cwd);
    options.create.agents = options.create.agents.map(|path| absolute(&path, effective));
    Ok(options)
}
/// Immutable context comes from the submitted form, not process cwd changes or
/// model-origin environment variables. Existing clients own their completion.
pub(super) async fn execute(
    operation: ClientOperation,
    context: ClientContext,
) -> Result<Value, ApiError> {
    if let ClientOperation::Docs(command) = operation {
        return super::presentation_work(move || docs(command)).await;
    }
    if !context.cwd.is_absolute() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Client operation requires an explicit absolute cwd",
        ));
    }
    let data = absolute(&context.data_dir, &context.cwd);
    match operation {
        ClientOperation::Docs(_) => unreachable!("documentation handled without path I/O"),
        ClientOperation::Config(command) => Ok(envelope(
            crate::config_setup::execute(command, context.config_path, Some(data), &context.cwd)
                .await,
        )),
        ClientOperation::Rag {
            options,
            owned_stdin,
        } => Ok(crate::rag::execute_with_text(
            options,
            context.config_path,
            Some(data),
            &context.cwd,
            owned_stdin,
        )
        .await),
        ClientOperation::Exec(options) => {
            let options = match exec_options(options, &context.cwd) {
                Ok(options) => options,
                Err(outcome) => return Ok(exec_response(outcome)),
            };
            // A UI action is not a model's authenticated shell origin. Exec keeps
            // its existing multi-RPC IDs and never accepts a single operation ID.
            Ok(exec_response(
                crate::exec_client::execute(options, &data, &context.cwd, None, None).await,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(root: &Path) -> ClientContext {
        ClientContext {
            config_path: Some(root.join("config.toml")),
            data_dir: root.join("data"),
            cwd: root.into(),
        }
    }
    fn exec() -> crate::exec_client::ExecOptions {
        crate::exec_client::ExecOptions {
            schema: "schema.json".into(),
            thread_id: None,
            create: crate::cli::ThreadCreateOptions {
                agents: Some("guide/AGENTS.md".into()),
                cwd: Some("work".into()),
                connection: None,
                model: None,
                effort: None,
                parent_id: None,
            },
            text: Some("request".into()),
            text_stdin: false,
            max_repairs: 0,
            timeout_ms: 100,
            sender_label: "UI fixture".into(),
        }
    }
    #[test]
    fn exec_paths_resolve_to_the_frozen_cwd_and_both_output_channels_survive() {
        let root = tempfile::tempdir().unwrap();
        let options = exec_options(exec(), root.path()).ok().unwrap();
        assert_eq!(options.schema, root.path().join("schema.json"));
        assert_eq!(options.create.cwd, Some(root.path().join("work")));
        assert_eq!(
            options.create.agents,
            Some(root.path().join("work/guide/AGENTS.md"))
        );
        let value = exec_response(crate::exec_client::Outcome {
            code: 5,
            result: Some("paid answer retained".into()),
            diagnostics: json!({"status":"validation_failed","end_message_node_id":"saved"}),
        });
        assert_eq!(value["exit_code"], 5);
        assert_eq!(value["result"], "paid answer retained");
        assert_eq!(value["diagnostics"]["end_message_node_id"], "saved");
        assert_eq!(value["ok"], false);
    }
    #[tokio::test]
    async fn docs_and_config_keep_failure_observable_without_terminal_output() {
        let root = tempfile::tempdir().unwrap();
        let value = execute(
            ClientOperation::Docs(crate::docs::DocsCommand::Get {
                id: "missing".into(),
                section: None,
                limit_bytes: 100,
                cursor: None,
            }),
            context(root.path()),
        )
        .await
        .unwrap();
        assert_eq!(value["ok"], false);
        assert!(value["error"]["message"].as_str().is_some());
        let value = execute(
            ClientOperation::Config(crate::config_setup::ConfigCommand::Init),
            context(root.path()),
        )
        .await
        .unwrap();
        assert_eq!(value["ok"], true);
        assert!(root.path().join("config.toml").is_file());
        let value = execute(
            ClientOperation::Config(crate::config_setup::ConfigCommand::Init),
            context(root.path()),
        )
        .await
        .unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["result"]["created"], false);
        let value = execute(
            ClientOperation::Config(crate::config_setup::ConfigCommand::Check),
            context(root.path()),
        )
        .await
        .unwrap();
        assert_eq!(value["ok"], false);
        assert!(value["error"].is_object());
    }
    #[tokio::test]
    async fn exec_terminal_stdin_is_rejected_before_schema_or_owner_access() {
        let root = tempfile::tempdir().unwrap();
        let mut options = exec();
        options.text_stdin = true;
        options.text = None;
        let value = execute(ClientOperation::Exec(options), context(root.path()))
            .await
            .unwrap();
        assert_eq!(value["exit_code"], 2);
        assert_eq!(value["diagnostics"]["status"], "input_error");
        assert!(
            value["diagnostics"]["message"]
                .as_str()
                .unwrap()
                .contains("never reads terminal stdin")
        );
        assert!(!root.path().join("data").exists());
    }
    #[tokio::test]
    async fn rag_failure_envelope_survives_client_dispatch_without_reading_stdin() {
        let root = tempfile::tempdir().unwrap();
        let options = crate::rag::RagOptions {
            db: Some(root.path().join("knowledge.db")),
            command: crate::rag::RagCommand::Ingest {
                id: "owned".into(),
                source_file: "source.bin".into(),
                kind: crate::rag::IngestKind::Summary,
                text_file: None,
                text_stdin: true,
                metadata_file: None,
            },
        };
        let value = execute(
            ClientOperation::Rag {
                options,
                owned_stdin: None,
            },
            context(root.path()),
        )
        .await
        .unwrap();
        assert_eq!(value["protocol_version"], crate::protocol::PROTOCOL_VERSION);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert!(!root.path().join("knowledge.db").exists());
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::ui::{ManagementContext, UiAction, UiUpdate, actions};

    async fn result(
        incoming: &mut tokio::sync::mpsc::Receiver<UiUpdate>,
        epoch: u64,
    ) -> Vec<crate::presentation::PresentationField> {
        crate::test_watchdog::watchdog("typed client scoped result", async {
            while let Some(update) = incoming.recv().await {
                if let UiUpdate::ScopedManagement { context, update } = update
                    && context.epoch == epoch
                {
                    let update = match *update {
                        UiUpdate::ExternalManagement { update, .. } => *update,
                        other => other,
                    };
                    if let UiUpdate::StructuredManagement { fields, .. } = update {
                        return fields;
                    }
                }
            }
            panic!("frontend closed before its scoped client result");
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typed_client_dispatch_preserves_scope_failure_and_frozen_startup_paths() {
        let (root, owner, control, server) = super::super::control_tests::fixture().await;
        let (send, receive) = actions::channel(32);
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        let (forward, mut observed) = tokio::sync::mpsc::channel(128);
        let forwarder = std::thread::spawn(move || {
            while let Ok(update) = incoming.recv() {
                if forward.blocking_send(update).is_err() {
                    break;
                }
            }
        });
        let mut bridge = tokio::spawn(super::super::run(
            control,
            receive,
            updates,
            root.path().into(),
            owner,
            server,
        ));
        let context = |epoch| ManagementContext {
            epoch,
            owner_draft: "client-test".into(),
            owner_thread: Some("thread".into()),
        };
        send.try_send(UiAction::ManagementIntent {
            context: context(1),
            action: Box::new(UiAction::ClientOperation(Box::new(ClientOperation::Docs(
                crate::docs::DocsCommand::Get {
                    id: "nonexistent-client-guide".into(),
                    section: None,
                    limit_bytes: 1024,
                    cursor: None,
                },
            )))),
        })
        .unwrap();
        let fields = result(&mut observed, 1).await;
        assert!(
            fields
                .iter()
                .any(|field| field.key == "/ok" && field.value == "false")
        );
        assert!(
            fields
                .iter()
                .any(|field| field.key == "/error/message" && !field.value.is_empty())
        );
        send.try_send(UiAction::ManagementIntent {
            context: context(2),
            action: Box::new(UiAction::ClientOperation(Box::new(ClientOperation::Rag {
                options: crate::rag::RagOptions {
                    db: Some("client-fixture.db".into()),
                    command: crate::rag::RagCommand::Init,
                },
                owned_stdin: None,
            }))),
        })
        .unwrap();
        let fields = result(&mut observed, 2).await;
        assert!(
            fields
                .iter()
                .any(|field| field.key == "/ok" && field.value == "true")
        );
        assert!(
            root.path().join("client-fixture.db").is_file(),
            "client paths use the captured startup cwd, independent of process cwd"
        );
        send.try_send(UiAction::Quit).unwrap();
        crate::test_watchdog::watchdog("typed client bridge shutdown", async {
            loop {
                tokio::select! {
                    result = &mut bridge => { result.unwrap().unwrap(); break; }
                    Some(_) = observed.recv() => {}
                }
            }
        })
        .await
        .unwrap();
        observed.close();
        tokio::task::spawn_blocking(move || forwarder.join().unwrap())
            .await
            .unwrap();
    }
}
