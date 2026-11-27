//! Owner read of historical failure evidence for window observation.
use crate::protocol::{ApiError, ErrorCode};
#[cfg(test)]
use rusqlite::OptionalExtension;

// A resume observation can omit turn_id in older data. Resolve its saved Run's
// latest Turn at that observation time, not whichever Turn is currently active.
#[cfg(test)]
pub(crate) const RESUME: &str = r"
SELECT (SELECT t.id FROM turn t
        WHERE t.run_id=json_extract(e.payload_ref_json,'$.change.run_id')
          AND t.thread_id=e.target_thread_id
          AND julianday(t.created_at)<=julianday(e.created_at)
          AND (json_extract(e.payload_ref_json,'$.change.turn_id') IS NULL
               OR t.id=json_extract(e.payload_ref_json,'$.change.turn_id'))
        ORDER BY julianday(t.created_at) DESC,t.rowid DESC LIMIT 1),e.created_at
FROM node n JOIN event e ON e.id=json_extract(n.payload_json,'$.event_id')
WHERE n.id=?1 AND json_valid(n.payload_json) AND json_valid(e.payload_ref_json)
  AND e.kind='context_changed'
  AND json_extract(e.payload_ref_json,'$.kind')='recovery_resumed'";
#[cfg(test)]
pub(crate) const FAILURES: &str = r"
SELECT count(*) FROM model_attempt
WHERE turn_id=?1 AND julianday(ended_at)<=julianday(?2)
  AND json_valid(error_json) AND json_type(error_json,'$.error')='object'";
#[cfg(test)]
pub(crate) const LATEST: &str = r"
SELECT a.id,CASE WHEN length(CAST(a.error_json AS BLOB))<=131072 THEN a.error_json END,
       (SELECT n.id FROM node n WHERE n.turn_id=a.turn_id AND n.kind='incomplete_response'
          AND n.payload_json=a.error_json ORDER BY n.rowid DESC LIMIT 1)
FROM model_attempt a
WHERE a.turn_id=?1 AND julianday(a.ended_at)<=julianday(?2)
  AND json_valid(a.error_json) AND json_type(a.error_json,'$.error')='object'
ORDER BY julianday(a.ended_at) DESC,a.rowid DESC LIMIT 1";
// Materialized observations deduplicate work before failure-range scans and
// exact source lookups. Fixed SQL count does not imply constant query work.
pub(crate) const BATCH: &str = r"
WITH requested AS MATERIALIZED (
    SELECT CAST(j.key AS INTEGER) AS position,json_extract(j.value,'$[0]') AS item_index,
        json_extract(j.value,'$[1]') AS node_id FROM json_each(?1) j),
resolved AS MATERIALIZED (
    SELECT q.position,q.item_index,(SELECT t.id FROM turn t
        WHERE t.run_id=json_extract(e.payload_ref_json,'$.change.run_id')
          AND t.thread_id=e.target_thread_id
          AND julianday(t.created_at)<=julianday(e.created_at)
          AND (json_extract(e.payload_ref_json,'$.change.turn_id') IS NULL
               OR t.id=json_extract(e.payload_ref_json,'$.change.turn_id'))
        ORDER BY julianday(t.created_at) DESC,t.rowid DESC LIMIT 1) AS saved_turn,
        e.created_at AS observed_at
    FROM requested q JOIN node n ON n.id=q.node_id
    JOIN event e ON e.id=json_extract(n.payload_json,'$.event_id')
    WHERE json_valid(n.payload_json) AND json_valid(e.payload_ref_json)
      AND e.kind='context_changed'
      AND json_extract(e.payload_ref_json,'$.kind')='recovery_resumed'),
observations AS MATERIALIZED (
    SELECT DISTINCT saved_turn,observed_at FROM resolved WHERE saved_turn IS NOT NULL),
failures AS MATERIALIZED (
    SELECT o.saved_turn,o.observed_at,(SELECT count(*) FROM model_attempt a
        WHERE a.turn_id=o.saved_turn AND julianday(a.ended_at)<=julianday(o.observed_at)
          AND json_valid(a.error_json) AND json_type(a.error_json,'$.error')='object') AS failure_count,
        (SELECT a.id FROM model_attempt a WHERE a.turn_id=o.saved_turn
          AND julianday(a.ended_at)<=julianday(o.observed_at)
          AND json_valid(a.error_json) AND json_type(a.error_json,'$.error')='object'
          ORDER BY julianday(a.ended_at) DESC,a.rowid DESC LIMIT 1) AS latest_attempt
    FROM observations o),
evidence AS MATERIALIZED (
    SELECT f.saved_turn,f.observed_at,f.failure_count,a.id,
        CASE WHEN octet_length(a.error_json)<=131072 THEN a.error_json END AS error_json,
        (SELECT n.id FROM node n WHERE n.turn_id=a.turn_id AND n.kind='incomplete_response'
          AND n.payload_json=a.error_json ORDER BY n.rowid DESC LIMIT 1) AS source
    FROM failures f JOIN model_attempt a ON a.id=f.latest_attempt WHERE f.failure_count>0)
SELECT r.item_index,e.failure_count,e.id,e.error_json,e.source
FROM resolved r JOIN evidence e ON e.saved_turn=r.saved_turn AND e.observed_at=r.observed_at
ORDER BY r.position";

#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Evidence {
    pub(crate) index: usize,
    pub(crate) count: i64,
    pub(crate) attempt: String,
    pub(crate) error_json: Option<String>,
    pub(crate) source: Option<String>,
}

impl Evidence {
    fn model_error(&self) -> Option<crate::model::ModelError> {
        let body: serde_json::Value = serde_json::from_str(self.error_json.as_deref()?).ok()?;
        serde_json::from_value(body.get("error")?.clone()).ok()
    }

    pub(crate) fn into_window(
        self,
        items: &[(String, String)],
    ) -> crate::protocol::HistoryWindowRecovery {
        use crate::protocol::{HistoryWindowRecovery, HistoryWindowRecoveryReason};
        let reason = self.model_error().map(|error| HistoryWindowRecoveryReason {
            kind: error.kind.chars().take(64).collect(),
            excerpt: crate::plain_text::sanitize_plain(
                &error.message.chars().take(256).collect::<String>(),
                0,
            )
            .0,
            truncated: error.message.chars().nth(256).is_some(),
        });
        HistoryWindowRecovery {
            node_id: items[self.index].0.clone(),
            failure_count: self.count as u64,
            latest_attempt_id: self.attempt,
            source_node_id: self.source,
            reason,
        }
    }
}

pub(crate) fn read_tx(
    db: &rusqlite::Connection,
    requests: &[(usize, String)],
) -> Result<Vec<Evidence>, crate::storage::StorageError> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    if requests.len() > 128 {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Recovery evidence exceeds its source bound",
        )
        .into());
    }
    let encoded = serde_json::to_string(requests).expect("Recovery source identities serialize");
    let mut query = db.prepare_cached(BATCH)?;
    let evidence = query
        .query_map([encoded], |row| {
            Ok(Evidence {
                index: row.get::<_, i64>(0)? as usize,
                count: row.get(1)?,
                attempt: row.get(2)?,
                error_json: row.get(3)?,
                source: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(evidence)
}

#[cfg(test)]
pub(crate) fn read_tx_reference(
    db: &rusqlite::Connection,
    requests: &[(usize, String)],
) -> Result<Vec<Evidence>, crate::storage::StorageError> {
    let mut resume = db.prepare(RESUME)?;
    let mut count = db.prepare(FAILURES)?;
    let mut latest = db.prepare(LATEST)?;
    let mut evidence = Vec::new();
    for (index, node) in requests {
        let saved: Option<(Option<String>, String)> = resume
            .query_row([node], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((Some(turn), at)) = saved else {
            continue;
        };
        let count: i64 = count.query_row(rusqlite::params![turn, at], |r| r.get(0))?;
        if count == 0 {
            continue;
        }
        let (attempt, body, source): (String, Option<String>, Option<String>) = latest
            .query_row(rusqlite::params![turn, at], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        evidence.push(Evidence {
            index: *index,
            count,
            attempt,
            error_json: body,
            source,
        });
    }
    Ok(evidence)
}
