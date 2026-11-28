//! Bounded evidence around the latest saved Thread activity.
use crate::protocol::{ThreadActivityShell, ThreadListDetail};
use rusqlite::Connection;
use std::collections::BTreeMap;

// Exact-ID seeks only. Raw JSON witnesses are copied after a global 64 KiB
// budget, never by first collecting full bodies in the materialized facts.
const DETAILS: &str = r"
WITH requested AS MATERIALIZED (SELECT DISTINCT value AS id FROM json_each(?1)),
facts AS MATERIALIZED (
 SELECT t.id,t.parent_thread_id,CASE WHEN octet_length(parent.title)<=4096 THEN parent.title END AS parent_title,parent.deleted_at AS parent_deleted,
   latest.id AS run_id,u.id AS turn_id,u.answer_node_id,
   shell.id AS execution,shell.turn_id AS shell_turn,shell.started_at AS shell_started,
   octet_length(shell.shell_json) AS command_bytes,
   (SELECT received_at FROM output_chunk WHERE execution_id=shell.id ORDER BY sequence DESC LIMIT 1) AS output_at,
   (SELECT count(*) FROM shell_execution WHERE owner_run_id=latest.id AND state IN ('starting','running','stopping')) AS shells,
   (SELECT count(*) FROM receipt r WHERE r.target_thread_id=t.id AND r.state IN ('accepted','held') AND r.path_admission<>'admitted'
     AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id)) AS pending,
   octet_length(u.recovery_reason_json) AS recovery_bytes,
   (SELECT octet_length(payload_json) FROM node WHERE id=u.answer_node_id) AS answer_bytes
 FROM requested q CROSS JOIN thread t ON t.id=q.id
 LEFT JOIN thread parent ON parent.id=t.parent_thread_id
 LEFT JOIN run latest ON latest.id=(SELECT id FROM run WHERE thread_id=t.id ORDER BY created_at DESC,id DESC LIMIT 1)
 LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=latest.id ORDER BY created_at DESC,id DESC LIMIT 1)
 LEFT JOIN shell_execution shell ON shell.id=(SELECT id FROM shell_execution WHERE owner_run_id=latest.id
   AND state IN ('starting','running','stopping') ORDER BY julianday(started_at) IS NULL,julianday(started_at),id LIMIT 1)
 WHERE t.deleted_at IS NULL
), sized AS MATERIALIZED (
 SELECT *,COALESCE(CASE WHEN recovery_bytes<=16384 THEN recovery_bytes ELSE 0 END,0)
         +COALESCE(CASE WHEN answer_bytes<=16384 THEN answer_bytes ELSE 0 END,0)
         +COALESCE(CASE WHEN command_bytes<=16384 THEN command_bytes ELSE 0 END,0) AS source_bytes FROM facts
), budgeted AS MATERIALIZED (
 SELECT *,sum(CASE WHEN source_bytes<=16384 THEN source_bytes ELSE 0 END)
   OVER(ORDER BY id ROWS UNBOUNDED PRECEDING) AS used FROM sized
)
SELECT id,parent_thread_id,parent_title,parent_deleted,run_id,execution,shell_turn,shell_started,
 CASE WHEN command_bytes<=16384 AND source_bytes<=16384 AND used<=65536 THEN
   (SELECT CASE WHEN json_valid(shell_json) THEN CASE WHEN json_type(shell_json,'$.command')='text'
       THEN substr(json_extract(shell_json,'$.command'),1,513) END END FROM shell_execution WHERE id=budgeted.execution) END,
 output_at,shells,pending,
 CASE WHEN recovery_bytes<=16384 AND source_bytes<=16384 AND used<=65536 THEN (SELECT recovery_reason_json FROM turn WHERE id=budgeted.turn_id) END,
 CASE WHEN answer_bytes<=16384 AND source_bytes<=16384 AND used<=65536 THEN (SELECT payload_json FROM node WHERE id=budgeted.answer_node_id) END,
 recovery_bytes IS NOT NULL AND (recovery_bytes>16384 OR source_bytes>16384 OR used>65536),
 answer_bytes IS NOT NULL AND (answer_bytes>16384 OR source_bytes>16384 OR used>65536),
 (SELECT state FROM shell_execution WHERE id=budgeted.execution),
 command_bytes IS NOT NULL AND (command_bytes>16384 OR source_bytes>16384 OR used>65536)
FROM budgeted ORDER BY id";

pub(crate) struct Extra {
    #[cfg(test)]
    pub(crate) parent: Option<String>,
    pub(crate) parent_title: Option<String>,
    pub(crate) parent_deleted: bool,
    #[cfg(test)]
    pub(crate) run: Option<String>,
    pub(crate) execution: Option<String>,
    pub(crate) shell_state: Option<String>,
    pub(crate) shell_turn: Option<String>,
    pub(crate) shell_started: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) command_limited: bool,
    pub(crate) output_at: Option<String>,
    #[cfg(test)]
    pub(crate) shells: u64,
    pub(crate) pending: u64,
    pub(crate) recovery: Option<String>,
    pub(crate) answer: Option<String>,
    pub(crate) recovery_limited: bool,
    pub(crate) answer_limited: bool,
}
pub(crate) fn read(db: &Connection, ids: &[String]) -> rusqlite::Result<BTreeMap<String, Extra>> {
    let encoded = serde_json::to_string(ids).expect("Thread IDs serialize");
    db.prepare(DETAILS)?
        .query_map([encoded], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Extra {
                    #[cfg(test)]
                    parent: row.get(1)?,
                    parent_title: row.get(2)?,
                    parent_deleted: row.get::<_, Option<String>>(3)?.is_some(),
                    #[cfg(test)]
                    run: row.get(4)?,
                    execution: row.get(5)?,
                    shell_turn: row.get(6)?,
                    shell_started: row.get(7)?,
                    command: row.get(8)?,
                    command_limited: row.get(17)?,
                    output_at: row.get(9)?,
                    #[cfg(test)]
                    shells: row.get::<_, i64>(10)? as u64,
                    pending: row.get::<_, i64>(11)? as u64,
                    recovery: row.get(12)?,
                    answer: row.get(13)?,
                    recovery_limited: row.get(14)?,
                    answer_limited: row.get(15)?,
                    shell_state: row.get(16)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
}

impl Extra {
    pub(crate) fn detail(self) -> ThreadListDetail {
        let mut answer_excerpt = self
            .answer
            .as_deref()
            .and_then(|body| serde_json::from_str(body).ok())
            .map(|value| crate::history::conversation_excerpt(&value, 513));
        let answer_truncated = self.answer_limited | trim_excerpt(&mut answer_excerpt);
        let mut command = self.command;
        let command_limited = self.command_limited | trim_excerpt(&mut command);
        ThreadListDetail {
            parent_title: self.parent_title,
            parent_deleted: self.parent_deleted,
            shell: self.execution.map(|execution_id| ThreadActivityShell {
                execution_id,
                turn_id: self.shell_turn,
                started_at: self.shell_started,
                last_output_at: self.output_at,
                command_excerpt: command,
                command_limited,
                state: self.shell_state.unwrap_or_default(),
            }),
            pending_messages: self.pending,
            recovery_error: self
                .recovery
                .as_deref()
                .and_then(|body| serde_json::from_str(body).ok()),
            recovery_limited: self.recovery_limited,
            answer_excerpt,
            answer_truncated,
        }
    }
}

fn trim_excerpt(value: &mut Option<String>) -> bool {
    if let Some(text) = value
        && let Some((boundary, _)) = text.char_indices().nth(512)
    {
        text.truncate(boundary);
        true
    } else {
        false
    }
}
