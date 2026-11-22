//! Provider-reported usage of summary requests (#133). One small `app_meta`
//! record per summarizing compaction, keyed under the Thread; no new table.
//! Unreported values remain unknown: no character or token estimate is made.
use super::*;
use crate::usage::StandardUsage;
use crate::usage_totals::Number;

/// Counters of one compaction's summary requests. A request is counted before
/// it is sent and its four values are unknown until the provider reports them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SummaryUsage {
    pub requests: u64,
    pub input_tokens: Number,
    pub cached_tokens: Number,
    pub cache_creation_tokens: Number,
    pub output_tokens: Number,
}
impl SummaryUsage {
    fn fields(&mut self) -> [&mut Number; 4] {
        [
            &mut self.input_tokens,
            &mut self.cached_tokens,
            &mut self.cache_creation_tokens,
            &mut self.output_tokens,
        ]
    }
    pub(crate) fn add_attempt(&mut self, usage: StandardUsage) {
        self.requests = self.requests.saturating_add(1);
        for (field, value) in self.fields().into_iter().zip([
            usage.input_tokens,
            usage.cached_tokens,
            usage.cache_creation_tokens,
            usage.output_tokens,
        ]) {
            field.add(value.map(u128::from), u64::from(value.is_none()));
        }
    }
    #[cfg(test)]
    fn begin(&mut self) {
        self.requests += 1;
        for field in self.fields() {
            field.add(None, 1);
        }
    }
    /// Resolve the latest pending request. `None` keeps all four values unknown.
    #[cfg(test)]
    fn observe(&mut self, usage: Option<StandardUsage>) {
        let Some(usage) = usage else { return };
        let values = [
            usage.input_tokens,
            usage.cached_tokens,
            usage.cache_creation_tokens,
            usage.output_tokens,
        ];
        for (field, value) in self.fields().into_iter().zip(values) {
            if let Some(value) = value {
                field.unknown_count = field.unknown_count.saturating_sub(1);
                field.add(Some(value as u128), 0);
            }
        }
    }
    pub(crate) fn merge(&mut self, other: &SummaryUsage) {
        self.requests = self.requests.saturating_add(other.requests);
        let mut theirs = other.clone();
        for (mine, theirs) in self.fields().into_iter().zip(theirs.fields()) {
            mine.overflow |= theirs.overflow;
            mine.add(theirs.known_sum.map(u128::from), theirs.unknown_count);
            if mine.overflow {
                mine.known_sum = None;
            }
        }
    }
    /// Re-encode a saved projection; anything else becomes unknown (null).
    pub(crate) fn sanitize(value: &Value) -> Value {
        serde_json::from_value::<SummaryUsage>(value.clone())
            .ok()
            .map_or(Value::Null, |usage| json!(usage))
    }
}
pub(super) struct SummaryRequests {
    pub(super) group: crate::model::request_path::SummaryGroup,
    pub(super) turn_id: Option<String>,
    usage: SummaryUsage,
}
impl SummaryRequests {
    pub(super) fn new(turn_id: Option<String>) -> Self {
        Self {
            group: crate::model::request_path::SummaryGroup {
                id: uid(),
                started_at: String::new(),
            },
            turn_id,
            usage: SummaryUsage::default(),
        }
    }
    pub(super) fn usage(&self) -> &SummaryUsage {
        &self.usage
    }
    pub(super) fn observe(&mut self, usage: StandardUsage) {
        self.usage.add_attempt(usage);
    }
}

/// Attach the facts to a failed compaction when requests were already sent.
pub(super) fn annotate(mut error: ApiError, usage: &SummaryUsage) -> ApiError {
    if usage.requests == 0 {
        return error;
    }
    match &mut error.details {
        Some(Value::Object(details)) => {
            details.insert("summary_usage".into(), json!(usage));
            details.insert("model_requests".into(), json!(usage.requests));
        }
        None => {
            error.details = Some(json!({"summary_usage":usage,"model_requests":usage.requests}))
        }
        Some(_) => {}
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reported(input: u64, output: u64, cached: Option<u64>) -> StandardUsage {
        StandardUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_tokens: cached,
            ..Default::default()
        }
    }
    #[test]
    fn unreported_values_stay_unknown_and_known_values_sum() {
        let mut usage = SummaryUsage::default();
        usage.begin();
        usage.observe(Some(reported(100, 10, Some(80))));
        usage.begin();
        usage.observe(None);
        usage.begin();
        usage.observe(Some(reported(50, 5, None)));
        assert_eq!(usage.requests, 3);
        assert_eq!(usage.input_tokens.known_sum, Some(150));
        assert_eq!(usage.input_tokens.unknown_count, 1);
        assert_eq!(usage.output_tokens.known_sum, Some(15));
        assert_eq!(usage.cached_tokens.known_sum, Some(80));
        assert_eq!(usage.cached_tokens.unknown_count, 2);
        assert_eq!(usage.cache_creation_tokens.known_sum, None);
        assert_eq!(usage.cache_creation_tokens.unknown_count, 3);
        let mut total = usage.clone();
        total.merge(&usage);
        assert_eq!(total.requests, 6);
        assert_eq!(total.input_tokens.known_sum, Some(300));
        assert_eq!(total.cache_creation_tokens.unknown_count, 6);
        assert_eq!(SummaryUsage::sanitize(&json!(usage)), json!(usage));
        assert_eq!(SummaryUsage::sanitize(&json!({"requests":-1})), Value::Null);
    }
}

#[cfg(test)]
mod failure_projection_tests {
    use super::*;
    #[test]
    fn terminal_error_exposes_admissions_and_partial_usage_without_a_new_ledger() {
        let mut usage = SummaryUsage::default();
        usage.begin();
        usage.observe(Some(StandardUsage {
            input_tokens: Some(7),
            ..Default::default()
        }));
        usage.begin();
        let original = ApiError::new(ErrorCode::InvalidState, "original result");
        let error = annotate(original.clone(), &usage);
        assert_eq!(error.code, original.code);
        assert_eq!(error.message, original.message);
        let details = error.details.unwrap();
        assert_eq!(details["model_requests"], 2);
        assert_eq!(details["summary_usage"]["requests"], 2);
        assert_eq!(details["summary_usage"]["input_tokens"]["known_sum"], 7);
        assert_eq!(details["summary_usage"]["input_tokens"]["unknown_count"], 1);
        assert_eq!(
            details["summary_usage"]["output_tokens"]["known_sum"],
            Value::Null
        );
        assert_eq!(
            annotate(original.clone(), &SummaryUsage::default()),
            original
        );
    }
}
