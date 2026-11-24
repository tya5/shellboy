//! Capture only immutable page identities; body retention stays with management.
use crate::{
    protocol::{ApiError, Command, ErrorCode, HistoryBody, HistoryGetArgs, HistorySnippet, Page},
    ui::{HistorySourceScope, InspectionTarget},
};
use serde_json::Value;
pub(super) enum CapturedCommand {
    Full(Box<Command>),
    Limited(super::inspection_retained::RetainedRoute),
}
pub(super) fn capture_command(command: &Command) -> Option<CapturedCommand> {
    if !(matches!(command, Command::HistoryGet(_) | Command::HistoryList(_))
        || super::inspection_retained::supports(command))
    {
        return None;
    }
    // Count borrowed JSON before cloning the request's query/filter strings.
    Some(if super::inspection_retained::request_fits(command) {
        CapturedCommand::Full(Box::new(command.clone()))
    } else {
        CapturedCommand::Limited(super::inspection_retained::route(command))
    })
}
pub(super) fn capture_saved(command: &CapturedCommand, value: &Value) -> Option<InspectionTarget> {
    match command {
        CapturedCommand::Full(command) => capture(command, value),
        CapturedCommand::Limited(route) if value.get("error").is_none() => {
            Some(super::inspection_retained::limited_scope(*route))
        }
        _ => None,
    }
}
pub(super) async fn native_body(
    control: &crate::control::Control,
    args: HistoryGetArgs,
) -> Result<HistoryBody, ApiError> {
    native_read(control, Command::HistoryGet(args)).await
}

async fn native_read<T: serde::de::DeserializeOwned>(
    control: &crate::control::Control,
    command: Command,
) -> Result<T, ApiError> {
    let data_dir = control
        .store
        .database
        .path()
        .parent()
        .expect("owner database has a directory");
    let response = crate::ipc::call_selected(data_dir, command).await?;
    let malformed = || ApiError::new(ErrorCode::ProtocolMismatch, "Invalid native history response");
    if !response.ok {
        return Err(response.error.unwrap_or_else(malformed));
    }
    serde_json::from_value(response.result.ok_or_else(malformed)?).map_err(|_| malformed())
}

pub(super) async fn request(
    control: &std::sync::Arc<crate::control::Control>,
    command: Command,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Value, crate::protocol::ApiError> {
    tokio::select! { biased;
        _ = stop.cancelled() => Err(ApiError::new(ErrorCode::InvalidState, "Display snapshot observation closed")),
        result = async {
            match command {
                Command::HistoryGet(args) => {
                    let body = native_body(control, args).await?;
                    super::presentation_work(move || serialize(body)).await?
                }
                command @ Command::HistoryList(_) => {
                    let page: Page<HistorySnippet> = native_read(control, command).await?;
                    super::presentation_work(move || serialize(page)).await?
                }
                _ => Err(ApiError::new(ErrorCode::InvalidArgument, "Not a retained history read")),
            }
        } => result,
    }
}

fn serialize(value: impl serde::Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|_| {
        ApiError::new(
            ErrorCode::IoFailed,
            "Cannot prepare retained history result",
        )
    })
}
pub(super) fn capture(command: &Command, value: &Value) -> Option<InspectionTarget> {
    if let Some(target) = super::inspection_retained::capture_response(command, value) {
        return Some(target);
    }
    let (rows, thread) = match command {
        Command::HistoryGet(_) => (vec![value], None),
        Command::HistoryList(args) => (
            value.get("items")?.as_array()?.iter().take(201).collect(),
            Some(args.thread_id.as_str()),
        ),
        _ => return None,
    };
    let mut limited = rows.len() > 200 || thread.is_some_and(|id| id.is_empty() || id.len() > 512);
    let mut nodes = Vec::new();
    for row in rows {
        let id = row.get("node_id")?.as_str()?;
        if id.is_empty() || id.len() > 512 {
            limited = true;
            break;
        }
        nodes.push((
            id.to_owned(),
            row.pointer("/provenance/source_revision")
                .and_then(Value::as_i64),
        ));
    }
    if limited {
        nodes.clear();
    }
    Some(InspectionTarget::HistorySources(HistorySourceScope {
        nodes,
        thread_id: thread.filter(|_| !limited).map(str::to_owned),
        captured_at: chrono::Utc::now().to_rfc3339(),
        scope_limited: limited,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::HistoryListArgs;
    use serde_json::json;
    #[tokio::test]
    async fn native_body_inspection_continues_and_rejects_changed_revision() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                crate::history::append_node(
                    &tx,
                    "thread",
                    "inspection-source",
                    None,
                    "user",
                    &json!({"role":"user","content":"abcdefgh"}),
                    "2026-11-24T00:00:00Z",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let read = |cursor| {
            request(
                &control,
                Command::HistoryGet(HistoryGetArgs {
                    node_id: "inspection-source".into(),
                    cursor,
                    limit_bytes: 4,
                    encoding: "UTF-8".into(),
                    end_message_only: false,
                    parts: false,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
        };
        let first = read(None).await.unwrap();
        assert_eq!(first["text"], "abcd");
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        let second = read(Some(cursor.clone())).await.unwrap();
        assert_eq!(second["text"], "efgh");
        assert!(second.get("next_cursor").is_none());
        owner.db.call(|db| {
            db.execute("UPDATE node SET payload_revision=payload_revision+1 WHERE id='inspection-source'", [])?;
            Ok(())
        }).await.unwrap();
        assert_eq!(
            read(Some(cursor)).await.unwrap_err().code,
            ErrorCode::CursorExpired
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn body_identity_never_infers_selection_from_creation_or_viewer_and_limits_do_not_copy_bad_ids()
    {
        let command = Command::HistoryGet(HistoryGetArgs {
            node_id: "n".into(),
            cursor: None,
            limit_bytes: 65536,
            encoding: "UTF-8".into(),
            end_message_only: false,
            parts: false,
        });
        let InspectionTarget::HistorySources(scope)=capture(&command,&json!({"node_id":"n","provenance":{"source_revision":7,"origin_thread_id":"creation"},"text":"retained"})).unwrap() else {panic!()};
        assert_eq!(scope.nodes, vec![("n".into(), Some(7))]);
        assert!(scope.thread_id.is_none());
        assert!(!scope.scope_limited);
        let InspectionTarget::HistorySources(scope) =
            capture(&command, &json!({"node_id":"x".repeat(513)})).unwrap()
        else {
            panic!()
        };
        assert!(scope.scope_limited && scope.nodes.is_empty());
        assert!(capture(&command, &json!({"error":{"message":"absent"}})).is_none());
    }
    #[test]
    fn empty_list_still_captures_its_literal_thread_without_a_fabricated_revision() {
        let command = Command::HistoryList(HistoryListArgs {
            thread_id: "foreign".into(),
            excerpt_chars: 512,
            limit: 50,
            cursor: None,
            run_id: None,
            since: None,
            state: None,
        });
        let InspectionTarget::HistorySources(scope) =
            capture(&command, &json!({"items":[]})).unwrap()
        else {
            panic!()
        };
        assert_eq!(scope.thread_id.as_deref(), Some("foreign"));
        assert!(scope.nodes.is_empty());
    }
}
