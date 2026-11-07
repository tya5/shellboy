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
/// Which node kinds a Thread/all-history selection takes in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum KindFilter {
    /// `indexing.auto_kinds` plus the command's `--kind` additions.
    Selected,
    /// Retry acts on work that was already authorized, whatever its kind.
    Any,
}
/// Source IDs to act on and how many sources of other indexable kinds the
/// Thread/all-history selection left out (they are not counted or embedded).
pub(super) struct Selection {
    pub(super) ids: Vec<String>,
    pub(super) excluded: u64,
}
impl IndexService {
    /// Kinds taken in for Thread/all-history selections: the configured
    /// automatic kinds plus this command's additions; `all` adds every kind.
    pub(super) fn selected_kinds(&self, args: &IndexArgs) -> Vec<String> {
        let mut kinds = self.config.indexing.auto_kinds.clone();
        for kind in &args.kinds {
            if kind == "all" {
                kinds.extend(crate::config::INDEXABLE_KINDS.map(String::from));
            } else {
                kinds.push(kind.clone());
            }
        }
        let mut seen = HashSet::new();
        kinds.retain(|kind| seen.insert(kind.clone()));
        kinds
    }
    pub(super) async fn selected_sources(&self, args: &IndexArgs) -> Result<Vec<String>, ApiError> {
        Ok(self.select_sources(args, KindFilter::Selected).await?.ids)
    }
    pub(super) async fn select_sources(
        &self,
        args: &IndexArgs,
        filter: KindFilter,
    ) -> Result<Selection, ApiError> {
        if args.scope.is_none() && args.thread_ids.is_empty() && args.source_ids.is_empty() {
            return Err(err(
                ErrorCode::InvalidArgument,
                "Select source IDs, Thread IDs, or an explicit all scope before backfill/rebuild",
            ));
        }
        let kinds = match filter {
            KindFilter::Selected => self.selected_kinds(args),
            KindFilter::Any => crate::config::INDEXABLE_KINDS.map(String::from).to_vec(),
        };
        let kinds = serde_json::to_string(&kinds)
            .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid kinds"))?;
        let all = serde_json::to_string(&crate::config::INDEXABLE_KINDS)
            .map_err(|_| err(ErrorCode::InvalidArgument, "Invalid kinds"))?;
        let args = args.clone();
        self.database
            .call(move |db| {
                // Source IDs are an explicit choice and are never filtered by kind.
                let mut ids = args.source_ids;
                let mut excluded = 0u64;
                for thread in args.thread_ids {
                    let mut statement = db.prepare("SELECT p.node_id FROM thread_path p JOIN thread t ON t.id=p.thread_id JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND t.deleted_at IS NULL AND n.kind IN (SELECT value FROM json_each(?2)) ORDER BY p.ordinal")?;
                    ids.extend(
                        statement
                            .query_map(params![thread, kinds], |r| r.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    excluded += db.query_row("SELECT count(*) FROM thread_path p JOIN thread t ON t.id=p.thread_id JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND t.deleted_at IS NULL AND n.kind IN (SELECT value FROM json_each(?3)) AND n.kind NOT IN (SELECT value FROM json_each(?2))", params![thread, kinds, all], |r| r.get::<_, i64>(0))? as u64;
                }
                if args.scope == Some(IndexScope::All) {
                    let mut statement = db.prepare(
                        "SELECT id FROM node WHERE payload_json IS NOT NULL AND kind IN (SELECT value FROM json_each(?1)) ORDER BY created_at,id",
                    )?;
                    ids.extend(
                        statement
                            .query_map([&kinds], |r| r.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    excluded += db.query_row("SELECT count(*) FROM node WHERE kind IN (SELECT value FROM json_each(?2)) AND kind NOT IN (SELECT value FROM json_each(?1))", params![kinds, all], |r| r.get::<_, i64>(0))? as u64;
                }
                let mut seen = HashSet::new();
                ids.retain(|id| seen.insert(id.clone()));
                Ok(Selection { ids, excluded })
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
