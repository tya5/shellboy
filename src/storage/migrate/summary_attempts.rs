use super::StorageError;
use crate::model::request_path::SummaryGroup;
use crate::usage::StandardUsage;
use crate::usage_totals::Number;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::Deserialize;
use serde_json::{Value, json};

const PREFIX: &str = "summary.usage.";

#[derive(Deserialize)]
struct Ledger {
    id: String,
    thread_id: String,
    turn_id: Option<String>,
    run_id: Option<String>,
    profile_id: String,
    model_id: String,
    #[serde(default)]
    started_at: String,
    updated_at: String,
    requests: u64,
    input_tokens: Number,
    cached_tokens: Number,
    cache_creation_tokens: Number,
    output_tokens: Number,
    #[serde(default)]
    rate_observations: Vec<Value>,
}

pub(super) fn convert(tx: &Transaction<'_>) -> Result<(), StorageError> {
    let mut records = tx.prepare("SELECT key,value FROM app_meta WHERE key>=?1 AND key<'summary.usage/' AND key>?2 ORDER BY key LIMIT 1")?;
    let mut key = String::new();
    while let Some((next, raw)) = records
        .query_row(params![PREFIX, key], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()?
    {
        key = next;
        let invalid = |reason| StorageError::Schema(format!("summary ledger {key}: {reason}"));
        let record: Ledger = serde_json::from_str(&raw).map_err(|_| invalid("invalid record"))?;
        if key != format!("{PREFIX}{}.{}", record.thread_id, record.id) || record.requests == 0 {
            return Err(invalid("invalid identity or request count"));
        }
        if let Some(turn) = &record.turn_id {
            let scope = tx
                .query_row(
                    "SELECT thread_id,run_id FROM turn WHERE id=?1",
                    [turn],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            if scope
                .as_ref()
                .map(|(thread, run)| (thread.as_str(), Some(run.as_str())))
                != Some((record.thread_id.as_str(), record.run_id.as_deref()))
            {
                return Err(invalid("Turn does not match saved Run and Thread"));
            }
        } else if record.run_id.is_some() {
            return Err(invalid("Run without a usable Turn"));
        }
        let fields = [
            &record.input_tokens,
            &record.cached_tokens,
            &record.cache_creation_tokens,
            &record.output_tokens,
        ];
        for field in fields {
            if field.overflow
                || field.unknown_count > record.requests
                || field.known_sum.is_some_and(|n| n > i64::MAX as u64)
                || field.known_sum.is_some() != (field.unknown_count < record.requests)
            {
                return Err(invalid("unrepresentable token sum or counts"));
            }
        }
        for ordinal in 0..record.requests {
            let value = |field: &Number| {
                (ordinal < record.requests - field.unknown_count).then(|| {
                    if ordinal == 0 {
                        field.known_sum.expect("validated known position")
                    } else {
                        0
                    }
                })
            };
            let usage = StandardUsage {
                input_tokens: value(&record.input_tokens),
                cached_tokens: value(&record.cached_tokens),
                cache_creation_tokens: value(&record.cache_creation_tokens),
                output_tokens: value(&record.output_tokens),
                ..Default::default()
            };
            let id = format!("{key}:{ordinal}");
            let manifest = json!({"summary":SummaryGroup{id:record.id.clone(),started_at:record.started_at.clone()},
                "rate_observation":record.rate_observations.get(ordinal as usize)});
            tx.execute("INSERT INTO model_attempt(id,kind,thread_id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at) VALUES(?1,'summary',?2,?3,0,0,?4,?5,0,'incomplete',?6,?7,?8,?9)",
                params![id,record.thread_id,record.turn_id,record.profile_id,record.model_id,
                        manifest.to_string(),usage.to_value().to_string(),record.started_at,record.updated_at])?;
        }
        tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])?;
    }
    Ok(())
}
