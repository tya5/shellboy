//! UI-only source correlation. No model/core state or canonical node is changed.
use crate::{
    control::Control,
    presentation::{BlockKind, ToolReasonKind},
    protocol::ApiError,
    ui::UiNode,
};

struct CallSource {
    node_index: usize,
    call_id: String,
    ordinal: u32,
    command: Option<String>,
    result_state: String,
    shell_state: Option<String>,
    shell_exit_code: Option<i64>,
    released: bool,
    group_id: String,
    prior_reason: Option<String>,
    execution_id: Option<String>,
}

// The node's saved group uses the existing (group_id, call_id) primary key.
// Only old nodes without a group field need the unambiguous result-relation fallback.
const SOURCE_SQL: &str = r"
SELECT c.ordinal,
       CASE WHEN json_valid(c.arguments_json)
            THEN substr(json_extract(c.arguments_json,'$.command'),1,512) END,
       COALESCE((SELECT r.status FROM tool_result r
                 WHERE r.group_id=c.group_id AND r.call_id=c.call_id),c.state),
       s.state,
       EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id),
       c.group_id, s.exit_code, c.operation_id
FROM tool_call c LEFT JOIN shell_execution s ON s.id=c.operation_id
WHERE c.group_id=COALESCE(
    (SELECT CASE WHEN json_valid(n.payload_json) THEN json_extract(n.payload_json,'$.group_id') END
     FROM node n WHERE n.id=?1),
    (SELECT CASE WHEN count(*)=1 THEN min(r.group_id) END
     FROM tool_result r WHERE r.node_id=?1 AND r.call_id=?2))
  AND c.call_id=?2
  AND (EXISTS(SELECT 1 FROM tool_result r
              WHERE r.node_id=?1 AND r.group_id=c.group_id AND r.call_id=c.call_id)
       OR EXISTS(SELECT 1 FROM node n
                 WHERE n.id=?1 AND n.kind='assistant_response' AND json_valid(n.payload_json)
                   AND json_extract(n.payload_json,'$.group_id')=c.group_id))";

const PRIOR_REASON_SQL: &str = r"
SELECT CASE WHEN json_valid(json_extract(n.payload_json,'$.item.output'))
            THEN json_extract(json_extract(n.payload_json,'$.item.output'),'$.reason') END
FROM tool_result r
JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id
JOIN node n ON n.id=r.node_id
WHERE r.group_id=?1 AND c.ordinal<?2 AND r.status<>'cancelled' AND json_valid(n.payload_json)
ORDER BY c.ordinal DESC LIMIT 1";

pub(super) async fn enrich(control: &Control, nodes: &mut [UiNode]) -> Result<(), ApiError> {
    for node in &mut *nodes {
        for block in &mut node.presentation.blocks {
            if let Some(tool) = &mut block.tool {
                tool.clear_observation();
            }
        }
    }
    let requests = nodes
        .iter()
        .enumerate()
        .flat_map(|(index, node)| {
            let mut ids = std::collections::HashSet::new();
            node.presentation.blocks.iter().filter_map(move |block| {
                let call = block.tool.as_ref()?;
                if call.call_id.is_empty() || !ids.insert(call.call_id.clone()) {
                    return None;
                }
                Some((index, node.node_id.clone(), call.call_id.clone()))
            })
        })
        .collect::<Vec<_>>();
    if requests.is_empty() {
        return Ok(());
    }
    let sources = control
        .store
        .database
        .call(move |db| {
            let mut query = db.prepare(SOURCE_SQL)?;
            let mut prior_query = db.prepare(PRIOR_REASON_SQL)?;
            let mut sources = Vec::new();
            use rusqlite::OptionalExtension;
            for (index, node, call) in requests {
                let source = query
                    .query_row(rusqlite::params![node, call], |row| {
                        Ok(CallSource {
                            node_index: index,
                            call_id: call.clone(),
                            ordinal: row.get(0)?,
                            command: row.get(1)?,
                            result_state: row.get(2)?,
                            shell_state: row.get(3)?,
                            released: row.get(4)?,
                            group_id: row.get(5)?,
                            prior_reason: None,
                            shell_exit_code: row.get(6)?,
                            execution_id: row.get(7)?,
                        })
                    })
                    .optional()?;
                if let Some(mut source) = source {
                    if source.result_state == "cancelled" {
                        source.prior_reason = prior_query
                            .query_row(rusqlite::params![source.group_id, source.ordinal], |row| {
                                row.get(0)
                            })
                            .optional()?
                            .flatten();
                    }
                    sources.push(source);
                }
            }
            Ok(sources)
        })
        .await
        .map_err(|error| error.into_api_error())?;
    for source in sources {
        for block in &mut nodes[source.node_index].presentation.blocks {
            let Some(tool) = &mut block.tool else {
                continue;
            };
            if tool.call_id != source.call_id {
                continue;
            }
            tool.ordinal = Some(source.ordinal);
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
        node.presentation.bound_metadata();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
