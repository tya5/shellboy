//! Bounded fixed-region projection of work owned by the selected Run.
//! Saved elapsed bases are independent of the UI-observed Turn clock.
use super::*;
use crate::config::UiLanguage;
use crate::ui::text_width::glyphs;

fn phase_label(state: &str, language: UiLanguage) -> Option<&'static str> {
    use crate::activity::ActivityPhase::*;
    [
        Idle,
        Sending,
        Reflecting,
        Preparing,
        AwaitingModel,
        Retrying,
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
    ]
    .into_iter()
    .find(|phase| phase.column_label() == state || phase.column_label_for(UiLanguage::En) == state)
    .map(|phase| super::watch::phase_label(phase, language, false))
}

#[derive(Clone, Copy)]
enum WorkMessage<'a> {
    State(&'a str),
    ChildState(&'a str),
    Heading {
        empty: bool,
        folded: bool,
    },
    Counts {
        shells: u64,
        children: u64,
        waits: Option<u64>,
    },
    More(u64),
    OutputAge(u64),
    Elapsed(u64),
    Oldest(u64),
    Deadline(Option<u64>),
    PreviewLimit,
    PreviewNotice,
    PreviewLoading,
}
impl<'a> WorkMessage<'a> {
    fn state(row: &'a LiveWorkRow) -> Self {
        if matches!(row.target, LiveWorkTarget::ChildRun { .. }) {
            Self::ChildState(&row.state)
        } else {
            Self::State(&row.state)
        }
    }
    fn render(self, language: UiLanguage) -> String {
        use UiLanguage::{En, Ja};
        let rendered: String = match (language, self) {
            (_, Self::ChildState(state)) => phase_label(state, language)
                .map(str::to_owned)
                .unwrap_or_else(|| Self::State(state).render(language)),
            (En, Self::State(state)) => phase_label(state, language)
                .unwrap_or(match state {
                    "starting" => "Starting",
                    "running" | "live" => "Running",
                    "stopping" => "Stopping",
                    "waiting" => "Waiting",
                    other => other,
                })
                .into(),
            (Ja, Self::State(state)) => phase_label(state, language)
                .unwrap_or(match state {
                    "starting" => "起動中",
                    "running" | "live" => "実行中",
                    "stopping" => "停止中",
                    "waiting" => "待機中",
                    other => other,
                })
                .into(),
            (En, Self::Heading { empty: true, .. }) => "Owned work | None".into(),
            (Ja, Self::Heading { empty: true, .. }) => "抱えている作業 | なし".into(),
            (En, Self::Heading { folded: true, .. }) => "Owned work | Folded".into(),
            (Ja, Self::Heading { folded: true, .. }) => "抱えている作業 | 折りたたみ".into(),
            (En, Self::Heading { .. }) => "Owned work".into(),
            (Ja, Self::Heading { .. }) => "抱えている作業".into(),
            (
                En,
                Self::Counts {
                    shells,
                    children,
                    waits: Some(waits),
                },
            ) => format!("Shells: {shells} | Child runs: {children} | Waits: {waits}"),
            (
                En,
                Self::Counts {
                    shells,
                    children,
                    waits: None,
                },
            ) => format!("Shells: {shells} | Child runs: {children} | Waits: unknown"),
            (
                Ja,
                Self::Counts {
                    shells,
                    children,
                    waits: Some(waits),
                },
            ) => format!("shell {shells} | 子 {children} | 待機 {waits}"),
            (
                Ja,
                Self::Counts {
                    shells,
                    children,
                    waits: None,
                },
            ) => format!("shell {shells} | 子 {children} | 待機 未取得"),
            (En, Self::More(1)) => "1 more item".into(),
            (En, Self::More(count)) => format!("{count} more items"),
            (Ja, Self::More(count)) => format!("他{count}件"),
            (En, Self::Elapsed(seconds)) => format!("{seconds}s"),
            (Ja, Self::Elapsed(seconds)) => {
                crate::presentation::elapsed_text(seconds.saturating_mul(1000), false)
            }
            (En, Self::OutputAge(seconds)) => format!("Last output: {seconds}s ago"),
            (Ja, Self::OutputAge(seconds)) => format!(
                "最後の出力 {}前",
                crate::presentation::elapsed_text(seconds.saturating_mul(1000), false)
            ),
            (En, Self::Oldest(seconds)) => format!("Oldest work: {seconds}s"),
            (Ja, Self::Oldest(seconds)) => format!(
                "最古 {}",
                crate::presentation::elapsed_text(seconds.saturating_mul(1000), false)
            ),
            (En, Self::Deadline(Some(seconds))) => format!("Deadline estimate: {seconds}s"),
            (Ja, Self::Deadline(Some(seconds))) => format!(
                "期限目安 {}",
                crate::presentation::elapsed_text(seconds.saturating_mul(1000), false)
            ),
            (En, Self::Deadline(None)) => "Deadline unknown".into(),
            (Ja, Self::Deadline(None)) => "期限不明".into(),
            (En, Self::PreviewLimit) => "Output exceeds display limit | F6 Details".into(),
            (Ja, Self::PreviewLimit) => "出力の表示上限を超えました | F6 詳細".into(),
            (En, Self::PreviewNotice) => "  |   | . Display excerpt | F6 Details".into(),
            (Ja, Self::PreviewNotice) => "  |   | . 表示用抜粋 | F6 詳細".into(),
            (En, Self::PreviewLoading) => "  |   | Loading output.".into(),
            (Ja, Self::PreviewLoading) => "  |   | 出力を取得中.".into(),
        };
        match self {
            Self::State(_) | Self::ChildState(_) => rendered,
            Self::PreviewNotice | Self::PreviewLoading => {
                let prefix = format!("  {}   {} ", glyphs::vertical(), glyphs::vertical());
                let body = rendered.strip_prefix("  |   | ").unwrap_or(&rendered);
                let body = match self {
                    Self::PreviewNotice => format!(
                        "{}{}",
                        glyphs::ellipsis(),
                        body.strip_prefix('.').unwrap_or(body)
                    ),
                    Self::PreviewLoading => format!(
                        "{}{}",
                        body.strip_suffix('.').unwrap_or(body),
                        glyphs::ellipsis()
                    ),
                    _ => unreachable!(),
                };
                format!("{prefix}{}", glyphs::decorate(&body))
            }
            _ => glyphs::decorate(&rendered),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LiveWorkTarget {
    Shell {
        execution_id: String,
        turn_id: Option<String>,
    },
    ChildRun {
        run_id: String,
        thread_id: String,
    },
    Wait {
        operation_id: String,
        execution_id: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitInfo {
    pub target: Option<WaitTarget>,
    pub timeout: Option<Duration>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitTarget {
    Shell(String),
    Run(String),
    Thread(String),
}
#[derive(Clone, Debug)]
pub struct LiveWorkRow {
    pub target: LiveWorkTarget,
    pub label: String,
    pub description: Option<String>,
    pub state: String,
    pub started_at: Option<String>,
    pub elapsed_base: Option<Duration>,
    pub last_output_at: Option<String>,
    pub wait_info: Option<WaitInfo>,
    pub output_age_base: Option<Duration>,
}
impl PartialEq for LiveWorkRow {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
            && self.label == other.label
            && self.description == other.description
            && self.state == other.state
            && self.started_at == other.started_at
            && self.last_output_at == other.last_output_at
            && self.wait_info == other.wait_info
    }
}
impl Eq for LiveWorkRow {}
#[derive(Clone, Debug)]
pub struct LiveWorkSnapshot {
    pub run_id: String,
    pub rows: Vec<LiveWorkRow>,
    pub shells: u64,
    pub children: u64,
    pub waits: Option<u64>,
    pub overflow: u64,
    pub counts_capped: bool,
    pub observed_at: Instant,
}
impl PartialEq for LiveWorkSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.run_id == other.run_id
            && self.rows == other.rows
            && self.shells == other.shells
            && self.children == other.children
            && self.waits == other.waits
            && self.overflow == other.overflow
            && self.counts_capped == other.counts_capped
    }
}
impl Eq for LiveWorkSnapshot {}

fn counts_text(snapshot: &LiveWorkSnapshot, language: UiLanguage) -> String {
    if !snapshot.counts_capped {
        return WorkMessage::Counts {
            shells: snapshot.shells,
            children: snapshot.children,
            waits: snapshot.waits,
        }
        .render(language);
    }
    let shells = super::activity::count_label(snapshot.shells, true, language);
    let children = super::activity::count_label(snapshot.children, true, language);
    let waits = snapshot.waits.map(|n| n.to_string()).unwrap_or_else(|| {
        match language {
            UiLanguage::En => "unknown",
            UiLanguage::Ja => "未取得",
        }
        .into()
    });
    match language {
        UiLanguage::En => format!("Shells: {shells} | Child runs: {children} | Waits: {waits}"),
        UiLanguage::Ja => format!("shell {shells} | 子 {children} | 待機 {waits}"),
    }
}

fn more_text(count: u64, capped: bool, language: UiLanguage) -> String {
    if !capped {
        return WorkMessage::More(count).render(language);
    }
    match language {
        UiLanguage::En => format!("at least {count} more items"),
        UiLanguage::Ja => format!("他{count}件以上"),
    }
}
#[derive(Clone, Debug)]
pub struct LiveOutputPreview {
    pub stdout: String,
    pub stderr: String,
    pub decode_uncertain: bool,
    pub skipped: bool,
    pub complete: bool,
}
#[derive(Debug, Default)]
pub(super) struct LiveWorkView {
    language: UiLanguage,
    snapshot: Option<LiveWorkSnapshot>,
    pub active: bool,
    selected: Option<LiveWorkTarget>,
    open: Option<String>,
    preview: Option<LiveOutputPreview>,
    error: Option<String>,
    markers: BTreeMap<String, Instant>,
    seconds: Vec<(Option<u64>, Option<u64>)>,
    clock_origin: Option<Instant>,
    clock_step: Option<u64>,
    pub area: Option<Rect>,
    visible_rows: usize,
    /// Section fold and fixed body size, synced from the UI each layout (#131).
    collapsed: bool,
    capacity: usize,
    revision: u64,
    published: Option<(Vec<String>, Option<String>)>,
}
fn bounded(text: &str, limit: usize) -> String {
    crate::presentation::sanitize_plain(&text.chars().take(limit).collect::<String>(), 0)
        .0
        .replace(['\n', '\r', '\t'], " ")
        .chars()
        .take(limit)
        .collect()
}
fn byte_boundary(text: &str, limit: usize) -> usize {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}
fn fit(text: &str, width: usize) -> String {
    use crate::ui::text_width::UnicodeWidthStr;
    use unicode_segmentation::UnicodeSegmentation;
    if text.width() <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for part in text.graphemes(true) {
        if used + part.width() + 1 > width {
            break;
        }
        used += part.width();
        result.push_str(part);
    }
    result.push_str(glyphs::ellipsis());
    result
}

/// Share the live-work right-side budget and truncation policy with notice rows.
pub(super) fn row_columns(
    width: usize,
    prefix: &str,
    label: &str,
    metadata: &str,
    overflow: &str,
) -> (String, usize, String) {
    use crate::ui::text_width::UnicodeWidthStr;
    let right_budget = width / 2;
    let overflow = fit(overflow, right_budget);
    let right = format!(
        "{}{overflow}",
        fit(metadata, right_budget.saturating_sub(overflow.width()))
    );
    let label = fit(
        label,
        width.saturating_sub(prefix.width() + right.width() + 3),
    );
    let gap = width.saturating_sub(prefix.width() + label.width() + right.width());
    (label, gap, right)
}

impl LiveWorkView {
    pub fn set_language(&mut self, language: UiLanguage) {
        self.language = language;
    }
    pub fn accept(&mut self, snapshot: Option<LiveWorkSnapshot>, now: Instant) -> bool {
        let mut snapshot = snapshot;
        if let Some(snapshot) = &mut snapshot {
            snapshot.rows.truncate(10);
            for row in &mut snapshot.rows {
                row.label = bounded(&row.label, 512);
                row.description = crate::tool_description::label(row.description.as_deref());
                row.state = bounded(&row.state, 64);
            }
        }
        let changed = self.snapshot != snapshot;
        let owner_changed =
            self.snapshot.as_ref().map(|s| &s.run_id) != snapshot.as_ref().map(|s| &s.run_id);
        if owner_changed {
            self.close();
            self.selected = None;
            self.markers.clear();
            self.clock_origin = snapshot.as_ref().map(|snapshot| snapshot.observed_at);
        }
        if changed {
            self.clock_step = None;
        }
        // A newer saved output timestamp supersedes an earlier local commit
        // observation. Preserve markers produced after that DB snapshot.
        if let Some(next) = &snapshot {
            for row in &next.rows {
                if let LiveWorkTarget::Shell { execution_id, .. } = &row.target
                    && self
                        .snapshot
                        .as_ref()
                        .and_then(|old| old.rows.iter().find(|old| old.target == row.target))
                        .is_some_and(|old| old.last_output_at != row.last_output_at)
                    && self
                        .markers
                        .get(execution_id)
                        .is_some_and(|at| *at <= next.observed_at)
                {
                    self.markers.remove(execution_id);
                }
            }
        }
        self.snapshot = snapshot;
        if self.snapshot.is_none() {
            self.active = false;
        }
        if let Some(key) = &self.selected {
            if !self.rows().iter().any(|row| &row.target == key) {
                let replacement = match key {
                    LiveWorkTarget::Shell { execution_id, .. } => {
                        let mut matches = self.rows().iter().filter(|row| matches!(
                            &row.target, LiveWorkTarget::Wait { execution_id: id, .. } if id == execution_id
                        ));
                        let first = matches.next().map(|row| row.target.clone());
                        if matches.next().is_none() { first } else { None }
                    }
                    LiveWorkTarget::Wait { execution_id, .. } => self.rows().iter().find(|row| matches!(
                        &row.target, LiveWorkTarget::Shell { execution_id: id, .. } if id == execution_id
                    )).map(|row| row.target.clone()),
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    self.selected = Some(replacement);
                }
            }
        } else {
            self.selected = self.rows().first().map(|row| row.target.clone());
        }
        if self.open.as_ref().is_some_and(|id| {
            !self.rows().iter().any(|row| {
                matches!(
                    &row.target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == id
                )
            })
        }) {
            self.preview = None;
            self.error = None;
        }
        self.markers.retain(|id,_|self.snapshot.as_ref().is_some_and(|s|s.rows.iter().any(|row|matches!(&row.target,LiveWorkTarget::Shell {execution_id,..} if execution_id==id))));
        changed | self.advance(now)
    }
    fn rows(&self) -> &[LiveWorkRow] {
        self.snapshot.as_ref().map_or(&[], |s| s.rows.as_slice())
    }
    /// Fixed body rows; an unconfigured view (no layout yet) is uncapped.
    fn cap(&self) -> usize {
        if self.capacity == 0 {
            10
        } else {
            self.capacity
        }
    }
    pub fn configure(&mut self, collapsed: bool, capacity: usize) {
        self.collapsed = collapsed;
        self.capacity = capacity.clamp(1, 10);
        if collapsed {
            self.active = false;
            self.close();
        }
    }
    fn has_work(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| !s.rows.is_empty() || s.overflow > 0)
    }
    /// Heading always; a body of exactly `capacity` rows when open. The height
    /// never depends on how much work exists (#131).
    pub fn height(&self, available: u16) -> u16 {
        if available <= 3 {
            return 0;
        }
        if self.collapsed {
            return 2.min(available.saturating_sub(3));
        }
        let rows = self.cap() as u16;
        let preview = if self
            .open
            .as_ref()
            .is_some_and(|id| self.visible_execution(id))
        {
            3
        } else {
            0
        };
        (2 + rows + preview).min(available.saturating_sub(3))
    }
    pub fn set_area(&mut self, area: Rect) {
        self.area = (area.height > 0).then_some(area);
        self.visible_rows = if self.collapsed {
            0
        } else {
            let body = usize::from(area.height.saturating_sub(2)).min(self.cap());
            let overflow = self.snapshot.as_ref().is_some_and(|s| s.overflow > 0);
            // A separate "他N件" line is the last body row.
            let items = if overflow && body >= 2 {
                body - 1
            } else {
                body
            };
            self.rows().len().min(items)
        };
        if area.height == 0 {
            self.active = false;
        }
        if self.selected.is_none() && self.visible_rows > 0 {
            self.selected = self.rows().first().map(|row| row.target.clone());
        }
        if self
            .open
            .as_ref()
            .is_some_and(|execution| !self.visible_execution(execution))
        {
            self.preview = None;
            self.error = None;
        }
    }
    fn visible_execution(&self, execution: &str) -> bool {
        self.area.is_some() && self.rows().iter().take(self.visible_rows).any(|row|
            matches!(&row.target,LiveWorkTarget::Shell {execution_id,..} if execution_id==execution))
    }
    fn output_age_label(&self, index: usize) -> Option<String> {
        let age = self.seconds.get(index)?.1?;
        Some(format!(
            "{}{}",
            glyphs::separator(),
            WorkMessage::OutputAge(age).render(self.language)
        ))
    }
    /// The heading's blink origin (#90): the section's existing seconds
    /// clock, only while the section is drawn.
    pub fn blink_origin(&self) -> Option<Instant> {
        self.area
            .filter(|_| self.has_work())
            .and(self.snapshot.as_ref())
            .and(self.clock_origin)
    }
    pub fn advance(&mut self, now: Instant) -> bool {
        let step = self
            .clock_origin
            .map(|origin| now.saturating_duration_since(origin).as_secs());
        if self.clock_step == step && step.is_some() {
            return false;
        }
        self.clock_step = step;
        let count = self
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.rows.len());
        let mut changed = self.seconds.len() != count;
        self.seconds.resize(count, (None, None));
        if let Some(snapshot) = &self.snapshot {
            let since = now.saturating_duration_since(snapshot.observed_at);
            for (index, row) in snapshot.rows.iter().enumerate() {
                let elapsed = row
                    .elapsed_base
                    .map(|base| base.saturating_add(since).as_secs());
                let output = match &row.target {
                    LiveWorkTarget::Shell { execution_id, .. } => self
                        .markers
                        .get(execution_id)
                        .map(|at| now.saturating_duration_since(*at).as_secs())
                        .or_else(|| {
                            row.output_age_base
                                .map(|base| base.saturating_add(since).as_secs())
                        }),
                    _ => None,
                };
                changed |= self.seconds[index] != (elapsed, output);
                self.seconds[index] = (elapsed, output);
            }
        }
        changed
    }
    pub fn observe(&mut self, thread: String, generation: u64, visible: bool) -> Option<UiAction> {
        let executions = if visible && self.area.is_some() {
            self.rows()
                .iter()
                .take(self.visible_rows)
                .filter_map(|row| match &row.target {
                    LiveWorkTarget::Shell { execution_id, .. } => Some(execution_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        let open = self
            .open
            .as_ref()
            .filter(|id| executions.contains(id))
            .cloned();
        let desired = (executions, open);
        if self.published.as_ref() == Some(&desired)
            || (self.published.is_none() && desired.0.is_empty() && desired.1.is_none())
        {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        self.preview = None;
        self.error = None;
        self.published = Some(desired.clone());
        Some(UiAction::ObserveLiveWork {
            thread_id: thread,
            generation,
            revision: self.revision,
            executions: desired.0,
            open: desired.1,
        })
    }
    pub fn deactivate(&mut self, thread: String, generation: u64) -> Option<UiAction> {
        self.active = false;
        self.area = None;
        self.visible_rows = 0;
        self.close();
        self.observe(thread, generation, false)
    }
    fn close(&mut self) {
        self.open = None;
        self.preview = None;
        self.error = None;
    }
    pub fn accept_markers(
        &mut self,
        revision: u64,
        markers: Vec<(String, Instant)>,
        now: Instant,
    ) -> bool {
        if revision != self.revision {
            return false;
        }
        let Some((executions, _)) = &self.published else {
            return false;
        };
        let mut changed = false;
        for (id, at) in markers.into_iter().take(10) {
            if executions.contains(&id)
                && self.visible_execution(&id)
                && self.markers.get(&id).is_none_or(|old| at > *old)
            {
                self.markers.insert(id, at);
                changed = true;
            }
        }
        if changed {
            self.clock_step = None;
        }
        self.advance(now)
    }
    pub fn accept_preview(
        &mut self,
        revision: u64,
        execution: &str,
        result: Result<LiveOutputPreview, String>,
    ) -> bool {
        if revision != self.revision
            || self.open.as_deref() != Some(execution)
            || !self.visible_execution(execution)
            || self
                .published
                .as_ref()
                .and_then(|(_, open)| open.as_deref())
                != Some(execution)
        {
            return false;
        }
        match result {
            Ok(mut preview)
                if preview.stdout.len().saturating_add(preview.stderr.len()) <= 4096 =>
            {
                preview.stdout = crate::presentation::sanitize_plain(&preview.stdout, 0).0;
                preview.stderr = crate::presentation::sanitize_plain(&preview.stderr, 0).0;
                preview
                    .stdout
                    .truncate(byte_boundary(&preview.stdout, 4096));
                let remaining = 4096 - preview.stdout.len();
                preview
                    .stderr
                    .truncate(byte_boundary(&preview.stderr, remaining));
                self.preview = Some(preview);
                self.error = None;
            }
            Ok(_) => self.error = Some(WorkMessage::PreviewLimit.render(self.language)),
            Err(error) => self.error = Some(bounded(&error, 256)),
        }
        true
    }
    fn selected(&self) -> Option<&LiveWorkRow> {
        let selected = self.selected.as_ref()?;
        self.rows()
            .iter()
            .take(self.visible_rows)
            .find(|row| &row.target == selected)
    }
    fn move_focus(&mut self, down: bool) {
        let index = self
            .selected
            .as_ref()
            .and_then(|key| self.rows().iter().position(|row| &row.target == key))
            .unwrap_or(0);
        let index = if down {
            (index + 1).min(self.visible_rows.saturating_sub(1))
        } else {
            index.saturating_sub(1)
        };
        self.selected = self.rows().get(index).map(|row| row.target.clone());
    }
    fn toggle(&mut self) {
        let Some(LiveWorkRow {
            target: LiveWorkTarget::Shell { execution_id, .. },
            ..
        }) = self.selected()
        else {
            return;
        };
        let id = execution_id.clone();
        if self.open.as_ref() == Some(&id) {
            self.close();
        } else {
            self.open = Some(id);
            self.preview = None;
            self.error = None;
        }
    }
    fn clock_suffix(&self) -> String {
        let timing = self
            .seconds
            .first()
            .filter(|_| {
                self.snapshot
                    .as_ref()
                    .is_none_or(|snapshot| !snapshot.counts_capped)
            })
            .and_then(|(elapsed, _)| *elapsed)
            .map(|s| {
                format!(
                    "{}{}",
                    glyphs::separator(),
                    WorkMessage::Oldest(s).render(self.language)
                )
            })
            .unwrap_or_default();
        let output = self.output_age_label(0).unwrap_or_default();
        format!("{timing}{output}")
    }
    pub fn footer(&self) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        let timing = self.clock_suffix();
        Some(format!(
            "{}{sep}{}{timing}",
            WorkMessage::Heading {
                empty: false,
                folded: self.collapsed
            }
            .render(self.language),
            counts_text(snapshot, self.language),
            sep = glyphs::separator(),
        ))
    }
}
pub(super) fn draw(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &LiveWorkView,
    palette: theme::Palette,
    describe_calls: bool,
    heading_dim: bool,
) {
    if area.height == 0 {
        return;
    }
    let hidden = view.snapshot.as_ref().map_or(0, |s| {
        s.overflow
            .saturating_add((s.rows.len() - view.visible_rows) as u64)
    });
    let hidden_label = if view.visible_rows == 0 && hidden > 0 {
        format!(
            "{}{}",
            glyphs::separator(),
            more_text(
                hidden,
                view.snapshot.as_ref().is_some_and(|s| s.counts_capped),
                view.language
            )
        )
    } else {
        String::new()
    };
    let counts = view
        .snapshot
        .as_ref()
        .filter(|_| view.has_work())
        .map(|s| format!("{}{}", glyphs::separator(), counts_text(s, view.language)))
        .unwrap_or_default();
    let mark = glyphs::HEADING;
    let name = WorkMessage::Heading {
        empty: !view.has_work(),
        folded: view.collapsed,
    }
    .render(view.language);
    let heading = fit(
        &format!("{mark} {name}{hidden_label}{counts}{}", view.clock_suffix()),
        usize::from(area.width),
    );
    let separate_overflow = hidden > 0
        && view.visible_rows > 0
        && usize::from(area.height.saturating_sub(2)) > view.visible_rows;
    let mut lines = Vec::new();
    if area.height > 1 {
        lines.push(Line::from(""));
    }
    // Blink changes only the `◉` brightness, never its glyph or the width.
    let dim = heading_dim
        .then(|| palette.live_work_dim())
        .flatten()
        .filter(|_| heading.starts_with(mark));
    lines.push(match dim {
        Some(dim) => Line::from(vec![
            Span::styled(mark.to_owned(), dim),
            Span::styled(heading[mark.len()..].to_owned(), palette.live_work()),
        ]),
        None => Line::styled(heading, palette.live_work()),
    });
    for (index, row) in view.rows().iter().take(view.visible_rows).enumerate() {
        let selected = view.active && view.selected.as_ref() == Some(&row.target);
        // One-cell, East-Asian-narrow kind glyphs; not colour dependent.
        let kind = match row.target {
            LiveWorkTarget::Shell { .. } => glyphs::SHELL,
            LiveWorkTarget::ChildRun { .. } => glyphs::CHILD,
            LiveWorkTarget::Wait { .. } => glyphs::WAIT,
        };
        let last_item = index + 1 == view.visible_rows;

        let elapsed = view
            .seconds
            .get(index)
            .and_then(|(e, _)| *e)
            .map(|s| {
                format!(
                    "{}{}",
                    glyphs::separator(),
                    WorkMessage::Elapsed(s).render(view.language)
                )
            })
            .unwrap_or_default();
        let age = view.output_age_label(index).unwrap_or_default();
        let deadline = row
            .wait_info
            .as_ref()
            .map(|wait| {
                match (
                    wait.timeout,
                    view.seconds.get(index).and_then(|(elapsed, _)| *elapsed),
                ) {
                    (Some(timeout), Some(elapsed)) => format!(
                        "{}{}",
                        glyphs::separator(),
                        WorkMessage::Deadline(Some(
                            timeout
                                .saturating_sub(Duration::from_secs(elapsed))
                                .as_secs()
                        ))
                        .render(view.language)
                    ),
                    _ => format!(
                        "{}{}",
                        glyphs::separator(),
                        WorkMessage::Deadline(None).render(view.language)
                    ),
                }
            })
            .unwrap_or_default();
        let linked = row
            .wait_info
            .as_ref()
            .and_then(|wait| wait.target.as_ref())
            .and_then(|target| {
                view.rows()
                    .iter()
                    .take(view.visible_rows)
                    .position(|other| match (target, &other.target) {
                        (WaitTarget::Shell(a), LiveWorkTarget::Shell { execution_id, .. }) => {
                            a == execution_id
                        }
                        (WaitTarget::Run(a), LiveWorkTarget::ChildRun { run_id, .. }) => {
                            a == run_id
                        }
                        (WaitTarget::Thread(a), LiveWorkTarget::ChildRun { thread_id, .. }) => {
                            a == thread_id
                        }
                        _ => false,
                    })
            });

        let extra = if index + 1 == view.visible_rows && !separate_overflow {
            view.snapshot.as_ref().map_or(0, |s| {
                s.overflow
                    .saturating_add((s.rows.len() - view.visible_rows) as u64)
            })
        } else {
            0
        };
        let overflow = if extra > 0 {
            format!(
                "{}{}",
                glyphs::separator(),
                more_text(
                    extra,
                    view.snapshot.as_ref().is_some_and(|s| s.counts_capped),
                    view.language
                )
            )
        } else {
            String::new()
        };
        let prefix = format!(
            "{}{}",
            glyphs::numbered_branch(last_item && !separate_overflow, index + 1, selected, kind),
            linked
                .map(|i| format!("{} [{}] ", glyphs::arrow(), i + 1))
                .unwrap_or_default()
        );
        let metadata = format!(
            "{}{elapsed}{deadline}{age}",
            WorkMessage::state(row).render(view.language)
        );
        let (label, gap, right) = row_columns(
            usize::from(area.width),
            &prefix,
            if describe_calls && !(view.active && view.selected.as_ref() == Some(&row.target)) {
                row.description.as_deref().unwrap_or(&row.label)
            } else {
                &row.label
            },
            &metadata,
            &overflow,
        );
        lines.push(Line::from(vec![
            Span::styled(prefix, palette.live_work()),
            Span::styled(label, palette.live_work_content()),
            Span::raw(" ".repeat(gap)),
            Span::styled(
                right,
                if row.state == "stopping" && !palette.plain() {
                    Style::default().fg(theme::ATTENTION)
                } else {
                    Style::default()
                },
            ),
        ]));
    }
    if separate_overflow {
        lines.push(Line::styled(
            format!(
                "{} {}",
                glyphs::branch(true),
                more_text(
                    hidden,
                    view.snapshot.as_ref().is_some_and(|s| s.counts_capped),
                    view.language
                )
            ),
            palette.live_work(),
        ));
    }
    if !view.collapsed {
        let body = 1 + usize::from(area.height > 1) + view.cap();
        while lines.len() < body.min(usize::from(area.height)) {
            lines.push(Line::from(""));
        }
    }
    if view
        .open
        .as_ref()
        .is_some_and(|id| view.visible_execution(id))
        && lines.len() < usize::from(area.height)
    {
        if let Some(error) = &view.error {
            let rows = error
                .lines()
                .map(|line| Line::styled(line.to_owned(), Style::default().fg(palette.muted())))
                .collect();
            let available = usize::from(area.height).saturating_sub(lines.len());
            lines.extend(
                presentation::indented_wrap(rows, area.width)
                    .into_iter()
                    .take(available),
            );
        }
        if let Some(preview) = &view.preview {
            for (text, error) in [(&preview.stdout, false), (&preview.stderr, true)] {
                for line in text.lines() {
                    if lines.len() >= usize::from(area.height) {
                        break;
                    }
                    let row = Line::styled(
                        format!("{}   {} {line}", glyphs::vertical(), glyphs::vertical()),
                        if error {
                            Style::default().fg(theme::ATTENTION)
                        } else {
                            Style::default().fg(palette.muted())
                        },
                    );
                    let available = usize::from(area.height).saturating_sub(lines.len());
                    lines.extend(
                        presentation::indented_wrap(vec![row], area.width)
                            .into_iter()
                            .take(available),
                    );
                }
            }
            if preview.skipped || preview.decode_uncertain {
                let text = WorkMessage::PreviewNotice.render(view.language);
                let text = text.strip_prefix("  ").unwrap_or(&text).to_owned();
                let available = usize::from(area.height).saturating_sub(lines.len());
                lines.extend(
                    presentation::indented_wrap(
                        vec![Line::styled(text, Style::default().fg(palette.muted()))],
                        area.width,
                    )
                    .into_iter()
                    .take(available),
                );
            }
        } else if view.error.is_none() {
            let text = WorkMessage::PreviewLoading.render(view.language);
            let text = text.strip_prefix("  ").unwrap_or(&text).to_owned();
            let available = usize::from(area.height).saturating_sub(lines.len());
            lines.extend(
                presentation::indented_wrap(
                    vec![Line::styled(text, Style::default().fg(palette.muted()))],
                    area.width,
                )
                .into_iter()
                .take(available),
            );
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}
/// Alt+W folds the section unless the user bound it to send or newline.
pub(super) fn section_key_available(config: &crate::config::Config) -> bool {
    let key = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT);
    !super::key_matches(&config.ui.send_key, key)
        && !config
            .ui
            .newline_keys
            .iter()
            .any(|binding| super::key_matches(binding, key))
}
impl UiState {
    /// Fold or unfold the owned-work body. Remembered for this UI process.
    pub(super) fn toggle_live_work_section(&mut self) {
        self.live_work_collapsed = !self.live_work_collapsed;
        let (collapsed, rows) = (self.live_work_collapsed, self.config.ui.live_work_rows);
        self.live_work_mut().configure(collapsed, rows);
        self.dirty = true;
    }
    pub(super) fn live_work_key(&mut self, key: KeyEvent) -> Option<Vec<UiAction>> {
        if !self.composer_active() {
            return None;
        }
        if key.code == KeyCode::BackTab
            && !self.view().live_work.active
            && self.view().live_work.visible_rows > 0
            && !self.has_completions()
        {
            self.active_view_mut().items.active = false;
            self.active_view_mut().live_work.active = true;
            return Some(vec![]);
        }
        if !self.view().live_work.active || key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => {
                self.active_view_mut().live_work.active = false
            }
            KeyCode::Up => self.active_view_mut().live_work.move_focus(false),
            KeyCode::Down => self.active_view_mut().live_work.move_focus(true),
            KeyCode::Enter | KeyCode::Char(' ') => self.active_view_mut().live_work.toggle(),
            KeyCode::Char('o') => {
                if let Some(LiveWorkRow {
                    target: LiveWorkTarget::ChildRun { thread_id, .. },
                    ..
                }) = self.view().live_work.selected()
                {
                    return Some(self.select_thread(thread_id.clone()));
                }
            }
            KeyCode::F(6) => {
                let Some(target) = self
                    .view()
                    .live_work
                    .selected()
                    .map(|row| row.target.clone())
                else {
                    return Some(vec![]);
                };
                let mut context = self.operation_context(false);
                let operation = match target {
                    LiveWorkTarget::Shell { execution_id, .. } => {
                        context.set_reference(operations::TargetKind::Execution, execution_id);
                        operations::OpKind::ShellGet
                    }
                    LiveWorkTarget::ChildRun { run_id, .. } => {
                        context.set_reference(operations::TargetKind::Run, run_id);
                        operations::OpKind::RunGet
                    }
                    LiveWorkTarget::Wait { operation_id, .. } => {
                        context.set_reference(operations::TargetKind::Operation, operation_id);
                        operations::OpKind::OperationGet
                    }
                };
                self.invalidate_management();
                let mut popup = operations::Operations::new(context);
                popup.set_language(self.config.ui.language);
                popup.select(operation);
                self.operations = Some(popup);
            }
            KeyCode::F(7) => {}
            KeyCode::F(_) => return None,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => {}
            _ => return None,
        }
        self.dirty = true;
        Some(vec![])
    }
    pub(super) fn live_work_click(&mut self, column: u16, row: u16) -> bool {
        let Some(area) = self.view().live_work.area else {
            return false;
        };
        if !area.contains(ratatui::layout::Position::new(column, row)) {
            return false;
        }
        let heading_row = area.y + u16::from(area.height > 1);
        if row == heading_row {
            self.toggle_live_work_section();
            return true;
        }
        let Some(offset) = row.saturating_sub(area.y).checked_sub(2) else {
            return true;
        };
        let index = usize::from(offset);
        if index >= self.view().live_work.visible_rows {
            return true;
        }
        let target = self
            .view()
            .live_work
            .rows()
            .get(index)
            .map(|row| row.target.clone());
        if let Some(target) = target {
            self.active_view_mut().items.active = false;
            let live = &mut self.active_view_mut().live_work;
            live.active = true;
            live.selected = Some(target);
            self.dirty = true;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    fn japanese_view() -> LiveWorkView {
        let mut view = LiveWorkView::default();
        view.set_language(UiLanguage::Ja);
        view
    }
    fn japanese_config() -> Config {
        let mut config = Config::default();
        config.ui.language = UiLanguage::Ja;
        config
    }

    use super::*;
    #[test]
    fn capped_counts_and_overflow_are_explicit_lower_bounds() {
        let mut data = snapshot(Instant::now());
        data.counts_capped = true;
        assert!(counts_text(&data, UiLanguage::Ja).contains("2件以上"));
        assert!(counts_text(&data, UiLanguage::En).contains("at least 2"));
        assert_eq!(more_text(7, true, UiLanguage::Ja), "他7件以上");
        assert_eq!(more_text(7, true, UiLanguage::En), "at least 7 more items");
    }

    #[test]
    fn capped_footer_does_not_claim_an_exact_oldest_work_time() {
        let at = Instant::now();
        let mut view = LiveWorkView::default();
        view.set_language(UiLanguage::En);
        view.accept(Some(snapshot(at)), at);
        assert!(view.footer().unwrap().contains("Oldest work"));
        let mut capped = snapshot(at);
        capped.counts_capped = true;
        view.accept(Some(capped), at);
        let footer = view.footer().unwrap();
        assert!(footer.contains("at least"));
        assert!(
            !footer.contains("Oldest work"),
            "a truncated sample cannot identify the oldest work: {footer}"
        );
    }

    fn snapshot(at: Instant) -> LiveWorkSnapshot {
        LiveWorkSnapshot {
            run_id: "owner".into(),
            shells: 2,
            children: 1,
            waits: Some(1),
            overflow: 1,
            counts_capped: false,
            observed_at: at,
            rows: vec![
                LiveWorkRow {
                    description: None,
                    target: LiveWorkTarget::Shell {
                        execution_id: "execution".into(),
                        turn_id: Some("answered-turn".into()),
                    },
                    label: "printf synthetic-output".into(),
                    state: "running".into(),
                    started_at: Some("2026-10-31T00:00:00Z".into()),
                    elapsed_base: Some(Duration::from_secs(900)),
                    last_output_at: None,
                    wait_info: None,
                    output_age_base: Some(Duration::from_secs(30)),
                },
                LiveWorkRow {
                    description: None,
                    target: LiveWorkTarget::ChildRun {
                        run_id: "child-run".into(),
                        thread_id: "child-thread".into(),
                    },
                    label: "child task".into(),
                    state: "live".into(),
                    started_at: None,
                    elapsed_base: None,
                    last_output_at: None,
                    wait_info: None,
                    output_age_base: None,
                },
                LiveWorkRow {
                    description: None,
                    target: LiveWorkTarget::Wait {
                        operation_id: "wait-operation".into(),
                        execution_id: "execution".into(),
                    },
                    label: "saved wait".into(),
                    state: "waiting".into(),
                    started_at: None,
                    elapsed_base: None,
                    last_output_at: None,
                    wait_info: None,
                    output_age_base: None,
                },
            ],
        }
    }
    fn ui(at: Instant) -> UiState {
        let mut ui = UiState::new(&japanese_config(), PathBuf::from("/"));
        ui.select_thread("thread".into());
        ui.apply(UiUpdate::Activity {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            epoch: 1,
            snapshot: activity::ActivitySnapshot {
                run_id: Some("owner".into()),
                turn_id: Some("answered-turn".into()),
                phase: activity::ActivityPhase::Background,
                turn_in_progress: false,
                live_work: Some(snapshot(at)),
                ..Default::default()
            },
            observed_at: at,
        });
        ui
    }
    #[test]
    fn f6_operations_form_uses_configured_language() {
        use ratatui::backend::TestBackend;
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let at = Instant::now();
            let mut ui = ui(at);
            ui.config.ui.language = language;
            ui.view_mut().live_work.set_area(Rect::new(0, 0, 100, 10));
            ui.view_mut().live_work.active = true;
            ui.view_mut().live_work.selected = Some(LiveWorkTarget::Shell {
                execution_id: "execution".into(),
                turn_id: Some("answered-turn".into()),
            });
            ui.live_work_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
            let popup = ui.operations.as_ref().expect("F6 opens operations");
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| popup.draw(frame, frame.area()))
                .unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            let expected = match language {
                UiLanguage::En => "Esc List",
                UiLanguage::Ja => "Esc 一 覧",
            };
            assert!(screen.contains(expected), "{language:?}: {screen}");
        }
    }

    #[test]
    fn configured_cap_applies_to_ui_and_counts_hidden_rows() {
        let at = Instant::now();
        let mut config = japanese_config();
        config.ui.live_work_rows = 1;
        let mut ui = UiState::new(&config, PathBuf::from("/"));
        ui.select_thread("thread".into());
        ui.apply(UiUpdate::Activity {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            epoch: 1,
            snapshot: activity::ActivitySnapshot {
                run_id: Some("owner".into()),
                phase: activity::ActivityPhase::Background,
                live_work: Some(snapshot(at)),
                ..Default::default()
            },
            observed_at: at,
        });
        assert_eq!(ui.view().live_work.rows().len(), 1);
        assert_eq!(ui.view().live_work.snapshot.as_ref().unwrap().overflow, 3);
    }
    #[test]
    fn tiny_section_shows_header_and_header_click_does_not_select() {
        let at = Instant::now();
        let mut ui = ui(at);
        ui.view_mut().live_work.set_area(Rect::new(0, 0, 80, 1));
        let area = Rect::new(0, 0, 80, 1);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    area,
                    &ui.view().live_work,
                    theme::Palette::from_capabilities(
                        crate::config::PaletteMode::Dark,
                        true,
                        false,
                    ),
                    false,
                    false,
                )
            })
            .unwrap();
        let mut text = String::new();
        let mut x = 0;
        while x < 80 {
            let symbol = terminal.backend().buffer()[(x, 0)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        assert!(text.contains("◉") && text.contains("他4件"), "{text}");
        assert!(ui.live_work_click(0, 0));
        assert!(!ui.view().live_work.active);
    }
    #[test]
    fn saved_work_clock_survives_answer_and_dirties_at_most_once_per_second_without_actions() {
        let at = Instant::now();
        let mut ui = ui(at);
        ui.view_mut().live_work.set_area(Rect::new(0, 20, 80, 5));
        let first = ui.take_deferred_actions();
        assert!(
            matches!(first.as_slice(),[UiAction::ObserveLiveWork {executions,open:None,..}] if executions==&vec!["execution".to_string()])
        );
        assert_eq!(
            ui.view().activity.displayed_elapsed_ms(),
            None,
            "completed Turn has no running observed clock"
        );
        let mut redraws = 0;
        for tick in 1..=6000 {
            ui.dirty = false;
            ui.animation_tick(at + Duration::from_millis(tick * 100));
            redraws += usize::from(ui.dirty);
            assert!(
                ui.take_deferred_actions().is_empty(),
                "clock must not produce DB/output reads"
            );
        }
        assert_eq!(redraws, 600);
        assert_eq!(ui.view().live_work.seconds[0].0, Some(1500));
        assert_eq!(
            ui.view().live_work.rows().len(),
            3,
            "answer does not remove owned work"
        );
        let revision = ui.view().history_revision();
        ui.animation_tick(at + Duration::from_secs(601));
        assert_eq!(
            ui.view().history_revision(),
            revision,
            "display clock does not reparse history"
        );
    }
    #[test]
    fn quiet_heading_blink_is_one_frame_per_second_and_static_when_motion_or_color_is_off() {
        use crate::config::{MotionMode, PaletteMode};
        let at = Instant::now();
        for (motion, no_color, expected) in [
            (MotionMode::On, false, 60),
            (MotionMode::Off, false, 0),
            (MotionMode::On, true, 0),
        ] {
            let mut config = japanese_config();
            config.ui.motion = motion;
            let mut ui = UiState::new(&config, PathBuf::from("/"));
            ui.palette = theme::Palette::from_capabilities(PaletteMode::Dark, no_color, true);
            ui.select_thread("thread".into());
            // A child row alone has no ticking seconds: only the blink can move.
            let mut work = snapshot(at);
            work.rows
                .retain(|row| matches!(row.target, LiveWorkTarget::ChildRun { .. }));
            ui.apply(UiUpdate::Activity {
                thread_id: "thread".into(),
                generation: ui.view().generation,
                epoch: 1,
                snapshot: activity::ActivitySnapshot {
                    run_id: Some("owner".into()),
                    phase: activity::ActivityPhase::Background,
                    live_work: Some(work),
                    ..Default::default()
                },
                observed_at: at,
            });
            ui.view_mut().live_work.set_area(Rect::new(0, 20, 80, 5));
            ui.take_deferred_actions();
            let mut redraws = 0;
            let mut transitions = 0;
            let mut dim = ui.animation.blink_dim(at);
            for tick in 1..=2400u64 {
                ui.dirty = false;
                let now = at + Duration::from_millis(tick * 25);
                ui.animation_tick(now);
                redraws += usize::from(ui.dirty);
                assert!(ui.take_deferred_actions().is_empty(), "no DB/output reads");
                let next = ui.animation.blink_dim(now);
                if next != dim {
                    transitions += 1;
                    assert!(ui.dirty, "every visible blink change has its frame");
                    dim = next;
                }
                assert_eq!(ui.animation.spinner(now), None, "no conversation spinner");
            }
            assert_eq!(redraws, expected, "{motion:?} no_color={no_color}");
            assert_eq!(transitions, expected);
        }
    }
    #[test]
    fn heading_blink_changes_only_the_mark_brightness() {
        let at = Instant::now();
        let ui = ui(at);
        let palette =
            theme::Palette::from_capabilities(crate::config::PaletteMode::Dark, false, true);
        let render = |dim: bool| {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 5)).unwrap();
            let mut view = japanese_view();
            view.accept(ui.view().live_work.snapshot.clone(), at);
            view.set_area(Rect::new(0, 0, 80, 5));
            terminal
                .draw(|frame| draw(frame, Rect::new(0, 0, 80, 5), &view, palette, false, dim))
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let (bright, dim) = (render(false), render(true));
        for y in 0..5 {
            for x in 0..80 {
                assert_eq!(bright[(x, y)].symbol(), dim[(x, y)].symbol());
                if bright[(x, y)] != dim[(x, y)] {
                    assert_eq!((x, y, bright[(x, y)].symbol()), (0, 1, "◉"));
                }
            }
        }
        assert_ne!(bright[(0, 1)].fg, dim[(0, 1)].fg);
    }
    #[test]
    fn maximum_rows_share_one_quiet_clock_without_reads() {
        let at = Instant::now();
        let mut config = japanese_config();
        config.ui.live_work_rows = 10;
        let mut ui = UiState::new(&config, PathBuf::from("/"));
        ui.select_thread("thread".into());
        let mut work = snapshot(at);
        let template = work.rows[0].clone();
        work.rows = (0..10)
            .map(|i| {
                let mut row = template.clone();
                row.target = LiveWorkTarget::Shell {
                    execution_id: format!("execution-{i}"),
                    turn_id: None,
                };
                row
            })
            .collect();
        work.shells = 10;
        work.children = 0;
        work.waits = Some(0);
        work.overflow = 0;
        ui.apply(UiUpdate::Activity {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            epoch: 1,
            snapshot: activity::ActivitySnapshot {
                run_id: Some("owner".into()),
                phase: activity::ActivityPhase::Background,
                live_work: Some(work),
                ..Default::default()
            },
            observed_at: at,
        });
        ui.view_mut().live_work.set_area(Rect::new(0, 0, 80, 12));
        assert_eq!(ui.view().live_work.rows().len(), 10);
        ui.take_deferred_actions();
        let mut redraws = 0;
        for tick in 1..=600 {
            ui.dirty = false;
            ui.animation_tick(at + Duration::from_millis(tick * 100));
            redraws += usize::from(ui.dirty);
            assert!(ui.take_deferred_actions().is_empty());
        }
        assert_eq!(redraws, 60);
    }
    #[test]
    fn folding_preserves_identity_and_ambiguous_or_evicted_targets_do_not_redirect() {
        let at = Instant::now();
        let mut ui = ui(at);
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
        let mut all = snapshot(at);
        all.overflow = 0;
        let mut original = all.clone();
        original.rows.remove(2);
        let shell = original.rows[0].target.clone();
        ui.view_mut().live_work.selected = Some(shell.clone());
        ui.view_mut().live_work.toggle();
        let mut folded = all;
        folded.rows.remove(0);
        ui.view_mut().live_work.accept(Some(folded.clone()), at);
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
        assert!(matches!(&ui.view().live_work.selected().unwrap().target,
            LiveWorkTarget::Wait { operation_id, execution_id } if operation_id == "wait-operation" && execution_id == "execution"));
        assert_eq!(ui.view().live_work.open.as_deref(), Some("execution"));
        ui.view_mut().live_work.accept(Some(original.clone()), at);
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
        assert_eq!(ui.view().live_work.selected().unwrap().target, shell);
        let mut second = folded.rows[1].clone();
        second.target = LiveWorkTarget::Wait {
            operation_id: "other-wait".into(),
            execution_id: "execution".into(),
        };
        folded.rows.push(second);
        ui.view_mut().live_work.accept(Some(folded), at);
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
        assert_eq!(ui.view().live_work.selected, Some(shell.clone()));
        assert!(ui.view().live_work.selected().is_none());
        ui.view_mut().live_work.active = true;
        assert!(
            matches!(ui.live_work_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)), Some(actions) if actions.is_empty())
        );
        assert!(ui.operations.is_none());
        ui.view_mut().live_work.accept(Some(original), at);
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
        assert_eq!(ui.view().live_work.selected().unwrap().target, shell);
    }
    #[test]
    fn scope_revision_and_exact_execution_guard_live_output_and_postcommit_markers() {
        let at = Instant::now();
        let mut view = japanese_view();
        view.accept(Some(snapshot(at)), at);
        view.set_area(Rect::new(0, 0, 80, 5));
        let Some(UiAction::ObserveLiveWork {
            revision: first, ..
        }) = view.observe("thread".into(), 1, true)
        else {
            panic!()
        };
        view.toggle();
        let Some(UiAction::ObserveLiveWork {
            revision: opened,
            open,
            ..
        }) = view.observe("thread".into(), 1, true)
        else {
            panic!()
        };
        assert_eq!(open.as_deref(), Some("execution"));
        assert!(opened > first);
        let preview = || LiveOutputPreview {
            stdout: "paid output".into(),
            stderr: "failure text".into(),
            decode_uncertain: false,
            skipped: false,
            complete: false,
        };
        assert!(!view.accept_preview(first, "execution", Ok(preview())));
        assert!(!view.accept_preview(opened, "unrelated", Ok(preview())));
        assert!(view.accept_preview(opened, "execution", Ok(preview())));
        assert_eq!(view.preview.as_ref().unwrap().stdout, "paid output");
        view.accept_markers(
            opened,
            vec![("other".into(), at + Duration::from_secs(8))],
            at + Duration::from_secs(10),
        );
        assert!(!view.markers.contains_key("other"));
        view.accept_markers(
            opened,
            vec![("execution".into(), at + Duration::from_secs(8))],
            at + Duration::from_secs(10),
        );
        assert_eq!(view.seconds[0].1, Some(2));
        let Some(UiAction::ObserveLiveWork {
            executions,
            open: None,
            ..
        }) = view.observe("thread".into(), 1, false)
        else {
            panic!()
        };
        assert!(executions.is_empty());
        assert!(view.preview.is_none());
        assert!(!view.accept_preview(opened, "execution", Ok(preview())));
    }
    #[test]
    fn snapshot_time_reobservation_avoids_redraw_feedback_and_small_layout_preserves_history() {
        let at = Instant::now();
        let mut view = japanese_view();
        assert!(view.accept(Some(snapshot(at)), at));
        let mut newer = snapshot(at + Duration::from_millis(500));
        newer.rows[0].elapsed_base = Some(Duration::from_millis(900_500));
        newer.rows[0].output_age_base = Some(Duration::from_millis(30_500));
        assert!(!view.accept(Some(newer), at + Duration::from_millis(500)));
        view.toggle();
        for available in 0..30 {
            assert!(view.height(available) <= available.saturating_sub(3));
        }
        assert_eq!(view.height(4), 1);
        view.accept(None, at);
        view.configure(false, 3);
        assert_eq!(view.height(30), 5);
        assert_eq!(view.height(3), 0);
        assert!(view.preview.is_none());
    }
    #[test]
    fn actual_draw_and_focus_keep_live_work_separate_from_history_and_composer() {
        let at = Instant::now();
        let mut ui = ui(at);
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &mut ui))
            .unwrap();
        let area = ui.view().live_work.area.unwrap();
        assert!(ui.view().navigation.scroll.area.unwrap().height >= 3);
        // Wide graphemes own multiple terminal columns. The continuation
        // cell contains a blank, which is geometry rather than an extra space
        // inside the displayed Japanese label. Preserve real spaces/row breaks.
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in buffer.area.y..buffer.area.bottom() {
            let mut x = buffer.area.x;
            while x < buffer.area.right() {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            text.push('\n');
        }
        assert!(text.contains("printf synthetic-output"), "{text}");
        assert!(
            text.contains("他2件"),
            "exact overflow count must remain visible: {text}"
        );
        assert!(!text.contains("answered-turn"));
        assert!(!text.contains("child-run"));
        ui.insert("draft stays", false);
        let anchor = ui.view().navigation.anchor.clone();
        assert!(ui.live_work_click(area.x, area.y + 3));
        assert!(ui.view().live_work.active);
        assert_eq!(ui.view().draft.text, "draft stays");
        assert_eq!(ui.view().navigation.anchor, anchor);
        let actions = ui
            .live_work_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE))
            .unwrap();
        assert!(actions.iter().any(|action|matches!(action,UiAction::SelectThread {thread_id,..} if thread_id=="child-thread")));
        assert!(!ui.view().live_work.active);
    }
    #[test]
    fn wait_target_number_and_approximate_deadline_are_visible() {
        let at = Instant::now();
        let mut view = japanese_view();
        let mut data = snapshot(at);
        data.overflow = 0;
        data.rows[2].wait_info = Some(WaitInfo {
            target: Some(WaitTarget::Shell("execution".into())),
            timeout: Some(Duration::from_secs(300)),
        });
        data.rows[2].elapsed_base = Some(Duration::from_secs(10));
        view.accept(Some(data), at);
        let area = Rect::new(0, 0, 100, 5);
        view.set_area(area);
        let backend = ratatui::backend::TestBackend::new(100, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    area,
                    &view,
                    theme::Palette::from_capabilities(
                        crate::config::PaletteMode::Dark,
                        false,
                        true,
                    ),
                    false,
                    false,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..5 {
            let mut x = 0;
            while x < 100 {
                let c = buffer[(x, y)].symbol();
                text.push_str(c);
                x += unicode_width::UnicodeWidthStr::width(c).max(1) as u16;
            }
            text.push('\n');
        }
        assert!(text.contains("◉ 抱えている作業"), "{text}");
        assert!(text.contains("[1] $") && text.contains("> [1]"), "{text}");
        assert!(
            text.contains("期限目安") && text.contains("待機中"),
            "{text}"
        );
    }
    #[test]
    fn wrong_generation_reply_never_hydrates_and_labels_cannot_inject_terminal_controls() {
        let at = Instant::now();
        let mut ui = ui(at);
        let mut source = snapshot(at);
        source.rows[0].label = "unsafe\u{1b}[31m\nnew line".into();
        ui.view_mut().live_work.accept(Some(source), at);
        assert!(
            !ui.view().live_work.rows()[0]
                .label
                .contains(['\u{1b}', '\n'])
        );
        ui.view_mut().live_work.set_area(Rect::new(0, 0, 80, 5));
        ui.view_mut().live_work.toggle();
        let actions = ui.take_deferred_actions();
        let [UiAction::ObserveLiveWork { revision, .. }] = actions.as_slice() else {
            panic!()
        };
        ui.apply(UiUpdate::LiveWorkPreview {
            thread_id: "thread".into(),
            generation: ui.view().generation + 1,
            revision: *revision,
            execution_id: "execution".into(),
            result: Ok(LiveOutputPreview {
                stdout: "wrong generation".into(),
                stderr: String::new(),
                decode_uncertain: false,
                skipped: false,
                complete: false,
            }),
        });
        assert!(ui.view().live_work.preview.is_none());
    }
    fn output(text: &str) -> LiveOutputPreview {
        LiveOutputPreview {
            stdout: text.into(),
            stderr: String::new(),
            decode_uncertain: false,
            skipped: false,
            complete: false,
        }
    }
    #[test]
    fn overlay_f2_and_thread_switch_clear_scope_and_reject_reply_before_deferred_clear() {
        for popup in [KeyCode::F(1), KeyCode::F(2)] {
            let at = Instant::now();
            let mut ui = ui(at);
            ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
            ui.view_mut().live_work.toggle();
            let actions = ui.take_deferred_actions();
            let [UiAction::ObserveLiveWork { revision, .. }] = actions.as_slice() else {
                panic!()
            };
            let revision = *revision;
            let generation = ui.view().generation;
            ui.event(Event::Key(KeyEvent::new(popup, KeyModifiers::NONE)));
            assert!(!ui.composer_active());
            ui.apply(UiUpdate::LiveWorkPreview {
                thread_id: "thread".into(),
                generation,
                revision,
                execution_id: "execution".into(),
                result: Ok(output("hidden late text")),
            });
            assert!(
                ui.view().live_work.preview.is_none(),
                "covered consumer must reject before clear delivery"
            );
            let clear = ui.take_deferred_actions();
            assert!(
                matches!(clear.as_slice(),[UiAction::ObserveLiveWork {executions,open:None,..}] if executions.is_empty())
            );
            assert!(ui.take_deferred_actions().is_empty());
            ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            assert!(
                matches!(ui.take_deferred_actions().as_slice(),[UiAction::ObserveLiveWork {open:Some(id),..}] if id=="execution")
            );
            ui.select_thread("another".into());
            ui.apply(UiUpdate::LiveWorkPreview {
                thread_id: "thread".into(),
                generation,
                revision,
                execution_id: "execution".into(),
                result: Ok(output("old Thread text")),
            });
            assert!(ui.view().live_work.preview.is_none());
            let previous = ui
                .views
                .values()
                .find(|view| view.thread_id.as_deref() == Some("thread"))
                .unwrap();
            assert!(previous.live_work.preview.is_none());
            assert!(ui.take_deferred_actions().iter().any(|action|matches!(action,UiAction::ObserveLiveWork {thread_id,executions,open:None,..} if thread_id=="thread" && executions.is_empty())));
        }
    }
    #[test]
    fn shrinking_preserves_target_and_rejects_late_preview_without_waiting_for_publication() {
        let at = Instant::now();
        let mut ui = ui(at);
        let mut source = snapshot(at);
        source.rows.swap(0, 1);
        source.overflow = 0;
        ui.view_mut().live_work.accept(Some(source), at);
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
        ui.view_mut().live_work.selected = Some(LiveWorkTarget::Shell {
            execution_id: "execution".into(),
            turn_id: Some("answered-turn".into()),
        });
        ui.view_mut().live_work.toggle();
        let actions = ui.take_deferred_actions();
        let [UiAction::ObserveLiveWork { revision, .. }] = actions.as_slice() else {
            panic!()
        };
        let revision = *revision;
        assert!(ui.view_mut().live_work.accept_preview(
            revision,
            "execution",
            Ok(output("kept while visible"))
        ));
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 3));
        assert_eq!(ui.view().live_work.open.as_deref(), Some("execution"));
        assert!(ui.view().live_work.preview.is_none());
        assert!(ui.view().live_work.selected().is_none());
        assert!(!ui.view_mut().live_work.accept_preview(
            revision,
            "execution",
            Ok(output("late hidden row"))
        ));
        ui.view_mut().live_work.active = true;
        ui.live_work_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert!(ui.operations.is_none());
        ui.view_mut().live_work.set_area(Rect::new(0, 10, 80, 5));
        assert!(matches!(
            ui.view().live_work.selected().unwrap().target,
            LiveWorkTarget::Shell { .. }
        ));
    }
    #[test]
    fn newer_saved_output_replaces_only_older_commit_observations_and_labels_the_source() {
        let at = Instant::now();
        let mut view = japanese_view();
        view.accept(Some(snapshot(at)), at);
        view.set_area(Rect::new(0, 0, 80, 5));
        let Some(UiAction::ObserveLiveWork { revision, .. }) =
            view.observe("thread".into(), 1, true)
        else {
            panic!()
        };
        view.accept_markers(
            revision,
            vec![("execution".into(), at + Duration::from_secs(2))],
            at + Duration::from_secs(3),
        );
        assert_eq!(
            view.output_age_label(0).as_deref(),
            Some(" | 最後の出力 1秒前")
        );
        let mut saved = snapshot(at + Duration::from_secs(5));
        saved.rows[0].last_output_at = Some("new saved timestamp".into());
        saved.rows[0].output_age_base = Some(Duration::from_secs(1));
        view.accept(Some(saved.clone()), at + Duration::from_secs(5));
        assert!(!view.markers.contains_key("execution"));
        assert_eq!(
            view.output_age_label(0).as_deref(),
            Some(" | 最後の出力 1秒前")
        );
        view.accept_markers(
            revision,
            vec![("execution".into(), at + Duration::from_secs(7))],
            at + Duration::from_secs(8),
        );
        saved.rows[0].last_output_at = Some("snapshot older than marker".into());
        view.accept(Some(saved), at + Duration::from_secs(8));
        assert!(view.markers.contains_key("execution"));
        assert_eq!(
            view.output_age_label(0).as_deref(),
            Some(" | 最後の出力 1秒前")
        );
    }

    fn screen(ui: &mut UiState) -> (String, u16) {
        screen_at(ui, 80, 24)
    }
    fn screen_at(ui: &mut UiState, width: u16, height: u16) -> (String, u16) {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, ui))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in buffer.area.y..buffer.area.bottom() {
            let mut x = buffer.area.x;
            while x < buffer.area.right() {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            text.push('\n');
        }
        (text, ui.view().live_work.area.map_or(0, |a| a.height))
    }
    fn set_work(ui: &mut UiState, work: Option<LiveWorkSnapshot>, epoch: u64, at: Instant) {
        ui.apply(UiUpdate::Activity {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            epoch,
            snapshot: activity::ActivitySnapshot {
                run_id: Some("owner".into()),
                phase: activity::ActivityPhase::Background,
                live_work: work,
                ..Default::default()
            },
            observed_at: at,
        });
    }
    #[test]
    fn work_heading_shows_clocks_and_footer_falls_back_when_hidden() {
        for language in [UiLanguage::Ja, UiLanguage::En] {
            let at = Instant::now();
            let mut config = Config::default();
            config.ui.language = language;
            let mut ui = UiState::new(&config, PathBuf::from("/"));
            ui.select_thread("thread".into());
            set_work(&mut ui, Some(snapshot(at)), 1, at);
            let (title, oldest, output) = match language {
                UiLanguage::Ja => ("抱えている作業", "最古", "最後の出力"),
                UiLanguage::En => ("Owned work", "Oldest work", "Last output"),
            };
            for (height, collapsed) in [(24, false), (7, false), (24, true), (7, true), (24, false)]
            {
                ui.live_work_collapsed = collapsed;
                let (text, panel_height) = screen_at(&mut ui, 160, height);
                let status = text.lines().nth(usize::from(height - 2)).unwrap();
                if panel_height == 0 {
                    let expected = ui.view().live_work.footer().unwrap();
                    assert!(
                        status.contains(&expected),
                        "{language:?}: {status} / {expected}"
                    );
                } else {
                    let heading = text.lines().find(|line| line.contains(title)).unwrap();
                    assert!(heading.contains(oldest), "{language:?}: {heading}");
                    assert!(heading.contains(output), "{language:?}: {heading}");
                    assert!(!status.contains(title), "{language:?}: {status}");
                    if collapsed {
                        assert!(!text.contains("printf synthetic-output"), "{text}");
                    }
                }
            }
        }
    }

    #[test]
    fn height_is_constant_across_zero_two_zero_works_and_empty_heading_does_not_blink() {
        let at = Instant::now();
        let mut ui = UiState::new(&japanese_config(), PathBuf::from("/"));
        ui.select_thread("thread".into());
        let (text, empty) = screen(&mut ui);
        assert!(text.contains("◉ 抱えている作業 | なし"), "{text}");
        assert_eq!(empty, 5);
        assert!(ui.view().live_work.blink_origin().is_none());
        let mut two = snapshot(at);
        two.rows.truncate(2);
        set_work(&mut ui, Some(two), 1, at);
        let (text, busy) = screen(&mut ui);
        assert!(text.contains("printf synthetic-output"), "{text}");
        assert_eq!(busy, empty);
        set_work(&mut ui, None, 2, at);
        let (_, after) = screen(&mut ui);
        assert_eq!(after, empty);
        let mut empty_snapshot = snapshot(at);
        empty_snapshot.rows.clear();
        empty_snapshot.overflow = 0;
        set_work(&mut ui, Some(empty_snapshot), 3, at);
        screen(&mut ui);
        assert!(ui.view().live_work.blink_origin().is_none());
    }
    #[test]
    fn key_and_heading_click_toggle_between_heading_only_and_fixed_rows() {
        for rows in [1, 5] {
            let at = Instant::now();
            let mut config = japanese_config();
            config.ui.live_work_rows = rows;
            let mut ui = UiState::new(&config, PathBuf::from("/"));
            ui.select_thread("thread".into());
            set_work(&mut ui, Some(snapshot(at)), 1, at);
            let (_, open) = screen(&mut ui);
            assert_eq!(open, 2 + rows as u16);
            let alt_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT);
            ui.key(alt_w);
            let (text, closed) = screen(&mut ui);
            assert_eq!(closed, 2);
            assert!(!text.contains("printf synthetic-output"), "{text}");
            ui.key(alt_w);
            assert_eq!(screen(&mut ui).1, open);
            let area = ui.view().live_work.area.unwrap();
            assert!(ui.live_work_click(area.x + 2, area.y + 1));
            assert_eq!(screen(&mut ui).1, 2);
            let area = ui.view().live_work.area.unwrap();
            assert!(ui.live_work_click(area.x + 2, area.y + 1));
            assert_eq!(screen(&mut ui).1, open);
        }
    }

    #[test]
    fn kind_glyphs_are_one_column_even_under_cjk_ambiguous_width() {
        use crate::ui::text_width::UnicodeWidthStr;
        for glyph in ["$", "◉", "↳", "⋯", "›"] {
            assert_eq!(glyph.width(), 1, "{glyph}");
            assert_eq!(glyph.width_cjk(), 1, "{glyph} ambiguous under CJK");
        }
    }
    fn body_lines(ui: &mut UiState) -> Vec<String> {
        let (text, _) = screen(ui);
        text.lines()
            .filter(|l| {
                let row = l.trim_start();
                row.starts_with("├─[") || row.starts_with("└─[") || row.starts_with("└─ 他")
            })
            .map(str::to_owned)
            .collect()
    }
    #[test]
    fn measured_tree_fits_both_languages_and_terminal_widths() {
        use crate::ui::text_width::{self, UnicodeWidthStr};
        for observed in [Some(1), Some(2), None] {
            text_width::set_terminal_width(observed);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                for width in [80, 120] {
                    let now = Instant::now();
                    let mut view = LiveWorkView::default();
                    view.set_language(language);
                    let mut data = snapshot(now);
                    data.overflow = 0;
                    data.rows[2].wait_info = Some(WaitInfo {
                        target: Some(WaitTarget::Shell("execution".into())),
                        timeout: None,
                    });
                    view.accept(Some(data), now);
                    view.set_area(Rect::new(0, 0, width, 5));
                    view.active = true;
                    let mut terminal =
                        Terminal::new(ratatui::backend::TestBackend::new(width, 5)).unwrap();
                    terminal
                        .draw(|frame| {
                            draw(
                                frame,
                                Rect::new(0, 0, width, 5),
                                &view,
                                theme::Palette::from_capabilities(
                                    crate::config::PaletteMode::Dark,
                                    true,
                                    false,
                                ),
                                false,
                                false,
                            )
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let mut rows = Vec::new();
                    for y in 0..5 {
                        let mut text = String::new();
                        let mut x = 0;
                        while x < width {
                            let cell = buffer[(x, y)].symbol();
                            text.push_str(cell);
                            x += cell.width().max(1) as u16;
                        }
                        assert!(
                            text.width() <= usize::from(width),
                            "{observed:?} {language:?} {width}: {text}"
                        );
                        rows.push(text);
                    }
                    assert!(rows[1].starts_with("◉ "), "{rows:?}");
                    let (first, last, sep) = if observed == Some(1) {
                        ("├─", "└─", " · ")
                    } else {
                        ("+-", "\\-", " | ")
                    };
                    assert!(rows[1].contains(sep), "{rows:?}");
                    assert!(rows[2].starts_with(&format!("{first}[1]")), "{rows:?}");
                    assert!(rows[3].starts_with(&format!("{first}[2]")), "{rows:?}");
                    assert!(rows[4].starts_with(&format!("{last}[3]")), "{rows:?}");
                    assert!(rows[2].contains("[1]›$"), "{rows:?}");
                    assert!(rows[3].contains("↳") && rows[4].contains("⋯"), "{rows:?}");
                    let arrow = if observed == Some(1) {
                        "→ [1]"
                    } else {
                        "> [1]"
                    };
                    assert!(rows[4].contains(arrow), "{rows:?}");
                }
            }
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn tree_connectors_cover_single_multiple_and_overflow_rows_and_all_kinds() {
        crate::ui::text_width::set_terminal_width(Some(1));
        let at = Instant::now();
        let mut ui = UiState::new(&japanese_config(), PathBuf::from("/"));
        ui.select_thread("thread".into());
        let mut one = snapshot(at);
        one.rows.truncate(1);
        one.overflow = 0;
        set_work(&mut ui, Some(one), 1, at);
        let lines = body_lines(&mut ui);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("└─[1]") && lines[0].contains("$ printf"),
            "{lines:?}"
        );
        assert!(!lines[0].contains("shell "), "{lines:?}");
        let mut three = snapshot(at);
        three.overflow = 0;
        set_work(&mut ui, Some(three), 2, at);
        let lines = body_lines(&mut ui);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("├─[1]") && lines[0].contains("$ "));
        assert!(lines[1].starts_with("├─[2]") && lines[1].contains("↳ child task"));
        assert!(lines[2].starts_with("└─[3]") && lines[2].contains("⋯ saved wait"));
        // Padding rows carry no connector: same count with fewer items.
        let mut two = snapshot(at);
        two.rows.truncate(2);
        two.overflow = 0;
        set_work(&mut ui, Some(two), 3, at);
        let lines = body_lines(&mut ui);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].starts_with("└─[2]"));
        // Overflow is its own last line.
        let mut many = snapshot(at);
        many.overflow = 4;
        set_work(&mut ui, Some(many), 4, at);
        let lines = body_lines(&mut ui);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("├─[1]") && lines[1].starts_with("├─[2]"));
        assert!(lines[2].starts_with("└─ 他5件"), "{lines:?}");
        crate::ui::text_width::clear_terminal_width();
    }
}

#[cfg(test)]
mod language_tests {
    use super::*;
    fn populate(ui: &mut UiState) {
        let view = &mut ui.active_view_mut().live_work;
        let targets = [
            LiveWorkTarget::Shell {
                execution_id: "shell-id".into(),
                turn_id: None,
            },
            LiveWorkTarget::ChildRun {
                run_id: "child-run".into(),
                thread_id: "child-thread".into(),
            },
            LiveWorkTarget::Wait {
                operation_id: "wait-id".into(),
                execution_id: "shell-id".into(),
            },
        ];
        let rows = targets
            .into_iter()
            .enumerate()
            .map(|(index, target)| LiveWorkRow {
                target,
                label: format!("Synthetic work {index}"),
                description: None,
                state: if index == 2 { "waiting" } else { "running" }.into(),
                started_at: None,
                elapsed_base: None,
                last_output_at: None,
                wait_info: None,
                output_age_base: None,
            })
            .collect();
        assert!(view.accept(
            Some(LiveWorkSnapshot {
                run_id: "owner".into(),
                observed_at: Instant::now(),
                shells: 1,
                children: 1,
                waits: Some(1),
                overflow: 0,
                counts_capped: false,
                rows
            }),
            Instant::now()
        ));
    }
    #[test]
    fn owned_work_is_registered_in_both_languages_and_widths() {
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &[
                    "Owned work",
                    "Shells: 1",
                    "Child runs: 1",
                    "Running",
                    "Waiting",
                ],
                UiLanguage::Ja => &["抱えている作業", "shell 1", "子 1", "実行中", "待機中"],
            }
        }
        super::super::i18n::verify_screen(populate, expected);
    }
    #[test]
    fn folded_and_empty_work_are_registered() {
        fn folded(ui: &mut UiState) {
            populate(ui);
            ui.toggle_live_work_section();
        }
        fn folded_expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Owned work", "Folded", "Shells: 1"],
                UiLanguage::Ja => &["抱えている作業", "折りたたみ", "shell 1"],
            }
        }
        fn empty(_: &mut UiState) {}
        fn empty_expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Owned work", "None"],
                UiLanguage::Ja => &["抱えている作業", "なし"],
            }
        }
        super::super::i18n::verify_screen(folded, folded_expected);
        super::super::i18n::verify_screen(empty, empty_expected);
    }
    #[test]
    fn child_phase_projection_is_english_without_rewriting_source_states() {
        use crate::activity::ActivityPhase::*;
        let expected = [
            (Idle, "Idle"),
            (Sending, "Sending"),
            (Reflecting, "Loading saved input"),
            (Preparing, "Preparing"),
            (AwaitingModel, "Awaiting model"),
            (Retrying, "Retrying"),
            (Receiving, "Receiving"),
            (Tools, "Running tools"),
            (Running, "Running"),
            (Waiting, "Waiting"),
            (Background, "Background work"),
            (Continuing, "Continuing"),
            (Stopping, "Stopping"),
            (Recovery, "Recovery needed"),
            (Failed, "Failed"),
            (Stopped, "Stopped"),
            (Completed, "Completed"),
        ];
        let mut ui = UiState::new(&Config::default(), PathBuf::new());
        populate(&mut ui);
        let mut row = ui.view().live_work.snapshot.as_ref().unwrap().rows[1].clone();
        assert_eq!(ui.view().live_work.language, UiLanguage::En);
        for (phase, english) in expected {
            row.state = phase.column_label().into();
            let source = row.state.clone();
            for observed in [Some(1), Some(2), None] {
                crate::ui::text_width::set_terminal_width(observed);
                assert_eq!(WorkMessage::state(&row).render(UiLanguage::En), english);
                assert_eq!(WorkMessage::state(&row).render(UiLanguage::Ja), source);
                assert_eq!(row.state.as_bytes(), source.as_bytes());
            }
            row.state = phase.column_label_for(UiLanguage::En).into();
            assert_eq!(WorkMessage::state(&row).render(UiLanguage::En), english);
            assert_eq!(
                WorkMessage::state(&row).render(UiLanguage::Ja),
                phase.column_label()
            );
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn state_projection_preserves_unknown_and_non_child_source_bytes() {
        let mut ui = UiState::new(&Config::default(), PathBuf::new());
        populate(&mut ui);
        for mut row in ui.view().live_work.snapshot.as_ref().unwrap().rows.clone() {
            for source in ["external | 値（raw） -> …", "stopping-now"] {
                row.state = source.into();
                for observed in [Some(1), Some(2), None] {
                    crate::ui::text_width::set_terminal_width(observed);
                    for language in [UiLanguage::En, UiLanguage::Ja] {
                        assert_eq!(WorkMessage::state(&row).render(language), source);
                        assert_eq!(row.state.as_bytes(), source.as_bytes());
                    }
                }
            }
            row.state = crate::activity::ActivityPhase::Recovery
                .column_label()
                .into();
            if !matches!(row.target, LiveWorkTarget::ChildRun { .. }) {
                assert_eq!(
                    WorkMessage::state(&row).render(UiLanguage::En),
                    "Recovery needed"
                );
                assert_eq!(WorkMessage::state(&row).render(UiLanguage::Ja), row.state);
            } else {
                row.state = "live".into();
                assert_eq!(WorkMessage::state(&row).render(UiLanguage::En), "Running");
                assert_eq!(WorkMessage::state(&row).render(UiLanguage::Ja), "実行中");
                assert_eq!(row.state, "live");
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn canonical_child_states_render_at_both_widths_for_one_two_and_many() {
        fn children<const COUNT: usize>(ui: &mut UiState) {
            populate(ui);
            let view = &mut ui.active_view_mut().live_work;
            let mut data = view.snapshot.as_ref().unwrap().clone();
            let child = data.rows[1].clone();
            data.rows = (0..COUNT)
                .map(|index| {
                    let mut row = child.clone();
                    row.target = LiveWorkTarget::ChildRun {
                        run_id: format!("child-run-{index}"),
                        thread_id: format!("child-thread-{index}"),
                    };
                    row.state = crate::activity::ActivityPhase::Recovery
                        .column_label()
                        .into();
                    row
                })
                .collect();
            data.children = COUNT as u64;
            data.shells = 0;
            data.waits = Some(0);
            view.accept(Some(data), Instant::now());
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Owned work", "Recovery needed"],
                UiLanguage::Ja => &["抱えている作業", "復旧待ち"],
            }
        }
        super::super::i18n::verify_screen(children::<1>, expected);
        super::super::i18n::verify_screen(children::<2>, expected);
        super::super::i18n::verify_screen(children::<10>, expected);
    }
    #[test]
    fn clipped_live_work_uses_measured_ellipsis_without_exceeding_width() {
        use crate::ui::text_width::UnicodeWidthStr;
        for (observed, ellipsis) in [(Some(1), "…"), (Some(2), "."), (None, ".")] {
            crate::ui::text_width::set_terminal_width(observed);
            assert_eq!(fit("abcdef", 4), format!("abc{ellipsis}"));
            assert_eq!(fit("abcdef", 0), "");
            assert_eq!(fit("ok", 4), "ok");
            for width in 0..10 {
                let value = fit("日e\u{301}本語abcdefgh", width);
                assert!(value.width() <= width, "{observed:?} {width}: {value}");
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn preview_decoration_does_not_rewrite_unknown_state() {
        for observed in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(observed);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let preview = WorkMessage::PreviewNotice.render(language);
                let expected_prefix = if observed == Some(1) {
                    "  │   │ … "
                } else {
                    "  |   | . "
                };
                assert!(preview.starts_with(expected_prefix), "{preview}");
                assert!(preview.contains(glyphs::separator()), "{preview}");
                assert!(
                    WorkMessage::PreviewLoading
                        .render(language)
                        .ends_with(glyphs::ellipsis())
                );
                assert_eq!(
                    WorkMessage::State("external | value").render(language),
                    "external | value"
                );
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn count_grammar_and_unknown_values_are_explicit() {
        for (count, english) in [(0, "0 more items"), (1, "1 more item"), (2, "2 more items")] {
            assert_eq!(WorkMessage::More(count).render(UiLanguage::En), english);
            assert_eq!(
                WorkMessage::More(count).render(UiLanguage::Ja),
                format!("他{count}件")
            );
        }
        assert_eq!(
            WorkMessage::Deadline(None).render(UiLanguage::En),
            "Deadline unknown"
        );
        assert!(
            WorkMessage::Counts {
                shells: 0,
                children: 0,
                waits: None
            }
            .render(UiLanguage::En)
            .ends_with("Waits: unknown")
        );
    }
}
