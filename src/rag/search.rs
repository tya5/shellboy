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
        .embeddings(&profile.model, texts, dimension, CancellationToken::new())
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
        json!({"reference":encode(&self.reference),"revision":self.reference.revision,"hash":self.reference.hash,"title":self.title,"excerpt":self.excerpt,"kind":self.kind,"location":self.location,"provenance":self.provenance,"scopes":self.scopes,"score":self.score,"distance":self.distance,"embedding_space":self.space,"method":self.method})
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
            hits.push(Hit{reference:Reference{version:1,store:s.uuid.clone(),kind:"material".into(),id:row.get(8)?,revision:row.get(1)?,hash:content_hash,witness:Some(json!({"block_ordinal":ordinal}))},excerpt:excerpt(&text),title:row.get(5)?,kind:row.get(4)?,location:json!({"source":serde_json::from_str::<Value>(&row.get::<_,String>(3)?).unwrap_or(Value::Null),"block_ordinal":ordinal,"block_offset_bytes":0,"block_end_bytes":text.len(),"coordinate":"extracted_utf8_bytes"}),provenance:json!({"source":serde_json::from_str::<Value>(&provenance).unwrap_or(Value::Null),"state":row.get::<_,String>(7)?,"document_id":row.get::<_,String>(8)?,"retrieval_unit":"block"}),scopes:vec![format!("source:{source}")],score,distance:None,space:None,method:"keyword".into()});
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
            "index_incompatible",
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
            "index_incompatible",
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
        hits.push(Hit{reference:Reference{version:1,store:s.uuid.clone(),kind:"material".into(),id:row.get(8)?,revision,hash:hash(text.as_bytes()),witness:Some(json!({"block_ordinal":ordinal}))},excerpt:excerpt(&text),title:row.get(5)?,kind:row.get(4)?,location:json!({"source":serde_json::from_str::<Value>(&row.get::<_,String>(3)?).unwrap_or(Value::Null),"block_ordinal":ordinal,"chunk_start_bytes":row.get::<_,i64>(10)?,"chunk_end_bytes":row.get::<_,i64>(11)?,"coordinate":"extracted_utf8_bytes"}),provenance:json!({"source":serde_json::from_str::<Value>(&row.get::<_,String>(6)?).unwrap_or(Value::Null),"state":row.get::<_,String>(7)?,"document_id":row.get::<_,String>(8)?,"retrieval_unit":"block"}),scopes:vec![format!("source:{source}")],score:distance,distance:Some(distance),space:Some(profile.id.clone()),method:"semantic".into()});
        if hits.len() >= 100 {
            break;
        }
    }
    drop(rows);
    drop(q);
    snapshot.commit()?;
    Ok(hits)
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
    let mut resolved_sources = HashSet::new();
    let mut breaker = ProviderBreaker::default();
    for scope in &scopes {
        if breaker.paused() {
            failures.push(json!({"scope":scope,"code":"embedding_paused","message":"Provider failure paused later scopes; no request was sent"}));
            continue;
        }
        let result: Result<()> = async {
            if scope == "docs" {
                if mode != SearchMode::Semantic {
                    lists.push(docs_keyword(query));
                }
                if mode != SearchMode::Keyword {
                    lists.push(super::builtin::semantic(context, query).await?);
                }
                return Ok(());
            }
            if scope == "history" {
                if mode != SearchMode::Semantic {
                    lists.push(history::keyword(&context.history, query)?);
                }
                if mode != SearchMode::Keyword {
                    lists.push(history::semantic(context, query).await?);
                }
                return Ok(());
            }
            let source = scope
                .strip_prefix("source:")
                .ok_or_else(|| err("scope_invalid", "Scope must be docs, history, or source:ID"))?;
            let s = store::open(&context.database, false)?;
            let id = store::resolve_source(&s.db, source)?;
            if !resolved_sources.insert(id.clone()) {
                return Ok(());
            }
            if mode != SearchMode::Semantic {
                lists.push(material_hits(&s, &id, query)?);
            }
            if mode != SearchMode::Keyword {
                let profile = embedding_profile(context)?;
                lists.push(material_semantic(&s, &id, query, &profile).await?);
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            breaker.failure(&error.code);
            failures.push(json!({"scope":scope,"code":error.code,"message":error.message}));
        } else {
            breaker.success();
        }
    }
    let mut fused: BTreeMap<String, Hit> = BTreeMap::new();
    for list in lists {
        let mut local = HashSet::new();
        for (rank, mut hit) in list.into_iter().take(100).enumerate() {
            let key = encode(&hit.reference);
            if !local.insert(key.clone()) {
                continue;
            }
            let score = 1.0 / (60.0 + rank as f64 + 1.0);
            if let Some(existing) = fused.get_mut(&key) {
                existing.score += score;
                for scope in hit.scopes {
                    if !existing.scopes.contains(&scope) {
                        existing.scopes.push(scope);
                    }
                }
                if existing.method != hit.method {
                    existing.method = "hybrid".into();
                }
                if existing.distance.is_none() {
                    existing.distance = hit.distance;
                    existing.space = hit.space;
                }
            } else {
                hit.score = score;
                fused.insert(key, hit);
            }
        }
    }
    let mut hits = fused.into_values().collect::<Vec<_>>();
    hits.sort_by(Hit::compare);
    hits.truncate(limit as usize);
    let mut result = json!({"items":hits.iter().map(Hit::value).collect::<Vec<_>>(),"scopes":scopes,"mode":match mode{SearchMode::Keyword=>"keyword",SearchMode::Semantic=>"semantic",SearchMode::Hybrid=>"hybrid"},"rank_fusion":"rrf_k60","snapshot":"independent_sources_history_watermark_per_node_witness","scope_errors":failures});
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
    if failures.is_empty() {
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
