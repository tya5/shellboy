//! Display facts and a local animation clock. No lifecycle writes, I/O or tasks.
mod timing;
use super::theme;
use crate::config::UiLanguage;
use crate::presentation::messages::{Message, phase_label};
use ratatui::{style::Style, text::Span};
use std::time::{Duration, Instant};
use timing::PhaseClock;
pub use timing::SavedTiming;

mod wait;
pub use crate::activity::ActivityPhase;
/// Stable footer vocabulary for the later #35 translation layer.
#[derive(Clone, Copy, Debug)]
pub enum FooterText {
    Observed,
    UnknownAge,
    UsageLimit,
    RateLimit,
    RecoveryActions,
    EndedUnknown,
}
impl FooterText {
    pub fn key(self) -> &'static str {
        match self {
            Self::Observed => "ui.activity.age.observed",
            Self::UnknownAge => "ui.activity.age.unknown",
            Self::UsageLimit => "ui.activity.limit.usage",
            Self::RateLimit => "ui.activity.limit.rate",
            Self::RecoveryActions => "ui.activity.limit.actions",
            Self::EndedUnknown => "ui.activity.ended_unknown",
        }
    }
    pub fn localized(self, language: UiLanguage) -> &'static str {
        if language == UiLanguage::Ja {
            return self.text();
        }
        match self {
            Self::Observed => "Observed ",
            Self::UnknownAge => "Elapsed time unavailable",
            Self::UsageLimit => "Usage limit",
            Self::RateLimit => "Rate limit",
            Self::RecoveryActions => crate::ui::text_width::glyphs::select(
                "Retry is manual after reset · /model to switch connection",
                "Retry is manual after reset | /model to switch connection",
            ),
            Self::EndedUnknown => "End time unavailable",
        }
    }
    pub fn text(self) -> &'static str {
        match self {
            Self::Observed => "観測 ",
            Self::UnknownAge => "経過未取得",
            Self::UsageLimit => "使用量上限",
            Self::RateLimit => "要求頻度の制限",
            Self::RecoveryActions => "待つ・時刻後も明示的に再試行 / /model で接続を選ぶ",
            Self::EndedUnknown => crate::presentation::TIME_TEXT[3].1,
        }
    }
}

#[cfg(test)]
pub(super) use wait::label as context_wait_label;
pub(super) use wait::label_localized as context_wait_label_localized;
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
            Self::Retrying | Self::Waiting | Self::Stopping => Motion::Pulse,
            _ => Motion::Still,
        }
    }
    fn marker(self) -> &'static str {
        match self {
            Self::Background | Self::Continuing | Self::Stopping => "◌",
            Self::Completed => "✓",
            Self::Recovery | Self::Failed => "!",
            Self::Stopped => crate::ui::text_width::glyphs::select("■", "#"),
            Self::Idle => crate::ui::text_width::glyphs::select("·", "|"),
            _ => crate::ui::text_width::glyphs::select("●", "*"),
        }
    }
}
pub(crate) fn count_label(count: u64, capped: bool, language: UiLanguage) -> String {
    match (language, capped) {
        (UiLanguage::En, true) => format!("at least {count}"),
        (UiLanguage::En, false) => count.to_string(),
        (UiLanguage::Ja, true) => format!("{count}件以上"),
        (UiLanguage::Ja, false) => format!("{count}件"),
    }
}

/// Existing saved facts for human recovery; never a new lifecycle state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoverySnapshot {
    pub resolved: bool,
    pub usage_limit: Option<crate::model::usage_limit::UsageLimit>,
    pub content_filter: Option<crate::model::content_filter::ContentFilterFacts>,
    pub stage: String,
    pub message: String,
    pub pending_messages: u64,
    pub saved_tool_results: u64,
    pub live_shells: u64,
    pub uncollected_shells: u64,
    pub counts_capped: bool,
    pub stop_reason: Option<String>,
}
impl RecoverySnapshot {
    pub fn limit_summary_localized(&self, language: UiLanguage) -> Option<String> {
        if language == UiLanguage::Ja {
            return self.limit_summary();
        }
        let limit = self.usage_limit?;
        let at = chrono::DateTime::from_timestamp(limit.resets_at, 0)?;
        let kind = match limit.kind {
            crate::model::usage_limit::LimitKind::UsageLimitReached => FooterText::UsageLimit,
            crate::model::usage_limit::LimitKind::RateLimitRetryAfter => FooterText::RateLimit,
        }
        .localized(language);
        Some(match limit.limit_window_minutes {
            Some(n) => crate::ui::text_width::glyphs::formatted!(
                "{kind} · Resets {} UTC · {n}-minute window",
                "{kind} | Resets {} UTC | {n}-minute window",
                at.format("%m-%d %H:%M")
            ),
            None => crate::ui::text_width::glyphs::formatted!(
                "{kind} · Resets {} UTC",
                "{kind} | Resets {} UTC",
                at.format("%m-%d %H:%M")
            ),
        })
    }
    pub fn limit_summary(&self) -> Option<String> {
        let limit = self.usage_limit?;
        let at = chrono::DateTime::from_timestamp(limit.resets_at, 0)?;
        let kind = match limit.kind {
            crate::model::usage_limit::LimitKind::UsageLimitReached => FooterText::UsageLimit,
            crate::model::usage_limit::LimitKind::RateLimitRetryAfter => FooterText::RateLimit,
        };
        Some(crate::ui::text_width::glyphs::formatted!(
            "{} · {} UTC{}",
            "{} | {} UTC{}",
            kind.text(),
            at.format("%m-%d %H:%M"),
            limit.limit_window_minutes.map_or(
                String::new(),
                |n| crate::ui::text_width::glyphs::formatted!(" · {n}分枠", " | {n}分枠")
            )
        ))
    }
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
    pub(super) fn label_localized(&self, language: UiLanguage) -> String {
        let wait = Message::Elapsed {
            ms: self.delay_ms.div_ceil(1000).saturating_mul(1000),
            decimal: false,
        }
        .render(language);
        // Consequences precede optional progress counts in constrained footers.
        match (language, self.partial_response_discarded) {
            (UiLanguage::En, true) => crate::ui::text_width::glyphs::formatted!(
                "Re-sending may be billed twice · Retry {}/{} in {wait} · Partial response saved; starting over",
                "Re-sending may be billed twice | Retry {}/{} in {wait} | Partial response saved; starting over",
                self.number,
                self.limit
            ),
            (UiLanguage::En, false) => crate::ui::text_width::glyphs::formatted!(
                "Re-sending may be billed twice · Retry {}/{} in {wait}",
                "Re-sending may be billed twice | Retry {}/{} in {wait}",
                self.number,
                self.limit
            ),
            (UiLanguage::Ja, true) => crate::ui::text_width::glyphs::formatted!(
                "再送は重複課金の可能性 · 再試行（{}/{}） · {wait}待機 · 途中応答を保存して作り直し",
                "再送は重複課金の可能性 | 再試行（{}/{}） | {wait}待機 | 途中応答を保存して作り直し",
                self.number,
                self.limit
            ),
            (UiLanguage::Ja, false) => crate::ui::text_width::glyphs::formatted!(
                "再送は重複課金の可能性 · 再試行（{}/{}） · {wait}待機",
                "再送は重複課金の可能性 | 再試行（{}/{}） | {wait}待機",
                self.number,
                self.limit
            ),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomaticCompactionSnapshot {
    pub input_tokens: u64,
    pub threshold: u64,
    pub started_at: String,
}
impl AutomaticCompactionSnapshot {
    pub fn label_localized(&self, language: UiLanguage) -> String {
        let input = self.input_tokens;
        let threshold = self.threshold;
        let sep = crate::ui::text_width::glyphs::separator();
        match language {
            UiLanguage::Ja => format!("自動圧縮中{sep}入力 {input} / 閾値 {threshold}"),
            UiLanguage::En => format!("Auto-compacting{sep}Input {input} / threshold {threshold}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivitySnapshot {
    pub timing: SavedTiming,
    pub tool_running: bool,
    /// Saved live-Run Revisit count/reservation, projected by the reader.
    pub revisit: Option<String>,
    /// Saved automatic-trim facts for this Turn/generation, if observed.
    pub context_reduction: Option<String>,
    /// Last admitted request contained at least one unavailable attachment.
    pub attachment_unavailable: bool,
    /// At least one unavailable attachment had an unrecognized binary format.
    pub attachment_unsupported: bool,
    /// The bounded attempt prefix did not prove all attachment states were read.
    pub attachment_check_limited: bool,
    pub automatic_retry: Option<AutomaticRetrySnapshot>,
    pub automatic_compaction: Option<AutomaticCompactionSnapshot>,
    /// Persisted index pause, read on the existing snapshot cadence.
    pub index_pause: Option<crate::index::PauseReason>,
    pub recovery: Option<RecoverySnapshot>,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub attempt_id: Option<String>,
    pub phase: ActivityPhase,
    pub turn_in_progress: bool,
    pub live_work: Option<super::live_work::LiveWorkSnapshot>,
    pub waiting_reason: Option<crate::protocol::RunWaitReason>,
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
    early_receive: Option<ReceiveObservation>,
    clock: Option<ObservedClock>,
    phase_clock: Option<PhaseClock>,
    request_clock: Option<PhaseClock>,
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
        let may_be_pre_inflight = matches!(
            snapshot.phase,
            ActivityPhase::AwaitingModel | ActivityPhase::Preparing | ActivityPhase::Running
        );
        if !may_be_pre_inflight && snapshot.attempt_id == self.received_attempt {
            self.received_attempt = None;
        }
        if snapshot.phase == ActivityPhase::AwaitingModel {
            if self
                .early_receive
                .as_ref()
                .is_some_and(|r| Some(&r.attempt) == snapshot.attempt_id.as_ref())
            {
                self.receive = self.early_receive.take();
                self.received_attempt = snapshot.attempt_id.clone();
            }
            if let Some(attempt) = &snapshot.attempt_id {
                if self
                    .receive
                    .as_ref()
                    .is_none_or(|receive| receive.attempt != *attempt)
                {
                    self.receive = Some(ReceiveObservation {
                        attempt: attempt.clone(),
                        observed_at: at,
                        first_at: at,
                        received: false,
                    });
                }
            } else {
                self.receive = None;
            }
        } else if !may_be_pre_inflight
            && self
                .receive
                .as_ref()
                .is_some_and(|r| snapshot.attempt_id.as_deref() == Some(&r.attempt))
        {
            self.receive = None;
        }
        let changed = self.snapshot.as_ref() != Some(&snapshot);
        self.snapshot = Some(snapshot);
        let previous_clock = self.phase_clock.take();
        self.phase_clock = PhaseClock::observe(self, previous_clock, at);
        let previous_request = self.request_clock.take();
        self.request_clock = PhaseClock::request(self, previous_request, at);
        changed
    }
    pub fn unavailable_at(&mut self, epoch: u64, at: Instant) -> bool {
        if self.epoch.is_some_and(|current| epoch < current) {
            return false;
        }
        self.epoch = Some(epoch);
        self.phase_clock = None;
        self.request_clock = None;
        self.early_receive = None;
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
        if let Some(age) = &mut self.phase_clock {
            age.commit(at);
        }
        if let Some(age) = &mut self.request_clock {
            age.commit(at);
        }

        if let Some(clock) = &mut self.clock
            && clock.running
        {
            clock.observe(at, true);
        }
    }
    #[cfg(test)]
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
        self.receive_at(attempt, Instant::now());
    }

    /// Headers and any body bytes are liveness observations, not assistant text.
    /// Keep one candidate when an observation precedes its saved projection.
    pub fn receive_at(&mut self, attempt: &str, at: Instant) -> bool {
        if let Some(snapshot) = &self.snapshot {
            if matches!(
                snapshot.phase,
                ActivityPhase::AwaitingModel | ActivityPhase::Preparing | ActivityPhase::Running
            ) {
                if snapshot.attempt_id.is_some() && snapshot.attempt_id.as_deref() != Some(attempt)
                {
                    // Retain one candidate without demoting the currently observed attempt.
                    match &mut self.early_receive {
                        Some(r) if r.attempt == attempt => {
                            r.first_at = r.first_at.min(at);
                            r.observed_at = r.observed_at.max(at);
                        }
                        Some(r) if r.observed_at > at => {}
                        _ => {
                            self.early_receive = Some(ReceiveObservation {
                                attempt: attempt.into(),
                                first_at: at,
                                observed_at: at,
                                received: true,
                            })
                        }
                    }
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
                if !receive.received {
                    receive.first_at = at;
                }
                receive.received = true;
            }
            _ => {
                self.receive = Some(ReceiveObservation {
                    attempt: attempt.into(),
                    observed_at: at,
                    first_at: at,
                    received: true,
                })
            }
        }
        let before = self.phase();
        let before_secondary = self.secondary_phase();
        self.received_attempt = Some(attempt.into());
        let previous_clock = self.phase_clock.take();
        self.phase_clock = PhaseClock::observe(self, previous_clock, at);
        let previous_request = self.request_clock.take();
        self.request_clock = PhaseClock::request(self, previous_request, at);
        was_visible || before != self.phase() || before_secondary != self.secondary_phase()
    }

    #[cfg(test)]
    pub fn phase_time_note(&self, at: Instant) -> Option<String> {
        self.phase_time_note_localized(at, UiLanguage::Ja)
    }
    pub fn phase_time_note_localized(&self, at: Instant, language: UiLanguage) -> Option<String> {
        self.phase_clock
            .as_ref()
            .map(|clock| clock.note_localized(at, language))
    }
    pub fn secondary_phase(&self) -> Option<ActivityPhase> {
        let s = self.snapshot.as_ref()?;
        (self.phase() == ActivityPhase::Tools && s.phase == ActivityPhase::AwaitingModel).then(
            || {
                if s.attempt_id.is_some() && self.received_attempt == s.attempt_id {
                    ActivityPhase::Receiving
                } else {
                    ActivityPhase::AwaitingModel
                }
            },
        )
    }

    #[cfg(test)]
    pub fn secondary_note(&self, at: Instant) -> Option<String> {
        self.secondary_note_localized(at, UiLanguage::Ja)
    }
    pub fn secondary_note_localized(&self, at: Instant, language: UiLanguage) -> Option<String> {
        let phase = self.secondary_phase()?;
        Some(crate::ui::text_width::glyphs::formatted!(
            "{} · {}",
            "{} | {}",
            phase_label(phase, language),
            self.request_clock.as_ref().map_or_else(
                || FooterText::UnknownAge.localized(language).to_owned(),
                |clock| clock.note_localized(at, language)
            )
        ))
    }

    pub fn receive_gap_note(&self, now: Instant) -> Option<String> {
        self.receive_gap_note_localized(now, UiLanguage::Ja)
    }
    pub fn receive_gap_note_localized(&self, now: Instant, language: UiLanguage) -> Option<String> {
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
        let elapsed = Message::Elapsed {
            ms: seconds.saturating_mul(1000),
            decimal: false,
        }
        .render(language);
        Some(match (language, receive.received) {
            (UiLanguage::En, true) => format!("{elapsed} since last receive"),
            (UiLanguage::En, false) => {
                format!("Waiting to receive for {elapsed} (observed on this screen)")
            }
            (UiLanguage::Ja, true) => format!("最後の受信から{elapsed}"),
            (UiLanguage::Ja, false) => format!("受信待ち{elapsed}（画面の観測開始から）"),
        })
    }

    pub fn phase(&self) -> ActivityPhase {
        let Some(snapshot) = &self.snapshot else {
            return ActivityPhase::Idle;
        };
        if snapshot.tool_running
            && matches!(
                snapshot.phase,
                ActivityPhase::AwaitingModel
                    | ActivityPhase::Running
                    | ActivityPhase::Retrying
                    | ActivityPhase::Tools
            )
        {
            return ActivityPhase::Tools;
        }
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
    first_at: Instant,
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

/// What may move in the next frame: at most one decoration per region (#90).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MotionDemand {
    /// Status bar: the existing shimmer/pulse of the current phase.
    pub footer: ActivityPhase,
    /// Conversation: `motion::select` found a winner.
    pub spinner: bool,
    /// Owned-work heading: the section's existing seconds origin while its
    /// heading is drawn with a palette that can change brightness. Blink
    /// flips land on the same boundaries as the section's elapsed seconds.
    pub blink: Option<Instant>,
}
impl MotionDemand {
    #[cfg(test)]
    pub fn still(footer: ActivityPhase) -> Self {
        Self {
            footer,
            spinner: false,
            blink: None,
        }
    }
    fn fast(self) -> bool {
        self.footer.is_active() || self.spinner
    }
}

/// The single display clock for every decoration: footer shimmer/pulse,
/// conversation spinner and owned-work heading blink. Fast decorations share
/// one `max_fps`-bounded (<=10fps) deadline; blink alone wakes once per
/// second on the section's seconds boundary. Time changes styles or the
/// one-column spinner glyph, never text width or layout keys.
pub(super) struct Animation {
    origin: Instant,
    next: Option<Instant>,
    phase: ActivityPhase,
    fast: bool,
    /// The demand last scheduled while visible with motion on; drawing reads
    /// frames only from it, so motion off and hidden frames stay static.
    demand: Option<MotionDemand>,
}
impl Animation {
    pub fn new(now: Instant) -> Self {
        Self {
            origin: now,
            next: None,
            phase: ActivityPhase::Idle,
            fast: false,
            demand: None,
        }
    }
    #[cfg(test)]
    pub fn tick(
        &mut self,
        now: Instant,
        phase: ActivityPhase,
        visible: bool,
        max_fps: u32,
    ) -> bool {
        self.tick_demand(now, MotionDemand::still(phase), visible, max_fps, true)
    }
    /// Returns whether a frame is due. Never invalidates when nothing moves.
    pub fn tick_demand(
        &mut self,
        now: Instant,
        demand: MotionDemand,
        visible: bool,
        max_fps: u32,
        motion: bool,
    ) -> bool {
        if !visible || !motion || (!demand.fast() && demand.blink.is_none()) {
            self.next = None;
            self.phase = demand.footer;
            self.fast = false;
            self.demand = None;
            return false;
        }
        let previous = self.demand.replace(demand);
        if demand.fast() {
            let step = Duration::from_secs_f64(1.0 / f64::from(max_fps.clamp(1, 10)));
            if self.phase != demand.footer || self.next.is_none() || !self.fast {
                self.phase = demand.footer;
                self.fast = true;
                self.origin = now;
                self.next = Some(now + step);
                return true;
            }
            if self.next.is_some_and(|deadline| now >= deadline) {
                self.next = Some(now + step);
                return true;
            }
            return false;
        }
        // Quiet: only the heading blinks. Wake on the section's own second
        // boundary so blink and elapsed seconds share one frame per second.
        let origin = demand.blink.expect("quiet demand has a blink origin");
        let boundary = origin
            + Duration::from_secs(
                now.saturating_duration_since(origin)
                    .as_secs()
                    .saturating_add(1),
            );
        let restarted = self.fast
            || self.next.is_none()
            || previous.and_then(|previous| previous.blink) != Some(origin);
        self.phase = demand.footer;
        self.fast = false;
        if restarted {
            self.next = Some(boundary);
            return false;
        }
        if self.next.is_some_and(|deadline| now >= deadline) {
            self.next = Some(boundary);
            return true;
        }
        false
    }
    /// The conversation spinner glyph for this frame, if it may move.
    pub fn spinner(&self, now: Instant) -> Option<&'static str> {
        self.demand.filter(|demand| demand.spinner)?;
        let tenths = now.saturating_duration_since(self.origin).as_millis() / 100;
        Some(super::motion::spinner_frame(tenths as usize))
    }
    /// Whether the owned-work heading mark is in its dim half-period.
    /// At most one transition per second, on the section's seconds boundary.
    pub fn blink_dim(&self, now: Instant) -> bool {
        self.demand
            .and_then(|demand| demand.blink)
            .is_some_and(|origin| now.saturating_duration_since(origin).as_secs() % 2 == 1)
    }
    /// Footer decorations freeze with the other regions when motion is off.
    pub fn footer_moves(&self) -> bool {
        self.demand.is_some()
    }
    #[cfg(test)]
    pub fn spans(&self, phase: ActivityPhase, base: Style, now: Instant) -> Vec<Span<'static>> {
        self.spans_localized(phase, base, now, UiLanguage::Ja)
    }
    pub fn spans_localized(
        &self,
        phase: ActivityPhase,
        base: Style,
        now: Instant,
        language: UiLanguage,
    ) -> Vec<Span<'static>> {
        let text = format!("{} {}", phase.marker(), phase_label(phase, language));
        let elapsed = now.saturating_duration_since(self.origin).as_secs_f64();
        let motion = if self.footer_moves() {
            phase.motion()
        } else {
            Motion::Still
        };
        match motion {
            Motion::Still => vec![Span::styled(text, base)],
            Motion::Pulse => {
                let level = (1.0 + (elapsed * std::f64::consts::PI).sin()) / 2.0;
                vec![
                    Span::styled(
                        phase.marker().to_owned(),
                        base.fg(theme::activity_pulse(level)),
                    ),
                    Span::styled(format!(" {}", phase_label(phase, language)), base),
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
    fn automatic_compaction_labels_use_the_shared_separator() {
        use crate::ui::text_width;
        let activity = AutomaticCompactionSnapshot {
            input_tokens: 203000,
            threshold: 200000,
            started_at: "2026-11-02T10:00:00Z".into(),
        };
        for (policy, separator) in [(Some(1), " · "), (Some(2), " | "), (None, " | ")] {
            text_width::set_terminal_width(policy);
            assert_eq!(
                activity.label_localized(UiLanguage::Ja),
                format!("自動圧縮中{separator}入力 203000 / 閾値 200000")
            );
            assert_eq!(
                activity.label_localized(UiLanguage::En),
                format!("Auto-compacting{separator}Input 203000 / threshold 200000")
            );
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn activity_markers_and_retry_text_follow_measured_width() {
        use crate::ui::text_width::{self, UnicodeWidthStr};
        let retry = AutomaticRetrySnapshot {
            number: 1,
            limit: 3,
            delay_ms: 1500,
            partial_response_discarded: true,
        };
        for (policy, stopped, idle, active, separator) in [
            (Some(1), "■", "·", "●", " · "),
            (Some(2), "#", "|", "*", " | "),
            (None, "#", "|", "*", " | "),
        ] {
            text_width::set_terminal_width(policy);
            assert_eq!(ActivityPhase::Stopped.marker(), stopped);
            assert_eq!(ActivityPhase::Idle.marker(), idle);
            assert_eq!(ActivityPhase::Running.marker(), active);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let text = retry.label_localized(language);
                let separators = if language == UiLanguage::Ja { 3 } else { 2 };
                assert_eq!(
                    text.matches(separator).count(),
                    separators,
                    "{policy:?} {language:?}: {text}"
                );
                assert!(text.contains("1/3"), "{text}");
                assert!(text.width() <= 120, "{text}");
            }
        }
        text_width::clear_terminal_width();
    }
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
        // #85: header/body liveness changes transport phase, not text progress.
        assert_eq!(view.phase(), ActivityPhase::Receiving);
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
    fn one_clock_serves_spinner_and_blink_and_switches_to_quiet_seconds() {
        let now = Instant::now();
        let origin = now - Duration::from_millis(300);
        let mut animation = Animation::new(now);
        let spinner = MotionDemand {
            footer: ActivityPhase::Background,
            spinner: true,
            blink: Some(origin),
        };
        let mut frames = Vec::new();
        for tick in 0..=40u64 {
            let at = now + Duration::from_millis(tick * 25);
            if animation.tick_demand(at, spinner, true, 60, true) {
                frames.push(at);
            }
        }
        // Spinner alone (still footer) uses the same <=10fps step.
        assert_eq!(frames.len(), 11);
        assert!(
            frames
                .windows(2)
                .all(|w| w[1] - w[0] >= Duration::from_millis(100))
        );
        let glyphs = frames
            .iter()
            .filter_map(|at| animation.spinner(*at))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(glyphs.len() > 1);
        // Turn ends: only the heading remains, on the origin's second boundary.
        let quiet = MotionDemand {
            spinner: false,
            ..spinner
        };
        let start = now + Duration::from_millis(1_050);
        assert!(!animation.tick_demand(start, quiet, true, 60, true));
        assert_eq!(animation.spinner(start), None);
        let mut due = Vec::new();
        for tick in 1..=200u64 {
            let at = start + Duration::from_millis(tick * 25);
            if animation.tick_demand(at, quiet, true, 60, true) {
                due.push(at.saturating_duration_since(origin).as_millis() % 1000);
            }
        }
        assert_eq!(due.len(), 5, "{due:?}");
        assert!(due.iter().all(|offset| *offset < 25), "{due:?}");
        // Nothing moving: no deadline at all.
        let idle = MotionDemand::still(ActivityPhase::Completed);
        for tick in 0..100u64 {
            assert!(!animation.tick_demand(
                start + Duration::from_secs(10 + tick),
                idle,
                true,
                60,
                true
            ));
        }
        assert!(!animation.blink_dim(start + Duration::from_secs(11)));
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

#[cfg(test)]
#[path = "activity/i18n_tests.rs"]
mod i18n_tests;
