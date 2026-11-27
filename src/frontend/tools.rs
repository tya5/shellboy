//! UI-only source correlation. No model/core state or canonical node is changed.
use crate::{
    presentation::{BlockKind, ToolReasonKind},
    ui::UiNode,
};

pub(super) use crate::history::window_facts::tools::CallSource;
#[cfg(test)]
pub(super) use crate::history::window_facts::tools::*;
#[cfg(test)]
use crate::protocol::ErrorCode;
#[cfg(test)]
use crate::{control::Control, protocol::ApiError};
#[cfg(test)]
use rusqlite::Connection;

/// Clear current observations but keep the saved group that selects each call.
pub(super) fn reset_observations(nodes: &mut [UiNode]) {
    for tool in nodes
        .iter_mut()
        .flat_map(|node| &mut node.presentation.blocks)
        .filter_map(|block| block.tool.as_mut())
    {
        tool.clear_observation();
        tool.call_node_id = None;
        tool.result_node_id = None;
    }
}

pub(super) fn apply_window(nodes: &mut [UiNode], facts: &crate::protocol::HistoryWindowTools) {
    super::notices::apply(nodes, &facts.notices);
    apply_calls(nodes, &facts.calls, &facts.limited_node_ids, true);
    // The saved payload group only selects the exact call; like the former
    // readers, a group is shown only when the owner confirmed that call.
    for node in nodes {
        for tool in node
            .presentation
            .blocks
            .iter_mut()
            .filter_map(|block| block.tool.as_mut())
        {
            if !facts.calls.iter().any(|call| {
                call.node_id == node.node_id
                    && call.call_id == tool.call_id
                    && tool.group_id.as_deref() == Some(call.group_id.as_str())
            }) {
                tool.group_id = None;
            }
        }
    }
}

fn apply_calls(
    nodes: &mut [UiNode],
    calls: &[CallSource],
    limited_node_ids: &[String],
    require_group_match: bool,
) {
    for source in calls {
        let Some(node) = nodes.iter_mut().find(|node| node.node_id == source.node_id) else {
            continue;
        };
        let spilled = node.presentation.kind == crate::presentation::PresentationKind::Spill;
        for block in &mut node.presentation.blocks {
            let Some(tool) = &mut block.tool else {
                continue;
            };
            if tool.call_id != source.call_id
                || (require_group_match
                    && tool
                        .group_id
                        .as_ref()
                        .is_some_and(|group| group != &source.group_id))
            {
                continue;
            }
            tool.ordinal = Some(source.ordinal);
            tool.group_id = Some(source.group_id.clone());
            tool.call_node_id = source.call_node_id.clone();
            tool.result_node_id = source.result_node_id.clone();
            if spilled {
                // The visible, openable owner is the spill; originals stay
                // reachable through its source references.
                tool.call_node_id = Some(source.node_id.clone());
                tool.result_node_id = Some(source.node_id.clone());
            }
            if let Some(elapsed) = source.elapsed_ms {
                tool.elapsed_ms = Some(elapsed);
            }
            if source.shell_state.is_some() {
                tool.execution_id = source.execution_id.clone();
            }
            if tool.status == "cancelled"
                && tool.reason_kind == ToolReasonKind::PriorCallInterrupted
            {
                match ToolReasonKind::from_saved(source.prior_reason.as_deref().unwrap_or("")) {
                    ToolReasonKind::HumanBackgrounded => {
                        tool.reason = "先行する呼出しを利用者が背景へ移したため未実行".into()
                    }
                    ToolReasonKind::AdditionalInput => {
                        tool.reason = "追加の指示で先行する呼出しが背景へ移ったため未実行".into()
                    }
                    _ => {}
                }
            }
            tool.description = crate::tool_description::label(source.description.as_deref());
            if tool.command.is_empty() {
                tool.command =
                    crate::presentation::sanitize_plain(source.command.as_deref().unwrap_or(""), 0)
                        .0;
            }
            tool.current_state = source.shell_state.as_deref().map(|state| match state {
                "running" if source.released => "background_current".into(),
                "running" => "running_current".into(),
                other => other.into(),
            });
            tool.current_exit_code = source.shell_exit_code;
            tool.output_revision = source.output_revision;
            tool.started_at = source.started_at.clone();
            tool.elapsed_observation = source
                .started_at
                .as_deref()
                .and_then(|saved| chrono::DateTime::parse_from_rfc3339(saved).ok())
                .map(|start| {
                    let ms = chrono::Utc::now()
                        .signed_duration_since(start)
                        .num_milliseconds()
                        .max(0) as u64;
                    (
                        std::time::Duration::from_millis(ms),
                        std::time::Instant::now(),
                    )
                });
            tool.background = source.background
                || (source.released && source.shell_state.as_deref() == Some("running"));
            if block.kind == BlockKind::ToolCall {
                // A call has no saved shell-result code. The linked terminal
                // result means "result available"; its current code is separate.
                tool.status = if source.result_state == "exited" {
                    "completed".into()
                } else {
                    source.result_state.clone()
                };
            }
        }
    }
    for node in nodes {
        if limited_node_ids.iter().any(|id| id == &node.node_id) {
            node.presentation.metadata_limited = true;
            node.presentation.details_omitted = true;
        }
        node.presentation.bound_metadata();
    }
}

/// Resolve a saved source relation, never an execution borrowed by call ID alone.
#[cfg(test)]
pub(super) async fn preview_source(
    control: &Control,
    node: &str,
    group: Option<&str>,
    call: &str,
) -> Result<Option<String>, ApiError> {
    let (node, group, call) = (node.to_owned(), group.map(str::to_owned), call.to_owned());
    control
        .store
        .database
        .call(move |db| {
            use rusqlite::OptionalExtension;
            let source: Option<(String, Option<String>, Option<String>)> = db
                .query_row(SOURCE_SQL, rusqlite::params![node, call], |row| {
                    Ok((row.get(5)?, row.get(7)?, row.get(3)?))
                })
                .optional()?;
            let unavailable = || {
                crate::storage::StorageError::from(ApiError::new(
                    crate::protocol::ErrorCode::TargetUnavailable,
                    "Saved tool source is unavailable",
                ))
            };
            let Some((saved_group, execution, shell_state)) = source else {
                return Err(unavailable());
            };
            if group.as_deref().is_some_and(|group| group != saved_group) {
                return Err(unavailable());
            }
            if execution.is_some() && shell_state.is_none() {
                return Err(unavailable());
            }
            Ok(execution)
        })
        .await
        .map_err(|error| error.into_api_error())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::{app::DataOwner, presentation};
    use serde_json::json;

    async fn enrich(control: &Control, nodes: &mut [UiNode]) -> Result<(), ApiError> {
        crate::frontend::window_facts::apply_saved(control, nodes, crate::config::UiLanguage::Ja)
            .await
    }
    #[tokio::test]
    async fn saved_group_and_node_relations_resolve_repeated_calls_without_global_fallback() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        control.store.database.call(|db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');")?;
            for group in ["g","other"] {
                let attempt=format!("attempt-{group}");
                let call_node=format!("call-{group}");
                db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),?1,'u',0,0,'p','m',0,'completed','{}','now')",[&attempt])?;
                db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,'u','completed','now')",rusqlite::params![group,attempt])?;
                db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','assistant_response','now',?2)",rusqlite::params![call_node,json!({"group_id":group}).to_string()])?;
                db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,0,'function_call','completed',?2,?3)",rusqlite::params![attempt,json!({"call_id":"same"}).to_string(),call_node])?;
                db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,'same',0,'shell','{}','completed')",[group])?;
            }
            db.execute_batch("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','r','u','exec','exited','/','{\"completion\":{\"elapsed_ms\":123}}','UTF-8','UTF-8','closed','now'); UPDATE tool_call SET operation_id='exec' WHERE group_id='g'; INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('result','u','tool_result','now','{\"group_id\":\"g\",\"item\":{\"type\":\"function_call_output\",\"call_id\":\"same\",\"output\":\"{\\\"status\\\":\\\"exited\\\"}\"}}'); INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','same','result','exited');")?;
            Ok(())
        }).await.unwrap();
        assert_eq!(
            preview_source(&control, "call-g", Some("g"), "same")
                .await
                .unwrap()
                .as_deref(),
            Some("exec")
        );
        assert_eq!(
            preview_source(&control, "result", Some("g"), "same")
                .await
                .unwrap()
                .as_deref(),
            Some("exec")
        );
        assert!(
            preview_source(&control, "call-other", Some("other"), "same")
                .await
                .unwrap()
                .is_none()
        );
        for (node, group, call) in [
            ("call-g", "other", "same"),
            ("missing", "g", "same"),
            ("result", "g", "missing"),
        ] {
            assert_eq!(
                preview_source(&control, node, Some(group), call)
                    .await
                    .unwrap_err()
                    .code,
                crate::protocol::ErrorCode::TargetUnavailable
            );
        }
        let payload = json!({"group_id":"g","item":{"type":"function_call_output","call_id":"same","output":"{\"status\":\"exited\"}"}});
        let text = crate::history::text_of_payload(&payload);
        let mut nodes = vec![UiNode {
            node_id: "result".into(),
            role: "tool".into(),
            text: text.clone(),
            total_chars: text.chars().count(),
            presentation: presentation::project("tool_result", &payload, &text, 0),
            ..Default::default()
        }];
        enrich(&control, &mut nodes).await.unwrap();
        let tool = nodes[0]
            .presentation
            .blocks
            .iter()
            .find_map(|block| block.tool.as_ref())
            .unwrap();
        assert_eq!(tool.group_id.as_deref(), Some("g"));
        assert_eq!(tool.call_node_id.as_deref(), Some("call-g"));
        assert_eq!(tool.result_node_id.as_deref(), Some("result"));
        assert_eq!(tool.elapsed_ms, Some(123));
        let serialized = serde_json::to_value(tool).unwrap();
        for field in [
            "group_id",
            "call_node_id",
            "result_node_id",
            "started_at",
            "background",
            "elapsed_observation",
        ] {
            assert!(serialized.get(field).is_none());
        }
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod batch_tests;
