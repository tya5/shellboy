//! Branch panel (#237): the branch tree of the viewed conversation, drawn from
//! one `thread branches` read. Only divergence points and branch tips are
//! listed. Opening, forking and pruning reuse the existing operations.
//!
//! Glyphs come from the existing vocabulary only: `◉` heading, `├─ └─ │` tree
//! (with their ASCII fallbacks); states are words.
use super::i18n::local;
use super::popup;
use super::text_width::{UnicodeWidthStr, glyphs};
use crate::config::UiLanguage;
use crate::presentation::branch_marker::BranchState;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Frame, layout::Rect, text::Line};
use serde_json::Value;
use std::collections::BTreeMap;

/// Width from which time and size join the row (below it the detail area has them).
const WIDE: usize = 64;
/// Detail lines under the list when the popup is tall enough.
const DETAIL_LINES: u16 = 3;
/// Columns kept for a row's state word and excerpt when the tree prefix is trimmed.
const PREFIX_RESERVE: usize = 24;
/// Columns a request excerpt may take in a row.
const EXCERPT_MAX: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub thread_id: String,
    pub title: Option<String>,
    pub parent: Option<String>,
    pub state: String,
    pub running: bool,
    pub pruned: bool,
    pub point_ordinal: Option<i64>,
    pub point_request: Option<String>,
    pub point_node: Option<String>,
    pub tip_node: Option<String>,
    pub tip_at: Option<String>,
    pub tip_request: Option<String>,
    pub path_nodes: i64,
    pub after_point: Option<i64>,
}
impl Entry {
    fn parse(item: &Value) -> Option<Self> {
        let text = |v: &Value| v.as_str().map(str::to_owned);
        Some(Self {
            thread_id: text(&item["thread_id"])?,
            title: text(&item["title"]),
            parent: text(&item["parent_thread_id"]),
            state: text(&item["state"]).unwrap_or_default(),
            running: item["running"] == true,
            pruned: item["pruned_originals"] == true,
            point_ordinal: item["divergence"]["ordinal"].as_i64(),
            point_request: text(&item["divergence"]["request"]["text"]),
            point_node: text(&item["divergence"]["node_id"]),
            tip_node: text(&item["tip"]["node_id"]),
            tip_at: text(&item["tip"]["at"]),
            tip_request: text(&item["tip"]["last_request"]["text"]),
            path_nodes: item["size"]["path_nodes"].as_i64().unwrap_or(0),
            after_point: item["size"]["nodes_after_divergence"].as_i64(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowKind {
    Thread(usize),
    Point { request: Option<String> },
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    prefix: String,
    kind: RowKind,
}

enum Load {
    Loading,
    Ready,
    Failed(String),
}
pub(super) enum Outcome {
    None,
    Close,
    Load { cursor: String },
    Open(String),
    Fork { thread_id: String, node_id: String },
    Prune { thread_id: String, node_id: String },
}

pub(super) struct BranchPanel {
    language: UiLanguage,
    pub(super) request_id: u64,
    pub(super) viewed: String,
    load: Load,
    /// A next page is being read; the loaded rows stay visible.
    loading_more: bool,
    /// The last next-page read failed; shown in the detail lines.
    page_error: Option<String>,
    entries: Vec<Entry>,
    next_cursor: Option<String>,
    family_truncated: bool,
    ancestors_truncated: bool,
    rows: Vec<Row>,
    /// Index into `rows`; always a Thread row once any exist.
    selected: usize,
    paint: std::cell::Cell<Option<popup::PaintBounds>>,
}

impl BranchPanel {
    pub(super) fn new(language: UiLanguage, viewed: String, request_id: u64) -> Self {
        Self {
            language,
            request_id,
            viewed,
            load: Load::Loading,
            loading_more: false,
            page_error: None,
            entries: Vec::new(),
            next_cursor: None,
            family_truncated: false,
            ancestors_truncated: false,
            rows: Vec::new(),
            selected: 0,
            paint: Default::default(),
        }
    }
    pub(super) fn set_language(&mut self, language: UiLanguage) {
        self.language = language;
    }
    pub(super) fn paint_bounds(&self) -> Option<popup::PaintBounds> {
        self.paint.get()
    }
    pub(super) fn begin_page(&mut self, request_id: u64) {
        self.request_id = request_id;
        self.page_error = None;
        if self.entries.is_empty() {
            self.load = Load::Loading;
        } else {
            self.loading_more = true;
        }
    }
    /// Take one page (`append` continues the previous one).
    pub(super) fn accept(&mut self, value: &Value, append: bool) {
        if !append {
            self.entries.clear();
        }
        for item in value["items"].as_array().into_iter().flatten() {
            if let Some(entry) = Entry::parse(item) {
                self.entries.push(entry);
            }
        }
        self.next_cursor = value["next_cursor"].as_str().map(str::to_owned);
        self.family_truncated = value["family_truncated"] == true;
        self.ancestors_truncated = value["ancestors_truncated"] == true;
        self.load = Load::Ready;
        self.loading_more = false;
        self.page_error = None;
        let keep = self.selected_thread().map(str::to_owned);
        self.rows = build_rows(&self.entries);
        self.selected = keep
            .and_then(|id| self.row_of(&id))
            .or_else(|| self.row_of(&self.viewed.clone()))
            .or_else(|| self.first_thread_row())
            .unwrap_or(0);
    }
    pub(super) fn fail(&mut self, message: String) {
        self.loading_more = false;
        if self.entries.is_empty() {
            self.load = Load::Failed(message);
        } else {
            // Keep what was read; a retry is N again.
            self.page_error = Some(message);
        }
    }
    fn row_of(&self, thread: &str) -> Option<usize> {
        self.rows.iter().position(|row| match row.kind {
            RowKind::Thread(i) => self.entries[i].thread_id == thread,
            _ => false,
        })
    }
    fn first_thread_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row.kind, RowKind::Thread(_)))
    }
    fn selected_entry(&self) -> Option<&Entry> {
        match self.rows.get(self.selected)?.kind {
            RowKind::Thread(i) => self.entries.get(i),
            _ => None,
        }
    }
    fn selected_thread(&self) -> Option<&str> {
        self.selected_entry().map(|e| e.thread_id.as_str())
    }
    fn move_by(&mut self, delta: isize) {
        let threads: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, RowKind::Thread(_)))
            .map(|(i, _)| i)
            .collect();
        let Some(position) = threads.iter().position(|i| *i == self.selected) else {
            return;
        };
        self.selected = threads[popup::move_selection(position, delta, threads.len())];
    }
    pub(super) fn wheel(&mut self, delta: isize) -> bool {
        let before = self.selected;
        self.move_by(delta.signum());
        before != self.selected
    }
    pub(super) fn key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Up => {
                self.move_by(-1);
                Outcome::None
            }
            KeyCode::Down => {
                self.move_by(1);
                Outcome::None
            }
            KeyCode::PageUp => {
                self.move_by(-8);
                Outcome::None
            }
            KeyCode::PageDown => {
                self.move_by(8);
                Outcome::None
            }
            KeyCode::Home => {
                self.selected = self.first_thread_row().unwrap_or(0);
                Outcome::None
            }
            KeyCode::End => {
                self.move_by(isize::MAX / 2);
                Outcome::None
            }
            KeyCode::Enter => self
                .selected_thread()
                .map_or(Outcome::None, |id| Outcome::Open(id.to_owned())),
            KeyCode::Char(c) => match c.to_ascii_lowercase() {
                'f' => self
                    .selected_entry()
                    .and_then(|e| Some((e.thread_id.clone(), e.tip_node.clone()?)))
                    .map_or(Outcome::None, |(thread_id, node_id)| Outcome::Fork {
                        thread_id,
                        node_id,
                    }),
                'p' => self
                    .selected_entry()
                    .and_then(|e| Some((e.thread_id.clone(), e.tip_node.clone()?)))
                    .map_or(Outcome::None, |(thread_id, node_id)| Outcome::Prune {
                        thread_id,
                        node_id,
                    }),
                'n' => match (&self.load, &self.next_cursor) {
                    (Load::Ready, Some(cursor)) if !self.loading_more => Outcome::Load {
                        cursor: cursor.clone(),
                    },
                    _ => Outcome::None,
                },
                _ => Outcome::None,
            },
            _ => Outcome::None,
        }
    }

    fn title(&self) -> &'static str {
        local!(self.language, "分岐", "Branches")
    }
    fn footer(&self) -> String {
        let more = self.next_cursor.is_some();
        let text = match (self.language, more) {
            (UiLanguage::Ja, false) => "^v 選択 | Enter 開く | F 分岐 | P 手放す | Esc 閉じる",
            (UiLanguage::Ja, true) => {
                "^v 選択 | Enter 開く | F 分岐 | P 手放す | N 続き | Esc 閉じる"
            }
            (UiLanguage::En, false) => "^v Select | Enter Open | F Fork | P Prune | Esc Close",
            (UiLanguage::En, true) => {
                "^v Select | Enter Open | F Fork | P Prune | N More | Esc Close"
            }
        };
        glyphs::controls(text)
    }
    fn context(&self) -> String {
        let sep = glyphs::separator();
        let points = self
            .entries
            .iter()
            .filter_map(|e| e.point_node.as_deref().map(|n| (e.parent.as_deref(), n)))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let branches = self.entries.len().saturating_sub(1);
        let viewed = self
            .entries
            .iter()
            .find(|e| e.thread_id == self.viewed)
            .map(label_of)
            .unwrap_or_default();
        match self.language {
            UiLanguage::Ja => format!(
                "{} {}{sep}{viewed}{sep}分岐点 {points}{sep}枝 {branches}",
                glyphs::HEADING,
                self.title()
            ),
            UiLanguage::En => format!(
                "{} {}{sep}{viewed}{sep}{points} points{sep}{branches} branches",
                glyphs::HEADING,
                self.title()
            ),
        }
    }
    fn state_word(&self, entry: &Entry) -> String {
        let en = self.language == UiLanguage::En;
        let mut word = BranchState::from_wire(&entry.state)
            .word(self.language)
            .to_owned();
        if entry.running {
            word.push_str(if en { " running" } else { " 実行中" });
        }
        if entry.pruned {
            // A word only: the summary mark means a summary, not released originals.
            word.push_str(if en { " pruned" } else { " 原文解放済" });
        }
        word
    }
    fn excerpt(&self, entry: &Entry) -> String {
        match entry.tip_request.as_deref().filter(|t| !t.is_empty()) {
            Some(text) => format!("\"{}\"", clip(text, EXCERPT_MAX)),
            None => clip(&label_of(entry), EXCERPT_MAX),
        }
    }
    fn time(&self, entry: &Entry) -> String {
        entry.tip_at.as_deref().map(local_time).unwrap_or_default()
    }
    fn size(&self, entry: &Entry) -> String {
        match self.language {
            UiLanguage::Ja => format!("{}件", entry.path_nodes),
            UiLanguage::En => format!("{} nodes", entry.path_nodes),
        }
    }
    fn row_text(&self, row: &Row, width: usize) -> String {
        // Keep room for the state word and an excerpt; outer levels give way first.
        let prefix = trim_prefix(&row.prefix, width.saturating_sub(PREFIX_RESERVE).max(6));
        match &row.kind {
            RowKind::Point { request } => {
                let text = request.as_deref().filter(|t| !t.is_empty()).map_or_else(
                    || local!(self.language, "(依頼なし)", "(no request)").to_owned(),
                    |t| format!("\"{}\"", clip(t, EXCERPT_MAX)),
                );
                clip_all(&format!("{prefix}{text}"), width)
            }
            RowKind::Thread(i) => {
                let entry = &self.entries[*i];
                let left = format!("{prefix}{} {}", self.state_word(entry), self.excerpt(entry));
                let sep = glyphs::separator();
                let right = format!("{}{sep}{}", self.time(entry), self.size(entry));
                if width >= WIDE && left.as_str().width() + right.as_str().width() + 2 <= width {
                    let pad = width - left.as_str().width() - right.as_str().width();
                    format!("{left}{}{right}", " ".repeat(pad))
                } else {
                    clip_all(&left, width)
                }
            }
        }
    }
    fn detail(&self, width: usize) -> Vec<String> {
        let Some(entry) = self.selected_entry() else {
            return Vec::new();
        };
        let sep = glyphs::separator();
        let en = self.language == UiLanguage::En;
        let mut lines = vec![format!(
            "{}{sep}{}",
            label_of(entry),
            entry.thread_id.chars().take(8).collect::<String>()
        )];
        let after = entry.after_point.map_or_else(String::new, |n| {
            if en {
                format!("{sep}{n} after the point")
            } else {
                format!("{sep}分岐点から{n}件")
            }
        });
        lines.push(format!(
            "{}{sep}{}{sep}{}{after}",
            self.time(entry),
            self.size(entry),
            self.state_word(entry)
        ));
        if let Some(ordinal) = entry.point_ordinal {
            lines.push(if en {
                format!("diverges after node {}", ordinal + 1)
            } else {
                format!("{}件目の後で分岐", ordinal + 1)
            });
        }
        if let Some(error) = &self.page_error {
            lines.push(match self.language {
                UiLanguage::Ja => format!("続きを読めませんでした: {error}"),
                UiLanguage::En => format!("Could not read the next page: {error}"),
            });
        } else if self.loading_more {
            lines.push(
                local!(
                    self.language,
                    "続きを読み込んでいます",
                    "Loading the next page"
                )
                .to_owned(),
            );
        }
        if self.family_truncated || self.ancestors_truncated {
            lines.push(
                local!(
                    self.language,
                    "上限のため一部のみ表示しています",
                    "Showing part of the tree (limit reached)"
                )
                .to_owned(),
            );
        }
        lines.into_iter().map(|l| clip_all(&l, width)).collect()
    }

    pub(super) fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        let context = self.context();
        let footer = self.footer();
        let body = popup::panel_recorded(frame, area, self.title(), &context, &footer, &self.paint);
        if body.width == 0 || body.height == 0 {
            return;
        }
        let width = usize::from(body.width.saturating_sub(3));
        match &self.load {
            Load::Loading => {
                popup::reader(
                    frame,
                    body,
                    &[Line::from(local!(
                        self.language,
                        "分岐を読み込んでいます",
                        "Loading branches"
                    ))],
                    0,
                );
                return;
            }
            Load::Failed(message) => {
                popup::reader(frame, body, &[Line::from(message.clone())], 0);
                return;
            }
            Load::Ready => {}
        }
        if self.rows.is_empty() {
            popup::reader(
                frame,
                body,
                &[Line::from(local!(
                    self.language,
                    "分岐はありません",
                    "No branches"
                ))],
                0,
            );
            return;
        }
        let detail = self.detail(usize::from(body.width));
        let detail_rows = if body.height > DETAIL_LINES + 4 {
            (detail.len() as u16).min(DETAIL_LINES + 1)
        } else {
            0
        };
        let list_area = Rect::new(body.x, body.y, body.width, body.height - detail_rows);
        let items = self
            .rows
            .iter()
            .map(|row| popup::Item::new(vec![Line::from(self.row_text(row, width))]))
            .collect();
        popup::list(frame, list_area, items, self.selected);
        if detail_rows > 0 {
            let lines = detail
                .into_iter()
                .take(usize::from(detail_rows))
                .map(Line::from)
                .collect::<Vec<_>>();
            popup::reader(
                frame,
                Rect::new(body.x, body.bottom() - detail_rows, body.width, detail_rows),
                &lines,
                0,
            );
        }
    }
}

fn label_of(entry: &Entry) -> String {
    entry
        .title
        .as_deref()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| entry.thread_id.chars().take(8).collect())
}
fn local_time(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| raw.chars().take(16).collect())
}
fn clip(text: &str, columns: usize) -> String {
    super::region::fit(text, columns)
}
fn clip_all(text: &str, columns: usize) -> String {
    super::region::fit(text, columns)
}
/// Keep the tree prefix within `budget` columns by dropping the outermost levels.
fn trim_prefix(prefix: &str, budget: usize) -> String {
    if prefix.width() <= budget {
        return prefix.to_owned();
    }
    let chars: Vec<char> = prefix.chars().collect();
    let mut start = 0;
    while start < chars.len() && chars[start..].iter().collect::<String>().width() + 1 > budget {
        start += 3;
    }
    format!(
        "{}{}",
        glyphs::ellipsis(),
        chars[start.min(chars.len())..].iter().collect::<String>()
    )
}

/// The tree: each Thread, then its divergence points (ordered along its path),
/// each holding the branches that diverged there.
fn build_rows(entries: &[Entry]) -> Vec<Row> {
    let index: BTreeMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.thread_id.as_str(), i))
        .collect();
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut roots = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        match entry.parent.as_deref().and_then(|p| index.get(p)) {
            Some(parent) if *parent != i => children.entry(*parent).or_default().push(i),
            _ => roots.push(i),
        }
    }
    let mut rows = Vec::new();
    let mut visited = vec![false; entries.len()];
    for root in roots {
        emit(
            entries,
            &children,
            root,
            String::new(),
            String::new(),
            &mut visited,
            &mut rows,
        );
    }
    rows
}
fn emit(
    entries: &[Entry],
    children: &BTreeMap<usize, Vec<usize>>,
    at: usize,
    line_prefix: String,
    child_prefix: String,
    visited: &mut [bool],
    rows: &mut Vec<Row>,
) {
    if std::mem::replace(&mut visited[at], true) {
        return;
    }
    rows.push(Row {
        prefix: line_prefix,
        kind: RowKind::Thread(at),
    });
    let Some(kids) = children.get(&at) else {
        return;
    };
    // Group by the node they diverged at, points in path order; unknown last.
    type Point<'a> = Option<(i64, &'a str)>;
    let mut groups: Vec<(Point<'_>, Vec<usize>)> = Vec::new();
    let mut sorted = kids.clone();
    sorted.sort_by_key(|k| {
        (
            entries[*k].point_ordinal.unwrap_or(i64::MAX),
            entries[*k].thread_id.clone(),
        )
    });
    for kid in sorted {
        let key = entries[kid]
            .point_ordinal
            .zip(entries[kid].point_node.as_deref());
        match groups.last_mut() {
            Some((last, members)) if *last == key => members.push(kid),
            _ => groups.push((key, vec![kid])),
        }
    }
    let count = groups.len();
    for (g, (_, members)) in groups.into_iter().enumerate() {
        let last_group = g + 1 == count;
        rows.push(Row {
            prefix: format!("{}{} ", child_prefix, glyphs::branch(last_group)),
            kind: RowKind::Point {
                request: entries[members[0]].point_request.clone(),
            },
        });
        let inner = format!(
            "{}{}",
            child_prefix,
            if last_group {
                "   ".to_owned()
            } else {
                format!("{}  ", glyphs::vertical())
            }
        );
        let total = members.len();
        for (m, member) in members.into_iter().enumerate() {
            let last = m + 1 == total;
            emit(
                entries,
                children,
                member,
                format!("{inner}{} ", glyphs::branch(last)),
                format!(
                    "{inner}{}",
                    if last {
                        "   ".to_owned()
                    } else {
                        format!("{}  ", glyphs::vertical())
                    }
                ),
                visited,
                rows,
            );
        }
    }
}

#[cfg(test)]
mod tests;

impl super::UiState {
    /// Open the panel and ask for the first page (a read on the DB worker).
    pub(super) fn open_branches(&mut self, thread: Option<String>) -> Vec<super::UiAction> {
        let Some(thread_id) = thread.or_else(|| self.view().thread_id.clone()) else {
            return self.command_error(local!(
                self.config.ui.language,
                "/branches は保存済みの会話で使えます",
                "/branches needs a saved conversation"
            ));
        };
        self.next_branches_request += 1;
        let request_id = self.next_branches_request;
        self.branches = Some(BranchPanel::new(
            self.config.ui.language,
            thread_id.clone(),
            request_id,
        ));
        vec![super::UiAction::LoadBranches {
            thread_id,
            request_id,
            cursor: None,
        }]
    }
    pub(super) fn branches_loaded(
        &mut self,
        request_id: u64,
        append: bool,
        result: Result<Value, String>,
    ) {
        let Some(panel) = self.branches.as_mut() else {
            return;
        };
        if panel.request_id != request_id {
            return;
        }
        match result {
            Ok(value) => panel.accept(&value, append),
            Err(message) => panel.fail(message),
        }
    }
    pub(super) fn branches_key(&mut self, key: KeyEvent) -> Vec<super::UiAction> {
        let Some(panel) = self.branches.as_mut() else {
            return vec![];
        };
        panel.set_language(self.config.ui.language);
        match panel.key(key) {
            Outcome::None => vec![],
            Outcome::Close => {
                self.branches = None;
                vec![]
            }
            Outcome::Load { cursor } => {
                self.next_branches_request += 1;
                let request_id = self.next_branches_request;
                panel.begin_page(request_id);
                vec![super::UiAction::LoadBranches {
                    thread_id: panel.viewed.clone(),
                    request_id,
                    cursor: Some(cursor),
                }]
            }
            Outcome::Open(thread_id) => {
                let viewed = panel.viewed.clone();
                self.branches = None;
                if thread_id == viewed && self.view().thread_id.as_deref() == Some(&viewed) {
                    vec![]
                } else {
                    self.select_thread(thread_id)
                }
            }
            Outcome::Fork { thread_id, node_id } => {
                self.branches = None;
                vec![super::UiAction::Command(
                    crate::protocol::Command::ThreadFork(crate::protocol::ThreadForkArgs {
                        agents: None,
                        thread_id,
                        node_id,
                        cwd: None,
                    }),
                )]
            }
            Outcome::Prune { thread_id, node_id } => {
                // Prune is irreversible: the existing form opens with the dry run
                // preselected and this branch's tip as the boundary.
                self.branches = None;
                let mut context = self.operation_context(true);
                context.thread = thread_id;
                context.node = node_id;
                let mut operations = super::operations::Operations::new(context);
                operations.set_language(self.config.ui.language);
                operations.select(super::operations::OpKind::ThreadPrune);
                self.invalidate_management();
                self.operations = Some(operations);
                vec![]
            }
        }
    }
}
