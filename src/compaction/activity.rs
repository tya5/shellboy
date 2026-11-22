use super::CompactionKeys;
use rusqlite::{Connection, OptionalExtension};

#[derive(serde::Deserialize)]
pub(crate) struct AutomaticCompactionWitness {
    run_id: String,
    turn_id: String,
    pub input_tokens: u64,
    pub threshold: u64,
    pub started_at: String,
}

/// Interpret the active witness only in the selected Run and Turn's live scope.
pub(crate) fn read_automatic_activity(
    db: &Connection,
    thread: &str,
    run_id: &str,
    run_state: &str,
    turn_id: &str,
    turn_state: &str,
) -> rusqlite::Result<Option<AutomaticCompactionWitness>> {
    if run_state != "live" || !matches!(turn_state, "preparing" | "running") {
        return Ok(None);
    }
    let body: Option<String> = db
        .query_row(
            "SELECT substr(value,1,4096) FROM app_meta WHERE key=?1",
            [CompactionKeys::new(thread).automatic_active()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(body
        .and_then(|body| serde_json::from_str::<AutomaticCompactionWitness>(&body).ok())
        .filter(|w| {
            w.run_id == run_id
                && w.turn_id == turn_id
                && w.threshold > 0
                && w.input_tokens >= w.threshold
        }))
}
