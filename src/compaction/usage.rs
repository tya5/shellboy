//! Provider-reported usage of summary requests (#133). One small `app_meta`
//! record per summarizing compaction, keyed under the Thread; no new table.
//! Unreported values remain unknown: no character or token estimate is made.
use super::*;
use crate::usage::StandardUsage;
use crate::usage_totals::Number;

/// Counters of one compaction's summary requests. A request is counted before
/// it is sent and its four values are unknown until the provider reports them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub(super) fn begin(&mut self) {
        self.requests += 1;
        for field in self.fields() {
            field.add(None, 1);
        }
    }
    /// Resolve the latest pending request. `None` keeps all four values unknown.
    pub(super) fn observe(&mut self, usage: Option<StandardUsage>) {
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
pub(crate) fn usage_of(response: Option<&Value>) -> Option<StandardUsage> {
    response
        .and_then(|raw| raw.get("usage"))
        .filter(|usage| usage.is_object())
        .map(StandardUsage::from_value)
}

/// The durable record. `turn_id`/`run_id` are the Thread's current Turn/Run
/// when the compaction snapshot was taken (null outside a live Run).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct UsageRecord {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
    pub profile_id: String,
    pub model_id: String,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rate_observations: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_observation_pending: Option<u64>,
    #[serde(flatten)]
    pub usage: SummaryUsage,
}
pub(crate) const RECORD_PREFIX: &str = "summary.usage.";
/// Bounds each per-summary accounting record, including retry observations.
pub(crate) const RECORD_MAX_BYTES: i64 = 2 * 1024 * 1024;
pub(crate) fn record_key(thread: &str, id: &str) -> String {
    format!("{RECORD_PREFIX}{thread}.{id}")
}
/// Half-open key range of one Thread's records; '/' follows '.' in byte order.
/// `COMPACTION_USAGE_SQL` builds the same bounds.
#[cfg(test)]
fn thread_range(thread: &str) -> (String, String) {
    (
        format!("{RECORD_PREFIX}{thread}."),
        format!("{RECORD_PREFIX}{thread}/"),
    )
}

pub(super) struct UsageLedger {
    record: UsageRecord,
}
impl UsageLedger {
    pub(super) fn new(snapshot: &Snapshot) -> Self {
        let now = timestamp();
        Self {
            record: UsageRecord {
                id: uid(),
                thread_id: snapshot.thread.clone(),
                turn_id: snapshot.current_turn.clone(),
                run_id: snapshot.run_fence.clone(),
                profile_id: snapshot.profile.clone(),
                model_id: snapshot.model.clone(),
                started_at: now.clone(),
                updated_at: now,
                rate_observations: Vec::new(),
                rate_observation_pending: None,
                usage: SummaryUsage::default(),
            },
        }
    }
    pub(super) fn usage(&self) -> &SummaryUsage {
        &self.record.usage
    }
    /// Count the request durably before it is sent, so an interrupted request
    /// remains a request with unknown usage rather than disappearing.
    pub(super) async fn begin(&mut self, database: &DbWorker) -> Result<usize, ApiError> {
        if self.record.usage.requests == 0 {
            self.record.started_at = timestamp();
        }
        self.record.usage.begin();
        let index = self.record.rate_observations.len();
        self.record.rate_observation_pending = Some(index as u64);
        self.record.rate_observations.push(Value::Null);
        self.save(database).await?;
        Ok(index)
    }
    pub(super) async fn observe(
        &mut self,
        database: &DbWorker,
        usage: Option<StandardUsage>,
    ) -> Result<(), ApiError> {
        if usage.is_none() {
            return Ok(());
        }
        self.record.usage.observe(usage);
        self.save(database).await
    }
    pub(super) async fn observe_http(
        &mut self,
        database: &DbWorker,
        index: usize,
        facts: crate::model::http::ResponseFacts,
    ) -> Result<(), ApiError> {
        let mut observation = facts.rate_metadata();
        observation["applied_wait_ms"] = json!(facts.admission_wait_ms.unwrap_or(0));
        if let Some(slot) = self.record.rate_observations.get_mut(index) {
            *slot = observation;
        }
        if self.record.rate_observations.iter().all(|o| !o.is_null()) {
            self.record.rate_observation_pending = None;
        }
        self.save(database).await
    }
    async fn save(&mut self, database: &DbWorker) -> Result<(), ApiError> {
        self.record.updated_at = timestamp();
        let key = record_key(&self.record.thread_id, &self.record.id);
        let body = serde_json::to_string(&self.record).expect("summary usage record");
        database
            .call(move |db| {
                db.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2)ON CONFLICT(key)DO UPDATE SET value=excluded.value",params![key,body])?;
                Ok(())
            })
            .await
            .map_err(storage_error)
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
        }
        None => error.details = Some(json!({"summary_usage":usage})),
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
    #[test]
    fn record_keys_scope_one_thread_by_range() {
        let (low, high) = thread_range("a");
        let own = record_key("a", "x");
        let other = record_key("a.b", "x");
        assert!(own >= low && own < high);
        // A dotted Thread ID can share the prefix; readers also compare thread_id.
        assert!(other >= low && other < high);
        assert!(record_key("ab", "x") >= high || record_key("ab", "x") < low);
    }
}
