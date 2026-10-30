//! A bounded display-only projection from the existing automatic-trim evidence.
use rusqlite::{Connection, OptionalExtension};

// Search only the latest 256 attempts in this Turn. The indexed reverse walk
// never loads full error_json into Rust, and only integer facts leave SQLite.
// A missing result means no evidence in this bounded window, not "never trimmed".
const SUMMARY: &str = "WITH recent AS (
 SELECT generation,CASE WHEN generation=?2 THEN CASE WHEN json_valid(error_json) THEN
  CASE WHEN json_type(error_json,'$.automatic_trim')='object'
   AND json_type(error_json,'$.automatic_trim.before_characters')='integer'
   AND json_type(error_json,'$.automatic_trim.after_characters')='integer'
   AND json_type(error_json,'$.automatic_trim.compact.spilled_groups')='integer'
   AND json_type(error_json,'$.automatic_trim.omitted_nodes')='integer'
   AND typeof(json_extract(error_json,'$.automatic_trim.before_characters'))='integer'
   AND typeof(json_extract(error_json,'$.automatic_trim.after_characters'))='integer'
   AND typeof(json_extract(error_json,'$.automatic_trim.compact.spilled_groups'))='integer'
   AND typeof(json_extract(error_json,'$.automatic_trim.omitted_nodes'))='integer'
   AND json_extract(error_json,'$.automatic_trim.before_characters')>0
   AND json_extract(error_json,'$.automatic_trim.after_characters')>=0
   AND json_extract(error_json,'$.automatic_trim.after_characters')<json_extract(error_json,'$.automatic_trim.before_characters')
   AND json_extract(error_json,'$.automatic_trim.compact.spilled_groups')>=0
   AND json_extract(error_json,'$.automatic_trim.omitted_nodes')>=0
   AND (json_extract(error_json,'$.automatic_trim.compact.spilled_groups')>0 OR json_extract(error_json,'$.automatic_trim.omitted_nodes')>0)
   AND json_extract(error_json,'$.automatic_trim.compact.no_target') IS NOT 1
  THEN json_array(
   json_extract(error_json,'$.automatic_trim.before_characters'),
   json_extract(error_json,'$.automatic_trim.after_characters'),
   json_extract(error_json,'$.automatic_trim.compact.spilled_groups'),
   json_extract(error_json,'$.automatic_trim.omitted_nodes')) END END END AS facts
 FROM model_attempt INDEXED BY attempts_by_turn WHERE turn_id=?1
 ORDER BY started_at DESC,rowid DESC LIMIT 256
 ) SELECT facts FROM recent WHERE generation=?2 AND facts IS NOT NULL LIMIT 1";

pub(super) fn read_tx(
    db: &Connection,
    turn: &str,
    generation: i64,
) -> Result<Option<String>, rusqlite::Error> {
    db.query_row(SUMMARY, rusqlite::params![turn, generation], |row| {
        row.get(0)
    })
    .optional()
}

pub(super) fn prepare(facts: Option<String>) -> Option<String> {
    facts
        .and_then(|body| serde_json::from_str::<[u64; 4]>(&body).ok())
        .map(|[before, after, spilled, omitted]| {
            format!("自動縮小済み · spill {spilled}組 · 省略 {omitted}件 · {before}→{after} chars")
        })
}
