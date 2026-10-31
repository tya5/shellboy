//! Session-only Watch projection. No I/O, workers, expiry timers or Core state.
//!
//! Prepare summaries/details on the presentation worker. The UI state accepts
//! owned prepared facts; clock ticks only update bounded visible age labels.
//! Seen completion identities use an LRU of 2000. After any eviction, absent
//! identities are explicitly uncertain unless a new completion transition was
//! positively observed during this open Watch session.
use super::live_work::{LiveWorkSnapshot, LiveWorkTarget};
use crate::{
    activity::ActivityPhase,
    protocol::{RevisitSummary, ThreadSummary},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub const MAX_ROWS: usize = 2000;
pub const MAX_VISIBLE_DETAILS: usize = 128;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowserMode {
    #[default]
    List,
    Watch,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WatchCategory {
    Recovery,
    Failed,
    Unread,
    Active,
    Background,
    Confirming,
    Read,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchAgeOrigin {
    /// A genuine timestamp for this state's transition, not Turn creation.
    SavedTransition,
    /// Time observed in this UI session, not an inferred saved transition.
    Observed,
}
#[derive(Clone, Debug)]
pub struct WatchAge {
    pub elapsed_base: Duration,
    pub observed_at: Instant,
    pub origin: WatchAgeOrigin,
}
#[derive(Clone, Debug)]
pub struct WatchDetail {
    pub thread_id: String,
    pub latest_run_id: Option<String>,
    pub answer_id: Option<String>,
    pub turn_id: Option<String>,
    pub turn_generation: Option<u64>,
    pub phase: ActivityPhase,
    pub parent_title: Option<String>,
    pub state_age: Option<WatchAge>,
    pub model_request_age: Option<WatchAge>,
    pub work: Option<LiveWorkSnapshot>,
    pub recovery_reason: Option<String>,
    pub answer_preview: Option<String>,
    pub pending_messages: u64,
    pub revisit: Option<RevisitSummary>,
}
#[derive(Clone, Debug)]
pub struct PreparedWatchDetail {
    facts: WatchDetail,
    pub parent_label: String,
    pub secondary: String,
    execution_id: Option<String>,
    stale_output: Option<u64>,
    initial_state_clock: String,
    initial_secondary_clock: String,
    work_clocks: Option<WorkClocks>,
}
#[derive(Clone, Debug)]
struct WorkClocks {
    oldest: Option<Duration>,
    latest_output: Option<Duration>,
    observed_at: Instant,
}
fn safe(text: &str, chars: usize) -> String {
    crate::presentation::sanitize_plain(&text.chars().take(chars).collect::<String>(), 0)
        .0
        .replace(['\n', '\r', '\t'], " ")
        .chars()
        .take(chars)
        .collect()
}
impl WatchDetail {
    pub fn prepare(mut self) -> PreparedWatchDetail {
        self.parent_title = self.parent_title.map(|text| safe(&text, 160));
        self.recovery_reason = self.recovery_reason.map(|text| safe(&text, 512));
        self.answer_preview = self.answer_preview.map(|text| {
            let bounded = text.chars().take(2048).collect::<String>();
            let first = bounded
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            super::threads::plain_preview(first, 160)
        });
        if self
            .work
            .as_ref()
            .is_some_and(|work| Some(work.run_id.as_str()) != self.latest_run_id.as_deref())
        {
            self.work = None;
        }
        if let Some(work) = &mut self.work {
            work.rows.truncate(3);
            for row in &mut work.rows {
                row.label = safe(&row.label, 512);
                row.state = safe(&row.state, 64);
            }
        }
        let execution_id = self.work.as_ref().and_then(|work| {
            work.rows.iter().find_map(|row| match &row.target {
                LiveWorkTarget::Shell { execution_id, .. } => Some(execution_id.clone()),
                _ => None,
            })
        });
        let stale_output = self
            .work
            .as_ref()
            .and_then(|work| {
                work.rows
                    .iter()
                    .filter_map(|row| {
                        matches!(row.target, LiveWorkTarget::Shell { .. })
                            .then_some(row.output_age_base)
                            .flatten()
                    })
                    .max()
            })
            .map(|age| age.as_secs());
        let work_clocks = self.work.as_ref().map(|work| WorkClocks {
            oldest: work.rows.iter().filter_map(|row| row.elapsed_base).max(),
            latest_output: work
                .rows
                .iter()
                .filter_map(|row| {
                    matches!(row.target, LiveWorkTarget::Shell { .. })
                        .then_some(row.output_age_base)
                        .flatten()
                })
                .min(),
            observed_at: work.observed_at,
        });
        let mut secondary = match self.phase {
            ActivityPhase::Recovery => format!(
                "{} · 詳細で復旧を確認",
                self.recovery_reason
                    .as_deref()
                    .unwrap_or("復旧理由は未取得")
            ),
            ActivityPhase::Failed => self
                .recovery_reason
                .clone()
                .unwrap_or_else(|| "失敗理由は未取得 · 詳細で確認".into()),
            ActivityPhase::Completed => self
                .answer_preview
                .clone()
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "回答の抜粋は未取得".into()),
            ActivityPhase::Background | ActivityPhase::Continuing => self
                .work
                .as_ref()
                .map(|work| {
                    format!(
                        "shell {} · 子 {} · 待機 {}{}",
                        work.shells,
                        work.children,
                        work.waits
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "未取得".into()),
                        work.rows
                            .first()
                            .map(|row| format!(" · {}", row.label))
                            .unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| "所有する作業の詳細は未取得".into()),
            _ => self
                .work
                .as_ref()
                .and_then(|work| {
                    work.rows
                        .iter()
                        .find(|row| matches!(row.target, LiveWorkTarget::Shell { .. }))
                })
                .map(|row| row.label.clone())
                .unwrap_or_else(|| self.phase.label().into()),
        };
        if self.pending_messages > 0 {
            secondary.push_str(&format!(" · 待ち {}", self.pending_messages));
        }
        if let Some(label) = self
            .revisit
            .as_ref()
            .and_then(crate::presentation::revisit_label)
        {
            secondary.push_str(" · ");
            secondary.push_str(&label);
        }
        let now = Instant::now();
        let initial_state_clock = self
            .state_age
            .as_ref()
            .map(|age| age_label(age, now))
            .unwrap_or_default();
        let initial_secondary_clock =
            secondary_age_label(self.model_request_age.as_ref(), work_clocks.as_ref(), now);
        PreparedWatchDetail {
            parent_label: self.parent_title.clone().unwrap_or_default(),
            facts: self,
            secondary,
            execution_id,
            stale_output,
            initial_state_clock,
            initial_secondary_clock,
            work_clocks,
        }
    }
}
impl PreparedWatchDetail {
    pub fn facts(&self) -> &WatchDetail {
        &self.facts
    }
}
#[derive(Clone, Debug)]
pub struct WatchSummary {
    pub thread_id: String,
    pub title: String,
    pub phase: ActivityPhase,
    pub updated_at: String,
    pub latest_run_id: Option<String>,
    pub answer_id: Option<String>,
}
impl WatchSummary {
    /// Worker-only bounded text preparation. updated_at is cache freshness, not unread identity.
    pub fn prepare(summary: &ThreadSummary) -> Self {
        Self {
            thread_id: summary.thread_id.clone(),
            title: safe(&summary.title, 160),
            phase: summary.activity,
            updated_at: summary.updated_at.clone(),
            latest_run_id: None,
            answer_id: None,
        }
    }
    pub fn with_identity(mut self, run: Option<String>, answer: Option<String>) -> Self {
        self.latest_run_id = run;
        self.answer_id = answer;
        self
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompletionIdentity {
    pub run_id: String,
    pub answer_id: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchActionKind {
    Stop,
    Retry,
    Output,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchActionTarget {
    pub browser_generation: u64,
    pub thread_id: String,
    pub run_id: String,
    pub turn_id: Option<String>,
    pub turn_generation: Option<u64>,
    pub phase: ActivityPhase,
    pub execution_id: Option<String>,
    pub kind: WatchActionKind,
}
struct Entry {
    summary: WatchSummary,
    detail: Option<PreparedWatchDetail>,
    read_sequence: u64,
    category: WatchCategory,
    ordinal: u64,
    stale_rank: Option<u64>,
    rank_initialized: bool,
    observed_in_session: bool,
    known_new_completion: bool,
    state_clock: String,
    secondary_clock: String,
    previous_observed: Option<(Option<CompletionIdentity>, Option<String>, ActivityPhase)>,
}
impl Entry {
    fn identity(&self) -> Option<CompletionIdentity> {
        Some(CompletionIdentity {
            run_id: self.summary.latest_run_id.clone()?,
            answer_id: self.summary.answer_id.clone()?,
        })
    }
}
#[derive(Default)]
pub struct WatchState {
    generation: u64,
    open: bool,
    entries: BTreeMap<String, Entry>,
    order: Vec<String>,
    retained: BTreeSet<String>,
    seen: BTreeMap<CompletionIdentity, u64>,
    seen_overflow: bool,
    clock: u64,
    tick_origin: Option<Instant>,
    tick_second: Option<u64>,
    limited: bool,
}
pub struct WatchRow<'a> {
    pub thread_id: &'a str,
    pub title: &'a str,
    pub parent_title: &'a str,
    pub category: WatchCategory,
    pub status: &'static str,
    pub state_age: &'a str,
    pub secondary: &'a str,
    pub secondary_age: &'a str,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleWatchScope {
    pub ids: Vec<String>,
    pub limited: bool,
}
fn ordinary_category(phase: ActivityPhase) -> WatchCategory {
    match phase {
        ActivityPhase::Recovery => WatchCategory::Recovery,
        ActivityPhase::Failed => WatchCategory::Failed,
        ActivityPhase::Background | ActivityPhase::Continuing => WatchCategory::Background,
        ActivityPhase::Idle | ActivityPhase::Stopped => WatchCategory::Read,
        ActivityPhase::Completed => WatchCategory::Confirming,
        _ => WatchCategory::Active,
    }
}
impl WatchState {
    pub fn open(&mut self, generation: u64, summaries: Vec<WatchSummary>) -> bool {
        if summaries.len() > MAX_ROWS {
            self.limited = true;
            return false;
        }
        self.open = true;
        self.limited = false;
        self.generation = generation;
        self.retained.clear();
        self.tick_origin = None;
        self.tick_second = None;
        let ids = summaries
            .iter()
            .map(|row| row.thread_id.as_str())
            .collect::<BTreeSet<_>>();
        self.entries.retain(|id, _| ids.contains(id.as_str()));
        for entry in self.entries.values_mut() {
            entry.observed_in_session = false;
            entry.known_new_completion = false;
            entry.read_sequence = 0;
            entry.previous_observed = None;
        }
        self.reconcile(generation, 0, summaries);
        self.rebuild();
        true
    }
    pub fn close(&mut self) {
        self.open = false;
        self.retained.clear();
        self.tick_origin = None;
        self.tick_second = None;
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn limited(&self) -> bool {
        self.limited
    }
    pub fn reconcile(
        &mut self,
        generation: u64,
        sequence: u64,
        summaries: Vec<WatchSummary>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        if summaries.len() > MAX_ROWS {
            self.limited = true;
            return false;
        }
        let new = summaries
            .iter()
            .map(|row| row.thread_id.as_str())
            .filter(|id| !self.entries.contains_key(*id))
            .collect::<BTreeSet<_>>()
            .len();
        if self.entries.len() + new > MAX_ROWS {
            self.limited = true;
            return false;
        }
        let mut changed = false;
        for mut summary in summaries {
            if summary.thread_id.starts_with("draft:") {
                continue;
            }
            if let Some(entry) = self.entries.get_mut(&summary.thread_id) {
                if sequence < entry.read_sequence {
                    continue;
                }
                let previous = entry.identity();
                let previous_phase = entry.summary.phase;
                let previous_run = entry.summary.latest_run_id.clone();
                let stale = entry.summary.updated_at != summary.updated_at
                    || entry.summary.phase != summary.phase
                    || (summary.latest_run_id.is_some()
                        && entry.summary.latest_run_id != summary.latest_run_id);
                if stale {
                    if entry.observed_in_session && entry.summary.latest_run_id.is_some() {
                        entry.previous_observed =
                            Some((previous.clone(), previous_run.clone(), previous_phase));
                    }
                    entry.detail = None;
                    entry.state_clock.clear();
                    entry.secondary_clock.clear();
                } else if summary.latest_run_id.is_none() {
                    summary.latest_run_id = entry.summary.latest_run_id.clone();
                    summary.answer_id = entry.summary.answer_id.clone();
                }
                entry.summary = summary;
                entry.read_sequence = sequence;
                if self.open
                    && entry.observed_in_session
                    && entry.summary.phase == ActivityPhase::Completed
                    && entry.identity().is_some()
                    && (previous
                        .as_ref()
                        .is_some_and(|old| Some(old) != entry.identity().as_ref())
                        || (previous_phase != ActivityPhase::Completed && previous_run.is_some()))
                {
                    entry.known_new_completion = true;
                }
                changed = true;
            } else {
                self.clock = self.clock.wrapping_add(1);
                self.entries.insert(
                    summary.thread_id.clone(),
                    Entry {
                        category: ordinary_category(summary.phase),
                        summary,
                        detail: None,
                        read_sequence: sequence,
                        ordinal: self.clock,
                        stale_rank: None,
                        rank_initialized: false,
                        observed_in_session: false,
                        known_new_completion: false,
                        state_clock: String::new(),
                        secondary_clock: String::new(),
                        previous_observed: None,
                    },
                );
                changed = true;
            }
        }
        if changed {
            self.reclassify();
            self.rebuild();
        }
        changed
    }
    pub fn remove(&mut self, generation: u64, ids: &[String]) -> bool {
        if generation != self.generation {
            return false;
        }
        let mut changed = false;
        for id in ids {
            changed |= self.entries.remove(id).is_some();
            self.retained.remove(id);
        }
        if changed {
            self.rebuild();
        }
        changed
    }
    pub fn apply_detail(
        &mut self,
        generation: u64,
        sequence: u64,
        detail: PreparedWatchDetail,
    ) -> bool {
        self.apply_details(generation, sequence, vec![detail])
    }
    pub fn apply_details(
        &mut self,
        generation: u64,
        sequence: u64,
        details: Vec<PreparedWatchDetail>,
    ) -> bool {
        if generation != self.generation || details.len() > MAX_VISIBLE_DETAILS {
            return false;
        }
        let mut changed = false;
        for detail in details {
            let id = detail.facts.thread_id.clone();
            let Some(entry) = self.entries.get_mut(&id) else {
                continue;
            };
            if sequence < entry.read_sequence {
                continue;
            }
            let previous = entry.identity().or_else(|| {
                entry
                    .previous_observed
                    .as_ref()
                    .and_then(|(identity, _, _)| identity.clone())
            });
            let previous_phase = if entry.summary.latest_run_id.is_some() {
                entry.summary.phase
            } else {
                entry
                    .previous_observed
                    .as_ref()
                    .map_or(entry.summary.phase, |(_, _, phase)| *phase)
            };
            let previous_run = entry.summary.latest_run_id.clone().or_else(|| {
                entry
                    .previous_observed
                    .as_ref()
                    .and_then(|(_, run, _)| run.clone())
            });
            let observed = entry.observed_in_session;
            entry.summary.phase = detail.facts.phase;
            entry.summary.latest_run_id = detail.facts.latest_run_id.clone();
            entry.summary.answer_id = detail.facts.answer_id.clone();
            entry.read_sequence = sequence;
            if self.open
                && observed
                && entry.summary.phase == ActivityPhase::Completed
                && entry.identity().is_some()
                && (previous
                    .as_ref()
                    .is_some_and(|old| Some(old) != entry.identity().as_ref())
                    || (previous_phase != ActivityPhase::Completed && previous_run.is_some()))
            {
                entry.known_new_completion = true;
            }
            entry.observed_in_session = self.open;
            entry.previous_observed = None;
            entry.state_clock = detail.initial_state_clock.clone();
            entry.secondary_clock = detail.initial_secondary_clock.clone();
            entry.detail = Some(detail);
            changed = true;
        }
        if changed {
            self.reclassify();
            self.rebuild();
        }
        changed
    }
    fn reclassify(&mut self) {
        for entry in self.entries.values_mut() {
            let category = if entry.summary.phase == ActivityPhase::Completed {
                match entry.identity() {
                    Some(key) if self.seen.contains_key(&key) => WatchCategory::Read,
                    Some(_) if !self.seen_overflow || entry.known_new_completion => {
                        WatchCategory::Unread
                    }
                    _ => WatchCategory::Confirming,
                }
            } else {
                ordinary_category(entry.summary.phase)
            };
            if category != entry.category {
                self.clock = self.clock.wrapping_add(1);
                entry.ordinal = self.clock;
                entry.category = category;
                entry.rank_initialized = false;
                entry.stale_rank = None;
            }
            if category == WatchCategory::Active
                && !entry.rank_initialized
                && let Some(detail) = &entry.detail
            {
                entry.stale_rank = detail.stale_output;
                entry.rank_initialized = true;
            }
        }
    }
    fn rebuild(&mut self) {
        self.order = self
            .entries
            .iter()
            .filter(|(id, entry)| {
                entry.category != WatchCategory::Read || self.retained.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        self.order.sort_by(|a, b| {
            let a = &self.entries[a];
            let b = &self.entries[b];
            a.category
                .cmp(&b.category)
                .then_with(|| {
                    if a.category == WatchCategory::Active {
                        b.stale_rank.cmp(&a.stale_rank)
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| a.ordinal.cmp(&b.ordinal))
                .then_with(|| a.summary.thread_id.cmp(&b.summary.thread_id))
        });
    }
    pub fn ordered_ids(&self) -> &[String] {
        &self.order
    }
    pub fn visible_scope(&mut self, start: usize, count: usize) -> VisibleWatchScope {
        let ids = self
            .order
            .iter()
            .skip(start)
            .take(count.min(MAX_VISIBLE_DETAILS))
            .cloned()
            .collect::<Vec<_>>();
        self.retained.extend(ids.iter().cloned());
        VisibleWatchScope {
            ids,
            limited: count > MAX_VISIBLE_DETAILS,
        }
    }
    pub fn row(&self, id: &str) -> Option<WatchRow<'_>> {
        let entry = self.entries.get(id)?;
        let detail = entry.detail.as_ref();
        let status = match entry.category {
            WatchCategory::Unread => "未読の完了",
            WatchCategory::Confirming if entry.identity().is_some() && self.seen_overflow => {
                "確認履歴の保持範囲外"
            }
            WatchCategory::Confirming => "確認中",
            WatchCategory::Read => {
                if entry.summary.phase == ActivityPhase::Completed {
                    "既読の完了"
                } else {
                    entry.summary.phase.column_label()
                }
            }
            _ => entry.summary.phase.column_label(),
        };
        Some(WatchRow {
            thread_id: &entry.summary.thread_id,
            title: &entry.summary.title,
            parent_title: detail.map_or("", |d| d.parent_label.as_str()),
            category: entry.category,
            status,
            state_age: &entry.state_clock,
            secondary: detail.map_or("詳細を確認中", |d| d.secondary.as_str()),
            secondary_age: &entry.secondary_clock,
        })
    }
    pub fn mark_seen(&mut self, thread: &str, key: CompletionIdentity) -> bool {
        self.clock = self.clock.wrapping_add(1);
        self.seen.insert(key.clone(), self.clock);
        if self.seen.len() > MAX_ROWS {
            if let Some(old) = self
                .seen
                .iter()
                .min_by_key(|(_, clock)| **clock)
                .map(|(key, _)| key.clone())
            {
                self.seen.remove(&old);
            }
            self.seen_overflow = true;
        }
        let changed = self
            .entries
            .get(thread)
            .is_some_and(|entry| entry.identity().as_ref() == Some(&key));
        if changed && let Some(entry) = self.entries.get_mut(thread) {
            entry.known_new_completion = false;
        }
        self.reclassify();
        self.rebuild();
        changed
    }
    pub fn mark_opened(&mut self, thread: &str) -> Option<CompletionIdentity> {
        let entry = self.entries.get(thread)?;
        if entry.summary.phase != ActivityPhase::Completed {
            return None;
        }
        let key = entry.identity()?;
        self.mark_seen(thread, key.clone());
        Some(key)
    }
    /// One local whole-second display update. Caller supplies actually visible IDs.
    pub fn tick(&mut self, visible: &[String], now: Instant) -> bool {
        if !self.open || visible.is_empty() {
            return false;
        }
        let origin = *self.tick_origin.get_or_insert(now);
        let second = now.saturating_duration_since(origin).as_secs();
        if self.tick_second == Some(second) {
            return false;
        }
        self.tick_second = Some(second);
        let mut changed = false;
        for id in visible.iter().take(MAX_VISIBLE_DETAILS) {
            let Some(entry) = self.entries.get_mut(id) else {
                continue;
            };
            let Some(detail) = &entry.detail else {
                continue;
            };
            let label = detail
                .facts
                .state_age
                .as_ref()
                .map(|age| age_label(age, now))
                .unwrap_or_default();
            let secondary = secondary_age_label(
                detail.facts.model_request_age.as_ref(),
                detail.work_clocks.as_ref(),
                now,
            );
            changed |= entry.state_clock != label || entry.secondary_clock != secondary;
            entry.state_clock = label;
            entry.secondary_clock = secondary;
        }
        changed
    }
    pub fn action_target(&self, thread: &str, kind: WatchActionKind) -> Option<WatchActionTarget> {
        let entry = self.entries.get(thread)?;
        let detail = entry.detail.as_ref()?;
        let facts = &detail.facts;
        match kind {
            WatchActionKind::Retry
                if facts.phase != ActivityPhase::Recovery
                    || facts.turn_id.is_none()
                    || facts.turn_generation.is_none() =>
            {
                return None;
            }
            WatchActionKind::Stop
                if matches!(
                    entry.category,
                    WatchCategory::Read
                        | WatchCategory::Unread
                        | WatchCategory::Confirming
                        | WatchCategory::Failed
                ) =>
            {
                return None;
            }
            WatchActionKind::Output if detail.execution_id.is_none() => return None,
            _ => {}
        }
        Some(WatchActionTarget {
            browser_generation: self.generation,
            thread_id: thread.into(),
            run_id: facts.latest_run_id.clone()?,
            turn_id: facts.turn_id.clone(),
            turn_generation: facts.turn_generation,
            phase: facts.phase,
            execution_id: detail.execution_id.clone(),
            kind,
        })
    }
    pub fn validates(&self, target: &WatchActionTarget) -> bool {
        if target.browser_generation != self.generation {
            return false;
        }
        let Some(current) = self.action_target(&target.thread_id, target.kind) else {
            return false;
        };
        current.run_id == target.run_id
            && (target.kind != WatchActionKind::Retry
                || (current.turn_id == target.turn_id
                    && current.turn_generation == target.turn_generation
                    && current.phase == ActivityPhase::Recovery))
            && (target.kind != WatchActionKind::Output
                || current.execution_id == target.execution_id)
    }
}
fn age_label(age: &WatchAge, now: Instant) -> String {
    let seconds = age
        .elapsed_base
        .saturating_add(now.saturating_duration_since(age.observed_at))
        .as_secs();
    format!(
        "{} {seconds}秒",
        match age.origin {
            WatchAgeOrigin::SavedTransition => "状態",
            WatchAgeOrigin::Observed => "観測",
        }
    )
}

fn secondary_age_label(
    model: Option<&WatchAge>,
    work: Option<&WorkClocks>,
    now: Instant,
) -> String {
    let mut labels = Vec::with_capacity(3);
    if let Some(age) = model {
        let seconds = age
            .elapsed_base
            .saturating_add(now.saturating_duration_since(age.observed_at))
            .as_secs();
        labels.push(format!(
            "モデル要求{} {seconds}秒",
            if age.origin == WatchAgeOrigin::Observed {
                "の観測"
            } else {
                "開始から"
            }
        ));
    }
    if let Some(work) = work {
        let since = now.saturating_duration_since(work.observed_at);
        if let Some(age) = work.oldest {
            labels.push(format!(
                "表示内最古の作業 {}秒",
                age.saturating_add(since).as_secs()
            ));
        }
        if let Some(age) = work.latest_output {
            labels.push(format!(
                "表示内の保存出力 {}秒前",
                age.saturating_add(since).as_secs()
            ));
        }
    }
    labels.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn summary(id: &str, phase: ActivityPhase) -> WatchSummary {
        WatchSummary {
            thread_id: id.into(),
            title: format!("title {id}"),
            phase,
            updated_at: "saved".into(),
            latest_run_id: None,
            answer_id: None,
        }
    }
    fn detail(id: &str, run: &str, answer: Option<&str>, phase: ActivityPhase) -> WatchDetail {
        WatchDetail {
            thread_id: id.into(),
            latest_run_id: Some(run.into()),
            answer_id: answer.map(str::to_owned),
            turn_id: Some("turn".into()),
            turn_generation: Some(1),
            phase,
            parent_title: Some("parent\u{1b}[31m\n title".into()),
            state_age: None,
            model_request_age: None,
            work: None,
            recovery_reason: None,
            answer_preview: Some("**first answer**\nsecond line".into()),
            pending_messages: 0,
            revisit: None,
        }
    }
    fn output_detail(id: &str, age: u64) -> PreparedWatchDetail {
        let mut facts = detail(id, id, None, ActivityPhase::Tools);
        facts.work = Some(LiveWorkSnapshot {
            run_id: id.into(),
            rows: vec![super::super::live_work::LiveWorkRow {
                target: LiveWorkTarget::Shell {
                    execution_id: format!("shell-{id}"),
                    turn_id: Some("turn".into()),
                },
                label: "printf \u{1b}[32mwork".into(),
                state: "running".into(),
                started_at: None,
                elapsed_base: None,
                last_output_at: Some("saved".into()),
                output_age_base: Some(Duration::from_secs(age)),
            }],
            shells: 1,
            children: 0,
            waits: Some(0),
            overflow: 0,
            observed_at: Instant::now(),
        });
        facts.prepare()
    }
    #[test]
    fn completed_without_identity_is_confirming_and_opened_completion_is_retained_until_reopen() {
        let mut state = WatchState::default();
        state.open(1, vec![summary("t", ActivityPhase::Completed)]);
        assert_eq!(state.row("t").unwrap().status, "確認中");
        state.apply_detail(
            1,
            10,
            detail("t", "run", Some("answer"), ActivityPhase::Completed).prepare(),
        );
        assert_eq!(state.row("t").unwrap().category, WatchCategory::Unread);
        let visible = state.visible_scope(0, 10);
        assert_eq!(visible.ids, ["t"]);
        let identity = state.mark_opened("t").unwrap();
        assert_eq!(identity.run_id, "run");
        assert_eq!(state.ordered_ids(), ["t"]);
        assert_eq!(state.row("t").unwrap().category, WatchCategory::Read);
        let mut settings = summary("t", ActivityPhase::Completed);
        settings.updated_at = "settings changed".into();
        state.reconcile(1, 11, vec![settings.clone()]);
        state.apply_detail(
            1,
            12,
            detail("t", "run", Some("answer"), ActivityPhase::Completed).prepare(),
        );
        assert_eq!(
            state.row("t").unwrap().category,
            WatchCategory::Read,
            "settings timestamp never makes an answer unread"
        );
        state.close();
        state.open(2, vec![settings]);
        assert!(
            state.ordered_ids().is_empty(),
            "read rows are pruned only on reopening"
        );
    }
    #[test]
    fn active_order_is_fixed_at_entry_and_category_transitions_reprioritize_but_clocks_do_not() {
        let mut state = WatchState::default();
        state.open(
            1,
            vec![
                summary("a", ActivityPhase::Tools),
                summary("b", ActivityPhase::Tools),
            ],
        );
        state.apply_details(1, 1, vec![output_detail("a", 5), output_detail("b", 20)]);
        assert_eq!(state.ordered_ids(), ["b", "a"]);
        state.apply_detail(1, 2, output_detail("a", 100));
        assert_eq!(
            state.ordered_ids(),
            ["b", "a"],
            "new output age cannot reorder an active category"
        );
        let mut recovery = detail("a", "a", None, ActivityPhase::Recovery);
        recovery.recovery_reason = Some("saved failure".into());
        state.apply_detail(1, 3, recovery.prepare());
        assert_eq!(state.ordered_ids(), ["a", "b"]);
        let ids = state.visible_scope(0, 2).ids;
        let at = Instant::now();
        state.tick(&ids, at);
        let order = state.ordered_ids().to_vec();
        for second in 1..100 {
            state.tick(&ids, at + Duration::from_secs(second));
            assert_eq!(state.ordered_ids(), order);
        }
    }
    #[test]
    fn visible_clocks_distinguish_saved_transition_from_observed_age_and_are_bounded() {
        let at = Instant::now();
        let mut facts = detail("t", "r", None, ActivityPhase::Recovery);
        facts.state_age = Some(WatchAge {
            elapsed_base: Duration::from_secs(90),
            observed_at: at,
            origin: WatchAgeOrigin::Observed,
        });
        let mut state = WatchState::default();
        state.open(1, vec![summary("t", ActivityPhase::Recovery)]);
        state.apply_detail(1, 1, facts.prepare());
        assert!(state.row("t").unwrap().state_age.starts_with("観測"));
        let visible = state.visible_scope(0, 129);
        assert!(visible.limited);
        assert!(visible.ids.len() <= 128);
        state.tick(&visible.ids, at);
        assert!(!state.tick(&visible.ids, at + Duration::from_millis(500)));
        assert!(state.tick(&visible.ids, at + Duration::from_secs(1)));
        let label = state.row("t").unwrap().state_age.to_owned();
        assert!(!state.tick(&[], at + Duration::from_secs(100)));
        assert_eq!(state.row("t").unwrap().state_age, label);
        state.close();
        assert!(!state.tick(&visible.ids, at + Duration::from_secs(200)));
        let prepared = detail("t", "r", None, ActivityPhase::Recovery).prepare();
        assert!(
            prepared.facts().state_age.is_none(),
            "missing transition cannot invent recovery age"
        );
    }
    #[test]
    fn seen_lru_overflow_is_uncertain_until_opened_or_a_new_completion_is_observed() {
        let mut state = WatchState::default();
        state.open(1, vec![summary("t", ActivityPhase::Completed)]);
        state.apply_detail(
            1,
            1,
            detail("t", "old-run", Some("old-answer"), ActivityPhase::Completed).prepare(),
        );
        state.visible_scope(0, 1);
        state.mark_opened("t");
        for n in 0..MAX_ROWS {
            state.mark_seen(
                "not-loaded",
                CompletionIdentity {
                    run_id: format!("r{n}"),
                    answer_id: format!("a{n}"),
                },
            );
        }
        assert_eq!(state.seen.len(), MAX_ROWS);
        assert!(state.seen_overflow);
        assert_eq!(state.row("t").unwrap().status, "確認履歴の保持範囲外");
        state.apply_detail(
            1,
            2,
            detail("t", "old-run", Some("old-answer"), ActivityPhase::Completed).prepare(),
        );
        assert_eq!(
            state.row("t").unwrap().category,
            WatchCategory::Confirming,
            "same old identity is not positively new"
        );
        state.apply_detail(
            1,
            3,
            detail("t", "new-run", None, ActivityPhase::AwaitingModel).prepare(),
        );
        let mut changed = summary("t", ActivityPhase::Completed);
        changed.updated_at = "new completion".into();
        state.reconcile(1, 4, vec![changed]);
        state.apply_detail(
            1,
            5,
            detail("t", "new-run", Some("new-answer"), ActivityPhase::Completed).prepare(),
        );
        assert_eq!(state.row("t").unwrap().category, WatchCategory::Unread);
        state.mark_opened("t");
        assert_eq!(state.row("t").unwrap().category, WatchCategory::Read);
    }
    #[test]
    fn action_targets_reject_replacement_run_recovery_generation_and_stale_browser_reply() {
        let mut state = WatchState::default();
        state.open(7, vec![summary("t", ActivityPhase::Recovery)]);
        state.apply_detail(
            7,
            10,
            detail("t", "r", None, ActivityPhase::Recovery).prepare(),
        );
        let target = state.action_target("t", WatchActionKind::Retry).unwrap();
        assert!(state.validates(&target));
        let mut later = detail("t", "r", None, ActivityPhase::Recovery);
        later.turn_generation = Some(2);
        state.apply_detail(7, 11, later.prepare());
        assert!(!state.validates(&target));
        let stop = state.action_target("t", WatchActionKind::Stop).unwrap();
        state.apply_detail(
            7,
            12,
            detail("t", "replacement", None, ActivityPhase::Recovery).prepare(),
        );
        assert!(!state.validates(&stop));
        assert!(!state.apply_detail(
            6,
            100,
            detail("t", "r", None, ActivityPhase::Recovery).prepare()
        ));
        assert!(!state.apply_detail(
            7,
            9,
            detail("t", "r", None, ActivityPhase::Recovery).prepare()
        ));
        let mut unknown = detail("t", "replacement", None, ActivityPhase::Recovery);
        unknown.turn_generation = None;
        state.apply_detail(7, 13, unknown.prepare());
        assert!(state.action_target("t", WatchActionKind::Retry).is_none());
    }
    #[test]
    fn preparation_sanitizes_external_text_and_caps_cache_without_fabricated_output_owner() {
        let prepared = output_detail("t", 2);
        assert!(!prepared.parent_label.contains(['\u{1b}', '\n']));
        assert!(!prepared.secondary.contains('\u{1b}'));
        let mut mismatch = detail("t", "r", Some("a"), ActivityPhase::Completed);
        mismatch.work = prepared.facts().work.clone();
        let mismatch = mismatch.prepare();
        assert!(mismatch.facts().work.is_none());
        assert_eq!(mismatch.secondary, "first answer");
        let mut state = WatchState::default();
        let too_many = (0..=MAX_ROWS)
            .map(|n| summary(&format!("t{n}"), ActivityPhase::Tools))
            .collect();
        assert!(!state.open(1, too_many));
        assert!(state.limited());
        assert!(state.entries.is_empty());
        assert!(state.open(2, vec![summary("valid", ActivityPhase::Tools)]));
        assert!(!state.limited());
    }
    #[test]
    fn work_ages_use_oldest_elapsed_latest_output_and_only_visible_one_second_ticks() {
        let at = Instant::now();
        let mut facts = detail("t", "t", None, ActivityPhase::Tools);
        let mut work = output_detail("t", 20).facts().work.clone().unwrap();
        work.observed_at = at;
        work.rows[0].elapsed_base = Some(Duration::from_secs(30));
        let mut another = work.rows[0].clone();
        another.target = LiveWorkTarget::Shell {
            execution_id: "second".into(),
            turn_id: None,
        };
        another.elapsed_base = Some(Duration::from_secs(90));
        another.output_age_base = Some(Duration::from_secs(5));
        work.rows.push(another);
        facts.work = Some(work);
        let mut state = WatchState::default();
        state.open(
            1,
            vec![
                summary("t", ActivityPhase::Tools),
                summary("b", ActivityPhase::Tools),
            ],
        );
        state.apply_details(1, 1, vec![facts.prepare(), output_detail("b", 10)]);
        assert_eq!(
            state.ordered_ids(),
            ["t", "b"],
            "stalled-shell priority uses maximum known silent age, not most recent output"
        );
        let ids = state.visible_scope(0, 1).ids;
        assert!(
            state
                .row("t")
                .unwrap()
                .secondary_age
                .contains("最古の作業 90秒")
        );
        assert!(
            state
                .row("t")
                .unwrap()
                .secondary_age
                .contains("保存出力 5秒前")
        );
        state.tick(&ids, at);
        let order = state.ordered_ids().to_vec();
        let mut changes = 0;
        let invisible = state.row("b").unwrap().secondary_age.to_owned();
        for tick in 1..=600 {
            changes += usize::from(state.tick(&ids, at + Duration::from_millis(tick * 100)));
            assert_eq!(state.ordered_ids(), order);
        }
        assert_eq!(changes, 60);
        assert_eq!(
            state.row("b").unwrap().secondary_age,
            invisible,
            "offscreen row clocks do not update"
        );
        let label = state.row("t").unwrap().secondary_age.to_owned();
        assert!(label.contains("最古の作業 150秒") && label.contains("保存出力 65秒前"));
        assert!(!state.tick(&[], at + Duration::from_secs(600)));
        assert_eq!(state.row("t").unwrap().secondary_age, label);
        let mut unknown = detail("t", "t", None, ActivityPhase::Background);
        let mut work = output_detail("t", 20).facts().work.clone().unwrap();
        work.rows[0].elapsed_base = None;
        work.rows[0].output_age_base = None;
        unknown.work = Some(work);
        state.apply_detail(1, 2, unknown.prepare());
        assert!(
            state.row("t").unwrap().secondary_age.is_empty(),
            "unrecorded ages cannot become zero-duration facts"
        );
    }
}
