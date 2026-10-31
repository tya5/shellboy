//! Visible Watch metadata and bounded witnesses in the caller's read snapshot.
use crate::{
    control::db::thread_summary,
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::{
        live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget},
        watch::{PreparedWatchDetail, WatchAge, WatchAgeOrigin, WatchDetail},
    },
};
use rusqlite::Connection;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

// Exact-ID seeks only. Raw JSON witnesses are copied after a global64KiB
// budget, never by first collecting full bodies in the materialized facts.
const DETAILS: &str = r"
WITH requested AS MATERIALIZED (SELECT DISTINCT value AS id FROM json_each(?1)),
facts AS MATERIALIZED (
 SELECT t.id,t.parent_thread_id,CASE WHEN octet_length(parent.title)<=4096 THEN parent.title ELSE '親の名称は取得範囲外' END AS parent_title,parent.deleted_at AS parent_deleted,
   latest.id AS run_id,u.id AS turn_id,u.answer_node_id,
   shell.id AS execution,shell.turn_id AS shell_turn,shell.started_at AS shell_started,
   octet_length(shell.shell_json) AS command_bytes,
   (SELECT received_at FROM output_chunk WHERE execution_id=shell.id ORDER BY sequence DESC LIMIT 1) AS output_at,
   child.id AS child_run,child.thread_id AS child_thread,child.created_at AS child_started,child.state AS child_state,
   (SELECT count(*) FROM shell_execution WHERE owner_run_id=latest.id AND state IN ('starting','running','stopping')) AS shells,
   (SELECT count(*) FROM run WHERE owner_run_id=latest.id AND state IN ('live','stopping')) AS children,
   (SELECT count(*) FROM receipt r WHERE r.target_thread_id=t.id AND r.state IN ('accepted','held') AND r.path_admission<>'admitted'
     AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id)) AS pending,
   octet_length(u.recovery_reason_json) AS recovery_bytes,
   (SELECT octet_length(payload_json) FROM node WHERE id=u.answer_node_id) AS answer_bytes
 FROM requested q CROSS JOIN thread t ON t.id=q.id
 LEFT JOIN thread parent ON parent.id=t.parent_thread_id
 LEFT JOIN run latest ON latest.id=(SELECT id FROM run WHERE thread_id=t.id ORDER BY created_at DESC,id DESC LIMIT 1)
 LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=latest.id ORDER BY created_at DESC,id DESC LIMIT 1)
 LEFT JOIN shell_execution shell ON shell.id=(SELECT id FROM shell_execution WHERE owner_run_id=latest.id
   AND state IN ('starting','running','stopping') ORDER BY julianday(started_at) IS NULL,julianday(started_at),id LIMIT 1)
 LEFT JOIN run child ON child.id=(SELECT id FROM run WHERE owner_run_id=latest.id AND state IN ('live','stopping')
   ORDER BY julianday(created_at) IS NULL,julianday(created_at),id LIMIT 1)
 WHERE t.deleted_at IS NULL
), sized AS MATERIALIZED (
 SELECT *,COALESCE(CASE WHEN recovery_bytes<=16384 THEN recovery_bytes ELSE 0 END,0)
         +COALESCE(CASE WHEN answer_bytes<=16384 THEN answer_bytes ELSE 0 END,0)
         +COALESCE(CASE WHEN command_bytes<=16384 THEN command_bytes ELSE 0 END,0) AS source_bytes FROM facts
), budgeted AS MATERIALIZED (
 SELECT *,sum(CASE WHEN source_bytes<=16384 THEN source_bytes ELSE 0 END)
   OVER(ORDER BY id ROWS UNBOUNDED PRECEDING) AS used FROM sized
)
SELECT id,parent_thread_id,parent_title,parent_deleted,run_id,execution,shell_turn,shell_started,
 CASE WHEN command_bytes<=16384 AND source_bytes<=16384 AND used<=65536 THEN
   (SELECT CASE WHEN json_valid(shell_json) THEN CASE WHEN json_type(shell_json,'$.command')='text'
       THEN substr(json_extract(shell_json,'$.command'),1,512) END END FROM shell_execution WHERE id=budgeted.execution) END,
 output_at,child_run,child_thread,child_started,child_state,shells,children,pending,
 CASE WHEN recovery_bytes<=16384 AND source_bytes<=16384 AND used<=65536 THEN (SELECT recovery_reason_json FROM turn WHERE id=budgeted.turn_id) END,
 CASE WHEN answer_bytes<=16384 AND source_bytes<=16384 AND used<=65536 THEN (SELECT payload_json FROM node WHERE id=budgeted.answer_node_id) END,
 recovery_bytes IS NOT NULL AND (recovery_bytes>16384 OR source_bytes>16384 OR used>65536),
 answer_bytes IS NOT NULL AND (answer_bytes>16384 OR source_bytes>16384 OR used>65536),
 (SELECT state FROM shell_execution WHERE id=budgeted.execution),
 command_bytes IS NOT NULL AND (command_bytes>16384 OR source_bytes>16384 OR used>65536)
FROM budgeted ORDER BY id";

struct Extra {
    parent: Option<String>,
    parent_title: Option<String>,
    parent_deleted: bool,
    run: Option<String>,
    execution: Option<String>,
    shell_state: Option<String>,
    shell_turn: Option<String>,
    shell_started: Option<String>,
    command: Option<String>,
    command_limited: bool,
    output_at: Option<String>,
    child_run: Option<String>,
    child_thread: Option<String>,
    child_started: Option<String>,
    child_state: Option<String>,
    shells: u64,
    children: u64,
    pending: u64,
    recovery: Option<String>,
    answer: Option<String>,
    recovery_limited: bool,
    answer_limited: bool,
}
pub(super) struct RawWatch {
    summaries: Vec<thread_summary::RawSummary>,
    extras: BTreeMap<String, Extra>,
    parents: super::header::RawTitles,
    observed_at: Instant,
    wall_at: chrono::DateTime<chrono::Utc>,
}
#[cfg(test)]
pub(super) fn read_tx(db: &Connection, ids: &[String]) -> Result<RawWatch, StorageError> {
    read_shared_tx(db, ids, &[], &BTreeSet::new())
}

/// Reuse only facts observed by this caller in the same read transaction. The
/// shared parent IDs are promises to supply their prepared E titles later.
pub(super) fn read_shared_tx(
    db: &Connection,
    ids: &[String],
    known_summaries: &[thread_summary::RawSummary],
    known_parent_title_ids: &BTreeSet<String>,
) -> Result<RawWatch, StorageError> {
    if ids.len() > 128 || known_summaries.len() > 128 || known_parent_title_ids.len() > 128 {
        return Err(ApiError::new(ErrorCode::InputLimit, "Watch scope exceeds128 IDs").into());
    }
    if ids.is_empty() {
        return Ok(RawWatch {
            summaries: Vec::new(),
            extras: BTreeMap::new(),
            parents: super::header::read_titles_tx(db, &[])?,
            observed_at: Instant::now(),
            wall_at: chrono::Utc::now(),
        });
    }
    let known = known_summaries
        .iter()
        .map(|summary| (summary.thread_id.as_str(), summary))
        .collect::<BTreeMap<_, _>>();
    let missing = ids
        .iter()
        .filter(|id| !known.contains_key(id.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut observed = thread_summary::read_ids_tx(db, &missing)?
        .into_iter()
        .map(|summary| (summary.thread_id.clone(), summary))
        .collect::<BTreeMap<_, _>>();
    let mut summaries = Vec::new();
    let mut selected = BTreeSet::new();
    for id in ids {
        if !selected.insert(id) {
            continue;
        }
        if let Some(summary) = known.get(id.as_str()) {
            summaries.push((**summary).clone());
        } else if let Some(summary) = observed.remove(id) {
            summaries.push(summary);
        }
    }
    let encoded = serde_json::to_string(ids).expect("Thread IDs serialize");
    let extras = db
        .prepare(DETAILS)?
        .query_map([encoded], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Extra {
                    parent: row.get(1)?,
                    parent_title: row.get(2)?,
                    parent_deleted: row.get::<_, Option<String>>(3)?.is_some(),
                    run: row.get(4)?,
                    execution: row.get(5)?,
                    shell_turn: row.get(6)?,
                    shell_started: row.get(7)?,
                    command: row.get(8)?,
                    command_limited: row.get(22)?,
                    output_at: row.get(9)?,
                    child_run: row.get(10)?,
                    child_thread: row.get(11)?,
                    child_started: row.get(12)?,
                    child_state: row.get(13)?,
                    shells: row.get::<_, i64>(14)? as u64,
                    children: row.get::<_, i64>(15)? as u64,
                    pending: row.get::<_, i64>(16)? as u64,
                    recovery: row.get(17)?,
                    answer: row.get(18)?,
                    recovery_limited: row.get(19)?,
                    answer_limited: row.get(20)?,
                    shell_state: row.get(21)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let parents = extras
        .values()
        .filter(|row| !row.parent_deleted)
        .filter(|row| {
            row.parent
                .as_ref()
                .is_none_or(|id| !known_parent_title_ids.contains(id))
        })
        .filter_map(|row| Some((row.parent.clone()?, row.parent_title.clone()?)))
        .collect::<Vec<_>>();
    let parents = super::header::read_titles_tx(db, &parents)?;
    Ok(RawWatch {
        summaries,
        extras,
        parents,
        observed_at: Instant::now(),
        wall_at: chrono::Utc::now(),
    })
}

/// Runs on the existing bounded presentation worker, never in an input handler.
#[cfg(test)]
pub(super) fn prepare(raw: RawWatch) -> Vec<PreparedWatchDetail> {
    prepare_with_titles(raw, &BTreeMap::new())
}

pub(super) fn prepare_with_titles(
    raw: RawWatch,
    known: &BTreeMap<String, String>,
) -> Vec<PreparedWatchDetail> {
    let RawWatch {
        summaries,
        mut extras,
        parents,
        observed_at,
        wall_at,
    } = raw;
    let (parents, _limited) = parents.prepare();
    let saved_age = |value: Option<&str>| {
        super::live_work::age(value, wall_at).map(|elapsed_base| WatchAge {
            elapsed_base,
            observed_at,
            origin: WatchAgeOrigin::SavedTransition,
        })
    };
    summaries
        .into_iter()
        .filter_map(|summary| {
            let extra = extras.remove(&summary.thread_id)?;
            let phase = thread_summary::prepare_activity(&summary.activity);
            let identity = &summary.identity;
            let state_age = match phase {
                crate::activity::ActivityPhase::Completed
                | crate::activity::ActivityPhase::Stopped
                | crate::activity::ActivityPhase::Failed => {
                    saved_age(identity.run_ended_at.as_deref())
                }
                // A Turn creation time does not prove when waiting/recovery began.
                _ => None,
            };
            let model_request_age = matches!(
                phase,
                crate::activity::ActivityPhase::AwaitingModel
                    | crate::activity::ActivityPhase::Receiving
            )
            .then(|| saved_age(identity.attempt_started_at.as_deref()))
            .flatten();
            let mut rows = Vec::with_capacity(2);
            if let Some(id) = extra.execution {
                rows.push(LiveWorkRow {
                    target: LiveWorkTarget::Shell {
                        execution_id: id,
                        turn_id: extra.shell_turn,
                    },
                    label: super::live_work::label(
                        extra.command.as_deref(),
                        if extra.command_limited {
                            "shell（コマンドは取得範囲外・詳細で確認）"
                        } else {
                            "shell（コマンドは詳細で確認）"
                        },
                    ),
                    state: extra.shell_state.unwrap_or_default(),
                    elapsed_base: super::live_work::age(extra.shell_started.as_deref(), wall_at),
                    started_at: extra.shell_started,
                    output_age_base: super::live_work::age(extra.output_at.as_deref(), wall_at),
                    last_output_at: extra.output_at,
                });
            }
            if let (Some(run_id), Some(thread_id)) = (extra.child_run, extra.child_thread) {
                rows.push(LiveWorkRow {
                    target: LiveWorkTarget::ChildRun { run_id, thread_id },
                    label: "子の会話".into(),
                    state: extra.child_state.unwrap_or_default(),
                    elapsed_base: super::live_work::age(extra.child_started.as_deref(), wall_at),
                    started_at: extra.child_started,
                    last_output_at: None,
                    output_age_base: None,
                });
            }
            let work = extra
                .run
                .filter(|_| extra.shells + extra.children > 0)
                .map(|run_id| LiveWorkSnapshot {
                    run_id,
                    overflow: (extra.shells + extra.children).saturating_sub(rows.len() as u64),
                    rows,
                    shells: extra.shells,
                    children: extra.children,
                    waits: None,
                    observed_at,
                });
            let parent_title = extra.parent.as_ref().map(|id| {
                if extra.parent_deleted {
                    "削除済みの親".into()
                } else {
                    known
                        .get(id)
                        .or_else(|| parents.get(id))
                        .cloned()
                        .unwrap_or_else(|| "親の名称は取得範囲外".into())
                }
            });
            let recovery_reason = if extra.recovery_limited {
                Some("復旧理由は取得範囲外（詳細で確認）".into())
            } else {
                extra.recovery.as_deref().map(|body| {
                    serde_json::from_str::<ApiError>(body)
                        .map(|error| crate::config_setup::tui_error(&error))
                        .unwrap_or_else(|_| "保存された復旧理由を詳細で確認してください".into())
                })
            };
            let answer_preview = if extra.answer_limited {
                Some("回答本文は取得範囲外（会話を開いて確認）".into())
            } else {
                extra.answer.as_deref().map(|body| {
                    serde_json::from_str(body)
                        .map(|value| crate::history::conversation_excerpt(&value, 512))
                        .unwrap_or_else(|_| "回答本文は読み取れません（原文で確認）".into())
                })
            };
            let answer_preview = answer_preview.or_else(|| {
                identity
                    .answer_id
                    .as_ref()
                    .map(|_| "回答本文は未取得（会話を開いて確認）".into())
            });
            let detail = WatchDetail {
                thread_id: summary.thread_id.clone(),
                latest_run_id: identity.run_id.clone(),
                answer_id: identity.answer_id.clone(),
                turn_id: identity.turn_id.clone(),
                turn_generation: identity.turn_generation,
                phase,
                parent_title,
                state_age,
                model_request_age,
                work,
                recovery_reason,
                answer_preview,
                pending_messages: extra.pending,
                revisit: thread_summary::prepare(summary, String::new(), String::new()).revisit,
            };
            Some(detail.prepare())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use rusqlite::{StatementStatus, params};
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','Saved parent','/','','p','m','now'),('t','Visible','/','','p','m','now'); UPDATE thread SET parent_thread_id='parent' WHERE id='t';
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2020-01-01T00:00:00Z');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2020-01-01T00:00:00Z');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('s','r','u','op','stopping','/','{\"command\":\"echo watch\"}','UTF-8','UTF-8','open','2020-01-01T00:00:01Z');
            INSERT INTO node(id,kind,created_at,payload_json)VALUES('pending','user','now','{\"content\":[{\"type\":\"input_text\",\"text\":\"queued\"}]}');
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('receipt','receipt-op','{}','t','pending','accepted','now');").unwrap();
        db
    }
    #[test]
    fn command_json_and_parent_labels_are_bounded_before_copy() {
        let db = fixture();
        let huge = serde_json::json!({"command":"🚀".repeat(5000)}).to_string();
        db.execute(
            "UPDATE shell_execution SET shell_json=?1 WHERE id='s'",
            [&huge],
        )
        .unwrap();
        db.execute(
            "UPDATE thread SET title=?1 WHERE id='parent'",
            ["🚀".repeat(2000)],
        )
        .unwrap();
        let raw = read_tx(&db, &["t".into()]).unwrap();
        assert!(raw.extras["t"].command.is_none());
        assert!(raw.extras["t"].command_limited);
        assert!(raw.extras["t"].parent_title.as_ref().unwrap().len() <= 4096);
        let prepared = prepare(raw);
        assert!(
            prepared[0].facts().work.as_ref().unwrap().rows[0]
                .label
                .contains("取得範囲外")
        );
        assert!(
            prepared[0]
                .facts()
                .parent_title
                .as_ref()
                .unwrap()
                .contains("取得範囲外")
        );
        db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE shell_execution SET shell_json='broken json' WHERE id='s'; PRAGMA ignore_check_constraints=OFF;").unwrap();
        let raw = read_tx(&db, &["t".into()]).unwrap();
        assert!(raw.extras["t"].command.is_none());
        assert!(!raw.extras["t"].command_limited);
    }
    #[test]
    fn recovery_and_answer_share_the_per_row_source_limit() {
        let db = fixture();
        let answer = serde_json::json!({"content":"a".repeat(9000)}).to_string();
        let recovery = serde_json::json!({"message":"r".repeat(9000)}).to_string();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('answer','assistant','now',?1)", [&answer]).unwrap();
        db.execute(
            "UPDATE turn SET answer_node_id='answer',recovery_reason_json=?1 WHERE id='u'",
            [&recovery],
        )
        .unwrap();
        let raw = read_tx(&db, &["t".into()]).unwrap();
        let extra = &raw.extras["t"];
        assert!(extra.answer.is_none() && extra.recovery.is_none() && extra.command.is_none());
        assert!(extra.answer_limited && extra.recovery_limited && extra.command_limited);
    }
    #[test]
    fn aggregate_budget_charges_whole_command_sources_and_unicode_is_not_bytes() {
        let db = fixture();
        let body = serde_json::json!({"command":"🚀".repeat(3000)}).to_string();
        let mut ids = Vec::new();
        for n in 0..12 {
            let id = format!("visible-{n:02}");
            let run = format!("run-{n}");
            let shell = format!("shell-{n}");
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'Title','/','','p','m','now')", [&id]).unwrap();
            db.execute(
                "INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,'live','now')",
                params![run, id],
            )
            .unwrap();
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'running','now')", params![shell,id,run]).unwrap();
            db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,?1,?1,'running','/',?3,'UTF-8','UTF-8','open','2020-01-01T00:00:00Z')", params![shell,run,body]).unwrap();
            ids.push(id);
        }
        let raw = read_tx(&db, &ids).unwrap();
        let admitted = raw
            .extras
            .values()
            .filter(|row| row.command.is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            admitted.len(),
            65536 / body.len(),
            "charge source JSON rather than truncated labels"
        );
        assert!(
            admitted
                .iter()
                .all(|row| row.command.as_ref().unwrap().chars().count() == 512)
        );
        assert!(
            admitted
                .iter()
                .all(|row| row.command.as_ref().unwrap().len() == 2048)
        );
        assert_eq!(
            raw.extras
                .values()
                .filter(|row| row.command_limited)
                .count(),
            12 - admitted.len()
        );
        assert!(admitted.len() * body.len() <= 65536);
    }
    #[test]
    fn saved_work_queue_and_identities_share_two_batches_without_raw_output() {
        let db = fixture();
        let trace = Statements::new(&db);
        let raw = read_tx(&db, &["t".into()]).unwrap();
        assert_eq!(
            trace.count(),
            2,
            "one summary + one visible detail batch, stored parent needs no title SQL"
        );
        drop(trace);
        let extra = &raw.extras["t"];
        assert_eq!(
            extra.pending,
            super::super::message_queue::query(&db, "t").unwrap().total
        );
        assert_eq!(extra.execution.as_deref(), Some("s"));
        assert_eq!(extra.shell_state.as_deref(), Some("stopping"));
        assert_eq!(raw.summaries[0].identity.run_id.as_deref(), Some("r"));
        let rows = prepare(raw);
        let row = rows[0].facts();
        assert_eq!(row.parent_title.as_deref(), Some("Saved parent"));
        assert_eq!(row.phase, crate::activity::ActivityPhase::Background);
        assert_eq!(row.work.as_ref().unwrap().rows[0].state, "stopping");
        assert!(
            row.state_age.is_none(),
            "Turn creation is not a background/recovery transition time"
        );
        assert!(row.model_request_age.is_none());
    }
    #[test]
    fn witness_budget_is_global_and_missing_or_huge_answer_is_explicit() {
        let db = fixture();
        for i in 0..8 {
            let id = format!("v{i}");
            let run = format!("r{i}");
            let turn = format!("u{i}");
            let node = format!("n{i}");
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'Title','/','','p','m','now')",[&id]).unwrap();
            db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",params![run,id]).unwrap();
            let body=serde_json::json!({"content":[{"type":"output_text","text":"x".repeat(if i==7 {32768}else{12000})}]}).to_string();
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'assistant','now',?2)",
                params![node, body],
            )
            .unwrap();
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,answer_node_id,created_at,ended_at)VALUES(?1,?2,?3,'completed',?4,'2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",params![turn,id,run,node]).unwrap();
        }
        let ids = (0..8).map(|i| format!("v{i}")).collect::<Vec<_>>();
        let raw = read_tx(&db, &ids).unwrap();
        assert!(
            raw.extras
                .values()
                .map(|row| row.answer.as_ref().map_or(0, String::len)
                    + row.recovery.as_ref().map_or(0, String::len))
                .sum::<usize>()
                <= 65536
        );
        assert!(raw.extras["v7"].answer.is_none());
        assert!(raw.extras["v7"].answer_limited);
        let rows = prepare(raw);
        assert_eq!(rows.len(), 8);
        assert!(
            rows.iter()
                .find(|row| row.facts().thread_id == "v7")
                .unwrap()
                .facts()
                .answer_preview
                .as_ref()
                .unwrap()
                .contains("取得範囲外")
        );
        assert!(read_tx(&db, &vec!["t".into(); 129]).is_err());
    }
    #[test]
    fn empty_scope_does_no_sql_and_detail_seeks_do_not_scale_with_unrelated_threads() {
        let db = fixture();
        let trace = Statements::new(&db);
        assert!(read_tx(&db, &[]).unwrap().summaries.is_empty());
        assert_eq!(trace.count(), 0);
        drop(trace);
        fn steps(db: &Connection) -> (i32, i32) {
            let mut statement = db.prepare(DETAILS).unwrap();
            statement
                .query_map(["[\"t\"]"], |_| Ok(()))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (
                statement.get_status(StatementStatus::VmStep),
                statement.get_status(StatementStatus::FullscanStep),
            )
        }
        let before = steps(&db);
        db.execute_batch("WITH RECURSIVE ids(i)AS(VALUES(1)UNION ALL SELECT i+1 FROM ids WHERE i<10000) INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) SELECT 'unrelated-'||i,'Other','/','','p','m','now' FROM ids;").unwrap();
        let after = steps(&db);
        assert_eq!(
            after, before,
            "actual detail query must seek only its visible IDs"
        );
    }
}

#[cfg(test)]
mod shared_consumer_tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','','/','','p','m','now'),('child','Child','/','','p','m','now'),('other','Other','/','','p','m','now');
          UPDATE thread SET parent_thread_id='parent' WHERE id='child';
          INSERT INTO node(id,kind,created_at,payload_json)VALUES('question','user','now','{\"role\":\"user\",\"content\":\"Derived parent title\"}');
          INSERT INTO thread_path VALUES('parent',0,'question');").unwrap();
        db
    }
    #[test]
    fn shared_e_metadata_and_parent_titles_do_not_repeat_summary_or_witness_sql() {
        let db = fixture();
        let known = thread_summary::read_ids_tx(&db, &["parent".into(), "child".into()]).unwrap();
        let parent_ids = BTreeSet::from(["parent".into()]);
        let trace = Statements::new(&db);
        let raw = read_shared_tx(&db, &["child".into()], &known, &parent_ids).unwrap();
        assert_eq!(
            trace.count(),
            1,
            "only the visible detail statement; E already owns metadata and parent title evidence"
        );
        drop(trace);
        let titles = BTreeMap::from([("parent".into(), "Derived parent title".into())]);
        let rows = prepare_with_titles(raw, &titles);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].facts().parent_title.as_deref(),
            Some("Derived parent title")
        );
    }
    #[test]
    fn missing_summary_ids_use_one_bounded_batch_and_limited_parent_title_is_preserved() {
        let db = fixture();
        let known = thread_summary::read_ids_tx(&db, &["child".into()]).unwrap();
        let parent_ids = BTreeSet::from(["parent".into()]);
        let trace = Statements::new(&db);
        let raw = read_shared_tx(
            &db,
            &["child".into(), "other".into(), "other".into()],
            &known,
            &parent_ids,
        )
        .unwrap();
        assert_eq!(
            trace.count(),
            2,
            "one missing-ID summary batch plus detail batch"
        );
        drop(trace);
        let limited = "名称未設定（元本文の取得範囲外）";
        let rows = prepare_with_titles(raw, &BTreeMap::from([("parent".into(), limited.into())]));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].facts().parent_title.as_deref(), Some(limited));
        let trace = Statements::new(&db);
        assert!(read_shared_tx(&db, &[], &vec![known[0].clone(); 129], &BTreeSet::new()).is_err());
        assert!(
            read_shared_tx(&db, &[], &[], &(0..129).map(|n| format!("p{n}")).collect()).is_err()
        );
        assert_eq!(trace.count(), 0);
    }
    #[test]
    fn missing_known_title_promise_remains_unavailable_and_deleted_parent_overrides_cached_name() {
        let db = fixture();
        let known = thread_summary::read_ids_tx(&db, &["child".into()]).unwrap();
        let ids = BTreeSet::from(["parent".into()]);
        let raw = read_shared_tx(&db, &["child".into()], &known, &ids).unwrap();
        assert_eq!(
            prepare_with_titles(raw, &BTreeMap::new())[0]
                .facts()
                .parent_title
                .as_deref(),
            Some("親の名称は取得範囲外")
        );
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='parent'", [])
            .unwrap();
        let raw = read_shared_tx(&db, &["child".into()], &known, &ids).unwrap();
        assert_eq!(
            prepare_with_titles(
                raw,
                &BTreeMap::from([("parent".into(), "Stale cached title".into())])
            )[0]
            .facts()
            .parent_title
            .as_deref(),
            Some("削除済みの親")
        );
    }
}
