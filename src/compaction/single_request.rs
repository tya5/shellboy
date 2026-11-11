//! Pure building blocks for the reviewed b822 single-request design.
//! Not yet connected to dispatch/adoption: no #A writer or settings storage.
use super::*;

pub(super) const DEFAULT_OUTPUT_CHARACTERS: u64 = 20_000;

/// Already-resolved policy. Validation/storage remain at the settings boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OutputLimit(pub(super) u64);
impl OutputLimit {
    pub(super) fn resolve(thread: Option<Self>, connection: Option<Self>) -> Self {
        thread.or(connection).unwrap_or(Self(DEFAULT_OUTPUT_CHARACTERS))
    }
}

pub(super) fn request(snapshot: &Snapshot, selected: Range<usize>, limit: OutputLimit) -> Value {
    let text = snapshot.groups[selected].iter()
        .flat_map(|group| group.sources.iter())
        .chain(snapshot.extra_sources.iter())
        .map(|source| render(source, snapshot.epoch))
        .collect::<Vec<_>>().join("\n\n");
    model::response_request(&snapshot.model, vec![
        json!({"role":"developer","content":[{"type":"input_text","text":format!("{SUMMARY_PROMPT}{KEEP_INSTRUCTIONS} Maximum output length: {} characters.",limit.0)}]}),
        json!({"role":"user","content":[{"type":"input_text","text":text}]}),
    ], snapshot.effort.as_deref(), false)
}

/// Overflow final-summary range: path start, excluding latest and first unsafe.
/// Active selection has different rules and must not reuse this boundary.
pub(super) fn overflow_prefix(snapshot: &Snapshot) -> Range<usize> {
    let stop = snapshot.groups.len().saturating_sub(1);
    let end = (0..stop).find(|&i| !range::group_is_safe(snapshot, i)).unwrap_or(stop);
    0..end
}

/// Projection here covers selected groups only; extra_sources remain fixed in
/// the request. This scope and the zero-weight strict-prefix fallback below are
/// pending USER clarification; neither is wired to the production dispatcher.
/// Only a classified context refusal calls this. Transient retry stays with
/// the existing HTTP request retry mechanism, never this range transformation.
pub(super) fn half_prefix(snapshot: &Snapshot, selected: Range<usize>) -> Option<Range<usize>> {
    if selected.len() <= 1 { return None; }
    let weights: Vec<u128> = snapshot.groups[selected.clone()].iter().map(|group| {
        group.sources.iter().map(|source| edit::projection_characters(&source.payload,snapshot.epoch) as u128).sum()
    }).collect();
    let half = weights.iter().sum::<u128>() / 2;
    let mut total = 0;
    let mut count = 0;
    // PROPOSAL ONLY: zero-total len-1 fallback awaits USER decision.
    for weight in weights.iter().take(weights.len()-1) {
        if total + weight > half { break; }
        total += weight;
        count += 1;
    }
    Some(selected.start..selected.start + count.max(1))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Next {
    Done,
    RetryPrefix(Range<usize>),
    Failed,
}

pub(super) fn after_result(
    snapshot: &Snapshot,
    selected: Range<usize>,
    result: Result<(), &model::ModelError>,
    eligible: impl FnOnce(Range<usize>) -> bool,
) -> Next {
    match result {
        Ok(()) => Next::Done, // No remainder scheduling in this trial.
        Err(error) if error.is_context_limit() => {
            match half_prefix(snapshot, selected).filter(|prefix| eligible(prefix.clone())) {
                Some(prefix) => Next::RetryPrefix(prefix),
                None => Next::Failed,
            }
        }
        Err(_) => Next::Failed, // Transport retries occur before this final result.
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AdoptionSize {
    pub(super) exceeded_output_limit: bool,
}
/// Output cap is a request instruction, not a resend/quality gate.
pub(super) fn adoption_size(original: u64, summary: u64, limit: OutputLimit) -> Option<AdoptionSize> {
    (summary < original).then_some(AdoptionSize { exceeded_output_limit: summary > limit.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(lengths: &[usize]) -> Snapshot {
        super::super::tests::fixture_snapshot(lengths.iter().enumerate().map(|(i,&n)| Group {
            sources: vec![super::super::tests::source(&format!("source{i}"),None,json!({"type":"message","role":"user","content":"x".repeat(n)}))],
            complete: true, tool: false,
        }).collect())
    }
    #[test]
    fn one_request_contains_whole_selected_range_and_output_instruction() {
        let snapshot=fixture(&[40,60,80]);
        let body=request(&snapshot,0..2,OutputLimit::resolve(None,None));
        let text=body.to_string();
        assert!(text.contains("source0") && text.contains("source1"));
        assert!(!text.contains("source2"));
        assert!(text.contains("20000") && text.contains("User instructions to keep"));
        assert_eq!(OutputLimit::resolve(Some(OutputLimit(37)),Some(OutputLimit(80))),OutputLimit(37));
        assert_eq!(OutputLimit::resolve(None,Some(OutputLimit(80))),OutputLimit(80));
    }
    #[test]
    fn half_is_whole_prefix_strictly_smaller_and_success_never_schedules_remainder() {
        let snapshot=fixture(&[40,40,40,40]);
        let half=half_prefix(&snapshot,0..4).unwrap();
        assert_eq!(half,0..2);
        assert_eq!(after_result(&snapshot,half,Ok(()),|_|panic!("success must not inspect another range")),Next::Done);
        assert_eq!(half_prefix(&snapshot,0..1),None);
        let mut selected=0..4;
        while let Some(next)=half_prefix(&snapshot,selected.clone()) {
            assert!(next.len()<selected.len());selected=next;
        }
        assert_eq!(selected.len(),1);
    }
    #[test]
    fn overflow_stops_before_unsafe_or_latest_and_never_splits_a_group() {
        let mut snapshot=fixture(&[10,10,10,10]);
        assert_eq!(overflow_prefix(&snapshot),0..3);
        snapshot.groups[1].complete=false;
        assert_eq!(overflow_prefix(&snapshot),0..1);
        snapshot.groups[1].complete=true;
        snapshot.protected.insert("source0".into());
        assert_eq!(overflow_prefix(&snapshot),0..0);
        assert_eq!(overflow_prefix(&fixture(&[10])),0..0);
    }
    #[test]
    fn oversize_but_shrinking_is_adoptable_without_resend() {
        assert_eq!(adoption_size(100000,25000,OutputLimit(20000)),Some(AdoptionSize{exceeded_output_limit:true}));
        assert!(adoption_size(25000,25000,OutputLimit(20000)).is_none());
        assert!(adoption_size(25000,25001,OutputLimit(20000)).is_none());
        assert_eq!(adoption_size(100,50,OutputLimit(20000)),Some(AdoptionSize{exceeded_output_limit:false}));
    }
    #[test]
    fn only_context_refusal_halves_and_ineligible_or_single_group_stops() {
        let snapshot=fixture(&[40,40,40,40]);
        let mut context=model::ModelError::new("provider","synthetic overflow");
        context.provider_code=Some("context_length_exceeded".into());
        assert!(context.is_context_limit());
        assert_eq!(after_result(&snapshot,0..4,Err(&context),|prefix|prefix== (0..2)),Next::RetryPrefix(0..2));
        assert_eq!(after_result(&snapshot,0..4,Err(&context),|_|false),Next::Failed);
        assert_eq!(after_result(&snapshot,0..1,Err(&context),|_|true),Next::Failed);
        for kind in ["network","http","provider"] {
            let error=model::ModelError::new(kind,"synthetic non-context failure");
            assert_eq!(after_result(&snapshot,0..4,Err(&error),|_|panic!("transient failure must not select a shorter prefix")),Next::Failed);
        }
    }

}
