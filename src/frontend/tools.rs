//! UI-only source correlation. No model/core state or canonical node is changed.
use crate::{
    control::Control,
    presentation::{BlockKind, ToolReasonKind},
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::UiNode,
};
use rusqlite::Connection;
#[cfg(test)]
use rusqlite::OptionalExtension;

const MAX_NODES: usize = 128;
const MAX_GROUP_CALLS: usize = 128;

pub(super) struct CallSource {
    pub(super) node_id: String,
    pub(super) call_id: String,
    ordinal: u32,
    command: Option<String>,
    result_state: String,
    pub(super) shell_state: Option<String>,
    shell_exit_code: Option<i64>,
    released: bool,
    pub(super) group_id: String,
    prior_reason: Option<String>,
    pub(super) execution_id: Option<String>,
    call_node_id: Option<String>,
    result_node_id: Option<String>,
    elapsed_ms: Option<u64>,
    started_at: Option<String>,
    background: bool,
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
       c.group_id, s.exit_code, c.operation_id,
       (SELECT o.node_id FROM tool_group g JOIN output_item o ON o.attempt_id=g.attempt_id
        WHERE g.id=c.group_id AND o.item_type='function_call'
          AND json_valid(o.payload_json) AND json_extract(o.payload_json,'$.call_id')=c.call_id
        ORDER BY o.ordinal LIMIT 1),
       (SELECT r.node_id FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id),
       CASE WHEN json_valid(s.shell_json) AND json_type(s.shell_json,'$.completion.elapsed_ms')='integer'
         AND json_extract(s.shell_json,'$.completion.elapsed_ms')>=0
         THEN json_extract(s.shell_json,'$.completion.elapsed_ms') END,
       substr(s.started_at,1,128),
       CASE WHEN json_valid(c.arguments_json) THEN json_extract(c.arguments_json,'$.background')=1 ELSE 0 END
FROM tool_call c LEFT JOIN shell_execution s ON s.id=c.operation_id
    AND s.turn_id=(SELECT turn_id FROM tool_group WHERE id=c.group_id)
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

#[cfg(test)]
const PRIOR_REASON_SQL: &str = r"
SELECT CASE WHEN json_valid(json_extract(n.payload_json,'$.item.output'))
            THEN json_extract(json_extract(n.payload_json,'$.item.output'),'$.reason') END
FROM tool_result r
JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id
JOIN node n ON n.id=r.node_id
WHERE r.group_id=?1 AND c.ordinal<?2 AND r.status<>'cancelled' AND json_valid(n.payload_json)
ORDER BY c.ordinal DESC LIMIT 1";

#[derive(Default)]
pub(super) struct ToolFacts {
    sources: Vec<CallSource>,
    truncated_nodes: Vec<String>,
}
impl ToolFacts {
    pub(super) fn sources(&self) -> &[CallSource] {
        &self.sources
    }
    pub(super) fn append(&mut self, other: Self) {
        let mut seen = self
            .sources
            .iter()
            .map(|source| (source.node_id.clone(), source.call_id.clone()))
            .collect::<std::collections::HashSet<_>>();
        self.sources.extend(
            other
                .sources
                .into_iter()
                .filter(|source| seen.insert((source.node_id.clone(), source.call_id.clone()))),
        );
        for node in other.truncated_nodes {
            if !self.truncated_nodes.contains(&node) {
                self.truncated_nodes.push(node);
            }
        }
    }
}

fn clear_observations(nodes: &mut [UiNode]) {
    for node in nodes {
        for block in &mut node.presentation.blocks {
            if let Some(tool) = &mut block.tool {
                tool.clear_observation();
                tool.group_id = None;
                tool.call_node_id = None;
                tool.result_node_id = None;
            }
        }
    }
}

#[cfg(test)]
fn read_tx_reference(
    db: &Connection,
    requests: &[(String, String)],
) -> Result<ToolFacts, StorageError> {
    if requests.len() > MAX_NODES * MAX_GROUP_CALLS
        || requests
            .iter()
            .map(|(node, _)| node)
            .collect::<std::collections::HashSet<_>>()
            .len()
            > MAX_NODES
    {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Tool source metadata exceeds its input bound",
        )
        .into());
    }
    let mut query = db.prepare(SOURCE_SQL)?;
    let mut prior_query = db.prepare(PRIOR_REASON_SQL)?;
    let mut sources = Vec::new();
    for (node, call) in requests {
        let source = query
            .query_row(rusqlite::params![node, call], |row| {
                Ok(CallSource {
                    node_id: node.clone(),
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
                    call_node_id: row.get(8)?,
                    result_node_id: row.get(9)?,
                    elapsed_ms: row.get::<_, Option<i64>>(10)?.map(|value| value as u64),
                    started_at: row.get(11)?,
                    background: row.get::<_, Option<bool>>(12)?.unwrap_or(false),
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
    Ok(ToolFacts {
        sources,
        truncated_nodes: Vec::new(),
    })
}

// Preserve the existing saved-group-first, unique-result fallback for known
// projected (node,call) pairs. Legacy evidence is resolved as one relation set,
// never by searching for a call ID globally or executing a query per block.
const PROJECTED_SOURCE_BATCH_SQL: &str = r"
WITH requested AS MATERIALIZED (
 SELECT CAST(key AS INTEGER) AS position,json_extract(value,'$[0]') AS node_id,
        json_extract(value,'$[1]') AS call_id FROM json_each(?1)
), nodes AS MATERIALIZED (
 SELECT requested.*,CASE WHEN json_valid(n.payload_json) THEN json_extract(n.payload_json,'$.group_id') END AS saved_group
 FROM requested LEFT JOIN node n ON n.id=requested.node_id
), missing AS MATERIALIZED (SELECT DISTINCT node_id,call_id FROM nodes WHERE saved_group IS NULL),
legacy AS MATERIALIZED (
 SELECT r.node_id,r.call_id,CASE WHEN count(*)=1 THEN min(r.group_id) END AS group_id
 FROM missing JOIN tool_result r ON r.node_id=missing.node_id AND r.call_id=missing.call_id
 GROUP BY r.node_id,r.call_id
)
SELECT nodes.node_id,CAST(COALESCE(nodes.saved_group,legacy.group_id) AS TEXT),nodes.call_id
FROM nodes LEFT JOIN legacy ON legacy.node_id=nodes.node_id AND legacy.call_id=nodes.call_id
WHERE COALESCE(nodes.saved_group,legacy.group_id) IS NOT NULL ORDER BY nodes.position";

pub(super) fn read_tx(
    db: &Connection,
    requests: &[(String, String)],
) -> Result<ToolFacts, StorageError> {
    if requests.len() > MAX_NODES * MAX_GROUP_CALLS
        || requests
            .iter()
            .map(|(node, _)| node)
            .collect::<std::collections::HashSet<_>>()
            .len()
            > MAX_NODES
    {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Tool source metadata exceeds its input bound",
        )
        .into());
    }
    if requests.is_empty() {
        return Ok(ToolFacts::default());
    }
    let encoded = serde_json::to_string(requests).expect("source requests serialize");
    let resolved = db
        .prepare(PROJECTED_SOURCE_BATCH_SQL)?
        .query_map([encoded], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    Ok(ToolFacts {
        sources: read_sources_batch(db, &resolved)?,
        truncated_nodes: Vec::new(),
    })
}

// The requested source identities are one SQL input set. JSON functions select
// only fixed metadata fields; provider bodies are never copied into this batch.
// Requested IDs must stay outermost: a kind-index-first plan repeatedly scans
// json_each and makes unrelated retained tool nodes part of this bounded read.
const MODERN_NODE_BATCH_SQL: &str = r"
WITH requested AS (SELECT CAST(key AS INTEGER) AS position,value AS node_id FROM json_each(?1))
SELECT requested.position,n.id,n.kind,
 CASE WHEN json_valid(n.payload_json) THEN CASE WHEN json_type(n.payload_json,'$.group_id')='text' THEN json_extract(n.payload_json,'$.group_id') END END,
 CASE WHEN json_valid(n.payload_json) THEN CASE
   WHEN json_extract(n.payload_json,'$.item.type') IN ('function_call','function_call_output') AND json_type(n.payload_json,'$.item.call_id')='text' THEN json_extract(n.payload_json,'$.item.call_id')
   WHEN json_extract(n.payload_json,'$.type') IN ('function_call','function_call_output') AND json_type(n.payload_json,'$.call_id')='text' THEN json_extract(n.payload_json,'$.call_id') END END,
 CASE WHEN json_valid(n.payload_json) THEN
   EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(n.payload_json,'$.output')='array' THEN json_extract(n.payload_json,'$.output') ELSE '[]' END) j WHERE CASE WHEN j.type='object' THEN json_extract(j.value,'$.type') END='function_call')
   OR EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(n.payload_json,'$.response.output')='array' THEN json_extract(n.payload_json,'$.response.output') ELSE '[]' END) j WHERE CASE WHEN j.type='object' THEN json_extract(j.value,'$.type') END='function_call')
 ELSE 0 END
FROM requested CROSS JOIN node n ON n.id=requested.node_id WHERE n.kind IN ('assistant_response','tool_result') ORDER BY requested.position";

// One bounded ordered seek per actual group member. Unlike row_number over a
// group join, this never reads every call merely to retain its first129 rows.
// Sparse ordinals are preserved: the bound counts real calls, not ordinal IDs.
const GROUP_CALL_BATCH_SQL: &str = r"
WITH RECURSIVE groups(group_id) AS (SELECT DISTINCT value FROM json_each(?1)),
picked(group_id,position,ordinal) AS (
 SELECT group_id,1,(SELECT MIN(c.ordinal) FROM tool_call c WHERE c.group_id=groups.group_id) FROM groups
 UNION ALL
 SELECT group_id,position+1,(SELECT MIN(c.ordinal) FROM tool_call c WHERE c.group_id=picked.group_id AND c.ordinal>picked.ordinal)
 FROM picked WHERE position<129 AND ordinal IS NOT NULL
)
SELECT picked.group_id,c.call_id FROM picked JOIN tool_call c
 ON c.group_id=picked.group_id AND c.ordinal=picked.ordinal
ORDER BY picked.group_id,picked.position";

// Distinct call keys share shell/result/output-item and prior-result evidence,
// including when multiple displayed nodes refer to one saved call. This is one
// statement, not a prepared statement executed once for each projected block.
const SOURCE_BATCH_SQL: &str = r"
WITH requested AS MATERIALIZED (
 SELECT CAST(key AS INTEGER) AS position,
        json_extract(value,'$[0]') AS node_id,json_extract(value,'$[1]') AS group_id,
        json_extract(value,'$[2]') AS call_id FROM json_each(?1)
), keys AS MATERIALIZED (SELECT DISTINCT group_id,call_id FROM requested),
calls AS MATERIALIZED (
 SELECT c.group_id,c.call_id,c.ordinal,c.operation_id,g.attempt_id,g.turn_id,
        CASE WHEN json_valid(c.arguments_json) THEN substr(json_extract(c.arguments_json,'$.command'),1,512) END AS command,
        COALESCE(r.status,c.state) AS result_state,r.node_id AS result_node_id,
        s.state AS shell_state,s.exit_code,
        CASE WHEN json_valid(s.shell_json) THEN CASE WHEN json_type(s.shell_json,'$.completion.elapsed_ms')='integer'
          AND json_extract(s.shell_json,'$.completion.elapsed_ms')>=0 THEN json_extract(s.shell_json,'$.completion.elapsed_ms') END END AS elapsed_ms,
        substr(s.started_at,1,128) AS started_at,
        CASE WHEN json_valid(c.arguments_json) THEN json_extract(c.arguments_json,'$.background')=1 ELSE 0 END AS background
 FROM keys JOIN tool_call c ON c.group_id=keys.group_id AND c.call_id=keys.call_id
 JOIN tool_group g ON g.id=c.group_id
 LEFT JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id
 LEFT JOIN shell_execution s ON s.id=c.operation_id AND s.turn_id=g.turn_id
), attempts AS MATERIALIZED (SELECT DISTINCT attempt_id FROM calls),
item_calls AS MATERIALIZED (
 SELECT o.attempt_id,o.ordinal,CAST(json_extract(o.payload_json,'$.call_id') AS TEXT) AS call_id
 FROM attempts JOIN output_item o ON o.attempt_id=attempts.attempt_id
 WHERE o.item_type='function_call' AND json_valid(o.payload_json)
), first_items AS MATERIALIZED (
 SELECT c.group_id,c.call_id,MIN(o.ordinal) AS ordinal
 FROM calls c JOIN item_calls o ON o.attempt_id=c.attempt_id AND o.call_id=c.call_id
 GROUP BY c.group_id,c.call_id
), priors AS MATERIALIZED (
 SELECT c.group_id,c.call_id,
   (SELECT CASE WHEN json_valid(json_extract(n.payload_json,'$.item.output'))
           THEN json_extract(json_extract(n.payload_json,'$.item.output'),'$.reason') END
    FROM tool_call p JOIN tool_result r ON r.group_id=p.group_id AND r.call_id=p.call_id
    JOIN node n ON n.id=r.node_id
    WHERE p.group_id=c.group_id AND p.ordinal<c.ordinal AND r.status<>'cancelled' AND json_valid(n.payload_json)
    ORDER BY p.ordinal DESC LIMIT 1) AS reason
 FROM calls c WHERE c.result_state='cancelled'
)
SELECT requested.node_id,c.call_id,c.ordinal,c.command,
 c.result_state,c.shell_state,c.result_node_id IS NOT NULL,c.group_id,c.exit_code,c.operation_id,
 o.node_id,c.result_node_id,c.elapsed_ms,priors.reason,c.started_at,c.background
FROM requested JOIN calls c ON c.group_id=requested.group_id AND c.call_id=requested.call_id
LEFT JOIN node source ON source.id=requested.node_id
LEFT JOIN first_items first ON first.group_id=c.group_id AND first.call_id=c.call_id
LEFT JOIN output_item o ON o.attempt_id=c.attempt_id AND o.ordinal=first.ordinal
LEFT JOIN priors ON priors.group_id=c.group_id AND priors.call_id=c.call_id
WHERE c.result_node_id=requested.node_id
 OR (source.kind='assistant_response' AND json_valid(source.payload_json)
     AND json_extract(source.payload_json,'$.group_id')=c.group_id)
ORDER BY requested.position";

struct ModernSource {
    node_id: String,
    kind: String,
    group: Option<String>,
    direct: Option<String>,
    whole: bool,
}

pub(super) fn read_nodes_tx(db: &Connection, ids: &[String]) -> Result<ToolFacts, StorageError> {
    if ids.len() > MAX_NODES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Tool source metadata exceeds its node bound",
        )
        .into());
    }
    if ids.is_empty() {
        return Ok(ToolFacts::default());
    }
    let mut seen = std::collections::HashSet::new();
    let ids = ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .collect::<Vec<_>>();
    let encoded = serde_json::to_string(&ids).expect("source IDs serialize");
    let nodes = db
        .prepare(MODERN_NODE_BATCH_SQL)?
        .query_map([encoded], |row| {
            Ok(ModernSource {
                node_id: row.get(1)?,
                kind: row.get(2)?,
                group: row.get(3)?,
                direct: row.get(4)?,
                whole: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let groups = nodes
        .iter()
        .filter(|node| node.kind == "assistant_response" && node.direct.is_none() && node.whole)
        .filter_map(|node| node.group.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    let mut group_calls = std::collections::HashMap::<String, Vec<String>>::new();
    if !groups.is_empty() {
        let encoded = serde_json::to_string(&groups).expect("group IDs serialize");
        let mut query = db.prepare(GROUP_CALL_BATCH_SQL)?;
        let rows = query.query_map([encoded], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (group, call) = row?;
            group_calls.entry(group).or_default().push(call);
        }
    }
    let mut requests = Vec::<(String, String, String)>::new();
    let mut truncated_nodes = Vec::new();
    for node in nodes {
        let Some(group) = node.group else {
            continue;
        };
        if !matches!(node.kind.as_str(), "assistant_response" | "tool_result") {
            continue;
        }
        if let Some(call) = node.direct {
            requests.push((node.node_id, group, call));
        } else if node.kind == "assistant_response" && node.whole {
            let calls = group_calls
                .get(&group)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if calls.len() > MAX_GROUP_CALLS {
                truncated_nodes.push(node.node_id.clone());
            }
            requests.extend(
                calls
                    .iter()
                    .take(MAX_GROUP_CALLS)
                    .map(|call| (node.node_id.clone(), group.clone(), call.clone())),
            );
        }
    }
    if requests.is_empty() {
        return Ok(ToolFacts {
            sources: Vec::new(),
            truncated_nodes,
        });
    }
    let sources = read_sources_batch(db, &requests)?;
    Ok(ToolFacts {
        sources,
        truncated_nodes,
    })
}

fn read_sources_batch(
    db: &Connection,
    requests: &[(String, String, String)],
) -> Result<Vec<CallSource>, StorageError> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let encoded = serde_json::to_string(requests).expect("source requests serialize");
    let sources = db
        .prepare(SOURCE_BATCH_SQL)?
        .query_map([encoded], |row| {
            Ok(CallSource {
                node_id: row.get(0)?,
                call_id: row.get(1)?,
                ordinal: row.get(2)?,
                command: row.get(3)?,
                result_state: row.get(4)?,
                shell_state: row.get(5)?,
                released: row.get(6)?,
                group_id: row.get(7)?,
                shell_exit_code: row.get(8)?,
                execution_id: row.get(9)?,
                call_node_id: row.get(10)?,
                result_node_id: row.get(11)?,
                elapsed_ms: row.get::<_, Option<i64>>(12)?.map(|value| value as u64),
                prior_reason: row.get(13)?,
                started_at: row.get(14)?,
                background: row.get::<_, Option<bool>>(15)?.unwrap_or(false),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sources)
}

pub(super) fn apply(nodes: &mut [UiNode], facts: &ToolFacts) {
    clear_observations(nodes);
    for source in &facts.sources {
        let Some(node) = nodes.iter_mut().find(|node| node.node_id == source.node_id) else {
            continue;
        };
        for block in &mut node.presentation.blocks {
            let Some(tool) = &mut block.tool else {
                continue;
            };
            if tool.call_id != source.call_id {
                continue;
            }
            tool.ordinal = Some(source.ordinal);
            tool.group_id = Some(source.group_id.clone());
            tool.call_node_id = source.call_node_id.clone();
            tool.result_node_id = source.result_node_id.clone();
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
        if facts.truncated_nodes.iter().any(|id| id == &node.node_id) {
            node.presentation.metadata_limited = true;
            node.presentation.details_omitted = true;
        }
        node.presentation.bound_metadata();
    }
}

pub(super) async fn enrich(control: &Control, nodes: &mut [UiNode]) -> Result<(), ApiError> {
    clear_observations(nodes);
    let requests = nodes
        .iter()
        .flat_map(|node| {
            let mut seen = std::collections::HashSet::new();
            node.presentation.blocks.iter().filter_map(move |block| {
                let tool = block.tool.as_ref()?;
                (!tool.call_id.is_empty() && seen.insert(tool.call_id.clone()))
                    .then(|| (node.node_id.clone(), tool.call_id.clone()))
            })
        })
        .collect::<Vec<_>>();
    if requests.is_empty() {
        return Ok(());
    }
    let facts = control
        .store
        .database
        .call(move |db| read_tx(db, &requests))
        .await
        .map_err(StorageError::into_api_error)?;
    let owned = nodes.iter_mut().map(std::mem::take).collect::<Vec<_>>();
    let projected = super::presentation_work(move || {
        let mut owned = owned;
        apply(&mut owned, &facts);
        owned
    })
    .await?;
    for (target, node) in nodes.iter_mut().zip(projected) {
        *target = node;
    }
    Ok(())
}

/// Resolve a saved source relation, never an execution borrowed by call ID alone.
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
                db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,'u',0,0,'p','m',0,'completed','{}','now')",[&attempt])?;
                db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,'u','completed','now')",rusqlite::params![group,attempt])?;
                db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','assistant_response','now',?2)",rusqlite::params![call_node,json!({"group_id":group}).to_string()])?;
                db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,0,'function_call','completed',?2,?3)",rusqlite::params![attempt,json!({"call_id":"same"}).to_string(),call_node])?;
                db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,'same',0,'shell','{}','completed')",[group])?;
            }
            db.execute_batch("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','r','u','exec','exited','/','{\"completion\":{\"elapsed_ms\":123}}','UTF-8','UTF-8','closed','now'); UPDATE tool_call SET operation_id='exec' WHERE group_id='g'; INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('result','u','tool_result','now','{\"group_id\":\"g\"}'); INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','same','result','exited');")?;
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
