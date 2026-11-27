//! Owner read of exact tool-call source facts for window observation.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::Connection;
#[cfg(test)]
use rusqlite::OptionalExtension;

pub(crate) const MAX_NODES: usize = 128;
pub(crate) const MAX_GROUP_CALLS: usize = 128;

pub(crate) use crate::protocol::HistoryWindowToolCall as CallSource;

// The node's saved group uses the existing (group_id, call_id) primary key.
// Only old nodes without a group field need the unambiguous result-relation fallback.
pub(crate) const SOURCE_SQL: &str = r"
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
       CASE WHEN json_valid(c.arguments_json) THEN json_extract(c.arguments_json,'$.background')=1 ELSE 0 END,
       s.saved_sequence,s.output_complete,
       CASE WHEN json_valid(c.arguments_json) THEN substr(json_extract(c.arguments_json,'$.description'),1,80) END
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
pub(crate) const PRIOR_REASON_SQL: &str = r"
SELECT CASE WHEN json_valid(json_extract(n.payload_json,'$.item.output'))
            THEN json_extract(json_extract(n.payload_json,'$.item.output'),'$.reason') END
FROM tool_result r
JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id
JOIN node n ON n.id=r.node_id
WHERE r.group_id=?1 AND c.ordinal<?2 AND r.status<>'cancelled' AND json_valid(n.payload_json)
ORDER BY c.ordinal DESC LIMIT 1";

#[derive(Default)]
pub(crate) struct ToolFacts {
    pub(crate) sources: Vec<CallSource>,
    pub(crate) truncated_nodes: Vec<String>,
    pub(crate) notices: Vec<super::notices::Fact>,
}
impl ToolFacts {
    pub(crate) fn into_window(self) -> crate::protocol::HistoryWindowTools {
        crate::protocol::HistoryWindowTools {
            calls: self.sources,
            limited_node_ids: self.truncated_nodes,
            notices: self.notices,
        }
    }
    #[cfg(test)]
    pub(crate) fn sources(&self) -> &[CallSource] {
        &self.sources
    }
    pub(crate) fn append(&mut self, other: Self) {
        for notice in other.notices {
            if let Some(old) = self
                .notices
                .iter_mut()
                .find(|old| old.node_id == notice.node_id)
            {
                *old = notice;
            } else {
                self.notices.push(notice);
            }
        }
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

#[cfg(test)]
pub(crate) fn read_tx_reference(
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
                    description: row.get(15)?,
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
                    output_revision: {
                        let complete = row.get::<_, Option<bool>>(14)?.unwrap_or(false);
                        row.get::<_, Option<i64>>(13)?
                            .map(|seq| (seq as u64, complete))
                    },
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
        notices: Vec::new(),
    })
}

// The requested source identities are one SQL input set. JSON functions select
// only fixed metadata fields; provider bodies are never copied into this batch.
// Requested IDs must stay outermost: a kind-index-first plan repeatedly scans
// json_each and makes unrelated retained tool nodes part of this bounded read.
pub(crate) const MODERN_NODE_BATCH_SQL: &str = r"
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
FROM requested CROSS JOIN node n ON n.id=requested.node_id WHERE n.kind IN ('assistant_response','tool_result','event','spill') ORDER BY requested.position";

// Calls whose results a spill replaced; the group comes from the saved source results.
pub(crate) const SPILL_CALL_BATCH_SQL: &str = r"
WITH requested AS (SELECT CAST(key AS INTEGER) AS position,value AS node_id FROM json_each(?1))
SELECT DISTINCT requested.position,n.id,r.group_id,r.call_id
FROM requested JOIN node n ON n.id=requested.node_id AND n.kind='spill' AND json_valid(n.payload_json)
JOIN json_each(n.payload_json,'$.source_node_ids') s
JOIN tool_result r ON r.node_id=s.value
WHERE EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(n.payload_json,'$.output')='array' THEN json_extract(n.payload_json,'$.output') ELSE '[]' END) j
  WHERE CASE WHEN j.type='object' THEN json_extract(j.value,'$.type') END='function_call_output'
    AND json_extract(j.value,'$.call_id')=r.call_id)
ORDER BY requested.position,r.call_id
LIMIT 16384";

// One bounded ordered seek per actual group member. Unlike row_number over a
// group join, this never reads every call merely to retain its first129 rows.
// Sparse ordinals are preserved: the bound counts real calls, not ordinal IDs.
pub(crate) const GROUP_CALL_BATCH_SQL: &str = r"
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
pub(crate) const SOURCE_BATCH_SQL: &str = r"
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
        CASE WHEN json_valid(c.arguments_json) THEN json_extract(c.arguments_json,'$.background')=1 ELSE 0 END AS background,
        CASE WHEN json_valid(c.arguments_json) THEN substr(json_extract(c.arguments_json,'$.description'),1,80) END AS description,
        s.saved_sequence AS output_sequence,s.output_complete
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
 o.node_id,c.result_node_id,c.elapsed_ms,priors.reason,c.started_at,c.background,c.output_sequence,c.output_complete,c.description
FROM requested JOIN calls c ON c.group_id=requested.group_id AND c.call_id=requested.call_id
LEFT JOIN node source ON source.id=requested.node_id
LEFT JOIN first_items first ON first.group_id=c.group_id AND first.call_id=c.call_id
LEFT JOIN output_item o ON o.attempt_id=c.attempt_id AND o.ordinal=first.ordinal
LEFT JOIN priors ON priors.group_id=c.group_id AND priors.call_id=c.call_id
WHERE c.result_node_id=requested.node_id
 OR (source.kind='spill' AND json_valid(source.payload_json)
     AND EXISTS(SELECT 1 FROM json_each(source.payload_json,'$.source_node_ids') s WHERE s.value=c.result_node_id))
 OR (source.kind='assistant_response' AND json_valid(source.payload_json)
     AND json_extract(source.payload_json,'$.group_id')=c.group_id)
ORDER BY requested.position";

pub(crate) struct ModernSource {
    node_id: String,
    kind: String,
    group: Option<String>,
    direct: Option<String>,
    whole: bool,
}

pub(crate) fn read_nodes_tx(db: &Connection, ids: &[String]) -> Result<ToolFacts, StorageError> {
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
    let notice_ids = nodes
        .iter()
        .filter(|node| node.kind == "event")
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    let notices = super::notices::read_tx(db, &notice_ids)?;
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
    let spills = nodes
        .iter()
        .filter(|node| node.kind == "spill")
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    if !spills.is_empty() {
        let encoded = serde_json::to_string(&spills).expect("spill IDs serialize");
        let mut query = db.prepare(SPILL_CALL_BATCH_SQL)?;
        let rows = query.query_map([encoded], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut per_node = std::collections::HashMap::<String, usize>::new();
        for row in rows {
            let (node, group, call) = row?;
            let count = per_node.entry(node.clone()).or_default();
            *count += 1;
            if *count > MAX_GROUP_CALLS {
                if !truncated_nodes.contains(&node) {
                    truncated_nodes.push(node);
                }
                continue;
            }
            requests.push((node, group, call));
        }
    }
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
            notices,
        });
    }
    let sources = read_sources_batch(db, &requests)?;
    Ok(ToolFacts {
        sources,
        truncated_nodes,
        notices,
    })
}

pub(crate) fn read_sources_batch(
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
                description: row.get(18)?,
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
                output_revision: {
                    let complete = row.get::<_, Option<bool>>(17)?.unwrap_or(false);
                    row.get::<_, Option<i64>>(16)?
                        .map(|seq| (seq as u64, complete))
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sources)
}
