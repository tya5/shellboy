//! Read-only timing from durable shell facts, without inspecting OS liveness.
use chrono::{DateTime, Utc};
use rusqlite::Connection;

pub(crate) struct Timing {
    pub started_at: String,
    pub elapsed_ms: Option<u64>,
    pub last_output_at: Option<String>,
}

const TIMING_SQL: &str = "SELECT s.started_at,s.state,
    CASE WHEN json_type(s.shell_json,'$.completion.elapsed_ms')='integer'
      AND json_extract(s.shell_json,'$.completion.elapsed_ms') BETWEEN 0 AND 9223372036854775807
      THEN json_extract(s.shell_json,'$.completion.elapsed_ms') END,
    (SELECT received_at FROM output_chunk WHERE execution_id=s.id ORDER BY sequence DESC LIMIT 1)
    FROM shell_execution s WHERE s.id=?1";

pub(crate) fn read(
    db: &Connection,
    execution: &str,
    observed: DateTime<Utc>,
) -> rusqlite::Result<Timing> {
    db.query_row(TIMING_SQL, [execution], |row| {
        let started_at: String = row.get(0)?;
        let state: String = row.get(1)?;
        let saved: Option<i64> = row.get(2)?;
        let elapsed_ms = if matches!(state.as_str(), "starting" | "running" | "stopping") {
            DateTime::parse_from_rfc3339(&started_at).ok().map(|start| {
                observed
                    .signed_duration_since(start)
                    .num_milliseconds()
                    .max(0) as u64
            })
        } else {
            saved.map(|n| n as u64)
        };
        Ok(Timing {
            started_at,
            elapsed_ms,
            last_output_at: row.get(3)?,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','2026-10-29T00:00:00Z');
          INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','live','2026-10-29T00:00:00Z');
          INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','2026-10-29T00:00:00Z');
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)
          VALUES('s','r','u','op','running','/','{}','UTF-8','UTF-8','closed','2026-10-29T00:00:00Z');").unwrap();
        db
    }
    #[test]
    fn live_duration_grows_but_terminal_duration_is_saved_and_output_is_retained_only() {
        let db = fixture();
        let observed = DateTime::parse_from_rfc3339("2026-10-29T00:00:01Z")
            .unwrap()
            .to_utc();
        let first = read(&db, "s", observed).unwrap();
        assert_eq!(first.started_at, "2026-10-29T00:00:00Z");
        assert_eq!(first.elapsed_ms, Some(1000));
        assert_eq!(first.last_output_at, None);
        assert_eq!(
            read(&db, "s", observed + chrono::Duration::seconds(1))
                .unwrap()
                .elapsed_ms,
            Some(2000)
        );
        db.execute_batch(
            "INSERT INTO output_chunk VALUES('s',1,'stdout',0,X'61','2026-10-29T00:00:01Z');
          INSERT INTO output_chunk VALUES('s',2,'stderr',0,X'62','2026-10-29T00:00:00.5Z');",
        )
        .unwrap();
        // Sequence is authoritative even if the wall clock moved backwards.
        assert_eq!(
            read(&db, "s", observed).unwrap().last_output_at.as_deref(),
            Some("2026-10-29T00:00:00.5Z")
        );
        db.execute(
            "UPDATE shell_execution SET started_at='2026-10-30T00:00:00Z'",
            [],
        )
        .unwrap();
        assert_eq!(read(&db, "s", observed).unwrap().elapsed_ms, Some(0));
        db.execute("UPDATE shell_execution SET state='exited',shell_json='{\"completion\":{\"elapsed_ms\":42}}'", []).unwrap();
        assert_eq!(read(&db, "s", observed).unwrap().elapsed_ms, Some(42));
        db.execute("DELETE FROM output_chunk", []).unwrap();
        assert_eq!(read(&db, "s", observed).unwrap().last_output_at, None);
        db.execute("UPDATE shell_execution SET shell_json='{}'", [])
            .unwrap();
        assert_eq!(read(&db, "s", observed).unwrap().elapsed_ms, None);
    }
    #[test]
    fn last_output_uses_the_primary_key_without_a_sort_or_output_scan() {
        let db = fixture();
        let plans: Vec<String> = db
            .prepare(&format!("EXPLAIN QUERY PLAN {TIMING_SQL}"))
            .unwrap()
            .query_map(["s"], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plans
                .iter()
                .any(|p| p
                    .contains("SEARCH output_chunk USING INDEX sqlite_autoindex_output_chunk_1")),
            "{plans:?}"
        );
        assert!(
            !plans
                .iter()
                .any(|p| p.contains("TEMP B-TREE") || p.contains("SCAN output_chunk")),
            "{plans:?}"
        );
    }
}
