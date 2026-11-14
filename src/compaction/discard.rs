//! Content-free terminal facts: rejected guards and source-owned failures.
//! Observation never grants retry or adoption authority.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    Cancelled,
    ThreadUnavailable,
    SelectionChanged,
    ProfileChanged,
    SummarySettingsChanged,
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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPhase {
    ClosedSpill,
    ClosedSummary,
    ActiveSpill,
    ActiveSummary,
}
impl CompactionPhase {
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
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    ProviderError,
    ContextLimitTerminal,
    RejectedAtMinimum,
    Refused,
    Empty,
    NotShrinking,
    Cancelled,
    Unclassified,
    RetrySuppressed,
}
impl FailureReason {
    const ALL: [Self; 9] = [
        Self::ProviderError,
        Self::ContextLimitTerminal,
        Self::RejectedAtMinimum,
        Self::Refused,
        Self::Empty,
        Self::NotShrinking,
        Self::Cancelled,
        Self::Unclassified,
        Self::RetrySuppressed,
    ];
    fn required_origin(self) -> Option<FailureOrigin> {
        match self {
            Self::Refused | Self::Empty | Self::NotShrinking => {
                Some(FailureOrigin::OutputValidation)
            }
            Self::Cancelled => Some(FailureOrigin::Cancellation),
            Self::RetrySuppressed => Some(FailureOrigin::Preparation),
            _ => None,
        }
    }
    fn allows_http_status(self) -> bool {
        matches!(
            self,
            Self::ProviderError | Self::ContextLimitTerminal | Self::RejectedAtMinimum
        )
    }
    fn allows_final_counts(self) -> bool {
        matches!(self, Self::ContextLimitTerminal | Self::RejectedAtMinimum)
    }
}

// Schema and decoder share the reason-dependent rules. Optional null metadata
// remains equivalent to absence, as in the existing serde Option contract.
fn constrain_failure_schema(schema: &mut schemars::Schema) {
    use serde_json::json;
    let mut rules: Vec<_> = FailureReason::ALL
        .into_iter()
        .filter_map(|reason| {
            let mut properties = serde_json::Map::new();
            if let Some(origin) = reason.required_origin() {
                properties.insert("origin".into(), json!({"const":origin}));
            }
            if !reason.allows_http_status() {
                properties.insert("http_status".into(), json!({"type":"null"}));
            }
            if !reason.allows_final_counts() {
                for name in ["final_group_count", "final_source_nodes"] {
                    properties.insert(name.into(), json!({"type":"null"}));
                }
            }
            if reason == FailureReason::RejectedAtMinimum {
                properties.insert("final_group_count".into(), json!({"const":1}));
                properties.insert(
                    "final_source_nodes".into(),
                    json!({"type":"integer","minimum":1,"maximum":9007199254740991u64}),
                );
            }
            (!properties.is_empty()).then(|| {
                json!({
                    "if":{"properties":{"reason":{"const":reason}},"required":["reason"]},
                    "then":{"properties":properties},
                })
            })
        })
        .collect();
    rules.push(json!({
        "if":{"properties":{"reason":{"const":"rejected_at_minimum"}},"required":["reason"]},
        "then":{
            "required":["origin","http_status","final_group_count","final_source_nodes"],
            "properties":{"origin":{"const":"provider_response"},"final_group_count":{"const":1},"final_source_nodes":{"type":"integer","minimum":1,"maximum":9007199254740991u64}},
            "anyOf":[
                {"properties":{"http_status":{"enum":[400,413,422]}},"required":["http_status"]},
                {"properties":{"http_status":{"type":"integer","minimum":200,"maximum":299},"provider_status":{"enum":[400,413,422]},"provider_status_source":{"const":"stream_error_code"}},"required":["http_status","provider_status","provider_status_source"]}
            ]
        }
    }));
    rules.push(json!({
        "if": {
            "anyOf": [
                {"properties":{"provider_status":{"type":"number"}},"required":["provider_status"]},
                {"properties":{"provider_status_source":{"type":"string"}},"required":["provider_status_source"]}
            ]
        },
        "then": {
            "required":["provider_status","provider_status_source"],
            "properties":{
                "reason":{"enum":["provider_error","rejected_at_minimum"]},
                "origin":{"const":"provider_response"},
                "provider_status":{"type":"integer","minimum":100,"maximum":599},
                "provider_status_source":{"const":"stream_error_code"}
            }
        }
    }));
    for reason in FailureReason::ALL.into_iter().filter(|reason| {
        !matches!(
            reason,
            FailureReason::ProviderError | FailureReason::RejectedAtMinimum
        )
    }) {
        rules.push(json!({
            "if":{"properties":{"reason":{"const":reason}},"required":["reason"]},
            "then":{"properties":{"provider_status":{"type":"null"},"provider_status_source":{"type":"null"}}}
        }));
    }
    schema.insert("allOf".into(), json!(rules));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureOrigin {
    ProviderResponse,
    Transport,
    Protocol,
    Preparation,
    OutputValidation,
    Storage,
    Cancellation,
    Internal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Rejected {
        reason: RejectionReason,
    },
    #[schemars(transform = constrain_failure_schema)]
    Failed {
        reason: FailureReason,
        origin: FailureOrigin,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[schemars(range(min = 100, max = 599))]
        http_status: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[schemars(range(min = 100, max = 599))]
        provider_status: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        provider_status_source: Option<crate::model::StreamProviderStatusSource>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[schemars(range(min = 1, max = 9007199254740991u64))]
        final_group_count: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[schemars(range(min = 1, max = 9007199254740991u64))]
        final_source_nodes: Option<u64>,
    },
}
impl Outcome {
    pub(super) fn deterministic(self) -> bool {
        matches!(
            self,
            Self::Failed {
                reason: FailureReason::Refused
                    | FailureReason::Empty
                    | FailureReason::NotShrinking
                    | FailureReason::ContextLimitTerminal
                    | FailureReason::RejectedAtMinimum,
                ..
            }
        )
    }
    fn valid(self) -> bool {
        const SAFE: u64 = (1u64 << 53) - 1;
        match self {
            Self::Rejected { .. } => true,
            Self::Failed {
                reason,
                origin,
                http_status,
                provider_status,
                provider_status_source,
                final_group_count,
                final_source_nodes,
            } => reason
                .required_origin()
                .is_none_or(|required| origin == required)
                && http_status
                    .is_none_or(|s| (100..=599).contains(&s) && reason.allows_http_status())
                && provider_status.is_some() == provider_status_source.is_some()
                && provider_status.is_none_or(|status| {
                    (100..=599).contains(&status)
                        && matches!(
                            reason,
                            FailureReason::ProviderError | FailureReason::RejectedAtMinimum
                        )
                        && origin == FailureOrigin::ProviderResponse
                })
                && provider_status_source.is_none_or(|source| {
                    source == crate::model::StreamProviderStatusSource::StreamErrorCode
                        && matches!(
                            reason,
                            FailureReason::ProviderError | FailureReason::RejectedAtMinimum
                        )
                        && origin == FailureOrigin::ProviderResponse
                })
                && [final_group_count, final_source_nodes]
                    .into_iter()
                    .all(|n| n.is_none_or(|n| n > 0 && n <= SAFE && reason.allows_final_counts()))
                && (reason != FailureReason::RejectedAtMinimum
                    || (origin == FailureOrigin::ProviderResponse
                        && final_group_count == Some(1)
                        && final_source_nodes.is_some_and(|n| n > 0 && n <= SAFE)
                        && (http_status.is_some_and(|s| matches!(s, 400 | 413 | 422))
                            || (http_status.is_some_and(|s| (200..=299).contains(&s))
                                && provider_status
                                    .is_some_and(|s| matches!(s, 400 | 413 | 422))
                                && provider_status_source
                                    == Some(
                                        crate::model::StreamProviderStatusSource::StreamErrorCode,
                                    ))))),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompactionOutcomeFact {
    pub phase: CompactionPhase,
    #[schemars(range(max = 9007199254740991u64))]
    pub elapsed_ms: u64,
    pub outcome: Outcome,
}

pub(super) fn attach(
    mut error: crate::protocol::ApiError,
    cause: Outcome,
) -> crate::protocol::ApiError {
    debug_assert!(cause.valid());
    error.compaction_cause = Some(Box::new(cause));
    error
}
pub(super) fn rejected(reason: RejectionReason, message: &str) -> crate::protocol::ApiError {
    attach(super::invalid(message), Outcome::Rejected { reason })
}
pub(super) fn failed(
    reason: FailureReason,
    origin: FailureOrigin,
    message: &str,
) -> crate::protocol::ApiError {
    attach(
        super::invalid(message),
        Outcome::Failed {
            reason,
            origin,
            http_status: None,
            provider_status: None,
            provider_status_source: None,
            final_group_count: None,
            final_source_nodes: None,
        },
    )
}
pub(crate) fn cause(error: &crate::protocol::ApiError) -> Outcome {
    error
        .compaction_cause
        .as_deref()
        .copied()
        .unwrap_or(Outcome::Failed {
            reason: FailureReason::Unclassified,
            origin: if error.is_storage_failure() {
                FailureOrigin::Storage
            } else {
                FailureOrigin::Internal
            },
            http_status: None,
            provider_status: None,
            provider_status_source: None,
            final_group_count: None,
            final_source_nodes: None,
        })
}

/// Exact two-object shape, bounded before serde allocation. Unknown facts are
/// unavailable, never permission to fabricate a cause from historical details.
pub(crate) fn decode(text: &str) -> Option<CompactionOutcomeFact> {
    if text.len() > 1024 || text.bytes().filter(|b| *b == b'{' || *b == b'[').count() != 2 {
        return None;
    }
    let fact: CompactionOutcomeFact = serde_json::from_str(text).ok()?;
    (fact.elapsed_ms < (1u64 << 53) && fact.outcome.valid()).then_some(fact)
}
pub(crate) fn status_sql(alias: &str) -> String {
    format!(
        "CASE WHEN length(CAST({alias}.error_json AS BLOB))<=262144 THEN CASE WHEN json_valid({alias}.error_json) THEN CASE WHEN json_type({alias}.error_json,'$.automatic_trim_attempt.compaction_outcome')='object' AND length(CAST(json_extract({alias}.error_json,'$.automatic_trim_attempt.compaction_outcome') AS BLOB))<=1024 THEN json_extract({alias}.error_json,'$.automatic_trim_attempt.compaction_outcome') END END END"
    )
}

// Both observations and finalization qualify the original token; absence is not
// permission to recreate an observation after interruption/recovery.
pub(crate) const STAGE_SQL: &str = "UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.active_stage',?3) WHERE id=?1 AND state IN ('preparing','inflight','failed','completed') AND json_valid(error_json) AND json_extract(error_json,'$.automatic_trim_attempt.token')=?2";
pub(crate) const SAVE_SQL: &str = "UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.compaction_outcome',json(?3)) WHERE id=?1 AND state IN ('preparing','inflight','failed','completed') AND json_valid(error_json) AND json_extract(error_json,'$.automatic_trim_attempt.token')=?2 AND json_extract(error_json,'$.automatic_trim_attempt.active_stage')=?4";

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
        let fact = r#"{"outcome":{"kind":"rejected","reason":"run_started"},"phase":"closed_summary","elapsed_ms":3}"#;
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
            saved["automatic_trim_attempt"]["compaction_outcome"]["outcome"]["reason"],
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
        let fact = CompactionOutcomeFact {
            outcome: Outcome::Rejected {
                reason: RejectionReason::RunStarted,
            },
            phase: CompactionPhase::ClosedSummary,
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

pub(super) fn model_cause(error: &crate::model::ModelError, status: Option<u16>) -> Outcome {
    let (reason, origin) = match error.kind.as_str() {
        "usage_limit" if error.usage_limit.is_some() && status == Some(429) => (
            FailureReason::ProviderError,
            FailureOrigin::ProviderResponse,
        ),
        "provider" => (
            FailureReason::ProviderError,
            FailureOrigin::ProviderResponse,
        ),
        "transport" | "timeout" => (FailureReason::Unclassified, FailureOrigin::Transport),
        "protocol" | "incomplete" | "input_limit" => {
            (FailureReason::Unclassified, FailureOrigin::Protocol)
        }
        "cancelled" => (FailureReason::Cancelled, FailureOrigin::Cancellation),
        "config_invalid" | "config_missing" | "config_missing_credentials" | "input_invalid" => {
            (FailureReason::Unclassified, FailureOrigin::Preparation)
        }
        _ => (FailureReason::Unclassified, FailureOrigin::Internal),
    };
    Outcome::Failed {
        reason,
        origin,
        http_status: status
            .filter(|s| reason == FailureReason::ProviderError && (100..=599).contains(s)),
        provider_status: if reason == FailureReason::ProviderError
            && origin == FailureOrigin::ProviderResponse
        {
            error.stream_provider_status.map(|witness| witness.value)
        } else {
            None
        },
        provider_status_source: if reason == FailureReason::ProviderError
            && origin == FailureOrigin::ProviderResponse
        {
            error.stream_provider_status.map(|witness| witness.source)
        } else {
            None
        },
        final_group_count: None,
        final_source_nodes: None,
    }
}

#[cfg(test)]
mod outcome_contract_tests {
    use super::*;
    use crate::protocol::{ApiError, ErrorCode};
    use serde_json::json;

    #[test]
    fn typed_predicate_and_private_carrier_have_original_truth_set() {
        for reason in [
            FailureReason::ProviderError,
            FailureReason::ContextLimitTerminal,
            FailureReason::Refused,
            FailureReason::Empty,
            FailureReason::NotShrinking,
            FailureReason::Cancelled,
            FailureReason::Unclassified,
            FailureReason::RetrySuppressed,
        ] {
            let deterministic = matches!(
                reason,
                FailureReason::ContextLimitTerminal
                    | FailureReason::Refused
                    | FailureReason::Empty
                    | FailureReason::NotShrinking
            );
            let origin = if matches!(
                reason,
                FailureReason::Refused | FailureReason::Empty | FailureReason::NotShrinking
            ) {
                FailureOrigin::OutputValidation
            } else if reason == FailureReason::Cancelled {
                FailureOrigin::Cancellation
            } else if reason == FailureReason::RetrySuppressed {
                FailureOrigin::Preparation
            } else {
                FailureOrigin::Internal
            };
            let error = failed(reason, origin, "unchanged return text");
            assert_eq!(cause(&error).deterministic(), deterministic);
            let raw = serde_json::to_value(&error).unwrap();
            assert!(raw.get("compaction_cause").is_none());
            let restored: ApiError = serde_json::from_value(raw).unwrap();
            assert!(!cause(&restored).deterministic());
            assert_eq!(restored.code, error.code);
            assert_eq!(restored.message, error.message);
        }
        let minimum = attach(
            ApiError::new(ErrorCode::InvalidState, "minimum"),
            Outcome::Failed {
                reason: FailureReason::RejectedAtMinimum,
                origin: FailureOrigin::ProviderResponse,
                http_status: Some(400),
                provider_status: None,
                provider_status_source: None,
                final_group_count: Some(1),
                final_source_nodes: Some(1),
            },
        );
        assert!(cause(&minimum).deterministic());
        let forged = ApiError::new(ErrorCode::InvalidState, "unknown")
            .details(json!({"compaction_discard_reason":"summary_refused","single_summary_context_terminal":true,"compaction_cause":{"kind":"rejected","reason":"profile_changed"}}));
        assert!(!cause(&forged).deterministic());
        assert!(matches!(
            cause(&forged),
            Outcome::Failed {
                reason: FailureReason::Unclassified,
                ..
            }
        ));
    }
    #[test]
    fn strict_union_quarantines_unsafe_numbers_fields_and_cross_field_facts() {
        let generated: serde_json::Value =
            serde_json::from_str(&crate::protocol::schema_json()).unwrap();
        let schema = serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","$ref":"#/definitions/CompactionOutcomeFact","definitions":generated["definitions"]});
        let validator = jsonschema::validator_for(&schema).unwrap();
        let valid = json!({"phase":"closed_summary","elapsed_ms":17,"outcome":{"kind":"failed","reason":"provider_error","origin":"provider_response","http_status":401}});
        assert!(decode(&valid.to_string()).is_some());
        let paired = json!({"phase":"closed_summary","elapsed_ms":1,"outcome":{"kind":"failed","reason":"provider_error","origin":"provider_response","http_status":200,"provider_status":400,"provider_status_source":"stream_error_code"}});
        assert!(decode(&paired.to_string()).is_some());
        assert!(validator.is_valid(&paired), "valid paired stream witness");
        for bad_pair in [
            json!({"provider_status":400}),
            json!({"provider_status_source":"stream_error_code"}),
            json!({"provider_status":99,"provider_status_source":"stream_error_code"}),
            json!({"provider_status":600,"provider_status_source":"stream_error_code"}),
            json!({"provider_status":400,"provider_status_source":"future"}),
        ] {
            let mut candidate = paired.clone();
            candidate["outcome"]
                .as_object_mut()
                .unwrap()
                .remove("provider_status");
            candidate["outcome"]
                .as_object_mut()
                .unwrap()
                .remove("provider_status_source");
            candidate["outcome"]
                .as_object_mut()
                .unwrap()
                .extend(bad_pair.as_object().unwrap().clone());
            assert!(
                decode(&candidate.to_string()).is_none(),
                "decoder accepted {candidate}"
            );
            assert!(
                !validator.is_valid(&candidate),
                "schema accepted {candidate}"
            );
        }
        let mut wrong_pair_reason = paired.clone();
        wrong_pair_reason["outcome"]["reason"] = json!("refused");
        wrong_pair_reason["outcome"]["origin"] = json!("output_validation");
        assert!(decode(&wrong_pair_reason.to_string()).is_none());
        assert!(!validator.is_valid(&wrong_pair_reason));
        for (key, value) in [
            ("provider_code", json!("SECRET")),
            ("message", json!("SECRET")),
            ("http_status", json!(99)),
            ("final_source_nodes", json!(1)),
            ("origin", json!("future")),
        ] {
            let mut v = valid.clone();
            v["outcome"][key] = value;
            assert!(decode(&v.to_string()).is_none(), "{v}");
        }
        for elapsed in [json!(-1), json!(1.5), json!(9007199254740992u64)] {
            let mut v = valid.clone();
            v["elapsed_ms"] = elapsed;
            assert!(decode(&v.to_string()).is_none());
        }
        for reason in ["refused", "empty", "not_shrinking", "cancelled"] {
            let mut v = valid.clone();
            v["outcome"]["reason"] = json!(reason);
            assert!(decode(&v.to_string()).is_none());
        }
        let rejected = json!({"phase":"closed_summary","elapsed_ms":1,"outcome":{"kind":"rejected","reason":"profile_changed","http_status":401}});
        assert!(decode(&rejected.to_string()).is_none());
        assert!(decode(&format!("{}{}", " ".repeat(1024), valid)).is_none());
        assert!(decode(&"[".repeat(2048)).is_none());
    }
    #[test]
    fn adapter_origin_uses_trusted_kind_not_secret_prose_code_or_status() {
        for (kind, reason, origin) in [
            (
                "provider",
                FailureReason::ProviderError,
                FailureOrigin::ProviderResponse,
            ),
            (
                "transport",
                FailureReason::Unclassified,
                FailureOrigin::Transport,
            ),
            (
                "timeout",
                FailureReason::Unclassified,
                FailureOrigin::Transport,
            ),
            (
                "protocol",
                FailureReason::Unclassified,
                FailureOrigin::Protocol,
            ),
            (
                "config_missing_credentials",
                FailureReason::Unclassified,
                FailureOrigin::Preparation,
            ),
            (
                "cancelled",
                FailureReason::Cancelled,
                FailureOrigin::Cancellation,
            ),
            (
                "future",
                FailureReason::Unclassified,
                FailureOrigin::Internal,
            ),
        ] {
            let mut error = crate::model::ModelError::new(kind, "SECRET context length exceeded");
            error.provider_code = Some("SECRET_PROVIDER_CODE".into());
            error.status = Some(413);
            for status in [None, Some(0), Some(99), Some(413), Some(600)] {
                let cause = model_cause(&error, status);
                assert_eq!(
                    cause,
                    Outcome::Failed {
                        reason,
                        origin,
                        http_status: status
                            .filter(|s| kind == "provider" && (100..=599).contains(s)),
                        provider_status: None,
                        provider_status_source: None,
                        final_group_count: None,
                        final_source_nodes: None,
                    }
                );
                assert!(!cause.deterministic());
                assert!(!serde_json::to_string(&cause).unwrap().contains("SECRET"));
            }
        }
    }
    #[test]
    fn historical_other_is_not_a_current_union_or_capability() {
        for old in ["other", "summary_refused", "map_request_budget_exhausted"] {
            let v = json!({"reason":old,"phase":"closed_summary","elapsed_ms":1});
            assert!(decode(&v.to_string()).is_none());
        }
    }
}

#[cfg(test)]
mod retention_contract_tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    #[test]
    fn same_token_finish_retains_terminal_fact_and_committed_stages_not_foreign_body() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE model_attempt(id TEXT,state TEXT,error_json TEXT,response_id TEXT,usage_json TEXT); INSERT INTO model_attempt VALUES('a','completed','{}','response-kept','{\"input_tokens\":3}');").unwrap();
        let fact = json!({"phase":"closed_summary","elapsed_ms":12,"outcome":{"kind":"failed","reason":"provider_error","origin":"provider_response","http_status":401}});
        let committed = json!({"closed_spill":{"groups":1,"source_nodes":2}});
        let raw=json!({"independent":7,"automatic_trim_attempt":{"token":"token","active_stage":"closed_summary","compaction_outcome":fact,"committed_stages":committed}}).to_string();
        db.execute("UPDATE model_attempt SET error_json=?1", [&raw])
            .unwrap();
        let final_body=json!({"outcome":"failed","compaction_outcome":{"forged":true},"committed_stages":{"forged":true}}).to_string();
        for token in ["old", ""] {
            assert_eq!(
                db.execute(
                    super::super::attempt::FINISH_SQL,
                    params![final_body, "a", token]
                )
                .unwrap(),
                0
            );
            assert_eq!(
                db.query_row("SELECT error_json FROM model_attempt", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                raw
            );
        }
        assert_eq!(
            db.execute(
                super::super::attempt::FINISH_SQL,
                params![final_body, "a", "token"]
            )
            .unwrap(),
            1
        );
        let saved: String = db
            .query_row("SELECT error_json FROM model_attempt", [], |r| r.get(0))
            .unwrap();
        let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["automatic_trim_attempt"]["compaction_outcome"], fact);
        assert_eq!(
            saved["automatic_trim_attempt"]["committed_stages"],
            committed
        );
        assert_eq!(saved["independent"], 7);
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
            (
                "completed".into(),
                "response-kept".into(),
                "{\"input_tokens\":3}".into()
            )
        );
        db.execute("UPDATE model_attempt SET error_json='{}'", [])
            .unwrap();
        assert_eq!(
            db.execute(
                super::super::attempt::FINISH_SQL,
                params![final_body, "a", "token"]
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn canonical_guards_have_one_closed_public_meaning_and_no_deterministic_failure() {
        for reason in [
            RejectionReason::Cancelled,
            RejectionReason::ThreadUnavailable,
            RejectionReason::SelectionChanged,
            RejectionReason::ProfileChanged,
            RejectionReason::SummarySettingsChanged,
            RejectionReason::EpochChanged,
            RejectionReason::RunStarted,
            RejectionReason::RunStopped,
            RejectionReason::TurnStarted,
            RejectionReason::TurnChanged,
            RejectionReason::NonappendGenerationChanged,
            RejectionReason::SelectedPrefixChanged,
            RejectionReason::CurrentTailChanged,
            RejectionReason::SourceChanged,
            RejectionReason::SourcePruned,
        ] {
            let error = rejected(reason, "original guard text");
            assert!(!cause(&error).deterministic());
            assert_eq!(error.code, crate::protocol::ErrorCode::InvalidState);
            assert_eq!(error.message, "original guard text");
            let fact = CompactionOutcomeFact {
                phase: CompactionPhase::ClosedSummary,
                elapsed_ms: 1,
                outcome: cause(&error),
            };
            assert_eq!(decode(&serde_json::to_string(&fact).unwrap()), Some(fact));
        }
    }
}

#[cfg(test)]
mod schema_contract_tests {
    #[test]
    fn deserialized_model_error_cannot_mint_stream_status_authority() {
        let mut error =
            crate::model::ModelError::provider(None, &serde_json::json!({"error":{"code":"400"}}));
        error.stream_provider_status = Some(crate::model::StreamProviderStatus {
            value: 400,
            source: crate::model::StreamProviderStatusSource::StreamErrorCode,
        });
        let serialized = serde_json::to_value(&error).unwrap();
        assert_eq!(serialized["stream_provider_status"]["value"], 400);
        let restored: crate::model::ModelError = serde_json::from_value(serialized).unwrap();
        assert!(restored.stream_provider_status.is_none());
        assert_eq!(restored.status, None);
    }

    use super::*;
    #[test]
    fn generated_schema_matches_terminal_decoder_vectors() {
        use serde_json::json;
        let generated: serde_json::Value =
            serde_json::from_str(&crate::protocol::schema_json()).unwrap();
        let schema = json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "$ref":"#/definitions/CompactionOutcomeFact",
            "definitions":generated["definitions"],
        });
        let validator = jsonschema::validator_for(&schema).unwrap();
        let fact = |outcome| json!({"phase":"closed_summary","elapsed_ms":1,"outcome":outcome});
        let check = |value: serde_json::Value, expected: bool| {
            assert_eq!(
                decode(&value.to_string()).is_some(),
                expected,
                "decoder: {value}"
            );
            assert_eq!(
                validator.is_valid(&value),
                expected,
                "generated schema: {value}"
            );
        };
        // Independent review counterexample: all individual field bounds pass,
        // but origin, HTTP and counts contradict the output-validation reason.
        check(
            fact(
                json!({"kind":"failed","reason":"refused","origin":"transport","http_status":401,"final_group_count":1}),
            ),
            false,
        );
        check(
            fact(json!({"kind":"failed","reason":"cancelled","origin":"internal"})),
            false,
        );
        check(
            fact(
                json!({"kind":"failed","reason":"provider_error","origin":"provider_response","final_source_nodes":1}),
            ),
            false,
        );
        check(
            fact(
                json!({"kind":"failed","reason":"context_limit_terminal","origin":"provider_response","http_status":400,"final_group_count":1,"final_source_nodes":2}),
            ),
            true,
        );
        check(
            fact(
                json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":400,"final_group_count":1,"final_source_nodes":2}),
            ),
            true,
        );
        check(
            fact(
                json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":200,"provider_status":413,"provider_status_source":"stream_error_code","final_group_count":1,"final_source_nodes":2}),
            ),
            true,
        );
        check(
            fact(
                json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":400,"provider_status":500,"provider_status_source":"stream_error_code","final_group_count":1,"final_source_nodes":2}),
            ),
            true,
        );
        for invalid in [
            json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":500,"final_group_count":1,"final_source_nodes":2}),
            json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":200,"final_group_count":1,"final_source_nodes":2}),
            json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":400,"final_group_count":2,"final_source_nodes":2}),
            json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":400,"final_group_count":1}),
            json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":429,"provider_status":400,"provider_status_source":"stream_error_code","final_group_count":1,"final_source_nodes":2}),
            json!({"kind":"failed","reason":"rejected_at_minimum","origin":"provider_response","http_status":400,"provider_status":400,"final_group_count":1,"final_source_nodes":2}),
        ] {
            check(fact(invalid), false);
        }
        check(
            fact(json!({"kind":"failed","reason":"not_shrinking","origin":"output_validation"})),
            true,
        );
        check(
            fact(json!({"kind":"rejected","reason":"run_started"})),
            true,
        );
        check(
            fact(json!({"kind":"rejected","reason":"run_started","http_status":401})),
            false,
        );
        for reason in [
            "provider_error",
            "context_limit_terminal",
            "rejected_at_minimum",
            "refused",
            "empty",
            "not_shrinking",
            "cancelled",
            "unclassified",
        ] {
            for origin in [
                "provider_response",
                "transport",
                "protocol",
                "preparation",
                "output_validation",
                "storage",
                "cancellation",
                "internal",
            ] {
                for metadata in [
                    json!({}),
                    json!({"http_status":null}),
                    json!({"final_group_count":null,"final_source_nodes":null}),
                    json!({"http_status":100}),
                    json!({"http_status":401}),
                    json!({"http_status":599}),
                    json!({"http_status":99}),
                    json!({"http_status":600}),
                    json!({"final_group_count":1}),
                    json!({"final_source_nodes":2}),
                    json!({"final_group_count":0}),
                    json!({"final_source_nodes":-1}),
                    json!({"final_group_count":1.5}),
                    json!({"final_source_nodes":9007199254740992u64}),
                    json!({"http_status":401,"final_group_count":1,"final_source_nodes":2}),
                    json!({"provider_code":"SECRET"}),
                    json!({"message":"SECRET"}),
                ] {
                    let mut outcome = json!({"kind":"failed","reason":reason,"origin":origin});
                    outcome
                        .as_object_mut()
                        .unwrap()
                        .extend(metadata.as_object().unwrap().clone());
                    let value = fact(outcome);
                    let accepted = decode(&value.to_string()).is_some();
                    assert_eq!(
                        validator.is_valid(&value),
                        accepted,
                        "schema/decoder drift: {value}"
                    );
                }
            }
        }
    }
    #[test]
    fn fact_schema_has_no_provider_code_message_or_private_capability() {
        let schema = serde_json::to_value(schemars::schema_for!(CompactionOutcomeFact)).unwrap();
        let text = schema.to_string();
        for forbidden in [
            "provider_code",
            "message",
            "compaction_cause",
            "details",
            "Other",
            "map_request_budget_exhausted",
        ] {
            assert!(
                !text.contains(forbidden),
                "forbidden outcome schema field: {forbidden}"
            );
        }
        assert_eq!(schema["additionalProperties"], false);
        let variants = schema["$defs"]["Outcome"]["oneOf"].as_array().unwrap();
        assert_eq!(variants.len(), 2);
        for variant in variants {
            assert_eq!(variant["additionalProperties"], false);
        }
        assert!(text.contains("9007199254740991"));
    }
}
