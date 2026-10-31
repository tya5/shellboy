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
    pub(crate) encoding: Arc<str>,
    pub(crate) lifecycle: Arc<str>,
}
pub(crate) struct CaptureTarget<'a> {
    pub(crate) kind: TargetKind,
    pub(crate) id: &'a str,
    // Only the original request's explicit selection, never a viewer/provenance.
    pub(crate) selecting_thread: Option<&'a str>,
    pub(crate) source_revision: Option<u64>,
    pub(crate) source_hash: Option<[u8;32]>,
    pub(crate) raw: Option<&'a CapturedRawFacts>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExactTarget {
    pub(crate) kind: TargetKind,
    pub(crate) id: Arc<str>,
    pub(crate) selecting_thread: Option<Arc<str>>,
    pub(crate) source_revision: Option<u64>,
    pub(crate) source_hash: Option<[u8;32]>,
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
        let bytes = t.id.len()+t.selecting_thread.map(str::len).unwrap_or(0)+raw_bytes+32;
        if !id_ok(t.id) || t.selecting_thread.is_some_and(|s|!id_ok(s))
            || t.raw.is_some_and(|r|r.encoding.len()>128 || r.lifecycle.len()>32)
            || metadata_bytes.checked_add(bytes).is_none_or(|n|n>MAX_METADATA_BYTES)
        { unobserved_targets += 1; continue; }
        metadata_bytes += bytes;
        targets.push(ExactTarget { kind:t.kind,id:Arc::from(t.id),selecting_thread:t.selecting_thread.map(Arc::from),
            source_revision:t.source_revision,source_hash:t.source_hash,raw:t.raw.cloned() });
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
}
// G's adapter maps existing history::observation and retained raw metadata into
// these scalar facts. It must not issue search, raw-output or wait operations.
pub(crate) struct CurrentTargetFacts {
    pub(crate) target_index: usize,
    pub(crate) presence: Presence,
    pub(crate) selection: Selection,
    pub(crate) source_revision: Option<u64>,
    pub(crate) source_hash: Option<[u8;32]>,
    pub(crate) body_retained: Option<bool>,
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
    pub(crate) tombstone: Option<bool>,
    pub(crate) cursor_chunk_retained: Option<bool>,
}
fn unknown_target() -> PreparedTarget {
    PreparedTarget { presence:Presence::Unknown,selection:Selection::Unknown,source_changed:None,body_retained:None,
        generation_changed:None,output_grew:None,encoding_changed:None,lifecycle_changed:None,tombstone:None,cursor_chunk_retained:None }
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
        let changed_hash=original.source_hash.zip(f.source_hash).map(|(a,b)|a!=b);
        let changed_revision=original.source_revision.zip(f.source_revision).map(|(a,b)|a!=b);
        let mut p=PreparedTarget { presence:f.presence,
            selection:if original.selecting_thread.is_some(){f.selection}else{Selection::Unknown},
            source_changed:match (changed_hash,changed_revision) { (Some(a),Some(b))=>Some(a||b),(a,b)=>a.or(b) },
            body_retained:f.body_retained,..unknown_target() };
        if let (Some(before),Some(now))=(&original.raw,&f.raw) {
            p.generation_changed=now.generation.map(|v|v!=before.generation);
            p.output_grew=now.saved_sequence.map(|v|v>before.saved_sequence);
            p.encoding_changed=now.encoding.as_ref().map(|v|v!=&before.encoding);
            p.lifecycle_changed=now.lifecycle.as_ref().map(|v|v!=&before.lifecycle);
            p.tombstone=now.tombstone;p.cursor_chunk_retained=now.cursor_chunk_retained;
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
        CaptureTarget { kind:TargetKind::Node,id,selecting_thread:thread,source_revision:Some(1),source_hash:Some([1;32]),raw:None }
    }
    fn fact(presence:Presence)->CurrentTargetFacts {
        CurrentTargetFacts { target_index:0,presence,selection:Selection::NotSelected,source_revision:Some(2),source_hash:Some([2;32]),body_retained:Some(false),raw:None }
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
        let raw=CapturedRawFacts{generation:0,saved_sequence:0,encoding:Arc::from("x".repeat(MAX_METADATA_BYTES+1)),lifecycle:Arc::from("running")};
        let t=CaptureTarget{kind:TargetKind::Execution,id:"e",selecting_thread:None,source_revision:None,source_hash:None,raw:Some(&raw)};
        let scope=capture(RetainedRoute::ShellPage,&[t]);assert!(scope.targets.is_empty());assert_eq!(scope.unobserved_targets,1);
        assert!(scope.metadata_bytes<=MAX_METADATA_BYTES);
    }
    #[test]
    fn raw_growth_lifecycle_and_retention_are_separate_not_cursor_authorization() {
        let before=CapturedRawFacts{generation:1,saved_sequence:4,encoding:Arc::from("UTF-8"),lifecycle:Arc::from("running")};
        let t=CaptureTarget{kind:TargetKind::Execution,id:"e",selecting_thread:None,source_revision:None,source_hash:None,raw:Some(&before)};
        let scope=capture(RetainedRoute::ShellPage,&[t]);let mut f=fact(Presence::Present);
        f.raw=Some(CurrentRawFacts{generation:Some(1),saved_sequence:Some(9),encoding:Some(Arc::from("UTF-8")),lifecycle:Some(Arc::from("exited")),tombstone:Some(false),cursor_chunk_retained:Some(true)});
        let p=prepare_retained_facts(&scope,"original",Some(&[f])).unwrap();assert_eq!(p[0].output_grew,Some(true));assert_eq!(p[0].generation_changed,Some(false));assert_eq!(p[0].lifecycle_changed,Some(true));
        assert_eq!(scope.targets[0].raw.as_ref().unwrap().saved_sequence,4);
    }
}
