//! Passive confirmation evidence for one existing G owner, never execution authority.
//! G supplies request/revision fences and registers this leaf after G main lands.
use crate::ui::observation_notice::{
    Freshness, MAX_METADATA_BYTES, NextAction, ObservationKind, ObservationNotice, RequestWitness,
    TargetWitnesses, result_metadata_bytes,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{mem::size_of, sync::Arc};

pub(crate) struct ConfirmationCapture {
    identity: RequestWitness,
    kind: ObservationKind,
    result: Arc<Value>,
    captured_at: DateTime<Utc>,
    targets: TargetWitnesses,
    metadata_bytes: usize,
    stale: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureOutcome {
    Accepted,
    WrongOwner,
    HiddenOrCancelled,
    Duplicate,
    Unavailable,
}

/// Called only for a completed explicit preview on the existing request channel.
/// A relevant commit must advance current.revision even while the preview is pending.
pub(crate) fn capture_confirmation(
    slot: &mut Option<ConfirmationCapture>,
    current: &RequestWitness,
    reply: ConfirmationCapture,
    visible: bool,
    cancelled: bool,
) -> CaptureOutcome {
    if !visible || cancelled {
        return CaptureOutcome::HiddenOrCancelled;
    }
    if &reply.identity != current {
        return CaptureOutcome::WrongOwner;
    }
    if slot
        .as_ref()
        .is_some_and(|old| old.identity == reply.identity)
    {
        return CaptureOutcome::Duplicate;
    }
    if !reply.kind.confirmation() {
        return CaptureOutcome::Unavailable;
    }
    *slot = Some(reply);
    CaptureOutcome::Accepted
}

impl ConfirmationCapture {
    pub fn identity(&self) -> &RequestWitness {
        &self.identity
    }
    pub fn result(&self) -> &Arc<Value> {
        &self.result
    }
    pub fn metadata_bytes(&self) -> usize {
        self.metadata_bytes
    }
    /// Keep the exact existing result Arc and wall time; never regenerate a preview.
    pub fn prepare(
        identity: RequestWitness,
        kind: ObservationKind,
        result: Arc<Value>,
        captured_at: DateTime<Utc>,
        targets: TargetWitnesses,
    ) -> Option<Self> {
        if !kind.confirmation() {
            return None;
        }
        let base = size_of::<Self>()
            .checked_add(identity.metadata_bytes()?)?
            .checked_add(targets.metadata_bytes()?)?;
        let result_bytes = result_metadata_bytes(&result, MAX_METADATA_BYTES.checked_sub(base)?)?;
        Some(Self {
            identity,
            kind,
            result,
            captured_at,
            stale: !targets.available(),
            targets,
            metadata_bytes: base + result_bytes,
        })
    }
}

/// A committed change only marks captured evidence stale. It does not re-preview,
/// cancel a mutation, change captured IDs/results/time or dispatch any operation.
pub(crate) fn invalidate_confirmation(
    capture: &mut ConfirmationCapture,
    committed_targets: Option<&[String]>,
) -> bool {
    let relevant = capture.targets.relevant(committed_targets);
    if relevant {
        capture.stale = true;
    }
    relevant
}

pub(crate) fn prepare_confirmation_notice(
    capture: &ConfirmationCapture,
    current: &RequestWitness,
    visible: bool,
    cancelled: bool,
    preview_pending: bool,
) -> ObservationNotice {
    let owned = visible && !cancelled && &capture.identity == current;
    let same_scope = capture.identity.owner == current.owner
        && capture.identity.target == current.target
        && capture.identity.generation == current.generation;
    let enabled = owned && !preview_pending && !capture.stale && capture.targets.available();
    ObservationNotice {
        kind: capture.kind,
        captured_at: Some(capture.captured_at),
        last_attempt_at: None,
        freshness: if !capture.targets.available() {
            Freshness::Unavailable
        } else if enabled {
            Freshness::Captured
        } else {
            Freshness::Stale
        },
        next_action: if visible && !cancelled && same_scope && !preview_pending {
            NextAction::RenewPreview
        } else {
            NextAction::None
        },
        confirmation_enabled: enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity(request: u64) -> RequestWitness {
        RequestWitness {
            owner: "popup:one".into(),
            target: "thread:one".into(),
            request,
            generation: 7,
            revision: 11,
        }
    }
    fn capture(request: u64, kind: ObservationKind) -> ConfirmationCapture {
        ConfirmationCapture::prepare(
            identity(request),
            kind,
            Arc::new(json!({"exact_targets": ["one"], "cost": 42})),
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            TargetWitnesses::Known(vec!["thread:one".into(), "index:source".into()]),
        )
        .unwrap()
    }
    fn notice(capture: &ConfirmationCapture, current: &RequestWitness) -> ObservationNotice {
        prepare_confirmation_notice(capture, current, true, false, false)
    }

    #[test]
    fn relevant_commit_invalidates_deletion_cost_and_index_without_replacing_evidence() {
        for kind in [
            ObservationKind::Deletion,
            ObservationKind::Cost,
            ObservationKind::Index,
        ] {
            let mut captured = capture(1, kind);
            let exact = Arc::clone(&captured.result);
            let time = captured.captured_at;
            assert!(notice(&captured, &identity(1)).confirmation_enabled);
            assert!(invalidate_confirmation(
                &mut captured,
                Some(&["thread:one".into()])
            ));
            assert!(!notice(&captured, &identity(1)).confirmation_enabled);
            assert_eq!(captured.captured_at, time);
            assert!(Arc::ptr_eq(&captured.result, &exact));
            assert_eq!(captured.identity, identity(1));
        }
    }

    #[test]
    fn unrelated_commit_does_not_invalidate_exact_scope() {
        let mut captured = capture(1, ObservationKind::Deletion);
        assert!(!invalidate_confirmation(
            &mut captured,
            Some(&["thread:other".into()])
        ));
        assert!(notice(&captured, &identity(1)).confirmation_enabled);
    }

    #[test]
    fn stale_duplicate_cannot_authorize_but_explicit_new_preview_can() {
        let mut old = capture(1, ObservationKind::Cost);
        invalidate_confirmation(&mut old, None);
        let mut slot = Some(old);
        assert_eq!(
            capture_confirmation(
                &mut slot,
                &identity(1),
                capture(1, ObservationKind::Cost),
                true,
                false
            ),
            CaptureOutcome::Duplicate
        );
        assert!(!notice(slot.as_ref().unwrap(), &identity(1)).confirmation_enabled);
        assert_eq!(
            capture_confirmation(
                &mut slot,
                &identity(2),
                capture(2, ObservationKind::Cost),
                true,
                false
            ),
            CaptureOutcome::Accepted
        );
        assert!(notice(slot.as_ref().unwrap(), &identity(2)).confirmation_enabled);
    }

    #[test]
    fn pending_reply_after_relevant_revision_change_cannot_enable_confirmation() {
        let mut current = identity(1);
        current.revision += 1;
        let mut slot = None;
        assert_eq!(
            capture_confirmation(
                &mut slot,
                &current,
                capture(1, ObservationKind::Index),
                true,
                false
            ),
            CaptureOutcome::WrongOwner
        );
        assert!(slot.is_none());
    }

    #[test]
    fn owner_target_request_generation_and_cancelled_hidden_replies_are_rejected() {
        let mut slot = None;
        for mut current in [identity(1), identity(1), identity(2)] {
            current.generation += 1;
            assert_eq!(
                capture_confirmation(
                    &mut slot,
                    &current,
                    capture(1, ObservationKind::Deletion),
                    true,
                    false
                ),
                CaptureOutcome::WrongOwner
            );
        }
        let mut other = identity(1);
        other.target = "thread:other".into();
        assert_eq!(
            capture_confirmation(
                &mut slot,
                &other,
                capture(1, ObservationKind::Deletion),
                true,
                false
            ),
            CaptureOutcome::WrongOwner
        );
        other = identity(1);
        other.owner = "popup:other".into();
        assert_eq!(
            capture_confirmation(
                &mut slot,
                &other,
                capture(1, ObservationKind::Deletion),
                true,
                false
            ),
            CaptureOutcome::WrongOwner
        );
        for (visible, cancelled) in [(false, false), (true, true)] {
            assert_eq!(
                capture_confirmation(
                    &mut slot,
                    &identity(1),
                    capture(1, ObservationKind::Deletion),
                    visible,
                    cancelled
                ),
                CaptureOutcome::HiddenOrCancelled
            );
        }
        assert!(slot.is_none());
    }

    #[test]
    fn valid_limit_witnesses_preserve_all_exact_targets_without_truncation() {
        let ids = (0..200)
            .map(|index| format!("thread:{index:03}"))
            .collect::<Vec<_>>();
        let exact = ids.clone();
        let mut captured = ConfirmationCapture::prepare(
            identity(1),
            ObservationKind::Deletion,
            Arc::new(json!({"count": 200})),
            DateTime::from_timestamp(1, 0).unwrap(),
            TargetWitnesses::Known(ids),
        )
        .unwrap();
        assert_eq!(captured.targets, TargetWitnesses::Known(exact));
        assert!(captured.metadata_bytes() <= MAX_METADATA_BYTES);
        assert!(!invalidate_confirmation(
            &mut captured,
            Some(&["thread:999".into()])
        ));
        assert!(invalidate_confirmation(
            &mut captured,
            Some(&["thread:199".into()])
        ));
        assert!(!notice(&captured, &identity(1)).confirmation_enabled);
    }

    #[test]
    fn excessive_result_or_identity_is_not_silently_trimmed() {
        let mut too_long = identity(1);
        too_long.target = "x".repeat(513);
        assert!(
            ConfirmationCapture::prepare(
                too_long,
                ObservationKind::Cost,
                Arc::new(json!({})),
                DateTime::from_timestamp(1, 0).unwrap(),
                TargetWitnesses::Known(vec![])
            )
            .is_none()
        );
        assert!(
            ConfirmationCapture::prepare(
                identity(1),
                ObservationKind::Cost,
                Arc::new(json!("x".repeat(MAX_METADATA_BYTES))),
                DateTime::from_timestamp(1, 0).unwrap(),
                TargetWitnesses::Known(vec!["target".into()])
            )
            .is_none()
        );
    }

    #[test]
    fn unknown_scope_and_pending_new_request_never_mean_execution_permission() {
        let captured = ConfirmationCapture::prepare(
            identity(1),
            ObservationKind::Index,
            Arc::new(json!({})),
            DateTime::from_timestamp(1, 0).unwrap(),
            TargetWitnesses::Unavailable,
        )
        .unwrap();
        assert_eq!(
            notice(&captured, &identity(1)).freshness,
            Freshness::Unavailable
        );
        assert!(!notice(&captured, &identity(1)).confirmation_enabled);
        let empty = ConfirmationCapture::prepare(
            identity(1),
            ObservationKind::Index,
            Arc::new(json!({})),
            DateTime::from_timestamp(1, 0).unwrap(),
            TargetWitnesses::Known(vec![]),
        )
        .unwrap();
        assert_eq!(
            notice(&empty, &identity(1)).freshness,
            Freshness::Unavailable
        );
        assert!(!notice(&empty, &identity(1)).confirmation_enabled);
        let captured = capture(1, ObservationKind::Index);
        let pending = prepare_confirmation_notice(&captured, &identity(1), true, false, true);
        assert!(!pending.confirmation_enabled);
        assert_eq!(pending.next_action, NextAction::None);
        let mut other = identity(1);
        other.owner = "popup:other".into();
        let stale_owner = notice(&captured, &other);
        assert!(!stale_owner.confirmation_enabled);
        assert_eq!(stale_owner.next_action, NextAction::None);
        let renewed = notice(&captured, &identity(2));
        assert!(!renewed.confirmation_enabled);
        assert_eq!(renewed.next_action, NextAction::RenewPreview);
        assert!(captured.metadata_bytes <= MAX_METADATA_BYTES);
    }
}
