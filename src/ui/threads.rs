//! Thread-list presentation; previews use the same Markdown parser as history.
use super::theme;
use super::{UiAction, popup};
use crate::protocol::ThreadSummary;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pulldown_cmark::{Event, Options, Parser, TagEnd};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Reader-prepared wall-clock labels. Raw timestamps remain authoritative.
#[derive(Clone, Debug)]
pub struct ThreadDisplayTime {
    pub thread_id: String,
    pub raw_updated_at: String,
    pub date_time: String,
    pub time: String,
}

const FILTER_DELAY: Duration = Duration::from_millis(150);

#[derive(Default)]
pub(super) struct ThreadBrowser {
    collapsed: BTreeSet<String>,
    display_times: BTreeMap<String, ThreadDisplayTime>,
    items: Vec<ThreadSummary>,
    visible: bool,
    selected: usize,
    filter: String,
    next_cursor: Option<String>,
    loading: bool,
    error: Option<String>,
    preferred: Option<String>,
    generation: u64,
    filter_deadline: Option<Instant>,
}

pub(super) enum ThreadOutcome {
    None,
    Load(Box<UiAction>),
    Open(String),
    Info(String),
}

struct TreeRow {
    index: usize,
    ancestors: Vec<bool>,
    depth: usize,
    last: bool,
    missing_parent: bool,
    cycle: bool,
    children: bool,
}

impl ThreadBrowser {
    pub fn items(&self) -> &[ThreadSummary] {
        &self.items
    }
    pub fn visible(&self) -> bool {
        self.visible
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn filter(&self) -> &str {
        &self.filter
    }
    pub fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn close(&mut self) {
        self.visible = false;
        self.filter_deadline = None;
    }
    fn invalidate(&mut self, cursor: Option<String>) {
        self.next_cursor = cursor;
        self.generation += 1;
        self.loading = true;
        self.error = None;
    }
    fn begin(&mut self, cursor: Option<String>) -> UiAction {
        self.filter_deadline = None;
        self.invalidate(cursor.clone());
        self.request(cursor)
    }
    pub(super) fn take_filter_request(&mut self, now: Instant) -> Option<UiAction> {
        if !self.visible || self.filter_deadline.is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.filter_deadline = None;
        Some(self.request(None))
    }
    pub(super) fn filter_wait(&self, now: Instant) -> Option<Duration> {
        self.filter_deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }
    fn request(&self, cursor: Option<String>) -> UiAction {
        UiAction::LoadThreads {
            cursor,
            filter: self.filter.clone(),
            generation: self.generation,
        }
    }
    pub fn open(
        &mut self,
        filter: String,
        drafts: Vec<ThreadSummary>,
        preferred: Option<String>,
    ) -> UiAction {
        self.visible = true;
        self.filter = filter;
        self.preferred = preferred;
        self.items = drafts;
        self.selected = 0;
        self.begin(None)
    }
    pub fn failed(&mut self, generation: u64, error: String) {
        if generation == self.generation {
            self.loading = false;
            self.error = Some(error);
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        generation: u64,
        requested_cursor: Option<String>,
        items: Vec<ThreadSummary>,
        display_times: Vec<ThreadDisplayTime>,
        next_cursor: Option<String>,
        drafts: Vec<ThreadSummary>,
        active: Option<String>,
    ) -> bool {
        if generation != self.generation
            || (requested_cursor.is_some() && requested_cursor != self.next_cursor)
        {
            return false;
        }
        let selected = self.preferred.take().or_else(|| {
            self.items
                .get(self.selected)
                .map(|t| t.thread_id.clone())
                .or(active)
        });
        if requested_cursor.is_none() {
            self.items.clear();
        }
        let mut positions: BTreeMap<_, _> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, t)| (t.thread_id.clone(), i))
            .collect();
        for item in items.into_iter().chain(drafts) {
            if let Some(index) = positions.get(&item.thread_id) {
                self.items[*index] = item;
            } else if self.items.len() < 2000 {
                positions.insert(item.thread_id.clone(), self.items.len());
                self.items.push(item);
            }
        }
        self.update_times(display_times);
        self.loading = false;
        self.error = None;
        self.next_cursor = next_cursor;
        if self.items.len() >= 2000 && self.next_cursor.is_some() {
            self.error = Some("表示上限2000件。文字入力で絞り込んでください".into());
        }
        self.selected = selected
            .and_then(|id| self.items.iter().position(|t| t.thread_id == id))
            .unwrap_or_else(|| self.visible_indices().first().copied().unwrap_or(0));
        self.reveal(self.selected);
        true
    }
    pub fn key(&mut self, key: KeyEvent) -> ThreadOutcome {
        self.key_at(key, Instant::now())
    }
    fn key_at(&mut self, key: KeyEvent, now: Instant) -> ThreadOutcome {
        if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Left
                | KeyCode::Right
        ) {
            self.preferred = None;
        }
        let previous_filter_len = self.filter.len();
        let visible = self.visible_indices();
        let position = visible
            .iter()
            .position(|i| *i == self.selected)
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc => self.close(),
            KeyCode::F(1) => {
                if let Some(thread) = self.items.get(self.selected) {
                    return ThreadOutcome::Info(format!(
                        "会話ID: {}\nタイトル: {}\n作業フォルダー: {}\n親の会話: {}\n状態: {}\n更新日時: {}\n\n{}",
                        thread.thread_id,
                        thread.title,
                        thread.cwd,
                        thread.parent_id.as_deref().unwrap_or("root"),
                        thread.state,
                        thread.updated_at,
                        thread.excerpt
                    ));
                }
            }
            KeyCode::Up => {
                if let Some(index) = visible.get(position.saturating_sub(1)) {
                    self.selected = *index;
                }
            }
            KeyCode::Down => {
                if let Some(index) =
                    visible.get((position + 1).min(visible.len().saturating_sub(1)))
                {
                    self.selected = *index;
                }
            }
            KeyCode::Home => {
                if let Some(index) = visible.first() {
                    self.selected = *index;
                }
            }
            KeyCode::End => {
                if let Some(index) = visible.last() {
                    self.selected = *index;
                }
            }
            KeyCode::Left => {
                if !self.collapse(self.selected)
                    && let Some(parent) = self.parent_index(self.selected)
                {
                    self.selected = parent;
                    self.reveal(parent);
                }
            }
            KeyCode::Right => {
                if !self.expand(self.selected)
                    && let Some(thread) = self.items.get(self.selected)
                    && let Some(child) = self.items.iter().position(|t| {
                        t.thread_id != thread.thread_id
                            && t.parent_id.as_deref() == Some(&thread.thread_id)
                    })
                {
                    self.selected = child;
                    self.reveal(child);
                }
            }
            KeyCode::Enter => {
                if let Some(thread) = self.items.get(self.selected) {
                    return ThreadOutcome::Open(thread.thread_id.clone());
                }
            }
            KeyCode::PageDown => {
                if !self.loading
                    && self.items.len() < 2000
                    && let Some(cursor) = self.next_cursor.clone()
                {
                    return ThreadOutcome::Load(Box::new(self.begin(Some(cursor))));
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return ThreadOutcome::Load(Box::new(self.begin(None)));
            }
            KeyCode::Backspace => {
                if let Some((at, _)) = self.filter.grapheme_indices(true).next_back() {
                    self.filter.truncate(at);
                }
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.filter.chars().count() < 256 {
                    self.filter.push(ch);
                }
            }
            _ => return ThreadOutcome::None,
        }
        if self.filter.len() != previous_filter_len {
            self.invalidate(None);
            self.filter_deadline = Some(now + FILTER_DELAY);
        }
        ThreadOutcome::None
    }
    pub fn update_times(&mut self, times: Vec<ThreadDisplayTime>) {
        let threads = &self.items;
        let current: BTreeMap<_, _> = threads
            .iter()
            .map(|t| (t.thread_id.as_str(), t.updated_at.as_str()))
            .collect();
        self.display_times.retain(|id, time| {
            current
                .get(id.as_str())
                .is_some_and(|raw| *raw == time.raw_updated_at)
        });
        for time in times {
            if current
                .get(time.thread_id.as_str())
                .is_some_and(|raw| *raw == time.raw_updated_at)
            {
                self.display_times.insert(time.thread_id.clone(), time);
            }
        }
    }
    fn short_time(&self, thread: &ThreadSummary, width: usize) -> String {
        let value = self
            .display_times
            .get(&thread.thread_id)
            .filter(|time| time.raw_updated_at == thread.updated_at)
            .map(|time| {
                if width >= 11 {
                    time.date_time.as_str()
                } else {
                    time.time.as_str()
                }
            })
            .unwrap_or(&thread.updated_at);
        fit(&one_line(value), width)
    }
    fn rows(&self) -> Vec<TreeRow> {
        let threads = &self.items;
        let ids: BTreeMap<_, _> = threads
            .iter()
            .enumerate()
            .map(|(i, t)| (t.thread_id.as_str(), i))
            .collect();
        let mut children = vec![Vec::new(); threads.len()];
        let mut roots = Vec::new();
        for (index, thread) in threads.iter().enumerate() {
            if let Some(parent) = thread
                .parent_id
                .as_deref()
                .and_then(|id| ids.get(id))
                .copied()
            {
                children[parent].push(index);
            } else {
                roots.push(index);
            }
        }
        let mut seen = vec![false; threads.len()];
        let mut rows = Vec::new();
        // Every stack frame carries at most eight continuation marks. A deep or
        // cyclic graph therefore cannot overflow the stack or grow indentation.
        let mut stack: Vec<(usize, Vec<bool>, usize, bool, bool, bool)> = Vec::new();
        for &index in roots.iter().rev() {
            stack.push((index, Vec::new(), 0, true, true, false));
        }
        let mut remaining = 0;
        loop {
            if stack.is_empty() {
                while remaining < threads.len() && seen[remaining] {
                    remaining += 1;
                }
                if remaining == threads.len() {
                    break;
                }
                // Components with no root contain a cycle. Display it explicitly
                // instead of silently presenting one of its members as a root.
                stack.push((remaining, Vec::new(), 0, true, true, true));
            }
            let Some((index, ancestors, depth, last, visible, cycle)) = stack.pop() else {
                continue;
            };
            if seen[index] {
                continue;
            }
            seen[index] = true;
            let thread = &threads[index];
            if visible {
                rows.push(TreeRow {
                    index,
                    ancestors: ancestors.clone(),
                    depth,
                    last,
                    missing_parent: thread
                        .parent_id
                        .as_deref()
                        .is_some_and(|id| !ids.contains_key(id)),
                    cycle,
                    children: !children[index].is_empty(),
                });
            }
            let show_children = visible && !self.collapsed.contains(&thread.thread_id);
            for (position, &child) in children[index].iter().enumerate().rev() {
                let mut continuation = ancestors.clone();
                if depth > 0 && continuation.len() < 8 {
                    continuation.push(!last);
                }
                stack.push((
                    child,
                    continuation,
                    depth.saturating_add(1),
                    position + 1 == children[index].len(),
                    show_children,
                    false,
                ));
            }
        }
        rows
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.rows().into_iter().map(|row| row.index).collect()
    }

    fn parent_index(&self, index: usize) -> Option<usize> {
        let threads = &self.items;
        let parent = threads.get(index)?.parent_id.as_deref()?;
        threads.iter().position(|thread| {
            thread.thread_id == parent && thread.thread_id != threads[index].thread_id
        })
    }

    fn collapse(&mut self, index: usize) -> bool {
        let threads = &self.items;
        let Some(thread) = threads.get(index) else {
            return false;
        };
        if !threads
            .iter()
            .any(|child| child.parent_id.as_deref() == Some(thread.thread_id.as_str()))
        {
            return false;
        }
        self.collapsed.insert(thread.thread_id.clone())
    }

    fn expand(&mut self, index: usize) -> bool {
        let threads = &self.items;
        threads
            .get(index)
            .is_some_and(|thread| self.collapsed.remove(&thread.thread_id))
    }

    fn reveal(&mut self, mut index: usize) {
        let mut seen = BTreeSet::new();
        while let Some(parent) = self.parent_index(index) {
            if !seen.insert(parent) {
                break;
            }
            self.expand(parent);
            index = parent;
        }
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, active_thread: Option<&str>) {
        let threads = &self.items;
        let selected_index = self.selected;
        let filter = &self.filter;
        let next_cursor = self.next_cursor.is_some();
        let loading = self.loading;
        let error = self.error.as_deref();
        let rows = self.rows();
        let title = format!(
            "会話 · {}件{}{}",
            rows.len(),
            if rows.len() != threads.len() {
                format!(" / 取得{}件", threads.len())
            } else {
                String::new()
            },
            if loading {
                " · 読込み中"
            } else if error.is_some() {
                " · 取得失敗"
            } else {
                ""
            }
        );
        let footer_width = usize::from(area.width.saturating_sub(8).min(108));
        let footer_text = if let Some(error) = error.filter(|error| error.starts_with("表示上限"))
        {
            format!("Esc戻る · {}", one_line(error))
        } else if let Some(error) = error {
            format!("取得失敗 · Esc戻る\nCtrl+R再取得 · {}", one_line(error))
        } else {
            let mut keys = if footer_width >= 90 {
                "Esc戻る · ↑↓選択 · ←→展開 · Enter切替 · F1会話情報 · Ctrl+R再取得".to_owned()
            } else if footer_width >= 30 {
                "Esc戻る Enter切替 F1会話情報\n↑↓選択 ←→開閉 Ctrl+R再取得".to_owned()
            } else if footer_width >= 19 {
                "Esc戻る Enter切替\nF1情報 Ctrl+R再取得".to_owned()
            } else if footer_width >= 17 {
                "Esc戻る Enter切替\nF1情報".to_owned()
            } else {
                "Esc戻る".to_owned()
            };
            if next_cursor {
                let last_width = keys.lines().next_back().unwrap_or_default().width();
                if last_width + " PgDn次頁".width() <= footer_width {
                    keys.push_str(" PgDn次頁");
                }
            }
            keys
        };
        let context = match error {
            Some(error) => format!("取得失敗 {} · 検索 › {}", one_line(error), one_line(filter)),
            None => format!("検索 › {}", one_line(filter)),
        };
        let inner = popup::panel(frame, area, &title, &context, &footer_text);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let detail_height = if inner.height >= 10 {
            4
        } else if inner.height >= 7 {
            2
        } else {
            0
        };
        let detail = Rect {
            y: inner.bottom().saturating_sub(detail_height),
            height: detail_height,
            ..inner
        };
        if detail_height > 0 {
            self.draw_detail(frame, detail, threads.get(selected_index));
        }
        let table_area = Rect {
            height: inner.height.saturating_sub(detail_height),
            ..inner
        };
        if table_area.height == 0 {
            return;
        }
        let width = usize::from(table_area.width);
        let (cwd_width, state_width, time_width) = if width >= 90 {
            (24, 8, 11)
        } else if width >= 64 {
            (16, 8, 11)
        } else if width >= 44 {
            (0, 8, 5)
        } else if width >= 20 {
            (0, 6, 0)
        } else {
            (0, 0, 0)
        };
        let gaps =
            usize::from(cwd_width > 0) + usize::from(state_width > 0) + usize::from(time_width > 0);
        let title_width = width.saturating_sub(cwd_width + state_width + time_width + gaps);
        let mut columns = vec![Constraint::Length(title_width as u16)];
        let mut headers = vec![Cell::from("会話")];
        if cwd_width > 0 {
            columns.push(Constraint::Length(cwd_width as u16));
            headers.push(Cell::from("作業場所"));
        }
        if state_width > 0 {
            columns.push(Constraint::Length(state_width as u16));
            headers.push(Cell::from("状態"));
        }
        if time_width > 0 {
            columns.push(Constraint::Length(time_width as u16));
            headers.push(Cell::from("更新"));
        }
        let selected_visible = rows.iter().position(|row| row.index == selected_index);
        let table_rows = rows.iter().map(|row| {
            let thread = &threads[row.index];
            let selected = row.index == selected_index;
            let current = active_thread == Some(thread.thread_id.as_str());
            let label = self.label(row, thread, selected, current, title_width);
            let status_style = state_style(&thread.state);
            let mut cells = vec![Cell::from(label).style(status_style)];
            if cwd_width > 0 {
                cells.push(Cell::from(tail_path(&thread.cwd, cwd_width)));
            }
            if state_width > 0 {
                cells.push(
                    Cell::from(fit(&one_line(state(&thread.state)), state_width))
                        .style(status_style),
                );
            }
            if time_width > 0 {
                cells.push(Cell::from(self.short_time(thread, time_width)));
            }
            Row::new(cells).style(if selected {
                popup::selected_style()
            } else {
                popup::normal_style()
            })
        });
        let table = Table::new(table_rows, columns)
            .column_spacing(1)
            .header(Row::new(headers).style(Style::default().fg(theme::MUTED)));
        let mut selection = TableState::default().with_selected(selected_visible);
        frame.render_stateful_widget(table, table_area, &mut selection);
    }

    fn label(
        &self,
        row: &TreeRow,
        thread: &ThreadSummary,
        selected: bool,
        current: bool,
        width: usize,
    ) -> String {
        let mut label = format!(
            "{}{}",
            if selected { "›" } else { " " },
            if current { "●" } else { " " }
        );
        // Reserve enough room for a title at narrow widths. The ellipsis marks
        // omitted ancestor indentation while the local branch stays visible.
        let depth_budget = width.saturating_sub(14).min(16) / 2;
        let take = row.ancestors.len().min(depth_budget);
        for &continues in row.ancestors.iter().take(take) {
            label.push_str(if continues { "│ " } else { "  " });
        }
        if row.depth > take + 1 {
            label.push_str("… ");
        }
        if row.depth > 0 {
            label.push_str(if row.last { "└─" } else { "├─" });
        }
        label.push_str(if row.children {
            if self.collapsed.contains(&thread.thread_id) {
                "▸ "
            } else {
                "▾ "
            }
        } else {
            "  "
        });
        if row.missing_parent {
            label.push_str("[親未取得] ");
        }
        if row.cycle {
            label.push_str("[循環] ");
        }
        let title = one_line(&thread.title);
        label.push_str(if title.width() == 0 {
            "名称未設定"
        } else {
            &title
        });
        fit(&label, width)
    }

    fn draw_detail(&self, frame: &mut Frame<'_>, area: Rect, thread: Option<&ThreadSummary>) {
        let block = Block::default().borders(Borders::TOP);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(thread) = thread else {
            frame.render_widget(Paragraph::new("会話がありません"), inner);
            return;
        };
        let width = usize::from(inner.width);
        let lines = vec![
            Line::from(fit(&format!("cwd {}", one_line(&thread.cwd)), width)),
            Line::from(fit(
                &format!(
                    "ID {} · 親 {} · {}",
                    one_line(&thread.thread_id),
                    thread
                        .parent_id
                        .as_deref()
                        .map(one_line)
                        .unwrap_or_else(|| "なし".into()),
                    one_line(state(&thread.state))
                ),
                width,
            )),
            Line::from(Span::styled(
                plain_preview(&thread.excerpt, width),
                Style::default().fg(theme::TEXT),
            )),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn one_line(text: &str) -> String {
    crate::presentation::sanitize_plain(&text.replace('\r', "\n"), 0)
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if used + cells + 1 > width {
            break;
        }
        out.push_str(grapheme);
        used += cells;
    }
    out.push('…');
    out
}

fn tail_path(path: &str, width: usize) -> String {
    let path = one_line(path);
    if path.width() <= width {
        return path;
    }
    if width == 0 {
        return String::new();
    }
    let mut suffix = Vec::new();
    let mut used = 1;
    for grapheme in path.graphemes(true).rev() {
        let cells = grapheme.width();
        if used + cells > width {
            break;
        }
        suffix.push(grapheme);
        used += cells;
    }
    format!("…{}", suffix.into_iter().rev().collect::<String>())
}

fn state_style(value: &str) -> Style {
    Style::default().fg(match value {
        "preparing" | "running" | "live" => theme::ACCENT,
        "waiting" | "stopping" => theme::WARNING,
        "recovery_wait" | "failed" => theme::ATTENTION,
        "completed" => theme::SUCCESS,
        // Inherit the row's foreground for inactive/unknown states so the
        // selected row keeps its contrasting foreground on light terminals.
        _ => return Style::default(),
    })
}

fn state(value: &str) -> &str {
    match value {
        "preparing" => "準備",
        "running" => "実行",
        "waiting" => "待機",
        "recovery_wait" => "復旧待ち",
        "live" => "稼働",
        "stopping" => "停止中",
        "stopped" => "停止",
        "completed" => "完了",
        "failed" => "失敗",
        "idle" => "未実行",
        "draft" => "下書き",
        other => other,
    }
}

/// One line of visible Markdown text, bounded by graphemes and terminal cells.
/// The ellipsis is included in both limits. Never split a combining sequence.
pub(super) fn plain_preview(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    // Preserve standalone CR as a word boundary before removing terminal escapes.
    let safe = crate::presentation::sanitize_plain(&text.replace('\r', "\n"), 0).0;
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut plain = String::new();
    for event in Parser::new_ext(&safe, options) {
        match event {
            Event::Text(text) | Event::Code(text) => plain.push_str(&text),
            Event::SoftBreak | Event::HardBreak | Event::Rule => plain.push(' '),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::BlockQuote(_)
                | TagEnd::TableCell
                | TagEnd::TableRow,
            ) => plain.push(' '),
            _ => {}
        }
    }
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let graphemes: Vec<_> = plain.graphemes(true).collect();
    if graphemes.len() <= 80 && plain.width() <= width {
        return plain;
    }
    let mut preview = String::new();
    let mut cells = 0;
    for grapheme in graphemes.into_iter().take(79) {
        let next = grapheme.width();
        if cells + next + 1 > width {
            break;
        }
        preview.push_str(grapheme);
        cells += next;
    }
    preview.push('…');
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ux_title_uses_the_same_conversation_term_as_its_rows() {
        let shown = text(&capture(&[thread("fixture", None)], 0, 60, 18, None));
        assert!(shown.contains("会話 ·"), "{shown}");
        assert!(!shown.contains("スレッド"));
    }
    #[test]
    fn ux_compact_read_failure_keeps_failure_and_return_visible_without_context_rows() {
        let shown = text(&capture(&[], 0, 35, 10, Some("fixture unavailable")));
        assert!(
            shown.contains("取得失敗") && shown.contains("Esc戻る"),
            "{shown}"
        );
    }
    #[test]
    fn narrow_footer_preserves_action_meanings_instead_of_naked_keys() {
        let rows = vec![thread("fixture", None)];
        for (width, height) in [(60, 18), (120, 38)] {
            let shown = text(&capture(&rows, 0, width, height, None));
            for meaning in ["Esc戻る", "Enter切替", "F1会話情報", "Ctrl+R再取得"] {
                assert!(
                    shown.contains(meaning),
                    "{width}x{height}: missing{meaning}: {shown}"
                );
            }
        }
        let tiny = text(&capture(&rows, 0, 35, 10, None));
        assert!(
            tiny.contains("Esc戻る") && tiny.contains("Enter切替"),
            "{tiny}"
        );
    }
    #[test]
    fn prepared_times_are_bound_to_id_and_raw_revision_and_draw_only_reads_labels() {
        let mut row = thread("t", None);
        let mut tree = ThreadBrowser {
            items: vec![row.clone()],
            ..Default::default()
        };
        tree.update_times(vec![ThreadDisplayTime {
            thread_id: row.thread_id.clone(),
            raw_updated_at: row.updated_at.clone(),
            date_time: "12-31 23:59".into(),
            time: "23:59".into(),
        }]);
        assert_eq!(tree.short_time(&row, 11), "12-31 23:59");
        assert_eq!(tree.short_time(&row, 5), "23:59");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None))
            .unwrap();
        assert!(text(terminal.backend().buffer()).contains("12-31 23:59"));
        row.updated_at = "UNKNOWN".into();
        assert_eq!(tree.short_time(&row, 11), "UNKNOWN");
        tree.items = vec![row.clone()];
        tree.update_times(vec![]);
        assert!(tree.display_times.is_empty(), "stale labels are evicted");
        row.thread_id = "other".into();
        assert_eq!(tree.short_time(&row, 11), "UNKNOWN");
    }

    fn thread(id: &str, parent: Option<&str>) -> ThreadSummary {
        ThreadSummary {
            thread_id: id.into(),
            parent_id: parent.map(Into::into),
            title: format!("会話 {id}"),
            cwd: format!("/workspace/{id}"),
            state: "running".into(),
            updated_at: "2026-10-25T09:30:00Z".into(),
            excerpt: "**選択した会話の抜粋**".into(),
        }
    }

    #[test]
    fn state_colors_survive_selection_narrow_widths_and_same_id_refresh() {
        use ratatui::{Terminal, backend::TestBackend};
        let states = [
            ("running", theme::ACCENT),
            ("preparing", theme::ACCENT),
            ("live", theme::ACCENT),
            ("waiting", theme::WARNING),
            ("stopping", theme::WARNING),
            ("recovery_wait", theme::ATTENTION),
            ("failed", theme::ATTENTION),
            ("completed", theme::SUCCESS),
            ("stopped", theme::MUTED),
            ("idle", theme::MUTED),
            ("draft", theme::MUTED),
            ("future_state", theme::MUTED),
        ];
        let items = states
            .iter()
            .enumerate()
            .map(|(i, (state, _))| {
                let mut item = thread(&format!("s{i}"), None);
                item.title = format!("Q{i:02}");
                item.state = (*state).into();
                item
            })
            .collect::<Vec<_>>();
        let mut tree = ThreadBrowser::default();
        tree.open(String::new(), vec![], Some("s7".into()));
        let generation = tree.generation();
        assert!(tree.apply(generation, None, items.clone(), vec![], None, vec![], None));
        let find_title = |buffer: &ratatui::buffer::Buffer, title: &str| {
            buffer
                .content
                .chunks(buffer.area.width as usize)
                .find_map(|row| {
                    row.windows(title.len())
                        .find(|cells| {
                            cells.iter().map(|cell| cell.symbol()).collect::<String>() == title
                        })
                        .map(|cells| cells[0].clone())
                })
                .expect("thread title visible")
        };
        for width in [120, 24] {
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            terminal
                .draw(|frame| tree.draw(frame, frame.area(), None))
                .unwrap();
            for (i, (_, color)) in states.iter().enumerate() {
                let cell = find_title(terminal.backend().buffer(), &format!("Q{i:02}"));
                assert_eq!(cell.fg, *color, "width={width}, state={}", states[i].0);
                if i == 7 {
                    assert_eq!(cell.bg, theme::SELECTION_BACKGROUND);
                }
            }
        }
        let mut refreshed = items;
        refreshed[7].state = "recovery_wait".into();
        assert!(tree.apply(generation, None, refreshed, vec![], None, vec![], None));
        assert_eq!(tree.items()[tree.selected()].thread_id, "s7");
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None))
            .unwrap();
        assert_eq!(
            find_title(terminal.backend().buffer(), "Q07").fg,
            theme::ATTENTION
        );
        let mut inactive = tree.items().to_vec();
        inactive[7].state = "idle".into();
        assert!(tree.apply(generation, None, inactive, vec![], None, vec![], None));
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None))
            .unwrap();
        let selected = find_title(terminal.backend().buffer(), "Q07");
        assert_eq!(selected.fg, theme::SELECTION_FOREGROUND);
        assert_eq!(selected.bg, theme::SELECTION_BACKGROUND);
    }

    #[test]
    fn forest_preserves_root_and_sibling_source_order_and_folds_descendants() {
        let threads = vec![
            thread("a2", Some("a")),
            thread("b", None),
            thread("a", None),
            thread("a1", Some("a")),
            thread("grand", Some("a2")),
        ];
        let mut tree = ThreadBrowser {
            items: threads.clone(),
            ..Default::default()
        };
        assert_eq!(tree.visible_indices(), [1, 2, 0, 4, 3]);
        assert_eq!(tree.parent_index(4), Some(0));
        assert!(tree.collapse(2));
        assert_eq!(tree.visible_indices(), [1, 2]);
        assert!(!tree.collapse(2));
        tree.reveal(4);
        assert_eq!(tree.visible_indices(), [1, 2, 0, 4, 3]);
        assert!(tree.collapse(0));
        assert_eq!(tree.visible_indices(), [1, 2, 0, 3]);
        assert!(tree.expand(0));
        assert!(!tree.collapse(4));
    }

    #[test]
    fn missing_parents_cycles_and_deep_trees_are_explicit_and_bounded() {
        let mut threads = vec![
            thread("orphan", Some("missing")),
            thread("cycle-a", Some("cycle-b")),
            thread("cycle-b", Some("cycle-a")),
            thread("self", Some("self")),
        ];
        let mut tree = ThreadBrowser {
            items: threads.clone(),
            ..Default::default()
        };
        let rows = tree.rows();
        assert_eq!(rows.len(), 4);
        assert!(rows[0].missing_parent);
        assert_eq!(rows.iter().filter(|row| row.cycle).count(), 2);
        for index in 0..threads.len() {
            tree.reveal(index);
        }
        assert_eq!(tree.visible_indices().len(), 4);
        for i in 0..500 {
            let parent = (i > 0).then(|| format!("deep-{}", i - 1));
            threads.push(thread(&format!("deep-{i}"), parent.as_deref()));
        }
        tree.items = threads.clone();
        let rows = tree.rows();
        assert_eq!(rows.len(), threads.len());
        assert!(rows.iter().all(|row| row.ancestors.len() <= 8));
        // Rooted components precede the intentionally malformed cycle fixtures.
        let last = rows
            .iter()
            .find(|row| threads[row.index].thread_id == "deep-499")
            .unwrap();
        let label = tree.label(last, &threads[last.index], true, false, 30);
        assert!(label.width() <= 30);
        assert!(label.contains('…'));
    }

    fn capture(
        threads: &[ThreadSummary],
        selected: usize,
        width: u16,
        height: u16,
        error: Option<&str>,
    ) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("PRIVATE BACKGROUND MUST NOT LEAK"),
                    frame.area(),
                );
                ThreadBrowser {
                    items: threads.to_vec(),
                    selected,
                    filter: "検索語".into(),
                    error: error.map(str::to_owned),
                    ..Default::default()
                }
                .draw(frame, frame.area(), Some("t03"));
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buffer: &ratatui::buffer::Buffer) -> String {
        (buffer.area.y..buffer.area.bottom())
            .map(|y| {
                let mut line = String::new();
                let mut x = buffer.area.x;
                while x < buffer.area.right() {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    // Buffer reserves a blank continuation cell for a wide
                    // glyph; it is not a space in the actual terminal text.
                    x = x.saturating_add(symbol.width().max(1) as u16);
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dense_drawing_uses_one_row_per_thread_and_fixed_selected_detail() {
        let threads: Vec<_> = (0..24).map(|i| thread(&format!("t{i:02}"), None)).collect();
        let buffer = capture(&threads, 3, 120, 36, None);
        let rendered = text(&buffer);
        assert!(
            !rendered
                .lines()
                .skip(2)
                .any(|line| line.contains("PRIVATE"))
        );
        assert!(
            rendered.contains("作業場所") && rendered.contains("状態") && rendered.contains("更新")
        );
        let title_rows: Vec<_> = rendered
            .lines()
            .filter(|line| line.contains("会話 t"))
            .collect();
        assert!(title_rows.len() >= 16 && title_rows.len() < 24);
        for pair in title_rows.windows(2) {
            let first = rendered.lines().position(|line| line == pair[0]).unwrap();
            let second = rendered.lines().position(|line| line == pair[1]).unwrap();
            assert_eq!(second, first + 1);
        }
        assert_eq!(rendered.matches("選択した会話の抜粋").count(), 1);
        assert_eq!(rendered.matches("ID t03").count(), 1);
        let selected = rendered
            .lines()
            .position(|line| line.contains("›●"))
            .unwrap() as u16;
        assert_eq!(
            buffer[(60, selected)].bg,
            popup::selected_style().bg.unwrap()
        );
    }

    #[test]
    fn drawing_follows_selection_at_narrow_width_and_reports_fetch_failure() {
        let mut threads: Vec<_> = (0..80).map(|i| thread(&format!("t{i:02}"), None)).collect();
        threads[79].title = "日本語🙂e\u{301}👨‍👩‍👧‍👦".repeat(20);
        for width in [1, 10, 24, 48, 80] {
            let buffer = capture(&threads, 79, width, 15, Some("proxy unavailable"));
            let rendered = text(&buffer);
            assert!(
                !rendered
                    .lines()
                    .skip(2)
                    .any(|line| line.contains("PRIVATE"))
            );
            assert!(!rendered.contains('\u{1b}'));
            if width >= 24 {
                assert!(rendered.contains('›'));
                assert!(rendered.contains("Ctrl+R"));
            }
        }
    }

    #[test]
    fn empty_or_invisible_metadata_title_never_leaves_a_blank_conversation_label() {
        let mut thread = ThreadSummary {
            thread_id: "blank-title".into(),
            title: String::new(),
            cwd: "/synthetic".into(),
            parent_id: None,
            state: "idle".into(),
            updated_at: String::new(),
            excerpt: String::new(),
        };
        let mut tree = ThreadBrowser::default();
        for title in ["", " \n\t", "\u{1b}[31m\u{1b}[0m"] {
            thread.title = title.into();
            tree.items = vec![thread.clone()];
            let row = tree.rows().remove(0);
            assert!(
                tree.label(&row, &thread, false, false, 50)
                    .contains("名称未設定")
            );
        }
    }

    #[test]
    fn preview_uses_markdown_text_and_collapses_block_boundaries() {
        let input = "# 見出し\n\n**太字**と[リンク](https://example.com) `x*y`\n\n- 項目\n- 次\n\n```rust\nlet x = 1;\n```";
        assert_eq!(
            plain_preview(input, 200),
            "見出し 太字とリンク x*y 項目 次 let x = 1;"
        );
        assert_eq!(
            plain_preview("a**b**c ![画像](image.png) \\*literal\\*", 200),
            "abc 画像 *literal*"
        );
    }

    #[test]
    fn preview_removes_terminal_sequences_and_normalizes_whitespace() {
        assert_eq!(
            plain_preview(" \u{1b}[31m赤\u{1b}[0m\t色\r\n次\r行\u{3000}終\u{7} ", 200),
            "赤 色 次 行 終"
        );
        assert_eq!(
            plain_preview("[label](https://example.com)\n\n次", 200),
            "label 次"
        );
        assert!(
            !plain_preview("\u{1b}]8;;https://example.com\u{7}表示\u{1b}]8;;\u{7}", 200)
                .contains('\u{1b}')
        );
    }

    #[test]
    fn preview_limits_graphemes_including_ellipsis_and_terminal_cells() {
        let text = "e\u{301}".repeat(81);
        let preview = plain_preview(&text, 200);
        assert_eq!(preview.graphemes(true).count(), 80);
        assert_eq!(preview, format!("{}…", "e\u{301}".repeat(79)));
        assert_eq!(plain_preview(&"x".repeat(80), 80), "x".repeat(80));
        assert_eq!(plain_preview("日本語🙂", 5), "日本…");
        assert_eq!(plain_preview("👨‍👩‍👧‍👦 日本語", 3), "👨‍👩‍👧‍👦…");
        assert_eq!(plain_preview("日本語", 1), "…");
        assert_eq!(plain_preview("abc", 0), "");
        assert_eq!(plain_preview("abc", 3), "abc");
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn filter_burst_invalidates_old_pages_without_emitting_each_key() {
        let now = Instant::now();
        let mut browser = ThreadBrowser::default();
        browser.open(String::new(), vec![], None);
        let original = browser.generation();
        for (index, ch) in "日本語".chars().enumerate() {
            let outcome = browser.key_at(
                key(KeyCode::Char(ch)),
                now + std::time::Duration::from_millis(index as u64 * 40),
            );
            assert!(
                matches!(outcome, ThreadOutcome::None),
                "filter should coalesce instead of querying every key"
            );
        }
        assert_eq!(browser.filter(), "日本語");
        assert_eq!(browser.generation(), original + 3);
        assert!(
            !browser.apply(original, None, vec![], vec![], None, vec![], None),
            "old page is stale immediately, before the delayed request"
        );
    }
    #[test]
    fn trailing_deadline_sends_only_the_latest_filter_once() {
        let now = Instant::now();
        let mut browser = ThreadBrowser::default();
        browser.open(String::new(), vec![], None);
        browser.key_at(key(KeyCode::Char('a')), now);
        browser.key_at(key(KeyCode::Char('b')), now + Duration::from_millis(100));
        assert!(
            browser
                .take_filter_request(now + Duration::from_millis(249))
                .is_none()
        );
        let generation = browser.generation();
        assert!(
            matches!(browser.take_filter_request(now + Duration::from_millis(250)),
            Some(UiAction::LoadThreads { cursor: None, filter, generation: g }) if filter == "ab" && g == generation)
        );
        assert!(
            browser
                .take_filter_request(now + Duration::from_secs(1))
                .is_none()
        );
        assert!(browser.filter_wait(now).is_none());
    }
    #[test]
    fn explicit_refresh_close_and_unchanged_filter_do_not_leave_delayed_queries() {
        let now = Instant::now();
        let mut browser = ThreadBrowser::default();
        browser.open(String::new(), vec![], None);
        let original = browser.generation();
        browser.key_at(key(KeyCode::Backspace), now);
        assert_eq!(browser.generation(), original);
        browser.key_at(key(KeyCode::Char('a')), now);
        assert!(matches!(
            browser.key_at(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                now
            ),
            ThreadOutcome::Load(_)
        ));
        assert!(
            browser
                .take_filter_request(now + Duration::from_secs(1))
                .is_none()
        );
        browser.key_at(key(KeyCode::Char('b')), now);
        browser.close();
        assert!(
            browser
                .take_filter_request(now + Duration::from_secs(1))
                .is_none()
        );
        assert!(browser.filter_wait(now).is_none());
        browser.open("x".repeat(256), vec![], None);
        let full = browser.generation();
        browser.key_at(key(KeyCode::Char('y')), now);
        assert_eq!(browser.generation(), full);
        assert!(
            browser
                .take_filter_request(now + Duration::from_secs(1))
                .is_none()
        );
        assert!(browser.filter().chars().count() == 256);
    }
    #[test]
    fn full_action_queue_retries_one_latest_filter_and_discards_superseded_requests() {
        use super::super::{UiState, actions};
        let now = Instant::now();
        let mut ui = UiState::new(&crate::config::Config::default(), std::path::PathBuf::new());
        let initial = ui.thread_browser.open(String::new(), vec![], None);
        let (sender, _receiver) = actions::channel(1);
        sender.try_send(initial).unwrap();
        ui.thread_browser.key_at(key(KeyCode::Char('a')), now);
        let requests = ui.take_deferred_actions_at(now + FILTER_DELAY);
        assert_eq!(requests.len(), 1);
        for action in requests {
            ui.deferred_actions
                .push(sender.try_send(action).unwrap_err().into_inner());
        }
        for _ in 0..3 {
            let retry = ui.take_deferred_actions_at(now + FILTER_DELAY);
            assert_eq!(retry.len(), 1, "queue refusal retains one current snapshot");
            ui.deferred_actions.extend(retry);
        }
        ui.thread_browser
            .key_at(key(KeyCode::Char('b')), now + Duration::from_secs(1));
        assert!(
            ui.take_deferred_actions_at(now + Duration::from_secs(1))
                .is_empty(),
            "stale queued filter must be dropped before the new deadline"
        );
        assert!(
            matches!(&ui.take_deferred_actions_at(now + Duration::from_secs(2))[..],
            [UiAction::LoadThreads { filter, .. }] if filter == "ab")
        );
        assert!(
            ui.take_deferred_actions_at(now + Duration::from_secs(2))
                .is_empty()
        );
        ui.thread_browser
            .key_at(key(KeyCode::Char('c')), now + Duration::from_secs(3));
        let request = ui.take_deferred_actions_at(now + Duration::from_secs(4));
        ui.deferred_actions.extend(request);
        ui.thread_browser.close();
        assert!(
            ui.take_deferred_actions_at(now + Duration::from_secs(4))
                .is_empty()
        );
    }
}
