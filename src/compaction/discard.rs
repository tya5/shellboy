//! Content-free rejection facts. These describe a rejected adoption, not authority
//! to retry or relax its original transaction guards.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiscardReason {
    Cancelled,
    ThreadUnavailable,
    SelectionChanged,
    ProfileChanged,
    EpochChanged,
    RunStarted,
    RunStopped,
    TurnStarted,
    TurnChanged,
    NonappendGenerationChanged,
    SelectedPrefixChanged,
    CurrentTailChanged,
    SourceChanged,
    SourcePruned,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiscardPhase {
    ClosedSpill,
    ClosedSummary,
    ActiveSpill,
    ActiveSummary,
}
impl DiscardPhase {
    pub(crate) fn from_stage(stage: &str) -> Option<Self> {
        match stage {
            "closed_spill" => Some(Self::ClosedSpill),
            "closed_summary" => Some(Self::ClosedSummary),
            "active_spill" => Some(Self::ActiveSpill),
            "active_summary" => Some(Self::ActiveSummary),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompactionDiscard {
    pub reason: DiscardReason,
    pub phase: DiscardPhase,
    pub elapsed_ms: u64,
}

pub(super) fn rejected(reason: DiscardReason, message: &str) -> crate::protocol::ApiError {
    super::invalid(message).details(serde_json::json!({"compaction_discard_reason":reason}))
}

/// SQL supplies only a bounded fact, not the provider error body. Serde's default
/// recursion limit remains enabled; reject nesting before attempting allocation.
pub(crate) fn decode(text: &str) -> Option<CompactionDiscard> {
    if text.len() > 1024 || text.bytes().filter(|b| *b == b'{' || *b == b'[').count() > 1 {
        return None;
    }
    serde_json::from_str(text).ok()
}

/// The existing Status text budget also bounds the input before SQLite parses it.
/// Export only the small subtree; malformed/oversized legacy rows remain absent.
pub(crate) fn status_sql(alias: &str) -> String {
    format!(
        "CASE WHEN length(CAST({alias}.error_json AS BLOB))<=262144 THEN CASE WHEN json_valid({alias}.error_json) THEN CASE WHEN json_type({alias}.error_json,'$.automatic_trim_attempt.discard')='object' AND length(CAST(json_extract({alias}.error_json,'$.automatic_trim_attempt.discard') AS BLOB))<=1024 THEN json_extract({alias}.error_json,'$.automatic_trim_attempt.discard') END END END"
    )
}

// Both observations and finalization qualify the original token; absence is not
// permission to recreate an observation after interruption/recovery.
pub(crate) const STAGE_SQL: &str = "UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.active_stage',?3) WHERE id=?1 AND state IN ('preparing','inflight','failed','completed') AND json_valid(error_json) AND json_extract(error_json,'$.automatic_trim_attempt.token')=?2";
pub(crate) const SAVE_SQL: &str = "UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.discard',json(?3)) WHERE id=?1 AND state IN ('preparing','inflight','failed','completed') AND json_valid(error_json) AND json_extract(error_json,'$.automatic_trim_attempt.token')=?2 AND json_extract(error_json,'$.automatic_trim_attempt.active_stage')=?4";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_discard_requires_original_token_and_stage_and_preserves_success() {
        use rusqlite::params;
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE model_attempt(id TEXT,state TEXT,error_json TEXT,response_id TEXT,usage_json TEXT); INSERT INTO model_attempt VALUES('a','completed','{}','success','{}');").unwrap();
        let read = || {
            db.query_row("SELECT error_json FROM model_attempt", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        let fact = r#"{"reason":"run_started","phase":"closed_summary","elapsed_ms":3}"#;
        assert_eq!(
            db.execute(STAGE_SQL, params!["a", "token", "closed_summary"])
                .unwrap(),
            0
        );
        let original = serde_json::json!({"existing_counter":7,"automatic_trim_attempt":{"token":"token","committed_stages":{"closed_spill":{"groups":1}}}});
        db.execute(
            "UPDATE model_attempt SET error_json=?1",
            [original.to_string()],
        )
        .unwrap();
        assert_eq!(
            db.execute(STAGE_SQL, params!["a", "token", "closed_summary"])
                .unwrap(),
            1
        );
        for (token, phase) in [("old", "closed_summary"), ("token", "active_summary")] {
            assert_eq!(
                db.execute(SAVE_SQL, params!["a", token, fact, phase])
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            db.execute(SAVE_SQL, params!["a", "token", fact, "closed_summary"])
                .unwrap(),
            1
        );
        let saved: serde_json::Value = serde_json::from_str(&read()).unwrap();
        assert_eq!(saved["existing_counter"], 7);
        assert_eq!(
            saved["automatic_trim_attempt"]["committed_stages"],
            original["automatic_trim_attempt"]["committed_stages"]
        );
        assert_eq!(
            saved["automatic_trim_attempt"]["discard"]["reason"],
            "run_started"
        );
        assert_eq!(
            db.query_row(
                "SELECT state,response_id,usage_json FROM model_attempt",
                [],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ))
            )
            .unwrap(),
            ("completed".into(), "success".into(), "{}".into())
        );
        db.execute("UPDATE model_attempt SET error_json='{}'", [])
            .unwrap();
        assert_eq!(
            db.execute(SAVE_SQL, params!["a", "token", fact, "closed_summary"])
                .unwrap(),
            0
        );
        assert_eq!(
            db.execute(STAGE_SQL, params!["a", "token", "closed_summary"])
                .unwrap(),
            0
        );
        assert_eq!(read(), "{}");
    }
    #[test]
    fn discard_decoder_is_closed_bounded_and_content_free() {
        let fact = CompactionDiscard {
            reason: DiscardReason::RunStarted,
            phase: DiscardPhase::ClosedSummary,
            elapsed_ms: 42,
        };
        assert_eq!(decode(&serde_json::to_string(&fact).unwrap()), Some(fact));
        for text in [
            "{}",
            r#"{"reason":"future","phase":"closed_summary","elapsed_ms":1}"#,
            r#"{"reason":"run_started","phase":"closed_summary","elapsed_ms":-1}"#,
            r#"{"reason":"run_started","phase":"closed_summary","elapsed_ms":1,"secret":"PRIVATE_SENTINEL"}"#,
        ] {
            assert_eq!(decode(text), None);
        }
        assert_eq!(decode(&"[".repeat(2048)), None);
    }
}
