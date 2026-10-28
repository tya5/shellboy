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
            crate::storage::register_vec().map_err(db_error)?;
            let db = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(sql_error)?;
            *interrupt.lock().expect("search interrupt poisoned") = Some(db.get_interrupt_handle());
            // A cancellation racing connection creation still interrupts before
            // the first statement, without relying on a lost Notify permit.
            ensure_running(Some(&cancel))?;
            db.busy_timeout(Duration::from_millis(100))
                .map_err(sql_error)?;
            db.execute_batch("PRAGMA query_only=ON;")
                .map_err(sql_error)?;
            let version: i64 = db
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(sql_error)?;
            if version != crate::storage::SCHEMA_VERSION {
                return Err(err(
                    ErrorCode::InvalidState,
                    "History search schema changed; retry after reopening the owner",
                ));
            }
            let snapshot = db.unchecked_transaction().map_err(sql_error)?;
            check_space(&snapshot, &space)?;
            let ranked = ranked_hits(&snapshot, &space, &query, &args, Some(&cancel))?;
            snapshot.commit().map_err(sql_error)?;
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
            let current = db.unchecked_transaction().map_err(sql_error)?;
            check_space(&current, &space)?;
            let hits = refresh_hits(&current, ranked, &args, &cancel)?;
            current.commit().map_err(sql_error)?;
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
            result
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

fn check_space(db: &Connection, expected: &Space) -> Result<(), ApiError> {
    let selected = meta(db, IndexKeys::ACTIVE_SPACE).map_err(db_error)?;
    let current = selected
        .as_deref()
        .map(|id| load_space(db, id))
        .transpose()
        .map_err(db_error)?
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
            .map_err(db_error)?;
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
            .map_err(sql_error)?;
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
        let mut query=db.prepare("WITH RECURSIVE scope(id) AS(SELECT node_id FROM thread_path WHERE thread_id=?1 UNION SELECT rs.source_node_id FROM scope s JOIN replacement r ON r.output_node_id=s.id AND r.state='completed' JOIN replacement_source rs ON rs.replacement_id=r.id) SELECT id FROM scope WHERE id IN(SELECT value FROM json_each(?2))").map_err(sql_error)?;
        Some(
            query
                .query_map(params![thread, ids], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(sql_error)?,
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
            || !history::reachability::searchable(db, &hit.node_id).map_err(sql_error)?
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
            let embedding = identity::EmbeddingIdentity { key:"fixture-key".into(), profile:"fixture".into(), model:"synthetic".into(), query_key:"fixture-query".into() };
            let space = ensure_space(&tx, &embedding, "active", 2, true)?;
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
