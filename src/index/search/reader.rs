//! One admitted read-only ranking task; no connection pool or writer-side scan.
use super::*;
use rusqlite::{InterruptHandle, OpenFlags};
use std::{path::PathBuf, sync::Mutex};

pub(super) struct SearchGuard {
    pub(super) token: CancellationToken,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
    #[cfg(test)]
    refresh_gate: Option<Arc<RefreshGate>>,
}
#[cfg(test)]
struct RefreshGate {
    ready: Mutex<Option<tokio::sync::oneshot::Sender<usize>>>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl SearchGuard {
    pub(super) fn new(token: CancellationToken) -> Self {
        Self {
            token,
            interrupt: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            refresh_gate: None,
        }
    }
}
impl Drop for SearchGuard {
    fn drop(&mut self) {
        self.token.cancel();
        if let Some(handle) = self
            .interrupt
            .lock()
            .expect("search interrupt poisoned")
            .as_ref()
        {
            handle.interrupt();
        }
    }
}

pub(super) async fn rank(
    path: PathBuf,
    space: Space,
    query: String,
    args: HistorySearchArgs,
    guard: &SearchGuard,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<IndexHit>, ApiError> {
    let cancel = guard.token.clone();
    let interrupt = guard.interrupt.clone();
    #[cfg(test)]
    let refresh_gate = guard.refresh_gate.clone();
    tokio::task::spawn_blocking(move || {
        // This permit is released only after SQLite/CPU work actually exits,
        // including when the async observer has already been dropped.
        let _permit = permit;
        let operation = || -> Result<Vec<IndexHit>, ApiError> {
            ensure_running(Some(&cancel))?;
            crate::storage::register_vec().map_err(read_db_error)?;
            let db = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(read_sql_error)?;
            *interrupt.lock().expect("search interrupt poisoned") = Some(db.get_interrupt_handle());
            // A cancellation racing connection creation still interrupts before
            // the first statement, without relying on a lost Notify permit.
            ensure_running(Some(&cancel))?;
            db.busy_timeout(Duration::from_millis(100))
                .map_err(read_sql_error)?;
            db.execute_batch("PRAGMA query_only=ON;")
                .map_err(read_sql_error)?;
            let version: i64 = db
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(read_sql_error)?;
            if version != crate::storage::SCHEMA_VERSION {
                return Err(err(
                    ErrorCode::InvalidState,
                    "History search schema changed; retry after reopening the owner",
                ));
            }
            let snapshot = db.unchecked_transaction().map_err(read_sql_error)?;
            check_space(&snapshot, &space)?;
            let ranked = ranked_hits(&snapshot, &space, &query, &args, Some(&cancel))?;
            snapshot.commit().map_err(read_sql_error)?;
            #[cfg(test)]
            if let Some(gate) = refresh_gate {
                if let Some(ready) = gate.ready.lock().unwrap().take() {
                    let _ = ready.send(ranked.hits.len());
                }
                gate.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| {
                        err(
                            ErrorCode::InvalidState,
                            "Test refresh barrier was not released",
                        )
                    })?;
            }
            ensure_running(Some(&cancel))?;
            // Writers may commit deletion/GC while ranking uses its WAL snapshot.
            // Revalidate only the bounded returned nodes against a fresh snapshot.
            let current = db.unchecked_transaction().map_err(read_sql_error)?;
            check_space(&current, &space)?;
            let hits = refresh_hits(&current, ranked, &args, &cancel)?;
            current.commit().map_err(read_sql_error)?;
            ensure_running(Some(&cancel))?;
            Ok(hits)
        };
        let result = operation();
        // SQLite interruption is an observation cancellation, never a failed
        // persistence transaction that should stop the owner.
        if cancel.is_cancelled() {
            Err(err(
                ErrorCode::InvalidState,
                "Semantic search was cancelled",
            ))
        } else {
            result.map_err(observation_error)
        }
    })
    .await
    .map_err(|_| {
        err(
            ErrorCode::InvalidState,
            "Semantic search task ended unexpectedly",
        )
    })?
}

// Shared ranking helpers also run in writer transactions elsewhere and therefore
// retain their SaveFailed mapping. Only this readonly task owns the observation
// boundary: a failed open/read/lock cannot establish that a source write failed.
fn observation_error(mut error: ApiError) -> ApiError {
    if error.is_storage_failure() {
        error.code = ErrorCode::IoFailed;
        error.message = format!("Read-only history search failed: {}", error.message);
        let primary = error
            .details
            .as_ref()
            .and_then(|details| details.get("sqlite_primary_code"))
            .and_then(serde_json::Value::as_i64);
        let recovery = match primary {
            Some(code)
                if code == i64::from(rusqlite::ffi::SQLITE_BUSY)
                    || code == i64::from(rusqlite::ffi::SQLITE_LOCKED) =>
            {
                "Wait for the database lock or competing operation to finish, then retry history search."
            }
            Some(code)
                if code == i64::from(rusqlite::ffi::SQLITE_CORRUPT)
                    || code == i64::from(rusqlite::ffi::SQLITE_NOTADB)
                    || code == i64::from(rusqlite::ffi::SQLITE_SCHEMA)
                    || code == i64::from(rusqlite::ffi::SQLITE_ERROR) =>
            {
                "Inspect the reported database/schema failure and repair its cause before retrying; corrupt databases or missing tables are not repaired by retries."
            }
            _ => {
                "Inspect the reported read failure: for SQLITE_BUSY/SQLITE_LOCKED wait for the lock to clear before retrying; for corruption, a non-database file, or missing/incompatible schema, repair the cause first. Other read failures require checking the reported path and cause."
            }
        };
        error.hint = Some(format!(
            "{recovery} This is a read-only observation with no operation receipt to query. Repeating search after HTTP may incur another query embedding charge."
        ));
    }
    error
}

// Preserve SQLite codes before the existing shared helpers erase their types.
// Shared ranking/provenance errors without codes receive conditional guidance;
// do not infer a SQLite category by parsing provider- or platform-specific prose.
fn read_sql_error(error: rusqlite::Error) -> ApiError {
    let extended = error.sqlite_error().map(|sqlite| sqlite.extended_code);
    let mut error = sql_error(error);
    if let Some(extended) = extended {
        let details = error.details.get_or_insert_with(|| serde_json::json!({}));
        if let Some(details) = details.as_object_mut() {
            details.insert("sqlite_primary_code".into(), (extended & 0xff).into());
            details.insert("sqlite_extended_code".into(), extended.into());
        }
    }
    error
}

fn read_db_error(error: StorageError) -> ApiError {
    match error {
        StorageError::Sqlite(error) => read_sql_error(error),
        other => db_error(other),
    }
}

fn check_space(db: &Connection, expected: &Space) -> Result<(), ApiError> {
    let selected = meta(db, IndexKeys::ACTIVE_SPACE).map_err(read_db_error)?;
    let current = selected
        .as_deref()
        .map(|id| load_space(db, id))
        .transpose()
        .map_err(read_db_error)?
        .flatten();
    if current.as_ref().is_none_or(|current| {
        current.id != expected.id
            || current.profile != expected.profile
            || current.model != expected.model
            || current.dimension != expected.dimension
            || current.splitter != expected.splitter
            || current.table != expected.table
    }) {
        return Err(err(
            ErrorCode::InvalidState,
            "History embedding space changed during search; retry search",
        ));
    }
    Ok(())
}

fn refresh_hits(
    db: &Connection,
    mut ranked: RankedHits,
    args: &HistorySearchArgs,
    cancel: &CancellationToken,
) -> Result<Vec<IndexHit>, ApiError> {
    let mut validity = HashMap::<String, HashMap<bool, bool>>::new();
    for witness in ranked.witnesses {
        ensure_running(Some(cancel))?;
        let raw = witness.raw.is_some();
        let valid = witness_valid(db, &witness, if raw { "tool_raw" } else { "canonical" })
            .map_err(read_db_error)?;
        validity
            .entry(witness.node_id)
            .or_default()
            .entry(raw)
            .and_modify(|previous| *previous &= valid)
            .or_insert(valid);
    }
    let scoped = if let Some(thread) = &args.thread_id {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                [thread],
                |row| row.get(0),
            )
            .map_err(read_sql_error)?;
        if !exists {
            return Err(err(
                ErrorCode::ThreadNotFound,
                "Search Thread no longer exists",
            ));
        }
        let ids = serde_json::to_string(
            &ranked
                .hits
                .iter()
                .map(|hit| &hit.node_id)
                .collect::<Vec<_>>(),
        )
        .map_err(|_| err(ErrorCode::InvalidState, "Cannot encode search result IDs"))?;
        let sql = format!(
            "{} SELECT id FROM scope WHERE id IN(SELECT value FROM json_each(?2))",
            crate::history::search_scope::cte("?1")
        );
        let mut query = db.prepare(&sql).map_err(read_sql_error)?;
        Some(
            query
                .query_map(params![thread, ids], |row| row.get::<_, String>(0))
                .map_err(read_sql_error)?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(read_sql_error)?,
        )
    } else {
        None
    };
    let mut hits = Vec::new();
    for mut hit in ranked.hits.drain(..) {
        ensure_running(Some(cancel))?;
        if scoped
            .as_ref()
            .is_some_and(|scope| !scope.contains(&hit.node_id))
            || !history::reachability::searchable(db, &hit.node_id).map_err(read_sql_error)?
        {
            continue;
        }
        if hit.spans.is_empty()
            || hit.spans.iter().any(|span| {
                validity
                    .get(&hit.node_id)
                    .and_then(|kinds| kinds.get(&(span.body_kind == "tool_raw")))
                    != Some(&true)
            })
        {
            // Keep the original distance meaningful: if the winning raw/canonical
            // witness changed, discard this candidate rather than reuse its score.
            continue;
        }
        hit.provenance = history::provenance(db, &hit.node_id)?;
        hit.thread_scope = search_thread_scope(db, args, &hit.node_id)?;
        hits.push(hit);
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, DbWorker, Space) {
        let root = tempfile::tempdir().unwrap();
        let worker = DbWorker::open(root.path().join("history.sqlite"))
            .await
            .unwrap();
        let space = worker.call(|db| {
            let tx = db.transaction()?;
            let embedding = identity::EmbeddingIdentity { requested_dimensions: None, key:"fixture-key".into(), profile:"fixture".into(), model:"synthetic".into(), query_key:"fixture-query".into() };
            let space = ensure_space(&tx, &embedding, "active", 2)?;
            tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','/','/a','fixture','synthetic','now'); INSERT INTO node(id,kind,created_at,payload_json,body_hash)VALUES('first','user','now','{}','body'),('second','user','now','{}','body'); INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',0,'first'),('thread',1,'second');")?;
            for (ordinal, id) in [(1, "first"), (2, "second")] {
                let watermark=json!({"witness":{"node_id":id,"revision":0,"body_hash":"body","raw":null}}).to_string();
                tx.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES(?1,?2,?1,0,'canonical','body',0,1,'done',?3)",params![id,space.id,watermark])?;
                tx.execute(&format!("INSERT INTO {}(rowid,embedding)VALUES(?1,'[0.1,0.2]')",space.table),[ordinal])?;
                tx.execute("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES(?1,?1,?2,?1,0,0,1,'body',?3)",params![id,space.id,ordinal])?;
            }
            tx.commit()?; Ok(space)
        }).await.unwrap();
        (root, worker, space)
    }
    fn ranked(ids: &[&str]) -> RankedHits {
        RankedHits {
            hits: ids
                .iter()
                .map(|id| IndexHit {
                    node_id: (*id).into(),
                    excerpt: "old snapshot".into(),
                    distance: 0.5,
                    provenance: crate::protocol::NodeProvenance {
                        kind: "user".into(),
                        created_at: "now".into(),
                        source_revision: 0,
                        turn_id: None,
                        run_id: None,
                        origin_thread_id: None,
                    },
                    thread_scope: None,
                    spans: vec![SourceSpan {
                        source_revision: 0,
                        content_hash: "body".into(),
                        body_kind: "canonical".into(),
                        start: 0,
                        end: 1,
                    }],
                })
                .collect(),
            witnesses: ids
                .iter()
                .map(|id| Witness {
                    node_id: (*id).into(),
                    revision: 0,
                    body_hash: Some("body".into()),
                    raw: None,
                })
                .collect(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn readonly_ranking_releases_its_snapshot_before_observing_concurrent_writer_changes() {
        let (_root, db, space) = fixture().await;
        let slots = Arc::new(Semaphore::new(1));
        let (ready, observed) = tokio::sync::oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let mut guard = SearchGuard::new(CancellationToken::new());
        guard.refresh_gate = Some(Arc::new(RefreshGate {
            ready: Mutex::new(Some(ready)),
            resume: Mutex::new(resume),
        }));
        let path = db.path().to_path_buf();
        let permit = slots.clone().try_acquire_owned().unwrap();
        let query = tokio::spawn(async move {
            rank(
                path,
                space,
                "[0.1,0.2]".into(),
                HistorySearchArgs {
                    thread_id: Some("thread".into()),
                    limit: 2,
                    ..Default::default()
                },
                &guard,
                permit,
            )
            .await
        });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), observed)
                .await
                .unwrap()
                .unwrap(),
            2,
            "Both valid hits must come from the actual readonly ranking snapshot"
        );
        tokio::time::timeout(Duration::from_secs(2),db.call(|db| {
            let tx = db.transaction()?;
            tx.execute("UPDATE node SET payload_json=NULL,payload_revision=payload_revision+1,deleted_reason='collected' WHERE id='first'",[])?;
            tx.execute("DELETE FROM thread_path WHERE node_id='second'",[])?;
            tx.commit()?; Ok(())
        })).await.unwrap().unwrap();
        release.send(()).unwrap();
        let hits = tokio::time::timeout(Duration::from_secs(2), query)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            hits.is_empty(),
            "Fresh snapshot must discard both collected and deselected results"
        );
        assert_eq!(slots.available_permits(), 1);
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn fresh_snapshot_discards_collected_revised_and_deselected_nodes() {
        let (_root, db, _space) = fixture().await;
        db.call(|db| {
            db.execute(
                "UPDATE node SET payload_revision=payload_revision+1 WHERE id='first'",
                [],
            )?;
            let hits = refresh_hits(
                db,
                ranked(&["first", "second"]),
                &HistorySearchArgs::default(),
                &CancellationToken::new(),
            )
            .unwrap();
            assert_eq!(
                hits.iter().map(|h| h.node_id.as_str()).collect::<Vec<_>>(),
                vec!["second"]
            );
            db.execute("DELETE FROM thread_path WHERE node_id='second'", [])?;
            let args = HistorySearchArgs {
                thread_id: Some("thread".into()),
                ..Default::default()
            };
            assert!(
                refresh_hits(db, ranked(&["second"]), &args, &CancellationToken::new())
                    .unwrap()
                    .is_empty()
            );
            db.execute(
                "UPDATE node SET payload_json=NULL,deleted_reason='collected' WHERE id='second'",
                [],
            )?;
            assert!(
                refresh_hits(
                    db,
                    ranked(&["second"]),
                    &HistorySearchArgs::default(),
                    &CancellationToken::new()
                )
                .unwrap()
                .is_empty()
            );
            Ok(())
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn changed_raw_retention_discards_the_old_distance_without_hiding_canonical_only_hits() {
        let (_root, db, _space) = fixture().await;
        db.call(|db| {
            db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','completed','now'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,saved_sequence,retention_generation)VALUES('shell','run','turn','shell-op','exited','/','{}','UTF-8','UTF-8','closed','now',1,0);")?;
            let make = || {
                let mut ranked = ranked(&["first", "second"]);
                let mut span = ranked.hits[0].spans[0].clone(); span.body_kind = "tool_raw".into();
                ranked.hits[0].spans.push(span);
                ranked.witnesses.push(Witness { node_id:"first".into(),revision:0,body_hash:Some("body".into()),raw:Some(RawWitness {execution:"shell".into(),generation:0,saved_sequence:1,encoding:"UTF-8".into()}) });
                ranked
            };
            assert_eq!(refresh_hits(db,make(),&HistorySearchArgs::default(),&CancellationToken::new()).unwrap().len(),2);
            db.execute("UPDATE shell_execution SET retention_generation=1 WHERE id='shell'",[])?;
            let hits=refresh_hits(db,make(),&HistorySearchArgs::default(),&CancellationToken::new()).unwrap();
            assert_eq!(hits.iter().map(|hit|hit.node_id.as_str()).collect::<Vec<_>>(),vec!["second"]);
            assert_eq!(refresh_hits(db,ranked(&["first"]),&HistorySearchArgs::default(),&CancellationToken::new()).unwrap().len(),1);
            Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn selected_space_change_rejects_readonly_search_and_releases_admission() {
        let (_root, db, space) = fixture().await;
        db.call(|db| {
            db.execute("DELETE FROM app_meta WHERE key='index.active_space'", [])?;
            Ok(())
        })
        .await
        .unwrap();
        let slots = Arc::new(Semaphore::new(1));
        let guard = SearchGuard::new(CancellationToken::new());
        let result = rank(
            db.path().into(),
            space,
            "[0.1,0.2]".into(),
            HistorySearchArgs::default(),
            &guard,
            slots.clone().try_acquire_owned().unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(result.code, ErrorCode::InvalidState);
        assert!(result.message.contains("space changed"));
        assert_eq!(slots.available_permits(), 1);
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn admission_rejects_before_embedding_and_shutdown_waits_for_actual_task_permit() {
        let (_root, db, _space) = fixture().await;
        let service = IndexService::new(db.clone(), Arc::new(Config::default()));
        let permit = service.search_slots.clone().try_acquire_owned().unwrap();
        let error = service.search("synthetic", 10).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(
            error.message.contains("already running"),
            "No configuration or HTTP operation should run: {error:?}"
        );
        let waiting = service.clone();
        let mut idle = tokio::spawn(async move {
            waiting.wait_idle().await.unwrap();
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut idle)
                .await
                .is_err()
        );
        drop(permit);
        tokio::time::timeout(Duration::from_secs(1), idle)
            .await
            .unwrap()
            .unwrap();
        db.shutdown().await.unwrap();
    }

    async fn failed_rank(path: PathBuf, space: Space) -> ApiError {
        let slots = Arc::new(Semaphore::new(1));
        let guard = SearchGuard::new(CancellationToken::new());
        let error = rank(
            path,
            space,
            "[0.1,0.2]".into(),
            HistorySearchArgs::default(),
            &guard,
            slots.clone().try_acquire_owned().unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed, "{error:?}");
        assert!(!error.is_storage_failure());
        assert!(
            error
                .message
                .starts_with("Read-only history search failed:")
        );
        assert_eq!(slots.available_permits(), 1);
        error
    }

    async fn subsequent_mutation(db: &DbWorker) {
        db.call(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "UPDATE thread SET title='still writable' WHERE id='thread'",
                [],
            )?;
            tx.commit()?;
            let title: String =
                db.query_row("SELECT title FROM thread WHERE id='thread'", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(title, "still writable");
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn readonly_open_failure_does_not_poison_the_writer_or_hold_admission() {
        let (root, db, space) = fixture().await;
        let error = failed_rank(root.path().join("absent/history.sqlite"), space).await;
        assert!(
            error.message.contains("unable to open database file"),
            "{error:?}"
        );
        subsequent_mutation(&db).await;
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn non_database_reader_file_requires_repair_without_poisoning_writer() {
        let (root, db, space) = fixture().await;
        let path = root.path().join("not-a-database.sqlite");
        std::fs::write(
            &path,
            b"This file is not a SQLite database; preserve the reported cause.",
        )
        .unwrap();
        let error = failed_rank(path, space).await;
        assert!(error.message.contains("not a database"), "{error:?}");
        let hint = error.hint.as_deref().unwrap();
        assert!(
            hint.contains("repair its cause before retrying"),
            "{error:?}"
        );
        assert!(!hint.contains("Wait for"), "{error:?}");
        assert!(hint.contains("no operation receipt"), "{error:?}");
        assert!(hint.contains("embedding charge"), "{error:?}");
        assert_eq!(
            error.details.as_ref().unwrap()["sqlite_primary_code"],
            rusqlite::ffi::SQLITE_NOTADB
        );
        subsequent_mutation(&db).await;
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn readonly_query_failure_is_an_observation_and_preserves_the_sqlite_cause() {
        let (_root, db, space) = fixture().await;
        db.call(|db| {
            db.execute_batch("DROP TABLE embedding_chunk")?;
            Ok(())
        })
        .await
        .unwrap();
        let error = failed_rank(db.path().into(), space).await;
        assert!(
            error.message.contains("no such table: embedding_chunk"),
            "{error:?}"
        );
        let hint = error.hint.as_deref().unwrap();
        assert!(hint.contains("repair the cause first"), "{error:?}");
        assert!(hint.contains("no operation receipt"), "{error:?}");
        assert!(hint.contains("embedding charge"), "{error:?}");
        subsequent_mutation(&db).await;
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn readonly_space_check_replaces_writer_hint_and_keeps_sqlite_cause() {
        let (_root, db, space) = fixture().await;
        // check_space uses db_error, which already carries a writer recovery
        // hint. The actual ranking boundary must replace it before Control can
        // decline to add its own observation hint.
        db.call(|db| {
            db.execute_batch("ALTER TABLE index_space RENAME TO unavailable_index_space")?;
            Ok(())
        })
        .await
        .unwrap();
        let error = failed_rank(db.path().into(), space).await;
        assert!(
            error.message.contains("no such table: index_space"),
            "{error:?}"
        );
        let hint = error.hint.as_deref().unwrap();
        assert!(hint.contains("embedding charge"), "{error:?}");
        assert!(hint.contains("not repaired by retries"), "{error:?}");
        assert!(hint.contains("no operation receipt"), "{error:?}");
        assert_eq!(
            error.details.as_ref().unwrap()["sqlite_primary_code"],
            rusqlite::ffi::SQLITE_ERROR
        );
        assert!(!hint.contains("existing data was preserved"), "{error:?}");
        subsequent_mutation(&db).await;
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn readonly_busy_failure_is_an_observation_and_allows_writes_after_unlock() {
        let (_root, db, space) = fixture().await;
        let path = db.path().to_path_buf();
        // Close WAL writer first so the fixture can deterministically use a
        // rollback-journal exclusive lock on all supported desktop platforms.
        db.shutdown().await.unwrap();
        let lock = Connection::open(&path).unwrap();
        lock.execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .unwrap();
        let error = failed_rank(path.clone(), space).await;
        assert!(error.message.contains("database is locked"), "{error:?}");
        let hint = error.hint.as_deref().unwrap();
        assert!(hint.contains("Wait for the database lock"), "{error:?}");
        assert!(!hint.contains("repair"), "{error:?}");
        assert!(hint.contains("no operation receipt"), "{error:?}");
        assert!(hint.contains("embedding charge"), "{error:?}");
        assert_eq!(
            error.details.as_ref().unwrap()["sqlite_primary_code"],
            rusqlite::ffi::SQLITE_BUSY
        );
        lock.execute_batch("ROLLBACK").unwrap();
        drop(lock);
        let db = DbWorker::open(path).await.unwrap();
        subsequent_mutation(&db).await;
        db.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn readonly_query_error_through_control_keeps_owner_open_for_later_mutation() {
        use crate::protocol::{Command, Request, ThreadSetArgs};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config_path = root.path().join("config.toml");
        std::fs::write(&config_path, format!(
            "default_connection=\"fixture\"\ndefault_model=\"synthetic\"\n[connections.fixture]\nbase_url=\"http://{}/v1\"\n[embedding]\nconnection=\"fixture\"\nmodel=\"synthetic\"\n",
            listener.local_addr().unwrap()
        )).unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config_path),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let config = owner.loaded.config.clone();
        let table = owner.db.call(move |db| {
            let tx = db.transaction()?;
            let embedding = identity::EmbeddingIdentity::configured(&config, identity::profile_key(&config).unwrap());
            let space = ensure_space(&tx, &embedding, "active", 2)?;
            tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','/','/a','fixture','synthetic','now'); INSERT INTO node(id,kind,created_at,payload_json,body_hash)VALUES('first','user','now','{}','body'); INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',0,'first');")?;
            let watermark=json!({"witness":{"node_id":"first","revision":0,"body_hash":"body","raw":null}}).to_string();
            tx.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state,authorized_watermark)VALUES('first',?1,'first',0,'canonical','body',0,1,'done',?2)", params![space.id,watermark])?;
            tx.execute(&format!("INSERT INTO {}(rowid,embedding)VALUES(1,'[0.1,0.2]')", space.table), [])?;
            tx.execute("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES('first','first',?1,'first',0,0,1,'body',1)", [&space.id])?;
            tx.commit()?;
            Ok(space.table)
        }).await.unwrap();
        let writer = owner.db.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0, "Embedding request must arrive before fault injection");
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let len = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            // Fault only the derived vector table after writer-side marker
            // validation, so the next failure is in the real readonly ranking.
            writer
                .call(move |db| {
                    db.execute_batch(&format!("DROP TABLE {table}"))?;
                    Ok(())
                })
                .await
                .unwrap();
            let body = json!({"data":[{"index":0,"embedding":[0.1,0.2]}],"usage":{"prompt_tokens":1,"total_tokens":1}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let control = crate::control::Control::new(&owner, root.path().into());
        let search = control.request(Request::new(
            format!("{}:search", owner.instance_id),
            Command::HistorySearch(HistorySearchArgs {
                query: "synthetic".into(),
                limit: 10,
                ..Default::default()
            }),
        ));
        let response = tokio::time::timeout(Duration::from_secs(10), search)
            .await
            .unwrap();
        assert!(!response.ok);
        let error = response.error.unwrap();
        assert_eq!(error.code, ErrorCode::IoFailed, "{error:?}");
        assert!(error.message.contains("no such table"), "{error:?}");
        assert!(error.hint.as_deref().unwrap().contains("embedding charge"));
        server.await.unwrap();
        let changed = control
            .request(Request::new(
                format!("{}:later-mutation", owner.instance_id),
                Command::ThreadSet(ThreadSetArgs {
                    thread_id: Some("thread".into()),
                    title: Some("owner remained available".into()),
                    ..Default::default()
                }),
            ))
            .await;
        assert!(
            changed.ok,
            "A readonly error must not close owner mutation admission: {:?}",
            changed.error
        );
        let title = owner
            .db
            .call(|db| {
                Ok(
                    db.query_row("SELECT title FROM thread WHERE id='thread'", [], |row| {
                        row.get::<_, String>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(title, "owner remained available");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn readonly_boundary_keeps_domain_errors_and_does_not_change_shared_writer_mapping() {
        let domain = err(ErrorCode::ThreadNotFound, "gone");
        assert_eq!(observation_error(domain.clone()), domain);
        let sqlite = || rusqlite::Error::InvalidQuery;
        let writer_error = sql_error(sqlite());
        assert_eq!(writer_error.code, ErrorCode::SaveFailed);
        assert!(writer_error.is_storage_failure());
        assert_eq!(
            observation_error(sql_error(sqlite())).code,
            ErrorCode::IoFailed
        );
        assert_eq!(sql_error(sqlite()).code, ErrorCode::SaveFailed);
        let writer_hint = db_error(StorageError::from(sqlite()));
        assert!(
            writer_hint
                .hint
                .as_deref()
                .unwrap()
                .contains("storage capacity")
        );
        let observed = observation_error(writer_hint.clone());
        assert_eq!(observed.code, ErrorCode::IoFailed);
        assert!(observed.message.contains(&writer_hint.message));
        assert!(
            observed
                .hint
                .as_deref()
                .unwrap()
                .contains("embedding charge")
        );
        assert!(writer_hint.is_storage_failure());
        assert!(
            writer_hint
                .hint
                .as_deref()
                .unwrap()
                .contains("storage capacity")
        );
    }

    #[tokio::test]
    async fn cancellation_is_observation_failure_and_releases_readonly_task_slot() {
        let (_root, db, space) = fixture().await;
        let slots = Arc::new(Semaphore::new(1));
        let token = CancellationToken::new();
        token.cancel();
        let guard = SearchGuard::new(token);
        let result = rank(
            db.path().into(),
            space,
            "[0.1,0.2]".into(),
            HistorySearchArgs::default(),
            &guard,
            slots.clone().try_acquire_owned().unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(result.code, ErrorCode::InvalidState);
        assert_eq!(slots.available_permits(), 1);
        let token = CancellationToken::new();
        let guard = SearchGuard::new(token.clone());
        drop(guard);
        assert!(token.is_cancelled());
        db.shutdown().await.unwrap();
    }
}
