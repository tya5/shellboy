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
        let lease = client.acquire_rate(&cancel).await.map_err(|error| {
            let api = err(ErrorCode::IoFailed, error.to_string());
            if error.kind == "rate_limit_deferred" {
                api.details(json!({"failure_kind":"rate_limit_deferred"}))
            } else {
                api
            }
        })?;
        let expected = space.clone();
        let target = args.thread_id.clone();
        self.database
            .call(move |db| {
                reader::check_space(db, &expected).map_err(StorageError::from)?;
                if let Some(thread) = target {
                    let exists: bool = db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                        [thread],
                        |row| row.get(0),
                    )?;
                    if !exists {
                        return Err(StorageError::from(err(
                            ErrorCode::ThreadNotFound,
                            "Search Thread does not exist",
                        )));
                    }
                }
                Ok(())
            })
            .await
            .map_err(db_error)?;
        let response = client
            .embeddings_admitted(
                &space.model,
                std::slice::from_ref(&args.query),
                Some(space.dimension),
                space.requested_dimensions,
                cancel.clone(),
                lease,
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
    let mut eligible = Vec::<i64>::new();
    let mut witness_checks = HashMap::<Witness, bool>::new();
    let mut visibility = HashMap::<String, bool>::new();
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
        let witness = watermark.as_deref().and_then(Witness::from_watermark);
        let Some(mut witness) = witness else {
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
        let valid = if let Some(valid) = witness_checks.get(&witness) {
            *valid
        } else {
            let valid = witness_valid(db, &witness, &kind).map_err(db_error)?;
            witness_checks.insert(witness, valid);
            valid
        };
        if valid {
            let searchable = if let Some(visible) = visibility.get(&node) {
                *visible
            } else {
                let visible = history::reachability::searchable(db, &node).map_err(sql_error)?;
                visibility.insert(node, visible);
                visible
            };
            if searchable {
                eligible.push(row.get(0).map_err(sql_error)?);
            }
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
    // One vec0 scan of the largest allowed k (#240): every scan reads the whole
    // chunk table, so the old k*2 re-scans dominated. The former doubling
    // schedule is replayed over the ordered rows by `pick_nodes`.
    let outcome = pick_nodes(args.limit as usize, count, |k| {
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
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(sql_error)? {
            ensure_running(cancel)?;
            out.push(KnnRow {
                node: row.get(0).map_err(sql_error)?,
                span: SourceSpan {
                    source_revision: row.get(6).map_err(sql_error)?,
                    content_hash: row.get(7).map_err(sql_error)?,
                    body_kind: row.get(3).map_err(sql_error)?,
                    start: row.get::<_, i64>(1).map_err(sql_error)? as usize,
                    end: row.get::<_, i64>(2).map_err(sql_error)? as usize,
                },
                excerpt: row
                    .get::<_, Option<String>>(5)
                    .map_err(sql_error)?
                    .unwrap_or_default(),
                distance: row.get(4).map_err(sql_error)?,
            });
        }
        Ok(out)
    })?;
    let hits = match outcome {
        KnnOutcome::Picked(picked) => picked
            .into_iter()
            .map(|p| {
                Ok(IndexHit {
                    thread_scope: search_thread_scope(db, args, &p.node)?,
                    provenance: history::provenance(db, &p.node)?,
                    node_id: p.node,
                    excerpt: p.excerpt,
                    distance: p.distance,
                    spans: p.spans,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?,
        KnnOutcome::NeedExact => exact_filtered_hits(db, space, query, args, &candidates, cancel)?,
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
struct KnnRow {
    node: String,
    span: SourceSpan,
    excerpt: String,
    distance: f64,
}
struct Picked {
    node: String,
    excerpt: String,
    distance: f64,
    spans: Vec<SourceSpan>,
}
enum KnnOutcome {
    Picked(Vec<Picked>),
    NeedExact,
}
/// Nodes of the nearest chunks, best first. `scan(k)` returns the k nearest
/// eligible chunks ordered by distance. The result equals the former loop that
/// rescanned with k = limit*8, doubling up to count/4096 until `limit` distinct
/// nodes appeared: each stage there saw exactly the first k rows, so the same
/// stages are replayed over one ordered scan of min(count, 4096) rows. Spans of
/// a node therefore never include rows beyond the stage that satisfied limit.
fn pick_nodes(
    limit: usize,
    count: usize,
    mut scan: impl FnMut(usize) -> Result<Vec<KnnRow>, ApiError>,
) -> Result<KnnOutcome, ApiError> {
    let mut stage = (limit * 8).min(count).min(4096);
    let rows = scan(count.min(4096))?;
    let mut picked: Vec<Picked> = Vec::new();
    let mut index = HashMap::<String, usize>::new();
    // Stage boundary after `stage` rows: done, exact fallback, or next stage.
    let boundary = |picked: &Vec<Picked>, stage: &mut usize| {
        if picked.len() >= limit || *stage >= count {
            return Some(false);
        }
        if *stage == 4096 {
            return Some(true);
        }
        *stage = (*stage * 2).min(count).min(4096);
        None
    };
    let finish = |mut picked: Vec<Picked>, exact: bool| {
        if exact {
            return KnnOutcome::NeedExact;
        }
        picked.sort_by(|a, b| {
            a.distance
                .total_cmp(&b.distance)
                .then_with(|| a.node.cmp(&b.node))
        });
        picked.truncate(limit);
        KnnOutcome::Picked(picked)
    };
    for (n, row) in rows.into_iter().enumerate() {
        while n == stage {
            if let Some(exact) = boundary(&picked, &mut stage) {
                return Ok(finish(picked, exact));
            }
        }
        if let Some(&i) = index.get(&row.node) {
            picked[i].spans.push(row.span);
        } else {
            index.insert(row.node.clone(), picked.len());
            picked.push(Picked {
                node: row.node,
                excerpt: row.excerpt,
                distance: row.distance,
                spans: vec![row.span],
            });
        }
    }
    // Fewer rows than the stage asked for: the former loop would rescan with a
    // larger k and receive the same rows, ending at count or the exact fallback.
    loop {
        if let Some(exact) = boundary(&picked, &mut stage) {
            return Ok(finish(picked, exact));
        }
    }
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
mod pick_nodes_tests {
    use super::*;

    fn row(node: usize, i: usize) -> KnnRow {
        KnnRow {
            node: format!("n{node:04}"),
            span: SourceSpan {
                source_revision: 0,
                content_hash: String::new(),
                body_kind: String::new(),
                start: i,
                end: i + 1,
            },
            excerpt: String::new(),
            distance: i as f64 / 100.0,
        }
    }

    // (node, distance, span starts) per hit, or None for the exact fallback.
    type Shape = Option<Vec<(String, u64, Vec<usize>)>>;

    // The former loop, verbatim in structure: rescan with doubled k.
    fn old_loop(rows: &[KnnRow], limit: usize, count: usize) -> Shape {
        let mut k = (limit * 8).min(count).min(4096);
        loop {
            let mut hits: Vec<(String, f64, Vec<usize>)> = Vec::new();
            for r in rows.iter().take(k) {
                if let Some(h) = hits.iter_mut().find(|h| h.0 == r.node) {
                    h.2.push(r.span.start);
                } else {
                    hits.push((r.node.clone(), r.distance, vec![r.span.start]));
                }
            }
            hits.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            if hits.len() >= limit || k >= count {
                hits.truncate(limit);
                return Some(
                    hits.into_iter()
                        .map(|h| (h.0, h.1.to_bits(), h.2))
                        .collect(),
                );
            }
            if k == 4096 {
                return None;
            }
            k = (k * 2).min(count).min(4096);
        }
    }

    #[test]
    fn single_scan_equals_doubling_rescans() {
        let mut state = 7u64;
        let mut rand = move |n: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) % n) as usize
        };
        let (mut one_scan, mut exact) = (0, 0);
        for case in 0..300 {
            // Few nodes with many chunks make distinct nodes arrive late.
            let total = [5, 90, 700, 5000][case % 4];
            let nodes = 1 + rand(if case % 3 == 0 { 6 } else { 400 } as u64);
            let rows: Vec<KnnRow> = (0..total).map(|i| row(rand(nodes as u64), i)).collect();
            let limit = [0, 1, 3, 10, 50][case % 5];
            let mut scans = Vec::new();
            let got = pick_nodes(limit, total, |k| {
                scans.push(k);
                Ok(rows
                    .iter()
                    .take(k)
                    .map(|r| row(r.node[1..].parse().unwrap(), r.span.start))
                    .collect())
            })
            .ok()
            .unwrap();
            let got: Shape = match got {
                KnnOutcome::NeedExact => None,
                KnnOutcome::Picked(p) => Some(
                    p.into_iter()
                        .map(|p| {
                            (
                                p.node,
                                p.distance.to_bits(),
                                p.spans.iter().map(|s| s.start).collect(),
                            )
                        })
                        .collect(),
                ),
            };
            assert_eq!(scans, vec![total.min(4096)], "exactly one scan");
            assert_eq!(got, old_loop(&rows, limit, total), "case {case}");
            if got.is_none() {
                exact += 1
            } else {
                one_scan += 1
            }
        }
        assert!(one_scan > 0 && exact > 0, "{one_scan}/{exact}");
    }
}
