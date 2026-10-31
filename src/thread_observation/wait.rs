use crate::protocol::{ApiError, ErrorCode, WaitCondition};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet};

type Result<T> = std::result::Result<T, ApiError>;
const LIMIT: usize = 64;
const CURSOR_BYTES: usize = 65_536;
const RUN_NEXT: &str = "SELECT rowid,id,ended_at FROM run INDEXED BY wait_run_state WHERE thread_id=?1 AND state=?2 AND rowid>?3 ORDER BY rowid LIMIT 1";
const RUN_HEAD: &str = "SELECT rowid,id,ended_at FROM run INDEXED BY wait_run_state WHERE thread_id=?1 AND state=?2 ORDER BY rowid DESC LIMIT 1";
const RECOVERY_NEXT: &str = "SELECT rowid,id,json_extract(payload_ref_json,'$.run_id'),json_extract(payload_ref_json,'$.turn_id'),created_at FROM event INDEXED BY wait_recovery_thread WHERE kind='child_recovery_wait' AND json_extract(payload_ref_json,'$.thread_id')=?1 AND rowid>?2 ORDER BY rowid LIMIT 1";
const RECOVERY_HEAD: &str = "SELECT rowid,id,json_extract(payload_ref_json,'$.run_id'),json_extract(payload_ref_json,'$.turn_id'),created_at FROM event INDEXED BY wait_recovery_thread WHERE kind='child_recovery_wait' AND json_extract(payload_ref_json,'$.thread_id')=?1 ORDER BY rowid DESC LIMIT 1";
const CURRENT_RUN: &str = "SELECT id,state FROM run INDEXED BY one_live_run WHERE thread_id=?1 AND state IN ('live','stopping') LIMIT 1";
const LAST_RUN: &str = "SELECT id,state,ended_at FROM run INDEXED BY run_thread_created WHERE thread_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1";
const CURRENT_RECOVERY: &str = "SELECT id FROM turn INDEXED BY one_open_turn WHERE thread_id=?1 AND run_id=?2 AND state IN ('preparing','running','waiting','recovery_wait') AND state='recovery_wait' LIMIT 1";

fn invalid(text: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, text)
}
fn expired() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "Thread wait cursor does not match this query or its retained source anchors",
    )
}
fn sql(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("Thread observation failed: {error}"))
}

pub(crate) fn validate(conditions: &[WaitCondition], after: Option<&str>) -> Result<()> {
    let mut count = 0;
    for condition in conditions {
        if let WaitCondition::ThreadSettled { thread_ids, states } = condition {
            count += thread_ids.len();
            if thread_ids.is_empty()
                || count > LIMIT
                || thread_ids.iter().any(|id| id.is_empty() || id.len() > 128)
                || thread_ids.iter().collect::<HashSet<_>>().len() != thread_ids.len()
                || states.is_empty()
                || states.len() > 4
                || states.iter().any(|s| {
                    !matches!(
                        s.as_str(),
                        "completed" | "stopped" | "failed" | "recovery_wait"
                    )
                })
                || states.iter().collect::<HashSet<_>>().len() != states.len()
            {
                return Err(invalid(
                    "thread_settled requires 1..=64 total unique Thread IDs per condition (128 bytes each) and distinct completed/stopped/failed/recovery_wait states",
                ));
            }
        }
    }
    if after.is_some_and(|s| count == 0 || s.is_empty() || s.len() > CURSOR_BYTES) {
        return Err(invalid(
            "after requires a thread_settled condition and a bounded nonempty cursor",
        ));
    }
    Ok(())
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    row: i64,
    id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    scope: String,
    positions: Vec<Position>,
    next: usize,
}
#[derive(Clone)]
struct Saved {
    position: Position,
    value: Value,
}

pub(crate) struct Report {
    pub changes: Vec<Value>,
    pub cursor: String,
    pub more: bool,
    pub targets: Vec<String>,
    pub unavailable: bool,
}

fn fetch(db: &Connection, thread: &str, state: &str, row: Option<i64>) -> Result<Option<Saved>> {
    if state == "recovery_wait" {
        let mut stmt = db
            .prepare(if row.is_some() {
                RECOVERY_NEXT
            } else {
                RECOVERY_HEAD
            })
            .map_err(sql)?;
        let read = |r: &rusqlite::Row<'_>| {
            Ok(Saved {
                position: Position {
                    row: r.get(0)?,
                    id: r.get(1)?,
                },
                value: json!({"thread_id":thread,"run_id":r.get::<_,String>(2)?,"turn_id":r.get::<_,String>(3)?,"state":state,"at":r.get::<_,String>(4)?}),
            })
        };
        if let Some(row) = row {
            stmt.query_row(params![thread, row], read)
                .optional()
                .map_err(sql)
        } else {
            stmt.query_row([thread], read).optional().map_err(sql)
        }
    } else {
        let mut stmt = db
            .prepare(if row.is_some() { RUN_NEXT } else { RUN_HEAD })
            .map_err(sql)?;
        let read = |r: &rusqlite::Row<'_>| {
            Ok(Saved {
                position: Position {
                    row: r.get(0)?,
                    id: r.get(1)?,
                },
                value: json!({"thread_id":thread,"run_id":r.get::<_,String>(1)?,"state":state,"at":r.get::<_,Option<String>>(2)?}),
            })
        };
        if let Some(row) = row {
            stmt.query_row(params![thread, state, row], read)
                .optional()
                .map_err(sql)
        } else {
            stmt.query_row(params![thread, state], read)
                .optional()
                .map_err(sql)
        }
    }
}

fn anchor(db: &Connection, thread: &str, state: &str, position: &Position) -> Result<()> {
    if position.row == 0 {
        return if position.id.is_empty() {
            Ok(())
        } else {
            Err(expired())
        };
    }
    if position.row < 0 || position.id.is_empty() || position.id.len() > 128 {
        return Err(expired());
    }
    let query = if state == "recovery_wait" {
        "SELECT id FROM event WHERE rowid=?1 AND kind='child_recovery_wait' AND json_extract(payload_ref_json,'$.thread_id')=?2 AND ?3='recovery_wait'"
    } else {
        "SELECT id FROM run WHERE rowid=?1 AND thread_id=?2 AND state=?3"
    };
    let found: Option<String> = db
        .query_row(query, params![position.row, thread, state], |r| r.get(0))
        .optional()
        .map_err(sql)?;
    if found.as_deref() != Some(&position.id) {
        return Err(expired());
    }
    Ok(())
}

fn current(
    db: &Connection,
    thread: &str,
    states: &HashSet<&str>,
    head: Option<&Saved>,
) -> Result<Option<Value>> {
    let live: Option<(String, String)> = db
        .query_row(CURRENT_RUN, [thread], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(sql)?;
    if let Some((run, run_state)) = live {
        if run_state == "live" && states.contains("recovery_wait") {
            let turn: Option<String> = db
                .query_row(CURRENT_RECOVERY, params![thread, &run], |r| r.get(0))
                .optional()
                .map_err(sql)?;
            if let Some(turn) = turn {
                let at = head
                    .filter(|s| s.value["run_id"] == run && s.value["turn_id"] == turn)
                    .and_then(|s| s.value.get("at"))
                    .cloned()
                    .unwrap_or(Value::Null);
                return Ok(Some(
                    json!({"thread_id":thread,"run_id":run,"turn_id":turn,"state":"recovery_wait","at":at}),
                ));
            }
        }
        return Ok(None);
    }
    let last: Option<(String, String, Option<String>)> = db
        .query_row(LAST_RUN, [thread], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .map_err(sql)?;
    Ok(last
        .filter(|(_, state, _)| states.contains(state.as_str()))
        .map(|(run, state, at)| json!({"thread_id":thread,"run_id":run,"state":state,"at":at})))
}

pub(crate) fn observe(
    db: &Connection,
    conditions: &[WaitCondition],
    after: Option<&str>,
    initial: bool,
) -> Result<Option<Report>> {
    validate(conditions, after)?;
    let mut streams = BTreeSet::new();
    for condition in conditions {
        if let WaitCondition::ThreadSettled { thread_ids, states } = condition {
            for thread in thread_ids {
                for state in states {
                    streams.insert((thread.clone(), state.clone()));
                }
            }
        }
    }
    if streams.is_empty() {
        return Ok(None);
    }
    let streams: Vec<_> = streams.into_iter().collect();
    let scope = crate::digest::sha256_hex(
        serde_json::to_string(conditions)
            .map_err(|_| invalid("Cannot encode wait scope"))?
            .as_bytes(),
    );
    let mut cursor = if let Some(after) = after {
        let bytes = URL_SAFE_NO_PAD.decode(after).map_err(|_| expired())?;
        let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| expired())?;
        if cursor.version != 1
            || cursor.scope != scope
            || cursor.positions.len() != streams.len()
            || cursor.next >= streams.len()
        {
            return Err(expired());
        }
        cursor
    } else {
        Cursor {
            version: 1,
            scope,
            positions: vec![Position::default(); streams.len()],
            next: 0,
        }
    };
    let threads: BTreeSet<_> = streams.iter().map(|s| s.0.clone()).collect();
    let mut unavailable = HashSet::new();
    for thread in &threads {
        let saved: Option<Option<String>> = db
            .query_row("SELECT deleted_at FROM thread WHERE id=?1", [thread], |r| {
                r.get(0)
            })
            .optional()
            .map_err(sql)?;
        match saved {
            None if initial && after.is_none() => {
                return Err(ApiError::new(
                    ErrorCode::TargetUnavailable,
                    "Unknown Thread wait target",
                )
                .details(json!({"thread_id":thread})));
            }
            None | Some(Some(_)) => {
                unavailable.insert(thread.clone());
            }
            Some(None) => {}
        }
    }
    let mut changes = Vec::new();
    let mut more = false;
    if after.is_none() {
        let mut recovery = std::collections::HashMap::new();
        for (i, (thread, state)) in streams.iter().enumerate() {
            if unavailable.contains(thread) {
                continue;
            }
            if let Some(saved) = fetch(db, thread, state, None)? {
                cursor.positions[i] = saved.position.clone();
                if state == "recovery_wait" {
                    recovery.insert(thread.clone(), saved);
                }
            }
        }
        for thread in &threads {
            if unavailable.contains(thread) {
                continue;
            }
            let states = streams
                .iter()
                .filter(|s| s.0 == *thread)
                .map(|s| s.1.as_str())
                .collect();
            if let Some(change) = current(db, thread, &states, recovery.get(thread))? {
                changes.push(change);
            }
        }
    } else {
        let mut candidates = Vec::new();
        for (i, (thread, state)) in streams.iter().enumerate() {
            if unavailable.contains(thread) {
                candidates.push(None);
                continue;
            }
            anchor(db, thread, state, &cursor.positions[i])?;
            candidates.push(fetch(db, thread, state, Some(cursor.positions[i].row))?);
        }
        while changes.len() < LIMIT {
            let found = (0..streams.len())
                .map(|offset| (cursor.next + offset) % streams.len())
                .find(|i| candidates[*i].is_some());
            let Some(i) = found else { break };
            let saved = candidates[i].take().expect("selected ready stream");
            cursor.positions[i] = saved.position;
            changes.push(saved.value);
            cursor.next = (i + 1) % streams.len();
            candidates[i] = fetch(
                db,
                &streams[i].0,
                &streams[i].1,
                Some(cursor.positions[i].row),
            )?;
        }
        more = candidates.iter().any(Option::is_some);
    }
    let cursor = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&cursor).map_err(|_| invalid("Cannot encode Thread wait cursor"))?,
    );
    if cursor.len() > CURSOR_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Thread wait cursor exceeds its bound",
        ));
    }
    Ok(Some(Report {
        changes,
        cursor,
        more,
        targets: threads
            .into_iter()
            .map(|id| format!("thread:{id}"))
            .collect(),
        unavailable: !unavailable.is_empty(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db
    }
    fn thread(db: &Connection, id: &str) {
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?1,'/','/AGENTS.md','p','m','now')",[id]).unwrap();
    }
    fn conditions(ids: &[&str]) -> Vec<WaitCondition> {
        vec![WaitCondition::ThreadSettled {
            thread_ids: ids.iter().map(|s| (*s).into()).collect(),
            states: vec![
                "completed".into(),
                "recovery_wait".into(),
                "failed".into(),
                "stopped".into(),
            ],
        }]
    }
    #[test]
    fn actual_hot_queries_seek_bounded_thread_state_ranges() {
        let db = db();
        thread(&db, "t");
        for (query, bind, required) in [
            (
                RUN_NEXT,
                vec![
                    rusqlite::types::Value::Text("t".into()),
                    rusqlite::types::Value::Text("completed".into()),
                    rusqlite::types::Value::Integer(0),
                ],
                "wait_run_state",
            ),
            (
                RUN_HEAD,
                vec![
                    rusqlite::types::Value::Text("t".into()),
                    rusqlite::types::Value::Text("completed".into()),
                ],
                "wait_run_state",
            ),
            (
                RECOVERY_NEXT,
                vec![
                    rusqlite::types::Value::Text("t".into()),
                    rusqlite::types::Value::Integer(0),
                ],
                "wait_recovery_thread",
            ),
            (
                RECOVERY_HEAD,
                vec![rusqlite::types::Value::Text("t".into())],
                "wait_recovery_thread",
            ),
            (
                CURRENT_RUN,
                vec![rusqlite::types::Value::Text("t".into())],
                "one_live_run",
            ),
            (
                LAST_RUN,
                vec![rusqlite::types::Value::Text("t".into())],
                "run_thread_created",
            ),
            (
                CURRENT_RECOVERY,
                vec![
                    rusqlite::types::Value::Text("t".into()),
                    rusqlite::types::Value::Text("r".into()),
                ],
                "one_open_turn",
            ),
        ] {
            let plan = db
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap()
                .query_map(rusqlite::params_from_iter(bind), |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n");
            assert!(plan.contains("SEARCH") && plan.contains(required), "{plan}");
            assert!(
                !plan.contains("SCAN") && !plan.contains("TEMP B-TREE"),
                "{plan}"
            );
            if query == RUN_NEXT || query == RECOVERY_NEXT {
                assert!(plan.contains("rowid>?"), "{plan}");
            }
        }
    }
    #[test]
    fn bounded_cursor_catches_finish_restart_and_recovery_resume_without_clock_order() {
        let db = db();
        thread(&db, "a");
        thread(&db, "b");
        let cond = conditions(&["a", "b"]);
        let baseline = observe(&db, &cond, None, true).unwrap().unwrap();
        assert!(baseline.changes.is_empty());
        for i in 0..130 {
            db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed',?3,?3)",params![format!("r{i}"),if i%2==0 {"a"}else{"b"},format!("2000-{i:04}")]).unwrap();
        }
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('active','a','live','3000')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,generation)VALUES('u','a','active','preparing','3000',1)",[]).unwrap();
        db.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('recovery','child_recovery_wait','{}','a',?1,'1999')",[json!({"thread_id":"a","run_id":"active","turn_id":"u","generation":0,"state":"recovery_wait"}).to_string()]).unwrap();
        let mut cursor = baseline.cursor;
        let mut changes = Vec::new();
        loop {
            let report = observe(&db, &cond, Some(&cursor), true).unwrap().unwrap();
            assert!(report.changes.len() <= 64);
            changes.extend(report.changes);
            cursor = report.cursor;
            if !report.more {
                break;
            }
        }
        assert_eq!(changes.len(), 131);
        assert_eq!(
            changes.iter().filter(|c| c["state"] == "completed").count(),
            130
        );
        assert_eq!(
            changes
                .iter()
                .filter(|c| c["state"] == "recovery_wait")
                .count(),
            1
        );
        assert!(
            observe(&db, &cond, Some(&cursor), true)
                .unwrap()
                .unwrap()
                .changes
                .is_empty()
        );
        let mut reversed = cond.clone();
        if let WaitCondition::ThreadSettled { thread_ids, .. } = &mut reversed[0] {
            thread_ids.reverse();
        }
        assert_eq!(
            observe(&db, &reversed, Some(&cursor), true)
                .err()
                .unwrap()
                .code,
            ErrorCode::CursorExpired
        );
        assert_eq!(
            observe(&db, &cond, Some("invalid"), true)
                .err()
                .unwrap()
                .code,
            ErrorCode::CursorExpired
        );
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='a'", [])
            .unwrap();
        assert!(
            observe(&db, &cond, Some(&cursor), true)
                .unwrap()
                .unwrap()
                .unavailable
        );
    }
    #[test]
    fn current_active_run_is_not_an_old_completion_and_invalid_targets_are_rejected() {
        let db = db();
        thread(&db, "a");
        db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('old','a','completed','2000','2001')",[]).unwrap();
        let cond = conditions(&["a"]);
        assert_eq!(
            observe(&db, &cond, None, true).unwrap().unwrap().changes[0]["run_id"],
            "old"
        );
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('new','a','live','2002')",
            [],
        )
        .unwrap();
        assert!(
            observe(&db, &cond, None, true)
                .unwrap()
                .unwrap()
                .changes
                .is_empty()
        );
        assert_eq!(
            observe(&db, &conditions(&["missing"]), None, true)
                .err()
                .unwrap()
                .code,
            ErrorCode::TargetUnavailable
        );
        let ids = vec!["same"; 65];
        assert!(validate(&conditions(&ids), None).is_err());
        assert!(validate(&conditions(&["a", "a"]), None).is_err());
        assert!(validate(&[], Some("cursor")).is_err());
    }
}
