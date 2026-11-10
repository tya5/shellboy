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

#[cfg(test)]
mod tests {
    use super::*;
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
