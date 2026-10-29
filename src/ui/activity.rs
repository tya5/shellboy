//! Display facts and a local animation clock. No lifecycle writes, I/O or tasks.
use super::theme;
use ratatui::{style::Style, text::Span};
use std::time::{Duration, Instant};

pub use crate::activity::ActivityPhase;
impl ActivityPhase {
    pub fn is_active(self) -> bool {
        self.motion() != Motion::Still
    }
    fn motion(self) -> Motion {
        match self {
            Self::Sending
            | Self::Reflecting
            | Self::Preparing
            | Self::AwaitingModel
            | Self::Receiving
            | Self::Tools
            | Self::Running => Motion::Shimmer,
            Self::Retrying
            | Self::Waiting
            | Self::Background
            | Self::Continuing
            | Self::Stopping => Motion::Pulse,
            _ => Motion::Still,
        }
    }
    fn marker(self) -> &'static str {
        match self {
            Self::Background | Self::Continuing | Self::Stopping => "◌",
            Self::Completed => "✓",
            Self::Recovery | Self::Failed => "!",
            Self::Stopped => "■",
            Self::Idle => "·",
            _ => "●",
        }
    }
}
/// Existing saved facts for human recovery; never a new lifecycle state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoverySnapshot {
    pub stage: String,
    pub message: String,
    pub pending_messages: u64,
    pub saved_tool_results: u64,
    pub live_shells: u64,
    pub uncollected_shells: u64,
    pub stop_reason: Option<String>,
}
/// Counts recorded by the failed attempt before a finite retry wait.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct AutomaticRetrySnapshot {
    pub number: u64,
    pub limit: u64,
    pub delay_ms: u64,
    pub partial_response_discarded: bool,
}
impl AutomaticRetrySnapshot {
    pub(super) fn label(&self) -> String {
        format!(
            "（{}/{}） · {}秒待機{} · 再送は重複課金の可能性",
            self.number,
            self.limit,
            self.delay_ms.div_ceil(1000),
            if self.partial_response_discarded {
                " · 途中応答を保存して作り直し"
            } else {
                ""
            }
        )
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivitySnapshot {
    /// Saved live-Run Revisit count/reservation, projected by the reader.
    pub revisit: Option<String>,
    /// Saved automatic-trim facts for this Turn/generation, if observed.
    pub context_reduction: Option<String>,
    pub automatic_retry: Option<AutomaticRetrySnapshot>,
    pub recovery: Option<RecoverySnapshot>,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub attempt_id: Option<String>,
    pub phase: ActivityPhase,
    pub turn_in_progress: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    Still,
    Shimmer,
    Pulse,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ActivityView {
    pub snapshot: Option<ActivitySnapshot>,
    epoch: Option<u64>,
    received_attempt: Option<String>,
    receive: Option<ReceiveObservation>,
    clock: Option<ObservedClock>,
}
impl ActivityView {
    #[cfg(test)]
    pub fn accept(&mut self, epoch: u64, snapshot: ActivitySnapshot) -> bool {
        self.accept_at(epoch, snapshot, Instant::now())
    }
    pub fn accept_at(&mut self, epoch: u64, snapshot: ActivitySnapshot, at: Instant) -> bool {
        if self.epoch.is_some_and(|current| epoch < current) {
            return false;
        }
        self.epoch = Some(epoch);
        if self.snapshot.as_ref().is_none_or(|previous| {
            previous.run_id != snapshot.run_id || previous.turn_id != snapshot.turn_id
        }) {
            self.clock = None;
        }
        if snapshot.turn_in_progress && snapshot.run_id.is_some() && snapshot.turn_id.is_some() {
            self.clock
                .get_or_insert_with(|| ObservedClock::new(at))
                .observe(at, true);
        } else if let Some(clock) = &mut self.clock {
            clock.observe(at, false);
        }
        if snapshot.phase != ActivityPhase::AwaitingModel {
            self.received_attempt = None;
        }
        if snapshot.phase == ActivityPhase::AwaitingModel {
            if let Some(attempt) = &snapshot.attempt_id {
                if self
                    .receive
                    .as_ref()
                    .is_none_or(|receive| receive.attempt != *attempt)
                {
                    self.receive = Some(ReceiveObservation {
                        attempt: attempt.clone(),
                        observed_at: at,
                        received: false,
                    });
                }
            } else {
                self.receive = None;
            }
        } else {
            self.receive = None;
        }
        let changed = self.snapshot.as_ref() != Some(&snapshot);
        self.snapshot = Some(snapshot);
        changed
    }
    pub fn unavailable_at(&mut self, epoch: u64, at: Instant) -> bool {
        if self.epoch.is_some_and(|current| epoch < current) {
            return false;
        }
        self.epoch = Some(epoch);
        let had_receive = self.receive.take().is_some();
        if let Some(clock) = &mut self.clock {
            let running = clock.running;
            clock.observe(at, false);
            running || had_receive
        } else {
            had_receive
        }
    }
    pub fn advance_clock(&mut self, at: Instant) {
        if let Some(clock) = &mut self.clock
            && clock.running
        {
            clock.observe(at, true);
        }
    }
    pub fn displayed_elapsed_ms(&self) -> Option<u64> {
        self.clock
            .as_ref()
            .map(|clock| clock.elapsed.as_millis().min(u64::MAX as u128) as u64)
    }
    #[cfg(test)]
    pub fn elapsed_ms(&self, at: Instant) -> Option<u64> {
        self.clock
            .as_ref()
            .map(|clock| clock.elapsed_at(at).as_millis().min(u64::MAX as u128) as u64)
    }
    pub fn stream(&mut self, attempt: &str) {
        // A new attempt's first chunk may precede its DB projection, even
        // while the last Turn still appears completed. Keep one candidate;
        // phase() requires the matching saved inflight attempt before promotion.
        if self.snapshot.as_ref().is_none_or(|snapshot| {
            snapshot.phase != ActivityPhase::AwaitingModel
                || snapshot.attempt_id.as_deref() == Some(attempt)
        }) {
            self.received_attempt = Some(attempt.into());
        }
    }

    /// Headers and any body bytes are liveness observations, not assistant text.
    /// Keep one candidate when an observation precedes its saved projection.
    pub fn receive_at(&mut self, attempt: &str, at: Instant) -> bool {
        if let Some(snapshot) = &self.snapshot {
            if snapshot.phase == ActivityPhase::AwaitingModel {
                if snapshot.attempt_id.as_deref() != Some(attempt) {
                    return false;
                }
            } else if snapshot.attempt_id.as_deref() == Some(attempt) {
                return false;
            }
        }
        let was_visible = self.receive_gap_note(at).is_some();
        match &mut self.receive {
            Some(receive) if receive.attempt == attempt => {
                receive.observed_at = receive.observed_at.max(at);
                receive.received = true;
            }
            _ => {
                self.receive = Some(ReceiveObservation {
                    attempt: attempt.into(),
                    observed_at: at,
                    received: true,
                })
            }
        }
        was_visible
    }

    pub fn receive_gap_note(&self, now: Instant) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        let receive = self.receive.as_ref()?;
        if snapshot.phase != ActivityPhase::AwaitingModel
            || snapshot.attempt_id.as_deref() != Some(receive.attempt.as_str())
        {
            return None;
        }
        let seconds = now.saturating_duration_since(receive.observed_at).as_secs();
        if seconds < 30 {
            return None;
        }
        Some(if receive.received {
            format!("最後の受信から{seconds}秒")
        } else {
            format!("受信待ち{seconds}秒（画面の観測開始から）")
        })
    }

    pub fn phase(&self) -> ActivityPhase {
        let Some(snapshot) = &self.snapshot else {
            return ActivityPhase::Idle;
        };
        if snapshot.phase == ActivityPhase::AwaitingModel
            && snapshot.attempt_id.is_some()
            && snapshot.attempt_id == self.received_attempt
        {
            ActivityPhase::Receiving
        } else {
            snapshot.phase
        }
    }
}

/// One visible attempt only; timestamps are monotonic UI observations.
#[derive(Clone, Debug)]
struct ReceiveObservation {
    attempt: String,
    observed_at: Instant,
    received: bool,
}

/// One local observation interval accumulator, never a persisted Turn duration.
#[derive(Clone, Debug)]
struct ObservedClock {
    elapsed: Duration,
    observed_at: Instant,
    running: bool,
}
impl ObservedClock {
    fn new(at: Instant) -> Self {
        Self {
            elapsed: Duration::ZERO,
            observed_at: at,
            running: false,
        }
    }
    fn observe(&mut self, at: Instant, running: bool) {
        let at = at.max(self.observed_at);
        if self.running {
            self.elapsed = self
                .elapsed
                .saturating_add(at.duration_since(self.observed_at));
        }
        self.observed_at = at;
        self.running = running;
    }
    #[cfg(test)]
    fn elapsed_at(&self, at: Instant) -> Duration {
        self.elapsed.saturating_add(if self.running {
            at.saturating_duration_since(self.observed_at)
        } else {
            Duration::ZERO
        })
    }
}

/// At most one footer animation. Time changes styles, never text or layout keys.
pub(super) struct Animation {
    origin: Instant,
    next: Option<Instant>,
    phase: ActivityPhase,
}
impl Animation {
    pub fn new(now: Instant) -> Self {
        Self {
            origin: now,
            next: None,
            phase: ActivityPhase::Idle,
        }
    }
    pub fn tick(
        &mut self,
        now: Instant,
        phase: ActivityPhase,
        visible: bool,
        max_fps: u32,
    ) -> bool {
        if !visible || !phase.is_active() {
            self.next = None;
            self.phase = phase;
            return false;
        }
        let step = Duration::from_secs_f64(1.0 / f64::from(max_fps.clamp(1, 10)));
        if self.phase != phase || self.next.is_none() {
            self.phase = phase;
            self.origin = now;
            self.next = Some(now + step);
            return true;
        }
        if self.next.is_some_and(|deadline| now >= deadline) {
            self.next = Some(now + step);
            return true;
        }
        false
    }
    pub fn spans(&self, phase: ActivityPhase, base: Style, now: Instant) -> Vec<Span<'static>> {
        let text = format!("{} {}", phase.marker(), phase.label());
        let elapsed = now.saturating_duration_since(self.origin).as_secs_f64();
        match phase.motion() {
            Motion::Still => vec![Span::styled(text, base)],
            Motion::Pulse => {
                let level = (1.0 + (elapsed * std::f64::consts::PI).sin()) / 2.0;
                vec![
                    Span::styled(
                        phase.marker().to_owned(),
                        base.fg(theme::activity_pulse(level)),
                    ),
                    Span::styled(format!(" {}", phase.label()), base),
                ]
            }
            Motion::Shimmer => {
                let length = text.chars().count();
                let head = ((elapsed / 1.2) * (length + 4) as f64) as usize % (length + 4);
                text.chars()
                    .enumerate()
                    .map(|(index, ch)| {
                        let distance = index.abs_diff(head);
                        Span::styled(
                            ch.to_string(),
                            base.fg(if distance <= 1 {
                                theme::EMPHASIS
                            } else if distance <= 3 {
                                theme::ACTIVITY_HIGHLIGHT
                            } else {
                                theme::ACCENT
                            }),
                        )
                    })
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn awaiting(attempt: &str) -> ActivitySnapshot {
        ActivitySnapshot {
            run_id: Some("run".into()),
            turn_id: Some("turn".into()),
            attempt_id: Some(attempt.into()),
            phase: ActivityPhase::AwaitingModel,
            turn_in_progress: true,
            ..Default::default()
        }
    }

    #[test]
    fn receive_gap_tracks_headers_and_bytes_without_inventing_text_progress() {
        let at = Instant::now();
        let mut view = ActivityView::default();
        view.accept_at(1, awaiting("a"), at);
        assert_eq!(view.receive_gap_note(at + Duration::from_secs(29)), None);
        assert_eq!(
            view.receive_gap_note(at + Duration::from_secs(30))
                .as_deref(),
            Some("受信待ち30秒（画面の観測開始から）")
        );
        // Header arrival, then keepalive/comment/partial-frame bytes use the
        // same transport observation; neither is an assistant text delta.
        assert!(view.receive_at("a", at + Duration::from_secs(31)));
        assert_eq!(view.phase(), ActivityPhase::AwaitingModel);
        assert_eq!(view.receive_gap_note(at + Duration::from_secs(60)), None);
        assert_eq!(
            view.receive_gap_note(at + Duration::from_secs(61))
                .as_deref(),
            Some("最後の受信から30秒")
        );
        assert!(view.receive_at("a", at + Duration::from_secs(62)));
        view.receive_at("a", at + Duration::from_secs(12));
        assert_eq!(
            view.receive_gap_note(at + Duration::from_secs(91)),
            None,
            "late delivery cannot reverse the last receipt"
        );
        view.stream("a");
        assert_eq!(view.phase(), ActivityPhase::Receiving);
        assert_eq!(
            view.receive_gap_note(at + Duration::from_secs(92))
                .as_deref(),
            Some("最後の受信から30秒")
        );
    }

    #[test]
    fn receive_gap_is_attempt_scoped_and_resets_on_terminal_or_lost_observation() {
        let at = Instant::now();
        let mut view = ActivityView::default();
        // Transport headers may race the saved inflight projection.
        view.receive_at("a", at);
        view.accept_at(1, awaiting("a"), at + Duration::from_secs(10));
        assert_eq!(
            view.receive_gap_note(at + Duration::from_secs(30))
                .as_deref(),
            Some("最後の受信から30秒")
        );
        assert!(!view.receive_at("old", at + Duration::from_secs(31)));
        view.accept_at(2, awaiting("b"), at + Duration::from_secs(40));
        assert_eq!(view.receive_gap_note(at + Duration::from_secs(69)), None);
        view.receive_at("a", at + Duration::from_secs(70));
        assert_eq!(
            view.receive_gap_note(at + Duration::from_secs(70))
                .as_deref(),
            Some("受信待ち30秒（画面の観測開始から）")
        );
        assert!(!view.accept_at(1, awaiting("a"), at + Duration::from_secs(75)));
        view.unavailable_at(2, at + Duration::from_secs(80));
        assert_eq!(view.receive_gap_note(at + Duration::from_secs(999)), None);
        view.accept_at(3, awaiting("b"), at + Duration::from_secs(1000));
        assert_eq!(view.receive_gap_note(at + Duration::from_secs(1029)), None);
        view.accept_at(
            4,
            ActivitySnapshot {
                phase: ActivityPhase::Completed,
                ..awaiting("b")
            },
            at + Duration::from_secs(1030),
        );
        view.receive_at("b", at + Duration::from_secs(1040));
        assert_eq!(view.receive_gap_note(at + Duration::from_secs(9999)), None);
    }

    #[test]
    fn retry_wait_clears_receive_gap_and_new_attempt_restarts_observation() {
        let at = Instant::now();
        let mut view = ActivityView::default();
        view.accept_at(1, awaiting("first"), at);
        view.receive_at("first", at);
        assert!(
            view.receive_gap_note(at + Duration::from_secs(40))
                .is_some()
        );
        let retry = ActivitySnapshot {
            phase: ActivityPhase::Retrying,
            automatic_retry: Some(AutomaticRetrySnapshot {
                number: 1,
                limit: 2,
                ..Default::default()
            }),
            ..awaiting("first")
        };
        view.accept_at(2, retry, at + Duration::from_secs(41));
        assert_eq!(view.phase(), ActivityPhase::Retrying);
        assert!(
            view.receive_gap_note(at + Duration::from_secs(500))
                .is_none()
        );
        assert!(!view.receive_at("first", at + Duration::from_secs(42)));
        view.stream("first");
        assert_eq!(view.phase(), ActivityPhase::Retrying);
        view.accept_at(3, awaiting("next"), at + Duration::from_secs(43));
        assert_eq!(view.phase(), ActivityPhase::AwaitingModel);
        assert!(!view.receive_at("first", at + Duration::from_secs(44)));
        assert!(
            view.receive_gap_note(at + Duration::from_secs(72))
                .is_none()
        );
        assert_eq!(
            view.receive_gap_note(at + Duration::from_secs(73))
                .as_deref(),
            Some("受信待ち30秒（画面の観測開始から）")
        );
        view.accept_at(
            4,
            ActivitySnapshot {
                phase: ActivityPhase::Completed,
                turn_in_progress: false,
                ..awaiting("next")
            },
            at + Duration::from_secs(74),
        );
        assert_eq!(view.phase(), ActivityPhase::Completed);
        assert!(!view.phase().is_active());
        assert!(
            view.receive_gap_note(at + Duration::from_secs(900))
                .is_none()
        );
        assert!(!view.accept_at(2, awaiting("first"), at + Duration::from_secs(901)));
        assert_eq!(view.phase(), ActivityPhase::Completed);
    }

    #[test]
    fn animation_is_bounded_and_stops_without_changing_text() {
        let now = Instant::now();
        let mut animation = Animation::new(now);
        assert!(!animation.tick(now, ActivityPhase::Idle, true, 30));
        assert!(animation.tick(now, ActivityPhase::AwaitingModel, true, 30));
        assert!(!animation.tick(
            now + Duration::from_millis(50),
            ActivityPhase::AwaitingModel,
            true,
            30
        ));
        assert!(animation.tick(
            now + Duration::from_millis(100),
            ActivityPhase::AwaitingModel,
            true,
            30
        ));
        let a = animation.spans(ActivityPhase::AwaitingModel, Style::default(), now);
        let b = animation.spans(
            ActivityPhase::AwaitingModel,
            Style::default(),
            now + Duration::from_millis(600),
        );
        assert_eq!(
            a.iter().map(|s| s.content.as_ref()).collect::<String>(),
            b.iter().map(|s| s.content.as_ref()).collect::<String>()
        );
        assert_ne!(a, b);
        assert!(!animation.tick(
            now + Duration::from_secs(1),
            ActivityPhase::AwaitingModel,
            false,
            30
        ));
        assert!(!animation.tick(
            now + Duration::from_secs(2),
            ActivityPhase::Completed,
            true,
            30
        ));
    }
    #[test]
    fn first_chunk_before_a_new_attempt_projection_is_not_lost() {
        let mut view = ActivityView::default();
        view.accept(
            1,
            ActivitySnapshot {
                phase: ActivityPhase::Completed,
                ..Default::default()
            },
        );
        view.stream("new-attempt");
        assert_eq!(view.phase(), ActivityPhase::Completed);
        view.accept(
            2,
            ActivitySnapshot {
                phase: ActivityPhase::AwaitingModel,
                attempt_id: Some("new-attempt".into()),
                ..Default::default()
            },
        );
        assert_eq!(view.phase(), ActivityPhase::Receiving);
    }
    #[test]
    fn only_the_current_inflight_attempt_can_be_receiving() {
        let mut view = ActivityView::default();
        let snapshot = ActivitySnapshot {
            attempt_id: Some("attempt".into()),
            phase: ActivityPhase::AwaitingModel,
            ..Default::default()
        };
        assert!(view.accept(4, snapshot.clone()));
        view.stream("old");
        assert_eq!(view.phase(), ActivityPhase::AwaitingModel);
        view.stream("attempt");
        assert_eq!(view.phase(), ActivityPhase::Receiving);
        view.stream("old");
        assert_eq!(view.phase(), ActivityPhase::Receiving);
        assert!(!view.accept(4, snapshot));
        assert_eq!(view.phase(), ActivityPhase::Receiving);
        view.accept(
            5,
            ActivitySnapshot {
                phase: ActivityPhase::Completed,
                ..Default::default()
            },
        );
        view.stream("attempt");
        assert_eq!(view.phase(), ActivityPhase::Completed);
        assert!(!view.accept(
            3,
            ActivitySnapshot {
                phase: ActivityPhase::Running,
                ..Default::default()
            }
        ));
        assert_eq!(view.phase(), ActivityPhase::Completed);
    }

    #[test]
    fn turn_observation_clock_pauses_without_inventing_unobserved_or_restart_time() {
        let at = Instant::now();
        let active = ActivitySnapshot {
            run_id: Some("r".into()),
            turn_id: Some("u".into()),
            phase: ActivityPhase::Running,
            turn_in_progress: true,
            ..Default::default()
        };
        let mut view = ActivityView::default();
        view.accept_at(1, active.clone(), at);
        assert_eq!(
            view.elapsed_ms(at + Duration::from_secs(133)),
            Some(133_000)
        );
        let recovery = ActivitySnapshot {
            phase: ActivityPhase::Recovery,
            turn_in_progress: false,
            ..active.clone()
        };
        view.accept_at(1, recovery, at + Duration::from_secs(133));
        assert_eq!(
            view.elapsed_ms(at + Duration::from_secs(999)),
            Some(133_000)
        );
        assert!(!view.accept_at(0, active.clone(), at + Duration::from_secs(999)));
        assert_eq!(
            view.elapsed_ms(at + Duration::from_secs(1000)),
            Some(133_000)
        );
        view.accept_at(2, active.clone(), at + Duration::from_secs(2000));
        view.unavailable_at(2, at + Duration::from_secs(2007));
        assert_eq!(
            view.elapsed_ms(at + Duration::from_secs(9000)),
            Some(140_000)
        );
        let background = ActivitySnapshot {
            phase: ActivityPhase::Background,
            turn_in_progress: false,
            ..active.clone()
        };
        view.accept_at(2, background.clone(), at + Duration::from_secs(9000));
        assert_eq!(
            view.elapsed_ms(at + Duration::from_secs(10_000)),
            Some(140_000)
        );
        let mut restarted = ActivityView::default();
        restarted.accept_at(2, background, at);
        assert_eq!(restarted.elapsed_ms(at + Duration::from_secs(100)), None);
        view.accept_at(
            3,
            ActivitySnapshot {
                turn_id: Some("new".into()),
                ..active
            },
            at + Duration::from_secs(10_000),
        );
        assert_eq!(
            view.elapsed_ms(at + Duration::from_secs(10_001)),
            Some(1000)
        );
    }
    #[test]
    fn delayed_observation_timestamps_never_double_count_or_reverse_time() {
        let at = Instant::now();
        let active = ActivitySnapshot {
            run_id: Some("r".into()),
            turn_id: Some("u".into()),
            phase: ActivityPhase::Running,
            turn_in_progress: true,
            ..Default::default()
        };
        let mut view = ActivityView::default();
        view.accept_at(1, active.clone(), at);
        view.accept_at(1, active.clone(), at + Duration::from_secs(10));
        view.accept_at(1, active, at + Duration::from_secs(2));
        assert_eq!(view.elapsed_ms(at + Duration::from_secs(10)), Some(10_000));
        assert_eq!(view.elapsed_ms(at + Duration::from_secs(12)), Some(12_000));
    }

    #[test]
    fn late_resume_after_observation_loss_cannot_count_the_same_interval_twice() {
        let at = Instant::now();
        let active = ActivitySnapshot {
            run_id: Some("r".into()),
            turn_id: Some("u".into()),
            phase: ActivityPhase::Running,
            turn_in_progress: true,
            ..Default::default()
        };
        let mut view = ActivityView::default();
        view.accept_at(1, active.clone(), at);
        view.unavailable_at(1, at + Duration::from_secs(10));
        view.accept_at(1, active, at + Duration::from_secs(2));
        assert_eq!(view.elapsed_ms(at + Duration::from_secs(12)), Some(12_000));
    }
}
