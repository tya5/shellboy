//! Completed external/native facts on the existing single G request owner.
//! No provider/OS/file probes, timers, retries, mutation or execution dispatch.
use crate::ui::observation_notice::{
    Freshness, MAX_IDENTITY_BYTES, MAX_METADATA_BYTES, NextAction, ObservationKind,
    ObservationNotice, RequestWitness, result_metadata_bytes,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{mem::size_of, sync::Arc};

pub(crate) struct ExternalReply {
    pub identity: RequestWitness,
    pub kind: ObservationKind,
    pub completed_at: DateTime<Utc>,
    pub result: Result<Arc<Value>, ()>,
}

pub(crate) struct ExternalObservation {
    identity: RequestWitness,
    kind: ObservationKind,
    result: Option<Arc<Value>>,
    captured_identity: Option<RequestWitness>,
    captured_at: Option<DateTime<Utc>>,
    last_attempt_at: DateTime<Utc>,
    freshness: Freshness,
    metadata_bytes: usize,
}
impl ExternalObservation {
    pub fn result(&self) -> Option<&Arc<Value>> {
        self.result.as_ref()
    }
    pub fn captured_identity(&self) -> Option<&RequestWitness> {
        self.captured_identity.as_ref()
    }
    pub fn metadata_bytes(&self) -> usize {
        self.metadata_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionOutcome {
    Accepted,
    FailedRetained,
    FailedUnavailable,
    WrongOwner,
    HiddenOrCancelled,
    Duplicate,
}

/// Invocation is a delivery/completion, not a request to re-run any observation.
/// The existing owner supplies one pending token and advances it on explicit repeat.
pub(crate) fn completed_observation(
    slot: &mut Option<ExternalObservation>,
    current: &RequestWitness,
    reply: ExternalReply,
    visible: bool,
    cancelled: bool,
) -> CompletionOutcome {
    if !visible || cancelled {
        return CompletionOutcome::HiddenOrCancelled;
    }
    if &reply.identity != current || current.metadata_bytes().is_none() || reply.kind.confirmation() {
        return CompletionOutcome::WrongOwner;
    }
    if slot.as_ref().is_some_and(|old| old.identity == reply.identity) {
        return CompletionOutcome::Duplicate;
    }
    // Reserve both identities at their worst-case allowance so a failed refresh
    // cannot force eviction of a previously accepted result/time near the cap.
    let reserve = size_of::<ExternalObservation>()
        + 2 * (size_of::<RequestWitness>() + 2 * MAX_IDENTITY_BYTES);
    let accepted = reply.result.ok().and_then(|value| {
        result_metadata_bytes(&value, MAX_METADATA_BYTES.checked_sub(reserve)?)
            .map(|bytes| (value, bytes))
    });
    if let Some((result, bytes)) = accepted {
        *slot = Some(ExternalObservation {
            identity: reply.identity.clone(), kind: reply.kind,
            result: Some(result), captured_identity: Some(reply.identity),
            captured_at: Some(reply.completed_at), last_attempt_at: reply.completed_at,
            freshness: Freshness::Captured, metadata_bytes: reserve + bytes,
        });
        return CompletionOutcome::Accepted;
    }
    let retained = slot.as_ref().filter(|old| {
        old.identity.owner == current.owner && old.identity.target == current.target
            && old.identity.generation == current.generation && old.kind == reply.kind
    });
    let (result, captured_identity, captured_at, bytes) = retained.map_or(
        (None, None, None, reserve),
        |old| (old.result.clone(), old.captured_identity.clone(), old.captured_at, old.metadata_bytes),
    );
    let outcome = if result.is_some() { CompletionOutcome::FailedRetained } else { CompletionOutcome::FailedUnavailable };
    *slot = Some(ExternalObservation {
        identity: reply.identity, kind: reply.kind, result, captured_identity, captured_at,
        last_attempt_at: reply.completed_at, freshness: Freshness::FailedRefresh,
        metadata_bytes: bytes,
    });
    outcome
}

pub(crate) fn prepare_refresh_notice(
    captured: &ExternalObservation,
    current: &RequestWitness,
    visible: bool,
    cancelled: bool,
    request_pending: bool,
) -> ObservationNotice {
    let owned = visible && !cancelled && &captured.identity == current;
    let next_action = if !owned || request_pending {
        NextAction::None
    } else if captured.kind == ObservationKind::Models {
        NextAction::RefreshModels
    } else {
        NextAction::RepeatInspection
    };
    ObservationNotice {
        kind: captured.kind,
        captured_at: captured.captured_at,
        last_attempt_at: Some(captured.last_attempt_at),
        freshness: if captured.result.is_none() { Freshness::Unavailable }
            else if !owned || request_pending { Freshness::Stale }
            else { captured.freshness },
        next_action,
        confirmation_enabled: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity(request: u64) -> RequestWitness {
        RequestWitness { owner: "popup:one".into(), target: "connection:one".into(), request, generation: 7, revision: 11 }
    }
    fn reply(request: u64, kind: ObservationKind, result: Result<Arc<Value>, ()>) -> ExternalReply {
        ExternalReply { identity: identity(request), kind,
            completed_at: DateTime::from_timestamp(1_700_000_000 + request as i64, 0).unwrap(), result }
    }
    fn success(request: u64, kind: ObservationKind) -> ExternalReply {
        reply(request, kind, Ok(Arc::new(json!({"recorded": "exact-result"}))))
    }

    #[test]
    fn model_memory_and_client_completion_get_fixed_time_and_explicit_next_route() {
        for kind in [ObservationKind::Models, ObservationKind::NativeMemory, ObservationKind::StandaloneClient] {
            let mut slot = None;
            let response = success(1, kind);
            let time = response.completed_at;
            assert_eq!(completed_observation(&mut slot, &identity(1), response, true, false), CompletionOutcome::Accepted);
            let observation = slot.as_ref().unwrap();
            let notice = prepare_refresh_notice(observation, &identity(1), true, false, false);
            assert_eq!(notice.captured_at, Some(time));
            assert_eq!(notice.next_action, if kind == ObservationKind::Models { NextAction::RefreshModels } else { NextAction::RepeatInspection });
            assert_eq!(notice.text(), notice.text());
            assert!(!notice.confirmation_enabled);
            assert!(observation.metadata_bytes() <= MAX_METADATA_BYTES);
        }
    }

    #[test]
    fn failed_explicit_refresh_preserves_successful_result_identity_and_time() {
        let mut slot = None;
        completed_observation(&mut slot, &identity(1), success(1, ObservationKind::NativeMemory), true, false);
        let old = slot.as_ref().unwrap();
        let exact = Arc::clone(old.result().unwrap());
        let time = old.captured_at;
        assert_eq!(completed_observation(&mut slot, &identity(2), reply(2, ObservationKind::NativeMemory, Err(())), true, false), CompletionOutcome::FailedRetained);
        let retained = slot.as_ref().unwrap();
        assert!(Arc::ptr_eq(retained.result().unwrap(), &exact));
        assert_eq!(retained.captured_at, time);
        assert_eq!(retained.captured_identity(), Some(&identity(1)));
        let notice = prepare_refresh_notice(retained, &identity(2), true, false, false);
        assert_eq!(notice.freshness, Freshness::FailedRefresh);
        assert_eq!(notice.next_action, NextAction::RepeatInspection);
    }

    #[test]
    fn delayed_duplicate_cancelled_and_hidden_completions_cannot_replace_facts() {
        let mut slot = None;
        assert_eq!(completed_observation(&mut slot, &identity(2), success(1, ObservationKind::Models), true, false), CompletionOutcome::WrongOwner);
        for (visible, cancelled) in [(false, false), (true, true)] {
            assert_eq!(completed_observation(&mut slot, &identity(1), success(1, ObservationKind::Models), visible, cancelled), CompletionOutcome::HiddenOrCancelled);
        }
        assert!(slot.is_none());
        completed_observation(&mut slot, &identity(2), success(2, ObservationKind::Models), true, false);
        assert_eq!(completed_observation(&mut slot, &identity(2), reply(2, ObservationKind::Models, Err(())), true, false), CompletionOutcome::Duplicate);
        assert_eq!(slot.as_ref().unwrap().freshness, Freshness::Captured);
    }

    #[test]
    fn changed_connection_owner_or_generation_does_not_relabel_old_facts() {
        for changed_field in 0..3 {
            let mut slot = None;
            completed_observation(&mut slot, &identity(1), success(1, ObservationKind::Models), true, false);
            let mut current = identity(2);
            match changed_field { 0 => current.target = "connection:other".into(), 1 => current.owner = "popup:other".into(), _ => current.generation += 1 }
            let mut failed = reply(2, ObservationKind::Models, Err(()));
            failed.identity = current.clone();
            assert_eq!(completed_observation(&mut slot, &current, failed, true, false), CompletionOutcome::FailedUnavailable);
            assert!(slot.as_ref().unwrap().result().is_none());
            assert!(slot.as_ref().unwrap().captured_at.is_none());
        }
    }

    #[test]
    fn over_budget_result_retains_previous_fact_and_pending_notice_cannot_repeat() {
        let mut slot = None;
        completed_observation(&mut slot, &identity(1), success(1, ObservationKind::StandaloneClient), true, false);
        let huge = reply(2, ObservationKind::StandaloneClient, Ok(Arc::new(json!("x".repeat(MAX_METADATA_BYTES)))));
        assert_eq!(completed_observation(&mut slot, &identity(2), huge, true, false), CompletionOutcome::FailedRetained);
        let notice = prepare_refresh_notice(slot.as_ref().unwrap(), &identity(2), true, false, true);
        assert_eq!(notice.next_action, NextAction::None);
        assert_eq!(notice.freshness, Freshness::Stale);
    }
}
