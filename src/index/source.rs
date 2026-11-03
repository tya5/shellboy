//! Source selection, bounded parsing, and canonical body extraction.
use super::*;

#[derive(Debug)]
pub(super) struct Source {
    pub(super) witness: Witness,
    pub(super) kind: String,
    pub(super) payload: Value,
    pub(super) attachment_paths: Vec<String>,
    pub(super) raw_expected: bool,
    pub(super) _permit: OwnedSemaphorePermit,
}
impl IndexService {
    pub(super) async fn selected_sources(&self, args: &IndexArgs) -> Result<Vec<String>, ApiError> {
        let thread_path_sql = "SELECT p.node_id FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.thread_id=?1 AND t.deleted_at IS NULL ORDER BY p.ordinal";
        if args.scope.is_none() && args.thread_ids.is_empty() && args.source_ids.is_empty() {
            return Err(err(
                ErrorCode::InvalidArgument,
                "Select source IDs, Thread IDs, or an explicit all scope before backfill/rebuild",
            ));
        }
        let args = args.clone();
        self.database
            .call(move |db| {
                let mut ids = args.source_ids;
                for thread in args.thread_ids {
                    let mut statement = db.prepare(thread_path_sql)?;
                    ids.extend(
                        statement
                            .query_map([thread], |r| r.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                }
                if args.scope == Some(IndexScope::All) {
                    let mut statement = db.prepare(
                        "SELECT id FROM node WHERE payload_json IS NOT NULL ORDER BY created_at,id",
                    )?;
                    ids.extend(
                        statement
                            .query_map([], |r| r.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                }
                let mut seen = HashSet::new();
                ids.retain(|id| seen.insert(id.clone()));
                Ok(ids)
            })
            .await
            .map_err(db_error)
    }
    pub(super) async fn load_source(&self, node: &str) -> Result<Option<Source>, ApiError> {
        let attachment_ref_sql = "SELECT absolute_path FROM attachment_ref WHERE source_node_id=?1 ORDER BY created_at,id";
        let permit = self
            .source_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| err(ErrorCode::InvalidState, "Index source workers closed"))?;
        let node = node.to_owned();
        let source = self
            .database
            .call(move |db| {
                let source = history::NodeRow::load(db, &node)?;
                let Some(history::NodeRow {
                    kind,
                    payload: Some(body),
                    revision,
                    body_hash,
                    deleted_reason: None,
                }) = source
                else {
                    return Ok(None);
                };
                let raw = RawWitness::for_node(db, &node, false)?;
                let raw = match raw {
                    Some(raw)
                        if meta(db, &format!("raw_tombstone.{}", raw.0.execution))?.is_none() =>
                    {
                        Some(raw)
                    }
                    _ => None,
                };
                let raw_expected = raw.is_some();
                let raw = raw
                    .filter(|(_, state)| matches!(state.as_str(), "exited" | "failed" | "stopped"))
                    .map(|(witness, _)| witness);
                let mut statement = db.prepare(attachment_ref_sql)?;
                let paths = statement
                    .query_map([&node], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Some((
                    Witness {
                        node_id: node,
                        revision,
                        body_hash,
                        raw,
                    },
                    kind,
                    body,
                    paths,
                    raw_expected,
                )))
            })
            .await
            .map_err(db_error)?;
        let Some((witness, kind, body, attachment_paths, raw_expected)) = source else {
            return Ok(None);
        };
        tokio::task::spawn_blocking(move || {
            let payload = serde_json::from_str(&body).map_err(|_| {
                err(
                    ErrorCode::InvalidState,
                    "Stored source payload is malformed",
                )
            })?;
            Ok(Some(Source {
                witness,
                kind,
                payload,
                attachment_paths,
                raw_expected,
                _permit: permit,
            }))
        })
        .await
        .map_err(|_| err(ErrorCode::IoFailed, "Index source parser failed"))?
    }
}
pub(super) fn source_bodies(source: &Source) -> Vec<(String, String)> {
    body_facts(&source.kind, &source.payload, &source.attachment_paths)
}
/// Pure canonical body extraction for already collected saved facts.
pub(super) fn body_facts(kind: &str, payload: &Value, paths: &[String]) -> Vec<(String, String)> {
    let mut bodies = history::search_bodies(kind, payload);
    if !paths.is_empty() {
        bodies.push(("attachment_paths".into(), paths.join("\n")));
    }
    bodies
}
/// Own the existing pinned reader and bounded unfinished segment across pages.
/// Preview and authorization share segmentation, not authorization/DB effects.
pub(super) struct RawSegments {
    stream: crate::control::output::OutputStreamReader,
    carry: String,
    offset: usize,
    split_chars: usize,
}
impl RawSegments {
    pub(super) fn new(
        stream: crate::control::output::OutputStreamReader,
        split_chars: usize,
    ) -> Self {
        Self {
            stream,
            carry: String::new(),
            offset: 0,
            split_chars,
        }
    }
    pub(super) async fn next_page(&mut self) -> Result<Option<(u64, Vec<Segment>)>, ApiError> {
        let Some(page) = self.stream.next_page(64 * 1024).await? else {
            return Ok(None);
        };
        let characters = page.text.chars().count() as u64;
        self.carry.push_str(&page.text);
        let segments = drain_segments(
            &mut self.carry,
            self.split_chars,
            &mut self.offset,
            page.done,
        );
        Ok(Some((characters, segments)))
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use rusqlite::{
        OpenFlags, StatementStatus,
        trace::{TraceEvent, TraceEventCodes},
    };
    use std::{sync::Mutex, time::Instant};

    static TRACE: std::sync::OnceLock<Mutex<Vec<(String, std::time::Duration, i32)>>> =
        std::sync::OnceLock::new();

    #[test]
    #[ignore = "opt-in read-only IndexPreview DB projection audit"]
    fn measure_index_preview_db_projection_audit() {
        let path = std::path::PathBuf::from(
            std::env::var_os("WRITER_AUDIT_DB").expect("verified disposable DB copy required"),
        );
        let thread = std::env::var("WRITER_AUDIT_THREAD").expect("existing thread ID required");
        let db = rusqlite::Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open verified copy read-only");
        db.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT | TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(|event| {
                if let TraceEvent::Profile(stmt, elapsed) = event {
                    TRACE.get_or_init(Default::default).lock().unwrap().push((
                        stmt.sql().into_owned(),
                        elapsed,
                        stmt.get_status(StatementStatus::FullscanStep),
                    ));
                }
            }),
        );
        let active = meta(&db, IndexKeys::ACTIVE_SPACE).expect("read active space");
        let space = active
            .as_deref()
            .map(|id| load_space(&db, id))
            .transpose()
            .expect("load active space")
            .flatten();
        let path_sql = "SELECT p.node_id FROM thread_path p JOIN thread t ON t.id=p.thread_id WHERE p.thread_id=?1 AND t.deleted_at IS NULL ORDER BY p.ordinal";
        let attachment_sql = "SELECT absolute_path FROM attachment_ref WHERE source_node_id=?1 ORDER BY created_at,id";
        let coverage_sql = "SELECT j.body_kind,j.source_hash,j.span_start,j.span_end,j.state,j.authorized_watermark,EXISTS(SELECT 1 FROM embedding_chunk c WHERE c.job_id=j.id AND c.space_id=j.space_id AND c.source_node_id=j.source_node_id AND c.source_revision=j.source_revision AND c.content_hash=j.source_hash AND c.span_start=j.span_start AND c.span_end=j.span_end) FROM embedding_job j WHERE j.space_id=?1 AND j.source_node_id=?2 ORDER BY j.span_start,j.span_end";

        let mut timings: Vec<u128> = Vec::new();
        for repeat in 1..=3 {
            TRACE.get_or_init(Default::default).lock().unwrap().clear();
            let tx = db.unchecked_transaction().expect("read transaction");
            let start = Instant::now();
            let ids = tx
                .prepare(path_sql)
                .and_then(|mut statement| {
                    statement
                        .query_map([&thread], |row| row.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()
                })
                .expect("production IndexPreview thread source selection");
            for id in &ids {
                let Some(row) = history::NodeRow::load(&tx, id)
                    .expect("production IndexPreview node projection")
                else {
                    continue;
                };
                let raw = RawWitness::for_node(&tx, id, false)
                    .expect("production IndexPreview raw witness projection");
                let raw = match raw {
                    Some((witness, _))
                        if meta(&tx, &format!("raw_tombstone.{}", witness.execution))
                            .expect("read raw tombstone")
                            .is_none() =>
                    {
                        Some(witness)
                    }
                    _ => None,
                };
                let witness = Witness {
                    node_id: id.clone(),
                    revision: row.revision,
                    body_hash: row.body_hash,
                    raw,
                };
                let valid = witness_valid(&tx, &witness.canonical(), "canonical")
                    .expect("production IndexPreview witness validation");
                let paths = tx
                    .prepare(attachment_sql)
                    .and_then(|mut statement| {
                        statement
                            .query_map([id], |row| row.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .expect("production IndexPreview attachment projection");
                if let (Some(space), true) = (&space, valid) {
                    let _: Vec<(String, String, i64, i64, String, Option<String>, bool)> = tx
                        .prepare(coverage_sql)
                        .and_then(|mut statement| {
                            statement
                                .query_map(rusqlite::params![space.id, id], |row| {
                                    Ok((
                                        row.get(0)?,
                                        row.get(1)?,
                                        row.get(2)?,
                                        row.get(3)?,
                                        row.get(4)?,
                                        row.get(5)?,
                                        row.get(6)?,
                                    ))
                                })?
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .expect("production IndexPreview coverage projection");
                }
                if let Some(body) = row.payload {
                    if let Ok(payload) = serde_json::from_str::<Value>(&body) {
                        for (_, text) in body_facts(&row.kind, &payload, &paths) {
                            let _ = split_text(&text, 512, 0);
                        }
                    }
                }
            }
            let elapsed_us = start.elapsed().as_micros();
            timings.push(elapsed_us);
            eprintln!(
                "AUDIT kind=IndexPreviewDbProjection repeat={repeat} elapsed_us={elapsed_us}"
            );
            tx.commit().expect("close read transaction");
            let statements = TRACE.get_or_init(Default::default).lock().unwrap().clone();
            for (sql, elapsed, fullscan) in statements {
                eprintln!(
                    "AUDIT_SQL kind=IndexPreviewDbProjection elapsed_us={} fullscan_step={} sql={}",
                    elapsed.as_micros(),
                    fullscan,
                    sql
                );
                if let Ok(mut plan) = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")) {
                    let values =
                        std::iter::repeat_n(rusqlite::types::Value::Null, plan.parameter_count());
                    if let Ok(rows) = plan.query_map(rusqlite::params_from_iter(values), |row| {
                        row.get::<_, String>(3)
                    }) {
                        for detail in rows.flatten() {
                            eprintln!("AUDIT_PLAN kind=IndexPreviewDbProjection detail={detail}");
                        }
                    }
                }
            }
        }
        eprintln!("AUDIT_TIMES kind=IndexPreviewDbProjection values_us={timings:?}");
    }
}
