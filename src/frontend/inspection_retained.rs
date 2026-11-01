//! Retained scopes and body-free facts on the existing inspection owner.
//! Shared history observation and native cursor policy remain authoritative.
//! Original bytes, cursors and capture time remain solely in G management.detail.
use std::sync::Arc;

pub(crate) const MAX_TARGETS: usize = 200;
pub(crate) const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetainedRoute {
    Relations,
    SearchReferences,
    ShellPage,
    CompletedWait,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetKind {
    Node,
    Execution,
    Run,
    Turn,
    Thread,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedRawFacts {
    pub(crate) generation: Option<u64>,
    pub(crate) saved_sequence: Option<u64>,
    pub(crate) cursor_position: Option<(u64, u64)>,
    pub(crate) encoding: Option<Arc<str>>,
    pub(crate) lifecycle: Option<Arc<str>>,
}
pub(crate) struct CaptureTarget<'a> {
    pub(crate) kind: TargetKind,
    pub(crate) id: &'a str,
    // Only the original request's explicit selection, never a viewer/provenance.
    pub(crate) selecting_thread: Option<&'a str>,
    pub(crate) source_revision: Option<u64>,
    // Comparable node-body hash only. Replacement/span hashes stay in the
    // original result; they must not be compared with node.body_hash.
    pub(crate) source_hash: Option<&'a str>,
    pub(crate) raw: Option<&'a CapturedRawFacts>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExactTarget {
    pub(crate) kind: TargetKind,
    pub(crate) id: Arc<str>,
    pub(crate) selecting_thread: Option<Arc<str>>,
    pub(crate) source_revision: Option<u64>,
    pub(crate) source_hash: Option<Arc<str>>,
    pub(crate) raw: Option<CapturedRawFacts>,
}
#[derive(Clone, Debug)]
pub struct RetainedScope {
    pub(crate) result_identity: Arc<str>,
    pub(crate) route: RetainedRoute,
    pub(crate) targets: Vec<ExactTarget>,
    pub(crate) unobserved_targets: usize,
    pub(crate) metadata_bytes: usize,
    pub(crate) original_request: Option<Arc<crate::protocol::Command>>,
    pub(crate) captured_at: Arc<str>,
}
impl PartialEq for RetainedScope {
    fn eq(&self, other: &Self) -> bool {
        // An immutable captured result has one identity; never rescan a retained
        // query/filter body when existing visibility fences compare targets.
        self.result_identity == other.result_identity && self.route == other.route
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CaptureLimit {
    InvalidIdentity,
}
pub(crate) struct CaptureInput<'a> {
    pub(crate) result_identity: &'a str,
    pub(crate) route: RetainedRoute,
    // Original bytes/cursor/capture time stay in G management.detail, not here.
    pub(crate) targets: &'a [CaptureTarget<'a>],
}
fn text_charge(text: &str) -> usize {
    text.as_bytes()
        .iter()
        .map(|b| match b {
            b'"' | b'\\' => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum()
}
fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES
}

pub(crate) fn capture_retained_scope(
    input: CaptureInput<'_>,
) -> Result<RetainedScope, CaptureLimit> {
    if !id_ok(input.result_identity) {
        return Err(CaptureLimit::InvalidIdentity);
    }
    let mut metadata_bytes = text_charge(input.result_identity) + 128;
    let mut targets = Vec::with_capacity(input.targets.len().min(MAX_TARGETS));
    let mut unobserved_targets = input.targets.len().saturating_sub(MAX_TARGETS);
    for t in input.targets.iter().take(MAX_TARGETS) {
        let raw_bytes = t
            .raw
            .map(|r| {
                r.encoding.as_ref().map_or(0, |s| text_charge(s))
                    + r.lifecycle.as_ref().map_or(0, |s| text_charge(s))
            })
            .unwrap_or(0);
        let bytes = text_charge(t.id)
            + t.selecting_thread.map(text_charge).unwrap_or(0)
            + raw_bytes
            + t.source_hash.map(text_charge).unwrap_or(0);
        if !id_ok(t.id)
            || t.selecting_thread.is_some_and(|s| !id_ok(s))
            || t.source_hash.is_some_and(|h| h.len() > 128)
            || t.raw.is_some_and(|r| {
                r.encoding.as_ref().is_some_and(|s| s.len() > 128)
                    || r.lifecycle.as_ref().is_some_and(|s| s.len() > 32)
            })
            || metadata_bytes
                .checked_add(bytes)
                .is_none_or(|n| n > MAX_METADATA_BYTES)
        {
            unobserved_targets += 1;
            continue;
        }
        metadata_bytes += bytes;
        targets.push(ExactTarget {
            kind: t.kind,
            id: Arc::from(t.id),
            selecting_thread: t.selecting_thread.map(Arc::from),
            source_revision: t.source_revision,
            source_hash: t.source_hash.map(Arc::from),
            raw: t.raw.cloned(),
        });
    }
    Ok(RetainedScope {
        result_identity: Arc::from(input.result_identity),
        route: input.route,
        targets,
        unobserved_targets,
        metadata_bytes,
        original_request: None,
        captured_at: Arc::from(""),
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presence {
    Present,
    Missing,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    Selected,
    NotSelected,
    Unknown,
}
#[derive(Clone, Debug)]
pub(crate) struct CurrentRawFacts {
    pub(crate) generation: Option<u64>,
    pub(crate) saved_sequence: Option<u64>,
    pub(crate) encoding: Option<Arc<str>>,
    pub(crate) lifecycle: Option<Arc<str>>,
    pub(crate) tombstone: Option<bool>,
    pub(crate) cursor_chunk_retained: Option<bool>,
    pub(crate) cursor_chunk_length: Option<u64>,
    pub(crate) oldest_sequence: Option<u64>,
}
// G's adapter maps existing history::observation and retained raw metadata into
// these scalar facts. It must not issue search, raw-output or wait operations.
pub(crate) struct CurrentTargetFacts {
    pub(crate) target_index: usize,
    pub(crate) presence: Presence,
    pub(crate) selection: Selection,
    pub(crate) source_revision: Option<u64>,
    pub(crate) source_hash: Option<Arc<str>>,
    pub(crate) body_retained: Option<bool>,
    pub(crate) lifecycle: Option<Arc<str>>,
    pub(crate) raw: Option<CurrentRawFacts>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedTarget {
    pub(crate) presence: Presence,
    pub(crate) selection: Selection,
    pub(crate) source_changed: Option<bool>,
    pub(crate) body_retained: Option<bool>,
    pub(crate) generation_changed: Option<bool>,
    pub(crate) output_grew: Option<bool>,
    pub(crate) encoding_changed: Option<bool>,
    pub(crate) lifecycle_changed: Option<bool>,
    pub(crate) current_lifecycle: Option<Arc<str>>,
    pub(crate) current_generation: Option<u64>,
    pub(crate) current_sequence: Option<u64>,
    pub(crate) current_encoding: Option<Arc<str>>,
    pub(crate) current_revision: Option<u64>,
    pub(crate) current_hash: Option<Arc<str>>,
    pub(crate) tombstone: Option<bool>,
    pub(crate) cursor_chunk_retained: Option<bool>,
    pub(crate) cursor_chunk_length: Option<u64>,
    pub(crate) oldest_sequence: Option<u64>,
}
fn unknown_target() -> PreparedTarget {
    PreparedTarget {
        presence: Presence::Unknown,
        selection: Selection::Unknown,
        source_changed: None,
        body_retained: None,
        generation_changed: None,
        output_grew: None,
        encoding_changed: None,
        lifecycle_changed: None,
        current_lifecycle: None,
        current_generation: None,
        current_sequence: None,
        current_encoding: None,
        current_revision: None,
        current_hash: None,
        tombstone: None,
        cursor_chunk_retained: None,
        cursor_chunk_length: None,
        oldest_sequence: None,
    }
}
// None facts means a failed/limited observation, not a successful empty result.
// Adoption/generation gates belong to G; identity mismatch cannot adopt anything.
pub(crate) fn prepare_retained_facts(
    scope: &RetainedScope,
    result_identity: &str,
    facts: Option<&[CurrentTargetFacts]>,
) -> Option<Vec<PreparedTarget>> {
    if scope.result_identity.as_ref() != result_identity
        || scope.targets.len() > MAX_TARGETS
        || scope.metadata_bytes > MAX_METADATA_BYTES
    {
        return None;
    }
    let mut prepared = vec![unknown_target(); scope.targets.len()];
    let Some(facts) = facts else {
        return Some(prepared);
    };
    if facts.len() > MAX_TARGETS {
        return Some(prepared);
    }
    let mut seen = [false; MAX_TARGETS];
    for f in facts {
        let Some(original) = scope.targets.get(f.target_index) else {
            continue;
        };
        if seen[f.target_index] {
            prepared[f.target_index] = unknown_target();
            continue;
        }
        seen[f.target_index] = true;
        let changed_hash = original
            .source_hash
            .as_ref()
            .zip(f.source_hash.as_ref())
            .map(|(a, b)| a != b);
        let changed_revision = original
            .source_revision
            .zip(f.source_revision)
            .map(|(a, b)| a != b);
        let mut p = PreparedTarget {
            presence: f.presence,
            selection: if original.selecting_thread.is_some() {
                f.selection
            } else {
                Selection::Unknown
            },
            source_changed: match (changed_hash, changed_revision) {
                (Some(a), Some(b)) => Some(a || b),
                (a, b) => a.or(b),
            },
            body_retained: f.body_retained,
            current_lifecycle: f.lifecycle.clone(),
            current_revision: f.source_revision,
            current_hash: f.source_hash.clone(),
            ..unknown_target()
        };
        if let Some(now) = &f.raw {
            p.current_generation = now.generation;
            p.current_sequence = now.saved_sequence;
            p.current_encoding = now.encoding.clone();
            p.current_lifecycle = now.lifecycle.clone();
            p.tombstone = now.tombstone;
            p.cursor_chunk_retained = now.cursor_chunk_retained;
            p.cursor_chunk_length = now.cursor_chunk_length;
            p.oldest_sequence = now.oldest_sequence;
            if let Some(before) = &original.raw {
                p.generation_changed = before.generation.zip(now.generation).map(|(a, b)| a != b);
                p.output_grew = before
                    .saved_sequence
                    .zip(now.saved_sequence)
                    .filter(|_| p.generation_changed != Some(true))
                    .map(|(a, b)| b > a);
                p.encoding_changed = before
                    .encoding
                    .as_ref()
                    .zip(now.encoding.as_ref())
                    .map(|(a, b)| a != b);
                p.lifecycle_changed = before
                    .lifecycle
                    .as_ref()
                    .zip(now.lifecycle.as_ref())
                    .map(|(a, b)| a != b);
            }
        }
        prepared[f.target_index] = p;
    }
    Some(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(route: RetainedRoute, targets: &[CaptureTarget<'_>]) -> RetainedScope {
        capture_retained_scope(CaptureInput {
            result_identity: "original",
            route,
            targets,
        })
        .unwrap()
    }
    fn target<'a>(id: &'a str, thread: Option<&'a str>) -> CaptureTarget<'a> {
        CaptureTarget {
            kind: TargetKind::Node,
            id,
            selecting_thread: thread,
            source_revision: Some(1),
            source_hash: Some("captured opaque hash"),
            raw: None,
        }
    }
    fn fact(presence: Presence) -> CurrentTargetFacts {
        CurrentTargetFacts {
            target_index: 0,
            presence,
            selection: Selection::NotSelected,
            source_revision: Some(2),
            source_hash: Some(Arc::from("current opaque hash")),
            body_retained: Some(false),
            lifecycle: None,
            raw: None,
        }
    }
    #[test]
    fn all_routes_preserve_original_data_while_current_selection_and_sources_change() {
        for route in [
            RetainedRoute::Relations,
            RetainedRoute::SearchReferences,
            RetainedRoute::ShellPage,
            RetainedRoute::CompletedWait,
        ] {
            let scope = capture(route, &[target("node", Some("original-thread"))]);
            let original = scope.targets.clone();
            let facts =
                prepare_retained_facts(&scope, "original", Some(&[fact(Presence::Missing)]))
                    .unwrap();
            assert_eq!(facts[0].presence, Presence::Missing);
            assert_eq!(facts[0].selection, Selection::NotSelected);
            assert_eq!(facts[0].source_changed, Some(true));
            assert_eq!(original, scope.targets);
            assert_eq!(scope.result_identity.as_ref(), "original");
        }
    }
    #[test]
    fn no_original_selection_is_unknown_and_failed_observation_is_not_deletion() {
        let scope = capture(RetainedRoute::SearchReferences, &[target("node", None)]);
        let p =
            prepare_retained_facts(&scope, "original", Some(&[fact(Presence::Present)])).unwrap();
        assert_eq!(p[0].selection, Selection::Unknown);
        let p = prepare_retained_facts(&scope, "original", None).unwrap();
        assert_eq!(p[0].presence, Presence::Unknown);
        assert_eq!(p[0].body_retained, None);
        assert!(prepare_retained_facts(&scope, "new explicit result", None).is_none());
    }
    #[test]
    fn capture_observation_bound_and_metadata_are_checked_before_copy() {
        let targets = (0..201).map(|_| target("node", None)).collect::<Vec<_>>();
        let scope = capture(RetainedRoute::Relations, &targets);
        assert_eq!(scope.targets.len(), 200);
        assert_eq!(scope.unobserved_targets, 1);
        assert!(scope.metadata_bytes <= MAX_METADATA_BYTES);
        let id = "x".repeat(129);
        let scope = capture(RetainedRoute::Relations, &[target(&id, None)]);
        assert!(scope.targets.is_empty());
        assert_eq!(scope.unobserved_targets, 1);
        let raw = CapturedRawFacts {
            generation: Some(0),
            saved_sequence: Some(0),
            cursor_position: None,
            encoding: Some(Arc::from("x".repeat(MAX_METADATA_BYTES + 1))),
            lifecycle: Some(Arc::from("running")),
        };
        let t = CaptureTarget {
            kind: TargetKind::Execution,
            id: "e",
            selecting_thread: None,
            source_revision: None,
            source_hash: None,
            raw: Some(&raw),
        };
        let scope = capture(RetainedRoute::ShellPage, &[t]);
        assert!(scope.targets.is_empty());
        assert_eq!(scope.unobserved_targets, 1);
        assert!(scope.metadata_bytes <= MAX_METADATA_BYTES);
    }
    #[test]
    fn raw_growth_lifecycle_and_retention_are_separate_not_cursor_authorization() {
        let before = CapturedRawFacts {
            generation: Some(1),
            saved_sequence: Some(4),
            cursor_position: Some((2, 0)),
            encoding: Some(Arc::from("UTF-8")),
            lifecycle: Some(Arc::from("running")),
        };
        let t = CaptureTarget {
            kind: TargetKind::Execution,
            id: "e",
            selecting_thread: None,
            source_revision: None,
            source_hash: None,
            raw: Some(&before),
        };
        let scope = capture(RetainedRoute::ShellPage, &[t]);
        let mut f = fact(Presence::Present);
        f.raw = Some(CurrentRawFacts {
            generation: Some(1),
            saved_sequence: Some(9),
            encoding: Some(Arc::from("UTF-8")),
            lifecycle: Some(Arc::from("exited")),
            tombstone: Some(false),
            cursor_chunk_retained: Some(true),
            cursor_chunk_length: Some(3),
            oldest_sequence: Some(0),
        });
        let p = prepare_retained_facts(&scope, "original", Some(&[f])).unwrap();
        assert_eq!(p[0].output_grew, Some(true));
        assert_eq!(p[0].generation_changed, Some(false));
        assert_eq!(p[0].lifecycle_changed, Some(true));
        assert_eq!(
            scope.targets[0].raw.as_ref().unwrap().saved_sequence,
            Some(4)
        );
    }
}

// A single node cohort is read by the shared history facade on the caller's
// connection; no second observer, body cache, or independent DB job is created.
#[derive(Clone, Debug)]
pub(crate) struct HistoryObservationRequest {
    pub(crate) ids: Vec<String>,
    pub(crate) thread_id: Option<String>,
}
pub(crate) struct OwnedLifecycleRaw {
    target_index: usize,
    exists: bool,
    state: Option<String>,
    metadata_limited: bool,
}
pub(crate) struct OwnedRetainedRaw {
    scope: RetainedScope,
    history: Option<crate::history::observation::Raw>,
    history_indices: Vec<usize>,
    executions: Vec<(usize, crate::history::RetainedExecutionFacts)>,
    lifecycles: Vec<OwnedLifecycleRaw>,
    pub(crate) leaf_sql_count: usize,
}
const LIFECYCLE_SQL: &str = "WITH requested AS (
    SELECT CAST(key AS INTEGER) ordinal,json_extract(value,'$.kind') kind,json_extract(value,'$.id') id FROM json_each(?1)
    ) SELECT q.ordinal,COALESCE(r.id,u.id,t.id) IS NOT NULL,
       CASE WHEN length(CAST(COALESCE(r.state,u.state,CASE WHEN t.id IS NOT NULL THEN CASE WHEN t.deleted_at IS NULL THEN 'available' ELSE 'unavailable' END END) AS BLOB))<=32 THEN COALESCE(r.state,u.state,CASE WHEN t.id IS NOT NULL THEN CASE WHEN t.deleted_at IS NULL THEN 'available' ELSE 'unavailable' END END) END,
       COALESCE(length(CAST(COALESCE(r.state,u.state,CASE WHEN t.id IS NOT NULL THEN CASE WHEN t.deleted_at IS NULL THEN 'available' ELSE 'unavailable' END END) AS BLOB))>32,0)
    FROM requested q LEFT JOIN run r ON q.kind='run' AND r.id=q.id
    LEFT JOIN turn u ON q.kind='turn' AND u.id=q.id LEFT JOIN thread t ON q.kind='thread' AND t.id=q.id ORDER BY q.ordinal";

pub(crate) fn read_retained_source_facts_tx(
    db: &rusqlite::Connection,
    scope: &RetainedScope,
) -> Result<OwnedRetainedRaw, crate::storage::StorageError> {
    use crate::history::{RawWitness, RetainedExecutionRequest};
    if scope.targets.len() > MAX_TARGETS || scope.metadata_bytes > MAX_METADATA_BYTES {
        return Err(crate::storage::StorageError::InvalidInput(
            "retained observation scope exceeds bounds".into(),
        ));
    }
    let mut ids = Vec::new();
    let mut node_indices = Vec::new();
    let mut thread_id: Option<String> = None;
    let mut mixed = false;
    let mut remaining = MAX_METADATA_BYTES
        .checked_sub(
            scope
                .targets
                .iter()
                .map(|t| match t.kind {
                    TargetKind::Execution => 768,
                    TargetKind::Node => 0,
                    _ => 256,
                })
                .sum::<usize>()
                + 4096
                + scope
                    .targets
                    .iter()
                    .map(|t| text_charge(&t.id) + 2)
                    .sum::<usize>(),
        )
        .ok_or_else(|| {
            crate::storage::StorageError::from(crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::InputLimit,
                "retained aggregate metadata budget exceeded",
            ))
        })?;
    let mut raw_indices = Vec::new();
    let mut raw_requests = Vec::new();
    let mut lifecycle_indices = Vec::new();
    let mut lifecycle_requests = Vec::new();
    for (index, t) in scope.targets.iter().enumerate() {
        if !id_ok(&t.id) {
            return Err(crate::storage::StorageError::InvalidInput(
                "retained target ID exceeds scalar bound".into(),
            ));
        }
        match t.kind {
            TargetKind::Node => {
                ids.push(t.id.to_string());
                node_indices.push(index);
                match &t.selecting_thread {
                    Some(thread) if thread_id.as_deref().is_none_or(|id| id == thread.as_ref()) => {
                        thread_id = Some(thread.to_string());
                    }
                    _ => {
                        mixed = true;
                    }
                }
            }
            TargetKind::Execution => {
                raw_indices.push(index);
                raw_requests.push(RetainedExecutionRequest {
                    execution: t.id.to_string(),
                    cursor_sequence: t
                        .raw
                        .as_ref()
                        .and_then(|r| r.cursor_position.map(|(sequence, _)| sequence)),
                });
            }
            TargetKind::Run | TargetKind::Turn | TargetKind::Thread => {
                lifecycle_indices.push(index);
                lifecycle_requests.push(serde_json::json!({"id":t.id.as_ref(),"kind":match t.kind{TargetKind::Run=>"run",TargetKind::Turn=>"turn",_=>"thread"}}));
            }
        }
    }
    if mixed {
        thread_id = None;
    }
    let history_request = (!ids.is_empty()).then_some(HistoryObservationRequest { ids, thread_id });
    let mut leaf_sql_count = 0;
    let executions = if raw_requests.is_empty() {
        Vec::new()
    } else {
        leaf_sql_count += 1;
        let facts = RawWitness::for_retained_execution(db, &raw_requests, &mut remaining)?;
        if facts.len() != raw_indices.len()
            || facts.iter().enumerate().any(|(i, f)| f.request_index != i)
        {
            return Err(crate::storage::StorageError::Schema(
                "retained batch identity mismatch".into(),
            ));
        }
        raw_indices.into_iter().zip(facts).collect()
    };
    let lifecycles = if lifecycle_requests.is_empty() {
        Vec::new()
    } else {
        leaf_sql_count += 1;
        let parameter = serde_json::to_string(&lifecycle_requests)
            .map_err(|e| crate::storage::StorageError::InvalidInput(e.to_string()))?;
        db.prepare(LIFECYCLE_SQL)?
            .query_map([parameter], |r| {
                let value = r.get::<_, i64>(0)?;
                let ordinal = usize::try_from(value)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))?;
                let mut limited = r.get::<_, bool>(3)?;
                let state = if let rusqlite::types::ValueRef::Text(bytes) = r.get_ref(2)? {
                    let charge = bytes
                        .iter()
                        .map(|b| match b {
                            b'"' | b'\\' => 2,
                            0..=31 => 6,
                            _ => 1,
                        })
                        .sum::<usize>();
                    if charge > remaining {
                        limited = true;
                        None
                    } else {
                        remaining -= charge;
                        r.get(2)?
                    }
                } else {
                    r.get(2)?
                };
                Ok(OwnedLifecycleRaw {
                    target_index: *lifecycle_indices
                        .get(ordinal)
                        .ok_or(rusqlite::Error::InvalidQuery)?,
                    exists: r.get(1)?,
                    state,
                    metadata_limited: limited,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let history = if let Some(request) = history_request {
        leaf_sql_count += 1 + usize::from(request.thread_id.is_some());
        Some(crate::history::observation::read_bounded_tx(
            db,
            &request.ids,
            request.thread_id.as_deref(),
            remaining,
        )?)
    } else {
        None
    };
    Ok(OwnedRetainedRaw {
        scope: scope.clone(),
        history,
        history_indices: node_indices,
        executions,
        lifecycles,
        leaf_sql_count,
    })
}
pub(crate) struct PreparedRetained {
    pub(crate) scope: RetainedScope,
    pub(crate) current_targets: Vec<CurrentTargetFacts>,
    pub(crate) limited_fields: Vec<String>,
    pub(crate) leaf_sql_count: usize,
}
pub(crate) fn prepare(
    raw: OwnedRetainedRaw,
) -> Result<PreparedRetained, crate::protocol::ApiError> {
    let mut current_targets = Vec::with_capacity(raw.scope.targets.len());
    let mut limited_fields = Vec::new();
    if let Some(history) = raw.history {
        let value = crate::history::observation::prepare(history);
        let nodes = value["nodes"].as_array().ok_or_else(|| {
            crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::InvalidState,
                "invalid bounded history observation",
            )
        })?;
        for (index, node) in raw.history_indices.iter().zip(nodes) {
            if node["limited_fields"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
            {
                limited_fields.push(format!("targets/{index}/source"));
            }
            current_targets.push(CurrentTargetFacts {
                target_index: *index,
                presence: match node["node_exists"].as_bool() {
                    Some(true) => Presence::Present,
                    Some(false) => Presence::Missing,
                    None => Presence::Unknown,
                },
                selection: match node["selected"].as_bool() {
                    Some(true) => Selection::Selected,
                    Some(false) => Selection::NotSelected,
                    None => Selection::Unknown,
                },
                source_revision: node["source_revision"].as_u64(),
                source_hash: node["body_hash"].as_str().map(Arc::from),
                body_retained: node["body_available"].as_bool(),
                lifecycle: None,
                raw: None,
            });
        }
    }
    for (index, f) in raw.executions {
        if f.metadata_limited {
            limited_fields.push(format!("targets/{index}/raw"));
        }
        current_targets.push(CurrentTargetFacts {
            target_index: index,
            presence: if f.exists {
                Presence::Present
            } else {
                Presence::Missing
            },
            selection: Selection::Unknown,
            source_revision: None,
            source_hash: None,
            body_retained: None,
            lifecycle: f.state.as_deref().map(Arc::from),
            raw: Some(CurrentRawFacts {
                generation: f.generation.and_then(|n| u64::try_from(n).ok()),
                saved_sequence: f.saved_sequence.and_then(|n| u64::try_from(n).ok()),
                encoding: f.encoding.as_deref().and_then(|s| {
                    crate::control::output::canonical_encoding(s)
                        .ok()
                        .map(|e| Arc::from(e.name()))
                }),
                lifecycle: f.state.map(Arc::from),
                tombstone: Some(f.tombstone),
                cursor_chunk_retained: raw.scope.targets[index]
                    .raw
                    .as_ref()
                    .and_then(|c| c.cursor_position)
                    .and_then(|(sequence, _)| {
                        f.saved_sequence
                            .and_then(|n| u64::try_from(n).ok())
                            .filter(|n| sequence < *n)
                            .map(|_| f.cursor_chunk_length.is_some())
                    }),
                cursor_chunk_length: f.cursor_chunk_length,
                oldest_sequence: f.oldest_sequence,
            }),
        });
    }
    for f in raw.lifecycles {
        if f.metadata_limited {
            limited_fields.push(format!("targets/{}/lifecycle", f.target_index));
        }
        current_targets.push(CurrentTargetFacts {
            target_index: f.target_index,
            presence: if f.exists {
                Presence::Present
            } else {
                Presence::Missing
            },
            selection: Selection::Unknown,
            source_revision: None,
            source_hash: None,
            body_retained: None,
            lifecycle: f.state.map(Arc::from),
            raw: None,
        });
    }
    Ok(PreparedRetained {
        scope: raw.scope,
        current_targets,
        limited_fields,
        leaf_sql_count: raw.leaf_sql_count,
    })
}

#[cfg(test)]
mod adapter_contract_tests {
    use super::*;
    #[test]
    fn lifecycle_read_plan_preserves_exact_ids_and_no_wait_registration() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE run(id TEXT PRIMARY KEY,state TEXT);CREATE TABLE turn(id TEXT PRIMARY KEY,state TEXT);CREATE TABLE thread(id TEXT PRIMARY KEY,deleted_at TEXT);
            INSERT INTO run VALUES('r','completed');INSERT INTO turn VALUES('u','running');").unwrap();
        let requests=serde_json::json!([{"kind":"run","id":"r"},{"kind":"turn","id":"u"},{"kind":"run","id":"missing"}]).to_string();
        let rows = db
            .prepare(LIFECYCLE_SQL)
            .unwrap()
            .query_map([&requests], |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (0, true, Some("completed".into())),
                (1, true, Some("running".into())),
                (2, false, None)
            ]
        );
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {LIFECYCLE_SQL}"))
            .unwrap()
            .query_map([&requests], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("sqlite_autoindex_run_1"), "{plan}");
        assert!(plan.contains("sqlite_autoindex_turn_1"), "{plan}");
    }
}

#[cfg(test)]
mod comparison_limit_tests {
    use super::*;
    fn scope() -> RetainedScope {
        capture_retained_scope(CaptureInput {
            result_identity: "r",
            route: RetainedRoute::CompletedWait,
            targets: &[CaptureTarget {
                kind: TargetKind::Run,
                id: "run",
                selecting_thread: None,
                source_revision: None,
                source_hash: None,
                raw: None,
            }],
        })
        .unwrap()
    }
    fn current(index: usize, presence: Presence) -> CurrentTargetFacts {
        CurrentTargetFacts {
            target_index: index,
            presence,
            selection: Selection::Unknown,
            source_revision: None,
            source_hash: None,
            body_retained: None,
            lifecycle: Some(Arc::from("completed")),
            raw: None,
        }
    }
    #[test]
    fn completed_wait_current_lifecycle_does_not_replace_captured_identity() {
        let captured = scope();
        let p =
            prepare_retained_facts(&captured, "r", Some(&[current(0, Presence::Present)])).unwrap();
        assert_eq!(p[0].current_lifecycle.as_deref(), Some("completed"));
        assert_eq!(captured.result_identity.as_ref(), "r");
    }
    #[test]
    fn duplicate_rows_are_unknown_not_last_writer_wins() {
        let captured = scope();
        let p = prepare_retained_facts(
            &captured,
            "r",
            Some(&[current(0, Presence::Missing), current(0, Presence::Present)]),
        )
        .unwrap();
        assert_eq!(p[0].presence, Presence::Unknown);
        assert_eq!(p[0].current_lifecycle, None);
    }
    #[test]
    fn out_of_scope_target_cannot_fill_missing_witness() {
        let captured = scope();
        let p =
            prepare_retained_facts(&captured, "r", Some(&[current(1, Presence::Present)])).unwrap();
        assert_eq!(p[0].presence, Presence::Unknown);
    }
    #[test]
    fn expansion_over_200_current_facts_is_unknown_not_false_empty() {
        let captured = scope();
        let rows = (0..201)
            .map(|_| current(0, Presence::Missing))
            .collect::<Vec<_>>();
        let p = prepare_retained_facts(&captured, "r", Some(&rows)).unwrap();
        assert_eq!(p[0].presence, Presence::Unknown);
    }
    #[test]
    fn explicit_repeat_must_have_its_own_retained_result_identity() {
        assert!(
            prepare_retained_facts(&scope(), "repeat", Some(&[current(0, Presence::Present)]))
                .is_none()
        );
    }
}

pub(super) fn supports(command: &crate::protocol::Command) -> bool {
    matches!(
        command,
        crate::protocol::Command::HistoryRelations(_)
            | crate::protocol::Command::HistorySearch(_)
            | crate::protocol::Command::ShellRead(_)
            | crate::protocol::Command::Wait(_)
    )
}
fn command_bytes(command: &crate::protocol::Command) -> Option<usize> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= 128 * 1024)
                .ok_or_else(|| std::io::Error::other("retained request limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, command).ok()?;
    Some(count.0)
}
pub(super) fn capture_response(
    command: &crate::protocol::Command,
    value: &serde_json::Value,
) -> Option<crate::ui::InspectionTarget> {
    use crate::protocol::Command;
    let mut targets = Vec::new();
    let raw;
    let route = match command {
        Command::HistoryRelations(a) => {
            targets.push(CaptureTarget {
                kind: TargetKind::Node,
                id: &a.node_id,
                selecting_thread: None,
                source_revision: None,
                source_hash: None,
                raw: None,
            });
            for row in value.get("items")?.as_array()?.iter().take(201) {
                if let Some(id) = row["node_id"].as_str() {
                    targets.push(CaptureTarget {
                        kind: TargetKind::Node,
                        id,
                        selecting_thread: None,
                        source_revision: row["source_revision"].as_u64().or_else(|| {
                            row.pointer("/provenance/source_revision")
                                .and_then(serde_json::Value::as_u64)
                        }),
                        source_hash: row["body_hash"].as_str(),
                        raw: None,
                    });
                }
            }
            RetainedRoute::Relations
        }
        Command::HistorySearch(a) => {
            for row in value.get("items")?.as_array()?.iter().take(201) {
                if let Some(id) = row["node_id"].as_str() {
                    targets.push(CaptureTarget {
                        kind: TargetKind::Node,
                        id,
                        selecting_thread: a.thread_id.as_deref(),
                        source_revision: row
                            .pointer("/provenance/source_revision")
                            .and_then(serde_json::Value::as_u64),
                        source_hash: None,
                        raw: None,
                    });
                }
            }
            RetainedRoute::SearchReferences
        }
        Command::HistoryGet(a) => {
            let sources = value.get("source_node_ids")?.as_array()?;
            if sources.is_empty() {
                return None;
            }
            targets.push(CaptureTarget {
                kind: TargetKind::Node,
                id: &a.node_id,
                selecting_thread: None,
                source_revision: value
                    .pointer("/provenance/source_revision")
                    .and_then(serde_json::Value::as_u64),
                source_hash: None,
                raw: None,
            });
            for id in sources
                .iter()
                .take(201)
                .filter_map(serde_json::Value::as_str)
            {
                targets.push(CaptureTarget {
                    kind: TargetKind::Node,
                    id,
                    selecting_thread: None,
                    source_revision: None,
                    source_hash: None,
                    raw: None,
                });
            }
            RetainedRoute::SearchReferences
        }
        Command::ShellRead(a) => {
            let cursor = value
                .get("cursor")?
                .as_str()
                .and_then(|s| crate::control::output::OutputCursor::decode(s).ok())?;
            if cursor.execution_id != a.execution_id {
                return None;
            }
            raw = CapturedRawFacts {
                generation: Some(cursor.retention_generation),
                saved_sequence: cursor.eof_flushed.then_some(cursor.sequence),
                cursor_position: Some((cursor.sequence, cursor.byte_offset)),
                encoding: Some(Arc::from(cursor.encoding)),
                lifecycle: None,
            };
            targets.push(CaptureTarget {
                kind: TargetKind::Execution,
                id: &a.execution_id,
                selecting_thread: None,
                source_revision: None,
                source_hash: None,
                raw: Some(&raw),
            });
            RetainedRoute::ShellPage
        }
        Command::Wait(a) => {
            if !value
                .get("snapshot")
                .is_some_and(serde_json::Value::is_object)
            {
                return None;
            }
            for condition in a.conditions.iter().take(201) {
                let found = match condition {
                    crate::protocol::WaitCondition::RunEnded { run_id } => {
                        Some((TargetKind::Run, run_id.as_str()))
                    }
                    crate::protocol::WaitCondition::TurnCompleted { turn_id } => {
                        Some((TargetKind::Turn, turn_id.as_str()))
                    }
                    crate::protocol::WaitCondition::ShellEnded { execution_id }
                    | crate::protocol::WaitCondition::ShellOutput { execution_id, .. } => {
                        Some((TargetKind::Execution, execution_id.as_str()))
                    }
                    crate::protocol::WaitCondition::ThreadSettled { thread_ids, .. } => {
                        for id in thread_ids
                            .iter()
                            .take(MAX_TARGETS + 1 - targets.len().min(MAX_TARGETS + 1))
                        {
                            targets.push(CaptureTarget {
                                kind: TargetKind::Thread,
                                id,
                                selecting_thread: None,
                                source_revision: None,
                                source_hash: None,
                                raw: None,
                            });
                        }
                        None
                    }
                };
                if let Some((kind, id)) = found {
                    if targets.len() < MAX_TARGETS + 1 {
                        targets.push(CaptureTarget {
                            kind,
                            id,
                            selecting_thread: None,
                            source_revision: None,
                            source_hash: None,
                            raw: None,
                        });
                    }
                }
            }
            RetainedRoute::CompletedWait
        }
        _ => return None,
    };
    let identity = uuid::Uuid::new_v4().to_string();
    let mut scope = capture_retained_scope(CaptureInput {
        result_identity: &identity,
        route,
        targets: &targets,
    })
    .ok()?;
    let total = match command {
        Command::HistoryRelations(_) => 1 + value["items"].as_array().map_or(0, Vec::len),
        Command::HistorySearch(_) => value["items"].as_array().map_or(0, Vec::len),
        Command::HistoryGet(_) => 1 + value["source_node_ids"].as_array().map_or(0, Vec::len),
        Command::Wait(a) => a
            .conditions
            .iter()
            .map(|c| match c {
                crate::protocol::WaitCondition::ThreadSettled { thread_ids, .. } => {
                    thread_ids.len()
                }
                _ => 1,
            })
            .sum(),
        _ => 1,
    };
    scope.unobserved_targets += total.saturating_sub(targets.len());
    scope.captured_at = Arc::from(chrono::Utc::now().to_rfc3339());
    if let Some(bytes) =
        command_bytes(command).filter(|n| scope.metadata_bytes + *n <= MAX_METADATA_BYTES)
    {
        scope.metadata_bytes += bytes;
        scope.original_request = Some(Arc::new(command.clone()));
    } else {
        scope.unobserved_targets += scope.targets.len();
        scope.targets.clear();
    }
    Some(crate::ui::InspectionTarget::RetainedSources(Box::new(
        scope,
    )))
}
pub(crate) fn prepare_inspection(
    raw: OwnedRetainedRaw,
) -> Result<serde_json::Value, crate::protocol::ApiError> {
    use serde_json::json;
    let prepared = prepare(raw)?;
    let changes = prepare_retained_facts(
        &prepared.scope,
        &prepared.scope.result_identity,
        Some(&prepared.current_targets),
    )
    .ok_or_else(|| {
        crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::InvalidState,
            "retained identity changed",
        )
    })?;
    let rows=prepared.scope.targets.iter().zip(changes).map(|(target,f)|{
        let mut row=json!({"target_kind":format!("{:?}",target.kind),"presence":match f.presence{Presence::Present=>"present",Presence::Missing=>"unavailable",Presence::Unknown=>"unknown"}});
        match target.kind {
            TargetKind::Node=>{
                row["selected"]=json!(match f.selection{Selection::Selected=>Some(true),Selection::NotSelected=>Some(false),Selection::Unknown=>None});
                row["source_changed"]=json!(f.source_changed);row["body_available"]=json!(f.body_retained);
                row["body_scope"]=json!("saved node payload; raw search-span byte retention is not inferred");
                row["source_revision"]=json!(f.current_revision);row["body_hash"]=json!(f.current_hash.as_deref());
            }
            TargetKind::Execution=>{
                row["generation"]=json!(f.current_generation);row["saved_sequence"]=json!(f.current_sequence);row["encoding"]=json!(f.current_encoding.as_deref());
                row["generation_changed"]=json!(f.generation_changed);row["output_grew"]=json!(f.output_grew);row["encoding_changed"]=json!(f.encoding_changed);
                row["lifecycle_changed"]=json!(f.lifecycle_changed);row["raw_tombstone"]=json!(f.tombstone);row["cursor_chunk_retained"]=json!(f.cursor_chunk_retained);
                row["cursor_chunk_length"]=json!(f.cursor_chunk_length);row["oldest_sequence"]=json!(f.oldest_sequence);
            }
            _=>{}
        }
        if target.kind!=TargetKind::Node {row["current_state"]=json!(f.current_lifecycle.as_deref());}
        row[match target.kind{TargetKind::Node=>"node_id",TargetKind::Execution=>"execution_id",TargetKind::Run=>"run_id",TargetKind::Turn=>"turn_id",TargetKind::Thread=>"thread_id"}]=json!(target.id.as_ref());row
    }).collect::<Vec<_>>();
    let value = json!({"result_identity":prepared.scope.result_identity.as_ref(),"captured_result_at":prepared.scope.captured_at.as_ref(),"route":format!("{:?}",prepared.scope.route),
        "sources":rows,"unobserved_targets":prepared.scope.unobserved_targets,"captured_scope_complete":prepared.scope.unobserved_targets==0,
        "request_metadata_available":prepared.scope.original_request.is_some(),"limited_fields":prepared.limited_fields,"sql_statements":prepared.leaf_sql_count,
        "query_membership":"not rerun; repeat explicitly for new relation/search matches",
        "cursor_authorization":"not performed; original cursor preserved, output growth alone does not expire it",
        "notice":if prepared.scope.original_request.is_some(){"Current metadata only. Original bytes, outcome, request filters and cursor remain retained; missing/unknown does not prove deletion."}else{"Original result remains retained. Request metadata exceeded capture limits or was unavailable; re-enter those filters explicitly. Missing/unknown does not prove deletion."}});
    if serde_json::to_vec(&value)
        .map_err(|_| {
            crate::protocol::ApiError::new(
                crate::protocol::ErrorCode::InvalidState,
                "cannot prepare retained facts",
            )
        })?
        .len()
        > MAX_METADATA_BYTES
    {
        return Err(crate::protocol::ApiError::new(
            crate::protocol::ErrorCode::InputLimit,
            "retained prepared metadata exceeds channel",
        ));
    }
    Ok(value)
}

/// Add bounded original request facts once, before management takes immutable
/// detail ownership. Metadata refresh never serializes/replaces that body.
pub(super) fn decorate_original(
    value: &mut serde_json::Value,
    target: &Option<crate::ui::InspectionTarget>,
) {
    if let Some(crate::ui::InspectionTarget::RetainedSources(scope)) = target
        && let Some(object) = value.as_object_mut()
    {
        if let Some(request) = &scope.original_request {
            if let Ok(request) = serde_json::to_value(request.as_ref()) {
                object.insert("captured_request".into(), request);
            }
        }
        object.insert(
            "captured_result_identity".into(),
            serde_json::Value::String(scope.result_identity.to_string()),
        );
        object.insert(
            "captured_result_at".into(),
            serde_json::Value::String(scope.captured_at.to_string()),
        );
    }
}

pub(super) fn request_fits(command: &crate::protocol::Command) -> bool {
    command_bytes(command).is_some()
}
pub(super) fn route(command: &crate::protocol::Command) -> RetainedRoute {
    match command {
        crate::protocol::Command::HistoryRelations(_) => RetainedRoute::Relations,
        crate::protocol::Command::ShellRead(_) => RetainedRoute::ShellPage,
        crate::protocol::Command::Wait(_) => RetainedRoute::CompletedWait,
        _ => RetainedRoute::SearchReferences,
    }
}
pub(super) fn limited_scope(route: RetainedRoute) -> crate::ui::InspectionTarget {
    let identity = uuid::Uuid::new_v4().to_string();
    let mut scope = capture_retained_scope(CaptureInput {
        result_identity: &identity,
        route,
        targets: &[],
    })
    .expect("generated identity is bounded");
    scope.unobserved_targets = 1;
    scope.captured_at = Arc::from(chrono::Utc::now().to_rfc3339());
    crate::ui::InspectionTarget::RetainedSources(Box::new(scope))
}

#[cfg(test)]
mod registered_reader_tests {
    use super::*;
    use crate::protocol::*;
    use serde_json::json;
    fn relation() -> Command {
        Command::HistoryRelations(HistoryRelationsArgs {
            node_id: "n".into(),
            relation: HistoryRelation::Next,
            limit: 50,
            cursor: Some("original filter cursor".into()),
        })
    }
    fn scope(target: crate::ui::InspectionTarget) -> Box<RetainedScope> {
        let crate::ui::InspectionTarget::RetainedSources(s) = target else {
            panic!("retained target")
        };
        s
    }
    #[test]
    fn all_four_routes_capture_once_keep_original_members_and_repeat_identity() {
        let cursor = crate::control::output::OutputCursor {
            version: 1,
            execution_id: "e".into(),
            sequence: 1,
            byte_offset: 0,
            retention_generation: 2,
            encoding: "UTF-8".into(),
            streams: crate::control::output::StreamSelection::Both,
            eof_flushed: false,
        }
        .encode()
        .unwrap();
        let cases = vec![
            (
                relation(),
                json!({"items":[{"node_id":"other","source_revision":7}],"next_cursor":"original next"}),
            ),
            (
                Command::HistorySearch(HistorySearchArgs {
                    query: "original query".into(),
                    thread_id: Some("literal".into()),
                    ..Default::default()
                }),
                json!({"items":[{"node_id":"n","excerpt":"original excerpt","provenance":{"source_revision":7,"origin_thread_id":"not-a-selection"}}]}),
            ),
            (
                Command::ShellRead(ShellReadArgs {
                    execution_id: "e".into(),
                    cursor: None,
                    encoding: None,
                    limit_bytes: 1024,
                }),
                json!({"execution_id":"e","cursor":cursor,"encoding":"UTF-8","text":"original page","raw_bytes":13,"output_complete":false}),
            ),
            (
                Command::Wait(WaitArgs {
                    conditions: vec![WaitCondition::RunEnded { run_id: "r".into() }],
                    timeout_ms: 1,
                    after: None,
                }),
                json!({"status":"satisfied","matched":[{"type":"run_ended","run_id":"r"}],"snapshot":{"conditions":[{"type":"run","run_id":"r","state":"completed"}]}}),
            ),
        ];
        for (command, value) in cases {
            let target = capture_response(&command, &value).unwrap();
            let captured = scope(target.clone());
            assert_eq!(captured.original_request.as_deref(), Some(&command));
            let mut decorated = value.clone();
            decorate_original(&mut decorated, &Some(target));
            for (key, member) in value.as_object().unwrap() {
                assert_eq!(&decorated[key], member);
            }
            assert_eq!(
                decorated["captured_request"],
                serde_json::to_value(&command).unwrap()
            );
            let repeated = scope(capture_response(&command, &value).unwrap());
            assert_ne!(captured.result_identity, repeated.result_identity);
            if captured.route == RetainedRoute::ShellPage {
                assert!(
                    captured.targets[0]
                        .raw
                        .as_ref()
                        .unwrap()
                        .saved_sequence
                        .is_none(),
                    "a page cursor is not a saved high-water sequence"
                );
            }
        }
    }
    #[test]
    fn mixed_source_reader_is_query_only_four_statements_and_prepares_after_close() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
          INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');
          INSERT INTO node(id,kind,created_at,payload_json,payload_revision,body_hash)VALUES('n','user','now','{}',7,'original hash');INSERT INTO thread_path VALUES('t',0,'n');
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,retention_generation,started_at)VALUES('e','r','u','op','running','/','{}','UTF-8','UTF-8','closed',1,2,'now');
          INSERT INTO output_chunk VALUES('e',0,'stdout',0,x'616263','now');").unwrap();
        db.execute(
            "UPDATE node SET payload_json=?1 WHERE id='n'",
            [json!({"opaque":"x".repeat(1024*1024)}).to_string()],
        )
        .unwrap();
        let raw_capture = CapturedRawFacts {
            generation: Some(2),
            saved_sequence: Some(0),
            cursor_position: Some((0, 0)),
            encoding: Some(Arc::from("UTF-8")),
            lifecycle: Some(Arc::from("running")),
        };
        let targets = [
            CaptureTarget {
                kind: TargetKind::Node,
                id: "n",
                selecting_thread: Some("t"),
                source_revision: Some(7),
                source_hash: Some("original hash"),
                raw: None,
            },
            CaptureTarget {
                kind: TargetKind::Execution,
                id: "e",
                selecting_thread: None,
                source_revision: None,
                source_hash: None,
                raw: Some(&raw_capture),
            },
            CaptureTarget {
                kind: TargetKind::Run,
                id: "r",
                selecting_thread: None,
                source_revision: None,
                source_hash: None,
                raw: None,
            },
            CaptureTarget {
                kind: TargetKind::Turn,
                id: "u",
                selecting_thread: None,
                source_revision: None,
                source_hash: None,
                raw: None,
            },
        ];
        let captured = capture_retained_scope(CaptureInput {
            result_identity: "mixed",
            route: RetainedRoute::CompletedWait,
            targets: &targets,
        })
        .unwrap();
        db.pragma_update(None, "query_only", true).unwrap();
        let changes = db.total_changes();
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let raw = read_retained_source_facts_tx(&db, &captured).unwrap();
        assert_eq!(trace.count(), 4);
        assert_eq!(db.total_changes(), changes);
        drop(trace);
        drop(db);
        let value = prepare_inspection(raw).unwrap();
        assert_eq!(value["sql_statements"], 4);
        assert_eq!(value["sources"][0]["selected"], true);
        assert_eq!(value["sources"][0]["body_available"], true);
        assert_eq!(value["sources"][0]["source_changed"], false);
        assert_eq!(value["sources"][1]["cursor_chunk_length"], 3);
        assert_eq!(value["sources"][1]["generation_changed"], false);
        assert!(serde_json::to_vec(&value).unwrap().len() <= MAX_METADATA_BYTES);
    }
    #[test]
    fn capture_limits_are_explicit_and_do_not_clone_an_oversized_query() {
        let command = Command::HistorySearch(HistorySearchArgs {
            query: "q".repeat(1024 * 1024),
            ..Default::default()
        });
        assert!(matches!(
            super::super::inspection_history::capture_command(&command),
            Some(super::super::inspection_history::CapturedCommand::Limited(
                _
            ))
        ));
        let rows = (0..300)
            .map(|i| json!({"node_id":format!("n-{i}")}))
            .collect::<Vec<_>>();
        let captured = scope(capture_response(&relation(), &json!({"items":rows})).unwrap());
        assert_eq!(captured.targets.len(), 200);
        assert_eq!(captured.unobserved_targets, 101);
        let command = Command::HistorySearch(HistorySearchArgs {
            query: "q".into(),
            ..Default::default()
        });
        let captured = scope(
            capture_response(
                &command,
                &json!({"items":[{"node_id":"n","provenance":{"origin_thread_id":"creation"}}]}),
            )
            .unwrap(),
        );
        assert!(captured.targets[0].selecting_thread.is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn existing_admission_has_no_quiet_hidden_or_cancelled_read_and_one_inflight() {
        use super::super::{
            inspection_read::{Dependencies, PreparedInspection},
            inspection_state::{Completion, Observation},
        };
        use crate::ui::{InspectionTarget, ManagementContext};
        let target = capture_response(&relation(), &json!({"items":[]})).unwrap();
        let context = ManagementContext {
            epoch: 1,
            owner_draft: "draft".into(),
            owner_thread: Some("viewer".into()),
        };
        let mut state = Observation::default();
        state.install(1, context.clone(), target.clone(), None);
        state.observe_at(&context, &target, true, 1);
        let ticket = state.take_metadata(tokio::time::Instant::now()).unwrap();
        for _ in 0..20 {
            state.changed("foreign");
        }
        assert!(state.take_metadata(tokio::time::Instant::now()).is_none());
        let prepared = || PreparedInspection {
            target: target.clone(),
            dependencies: Dependencies {
                thread: String::new(),
                parent_thread: None,
            },
            value: json!({"current":"facts"}),
            observed_at: "snapshot".into(),
            limited_fields: vec![],
        };
        assert!(matches!(
            state.complete_metadata(&ticket, 1, Ok(prepared())),
            Completion::Updated
        ));
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        let next = state.take_metadata(tokio::time::Instant::now()).unwrap();
        assert!(matches!(
            state.complete_metadata(&next, 2, Ok(prepared())),
            Completion::Updated
        ));
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        assert!(state.take_metadata(tokio::time::Instant::now()).is_none());
        state.changed("foreign");
        let late = state.take_metadata(tokio::time::Instant::now()).unwrap();
        state.hide_at(2);
        assert!(matches!(
            state.complete_metadata(&late, 3, Ok(prepared())),
            Completion::Retired
        ));
        assert!(state.take_metadata(tokio::time::Instant::now()).is_none());
        state.observe_at(&context, &target, true, 3);
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        let cancelled = state.take_metadata(tokio::time::Instant::now()).unwrap();
        cancelled.stop.cancel();
        assert!(matches!(
            state.complete_metadata(&cancelled, 4, Ok(prepared())),
            Completion::Retired
        ));
        assert!(matches!(target, InspectionTarget::RetainedSources(_)));
    }
}
