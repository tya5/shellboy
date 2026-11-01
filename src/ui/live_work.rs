//! Bounded fixed-region projection of work owned by the selected Run.
//! Saved elapsed bases are independent of the UI-observed Turn clock.
use super::*;

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
    }
}
impl Eq for LiveWorkSnapshot {}
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
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
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
    result.push('…');
    result
}
fn state_label(state: &str) -> &str {
    match state {
        "starting" => "起動中",
        "running" | "live" => "実行中",
        "stopping" => "停止中",
        "waiting" => "待機中",
        _ => state,
    }
}
impl LiveWorkView {
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
    pub fn height(&self, available: u16) -> u16 {
        if self.snapshot.is_none() || available <= 3 {
            return 0;
        }
        let rows = self.rows().len().clamp(1, 10) as u16;
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
        self.visible_rows = self
            .rows()
            .len()
            .min(usize::from(area.height.saturating_sub(2)));
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
            " · 最後の出力 {}前",
            crate::presentation::elapsed_text(age.saturating_mul(1000), false)
        ))
    }
    /// The heading's blink origin (#90): the section's existing seconds
    /// clock, only while the section is drawn.
    pub fn blink_origin(&self) -> Option<Instant> {
        self.area.and(self.snapshot.as_ref()).and(self.clock_origin)
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
            Ok(_) => self.error = Some("出力の表示上限を超えました · F6 詳細".into()),
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
    pub fn footer(&self) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        let timing = self
            .seconds
            .first()
            .and_then(|(elapsed, _)| *elapsed)
            .map(|s| {
                format!(
                    " · 最古 {}",
                    crate::presentation::elapsed_text(s.saturating_mul(1000), false)
                )
            })
            .unwrap_or_default();
        let output = self.output_age_label(0).unwrap_or_default();
        Some(format!(
            "{} · shell {} · 子 {} · 待機 {}{timing}{output}",
            crate::presentation::rows::RowKind::LiveWork.spec().1,
            snapshot.shells,
            snapshot.children,
            snapshot
                .waits
                .map(|n| n.to_string())
                .unwrap_or_else(|| "未取得".into())
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
        format!(" · 他{hidden}件")
    } else {
        String::new()
    };
    let counts = view
        .snapshot
        .as_ref()
        .map(|s| {
            format!(
                " · shell {} · 子 {} · 待機 {}",
                s.shells,
                s.children,
                s.waits
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "未取得".into())
            )
        })
        .unwrap_or_default();
    let (mark, name, _) = crate::presentation::rows::RowKind::LiveWork.spec();
    let heading = fit(
        &format!("{mark} {name}{hidden_label}{counts}"),
        usize::from(area.width),
    );
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
        let marker = if view.active && view.selected.as_ref() == Some(&row.target) {
            "›"
        } else {
            "·"
        };
        let kind = match row.target {
            LiveWorkTarget::Shell { .. } => "shell",
            LiveWorkTarget::ChildRun { .. } => "子",
            LiveWorkTarget::Wait { .. } => "待機",
        };
        let elapsed = view
            .seconds
            .get(index)
            .and_then(|(e, _)| *e)
            .map(|s| {
                format!(
                    " · {}",
                    crate::presentation::elapsed_text(s.saturating_mul(1000), false)
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
                        " · 期限目安 {}",
                        crate::presentation::elapsed_text(
                            timeout
                                .saturating_sub(Duration::from_secs(elapsed))
                                .as_millis()
                                .min(u128::from(u64::MAX)) as u64,
                            false
                        )
                    ),
                    _ => " · 期限不明".to_owned(),
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

        let extra = if index + 1 == view.visible_rows {
            view.snapshot.as_ref().map_or(0, |s| {
                s.overflow
                    .saturating_add((s.rows.len() - view.visible_rows) as u64)
            })
        } else {
            0
        };
        let overflow = if extra > 0 {
            format!(" · 他{extra}件")
        } else {
            String::new()
        };
        use unicode_width::UnicodeWidthStr;
        let prefix = format!(
            "  │ {marker} [{}] {kind} {}",
            index + 1,
            linked
                .map(|i| format!("→ [{}] ", i + 1))
                .unwrap_or_default()
        );
        let right_budget = usize::from(area.width) / 2;
        let overflow = fit(&overflow, right_budget);
        let right = format!(
            "{}{overflow}",
            fit(
                &format!("{}{elapsed}{deadline}{age}", state_label(&row.state)),
                right_budget.saturating_sub(overflow.width()),
            )
        );
        let label = fit(
            if describe_calls && !(view.active && view.selected.as_ref() == Some(&row.target)) {
                row.description.as_deref().unwrap_or(&row.label)
            } else {
                &row.label
            },
            usize::from(area.width).saturating_sub(prefix.width() + right.width() + 3),
        );
        let gap =
            usize::from(area.width).saturating_sub(prefix.width() + label.width() + right.width());
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
    if view
        .open
        .as_ref()
        .is_some_and(|id| view.visible_execution(id))
        && lines.len() < usize::from(area.height)
    {
        if let Some(error) = &view.error {
            lines.push(Line::styled(
                error.clone(),
                Style::default().fg(theme::ATTENTION),
            ));
        }
        if let Some(preview) = &view.preview {
            for (text, error) in [(&preview.stdout, false), (&preview.stderr, true)] {
                for line in text.lines() {
                    if lines.len() >= usize::from(area.height) {
                        break;
                    }
                    lines.push(Line::styled(
                        format!("  │   │ {line}"),
                        if error {
                            Style::default().fg(theme::ATTENTION)
                        } else {
                            Style::default().fg(palette.muted())
                        },
                    ));
                }
            }
            if (preview.skipped || preview.decode_uncertain)
                && lines.len() < usize::from(area.height)
            {
                lines.push(Line::from("  │   │ … 表示用抜粋 · F6 詳細"));
            }
        } else if view.error.is_none() {
            lines.push(Line::from("  │   │ 出力を取得中…"));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}
impl UiState {
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
    use super::*;
    fn snapshot(at: Instant) -> LiveWorkSnapshot {
        LiveWorkSnapshot {
            run_id: "owner".into(),
            shells: 2,
            children: 1,
            waits: Some(1),
            overflow: 1,
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
        let mut ui = UiState::new(&Config::default(), PathBuf::from("/"));
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
    fn configured_cap_applies_to_ui_and_counts_hidden_rows() {
        let at = Instant::now();
        let mut config = Config::default();
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
            let mut config = Config::default();
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
            let mut view = LiveWorkView::default();
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
        let mut config = Config::default();
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
        let all = snapshot(at);
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
        let mut view = LiveWorkView::default();
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
        let mut view = LiveWorkView::default();
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
        assert_eq!(view.height(30), 0);
        assert!(view.preview.is_none());
    }
    #[test]
    fn actual_draw_and_focus_keep_live_work_separate_from_history_and_composer() {
        let at = Instant::now();
        let mut ui = ui(at);
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &mut ui))
            .unwrap();
        let area = ui.view().live_work.area.unwrap();
        assert!(ui.view().scroll.area.unwrap().height >= 3);
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
            text.contains("他1件"),
            "exact overflow count must remain visible: {text}"
        );
        assert!(!text.contains("answered-turn"));
        assert!(!text.contains("child-run"));
        ui.insert("draft stays", false);
        let anchor = ui.view().anchor.clone();
        assert!(ui.live_work_click(area.x, area.y + 3));
        assert!(ui.view().live_work.active);
        assert_eq!(ui.view().draft.text, "draft stays");
        assert_eq!(ui.view().anchor, anchor);
        let actions = ui
            .live_work_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE))
            .unwrap();
        assert!(actions.iter().any(|action|matches!(action,UiAction::SelectThread {thread_id,..} if thread_id=="child-thread")));
        assert!(!ui.view().live_work.active);
    }
    #[test]
    fn wait_target_number_and_approximate_deadline_are_visible() {
        let at = Instant::now();
        let mut view = LiveWorkView::default();
        let mut data = snapshot(at);
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
        assert!(
            text.contains("[1] shell") && text.contains("→ [1]"),
            "{text}"
        );
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
        let mut view = LiveWorkView::default();
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
            Some(" · 最後の出力 1秒前")
        );
        let mut saved = snapshot(at + Duration::from_secs(5));
        saved.rows[0].last_output_at = Some("new saved timestamp".into());
        saved.rows[0].output_age_base = Some(Duration::from_secs(1));
        view.accept(Some(saved.clone()), at + Duration::from_secs(5));
        assert!(!view.markers.contains_key("execution"));
        assert_eq!(
            view.output_age_label(0).as_deref(),
            Some(" · 最後の出力 1秒前")
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
            Some(" · 最後の出力 1秒前")
        );
    }
}
