//! Explicit query embedding and eligible-source vector search.
use super::*;
mod reader;

impl IndexService {
    pub async fn search_report(
        &self,
        args: &HistorySearchArgs,
        cancel: CancellationToken,
    ) -> Result<Value, ApiError> {
        let items = self.search_filtered_cancellable(args, cancel).await?;
        let automatic = self.config.indexing.auto_history;
        Ok(
            json!({"items":items,"auto_history":automatic,"degraded":!automatic,
            "scope_status":[{"scope":args.thread_id.as_ref().map_or_else(||"history".to_owned(),|id|format!("history:{id}")),
            "coverage":"indexed_sources_only","auto_history":automatic,
            "reason":if automatic {None} else {Some("auto_history_disabled")},
            "message":if automatic {"Only retained indexed sources are searched; coverage is not guaranteed complete"} else {"Automatic history indexing is off; newer sources may be missing. Only retained indexed sources are searched; explicitly backfill or use keyword search"}}]}),
        )
    }
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchMatch>, ApiError> {
        Ok(self
            .search_detailed(query, limit)
            .await?
            .into_iter()
            .map(|hit| SearchMatch {
                node_id: hit.node_id,
                excerpt: hit.excerpt,
                distance: hit.distance,
            })
            .collect())
    }
    pub async fn search_detailed(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<IndexHit>, ApiError> {
        self.search_filtered(&HistorySearchArgs {
            query: query.into(),
            limit,
            ..Default::default()
        })
        .await
    }
    /// Restrict the eligible source set before ranking; never post-filter a global KNN shortlist.
    pub async fn search_filtered(
        &self,
        args: &HistorySearchArgs,
    ) -> Result<Vec<IndexHit>, ApiError> {
        self.search_filtered_cancellable(args, self.stop.child_token())
            .await
    }
    /// Query HTTP follows both the caller's lifetime and owner shutdown. Dropping
    /// this bounded future aborts the transport; queued short DB work may finish.
    pub async fn search_filtered_cancellable(
        &self,
        args: &HistorySearchArgs,
        cancel: CancellationToken,
    ) -> Result<Vec<IndexHit>, ApiError> {
        tokio::select! { biased;
            _ = self.stop.cancelled() => Err(self.stopped_search_error()),
            _ = cancel.cancelled() => Err(err(ErrorCode::InvalidState, "Search caller disconnected")),
            result = self.search_filtered_inner(args, cancel.clone()) => result,
        }
    }
    async fn search_filtered_inner(
        &self,
        args: &HistorySearchArgs,
        cancel: CancellationToken,
    ) -> Result<Vec<IndexHit>, ApiError> {
        crate::protocol::Command::HistorySearch(args.clone()).validate()?;
        // No waiting search queue and no query embedding charge before admission.
        let permit = self.search_slots.clone().try_acquire_owned().map_err(|_| {
            err(
                ErrorCode::InvalidState,
                "A semantic search is already running; retry after it finishes",
            )
        })?;
        let guard = reader::SearchGuard::new(cancel.child_token());
        let cancel = guard.token.clone();
        let args = args.clone();
        if let Some(thread) = &args.thread_id {
            let thread = thread.clone();
            let exists = self
                .database
                .call(move |db| {
                    db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                        [thread],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(StorageError::from)
                })
                .await
                .map_err(db_error)?;
            if !exists {
                return Err(err(
                    ErrorCode::ThreadNotFound,
                    "Search Thread does not exist",
                ));
            }
        }
        let config = self.config.clone();
        let space = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let space = meta(&tx, IndexKeys::ACTIVE_SPACE)?
                    .map(|id| load_space(&tx, &id))
                    .transpose()?.flatten();
                if let Some(space) = &space {
                    if !config.connections.contains_key(&space.profile) {
                        return Err(StorageError::from(err(ErrorCode::ConfigInvalid,
                            "Selected index space connection is not configured")));
                    }
                    let marker = meta(&tx, &IndexKeys::query_profile(&space.id))?;
                    identity::query_profile_key(&config, &space.profile, &space.model,
                        marker.as_deref().unwrap_or("")).ok_or_else(|| StorageError::from(err(
                            ErrorCode::InvalidState,
                            "Index query fingerprint is missing or incompatible; explicitly rebuild with the intended embedding configuration")))?;
                }
                tx.commit()?;
                Ok(space)
            })
            .await
            .map_err(db_error)?;
        let Some(space) = space else {
            return Ok(Vec::new());
        };
        self.config.connections.get(&space.profile).ok_or_else(|| {
            err(
                ErrorCode::ConfigInvalid,
                "Selected index space connection is not configured",
            )
        })?;
        let client = self
            .clients
            .get(&space.profile)
            .map_err(|error| err(ErrorCode::IoFailed, error.to_string()))?;
        let response = client
            .embeddings(
                &space.model,
                std::slice::from_ref(&args.query),
                Some(space.dimension),
                space.requested_dimensions,
                cancel.clone(),
            )
            .await
            .map_err(|error| err(ErrorCode::IoFailed, error.to_string()))?;
        let vector = response
            .vectors
            .into_iter()
            .next()
            .expect("one validated query vector");
        let query = serde_json::to_string(&vector)
            .map_err(|_| err(ErrorCode::InvalidState, "Cannot encode query vector"))?;
        reader::rank(
            self.database.path().to_path_buf(),
            space,
            query,
            args,
            &guard,
            permit,
        )
        .await
    }
}
// vec0 accepts rowid-IN candidate sets (sqlite-vec 0.1.9). Validate source
// witnesses and absolute timestamps before handing candidates to KNN. The ID
// list is linear in eligible chunks; no vectors/previews are loaded into Rust.
fn filtered_hits(
    db: &Connection,
    space: &Space,
    query: &str,
    args: &HistorySearchArgs,
) -> Result<Vec<IndexHit>, ApiError> {
    Ok(ranked_hits(db, space, query, args, None)?.hits)
}
struct RankedHits {
    hits: Vec<IndexHit>,
    witnesses: Vec<Witness>,
}
fn ensure_running(cancel: Option<&CancellationToken>) -> Result<(), ApiError> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(err(
            ErrorCode::InvalidState,
            "Semantic search was cancelled",
        ));
    }
    Ok(())
}
fn ranked_hits(
    db: &Connection,
    space: &Space,
    query: &str,
    args: &HistorySearchArgs,
    cancel: Option<&CancellationToken>,
) -> Result<RankedHits, ApiError> {
    ensure_running(cancel)?;
    #[cfg(test)]
    performance_tests::ranking_started();
    valid_table(&space.table).map_err(db_error)?;
    if let Some(thread) = &args.thread_id {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                [thread],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if !exists {
            return Err(err(
                ErrorCode::ThreadNotFound,
                "Search Thread does not exist",
            ));
        }
    }
    let from = args
        .from
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid from timestamp"))?;
    let to = args
        .to
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid to timestamp"))?;
    let kinds = serde_json::to_string(&args.kinds)
        .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid kinds"))?;
    let excluded = serde_json::to_string(&args.exclude_kinds)
        .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid excluded kinds"))?;
    let eligible_sql = format!("{} SELECT c.vec_row_id,c.source_node_id,c.source_revision,j.authorized_watermark,n.created_at,j.body_kind
    FROM embedding_chunk c JOIN embedding_job j ON j.id=c.job_id AND j.state='done' AND j.space_id=c.space_id AND j.source_node_id=c.source_node_id AND j.source_revision=c.source_revision AND j.source_hash=c.content_hash AND j.span_start=c.span_start AND j.span_end=c.span_end
    JOIN node n ON n.id=c.source_node_id
    WHERE c.space_id=?1 AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL AND n.payload_revision=c.source_revision
      AND (?2 IS NULL OR n.id IN (SELECT id FROM scope))
      AND (json_array_length(?3)=0 OR n.kind IN (SELECT value FROM json_each(?3)))
      AND n.kind NOT IN (SELECT value FROM json_each(?4))", crate::history::search_scope::cte("?2"));
    let mut statement = db.prepare(&eligible_sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![space.id, args.thread_id, kinds, excluded])
        .map_err(sql_error)?;
    const WITNESS_BATCH: usize = 256;
    const VISIBILITY_BATCH: usize = 8192;
    let mut candidate_rows = Vec::<(i64, Witness, String)>::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        ensure_running(cancel)?;
        if from.is_some() || to.is_some() {
            let created: String = row.get(4).map_err(sql_error)?;
            let Ok(created) = chrono::DateTime::parse_from_rfc3339(&created) else {
                continue;
            };
            if from.is_some_and(|from| created < from) || to.is_some_and(|to| created >= to) {
                continue;
            }
        }
        let watermark: Option<String> = row.get(3).map_err(sql_error)?;
        let Some(mut witness) = watermark.as_deref().and_then(Witness::from_watermark) else {
            continue;
        };
        let kind: String = row.get(5).map_err(sql_error)?;
        if kind == "tool_raw" {
            if witness.raw.is_none() {
                continue;
            }
        } else {
            witness.raw = None;
        }
        let node: String = row.get(1).map_err(sql_error)?;
        let revision: i64 = row.get(2).map_err(sql_error)?;
        if witness.node_id != node || witness.revision != revision {
            continue;
        }
        candidate_rows.push((row.get(0).map_err(sql_error)?, witness, kind));
    }

    let mut witness_checks = HashMap::<Witness, bool>::new();
    let mut witness_kinds = HashMap::<Witness, String>::new();
    for (_, witness, kind) in &candidate_rows {
        witness_kinds
            .entry(witness.clone())
            .or_insert_with(|| kind.clone());
    }
    let witness_items: Vec<_> = witness_kinds.into_iter().collect();
    for batch in witness_items.chunks(WITNESS_BATCH) {
        ensure_running(cancel)?;
        let valid = witnesses_valid_many(db, batch).map_err(db_error)?;
        for ((witness, _), valid) in batch.iter().zip(valid) {
            witness_checks.insert(witness.clone(), valid);
        }
    }
    let mut eligible_nodes = Vec::<String>::new();
    let mut seen_nodes = HashSet::<String>::new();
    for (_, witness, _) in &candidate_rows {
        if witness_checks.get(witness) == Some(&true) && seen_nodes.insert(witness.node_id.clone())
        {
            eligible_nodes.push(witness.node_id.clone());
        }
    }
    let mut visibility = HashMap::<String, bool>::new();
    for batch in eligible_nodes.chunks(VISIBILITY_BATCH) {
        ensure_running(cancel)?;
        let visible = history::reachability::searchable_many(db, batch).map_err(sql_error)?;
        visibility.extend(batch.iter().cloned().zip(visible));
    }
    let mut eligible = Vec::<i64>::new();
    for (chunk, witness, _) in candidate_rows {
        if witness_checks.get(&witness) == Some(&true)
            && visibility.get(&witness.node_id) == Some(&true)
        {
            eligible.push(chunk);
        }
    }
    if eligible.is_empty() {
        return Ok(RankedHits {
            hits: Vec::new(),
            witnesses: Vec::new(),
        });
    }
    let count = eligible.len();
    let candidates = serde_json::to_string(&eligible)
        .map_err(|_| err(ErrorCode::InvalidState, "Cannot encode search candidates"))?;
    let sql = format!("SELECT c.source_node_id,c.span_start,c.span_end,j.body_kind,v.distance,m.value,c.source_revision,c.content_hash
        FROM {} v JOIN embedding_chunk c ON c.vec_row_id=v.rowid AND c.space_id=?1 JOIN embedding_job j ON j.id=c.job_id
        LEFT JOIN app_meta m ON m.key=?5||c.id
        WHERE v.embedding MATCH ?2 AND k=?3 AND v.rowid IN (SELECT value FROM json_each(?4)) ORDER BY v.distance",space.table);
    let mut k = (args.limit as usize * 8).min(count).min(4096);
    let hits = loop {
        ensure_running(cancel)?;
        let mut statement = db.prepare(&sql).map_err(sql_error)?;
        let mut rows = statement
            .query(params![
                space.id,
                query,
                k as i64,
                candidates,
                IndexKeys::PREVIEW_PREFIX
            ])
            .map_err(sql_error)?;
        let mut hits: Vec<IndexHit> = Vec::new();
        while let Some(row) = rows.next().map_err(sql_error)? {
            ensure_running(cancel)?;
            let node: String = row.get(0).map_err(sql_error)?;
            let span = SourceSpan {
                source_revision: row.get(6).map_err(sql_error)?,
                content_hash: row.get(7).map_err(sql_error)?,
                body_kind: row.get(3).map_err(sql_error)?,
                start: row.get::<_, i64>(1).map_err(sql_error)? as usize,
                end: row.get::<_, i64>(2).map_err(sql_error)? as usize,
            };
            if let Some(hit) = hits.iter_mut().find(|hit| hit.node_id == node) {
                hit.spans.push(span);
            } else {
                hits.push(IndexHit {
                    thread_scope: search_thread_scope(db, args, &node)?,
                    provenance: history::provenance(db, &node)?,
                    node_id: node,
                    excerpt: row
                        .get::<_, Option<String>>(5)
                        .map_err(sql_error)?
                        .unwrap_or_default(),
                    distance: row.get(4).map_err(sql_error)?,
                    spans: vec![span],
                });
            }
        }
        hits.sort_by(|a, b| {
            a.distance
                .total_cmp(&b.distance)
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        if hits.len() >= args.limit as usize || k >= count {
            hits.truncate(args.limit as usize);
            break hits;
        }
        if k == 4096 {
            break exact_filtered_hits(db, space, query, args, &candidates, cancel)?;
        }
        k = (k * 2).min(count).min(4096);
    };
    let selected: HashSet<_> = hits.iter().map(|hit| hit.node_id.as_str()).collect();
    let witnesses = witness_checks
        .into_iter()
        .filter(|(witness, valid)| *valid && selected.contains(witness.node_id.as_str()))
        .map(|(witness, _)| witness)
        .collect();
    Ok(RankedHits { hits, witnesses })
}

/// Read-only snapshot adapter for standalone RAG. Does not open/migrate the
/// conversation store or start indexing workers.
pub(crate) fn rag_profile_key(config: &Config, space: &SpaceDescriptor) -> Option<String> {
    identity::query_profile_key(
        config,
        &space.profile,
        &space.model,
        &space.query_profile_key,
    )
    .map(|key| {
        space.requested_dimensions.map_or_else(
            || key.clone(),
            |n| hash(format!("requested-dimension-v1\0{key}\0{n}").as_bytes()),
        )
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpaceDescriptor {
    pub(crate) id: String,
    pub(crate) profile: String,
    pub(crate) model: String,
    pub(crate) dimension: usize,
    pub(crate) requested_dimensions: Option<usize>,
    pub(crate) splitter: String,
    pub(crate) query_profile_key: String,
    table: String,
}
pub(crate) fn rag_space(db: &Connection) -> Result<Option<SpaceDescriptor>, ApiError> {
    let Some(id) = meta(db, IndexKeys::ACTIVE_SPACE).map_err(db_error)? else {
        return Ok(None);
    };
    let Some(space) = load_space(db, &id).map_err(db_error)? else {
        return Ok(None);
    };
    let query_profile_key = meta(db, &IndexKeys::query_profile(&space.id)).map_err(db_error)?
        .ok_or_else(|| err(ErrorCode::InvalidState, "History index query fingerprint is missing; explicitly rebuild with the intended embedding configuration"))?;
    Ok(Some(SpaceDescriptor {
        query_profile_key,
        id: space.id,
        profile: space.profile,
        model: space.model,
        dimension: space.dimension,
        requested_dimensions: space.requested_dimensions,
        splitter: space.splitter,
        table: space.table,
    }))
}
pub(crate) fn rag_hits(
    db: &Connection,
    descriptor: &SpaceDescriptor,
    vector: &[f32],
    limit: u32,
    thread: Option<&str>,
) -> Result<Vec<IndexHit>, ApiError> {
    let Some(current) = rag_space(db)? else {
        return Err(err(
            ErrorCode::InvalidState,
            "History embedding space is unavailable",
        ));
    };
    if current != *descriptor {
        return Err(err(
            ErrorCode::InvalidState,
            "History embedding space changed; retry search",
        ));
    }
    let space = load_space(db, &current.id)
        .map_err(db_error)?
        .ok_or_else(|| {
            err(
                ErrorCode::InvalidState,
                "History embedding space is unavailable",
            )
        })?;
    let query = serde_json::to_string(vector)
        .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid query embedding"))?;
    filtered_hits(
        db,
        &space,
        &query,
        &HistorySearchArgs {
            query: "rag query".into(),
            limit,
            thread_id: thread.map(str::to_owned),
            ..Default::default()
        },
    )
}
fn search_thread_scope(
    db: &Connection,
    args: &HistorySearchArgs,
    node: &str,
) -> Result<Option<SearchThreadScope>, ApiError> {
    args.thread_id
        .as_ref()
        .map(|thread| {
            let on_current_path = crate::history::search_scope::on_current_path(db, thread, node)
                .map_err(sql_error)?;
            Ok(SearchThreadScope {
                thread_id: thread.clone(),
                on_current_path,
            })
        })
        .transpose()
}
/// More than 4096 nearest chunks can belong to fewer than limit nodes. Only
/// that rare vec0 ceiling case uses exact L2 across the already eligible set.
fn exact_filtered_hits(
    db: &Connection,
    space: &Space,
    query: &str,
    args: &HistorySearchArgs,
    candidates: &str,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<IndexHit>, ApiError> {
    let sql = format!("SELECT c.source_node_id,c.span_start,c.span_end,j.body_kind,vec_distance_L2(v.embedding,?2) AS distance,m.value,c.source_revision,c.content_hash
        FROM embedding_chunk c JOIN {} v ON v.rowid=c.vec_row_id JOIN embedding_job j ON j.id=c.job_id
        LEFT JOIN app_meta m ON m.key=?4||c.id
        WHERE c.space_id=?1 AND c.vec_row_id IN (SELECT value FROM json_each(?3))
        ORDER BY distance,c.source_node_id,c.span_start,c.span_end,j.body_kind,c.id",space.table);
    let mut statement = db.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            space.id,
            query,
            candidates,
            IndexKeys::PREVIEW_PREFIX
        ])
        .map_err(sql_error)?;
    let mut hits: Vec<IndexHit> = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        ensure_running(cancel)?;
        let node: String = row.get(0).map_err(sql_error)?;
        let span = SourceSpan {
            source_revision: row.get(6).map_err(sql_error)?,
            content_hash: row.get(7).map_err(sql_error)?,
            body_kind: row.get(3).map_err(sql_error)?,
            start: row.get::<_, i64>(1).map_err(sql_error)? as usize,
            end: row.get::<_, i64>(2).map_err(sql_error)? as usize,
        };
        if let Some(hit) = hits.iter_mut().find(|hit| hit.node_id == node) {
            hit.spans.push(span);
        } else if hits.len() < args.limit as usize {
            hits.push(IndexHit {
                thread_scope: search_thread_scope(db, args, &node)?,
                provenance: history::provenance(db, &node)?,
                node_id: node,
                excerpt: row
                    .get::<_, Option<String>>(5)
                    .map_err(sql_error)?
                    .unwrap_or_default(),
                distance: row.get(4).map_err(sql_error)?,
                spans: vec![span],
            });
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod visibility_cost_tests {
    use super::*;

    /// A diagnostic measurement, not a latency SLA: same production visibility
    /// function, bundled SQLite, no provider traffic or persisted user history.
    #[tokio::test]
    async fn visibility_cost_across_shared_replacement_depths() {
        for depth in [0, 8, 64] {
            let temp = tempfile::tempdir().unwrap();
            let worker = DbWorker::open(temp.path().join("visibility.sqlite"))
                .await
                .unwrap();
            worker.call(move |db| {
                let tx = db.transaction()?;
                tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,deleted_at) VALUES('deleted','/','/a','p','m','now','now'),('live','/','/a','p','m','now',NULL);")?;
                for i in 0..=depth {
                    let summary = format!("summary-{i}");
                    tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'summary','now','{}')", [&summary])?;
                    tx.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES(?1,?1,?1,'1','1','completed','now')", [&summary])?;
                    if i > 0 {
                        tx.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES(?1,0,?2,0)", params![summary,format!("summary-{}",i-1)])?;
                    }
                }
                tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('live',0,?1)",[format!("summary-{depth}")])?;
                for i in 0..256 {
                    let source = format!("source-{i}");
                    tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')", [&source])?;
                    tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('deleted',?1,?2)",params![i,source])?;
                    tx.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES('summary-0',?1,?2,0)",params![i,source])?;
                }
                tx.commit()?;
                for live in [true, false] {
                    if !live {
                        db.execute("UPDATE thread SET deleted_at='now' WHERE id='live'", [])?;
                    }
                    let started = std::time::Instant::now();
                    for i in 0..256 {
                        assert_eq!(history::reachability::searchable(db, &format!("source-{i}"))?, live);
                    }
                    eprintln!("visibility sources=256 shared_depth={depth} live={live} elapsed_us={}",started.elapsed().as_micros());
                }
                Ok(())
            }).await.unwrap();
            worker.shutdown().await.unwrap();
        }
    }
}

#[cfg(test)]
mod performance_tests;

#[cfg(test)]
mod validation_benchmark_tests {
    use super::*;
    use rusqlite::OpenFlags;
    use std::time::Instant;

    #[test]
    #[ignore = "read-only real-history validation benchmark; set HISTORY_SEARCH_SNAPSHOT"]
    fn compare_old_and_batched_validation_on_snapshot() {
        let path = std::env::var("HISTORY_SEARCH_SNAPSHOT").unwrap();
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        db.pragma_update(None, "query_only", true).unwrap();
        let mut statement = db.prepare("SELECT DISTINCT j.authorized_watermark,j.body_kind FROM embedding_chunk c JOIN embedding_job j ON j.id=c.job_id AND j.state='done' AND j.space_id=c.space_id AND j.source_node_id=c.source_node_id AND j.source_revision=c.source_revision AND j.source_hash=c.content_hash AND j.span_start=c.span_start AND j.span_end=c.span_end JOIN node n ON n.id=c.source_node_id WHERE n.payload_json IS NOT NULL AND n.deleted_reason IS NULL AND n.payload_revision=c.source_revision").unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap();
        let mut witnesses = HashMap::<Witness, String>::new();
        for row in rows {
            let (watermark, kind) = row.unwrap();
            let Some(mut witness) = watermark.as_deref().and_then(Witness::from_watermark) else {
                continue;
            };
            if kind == "tool_raw" {
                if witness.raw.is_none() {
                    continue;
                }
            } else {
                witness.raw = None;
            }
            witnesses.entry(witness).or_insert(kind);
        }
        let inputs: Vec<_> = witnesses.into_iter().collect();
        let ids: Vec<_> = inputs
            .iter()
            .map(|(w, _)| w.node_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let started = Instant::now();
        let old_witness: Vec<_> = inputs
            .iter()
            .map(|(w, kind)| witness_valid(&db, w, kind).unwrap())
            .collect();
        let old_witness_elapsed = started.elapsed();
        let started = Instant::now();
        let old_visible: Vec<_> = ids
            .iter()
            .map(|id| history::reachability::searchable(&db, id).unwrap())
            .collect();
        let old_visibility_elapsed = started.elapsed();

        let started = Instant::now();
        let new_witness = witnesses_valid_many(&db, &inputs).unwrap();
        let new_witness_elapsed = started.elapsed();
        let started = Instant::now();
        let new_visible = ids
            .chunks(8192)
            .flat_map(|batch| history::reachability::searchable_many(&db, batch).unwrap())
            .collect::<Vec<_>>();
        let new_visibility_elapsed = started.elapsed();
        assert_eq!(new_witness, old_witness);
        assert_eq!(new_visible, old_visible);
        eprintln!(
            "history-search validation snapshot candidates={} witnesses={} old_witness_ms={} old_visibility_ms={} batch_witness_ms={} batch_visibility_ms={}",
            ids.len(),
            inputs.len(),
            old_witness_elapsed.as_millis(),
            old_visibility_elapsed.as_millis(),
            new_witness_elapsed.as_millis(),
            new_visibility_elapsed.as_millis()
        );
    }
}
