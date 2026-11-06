//! Capture only immutable page identities; body retention stays with management.
use crate::{
    protocol::Command,
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
#[derive(serde::Serialize)]
#[serde(untagged)]
enum Original {
    Body(Box<crate::protocol::HistoryBody>),
    List(crate::protocol::Page<crate::protocol::HistorySnippet>),
}
pub(super) async fn request(
    control: &std::sync::Arc<crate::control::Control>,
    command: Command,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Value, crate::protocol::ApiError> {
    command.validate()?;
    let original = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop,
        move |db| match command {
            Command::HistoryGet(args) => crate::history::get_node(db, &args)
                .map(|body| Original::Body(Box::new(body)))
                .map_err(Into::into),
            Command::HistoryList(args) => crate::history::list_nodes(db, &args)
                .map(Original::List)
                .map_err(Into::into),
            _ => Err(crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::InvalidArgument,
                "Not a retained history read",
            )
            .into()),
        },
    )
    .await?;
    super::presentation_work(move || {
        serde_json::to_value(original).map_err(|_| {
            crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::IoFailed,
                "Cannot prepare retained history result",
            )
        })
    })
    .await?
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
    use crate::protocol::{HistoryGetArgs, HistoryListArgs};
    use serde_json::json;
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
