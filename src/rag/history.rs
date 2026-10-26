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
    db.execute_batch("PRAGMA query_only=ON;BEGIN;")?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != crate::storage::SCHEMA_VERSION {
        return Err(err(
            "schema_unsupported",
            "History schema is not the supported version; no migration was attempted",
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
        scopes: vec!["history".into()],
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
pub(super) fn keyword(path: &Path, query: &str) -> Result<Vec<search::Hit>> {
    if query.trim().is_empty() {
        return Err(err("invalid_argument", "Provide a nonempty keyword query"));
    }
    // Reuse one finite reader and fetch only 32 IDs at a time. Release each
    // node snapshot; source witnesses, not one whole-history transaction, bind hits.
    let db = open(path)?;
    let mut after = 0i64;
    let watermark =
        db.connection
            .query_row("SELECT COALESCE(MAX(rowid),0) FROM node", [], |r| {
                r.get::<_, i64>(0)
            })?;
    let mut hits = Vec::new();
    loop {
        let rows = {
            let mut q=db.connection.prepare("SELECT rowid,id FROM node WHERE rowid>?1 AND rowid<=?2 AND payload_json IS NOT NULL AND deleted_reason IS NULL ORDER BY rowid LIMIT 32")?;
            q.query_map(params![after, watermark], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        if rows.is_empty() {
            break;
        }
        for (rowid, id) in rows {
            after = rowid;
            if !crate::history::reachability::searchable(&db.connection, &id)? {
                db.connection.execute_batch("ROLLBACK;BEGIN;")?;
                continue;
            }
            let (kind, _, payload, _) = match node(&db.connection, &id) {
                Ok(node) => node,
                Err(error) if error.code == "revision_unavailable" => {
                    db.connection.execute_batch("ROLLBACK;BEGIN;")?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            for body in bodies(&db.connection, &id, &kind, &payload)? {
                if let Some(score) = search::keyword_score(&body.text, query) {
                    hits.push(make_hit(&db, &id, body, (score, None, None))?);
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
                        )?;
                        if !range.is_null() {
                            hit.location["range"] = range;
                        }
                        hits.push(hit);
                    }
                    Ok(None) => {}
                    Err(error) if error.code == "revision_unavailable" => {}
                    Err(error) => return Err(error),
                }
            }
            // Bound accumulated candidates even within one snapshot.
            hits.sort_by(search::Hit::compare);
            hits.truncate(100);
            db.connection.execute_batch("ROLLBACK;BEGIN;")?;
        }
    }
    Ok(hits)
}
pub(super) async fn semantic(context: &search::Context, query: &str) -> Result<Vec<search::Hit>> {
    let descriptor = {
        let db = open_connection(&context.history)?;
        crate::index::rag_space(&db)
            .map_err(|_| {
                err(
                    "history_unavailable",
                    "Cannot inspect history embedding space",
                )
            })?
            .ok_or_else(|| err("index_incompatible", "History has no active semantic index"))?
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
        .ok_or_else(||err("index_incompatible","History index endpoint/model identity is unverified; restore its original split_chars setting or select a verified index space"))?;
    let profile = search::Profile {
        id: key,
        name: name.into(),
        model: model.into(),
        endpoint_hash: hash(connection.base_url.as_bytes()),
        client: crate::model::http::HttpModelClient::new(connection.clone()).map_err(|_| {
            err(
                "config_invalid",
                "Cannot initialize history embedding connection",
            )
        })?,
    };
    let dimension = descriptor.dimension;
    let response = search::embed(&profile, &[query.into()], Some(dimension)).await?;
    let db = open(&context.history)?;
    let matches = crate::index::rag_hits(&db.connection, &descriptor, &response.vectors[0], 100)
        .map_err(|_| {
            err(
                "history_changed",
                "History index changed or its witnesses expired; retry search",
            )
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
            )?;
            if let Some(range) = range {
                hit.location["range"] = range;
                hit.location["indexed_range"] = json!({"start":span.start,"end":span.end});
            }
            hits.push(hit);
        }
    }
    Ok(hits)
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
