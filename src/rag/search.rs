use super::*;
use crate::{
    config::{Config, PathEnvironment},
    model::http::HttpModelClient,
};
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, HashSet};
use tokio_util::sync::CancellationToken;

pub(super) struct Context {
    pub database: PathBuf,
    pub history: PathBuf,
    pub config: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub cwd: PathBuf,
}
pub(super) struct Profile {
    pub id: String,
    pub name: String,
    pub model: String,
    pub requested_dimensions: Option<usize>,
    pub endpoint_hash: String,
    pub client: HttpModelClient,
}
pub(super) fn embedding_profile(context: &Context) -> Result<Profile> {
    let loaded = Config::load_with_overrides(
        context.config.as_deref(),
        context.data_dir.as_deref(),
        &context.cwd,
        &PathEnvironment::current(),
    )
    .map_err(|_| {
        err(
            "config_invalid",
            "Embedding configuration could not be loaded",
        )
    })?;
    let name = loaded.config.embedding.connection.as_ref().ok_or_else(|| {
        err(
            "config_invalid",
            "Embedding connection/model are required for semantic processing",
        )
    })?;
    let model = loaded.config.embedding.model.as_ref().ok_or_else(|| {
        err(
            "config_invalid",
            "Embedding connection/model are required for semantic processing",
        )
    })?;
    let connection = loaded
        .config
        .connections
        .get(name)
        .ok_or_else(|| err("config_invalid", "Embedding connection is not configured"))?;
    let id = hash(
        format!(
            "{name}\0{}\0{model}\0rag-unicode-2000-v1",
            connection.base_url
        )
        .as_bytes(),
    );
    let client = HttpModelClient::for_connection(name, connection.clone())
        .map_err(|_| err("config_invalid", "Cannot initialize embedding connection"))?;
    Ok(Profile {
        id,
        name: name.clone(),
        model: model.clone(),
        requested_dimensions: None,
        endpoint_hash: hash(connection.base_url.as_bytes()),
        client,
    })
}
pub(super) async fn embed(
    profile: &Profile,
    texts: &[String],
    dimension: Option<usize>,
) -> Result<crate::model::http::EmbeddingResponse> {
    let response = profile
        .client
        .embeddings(
            &profile.model,
            texts,
            dimension,
            profile.requested_dimensions,
            CancellationToken::new(),
        )
        .await
        .map_err(|error| {
            let failure = error.embedding_failure();
            err(
                if failure.credentials_or_credit || failure.rate_limited {
                    "embedding_paused"
                } else {
                    "embedding_failed"
                },
                if error.kind == "timeout" {
                    // Timeout diagnostics are generated locally by the HTTP
                    // client; provider errors remain redacted to their kind.
                    format!("Embedding request failed (timeout): {}", error.message)
                } else {
                    format!("Embedding request failed ({})", error.kind)
                },
            )
        })?;
    if response.dimension > crate::storage::MAX_VECTOR_DIMENSIONS {
        return Err(err(
            "index_incompatible",
            "Embedding dimension exceeds sqlite-vec limit 8192",
        ));
    }
    Ok(response)
}
pub(super) async fn embed_revision(
    s: &mut store::Store,
    revision: &str,
    profile: &Profile,
) -> Result<()> {
    let pending = {
        let mut q=s.db.prepare("SELECT DISTINCT c.hash,c.text FROM chunks c WHERE c.revision_id=?1 AND NOT EXISTS(SELECT 1 FROM embeddings e WHERE e.space_id=?2 AND e.hash=c.hash) ORDER BY c.ordinal")?;
        q.query_map(params![revision, profile.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for batch in pending.chunks(8) {
        let dimension: Option<usize> =
            s.db.query_row(
                "SELECT dimension FROM spaces WHERE id=?1",
                [&profile.id],
                |r| r.get::<_, i64>(0).map(|n| n as usize),
            )
            .optional()?;
        let response = embed(
            profile,
            &batch.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(),
            dimension,
        )
        .await?;
        let table = format!("rag_vec_{}", &profile.id[..32]);
        let tx = s.db.transaction()?;
        tx.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(embedding float[{}]);",
            response.dimension
        ))?;
        tx.execute(
            "INSERT OR IGNORE INTO spaces VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                profile.id,
                response.dimension as i64,
                profile.name,
                profile.model,
                profile.endpoint_hash,
                table
            ],
        )?;
        for ((content_hash, _), vector) in batch.iter().zip(response.vectors) {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM embeddings WHERE space_id=?1 AND hash=?2)",
                params![profile.id, content_hash],
                |r| r.get(0),
            )?;
            if exists {
                continue;
            }
            let row: i64 = tx.query_row(
                "SELECT COALESCE(MAX(vec_rowid),0)+1 FROM embeddings WHERE space_id=?1",
                [&profile.id],
                |r| r.get(0),
            )?;
            tx.execute(
                &format!("INSERT INTO {table}(rowid,embedding)VALUES(?1,?2)"),
                params![row, serde_json::to_string(&vector).expect("finite vector")],
            )?;
            tx.execute(
                "INSERT INTO embeddings VALUES(?1,?2,?3)",
                params![profile.id, content_hash, row],
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct Hit {
    pub reference: Reference,
    pub excerpt: String,
    pub title: String,
    pub kind: String,
    pub location: Value,
    pub provenance: Value,
    pub scopes: Vec<String>,
    pub thread_scopes: Vec<crate::index::SearchThreadScope>,
    pub score: f64,
    pub distance: Option<f64>,
    pub space: Option<String>,
    pub method: String,
}
impl Hit {
    pub(super) fn compare(a: &Self, b: &Self) -> std::cmp::Ordering {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.reference.id.cmp(&b.reference.id))
    }

    fn value(&self) -> Value {
        let mut value = json!({"reference":encode(&self.reference),"revision":self.reference.revision,"hash":self.reference.hash,"title":self.title,"excerpt":self.excerpt,"kind":self.kind,"location":self.location,"provenance":self.provenance,"scopes":self.scopes,"score":self.score,"distance":self.distance,"embedding_space":self.space,"method":self.method});
        if self.thread_scopes.len() == 1 {
            value["thread_scope"] = json!(self.thread_scopes[0]);
        } else if !self.thread_scopes.is_empty() {
            value["thread_scopes"] = json!(self.thread_scopes);
        }
        value
    }
}
fn excerpt(text: &str) -> String {
    text.chars().take(500).collect()
}
pub(super) fn keyword_score(text: &str, query: &str) -> Option<f64> {
    let text = text.to_lowercase();
    let query = query.to_lowercase();
    let count = text.matches(&query).count();
    (count > 0).then_some(count as f64)
}
fn docs_keyword(query: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for doc in crate::docs::documents() {
        if let Some(score) = keyword_score(&doc.text, query) {
            let reference = Reference {
                version: 1,
                store: "builtin-docs".into(),
                kind: "docs".into(),
                id: doc.id.clone(),
                revision: hash(doc.text.as_bytes()),
                hash: hash(doc.text.as_bytes()),
                witness: None,
            };
            hits.push(Hit {
                reference,
                excerpt: excerpt(&doc.text),
                title: doc.title,
                kind: "guide".into(),
                location: json!({"document_id":doc.id}),
                provenance: json!({"parent_id":doc.parent_id,"source":"bundled_guide"}),
                scopes: vec!["docs".into()],
                thread_scopes: vec![],
                score,
                distance: None,
                space: None,
                method: "keyword".into(),
            });
        }
    }
    hits.sort_by(Hit::compare);
    hits
}

fn material_hits(s: &store::Store, source: &str, query: &str) -> Result<Vec<Hit>> {
    let mut q=s.db.prepare("SELECT b.ordinal,b.revision_id,b.text,b.location,r.kind,r.title,r.provenance,d.status,d.id FROM blocks b JOIN revisions r ON r.id=b.revision_id JOIN documents d ON d.active_revision=r.id WHERE d.source_id=?1 ORDER BY b.revision_id,b.ordinal")?;
    let mut rows = q.query([source])?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next()? {
        let text: String = row.get(2)?;
        if let Some(score) = keyword_score(&text, query) {
            let ordinal: i64 = row.get(0)?;
            let provenance: String = row.get(6)?;
            let content_hash = hash(text.as_bytes());
            hits.push(Hit{reference:Reference{version:1,store:s.uuid.clone(),kind:"material".into(),id:row.get(8)?,revision:row.get(1)?,hash:content_hash,witness:Some(json!({"block_ordinal":ordinal}))},excerpt:excerpt(&text),title:row.get(5)?,kind:row.get(4)?,location:json!({"source":serde_json::from_str::<Value>(&row.get::<_,String>(3)?).unwrap_or(Value::Null),"block_ordinal":ordinal,"block_offset_bytes":0,"block_end_bytes":text.len(),"coordinate":"extracted_utf8_bytes"}),provenance:json!({"source":serde_json::from_str::<Value>(&provenance).unwrap_or(Value::Null),"state":row.get::<_,String>(7)?,"document_id":row.get::<_,String>(8)?,"retrieval_unit":"block"}),scopes:vec![format!("source:{source}")],thread_scopes:vec![],score,distance:None,space:None,method:"keyword".into()});
            hits.sort_by(Hit::compare);
            hits.truncate(100);
        }
    }
    Ok(hits)
}

pub(super) async fn material_semantic(
    s: &store::Store,
    source: &str,
    query: &str,
    profile: &Profile,
) -> Result<Vec<Hit>> {
    let before = s.db.unchecked_transaction()?;
    let space: Option<(usize, String)> = before
        .query_row(
            "SELECT dimension,table_name FROM spaces WHERE id=?1",
            [&profile.id],
            |r| Ok((r.get::<_, i64>(0)? as usize, r.get(1)?)),
        )
        .optional()?;
    let Some((dimension, table)) = space else {
        return Err(err(
            "rag_semantic_profile_missing",
            "No semantic index for the configured embedding profile; run rag index --mode semantic",
        ));
    };
    let expected = format!("rag_vec_{}", &profile.id[..32]);
    if table != expected {
        return Err(err("schema_unsupported", "Invalid vector table identity"));
    }
    let generation: String =
        before.query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
            r.get(0)
        })?;
    let missing:i64=before.query_row("SELECT COUNT(*) FROM chunks c JOIN documents d ON d.active_revision=c.revision_id WHERE d.source_id=?1 AND NOT EXISTS(SELECT 1 FROM embeddings e WHERE e.hash=c.hash AND e.space_id=?2)",params![source,profile.id],|r|r.get(0))?;
    if missing > 0 {
        return Err(err(
            "rag_semantic_coverage_missing",
            "Some current documents lack embeddings for the configured profile; run rag index --mode semantic",
        ));
    }
    before.commit()?;
    let response = embed(profile, &[query.into()], Some(dimension)).await?;
    let vector = serde_json::to_string(&response.vectors[0]).expect("finite vector");
    let snapshot = s.db.unchecked_transaction()?;
    let current: String =
        snapshot.query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
            r.get(0)
        })?;
    if generation != current {
        return Err(err(
            "source_changed",
            "Document publication changed during query embedding; retry search",
        ));
    }
    // Exact L2 is supplied by sqlite-vec. Full saved blocks are retrieval units;
    // indexed chunk ranges remain explicit UTF-8 coordinates in extracted text.
    let mut q=snapshot.prepare(&format!("WITH positioned AS(SELECT revision_id,ordinal,text,location,COALESCE(SUM(length(CAST(text AS BLOB))+1) OVER(PARTITION BY revision_id ORDER BY ordinal ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING),0) AS start FROM blocks) SELECT b.ordinal,c.revision_id,b.text,b.location,r.kind,r.title,r.provenance,d.status,d.id,vec_distance_L2(v.embedding,?3) distance,c.start,c.end FROM chunks c JOIN documents d ON d.active_revision=c.revision_id JOIN revisions r ON r.id=c.revision_id JOIN positioned b ON b.revision_id=c.revision_id AND c.start>=b.start AND c.end<=b.start+length(CAST(b.text AS BLOB)) JOIN embeddings e ON e.hash=c.hash AND e.space_id=?2 JOIN {table} v ON v.rowid=e.vec_rowid WHERE d.source_id=?1 ORDER BY distance,c.id"))?;
    let mut rows = q.query(params![source, profile.id, vector])?;
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    while let Some(row) = rows.next()? {
        let revision: String = row.get(1)?;
        let ordinal: i64 = row.get(0)?;
        let unique = if source == "@docs" {
            revision.clone()
        } else {
            format!("{revision}:{ordinal}")
        };
        if !seen.insert(unique) {
            continue;
        }
        let text: String = row.get(2)?;
        let distance: f64 = row.get(9)?;
        hits.push(Hit{reference:Reference{version:1,store:s.uuid.clone(),kind:"material".into(),id:row.get(8)?,revision,hash:hash(text.as_bytes()),witness:Some(json!({"block_ordinal":ordinal}))},excerpt:excerpt(&text),title:row.get(5)?,kind:row.get(4)?,location:json!({"source":serde_json::from_str::<Value>(&row.get::<_,String>(3)?).unwrap_or(Value::Null),"block_ordinal":ordinal,"chunk_start_bytes":row.get::<_,i64>(10)?,"chunk_end_bytes":row.get::<_,i64>(11)?,"coordinate":"extracted_utf8_bytes"}),provenance:json!({"source":serde_json::from_str::<Value>(&row.get::<_,String>(6)?).unwrap_or(Value::Null),"state":row.get::<_,String>(7)?,"document_id":row.get::<_,String>(8)?,"retrieval_unit":"block"}),scopes:vec![format!("source:{source}")],thread_scopes:vec![],score:distance,distance:Some(distance),space:Some(profile.id.clone()),method:"semantic".into()});
        if hits.len() >= 100 {
            break;
        }
    }
    drop(rows);
    drop(q);
    snapshot.commit()?;
    Ok(hits)
}

/// Positive evidence of an absent/incomplete index, never an arbitrary provider failure.
#[derive(Clone)]
pub(super) struct Preparation {
    pub reason: &'static str,
    pub missing_documents: Option<i64>,
    pub missing_chunks: Option<i64>,
}

pub(super) fn material_preparation(
    s: &store::Store,
    source: &str,
    profile: &Profile,
) -> Result<Option<Preparation>> {
    let snapshot = s.db.unchecked_transaction()?;
    let space_exists: bool = snapshot.query_row(
        "SELECT EXISTS(SELECT 1 FROM spaces WHERE id=?1)",
        [&profile.id],
        |r| r.get(0),
    )?;
    let (missing_documents, missing_chunks): (i64, i64) = snapshot.query_row(
        "SELECT COUNT(DISTINCT c.revision_id),COUNT(*) FROM chunks c JOIN documents d ON d.active_revision=c.revision_id WHERE d.source_id=?1 AND NOT EXISTS(SELECT 1 FROM embeddings e WHERE e.hash=c.hash AND e.space_id=?2)",
        params![source, profile.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    snapshot.commit()?;
    Ok(
        (!space_exists || missing_chunks > 0).then_some(Preparation {
            reason: if space_exists {
                "embeddings_incomplete"
            } else {
                "profile_not_indexed"
            },
            missing_documents: Some(missing_documents),
            missing_chunks: Some(missing_chunks),
        }),
    )
}

fn command_prefix(context: &Context) -> Vec<String> {
    let mut command = vec!["shellboy".into()];
    if let Some(directory) = &context.data_dir {
        command.extend([
            "--data-dir".into(),
            directory.to_string_lossy().into_owned(),
        ]);
    }
    if let Some(config) = &context.config {
        command.extend(["--config".into(), config.to_string_lossy().into_owned()]);
    }
    command
}
fn history_scope(scope: &str) -> Option<Option<&str>> {
    if scope == "history" {
        Some(None)
    } else {
        scope.strip_prefix("history:").map(Some)
    }
}
fn preparation_next(
    context: &Context,
    scope: &str,
    source: Option<&str>,
    preparation: &Preparation,
) -> Value {
    let mut prefix = command_prefix(context);
    if history_scope(scope).is_some() {
        prefix.extend(["index".into(), "status".into()]);
        return json!({"source":"history", "steps":[{"command":prefix,"purpose":"Inspect the matching running history owner before explicitly selecting/backfilling an index"}],
            "note":"History indexing belongs to the conversation owner, not rag index; no indexing was started"});
    }
    prefix.extend([
        "rag".into(),
        "--db".into(),
        context.database.to_string_lossy().into_owned(),
    ]);
    let selected = if scope == "docs" {
        "docs"
    } else {
        source.expect("resolved source")
    };
    let mut steps = Vec::new();
    if preparation.reason == "store_not_initialized" {
        let mut init = prefix.clone();
        init.push("init".into());
        steps.push(
            json!({"command":init,"purpose":"Initialize the independent knowledge database"}),
        );
    }
    let mut index = prefix;
    index.extend([
        "index".into(),
        "--source".into(),
        selected.into(),
        "--mode".into(),
        "semantic".into(),
    ]);
    let mut preview = index.clone();
    preview.push("--dry-run".into());
    steps.push(
        json!({"command":preview,"purpose":"Preview semantic indexing without embedding requests"}),
    );
    steps.push(json!({"command":index,"purpose":"Explicitly prepare semantic indexing; embedding API charges may apply"}));
    json!({"source":selected,"steps":steps,"embedding_may_charge":true})
}
/// These codes are private to this module tree. Public scope errors retain
/// their established index_incompatible/db_not_found codes.
fn preparation_reason(code: &str) -> Option<(&'static str, &'static str)> {
    match code {
        "rag_semantic_store_missing" => Some(("store_not_initialized", "db_not_found")),
        "rag_semantic_guides_missing" => {
            Some(("guide_catalogue_not_indexed", "index_incompatible"))
        }
        "rag_semantic_profile_missing" => Some(("profile_not_indexed", "index_incompatible")),
        "rag_semantic_coverage_missing" => Some(("embeddings_incomplete", "index_incompatible")),
        "rag_semantic_history_missing" => {
            Some(("history_index_not_selected", "index_incompatible"))
        }
        _ => None,
    }
}
fn diagnose_preparation(
    context: &Context,
    scope: &str,
    source: Option<&str>,
    error: &Error,
) -> Option<Preparation> {
    let (reason, _) = preparation_reason(&error.code)?;
    // The failure site proves preparation was missing. Optional follow-up
    // counts are a separate short snapshot and may already have changed.
    let baseline = Preparation {
        reason,
        missing_documents: None,
        missing_chunks: None,
    };
    let observation = if scope == "docs" {
        super::builtin::preparation(context).ok().flatten()
    } else if history_scope(scope).is_some() {
        None
    } else {
        store::open(&context.database, false).ok().and_then(|s| {
            embedding_profile(context)
                .ok()
                .and_then(|profile| material_preparation(&s, source?, &profile).ok().flatten())
        })
    };
    Some(
        observation
            .filter(|observed| observed.reason == reason)
            .unwrap_or(baseline),
    )
}

// Scope selectors over the same history retriever are not independent votes.
// Keep one best-rank contribution per reference and method, while retaining
// every matching scope/membership projection. Each bounded entry has two slots.
fn fuse(lists: Vec<Vec<Hit>>) -> Vec<Hit> {
    let mut fused: BTreeMap<String, (Hit, [f64; 2])> = BTreeMap::new();
    for list in lists {
        let mut local = HashSet::new();
        for (rank, mut hit) in list.into_iter().take(100).enumerate() {
            let key = encode(&hit.reference);
            if !local.insert(key.clone()) {
                continue;
            }
            let method = match hit.method.as_str() {
                "keyword" => 0,
                "semantic" => 1,
                _ => unreachable!("individual retrieval lists contain one method"),
            };
            let score = 1.0 / (60.0 + rank as f64 + 1.0);
            if let Some((existing, scores)) = fused.get_mut(&key) {
                scores[method] = scores[method].max(score);
                existing.score = scores.iter().sum();
                for scope in hit.scopes {
                    if !existing.scopes.contains(&scope) {
                        existing.scopes.push(scope);
                    }
                }
                for membership in hit.thread_scopes {
                    if !existing
                        .thread_scopes
                        .iter()
                        .any(|item| item.thread_id == membership.thread_id)
                    {
                        existing.thread_scopes.push(membership);
                    }
                }
                if scores.iter().all(|score| *score > 0.0) {
                    existing.method = "hybrid".into();
                }
                if existing.distance.is_none() {
                    existing.distance = hit.distance;
                    existing.space = hit.space;
                }
            } else {
                let mut scores = [0.0; 2];
                scores[method] = score;
                hit.score = score;
                fused.insert(key, (hit, scores));
            }
        }
    }
    fused.into_values().map(|(hit, _)| hit).collect()
}

pub(super) async fn search(
    context: &Context,
    query: &str,
    scopes: &[String],
    mode: SearchMode,
    limit: u32,
) -> Result<Value> {
    if query.trim().is_empty()
        || query.len() > 16384
        || !(1..=100).contains(&limit)
        || scopes.len() > 200
    {
        return Err(err(
            "invalid_argument",
            "Provide query (1..=16384 bytes), limit 1..=100, and at most 200 scopes",
        ));
    }
    let mut scopes = scopes.to_vec();
    let mut discovery_error = None;
    if scopes.is_empty() {
        scopes.push("docs".into());
        if context.database.exists() {
            match store::open(&context.database, false) {
                Ok(s) => {
                    let mut q =
                        s.db.prepare("SELECT id FROM sources WHERE kind!='builtin' AND (kind!='derived' OR EXISTS(SELECT 1 FROM documents d WHERE d.source_id=sources.id AND d.active_revision IS NOT NULL)) ORDER BY id")?;
                    for id in q.query_map([], |r| r.get::<_, String>(0))? {
                        scopes.push(format!("source:{}", id?));
                    }
                }
                Err(error) => discovery_error = Some(error),
            }
        }
    }
    if scopes.len() > 201 {
        return Err(err("input_limit", "Expanded search exceeds 201 scopes"));
    }
    let mut seen = HashSet::new();
    scopes.retain(|s| seen.insert(s.clone()));
    let mut failures = Vec::new();
    if let Some(error) = discovery_error {
        failures
            .push(json!({"scope":"registered_sources","code":error.code,"message":error.message}));
    }
    let mut lists = Vec::new();
    let mut statuses = Vec::new();
    let mut warnings: BTreeMap<String, Value> = BTreeMap::new();
    let mut fatal = !failures.is_empty();
    let mut resolved_sources = HashSet::new();
    let mut breaker = ProviderBreaker::default();
    for scope in &scopes {
        let mut keyword = if mode == SearchMode::Semantic {
            "not_requested"
        } else {
            "not_used"
        };
        let mut semantic = if mode == SearchMode::Keyword {
            "not_requested"
        } else {
            "not_used"
        };
        let mut source_id = None;
        let mut duplicate = false;
        let result: Result<()> = async {
            if scope == "docs" {
                if mode != SearchMode::Semantic {
                    lists.push(docs_keyword(query));
                    keyword = "ok";
                }
                if mode != SearchMode::Keyword {
                    semantic = "error";
                    if breaker.paused() {
                        return Err(err(
                            "embedding_paused",
                            "Provider failure paused semantic search; no request was sent",
                        ));
                    }
                    lists.push(super::builtin::semantic(context, query).await?);
                    semantic = "ok";
                }
                return Ok(());
            }
            if let Some(thread) = history_scope(scope) {
                if thread.is_some_and(str::is_empty) {
                    return Err(err("scope_invalid", "history: requires a Thread ID"));
                }
                if mode != SearchMode::Semantic {
                    keyword = "error";
                    lists.push(history::keyword(&context.history, query, thread)?);
                    keyword = "ok";
                }
                if mode != SearchMode::Keyword {
                    semantic = "error";
                    if breaker.paused() {
                        return Err(err(
                            "embedding_paused",
                            "Provider failure paused semantic search; no request was sent",
                        ));
                    }
                    let (hits, automatic) = history::semantic(context, query, thread).await?;
                    lists.push(hits);
                    semantic = "ok";
                    if !automatic {
                        warnings.entry("auto_history_disabled".into()).or_insert_with(||json!({"code":"auto_history_disabled","message":"Automatic history indexing is off; newer sources may be missing. Semantic search covers retained indexed sources only; explicitly backfill or use keyword search"}));
                    }
                }
                return Ok(());
            }
            let source = scope.strip_prefix("source:").ok_or_else(|| {
                err(
                    "scope_invalid",
                    "Scope must be docs, history, history:THREAD, or source:ID",
                )
            })?;
            let s = store::open(&context.database, false)?;
            let id = store::resolve_source(&s.db, source)?;
            source_id = Some(id.clone());
            if !resolved_sources.insert(id.clone()) {
                duplicate = true;
                return Ok(());
            }
            if mode != SearchMode::Semantic {
                keyword = "error";
                lists.push(material_hits(&s, &id, query)?);
                keyword = "ok";
            }
            if mode != SearchMode::Keyword {
                semantic = "error";
                if breaker.paused() {
                    return Err(err(
                        "embedding_paused",
                        "Provider failure paused semantic search; no request was sent",
                    ));
                }
                let profile = embedding_profile(context)?;
                lists.push(material_semantic(&s, &id, query, &profile).await?);
                semantic = "ok";
            }
            Ok(())
        }
        .await;
        let mut status =
            json!({"scope":scope,"source":source_id,"keyword":keyword,"semantic":semantic});
        if history_scope(scope).is_some()
            && mode != SearchMode::Keyword
            && semantic == "ok"
            && warnings.contains_key("auto_history_disabled")
        {
            status["auto_history"] = json!(false);
            status["coverage"] = json!("indexed_sources_only");
            status["reason"] = json!("auto_history_disabled");
        }
        if duplicate {
            status["duplicate_of"] = json!(format!(
                "source:{}",
                source_id.as_deref().expect("resolved duplicate")
            ));
        }
        if let Err(error) = result {
            breaker.failure(&error.code);
            let wire_code =
                preparation_reason(&error.code).map_or(error.code.as_str(), |(_, code)| code);
            status["error_code"] = json!(wire_code);
            let preparation = diagnose_preparation(context, scope, source_id.as_deref(), &error);
            let degraded = mode == SearchMode::Hybrid && keyword == "ok" && preparation.is_some();
            let mut failure = json!({"scope":scope,"code":wire_code,"message":error.message});
            if let Some(preparation) = preparation {
                let next = preparation_next(context, scope, source_id.as_deref(), &preparation);
                status["semantic"] = json!("not_ready");
                status["reason"] = json!(preparation.reason);
                status["missing_documents"] = json!(preparation.missing_documents);
                status["missing_chunks"] = json!(preparation.missing_chunks);
                status["next"] = next.clone();
                failure["reason"] = json!(preparation.reason);
                failure["next"] = next;
                if degraded {
                    let warning = warnings.entry(preparation.reason.into()).or_insert_with(|| json!({
                        "code":"semantic_not_ready", "reason":preparation.reason,
                        "message":"Semantic index is not ready; keyword results are available. No document embedding or indexing was started.",
                        "scopes":[], "recovery":"See each scope_status entry for exact counts and source-specific next commands"}));
                    warning["scopes"]
                        .as_array_mut()
                        .expect("warning scopes")
                        .push(json!(scope));
                }
            }
            if !degraded {
                fatal = true;
                failures.push(failure);
            }
        } else if !duplicate && (mode == SearchMode::Keyword || semantic == "ok") {
            breaker.success();
        }
        statuses.push(status);
    }
    let mut hits = fuse(lists);
    hits.sort_by(Hit::compare);
    hits.truncate(limit as usize);
    let mut result = json!({"items":hits.iter().map(Hit::value).collect::<Vec<_>>(),"scopes":scopes,"mode":match mode{SearchMode::Keyword=>"keyword",SearchMode::Semantic=>"semantic",SearchMode::Hybrid=>"hybrid"},"rank_fusion":"rrf_k60","snapshot":"independent_sources_history_watermark_per_node_witness","scope_errors":failures,"scope_status":statuses,"degraded":!warnings.is_empty(),"warnings":warnings.into_values().collect::<Vec<_>>()});
    while serde_json::to_vec(&result).expect("result JSON").len() > 512 * 1024 {
        if result["items"]
            .as_array_mut()
            .expect("items")
            .pop()
            .is_none()
        {
            return Err(err("output_limit", "Scope errors exceed response budget"));
        }
        result["truncated"] = json!(true);
    }
    if !fatal {
        Ok(json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":true,"result":result}))
    } else {
        Ok(
            json!({"protocol_version":crate::protocol::PROTOCOL_VERSION,"ok":false,"result":result,"error":{"code":"partial_failure","message":"Some requested scopes failed; successful scope results are included"}}),
        )
    }
}

pub(super) fn get(
    context: &Context,
    token: &str,
    limit: u32,
    cursor: Option<&str>,
) -> Result<Value> {
    let reference: Reference = decode(token)?;
    if reference.version != 1 {
        return Err(err("revision_unavailable", "Unsupported reference version"));
    }
    let (text, provenance) = match reference.kind.as_str() {
        "docs" => {
            if reference.store != "builtin-docs" {
                return Err(err(
                    "revision_unavailable",
                    "Guide reference belongs to another store",
                ));
            }
            let doc = crate::docs::documents()
                .into_iter()
                .find(|d| d.id == reference.id)
                .ok_or_else(|| err("revision_unavailable", "Guide is unavailable"))?;
            if hash(doc.text.as_bytes()) != reference.hash || reference.revision != reference.hash {
                return Err(err(
                    "revision_unavailable",
                    "Guide revision changed with the installed binary",
                ));
            }
            (
                doc.text,
                json!({"title":doc.title,"parent_id":doc.parent_id}),
            )
        }
        "material" => {
            let s = store::open(&context.database, false)?;
            if s.uuid != reference.store {
                return Err(err(
                    "revision_unavailable",
                    "Reference belongs to another knowledge database",
                ));
            }
            let ordinal = reference
                .witness
                .as_ref()
                .and_then(|w| w.get("block_ordinal"))
                .and_then(Value::as_i64)
                .ok_or_else(|| err("revision_unavailable", "Missing saved block witness"))?;
            let row:Option<(String,String)>=s.db.query_row("SELECT b.text,r.provenance FROM blocks b JOIN revisions r ON r.id=b.revision_id WHERE r.document_id=?1 AND b.revision_id=?2 AND b.ordinal=?3",params![reference.id,reference.revision,ordinal],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let (text, provenance) = row.ok_or_else(|| {
                err(
                    "revision_unavailable",
                    "Saved source revision has been removed",
                )
            })?;
            let content_hash = hash(text.as_bytes());
            if content_hash != reference.hash || hash(text.as_bytes()) != reference.hash {
                return Err(err(
                    "revision_unavailable",
                    "Source content witness changed",
                ));
            }
            (
                text,
                serde_json::from_str(&provenance).unwrap_or(Value::Null),
            )
        }
        "history" => history::get(&context.history, &reference)?,
        _ => return Err(err("revision_unavailable", "Unknown reference kind")),
    };
    let mut value = page(&text, token, limit, cursor)?;
    value["reference"] = json!(token);
    value["provenance"] = provenance;
    Ok(value)
}

#[cfg(test)]
mod preparation_tests {
    use super::*;

    #[test]
    fn generic_incompatibility_is_never_reclassified_by_a_later_missing_store() {
        let directory = tempfile::tempdir().unwrap();
        let context = Context {
            database: directory.path().join("missing-rag.db"),
            history: directory.path().join("missing-history.db"),
            config: None,
            data_dir: Some(directory.path().join("data")),
            cwd: directory.path().into(),
        };
        for message in [
            "Embedding dimension exceeds sqlite-vec limit 8192",
            "History index endpoint/model identity is unverified",
        ] {
            let failure = err("index_incompatible", message);
            assert!(diagnose_preparation(&context, "docs", None, &failure).is_none());
            assert!(diagnose_preparation(&context, "history", None, &failure).is_none());
            assert!(diagnose_preparation(&context, "source:any", Some("any"), &failure).is_none());
        }
        let original = err("rag_semantic_coverage_missing", "Coverage was incomplete");
        let preparation = diagnose_preparation(&context, "source:any", Some("any"), &original)
            .expect("Original failure-site evidence survives later unavailable observations");
        assert_eq!(preparation.reason, "embeddings_incomplete");
        assert!(preparation.missing_documents.is_none());
        assert!(preparation.missing_chunks.is_none());
        assert_eq!(
            preparation_reason(&original.code).unwrap().1,
            "index_incompatible"
        );
    }
}

#[cfg(test)]
mod fusion_tests {
    use super::*;
    fn hit(id: &str, method: &str, scope: &str, thread: Option<&str>) -> Hit {
        Hit {
            reference: Reference {
                version: 1,
                store: "history-fixture".into(),
                kind: "history".into(),
                id: id.into(),
                revision: "0".into(),
                hash: "fixture".into(),
                witness: None,
            },
            title: id.into(),
            excerpt: id.into(),
            kind: "user".into(),
            location: Value::Null,
            provenance: Value::Null,
            scopes: vec![scope.into()],
            thread_scopes: thread
                .map(|thread| crate::index::SearchThreadScope {
                    thread_id: thread.into(),
                    on_current_path: true,
                })
                .into_iter()
                .collect(),
            score: 0.0,
            distance: (method == "semantic").then_some(0.1),
            space: (method == "semantic").then(|| "fixture".into()),
            method: method.into(),
        }
    }
    #[test]
    fn overlap_votes_once_at_best_rank_but_independent_methods_still_fuse() {
        let hits = fuse(vec![
            vec![
                hit("other", "keyword", "history", None),
                hit("shared", "keyword", "history", None),
            ],
            vec![hit("shared", "keyword", "history:one", Some("one"))],
            vec![hit("shared", "keyword", "history:fork", Some("fork"))],
            vec![hit("shared", "semantic", "history:one", Some("one"))],
        ]);
        let shared = hits
            .iter()
            .find(|hit| hit.reference.id == "shared")
            .unwrap();
        assert!((shared.score - 2.0 / 61.0).abs() < f64::EPSILON);
        assert_eq!(shared.method, "hybrid");
        assert_eq!(shared.scopes, ["history", "history:one", "history:fork"]);
        assert_eq!(shared.thread_scopes.len(), 2);
        assert_eq!(shared.distance, Some(0.1));
    }
}
