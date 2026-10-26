//! Display facts and a local animation clock. No lifecycle writes, I/O or tasks.
use super::theme;
use ratatui::{style::Style, text::Span};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityPhase {
    #[default]
    Idle,
    Sending,
    Reflecting,
    Preparing,
    AwaitingModel,
    Receiving,
    Tools,
    Running,
    Waiting,
    Background,
    Continuing,
    Stopping,
    Recovery,
    Failed,
    Stopped,
    Completed,
}
impl ActivityPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "入力できます",
            Self::Sending => "送信受付中",
            Self::Reflecting => "保存済み・表示待ち",
            Self::Preparing => "準備中",
            Self::AwaitingModel => "モデル応答待ち",
            Self::Receiving => "応答受信中",
            Self::Tools => "ツール処理中",
            Self::Running => "処理中",
            Self::Waiting => "待機中",
            Self::Background => "回答済み背景処理中",
            Self::Continuing => "実行継続中",
            Self::Stopping => "停止処理中",
            Self::Recovery => "復旧待ち",
            Self::Failed => "異常終了",
            Self::Stopped => "中断済み",
            Self::Completed => "完了",
        }
    }
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
            Self::Waiting | Self::Background | Self::Continuing | Self::Stopping => Motion::Pulse,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivitySnapshot {
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
        let changed = self.snapshot.as_ref() != Some(&snapshot);
        self.snapshot = Some(snapshot);
        changed
    }
    pub fn unavailable_at(&mut self, epoch: u64, at: Instant) -> bool {
        if self.epoch.is_some_and(|current| epoch < current) {
            return false;
        }
        self.epoch = Some(epoch);
        if let Some(clock) = &mut self.clock {
            let running = clock.running;
            clock.observe(at, false);
            running
        } else {
            false
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
