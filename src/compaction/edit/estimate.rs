//! Bounded calibration from observed usage, never a model capacity catalogue.
use super::*;

#[derive(Clone, Debug, Default)]
pub(in crate::compaction) struct TokenEstimate {
    ratio: Option<(u64, u64)>,
    overhead: u64,
    overhead_known: bool,
    failed_input_characters: Option<u64>,
    pub(in crate::compaction) attempt_id: Option<String>,
}
fn manifest_counts(body: &str) -> Option<(u64, u64)> {
    let value: Value = serde_json::from_str(body).ok()?;
    let input = value.get("input_characters")?.as_u64()?;
    let history = value.get("history_characters")?.as_u64()?;
    (input > 0 && history > 0 && history <= input).then_some((input, history))
}
impl TokenEstimate {
    pub(in crate::compaction) fn load(
        db: &Connection,
        thread: &str,
        profile: &str,
        model: &str,
        selection: i64,
    ) -> Result<Self, StorageError> {
        let mut estimate = Self::default();
        // Cap both rows and document size. Legacy/missing measurements are
        // unknown; do not invent a token count or propagate malformed metadata.
        let rows = db.prepare("SELECT a.id,CASE WHEN length(CAST(a.usage_json AS BLOB))<=65536 THEN a.usage_json END,CASE WHEN length(CAST(a.input_manifest_json AS BLOB))<=65536 THEN a.input_manifest_json END FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND a.state='completed' AND a.profile_id=?2 AND a.model_id=?3 ORDER BY a.rowid DESC LIMIT 64")?
            .query_map(params![thread,profile,model], |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (attempt, usage, manifest) in rows {
            let Some((input, history)) = manifest.as_deref().and_then(manifest_counts) else {
                continue;
            };
            let Some(tokens) = usage
                .as_deref()
                .and_then(|body| serde_json::from_str::<Value>(body).ok())
                .and_then(|usage| usage.get("input_tokens").and_then(Value::as_u64))
                .filter(|tokens| *tokens > 0)
            else {
                continue;
            };
            estimate.ratio = Some((tokens, input));
            estimate.overhead = input - history;
            estimate.overhead_known = true;
            estimate.attempt_id = Some(attempt);
            break;
        }
        let current: Option<(i64, Option<String>)> = db.query_row("SELECT a.selection_revision,CASE WHEN length(CAST(a.input_manifest_json AS BLOB))<=65536 THEN a.input_manifest_json END FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND a.profile_id=?2 AND a.model_id=?3 AND a.generation=u.generation AND a.state='inflight' AND u.state IN ('preparing','running','waiting') ORDER BY a.rowid DESC LIMIT 1", params![thread,profile,model], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((attempt_selection, manifest)) = current
            && let Some((input, history)) = manifest.as_deref().and_then(manifest_counts)
        {
            estimate.failed_input_characters = Some(input);
            if attempt_selection == selection {
                estimate.overhead = input - history;
                estimate.overhead_known = true;
            }
        }
        Ok(estimate)
    }
    fn tokens(&self, characters: u64) -> u64 {
        let (tokens, input) = self.ratio.unwrap_or((1, 4));
        ((characters as u128 * tokens as u128).div_ceil(input as u128)).min(u64::MAX as u128) as u64
    }
    pub(in crate::compaction) fn failed_input_tokens(&self) -> Option<u64> {
        self.failed_input_characters
            .map(|characters| self.tokens(characters))
    }
    pub(in crate::compaction) fn method(&self) -> &'static str {
        if self.ratio.is_some() {
            "same_thread_connection_model_usage_ratio"
        } else {
            "responses_history_characters_div_4"
        }
    }
    pub(in crate::compaction) fn overhead(&self) -> Option<u64> {
        self.overhead_known.then_some(self.overhead)
    }
}

/// Each projected item contributes its compact JSON characters plus one comma.
/// Internal event IDs, provenance and expired-epoch reasoning follow replay.
pub(in crate::compaction) fn projection_characters(payload: &Value, epoch: i64) -> u64 {
    crate::control::replay::convert(payload.clone(), epoch)
        .into_iter()
        .fold(0u64, |sum, item| {
            sum.saturating_add(item.to_string().chars().count() as u64)
                .saturating_add(1)
        })
}
pub(in crate::compaction) fn history_characters(snapshot: &Snapshot) -> u64 {
    let items = snapshot
        .groups
        .iter()
        .flat_map(|group| &group.sources)
        .map(|source| projection_characters(&source.payload, snapshot.epoch))
        .fold(0u64, u64::saturating_add);
    if items == 0 {
        2
    } else {
        items.saturating_add(1)
    }
}
pub(in crate::compaction) fn tokens_from_history_characters(
    snapshot: &Snapshot,
    characters: u64,
) -> u64 {
    snapshot
        .estimate
        .tokens(characters.saturating_add(snapshot.estimate.overhead))
}
pub(in crate::compaction) fn estimated_tokens(snapshot: &Snapshot) -> u64 {
    tokens_from_history_characters(snapshot, history_characters(snapshot))
}
/// Cache invalidation concerns a changed history suffix, not the fixed system
/// prefix. The ratio is shared with full-input estimates, the overhead is not.
pub(in crate::compaction) fn estimated_suffix_tokens(snapshot: &Snapshot, start: usize) -> u64 {
    let items = snapshot
        .groups
        .iter()
        .skip(start)
        .flat_map(|group| &group.sources)
        .map(|source| projection_characters(&source.payload, snapshot.epoch))
        .fold(0u64, u64::saturating_add);
    snapshot.estimate.tokens(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_zero_negative_and_inconsistent_measurements_are_unknown() {
        for body in [
            "{}",
            "{\"input_characters\":100}",
            "{\"input_characters\":100,\"history_characters\":0}",
            "{\"input_characters\":10,\"history_characters\":20}",
            "{\"input_characters\":-1,\"history_characters\":1}",
            "{\"input_characters\":\"100\",\"history_characters\":10}",
            "not JSON",
        ] {
            assert_eq!(manifest_counts(body), None);
        }
        assert_eq!(
            manifest_counts("{\"input_characters\":100,\"history_characters\":80}"),
            Some((100, 80))
        );
    }
}
