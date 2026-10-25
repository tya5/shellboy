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
    let mut bodies = history::search_bodies(&source.kind, &source.payload);
    if !source.attachment_paths.is_empty() {
        bodies.push((
            "attachment_paths".into(),
            source.attachment_paths.join("\n"),
        ));
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
