//! One physical-storage sample slot owned by the existing visible inspector.
//! The parent owns target/visibility admission; this adds no worker or timer.
use crate::{
    control::Control,
    protocol::{ApiError, ErrorCode},
    ui::ManagementContext,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
const PERIOD: Duration = Duration::from_secs(3);
#[derive(Clone)]
pub(super) struct Ticket {
    pub generation: u64,
    pub context: ManagementContext,
    pub stop: CancellationToken,
    serial: u64,
}
struct Flight {
    serial: u64,
    generation: u64,
    context: ManagementContext,
    stop: CancellationToken,
}
pub(super) struct Physical {
    dirty: bool,
    inflight: Option<Flight>,
    serial: u64,
    next: Option<Instant>,
    sequence: u64,
    facts: Option<Arc<Value>>,
}
impl Default for Physical {
    fn default() -> Self {
        Self {
            dirty: true,
            inflight: None,
            serial: 0,
            next: None,
            sequence: 0,
            facts: None,
        }
    }
}
impl Drop for Physical {
    fn drop(&mut self) {
        self.hide();
    }
}
impl Physical {
    /// Only call from the current visible Storage consumer's notification path.
    pub fn dirty(&mut self) {
        self.dirty = true;
    }
    /// The parent must query/admit this deadline only for visible Storage.
    pub fn deadline(&self) -> Option<Instant> {
        (self.dirty && self.inflight.is_none()).then(|| self.next.unwrap_or_else(Instant::now))
    }
    pub fn take(
        &mut self,
        now: Instant,
        generation: u64,
        context: ManagementContext,
        parent_stop: &CancellationToken,
    ) -> Option<Ticket> {
        if !self.dirty
            || self.inflight.is_some()
            || self.next.is_some_and(|next| next > now)
            || parent_stop.is_cancelled()
        {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let stop = parent_stop.child_token();
        self.inflight = Some(Flight {
            serial: self.serial,
            generation,
            context: context.clone(),
            stop: stop.clone(),
        });
        self.dirty = false;
        self.next = Some(now + PERIOD);
        Some(Ticket {
            generation,
            context,
            stop,
            serial: self.serial,
        })
    }
    /// Retire observation only. Parent scope changes never cancel durable work.
    pub fn hide(&mut self) {
        if let Some(flight) = self.inflight.take() {
            flight.stop.cancel();
        }
        self.dirty = true;
    }
    pub fn facts(&self) -> Option<Arc<Value>> {
        self.facts.clone()
    }
    #[cfg(test)]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Caller additionally fences the current parent inspection before calling.
    pub fn complete(&mut self, ticket: &Ticket, result: Result<Value, ApiError>) -> bool {
        if ticket.stop.is_cancelled()
            || self.inflight.as_ref().is_none_or(|flight| {
                flight.serial != ticket.serial
                    || flight.generation != ticket.generation
                    || flight.context != ticket.context
                    || flight.stop.is_cancelled()
            })
        {
            return false;
        }
        self.inflight = None;
        // A slow OS sample does not cause an immediate catch-up sample burst.
        let next = Instant::now() + PERIOD;
        self.next = Some(self.next.map_or(next, |old| old.max(next)));
        self.sequence = self.sequence.wrapping_add(1);
        let (sample, error) = match result {
            Ok(sample) => (Some(sample), None),
            Err(error) => (
                self.facts
                    .as_ref()
                    .and_then(|facts| facts.get("sample"))
                    .filter(|sample| !sample.is_null())
                    .cloned(),
                Some(
                    json!({"code":error.code,"message":error.message,"observed_at":chrono::Utc::now().to_rfc3339()}),
                ),
            ),
        };
        let available = sample.is_some();
        self.facts = Some(Arc::new(json!({
            "physical_sequence":self.sequence,
            "sample":sample,
            "available":available,
            "stale":error.is_some(),
            "refresh_error":error,
        })));
        true
    }
}
/// Existing finite control-file worker, followed by finite presentation work.
pub(super) async fn read(control: &Control, ticket: &Ticket) -> Result<Value, ApiError> {
    let deadline =
        Instant::now() + Duration::from_millis(control.store.config.io.file_prepare_timeout_ms);
    let sample = control
        .gc
        .sample_observation_physical(deadline, ticket.stop.clone())
        .await?;
    super::presentation_work(move || {
        serde_json::to_value(sample).map_err(|_| {
            ApiError::new(
                ErrorCode::IoFailed,
                "Cannot prepare physical storage sample",
            )
        })
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(epoch: u64) -> ManagementContext {
        ManagementContext {
            epoch,
            owner_draft: "draft".into(),
            owner_thread: Some("owner".into()),
        }
    }
    #[test]
    fn hides_cancel_inflight_and_reopen_preserves_deadline_and_rejects_old_serial() {
        let mut slot = Physical::default();
        let at = Instant::now();
        let stop = CancellationToken::new();
        let first = slot.take(at, 1, context(1), &stop).unwrap();
        slot.hide();
        assert!(first.stop.is_cancelled());
        assert!(!slot.complete(&first, Ok(json!({"observed_at":"late"}))));
        assert!(slot.facts().is_none());
        assert_eq!(slot.sequence(), 0);
        assert!(
            slot.take(at + PERIOD - Duration::from_millis(1), 2, context(2), &stop)
                .is_none()
        );
        let current = slot.take(at + PERIOD, 2, context(2), &stop).unwrap();
        assert_ne!(first.serial, current.serial);
        assert!(!slot.complete(&first, Ok(json!({"observed_at":"late"}))));
        assert!(slot.complete(
            &current,
            Ok(json!({"observed_at":"current","database":{"bytes":17,"complete":true}}))
        ));
        assert_eq!(slot.facts().unwrap()["sample"]["observed_at"], "current");
    }
    #[test]
    fn coalesced_dirty_never_overlaps_and_parent_cancel_never_publishes_sample() {
        let mut slot = Physical::default();
        let at = Instant::now();
        let parent = CancellationToken::new();
        let first = slot.take(at, 1, context(1), &parent).unwrap();
        for _ in 0..100 {
            slot.dirty();
            assert!(slot.take(at + PERIOD, 1, context(1), &parent).is_none());
        }
        parent.cancel();
        assert!(!slot.complete(&first, Ok(json!({"observed_at":"cancelled"}))));
        assert_eq!(slot.sequence(), 0);
        assert!(slot.facts().is_none());
        slot.hide();
        let live = CancellationToken::new();
        let next = slot.take(at + PERIOD, 2, context(2), &live).unwrap();
        assert!(slot.complete(&next, Ok(json!({"observed_at":"accepted"}))));
    }
    #[test]
    fn failure_keeps_previous_physical_timestamp_and_own_watermark_until_new_success() {
        let mut slot = Physical::default();
        let at = Instant::now();
        let parent = CancellationToken::new();
        let first = slot.take(at, 1, context(1), &parent).unwrap();
        assert!(slot.complete(&first,Ok(json!({"started_at":"sample-start","observed_at":"sample-end","database":{"bytes":29,"complete":true}}))));
        assert_eq!(slot.sequence(), 1);
        slot.dirty();
        let due = slot.deadline().unwrap();
        assert!(
            slot.take(due - Duration::from_millis(1), 1, context(1), &parent)
                .is_none()
        );
        let failed = slot.take(due, 1, context(1), &parent).unwrap();
        assert!(slot.complete(
            &failed,
            Err(ApiError::new(ErrorCode::IoFailed, "stat denied"))
        ));
        let facts = slot.facts().unwrap();
        assert_eq!(facts["sample"]["observed_at"], "sample-end");
        assert_eq!(facts["sample"]["database"]["bytes"], 29);
        assert_eq!(facts["stale"], true);
        assert_eq!(facts["available"], true);
        assert_eq!(facts["refresh_error"]["message"], "stat denied");
        assert_eq!(slot.sequence(), 2);
        assert!(
            slot.deadline().is_none(),
            "read error does not create timer polling"
        );
        slot.dirty();
        let next = slot
            .take(slot.deadline().unwrap(), 1, context(1), &parent)
            .unwrap();
        assert!(slot.complete(&next, Ok(json!({"observed_at":"fresh"}))));
        assert_eq!(slot.facts().unwrap()["stale"], false);
        assert!(slot.facts().unwrap()["refresh_error"].is_null());
    }
    #[test]
    fn first_sample_failure_is_unknown_not_fabricated_zero() {
        let mut slot = Physical::default();
        let ticket = slot
            .take(Instant::now(), 1, context(1), &CancellationToken::new())
            .unwrap();
        assert!(slot.complete(
            &ticket,
            Err(ApiError::new(ErrorCode::IoFailed, "unavailable"))
        ));
        let facts = slot.facts().unwrap();
        assert!(facts["sample"].is_null());
        assert_eq!(facts["available"], false);
        assert_eq!(facts["stale"], true);
    }
}
