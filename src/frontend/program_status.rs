//! Read-only terminal metadata on existing finite read admission, not a poller.
use crate::program_status::{
    CHILD_LIMIT, Counts, Record, Snapshot, State, clean, map_phase, thread_id,
};
use crate::protocol::ApiError;
use std::{collections::BTreeMap, path::PathBuf};
pub(super) struct Session {
    pub started: chrono::DateTime<chrono::Utc>,
    pub seen: BTreeMap<String, String>,
}
impl Session {
    pub fn new() -> Self {
        Self {
            started: chrono::Utc::now(),
            seen: BTreeMap::new(),
        }
    }

    pub fn accept(
        &mut self,
        result: Result<Projected, ApiError>,
        dirty: &mut bool,
    ) -> Option<Snapshot> {
        match result {
            Ok(projected) => {
                self.seen = projected.seen;
                Some(projected.snapshot)
            }
            Err(_) => {
                *dirty = true;
                None
            }
        }
    }
}

/// Only called through finite frontend admission; never occupies the writer queue.
pub(super) async fn read(
    path: PathBuf,
    selected: Option<String>,
    started: chrono::DateTime<chrono::Utc>,
    seen: BTreeMap<String, String>,
) -> Result<Projected, ApiError> {
    super::presentation_work(move || {
        let mut db = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.busy_timeout(std::time::Duration::from_millis(250))?;
        db.pragma_update(None, "query_only", true)?;
        // Keep all rows and retained-child titles on the same WAL snapshot.
        let transaction = db.transaction()?;
        let projected = project(&transaction, selected, started, seen)?;
        transaction.commit()?;
        Ok::<_, crate::storage::StorageError>(projected)
    })
    .await?
    .map_err(super::storage_error)
}

pub(super) struct Projected {
    pub snapshot: Snapshot,
    pub seen: BTreeMap<String, String>,
}
const SQL:&str="SELECT t.id,substr(t.title,1,1024),r.id,r.state,u.state,r.ended_at,
 EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=r.id AND state IN ('starting','running','stopping'))
 OR EXISTS(SELECT 1 FROM run WHERE owner_run_id=r.id AND state IN ('live','stopping')),
 a.state,CASE WHEN a.state='failed' AND a.generation=u.generation AND json_valid(a.error_json) THEN json_extract(a.error_json,'$.automatic_retry') END,
 CASE WHEN r.state='live' AND u.state='running' THEN EXISTS(SELECT 1 FROM tool_group WHERE turn_id=u.id AND state='running') ELSE 0 END,
 CASE WHEN json_valid(u.recovery_reason_json) THEN substr(json_extract(u.recovery_reason_json,'$.message'),1,4096) ELSE '' END,
 CASE WHEN json_valid(u.recovery_reason_json) THEN json_extract(u.recovery_reason_json,'$.code') END,
 CASE WHEN json_valid(u.recovery_reason_json) THEN json_extract(u.recovery_reason_json,'$.details.failure_kind') END,
 CASE WHEN a.generation=u.generation AND json_valid(a.error_json) THEN json_extract(a.error_json,'$.error.status') END
 FROM thread t LEFT JOIN run r ON r.id=(SELECT id FROM run WHERE thread_id=t.id ORDER BY created_at DESC,id DESC LIMIT 1)
 LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1)
 LEFT JOIN model_attempt a ON a.rowid=(SELECT rowid FROM model_attempt WHERE turn_id=u.id ORDER BY rowid DESC LIMIT 1)
 WHERE t.deleted_at IS NULL ORDER BY t.id";

pub(super) fn project(
    db: &rusqlite::Connection,
    selected: Option<String>,
    started: chrono::DateTime<chrono::Utc>,
    seen: BTreeMap<String, String>,
) -> Result<Projected, crate::storage::StorageError> {
    let mut statement = db.prepare(SQL)?;
    let mut rows = statement.query([])?;
    let mut counts = Counts::default();
    let mut children: Vec<(String, String, Record)> = Vec::new();
    let mut keep_seen = BTreeMap::new();
    let mut viewed = None;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let stored: String = row.get(1)?;
        let run_id: Option<String> = row.get(2)?;
        let run: Option<String> = row.get(3)?;
        let turn: Option<String> = row.get(4)?;
        let ended: Option<String> = row.get(5)?;
        let attempt: Option<String> = row.get(7)?;
        let diagnostic: Option<String> = row.get(8)?;
        let retrying = diagnostic
            .as_deref()
            .and_then(|value| {
                serde_json::from_str::<crate::ui::activity::AutomaticRetrySnapshot>(value).ok()
            })
            .is_some_and(|retry| retry.number > 0 && retry.number <= retry.limit);
        let phase = crate::activity::classify(
            run.as_deref(),
            turn.as_deref(),
            row.get(6)?,
            attempt.as_deref() == Some("inflight"),
            retrying,
            row.get(9)?,
        );
        let already_seen = run_id
            .as_ref()
            .is_some_and(|run| seen.get(&id) == Some(run));
        if already_seen && let Some(run) = &run_id {
            keep_seen.insert(id.clone(), run.clone());
        }
        let recent = ended
            .as_deref()
            .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
            .is_some_and(|date| date >= started);
        let current = selected.as_deref() == Some(&id);
        if phase == crate::activity::ActivityPhase::Completed
            && recent
            && current
            && !already_seen
            && let Some(run) = &run_id
        {
            viewed = Some((id.clone(), run.clone()));
        }
        let state = map_phase(phase, recent && !current && !already_seen);
        let code: Option<String> = row.get(11)?;
        let kind: Option<String> = row.get(12)?;
        let status: Option<i64> = row.get(13)?;
        let auth = state == State::Blocked
            && (code.as_deref() == Some("config_invalid")
                || matches!(
                    kind.as_deref(),
                    Some("config_invalid" | "config_missing" | "config_missing_credentials")
                )
                || matches!(status, Some(401 | 403)));
        counts.add(state, auth);
        if state == State::Idle {
            continue;
        }
        let reason: Option<String> = row.get(10)?;
        let msg = if state == State::Blocked {
            clean(
                reason
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or("Recovery required; saved reason unavailable"),
                crate::program_status::MSG_BYTES,
            )
        } else {
            phase.label().into()
        };
        children.push((
            id.clone(),
            stored,
            Record {
                id: Some(thread_id(&id)),
                state,
                auth,
                title: String::new(),
                msg,
            },
        ));
        children.sort_by(|a, b| b.2.state.cmp(&a.2.state).then(a.0.cmp(&b.0)));
        children.truncate(CHILD_LIMIT);
    }
    for (id, stored, record) in &mut children {
        record.title = clean(
            &crate::control::db::thread_display_title(db, id, stored)
                .map_err(crate::storage::StorageError::from)?,
            crate::program_status::TITLE_BYTES,
        );
    }
    Ok(Projected {
        snapshot: Snapshot {
            selected,
            viewed,
            counts,
            children: children.into_iter().map(|(_, _, record)| record).collect(),
        },
        seen: keep_seen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DbWorker;
    fn insert(
        db: &rusqlite::Connection,
        id: &str,
        run: Option<&str>,
        turn: &str,
        ended: Option<&str>,
        reason: Option<String>,
    ) -> Result<(), crate::storage::StorageError> {
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?1,'/fixture','/fixture/AGENTS.md','fixture','fixture','2026-01-01')",[id])?;
        if let Some(state) = run {
            db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,?3,'2026-01-01',?4)",rusqlite::params![format!("r-{id}"),id,state,ended])?;
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES(?1,?2,?3,?4,'2026-01-01',?5)",rusqlite::params![format!("u-{id}"),id,format!("r-{id}"),turn,reason])?;
        }
        Ok(())
    }
    #[tokio::test]
    async fn failed_projection_retries_without_changed_and_preserves_seen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let mut session = Session::new();
        session.seen.insert("seen".into(), "r-seen".into());
        let mut dirty = false;
        let failed = read(path.clone(), None, session.started, session.seen.clone()).await;
        assert!(failed.is_err());
        assert!(!path.exists(), "read-only open must not create a database");
        assert!(session.accept(failed, &mut dirty).is_none());
        assert!(
            dirty,
            "the next existing tick must be able to retry without Changed"
        );
        assert_eq!(session.seen.get("seen").map(String::as_str), Some("r-seen"));

        let worker = DbWorker::open(&path).await.unwrap();
        worker
            .call(|db| insert(db, "idle", None, "completed", None, None))
            .await
            .unwrap();
        // Simulate admission at the next coalescing opportunity, without Changed.
        dirty = false;
        let retried = read(path, None, session.started, session.seen.clone()).await;
        let snapshot = session.accept(retried, &mut dirty).unwrap();
        assert!(!dirty);
        assert_eq!(snapshot.counts.idle, 1);
        assert!(session.seen.is_empty());
        worker.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn ten_thousand_threads_read_while_writer_is_occupied() {
        use std::time::{Duration, Instant};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let worker = DbWorker::open(&path).await.unwrap();
        worker
            .call(|db| {
                let tx = db.transaction()?;
                for i in 0..9992 {
                    insert(
                        &tx,
                        &format!("historical-{i:05}"),
                        Some("completed"),
                        "completed",
                        Some("2026-01-01T00:00:00Z"),
                        None,
                    )?;
                }
                insert(&tx, "live", Some("live"), "running", None, None)?;
                insert(&tx, "stopping", Some("stopping"), "running", None, None)?;
                insert(&tx, "recovery", Some("live"), "recovery_wait", None, None)?;
                insert(
                    &tx,
                    "failed",
                    Some("failed"),
                    "failed",
                    Some("2026-01-01T00:00:00Z"),
                    None,
                )?;
                insert(
                    &tx,
                    "fresh",
                    Some("completed"),
                    "completed",
                    Some("2026-01-03T00:00:00Z"),
                    None,
                )?;
                insert(
                    &tx,
                    "seen",
                    Some("completed"),
                    "completed",
                    Some("2026-01-03T00:00:00Z"),
                    None,
                )?;
                insert(&tx, "stopped", Some("stopped"), "stopped", None, None)?;
                insert(&tx, "idle", None, "completed", None, None)?;
                tx.execute(
                    "INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) SELECT 'a-'||id,id,0,0,'fixture','fixture',0,CASE WHEN state='running' THEN 'inflight' WHEN state='recovery_wait' THEN 'failed' ELSE 'completed' END,'{}','2026-01-01' FROM turn",
                    [],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let start = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let seen = BTreeMap::from([("seen".into(), "r-seen".into())]);
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let writing = worker
            .try_call(move |db| {
                let tx = db.transaction()?;
                tx.execute("UPDATE run SET state='failed' WHERE id='r-live'", [])?;
                entered.send(()).unwrap();
                // Synthetic only: hold a writer transaction until all reads finish.
                crate::test_watchdog::blocking_recv("release writer hold", &gate).unwrap();
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        ready.await.unwrap();
        let reads = crate::test_watchdog::watchdog(
            "src/frontend/program_status.rs: ten_thousand_threads_read_while_writer_is_occupied",
            async {
                let mut timings = Vec::new();
                for _ in 0..10 {
                    let began = Instant::now();
                    let projected = read(path.clone(), None, start, seen.clone()).await.unwrap();
                    timings.push(began.elapsed());
                    let counts = &projected.snapshot.counts;
                    assert_eq!(
                        (
                            counts.working,
                            counts.blocked,
                            counts.error,
                            counts.done,
                            counts.idle
                        ),
                        (2, 1, 1, 1, 9995)
                    );
                    assert_eq!(projected.snapshot.children.len(), 5);
                    assert_eq!(projected.seen, seen);
                }
                timings
            },
        )
        .await;
        release.send(()).unwrap();
        writing.receive().await.unwrap();
        let timings = reads.expect("read-only projection must not queue behind writer");
        let micros: Vec<_> = timings.iter().map(Duration::as_micros).collect();
        eprintln!(
            "OSC7501 10000 threads / 10 read-only projections, open+transaction+SQL+close us: {micros:?}; mean={} us",
            micros.iter().sum::<u128>() / micros.len() as u128
        );
        let after = read(path.clone(), None, start, seen).await.unwrap();
        assert_eq!(after.snapshot.counts.working, 1);
        assert_eq!(after.snapshot.counts.error, 2);
        worker.shutdown().await.unwrap();
        // No retained read connection/transaction should obstruct shutdown/checkpoint.
        let db = rusqlite::Connection::open(&path).unwrap();
        let busy: i64 = db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 0);
    }

    #[tokio::test]
    async fn all_threads_count_but_only32_export_and_seen_completion_is_ephemeral() {
        let root = tempfile::tempdir().unwrap();
        let worker = DbWorker::open(root.path().join("db")).await.unwrap();
        worker.call(|db| {
            for i in 0..40 {insert(db,&format!("working-{i}"),Some("live"),"running",None,None)?;}
            insert(db,"auth",Some("live"),"recovery_wait",None,Some(serde_json::json!({"code":"config_invalid","message":"set TEST_KEY\u{202e}"}).to_string()))?;
            insert(db,"other",Some("live"),"recovery_wait",None,Some(serde_json::json!({"code":"io_failed","message":"inspect saved result"}).to_string()))?;
            insert(db,"failed",Some("failed"),"failed",Some("2026-01-03T00:00:00Z"),None)?;
            insert(db,"fresh",Some("completed"),"completed",Some("2026-01-03T00:00:00Z"),None)?;
            insert(db,"historical",Some("completed"),"completed",Some("2026-01-01T00:00:00Z"),None)?;
            insert(db,"stopped",Some("stopped"),"stopped",None,None)?;
            insert(db,"idle",None,"completed",None,None)?;
            Ok(())
        }).await.unwrap();
        let start = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        worker
            .call(move |db| {
                let first = project(db, None, start, BTreeMap::new())?;
                assert_eq!(first.snapshot.counts.working, 40);
                assert_eq!(first.snapshot.counts.blocked, 2);
                assert_eq!(first.snapshot.counts.auth, 1);
                assert_eq!(first.snapshot.counts.error, 1);
                assert_eq!(first.snapshot.counts.done, 1);
                assert_eq!(first.snapshot.children.len(), 32);
                assert!(
                    first
                        .snapshot
                        .children
                        .iter()
                        .any(|record| record.auth && record.state == State::Blocked)
                );
                assert!(
                    first
                        .snapshot
                        .children
                        .iter()
                        .all(|record| !record.msg.contains('\u{202e}'))
                );
                let selected = project(db, Some("fresh".into()), start, BTreeMap::new())?;
                assert_eq!(selected.snapshot.counts.done, 0);
                assert_eq!(
                    selected.snapshot.viewed,
                    Some(("fresh".into(), "r-fresh".into()))
                );
                let seen = BTreeMap::from([selected.snapshot.viewed.unwrap()]);
                assert_eq!(
                    project(db, None, start, seen.clone())?.snapshot.counts.done,
                    0
                );
                db.execute(
                    "UPDATE thread SET deleted_at='2026-01-04' WHERE id='fresh'",
                    [],
                )?;
                assert!(project(db, None, start, seen)?.seen.is_empty());
                for id in ["auth", "other", "failed", "stopped", "idle", "working-0"] {
                    let same = crate::frontend::activity::project(db, id)?.snapshot.phase;
                    let counts = project(db, None, start, BTreeMap::new())?.snapshot.counts;
                    assert!(matches!(
                        same,
                        crate::activity::ActivityPhase::Recovery
                            | crate::activity::ActivityPhase::Failed
                            | crate::activity::ActivityPhase::Stopped
                            | crate::activity::ActivityPhase::Idle
                            | crate::activity::ActivityPhase::Running
                    ));
                    assert_eq!(counts.working, 40);
                    let mapped = project(db, None, start, BTreeMap::new())?
                        .snapshot
                        .children
                        .into_iter()
                        .find(|record| record.id.as_deref() == Some(thread_id(id).as_str()))
                        .map_or(State::Idle, |record| record.state);
                    assert_eq!(mapped, map_phase(same, false));
                }
                Ok(())
            })
            .await
            .unwrap();
        worker.shutdown().await.unwrap();
    }
}
