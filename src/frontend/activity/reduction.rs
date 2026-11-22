//! A bounded display-only projection from the existing automatic-trim evidence.
use rusqlite::{Connection, OptionalExtension};

// Search only the latest 256 attempts in this Turn. The indexed reverse walk
// never loads full result_json into Rust, and only integer facts leave SQLite.
// A missing result means no evidence in this bounded window, not "never trimmed".
const SUMMARY: &str = "WITH recent AS (
 SELECT generation,CASE WHEN generation=?2 THEN CASE WHEN json_valid(result_json) THEN
  CASE WHEN json_type(result_json,'$.metrics')='object'
   AND json_type(result_json,'$.metrics.before_characters')='integer'
   AND json_type(result_json,'$.metrics.after_characters')='integer'
   AND json_type(result_json,'$.metrics.compact.spilled_groups')='integer'
   AND json_type(result_json,'$.metrics.omitted_nodes')='integer'
   AND typeof(json_extract(result_json,'$.metrics.before_characters'))='integer'
   AND typeof(json_extract(result_json,'$.metrics.after_characters'))='integer'
   AND typeof(json_extract(result_json,'$.metrics.compact.spilled_groups'))='integer'
   AND typeof(json_extract(result_json,'$.metrics.omitted_nodes'))='integer'
   AND json_extract(result_json,'$.metrics.before_characters')>0
   AND json_extract(result_json,'$.metrics.after_characters')>=0
   AND json_extract(result_json,'$.metrics.after_characters')<json_extract(result_json,'$.metrics.before_characters')
   AND json_extract(result_json,'$.metrics.compact.spilled_groups')>=0
   AND json_extract(result_json,'$.metrics.omitted_nodes')>=0
   AND (json_extract(result_json,'$.metrics.compact.spilled_groups')>0 OR json_extract(result_json,'$.metrics.omitted_nodes')>0)
   AND json_extract(result_json,'$.metrics.compact.no_target') IS NOT 1
  THEN json_array(
   json_extract(result_json,'$.metrics.before_characters'),
   json_extract(result_json,'$.metrics.after_characters'),
   json_extract(result_json,'$.metrics.compact.spilled_groups'),
   json_extract(result_json,'$.metrics.omitted_nodes')) END END END AS facts
 FROM model_attempt a INDEXED BY attempts_by_turn LEFT JOIN automatic_compaction c ON c.attempt_id=a.id WHERE a.turn_id=?1
 ORDER BY a.started_at DESC,a.rowid DESC LIMIT 256
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

pub(super) fn prepare(
    facts: Option<String>,
) -> Option<crate::ui::activity::ContextReductionSummary> {
    facts
        .and_then(|body| serde_json::from_str::<[u64; 4]>(&body).ok())
        .map(
            |[
                before_characters,
                after_characters,
                spilled_groups,
                omitted_items,
            ]| {
                crate::ui::activity::ContextReductionSummary {
                    before_characters,
                    after_characters,
                    spilled_groups,
                    omitted_items,
                }
            },
        )
}
