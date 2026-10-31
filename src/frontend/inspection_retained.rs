//! Pure retained-scope capture and comparison. G owns observation scheduling,
//! history::observation mapping and the thin DB/cursor-authority adapter.
//! Original bytes, cursors and capture time remain solely in G management.detail.
use std::sync::Arc;

pub(crate) const MAX_TARGETS: usize = 200;
pub(crate) const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetainedRoute { Relations, SearchReferences, ShellPage, CompletedWait }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetKind { Node, Execution, Run, Turn }
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedRawFacts {
    pub(crate) generation: u64,
    pub(crate) saved_sequence: u64,
    pub(crate) cursor_position: Option<(u64,u64)>,
    pub(crate) encoding: Arc<str>,
    pub(crate) lifecycle: Arc<str>,
}
pub(crate) struct CaptureTarget<'a> {
    pub(crate) kind: TargetKind,
    pub(crate) id: &'a str,
    // Only the original request's explicit selection, never a viewer/provenance.
    pub(crate) selecting_thread: Option<&'a str>,
    pub(crate) source_revision: Option<u64>,
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
pub(crate) struct RetainedScope {
    pub(crate) result_identity: Arc<str>,
    pub(crate) route: RetainedRoute,
    pub(crate) targets: Vec<ExactTarget>,
    pub(crate) unobserved_targets: usize,
    pub(crate) metadata_bytes: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CaptureLimit { InvalidIdentity }
pub(crate) struct CaptureInput<'a> {
    pub(crate) result_identity: &'a str,
    pub(crate) route: RetainedRoute,
    // Original bytes/cursor/capture time stay in G management.detail, not here.
    pub(crate) targets: &'a [CaptureTarget<'a>],
}
fn id_ok(id: &str) -> bool { !id.is_empty() && id.len() <= MAX_ID_BYTES }

pub(crate) fn capture_retained_scope(input: CaptureInput<'_>) -> Result<RetainedScope, CaptureLimit> {
    if !id_ok(input.result_identity) { return Err(CaptureLimit::InvalidIdentity); }
    let mut metadata_bytes = input.result_identity.len();
    let mut targets = Vec::with_capacity(input.targets.len().min(MAX_TARGETS));
    let mut unobserved_targets = input.targets.len().saturating_sub(MAX_TARGETS);
    for t in input.targets.iter().take(MAX_TARGETS) {
        let raw_bytes = t.raw.map(|r| r.encoding.len()+r.lifecycle.len()).unwrap_or(0);
        let bytes = t.id.len()+t.selecting_thread.map(str::len).unwrap_or(0)+raw_bytes+t.source_hash.map(str::len).unwrap_or(0);
        if !id_ok(t.id) || t.selecting_thread.is_some_and(|s|!id_ok(s))
            || t.source_hash.is_some_and(|h|h.len()>128)
            || t.raw.is_some_and(|r|r.encoding.len()>128 || r.lifecycle.len()>32)
            || metadata_bytes.checked_add(bytes).is_none_or(|n|n>MAX_METADATA_BYTES)
        { unobserved_targets += 1; continue; }
        metadata_bytes += bytes;
        targets.push(ExactTarget { kind:t.kind,id:Arc::from(t.id),selecting_thread:t.selecting_thread.map(Arc::from),
            source_revision:t.source_revision,source_hash:t.source_hash.map(Arc::from),raw:t.raw.cloned() });
    }
    Ok(RetainedScope { result_identity:Arc::from(input.result_identity),route:input.route,
targets,unobserved_targets,metadata_bytes })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presence { Present, Missing, Unknown }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection { Selected, NotSelected, Unknown }
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
    pub(crate) tombstone: Option<bool>,
    pub(crate) cursor_chunk_retained: Option<bool>,
    pub(crate) cursor_chunk_length: Option<u64>,
    pub(crate) oldest_sequence: Option<u64>,
}
fn unknown_target() -> PreparedTarget {
    PreparedTarget { presence:Presence::Unknown,selection:Selection::Unknown,source_changed:None,body_retained:None,
        generation_changed:None,output_grew:None,encoding_changed:None,lifecycle_changed:None,current_lifecycle:None,tombstone:None,cursor_chunk_retained:None,cursor_chunk_length:None,oldest_sequence:None }
}
// None facts means a failed/limited observation, not a successful empty result.
// Adoption/generation gates belong to G; identity mismatch cannot adopt anything.
pub(crate) fn prepare_retained_facts(
    scope: &RetainedScope, result_identity: &str, facts: Option<&[CurrentTargetFacts]>,
) -> Option<Vec<PreparedTarget>> {
    if scope.result_identity.as_ref()!=result_identity { return None; }
    let mut prepared=vec![unknown_target();scope.targets.len()];
    let Some(facts)=facts else { return Some(prepared); };
    if facts.len()>MAX_TARGETS { return Some(prepared); }
    let mut seen=[false;MAX_TARGETS];
    for f in facts {
        let Some(original)=scope.targets.get(f.target_index) else { continue; };
        if seen[f.target_index] { prepared[f.target_index]=unknown_target();continue; }
        seen[f.target_index]=true;
        let changed_hash=original.source_hash.as_ref().zip(f.source_hash.as_ref()).map(|(a,b)|a!=b);
        let changed_revision=original.source_revision.zip(f.source_revision).map(|(a,b)|a!=b);
        let mut p=PreparedTarget { presence:f.presence,
            selection:if original.selecting_thread.is_some(){f.selection}else{Selection::Unknown},
            source_changed:match (changed_hash,changed_revision) { (Some(a),Some(b))=>Some(a||b),(a,b)=>a.or(b) },
            body_retained:f.body_retained,current_lifecycle:f.lifecycle.clone(),..unknown_target() };
        if let (Some(before),Some(now))=(&original.raw,&f.raw) {
            p.generation_changed=now.generation.map(|v|v!=before.generation);
            p.output_grew=now.saved_sequence.map(|v|v>before.saved_sequence);
            p.encoding_changed=now.encoding.as_ref().map(|v|v!=&before.encoding);
            p.lifecycle_changed=now.lifecycle.as_ref().map(|v|v!=&before.lifecycle);
            p.current_lifecycle=now.lifecycle.clone();
            p.tombstone=now.tombstone;p.cursor_chunk_retained=now.cursor_chunk_retained;
            p.cursor_chunk_length=now.cursor_chunk_length;p.oldest_sequence=now.oldest_sequence;
        }
        prepared[f.target_index]=p;
    }
    Some(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(route:RetainedRoute,targets:&[CaptureTarget<'_>])->RetainedScope {
        capture_retained_scope(CaptureInput { result_identity:"original",route,targets }).unwrap()
    }
    fn target<'a>(id:&'a str,thread:Option<&'a str>)->CaptureTarget<'a> {
        CaptureTarget { kind:TargetKind::Node,id,selecting_thread:thread,source_revision:Some(1),source_hash:Some("captured opaque hash"),raw:None }
    }
    fn fact(presence:Presence)->CurrentTargetFacts {
        CurrentTargetFacts { target_index:0,presence,selection:Selection::NotSelected,source_revision:Some(2),source_hash:Some(Arc::from("current opaque hash")),body_retained:Some(false),lifecycle:None,raw:None }
    }
    #[test]
    fn all_routes_preserve_original_data_while_current_selection_and_sources_change() {
        for route in [RetainedRoute::Relations,RetainedRoute::SearchReferences,RetainedRoute::ShellPage,RetainedRoute::CompletedWait] {
            let scope=capture(route,&[target("node",Some("original-thread"))]);
            let original=scope.targets.clone();let facts=prepare_retained_facts(&scope,"original",Some(&[fact(Presence::Missing)])).unwrap();
            assert_eq!(facts[0].presence,Presence::Missing);assert_eq!(facts[0].selection,Selection::NotSelected);assert_eq!(facts[0].source_changed,Some(true));
            assert_eq!(original,scope.targets);
            assert_eq!(scope.result_identity.as_ref(),"original");
        }
    }
    #[test]
    fn no_original_selection_is_unknown_and_failed_observation_is_not_deletion() {
        let scope=capture(RetainedRoute::SearchReferences,&[target("node",None)]);
        let p=prepare_retained_facts(&scope,"original",Some(&[fact(Presence::Present)])).unwrap();assert_eq!(p[0].selection,Selection::Unknown);
        let p=prepare_retained_facts(&scope,"original",None).unwrap();assert_eq!(p[0].presence,Presence::Unknown);assert_eq!(p[0].body_retained,None);
        assert!(prepare_retained_facts(&scope,"new explicit result",None).is_none());
    }
    #[test]
    fn capture_observation_bound_and_metadata_are_checked_before_copy() {
        let targets=(0..201).map(|_|target("node",None)).collect::<Vec<_>>();
        let scope=capture(RetainedRoute::Relations,&targets);assert_eq!(scope.targets.len(),200);assert_eq!(scope.unobserved_targets,1);assert!(scope.metadata_bytes<=MAX_METADATA_BYTES);
        let id="x".repeat(129);let scope=capture(RetainedRoute::Relations,&[target(&id,None)]);assert!(scope.targets.is_empty());assert_eq!(scope.unobserved_targets,1);
        let raw=CapturedRawFacts{generation:0,saved_sequence:0,cursor_position:None,encoding:Arc::from("x".repeat(MAX_METADATA_BYTES+1)),lifecycle:Arc::from("running")};
        let t=CaptureTarget{kind:TargetKind::Execution,id:"e",selecting_thread:None,source_revision:None,source_hash:None,raw:Some(&raw)};
        let scope=capture(RetainedRoute::ShellPage,&[t]);assert!(scope.targets.is_empty());assert_eq!(scope.unobserved_targets,1);
        assert!(scope.metadata_bytes<=MAX_METADATA_BYTES);
    }
    #[test]
    fn raw_growth_lifecycle_and_retention_are_separate_not_cursor_authorization() {
        let before=CapturedRawFacts{generation:1,saved_sequence:4,cursor_position:Some((2,0)),encoding:Arc::from("UTF-8"),lifecycle:Arc::from("running")};
        let t=CaptureTarget{kind:TargetKind::Execution,id:"e",selecting_thread:None,source_revision:None,source_hash:None,raw:Some(&before)};
        let scope=capture(RetainedRoute::ShellPage,&[t]);let mut f=fact(Presence::Present);
        f.raw=Some(CurrentRawFacts{generation:Some(1),saved_sequence:Some(9),encoding:Some(Arc::from("UTF-8")),lifecycle:Some(Arc::from("exited")),tombstone:Some(false),cursor_chunk_retained:Some(true),cursor_chunk_length:Some(3),oldest_sequence:Some(0)});
        let p=prepare_retained_facts(&scope,"original",Some(&[f])).unwrap();assert_eq!(p[0].output_grew,Some(true));assert_eq!(p[0].generation_changed,Some(false));assert_eq!(p[0].lifecycle_changed,Some(true));
        assert_eq!(scope.targets[0].raw.as_ref().unwrap().saved_sequence,4);
    }
}

// Thin integration request, not a second history observer. G must execute this
// through history::observation::read_tx on the SAME caller-provided connection,
// and map its prepared result before final prepare_retained_facts adoption.
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
    pub(crate) history_request: Option<HistoryObservationRequest>,
    executions: Vec<(usize, crate::history::raw_witness::RetainedExecutionFacts)>,
    lifecycles: Vec<OwnedLifecycleRaw>,
    pub(crate) leaf_sql_count: usize,
}
const LIFECYCLE_SQL: &str = "WITH requested AS (
    SELECT CAST(key AS INTEGER) ordinal,json_extract(value,'$.kind') kind,json_extract(value,'$.id') id FROM json_each(?1)
    ) SELECT q.ordinal,COALESCE(r.id,u.id) IS NOT NULL,
       CASE WHEN length(CAST(COALESCE(r.state,u.state) AS BLOB))<=32 THEN COALESCE(r.state,u.state) END,
       COALESCE(length(CAST(COALESCE(r.state,u.state) AS BLOB))>32,0)
    FROM requested q LEFT JOIN run r ON q.kind='run' AND r.id=q.id
    LEFT JOIN turn u ON q.kind='turn' AND u.id=q.id ORDER BY q.ordinal";

pub(crate) fn read_retained_source_facts_tx(
    db: &rusqlite::Connection, scope: &RetainedScope,
) -> Result<OwnedRetainedRaw, crate::storage::StorageError> {
    use crate::history::raw_witness::{RawWitness, RetainedExecutionRequest};
    if scope.targets.len()>MAX_TARGETS || scope.metadata_bytes>MAX_METADATA_BYTES {
        return Err(crate::storage::StorageError::InvalidInput("retained observation scope exceeds bounds".into()));
    }
    let mut ids=Vec::new();let mut thread_id:Option<String>=None;let mut mixed=false;
    let mut raw_indices=Vec::new();let mut raw_requests=Vec::new();let mut lifecycle_indices=Vec::new();let mut lifecycle_requests=Vec::new();
    for (index,t) in scope.targets.iter().enumerate() {
        if !id_ok(&t.id) { return Err(crate::storage::StorageError::InvalidInput("retained target ID exceeds scalar bound".into())); }
        match t.kind {
            TargetKind::Node => {
                ids.push(t.id.to_string());
                match &t.selecting_thread {
                    Some(thread) if thread_id.as_deref().is_none_or(|id|id==thread.as_ref()) => { thread_id=Some(thread.to_string()); }
                    _ => { mixed=true; }
                }
            }
            TargetKind::Execution => { raw_indices.push(index);raw_requests.push(RetainedExecutionRequest { execution:t.id.to_string(),cursor_sequence:t.raw.as_ref().and_then(|r|r.cursor_position.map(|(sequence,_)|sequence)) }); }
            TargetKind::Run|TargetKind::Turn => { lifecycle_indices.push(index);lifecycle_requests.push(serde_json::json!({"id":t.id.as_ref(),"kind":if t.kind==TargetKind::Run{"run"}else{"turn"}})); }
        }
    }
    if mixed { thread_id=None; }
    let history_request=(!ids.is_empty()).then_some(HistoryObservationRequest { ids,thread_id });
    let mut leaf_sql_count=0;
    let executions=if raw_requests.is_empty() { Vec::new() } else {
        leaf_sql_count+=1;
        raw_indices.into_iter().zip(RawWitness::for_retained_execution(db,&raw_requests)?).collect()
    };
    let lifecycles=if lifecycle_requests.is_empty() { Vec::new() } else {
        leaf_sql_count+=1;
        let parameter=serde_json::to_string(&lifecycle_requests).map_err(|e|crate::storage::StorageError::InvalidInput(e.to_string()))?;
        db.prepare(LIFECYCLE_SQL)?.query_map([parameter],|r| {
            let ordinal=r.get::<_,usize>(0)?;
            Ok(OwnedLifecycleRaw { target_index:*lifecycle_indices.get(ordinal).ok_or(rusqlite::Error::InvalidQuery)?,exists:r.get(1)?,state:r.get(2)?,metadata_limited:r.get(3)? })
        })?.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(OwnedRetainedRaw { scope:scope.clone(),history_request,executions,lifecycles,leaf_sql_count })
}
pub(crate) struct PreparedRetained {
    pub(crate) scope: RetainedScope,
    // Node facts remain explicitly pending G's existing history adapter. They
    // are not a successful empty history response or a deletion inference.
    pub(crate) history_request: Option<HistoryObservationRequest>,
    pub(crate) current_targets: Vec<CurrentTargetFacts>,
    pub(crate) limited_fields: Vec<String>,
    pub(crate) leaf_sql_count: usize,
}
pub(crate) fn prepare(raw: OwnedRetainedRaw) -> Result<PreparedRetained, crate::protocol::ApiError> {
    let mut current_targets=Vec::with_capacity(raw.executions.len()+raw.lifecycles.len());let mut limited_fields=Vec::new();
    for (index,f) in raw.executions {
        if f.metadata_limited { limited_fields.push(format!("targets/{index}/raw")); }
        current_targets.push(CurrentTargetFacts { target_index:index,presence:if f.exists{Presence::Present}else{Presence::Missing},selection:Selection::Unknown,
            source_revision:None,source_hash:None,body_retained:None,lifecycle:f.state.as_deref().map(Arc::from),raw:Some(CurrentRawFacts { generation:f.generation.and_then(|n|u64::try_from(n).ok()),
                saved_sequence:f.saved_sequence.and_then(|n|u64::try_from(n).ok()),encoding:f.encoding.map(Arc::from),lifecycle:f.state.map(Arc::from),
                tombstone:Some(f.tombstone),cursor_chunk_retained:None,cursor_chunk_length:None,oldest_sequence:None }) });
    }
    for f in raw.lifecycles {
        if f.metadata_limited { limited_fields.push(format!("targets/{}/lifecycle",f.target_index)); }
        current_targets.push(CurrentTargetFacts { target_index:f.target_index,presence:if f.exists{Presence::Present}else{Presence::Missing},selection:Selection::Unknown,
            source_revision:None,source_hash:None,body_retained:None,lifecycle:f.state.map(Arc::from),raw:None });
    }
    Ok(PreparedRetained { scope:raw.scope,history_request:raw.history_request,current_targets,limited_fields,leaf_sql_count:raw.leaf_sql_count })
}

#[cfg(test)]
mod adapter_contract_tests {
    use super::*;
    #[test]
    fn lifecycle_read_plan_preserves_exact_ids_and_no_wait_registration() {
        let db=rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE run(id TEXT PRIMARY KEY,state TEXT);CREATE TABLE turn(id TEXT PRIMARY KEY,state TEXT);
            INSERT INTO run VALUES('r','completed');INSERT INTO turn VALUES('u','running');").unwrap();
        let requests=serde_json::json!([{"kind":"run","id":"r"},{"kind":"turn","id":"u"},{"kind":"run","id":"missing"}]).to_string();
        let rows=db.prepare(LIFECYCLE_SQL).unwrap().query_map([&requests],|r|Ok((r.get::<_,usize>(0)?,r.get::<_,bool>(1)?,r.get::<_,Option<String>>(2)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(rows,vec![(0,true,Some("completed".into())),(1,true,Some("running".into())),(2,false,None)]);
        let plan=db.prepare(&format!("EXPLAIN QUERY PLAN {LIFECYCLE_SQL}")).unwrap().query_map([&requests],|r|r.get::<_,String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap().join("\n");
        assert!(plan.contains("sqlite_autoindex_run_1"),"{plan}");assert!(plan.contains("sqlite_autoindex_turn_1"),"{plan}");
    }
}

#[cfg(test)]
mod comparison_limit_tests {
    use super::*;
    fn scope()->RetainedScope {
        capture_retained_scope(CaptureInput{result_identity:"r",route:RetainedRoute::CompletedWait,
            targets:&[CaptureTarget{kind:TargetKind::Run,id:"run",selecting_thread:None,source_revision:None,source_hash:None,raw:None}]}).unwrap()
    }
    fn current(index:usize,presence:Presence)->CurrentTargetFacts {
        CurrentTargetFacts{target_index:index,presence,selection:Selection::Unknown,source_revision:None,source_hash:None,body_retained:None,lifecycle:Some(Arc::from("completed")),raw:None}
    }
    #[test]
    fn completed_wait_current_lifecycle_does_not_replace_captured_identity() {
        let captured=scope();let p=prepare_retained_facts(&captured,"r",Some(&[current(0,Presence::Present)])).unwrap();
        assert_eq!(p[0].current_lifecycle.as_deref(),Some("completed"));assert_eq!(captured.result_identity.as_ref(),"r");
    }
    #[test]
    fn duplicate_rows_are_unknown_not_last_writer_wins() {
        let captured=scope();let p=prepare_retained_facts(&captured,"r",Some(&[current(0,Presence::Missing),current(0,Presence::Present)])).unwrap();
        assert_eq!(p[0].presence,Presence::Unknown);assert_eq!(p[0].current_lifecycle,None);
    }
    #[test]
    fn out_of_scope_target_cannot_fill_missing_witness() {
        let captured=scope();let p=prepare_retained_facts(&captured,"r",Some(&[current(1,Presence::Present)])).unwrap();
        assert_eq!(p[0].presence,Presence::Unknown);
    }
    #[test]
    fn expansion_over_200_current_facts_is_unknown_not_false_empty() {
        let captured=scope();let rows=(0..201).map(|_|current(0,Presence::Missing)).collect::<Vec<_>>();
        let p=prepare_retained_facts(&captured,"r",Some(&rows)).unwrap();assert_eq!(p[0].presence,Presence::Unknown);
    }
    #[test]
    fn explicit_repeat_must_have_its_own_retained_result_identity() {
        assert!(prepare_retained_facts(&scope(),"repeat",Some(&[current(0,Presence::Present)])).is_none());
    }
}
