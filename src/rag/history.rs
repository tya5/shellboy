//! Explicit read-only history adapter, identical for running and stopped owners.
use super::*;
mod raw;
use raw::raw_text;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::time::Duration;

fn open_connection(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        return Err(err(
            "history_unavailable",
            "Conversation database does not exist",
        ));
    }
    crate::storage::register_vec()
        .map_err(|_| err("history_unavailable", "Cannot initialize sqlite-vec"))?;
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(2))?;
    #[cfg(test)]
    if BENCH_STEPS_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        db.progress_handler(
            1000,
            Some(|| {
                BENCH_STEPS.fetch_add(1000, std::sync::atomic::Ordering::Relaxed);
                false
            }),
        )?;
    }
    db.execute_batch("PRAGMA query_only=ON;BEGIN;")?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != crate::storage::SCHEMA_VERSION {
        return Err(err(
            "schema_unsupported",
            "History schema is not the supported version; open the database with the writer to migrate before read-only access",
        ));
    }
    let ledger = {
        let mut q = db.prepare("SELECT version,name FROM schema_migration ORDER BY version")?;
        q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if ledger
        .iter()
        .map(|(version, name)| (*version, name.as_str()))
        .ne(crate::storage::MIGRATION_NAMES.iter().copied())
    {
        return Err(err(
            "schema_unsupported",
            "History migration ledger differs; no migration was attempted",
        ));
    }
    Ok(db)
}
/// A finite read-only reader owns the filesystem identity once; individual
/// hits cannot stat the path again and accidentally acquire another store ID.
struct HistoryDb {
    connection: Connection,
    store_id: String,
}
fn open(path: &Path) -> Result<HistoryDb> {
    let connection = open_connection(path)?;
    Ok(HistoryDb {
        connection,
        store_id: identity(path)?,
    })
}
struct HistoryBody {
    kind: String,
    text: String,
    raw: Option<Value>,
}
fn identity(path: &Path) -> Result<String> {
    let canonical = std::fs::canonicalize(path)?;
    let metadata = std::fs::metadata(&canonical)?;
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let file_id = format!("{:?}", metadata.created().ok());
    Ok(format!(
        "history-{}",
        hash(format!("{}\0{file_id}", canonical.to_string_lossy()).as_bytes())
    ))
}
fn node(db: &Connection, id: &str) -> Result<(String, i64, Value, Option<String>)> {
    let row = crate::history::NodeRow::load(db, id)?;
    let crate::history::NodeRow {
        kind,
        revision,
        payload: Some(payload),
        body_hash,
        deleted_reason: None,
    } = row.ok_or_else(|| err("revision_unavailable", "History node no longer exists"))?
    else {
        return Err(err(
            "revision_unavailable",
            "History payload was collected; metadata may remain available through history get",
        ));
    };
    if payload.len() > MAX_INPUT {
        return Err(err(
            "input_limit",
            "History payload exceeds bounded retrieval limit",
        ));
    }
    let payload = serde_json::from_str(&payload)
        .map_err(|_| err("history_unavailable", "History payload is malformed"))?;
    Ok((kind, revision, payload, body_hash))
}
fn provenance(db: &Connection, id: &str) -> Result<Value> {
    let native = crate::history::provenance(db, id)
        .map_err(|_| err("history_unavailable", "History provenance is unavailable"))?;
    let paths = {
        let mut q=db.prepare("SELECT p.thread_id FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.node_id=?1 AND t.deleted_at IS NULL ORDER BY p.thread_id LIMIT 201")?;
        q.query_map([id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    Ok(
        json!({"node":native,"current_thread_paths":paths.iter().take(200).collect::<Vec<_>>(),"current_thread_paths_truncated":paths.len()>200,"membership":"current_selected_path_only"}),
    )
}
fn make_hit(
    reader: &HistoryDb,
    id: &str,
    body: HistoryBody,
    ranking: (f64, Option<f64>, Option<String>),
    thread: Option<&str>,
) -> Result<search::Hit> {
    let db = &reader.connection;
    let (score, distance, space) = ranking;
    let (kind, revision, _, payload_hash) = node(db, id)?;
    let provenance = provenance(db, id)?;
    let reference = Reference {
        version: 1,
        store: reader.store_id.clone(),
        kind: "history".into(),
        id: id.into(),
        revision: revision.to_string(),
        hash: hash(body.text.as_bytes()),
        witness: Some(json!({"body_kind":body.kind,"payload_hash":payload_hash,"raw":body.raw})),
    };
    Ok(search::Hit {
        reference,
        excerpt: body.text.chars().take(500).collect(),
        title: kind.clone(),
        kind,
        location: json!({"node_id":id,"body_kind":body.kind,"coordinate":"extracted_unicode_characters"}),
        provenance,
        scopes: vec![thread.map_or_else(|| "history".into(), |id| format!("history:{id}"))],
        thread_scopes: thread
            .map(|thread| {
                crate::history::search_scope::on_current_path(db, thread, id).map(
                    |on_current_path| crate::index::SearchThreadScope {
                        thread_id: thread.into(),
                        on_current_path,
                    },
                )
            })
            .transpose()?
            .into_iter()
            .collect(),
        score,
        distance,
        space,
        method: if distance.is_some() {
            "semantic"
        } else {
            "keyword"
        }
        .into(),
    })
}
fn bodies(db: &Connection, id: &str, kind: &str, payload: &Value) -> Result<Vec<HistoryBody>> {
    let mut result = crate::history::search_bodies(kind, payload)
        .into_iter()
        .map(|(kind, text)| HistoryBody {
            kind,
            text,
            raw: None,
        })
        .collect::<Vec<_>>();
    let paths = {
        let mut q=db.prepare("SELECT absolute_path FROM attachment_ref WHERE source_node_id=?1 ORDER BY created_at,id")?;
        q.query_map([id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if !paths.is_empty() {
        result.push(HistoryBody {
            kind: "attachment_paths".into(),
            text: paths.join("\n"),
            raw: None,
        });
    }
    // Raw is streamed by keyword and separately witness-checked by semantic;
    // canonical body enumeration never decodes an unrequested full raw body.
    Ok(result)
}
fn raw_witness(db: &Connection, id: &str) -> Result<Option<Value>> {
    Ok(crate::history::RawWitness::for_node(db, id, true)?
        .map(|(raw, _)| json!({"execution":raw.execution,"generation":raw.generation,"saved_sequence":raw.saved_sequence,"encoding":raw.encoding})))
}
fn validate_thread(db: &Connection, thread: Option<&str>) -> Result<()> {
    if let Some(thread) = thread
        && !crate::history::search_scope::exists(db, thread)?
    {
        return Err(err("thread_not_found", "Search Thread does not exist"));
    }
    Ok(())
}
const KEYWORD_NODE_LIMIT: usize = 2_000;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeywordCursor {
    version: u32,
    store: String,
    query_hash: String,
    thread_hash: Option<String>,
    bound: i64,
    inclusive: bool,
    skip: usize,
    node_hash: Option<String>,
    signature: Option<String>,
}
pub(super) struct KeywordPage {
    pub hits: Vec<search::Hit>,
    pub next: Option<String>,
    pub restart: String,
}
pub(super) fn keyword(
    path: &Path,
    query: &str,
    thread: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<KeywordPage> {
    keyword_with_budget(path, query, thread, limit, cursor, KEYWORD_NODE_LIMIT)
}
fn keyword_with_budget(
    path: &Path,
    query: &str,
    thread: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
    node_budget: usize,
) -> Result<KeywordPage> {
    if query.trim().is_empty() {
        return Err(err("invalid_argument", "Provide a nonempty keyword query"));
    }
    // Reuse one finite reader and fetch only 32 IDs at a time. Release each
    // node snapshot; source witnesses, not one whole-history transaction, bind hits.
    let db = open(path)?;
    validate_thread(&db.connection, thread)?;
    let mut scanned = 0usize;
    let watermark =
        db.connection
            .query_row("SELECT COALESCE(MAX(rowid),0) FROM node", [], |r| {
                r.get::<_, i64>(0)
            })?;
    let query_hash = hash(query.as_bytes());
    let make_cursor = |bound, inclusive, skip, node, signature| {
        encode(&KeywordCursor {
            version: 1,
            store: db.store_id.clone(),
            query_hash: query_hash.clone(),
            thread_hash: thread.map(|id| hash(id.as_bytes())),
            bound,
            inclusive,
            skip,
            node_hash: node,
            signature,
        })
    };
    let restart = make_cursor(watermark, true, 0, None, None);
    let position = if let Some(token) = cursor {
        let c: KeywordCursor =
            decode(token).map_err(|_| err("cursor_expired", "Invalid history search cursor"))?;
        if c.version != 1
            || c.store != db.store_id
            || c.query_hash != query_hash
            || c.thread_hash != thread.map(|id| hash(id.as_bytes()))
            || (c.skip == 0) != (c.node_hash.is_none() && c.signature.is_none())
            || (c.skip > 0 && (!c.inclusive || c.node_hash.is_none() || c.signature.is_none()))
        {
            return Err(err(
                "cursor_expired",
                "History cursor does not match this search",
            ));
        }
        Some(c)
    } else {
        None
    };
    let mut after = Some(
        position
            .as_ref()
            .map_or((watermark, true), |c| (c.bound, c.inclusive)),
    );
    let mut resume = position.filter(|c| c.skip > 0);
    let mut hits = Vec::new();
    let mut hit_bytes = 0usize;
    while let Some((bound, inclusive)) = after {
        let rows = {
            // Collect only IDs here. Membership is checked using indexed reverse
            // ancestry in each node snapshot, before loading/scoring its body.
            let sql = if inclusive {
                "SELECT rowid,id FROM node WHERE rowid<=?1 ORDER BY rowid DESC LIMIT 32"
            } else {
                "SELECT rowid,id FROM node WHERE rowid<?1 ORDER BY rowid DESC LIMIT 32"
            };
            let mut q = db.connection.prepare_cached(sql)?;
            q.query_map([bound], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        if rows.is_empty() {
            if resume.is_some() {
                return Err(err(
                    "cursor_expired",
                    "History cursor node no longer exists",
                ));
            }
            break;
        }
        for (rowid, id) in rows {
            if scanned == node_budget {
                let (last, inclusive) =
                    after.expect("a scanned node supplies the continuation position");
                return Ok(KeywordPage {
                    hits,
                    next: Some(make_cursor(last, inclusive, 0, None, None)),
                    restart,
                });
            }
            if resume.as_ref().is_some_and(|c| {
                c.bound != rowid || c.node_hash.as_deref() != Some(hash(id.as_bytes()).as_str())
            }) {
                return Err(err(
                    "cursor_expired",
                    "History cursor node no longer exists",
                ));
            }
            scanned += 1;
            after = Some((rowid, false));
            // A previous node releases its snapshot. Do not return a candidate
            // from an obsolete Thread path/replacement set in this new snapshot.
            validate_thread(&db.connection, thread)?;
            if let Some(thread) = thread
                && !crate::history::search_scope::contains(&db.connection, thread, &id)?
            {
                if resume.is_some() {
                    return Err(err(
                        "cursor_expired",
                        "History cursor node is no longer visible",
                    ));
                }
                db.connection.execute_batch("ROLLBACK;BEGIN;")?;
                continue;
            }
            // Scoped membership already reaches a live Thread path in this
            // snapshot, which is a visibility root. Global reads still need
            // the general deleted-owner test rather than a duplicate walk.
            if thread.is_none() && !crate::history::reachability::searchable(&db.connection, &id)? {
                if resume.is_some() {
                    return Err(err(
                        "cursor_expired",
                        "History cursor node is no longer visible",
                    ));
                }
                db.connection.execute_batch("ROLLBACK;BEGIN;")?;
                continue;
            }
            let (kind, _, payload, _) = match node(&db.connection, &id) {
                Ok(node) => node,
                Err(error) if error.code == "revision_unavailable" => {
                    if resume.is_some() {
                        return Err(err(
                            "cursor_expired",
                            "History cursor node is no longer available",
                        ));
                    }
                    db.connection.execute_batch("ROLLBACK;BEGIN;")?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut node_hits = Vec::new();
            for body in bodies(&db.connection, &id, &kind, &payload)? {
                if let Some(score) = search::keyword_score(&body.text, query) {
                    node_hits.push(make_hit(&db, &id, body, (score, None, None), thread)?);
                }
            }
            if let Some(witness) = raw_witness(&db.connection, &id)? {
                match raw::keyword(&db.connection, &witness, query) {
                    Ok(Some(found)) => {
                        let range = found.witness["range"].clone();
                        let mut hit = make_hit(
                            &db,
                            &id,
                            HistoryBody {
                                kind: "tool_raw".into(),
                                text: found.body,
                                raw: Some(found.witness),
                            },
                            (found.score, None, None),
                            thread,
                        )?;
                        if !range.is_null() {
                            hit.location["range"] = range;
                        }
                        node_hits.push(hit);
                    }
                    Ok(None) => {}
                    Err(error) if error.code == "revision_unavailable" => {}
                    Err(error) => return Err(error),
                }
            }
            let signature = hash(
                serde_json::to_string(&node_hits.iter().map(|h| &h.reference).collect::<Vec<_>>())
                    .expect("references serialize")
                    .as_bytes(),
            );
            let skip = if let Some(previous) = resume.take() {
                if previous.signature.as_deref() != Some(signature.as_str())
                    || previous.skip > node_hits.len()
                {
                    return Err(err("cursor_expired", "History cursor node changed"));
                }
                previous.skip
            } else {
                0
            };
            let remaining = node_hits.len() - skip;
            let mut take = 0usize;
            for hit in node_hits.into_iter().skip(skip) {
                if hits.len() == limit {
                    break;
                }
                let size = serde_json::to_vec(&hit.value())
                    .expect("hit serializes")
                    .len()
                    + 1;
                if hit_bytes + size > 128 * 1024 {
                    if hits.is_empty() {
                        return Err(err(
                            "output_limit",
                            "A history result exceeds the page budget",
                        ));
                    }
                    break;
                }
                hit_bytes += size;
                hits.push(hit);
                take += 1;
            }
            let next = if take < remaining && skip + take == 0 {
                Some(make_cursor(rowid, true, 0, None, None))
            } else if take < remaining {
                Some(make_cursor(
                    rowid,
                    true,
                    skip + take,
                    Some(hash(id.as_bytes())),
                    Some(signature),
                ))
            } else if hits.len() == limit {
                after.map(|(bound, inclusive)| make_cursor(bound, inclusive, 0, None, None))
            } else {
                None
            };
            db.connection.execute_batch("ROLLBACK;BEGIN;")?;
            if next.is_some() {
                return Ok(KeywordPage {
                    hits,
                    next,
                    restart,
                });
            }
        }
    }
    Ok(KeywordPage {
        hits,
        next: None,
        restart,
    })
}
pub(super) async fn semantic(
    context: &search::Context,
    query: &str,
    thread: Option<&str>,
) -> Result<(Vec<search::Hit>, bool)> {
    let descriptor = {
        let db = open_connection(&context.history)?;
        validate_thread(&db, thread)?;
        crate::index::rag_space(&db)
            .map_err(|error| {
                if error.code == crate::protocol::ErrorCode::InvalidState {
                    err("index_incompatible", "History index query identity is unavailable; explicitly rebuild with the intended embedding configuration")
                } else {
                    err("history_unavailable", "Cannot inspect history embedding space")
                }
            })?
            .ok_or_else(|| {
                err(
                    "rag_semantic_history_missing",
                    "History has no active semantic index",
                )
            })?
    };
    let loaded = crate::config::Config::load_with_overrides(
        context.config.as_deref(),
        context.data_dir.as_deref(),
        &context.cwd,
        &crate::config::PathEnvironment::current(),
    )
    .map_err(|_| {
        err(
            "config_invalid",
            "Cannot load embedding connection configuration",
        )
    })?;
    let name = descriptor.profile.as_str();
    let model = descriptor.model.as_str();
    let connection = loaded.config.connections.get(name).ok_or_else(|| {
        err(
            "config_invalid",
            "History embedding connection is not configured",
        )
    })?;
    let key = crate::index::rag_profile_key(&loaded.config, &descriptor)
        .ok_or_else(||err("index_incompatible","History index endpoint/model identity is unverified; explicitly rebuild with the intended embedding configuration"))?;
    let profile = search::Profile {
        id: key,
        name: name.into(),
        model: model.into(),
        requested_dimensions: descriptor.requested_dimensions,
        endpoint_hash: hash(connection.base_url.as_bytes()),
        client: crate::model::http::HttpModelClient::for_connection(name, connection.clone())
            .map_err(|_| {
                err(
                    "config_invalid",
                    "Cannot initialize history embedding connection",
                )
            })?,
    };
    let dimension = descriptor.dimension;
    let response = search::embed(&profile, &[query.into()], Some(dimension)).await?;
    let db = open(&context.history)?;
    // Embedding releases the old snapshot. Deletion is a permanent scope
    // failure, not an index witness change for which retry would help.
    validate_thread(&db.connection, thread)?;
    let matches = crate::index::rag_hits(
        &db.connection,
        &descriptor,
        &response.vectors[0],
        100,
        thread,
    )
    .map_err(|error| {
        if error.code == crate::protocol::ErrorCode::ThreadNotFound {
            err("thread_not_found", "Search Thread does not exist")
        } else {
            err(
                "history_changed",
                "History index changed or its witnesses expired; retry search",
            )
        }
    })?;
    let mut hits = Vec::new();
    let mut raw_spans = Vec::new();
    for found in &matches {
        for span in &found.spans {
            if span.body_kind != "tool_raw" {
                continue;
            }
            let witness:Option<String>=db.connection.query_row("SELECT authorized_watermark FROM embedding_job WHERE source_node_id=?1 AND space_id=?2 AND body_kind='tool_raw' AND source_revision=?3 AND span_start=?4 AND span_end=?5 AND source_hash=?6 AND state='done' LIMIT 1",params![found.node_id,&descriptor.id,span.source_revision,span.start as i64,span.end as i64,span.content_hash],|r|r.get(0)).optional()?.flatten();
            let raw = witness
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.get("witness").and_then(|v| v.get("raw")).cloned())
                .filter(|v| !v.is_null())
                .ok_or_else(|| err("revision_unavailable", "Raw-output witness is unavailable"))?;
            raw_spans.push((raw, span));
        }
    }
    // KNN may select many spans from the same execution. Decode each saved
    // witness once, without changing ranked order or source verification.
    let mut raw_bodies = raw::semantic_batch(&db.connection, &raw_spans)?.into_iter();
    for found in matches {
        let (kind, _, payload, _) = node(&db.connection, &found.node_id)?;
        for span in found.spans {
            let (body, raw) = if span.body_kind == "tool_raw" {
                let (body, raw) = raw_bodies.next().expect("one result per raw span");
                (body, Some(raw))
            } else {
                let body = bodies(&db.connection, &found.node_id, &kind, &payload)?
                    .into_iter()
                    .find(|body| body.kind == span.body_kind)
                    .map(|body| body.text)
                    .ok_or_else(|| {
                        err(
                            "revision_unavailable",
                            "Indexed history body is unavailable",
                        )
                    })?;
                (body, None)
            };
            if raw.is_none() {
                let selected: String = body
                    .chars()
                    .skip(span.start)
                    .take(span.end.saturating_sub(span.start))
                    .collect();
                if span.start > span.end || hash(selected.as_bytes()) != span.content_hash {
                    return Err(err("revision_unavailable", "Indexed source range changed"));
                }
            }
            let range = raw.as_ref().and_then(|raw| raw.get("range")).cloned();
            let mut hit = make_hit(
                &db,
                &found.node_id,
                HistoryBody {
                    kind: span.body_kind,
                    text: body,
                    raw,
                },
                (
                    found.distance,
                    Some(found.distance),
                    Some(descriptor.id.clone()),
                ),
                thread,
            )?;
            if let Some(range) = range {
                hit.location["range"] = range;
                hit.location["indexed_range"] = json!({"start":span.start,"end":span.end});
            }
            hits.push(hit);
        }
    }
    Ok((hits, loaded.config.indexing.auto_history))
}
pub(super) fn get(path: &Path, reference: &Reference) -> Result<(String, Value)> {
    let store_id = identity(path)?;
    if store_id != reference.store {
        return Err(err(
            "revision_unavailable",
            "History reference belongs to another database",
        ));
    }
    let reader = HistoryDb {
        connection: open_connection(path)?,
        store_id,
    };
    let db = &reader.connection;
    let (kind, revision, payload, payload_hash) = node(db, &reference.id)?;
    let witness = reference
        .witness
        .as_ref()
        .ok_or_else(|| err("revision_unavailable", "Missing history source witness"))?;
    if revision.to_string() != reference.revision || witness["payload_hash"] != json!(payload_hash)
    {
        return Err(err(
            "revision_unavailable",
            "History payload revision changed",
        ));
    }
    let body_kind = witness["body_kind"]
        .as_str()
        .ok_or_else(|| err("revision_unavailable", "Invalid history body kind"))?;
    let text = if body_kind == "tool_raw" {
        raw_text(db, &witness["raw"])?
    } else {
        bodies(db, &reference.id, &kind, &payload)?
            .into_iter()
            .find(|body| body.kind == body_kind)
            .map(|body| body.text)
            .ok_or_else(|| err("revision_unavailable", "History body is unavailable"))?
    };
    if hash(text.as_bytes()) != reference.hash {
        return Err(err("revision_unavailable", "History text content changed"));
    }
    Ok((text, provenance(db, &reference.id)?))
}

#[cfg(test)]
static BENCH_STEPS_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static BENCH_STEPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod measurement {
    use super::*;
    use std::sync::atomic::Ordering;
    #[tokio::test]
    #[ignore = "Synthetic per-call history keyword VM steps and timing"]
    async fn history_keyword_work_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellboy.db");
        let db = crate::storage::DbWorker::open(path.clone()).await.unwrap();
        db.call(|c| {
            c.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('bench','/','/agents','p','m','now')", [])?;
            Ok(())
        }).await.unwrap();
        for total in [250_i64, 500, 1000, 2000, 5000, 10000] {
            db.call(move |c| {
                c.execute_batch("DELETE FROM thread_path; DELETE FROM node;")?;
                let tx = c.transaction()?;
                {
                    let mut q = tx.prepare("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES(?1,'user','now',?2,100)")?;
                    let mut path = tx.prepare("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('bench',?1,?2)")?;
                    for n in 0..total {
                        let id = format!("synthetic-{n}");
                        q.execute(params![id, r#"{"role":"user","content":[{"type":"input_text","text":"ordinary synthetic Japanese 文書"}]}"#])?;
                        path.execute(params![n, id])?;
                    }
                }
                tx.commit()?;
                Ok(())
            }).await.unwrap();
            for thread in [None, Some("bench")] {
                BENCH_STEPS.store(0, Ordering::SeqCst);
                BENCH_STEPS_ENABLED.store(true, Ordering::SeqCst);
                let start = std::time::Instant::now();
                let result =
                    keyword_with_budget(&path, "absent", thread, 100, None, total as usize)
                        .unwrap();
                let elapsed = start.elapsed();
                BENCH_STEPS_ENABLED.store(false, Ordering::SeqCst);
                assert!(result.hits.is_empty());
                println!(
                    "RAG_BUDGET rows={total} scope={} vm_steps_approx={} elapsed_ms={:.2}",
                    if thread.is_some() { "thread" } else { "global" },
                    BENCH_STEPS.load(Ordering::SeqCst),
                    elapsed.as_secs_f64() * 1000.0
                );
            }
            db.call(|c| {
                c.execute("DELETE FROM thread_path", [])?;
                Ok(())
            })
            .await
            .unwrap();
            BENCH_STEPS.store(0, Ordering::SeqCst);
            BENCH_STEPS_ENABLED.store(true, Ordering::SeqCst);
            let start = std::time::Instant::now();
            let result =
                keyword_with_budget(&path, "absent", None, 100, None, total as usize).unwrap();
            let elapsed = start.elapsed();
            BENCH_STEPS_ENABLED.store(false, Ordering::SeqCst);
            assert!(result.hits.is_empty());
            println!(
                "RAG_BUDGET rows={total} scope=unlinked vm_steps_approx={} elapsed_ms={:.2}",
                BENCH_STEPS.load(Ordering::SeqCst),
                elapsed.as_secs_f64() * 1000.0
            );
        }
        db.shutdown().await.unwrap();
    }
}
