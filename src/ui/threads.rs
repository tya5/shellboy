//! Thread-list presentation; previews use the same Markdown parser as history.
use super::theme;
use super::thread_patch::BrowserPatch;
use super::{UiAction, popup};
use crate::protocol::ThreadSummary;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use pulldown_cmark::{Event, Options, Parser, TagEnd};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use std::{
    cell::{OnceCell, RefCell},
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
    patch_error: Option<String>,
    notice: Option<String>,
    notice_seen: std::cell::Cell<bool>,
    preferred: Option<String>,
    preferred_screen: Option<usize>,
    reconfirming_row: Option<String>,
    generation: u64,
    page_read_sequence: u64,
    complete_page_sequence: Option<u64>,
    row_versions: BTreeMap<String, (u64, bool)>,
    patch_overlay: BTreeSet<String>,
    limited_titles: BTreeSet<String>,
    stale_excerpts: BTreeSet<String>,
    excerpt_error: Option<(String, String)>,
    observed_scope: Option<(u64, String, Option<String>, bool)>,
    filter_deadline: Option<Instant>,
    tree: OnceCell<TreeCache>,
    tree_revision: u64,
    table_state: RefCell<TableState>,
    table_body_rows: std::cell::Cell<usize>,
    #[cfg(test)]
    tree_rebuilds: std::cell::Cell<usize>,
}

pub(super) enum ThreadOutcome {
    None,
    Load(Box<UiAction>),
    Open(String),
    Delete(String),
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

struct TreeCache {
    rows: Vec<TreeRow>,
    visible: Vec<usize>,
    positions: Vec<Option<usize>>,
    ids: BTreeMap<String, usize>,
    revision: u64,
}

impl ThreadBrowser {
    fn invalidate_tree(&mut self) {
        self.tree_revision = self.tree_revision.wrapping_add(1);
        self.tree.take();
    }
    fn selected_position(&self) -> Option<usize> {
        self.tree().positions.get(self.selected).copied().flatten()
    }
    fn selected_screen_row(&self) -> usize {
        self.selected_position()
            .unwrap_or(0)
            .saturating_sub(self.table_state.borrow().offset())
    }
    fn restore_screen_row(&self, screen_row: usize) {
        let position = self.selected_position();
        let mut state = self.table_state.borrow_mut();
        state.select(position);
        *state.offset_mut() = position.unwrap_or(0).saturating_sub(screen_row);
    }
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
        self.generation = self.generation.wrapping_add(1);
        self.reset_patch_evidence();
        self.loading = true;
        self.error = None;
        self.notice = None;
    }
    fn begin(&mut self, cursor: Option<String>) -> UiAction {
        self.filter_deadline = None;
        if cursor.is_none() {
            self.invalidate(None);
        } else {
            self.loading = true;
            self.error = None;
            self.notice = None;
        }
        self.request(cursor)
    }
    pub(super) fn take_filter_request(&mut self, now: Instant) -> Option<UiAction> {
        if !self.visible
            || self
                .observed_scope
                .as_ref()
                .is_some_and(|(_, _, _, visible)| !*visible)
            || self.filter_deadline.is_none_or(|deadline| now < deadline)
        {
            return None;
        }
        self.filter_deadline = None;
        Some(self.request(self.next_cursor.clone()))
    }
    pub(super) fn filter_wait(&self, now: Instant) -> Option<Duration> {
        if !self.visible
            || self
                .observed_scope
                .as_ref()
                .is_some_and(|(_, _, _, visible)| !*visible)
        {
            return None;
        }
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
        self.preferred_screen = None;
        self.reconfirming_row = None;
        self.items = drafts;
        self.selected = 0;
        self.invalidate_tree();
        *self.table_state.get_mut() = TableState::default();
        self.table_body_rows.set(0);
        // Reopening must retain a deletion outcome that arrived while closed.
        // Filtering/paging still clears it through normal invalidation.
        let notice = self.notice.take().filter(|_| !self.notice_seen.get());
        let action = self.begin(None);
        self.notice = notice;
        action
    }
    pub fn failed(&mut self, generation: u64, error: String) {
        if generation == self.generation {
            self.loading = false;
            self.filter_deadline = None;
            self.error = Some(error);
        }
    }
    pub fn notice(&mut self, message: String) {
        self.notice = Some(message);
        self.notice_seen.set(false);
    }
    /// Apply confirmed deletion facts without accepting an older list response
    /// that could put a removed row back. Preserve filtering and pagination.
    pub fn remove_deleted(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let deleted: BTreeSet<_> = ids.iter().map(String::as_str).collect();
        let position = self.selected_position().unwrap_or(0);
        let screen_row = self.selected_screen_row();
        let surviving_selection = self
            .items
            .get(self.selected)
            .filter(|thread| !deleted.contains(thread.thread_id.as_str()))
            .map(|thread| thread.thread_id.clone());
        let refresh_filter = self.visible && (self.loading || self.filter_deadline.is_some());
        self.generation = self.generation.wrapping_add(1);
        self.reset_patch_evidence();
        self.filter_deadline = refresh_filter.then(Instant::now);
        self.loading = refresh_filter;
        self.error = None;
        self.notice = None;
        if self
            .preferred
            .as_ref()
            .is_some_and(|id| deleted.contains(id.as_str()))
        {
            self.clear_preferred();
        }
        if self
            .reconfirming_row
            .as_ref()
            .is_some_and(|id| deleted.contains(id.as_str()))
        {
            self.reconfirming_row = None;
        }
        self.items
            .retain(|thread| !deleted.contains(thread.thread_id.as_str()));
        self.collapsed.retain(|id| !deleted.contains(id.as_str()));
        self.display_times
            .retain(|id, _| !deleted.contains(id.as_str()));
        self.invalidate_tree();
        let visible = self.visible_indices();
        self.selected = surviving_selection
            .and_then(|id| self.tree().ids.get(&id).copied())
            .filter(|index| visible.contains(index))
            .or_else(|| {
                visible
                    .get(position.min(visible.len().saturating_sub(1)))
                    .copied()
            })
            .unwrap_or(0);
        self.restore_screen_row(screen_row);
    }
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        generation: u64,
        read_sequence: u64,
        requested_cursor: Option<String>,
        items: Vec<ThreadSummary>,
        display_times: Vec<ThreadDisplayTime>,
        next_cursor: Option<String>,
        drafts: Vec<ThreadSummary>,
        active: Option<String>,
    ) -> bool {
        if generation != self.generation
            || read_sequence < self.page_read_sequence
            || (requested_cursor.is_some() && requested_cursor != self.next_cursor)
        {
            return false;
        }
        let preferred = self.preferred.clone();
        let selected = preferred.clone().or_else(|| {
            self.items
                .get(self.selected)
                .map(|t| t.thread_id.clone())
                .or(active)
        });
        // An initial preferred selection has no previously displayed row to
        // anchor. Starting it at the top would hide earlier rows unnecessarily.
        let screen_row = self.preferred_screen.or_else(|| {
            self.items
                .get(self.selected)
                .filter(|thread| selected.as_deref() == Some(thread.thread_id.as_str()))
                .and_then(|_| self.selected_position())
                .map(|_| self.selected_screen_row())
        });
        let matched_preferred = preferred.as_ref().is_some_and(|id| {
            items
                .iter()
                .chain(drafts.iter())
                .any(|row| &row.thread_id == id)
                && self
                    .row_versions
                    .get(id)
                    .is_none_or(|(sequence, present)| *present && *sequence <= read_sequence)
        });
        let exhaustive = requested_cursor.is_none() && next_cursor.is_none();
        if exhaustive {
            if preferred.as_ref().is_some_and(|id| {
                !items
                    .iter()
                    .chain(drafts.iter())
                    .any(|row| &row.thread_id == id)
                    && !self
                        .row_versions
                        .get(id)
                        .is_some_and(|(sequence, present)| *present && *sequence > read_sequence)
            }) {
                self.clear_preferred();
            }
            if self.reconfirming_row.as_ref().is_some_and(|id| {
                !items
                    .iter()
                    .chain(drafts.iter())
                    .any(|row| &row.thread_id == id)
                    && !self
                        .row_versions
                        .get(id)
                        .is_some_and(|(sequence, present)| *present && *sequence > read_sequence)
            }) {
                self.reconfirming_row = None;
            }
            let returned = items
                .iter()
                .map(|row| row.thread_id.as_str())
                .collect::<BTreeSet<_>>();
            for (id, (sequence, present)) in &mut self.row_versions {
                if *sequence <= read_sequence && !returned.contains(id.as_str()) {
                    *sequence = read_sequence;
                    *present = false;
                }
            }
            self.complete_page_sequence = Some(
                self.complete_page_sequence
                    .map_or(read_sequence, |old| old.max(read_sequence)),
            );
            self.patch_overlay.retain(|id| {
                self.row_versions
                    .get(id)
                    .is_some_and(|(_, present)| *present)
            });
        }
        if requested_cursor.is_none() {
            self.items.retain(|row| {
                row.thread_id.starts_with("draft:")
                    || self.patch_overlay.contains(&row.thread_id)
                    || self.preferred.as_deref() == Some(row.thread_id.as_str())
                    || self.reconfirming_row.as_deref() == Some(row.thread_id.as_str())
            });
        }
        let mut positions: BTreeMap<_, _> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, t)| (t.thread_id.clone(), i))
            .collect();
        for item in items.into_iter().chain(drafts) {
            if !item.thread_id.starts_with("draft:") {
                if self
                    .row_versions
                    .get(&item.thread_id)
                    .is_some_and(|(sequence, present)| {
                        *sequence > read_sequence || (!present && *sequence >= read_sequence)
                    })
                {
                    continue;
                }
                if !self.row_versions.contains_key(&item.thread_id)
                    && self.row_versions.len() >= 2000
                {
                    self.resync(
                        generation,
                        "一覧の更新履歴が保持上限に達したため再同期します".into(),
                    );
                    return false;
                }
                if self.reconfirming_row.as_deref() == Some(item.thread_id.as_str()) {
                    self.reconfirming_row = None;
                }
                self.row_versions
                    .insert(item.thread_id.clone(), (read_sequence, true));
                self.stale_excerpts.remove(&item.thread_id);
                self.limited_titles.remove(&item.thread_id);
                self.patch_overlay.remove(&item.thread_id);
            }
            if let Some(index) = positions.get(&item.thread_id) {
                self.items[*index] = item;
            } else if self.items.len() < 2000 {
                positions.insert(item.thread_id.clone(), self.items.len());
                self.items.push(item);
            }
        }
        self.sort_items();
        self.page_read_sequence = read_sequence;
        self.filter_deadline = None;
        self.invalidate_tree();
        self.update_times(display_times);
        self.loading = false;
        self.error = None;
        self.next_cursor = next_cursor;
        if self.items.len() >= 2000 && self.next_cursor.is_some() {
            self.error = Some("表示上限2000件。文字入力で絞り込んでください".into());
        }
        self.selected = selected
            .and_then(|id| self.tree().ids.get(&id).copied())
            .unwrap_or_else(|| self.visible_indices().first().copied().unwrap_or(0));
        self.reveal(self.selected);
        if let Some(screen_row) = screen_row {
            self.restore_screen_row(screen_row);
        } else {
            let position = self.selected_position();
            let state = self.table_state.get_mut();
            state.select(position);
            *state.offset_mut() = 0;
        }
        if matched_preferred {
            self.clear_preferred();
        }
        true
    }
    fn clear_preferred(&mut self) {
        self.preferred = None;
        self.preferred_screen = None;
    }
    fn remember_selection(&mut self) {
        if self.preferred.is_none() {
            let selected = self
                .items
                .get(self.selected)
                .map(|row| row.thread_id.clone());
            let screen = self.selected_position().map(|_| self.selected_screen_row());
            if let Some(old) = self.reconfirming_row.take()
                && selected.as_deref() != Some(old.as_str())
            {
                self.items.retain(|row| row.thread_id != old);
                self.invalidate_tree();
                self.selected = selected
                    .as_ref()
                    .and_then(|id| self.items.iter().position(|row| &row.thread_id == id))
                    .unwrap_or(0);
            }
            self.preferred = selected;
            self.preferred_screen = screen;
            self.reconfirming_row = self
                .preferred
                .clone()
                .filter(|id| !id.starts_with("draft:"));
        }
    }
    fn reset_patch_evidence(&mut self) {
        self.page_read_sequence = 0;
        self.complete_page_sequence = None;
        self.row_versions.clear();
        self.patch_overlay.clear();
        self.limited_titles.clear();
        self.stale_excerpts.clear();
        self.excerpt_error = None;
        self.patch_error = None;
    }
    fn sort_items(&mut self) {
        self.items.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.thread_id.cmp(&a.thread_id))
        });
    }
    pub fn observe(&mut self, visible: bool) -> Option<UiAction> {
        let visible = visible && self.visible;
        if !visible && self.visible && self.loading && self.filter_deadline.is_none() {
            self.filter_deadline = Some(Instant::now());
        }
        let selected = visible
            .then(|| {
                self.preferred.clone().or_else(|| {
                    self.items
                        .get(self.selected)
                        .map(|row| row.thread_id.clone())
                })
            })
            .flatten()
            .filter(|id| !id.starts_with("draft:"));
        let scope = (
            self.generation,
            self.filter.clone(),
            selected.clone(),
            visible,
        );
        if self.observed_scope.as_ref() == Some(&scope)
            || (self.observed_scope.is_none() && !visible)
        {
            return None;
        }
        self.observed_scope = Some(scope);
        Some(UiAction::ObserveBrowser {
            generation: self.generation,
            filter: self.filter.clone(),
            selected,
            visible,
        })
    }
    pub fn resync(&mut self, generation: u64, reason: String) -> bool {
        if generation != self.generation {
            return false;
        }
        self.remember_selection();
        self.invalidate(None);
        self.notice = Some(one_line(&reason.chars().take(512).collect::<String>()));
        self.notice_seen.set(false);
        self.filter_deadline = Some(Instant::now());
        true
    }
    pub fn patch_failed(&mut self, generation: u64, error: String) {
        if generation != self.generation {
            return;
        }
        self.patch_error = Some(format!(
            "一覧の更新に失敗: {}",
            one_line(&error.chars().take(512).collect::<String>())
        ));
        self.stale_excerpts.extend(
            self.items
                .iter()
                .filter(|row| !row.thread_id.starts_with("draft:"))
                .map(|row| row.thread_id.clone()),
        );
    }
    pub fn patch(&mut self, patch: BrowserPatch) -> bool {
        let BrowserPatch {
            generation,
            read_sequence,
            rows,
            display_times,
            missing_ids,
            limited_titles,
            excerpt,
        } = patch;
        if generation != self.generation
            || self
                .complete_page_sequence
                .is_some_and(|floor| read_sequence < floor)
        {
            return false;
        }
        if rows.len() > 128 || missing_ids.len() > 128 || limited_titles.len() > 128 {
            return self.resync(
                generation,
                "一覧の更新範囲が上限を超えたため再同期します".into(),
            );
        }
        let added = rows
            .iter()
            .map(|row| &row.thread_id)
            .chain(missing_ids.iter())
            .filter(|id| !self.row_versions.contains_key(*id))
            .collect::<BTreeSet<_>>()
            .len();
        if self.row_versions.len() + added > 2000 {
            return self.resync(
                generation,
                "一覧の更新履歴が保持上限に達したため再同期します".into(),
            );
        }
        let selected = self.preferred.clone().or_else(|| {
            self.items
                .get(self.selected)
                .map(|row| row.thread_id.clone())
        });
        let position = self.selected_position().unwrap_or(0);
        let screen = self
            .preferred_screen
            .unwrap_or_else(|| self.selected_screen_row());
        let mut matched_preferred = false;
        let limited = limited_titles.into_iter().collect::<BTreeSet<_>>();
        let mut changed = false;
        let mut accepted = false;
        for row in rows {
            let id = row.thread_id.clone();
            if id.starts_with("draft:")
                || self
                    .row_versions
                    .get(&id)
                    .is_some_and(|(sequence, _)| *sequence > read_sequence)
            {
                continue;
            }
            accepted = true;
            let known_match = if limited.contains(&id) {
                crate::control::db::thread_summary::matches_filter(
                    &row.thread_id,
                    "",
                    "",
                    &self.filter,
                ) || crate::control::db::thread_summary::matches_filter(
                    "",
                    "",
                    &row.cwd,
                    &self.filter,
                )
            } else {
                crate::control::db::thread_summary::matches_filter(
                    &row.thread_id,
                    &row.title,
                    &row.cwd,
                    &self.filter,
                )
            };
            let uncertain = !known_match && limited.contains(&id);
            if !known_match && !uncertain {
                changed |= self.remove_patch_row(&id, read_sequence);
                continue;
            }
            if self.reconfirming_row.as_deref() == Some(id.as_str()) {
                self.reconfirming_row = None;
                changed = true;
            }
            matched_preferred |= known_match && self.preferred.as_deref() == Some(id.as_str());
            self.row_versions.insert(id.clone(), (read_sequence, true));
            self.patch_overlay.insert(id.clone());
            if uncertain {
                self.limited_titles.insert(id.clone());
            } else {
                self.limited_titles.remove(&id);
            }
            let mut row = row;
            if let Some(existing) = self.items.iter_mut().find(|old| old.thread_id == id) {
                row.excerpt = existing.excerpt.clone();
                changed |= *existing != row;
                *existing = row;
            } else {
                if self.items.len() >= 2000 {
                    return self
                        .resync(generation, "一覧の表示上限に達したため再同期します".into());
                }
                row.excerpt.clear();
                self.items.push(row);
                changed = true;
            }
            changed |= self.stale_excerpts.insert(id);
        }
        for id in missing_ids {
            if id.starts_with("draft:")
                || self
                    .row_versions
                    .get(&id)
                    .is_some_and(|(sequence, _)| *sequence > read_sequence)
            {
                continue;
            }
            accepted = true;
            changed |= self.remove_patch_row(&id, read_sequence);
        }
        self.sort_items();
        self.invalidate_tree();
        let visible = self.visible_indices();
        self.selected = selected
            .as_ref()
            .and_then(|id| self.tree().ids.get(id).copied())
            .filter(|index| visible.contains(index))
            .or_else(|| {
                visible
                    .get(position.min(visible.len().saturating_sub(1)))
                    .copied()
            })
            .unwrap_or(0);
        if matched_preferred {
            changed = true;
            if let Some(index) = selected
                .as_ref()
                .and_then(|id| self.tree().ids.get(id).copied())
            {
                self.selected = index;
                self.reveal(index);
            }
            self.clear_preferred();
        }
        self.restore_screen_row(screen);
        self.update_times(display_times);
        if let Some((id, result)) = excerpt
            && self
                .items
                .get(self.selected)
                .is_some_and(|row| row.thread_id == id)
            && self
                .row_versions
                .get(&id)
                .is_none_or(|(sequence, present)| *present && read_sequence >= *sequence)
        {
            accepted = true;
            match result {
                Ok(text) => {
                    if let Some(row) = self.items.iter_mut().find(|row| row.thread_id == id) {
                        changed |= row.excerpt != text;
                        row.excerpt = text;
                    }
                    changed |= self.stale_excerpts.remove(&id);
                    self.row_versions.insert(id.clone(), (read_sequence, true));
                    self.excerpt_error = None;
                }
                Err(error) => {
                    self.stale_excerpts.insert(id.clone());
                    self.excerpt_error =
                        Some((id, one_line(&error.chars().take(512).collect::<String>())));
                    changed = true;
                }
            }
        }
        if accepted {
            changed |= self.patch_error.take().is_some();
        }
        changed
    }
    fn remove_patch_row(&mut self, id: &str, sequence: u64) -> bool {
        let intent_changed =
            self.preferred.as_deref() == Some(id) || self.reconfirming_row.as_deref() == Some(id);
        if self.preferred.as_deref() == Some(id) {
            self.clear_preferred();
        }
        if self.reconfirming_row.as_deref() == Some(id) {
            self.reconfirming_row = None;
        }
        self.row_versions.insert(id.into(), (sequence, false));
        self.patch_overlay.remove(id);
        self.limited_titles.remove(id);
        self.stale_excerpts.remove(id);
        self.display_times.remove(id);
        let before = self.items.len();
        self.items.retain(|row| row.thread_id != id);
        intent_changed || before != self.items.len()
    }
    fn excerpt_text(&self, thread: &ThreadSummary) -> String {
        if (self.preferred.as_deref() == Some(thread.thread_id.as_str())
            || self.reconfirming_row.as_deref() == Some(thread.thread_id.as_str()))
            && !thread.thread_id.starts_with("draft:")
        {
            return format!("前回の表示（選択先を再確認中）\n{}", thread.excerpt);
        }
        if self.stale_excerpts.contains(&thread.thread_id) {
            let reason = self
                .excerpt_error
                .as_ref()
                .filter(|(id, _)| id == &thread.thread_id)
                .map(|(_, error)| format!("取得失敗: {error}"))
                .unwrap_or_else(|| "更新を取得中".into());
            if thread.excerpt.is_empty() {
                return format!("抜粋は未取得 · {reason}");
            }
            return format!("以前の抜粋（未更新） · {reason}\n{}", thread.excerpt);
        }
        thread.excerpt.clone()
    }
    pub fn key(&mut self, key: KeyEvent) -> ThreadOutcome {
        self.key_at(key, Instant::now())
    }
    fn key_at(&mut self, key: KeyEvent, now: Instant) -> ThreadOutcome {
        if key.kind == KeyEventKind::Release {
            return ThreadOutcome::None;
        }
        if key.code == KeyCode::Delete
            || (matches!(key.code, KeyCode::Char('d' | 'D'))
                && key.modifiers == KeyModifiers::CONTROL)
        {
            self.clear_preferred();
            return self.items.get(self.selected).map_or(ThreadOutcome::None, |thread| {
                if thread.thread_id.starts_with("draft:") {
                    ThreadOutcome::Info("未送信の下書きはこの一覧から削除できません。切り替えて本文や添付を編集してください".into())
                } else {
                    ThreadOutcome::Delete(thread.thread_id.clone())
                }
            });
        }
        if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Left
                | KeyCode::Right
        ) {
            self.clear_preferred();
        }
        let previous_filter_len = self.filter.len();
        let position = self.selected_position().unwrap_or(0);
        match key.code {
            KeyCode::Esc => self.close(),
            KeyCode::F(1) => {
                self.clear_preferred();
                if let Some(thread) = self.items.get(self.selected) {
                    return ThreadOutcome::Info(format!(
                        "会話ID: {}\nタイトル: {}\n作業フォルダー: {}\n親の会話: {}\n状態: {}\nNative state: {}\nLatest Run stop reason: {}\n更新日時: {}\n\n{}\n\nDelete / Ctrl+D: 削除影響と確認を開く（まだ削除しません）。macOSのDeleteはfn+deleteで入力できます。確認画面のD/Spaceで子孫も含めるか変更し、Yで実行、N/Escで一覧へ戻ります。文字入力/Backspaceは絞り込みです。",
                        thread.thread_id,
                        if self.limited_titles.contains(&thread.thread_id) {
                            format!("{}（検索条件との一致は未確認）", thread.title)
                        } else {
                            thread.title.clone()
                        },
                        thread.cwd,
                        thread.parent_id.as_deref().unwrap_or("root"),
                        activity_label(thread),
                        thread.state,
                        thread.latest_run_stop_reason.as_deref().unwrap_or("none"),
                        thread.updated_at,
                        self.excerpt_text(thread)
                    ));
                }
            }
            KeyCode::Up => {
                if let Some(index) = self
                    .visible_indices()
                    .get(position.saturating_sub(1))
                    .copied()
                {
                    self.selected = index;
                }
            }
            KeyCode::Down => {
                if let Some(index) = self
                    .visible_indices()
                    .get((position + 1).min(self.visible_indices().len().saturating_sub(1)))
                    .copied()
                {
                    self.selected = index;
                }
            }
            KeyCode::Home => {
                if let Some(index) = self.visible_indices().first().copied() {
                    self.selected = index;
                }
            }
            KeyCode::End => {
                if let Some(index) = self.visible_indices().last().copied() {
                    self.selected = index;
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
                self.clear_preferred();
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
                self.remember_selection();
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
            self.clear_preferred();
            self.reconfirming_row = None;
            self.invalidate_tree();
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
    fn tree(&self) -> &TreeCache {
        self.tree.get_or_init(|| {
            #[cfg(test)]
            self.tree_rebuilds.set(self.tree_rebuilds.get() + 1);
            self.build_tree()
        })
    }

    fn build_tree(&self) -> TreeCache {
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
        let visible: Vec<_> = rows.iter().map(|row| row.index).collect();
        let mut positions = vec![None; threads.len()];
        for (position, &index) in visible.iter().enumerate() {
            positions[index] = Some(position);
        }
        TreeCache {
            rows,
            visible,
            positions,
            ids: ids
                .into_iter()
                .map(|(id, index)| (id.to_owned(), index))
                .collect(),
            revision: self.tree_revision,
        }
    }

    fn rows(&self) -> &[TreeRow] {
        let tree = self.tree();
        debug_assert_eq!(tree.revision, self.tree_revision);
        &tree.rows
    }

    fn visible_indices(&self) -> &[usize] {
        &self.tree().visible
    }

    fn parent_index(&self, index: usize) -> Option<usize> {
        let threads = &self.items;
        let parent = threads.get(index)?.parent_id.as_deref()?;
        self.tree()
            .ids
            .get(parent)
            .copied()
            .filter(|parent| *parent != index)
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
        let changed = self.collapsed.insert(thread.thread_id.clone());
        if changed {
            self.invalidate_tree();
        }
        changed
    }

    fn expand(&mut self, index: usize) -> bool {
        let threads = &self.items;
        let changed = threads
            .get(index)
            .is_some_and(|thread| self.collapsed.remove(&thread.thread_id));
        if changed {
            self.invalidate_tree();
        }
        changed
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

    pub fn draw(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        active_thread: Option<&str>,
        notice_visible: bool,
    ) {
        let threads = &self.items;
        let selected_index = self.selected;
        let filter = &self.filter;
        let next_cursor = self.next_cursor.is_some();
        let loading = self.loading;
        let error = self.error.as_deref().or(self.patch_error.as_deref());
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
                "Esc戻る ↑↓選択 ←→展開 Enter切替 Delete/Ctrl+D削除 F1会話情報 Ctrl+R再取得"
                    .to_owned()
            } else if footer_width >= 45 {
                "Esc戻る Enter切替 Delete/Ctrl+D削除\n↑↓選択 ←→開閉 F1会話情報 Ctrl+R再取得"
                    .to_owned()
            } else if footer_width >= 30 {
                "Esc戻る Enter切替 Ctrl+D削除\n↑↓選択 ←→開閉 F1情報".to_owned()
            } else if footer_width >= 17 {
                "Esc戻る Enter切替\nCtrl+D削除 F1情報".to_owned()
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
        let mut context = match error {
            Some(error) => format!("取得失敗 {} · 検索 › {}", one_line(error), one_line(filter)),
            None => match &self.notice {
                Some(notice) => format!("{} · 検索 › {}", one_line(notice), one_line(filter)),
                None => format!("検索 › {}", one_line(filter)),
            },
        };
        if self
            .preferred
            .as_ref()
            .is_some_and(|id| !id.starts_with("draft:"))
        {
            context.push_str(" · 選択先を再確認中");
        } else if self.reconfirming_row.is_some() {
            context.push_str(" · 前回の行を再確認中");
        }
        let inner = popup::panel(frame, area, &title, &context, &footer_text);
        // Tiny popups omit their context row. Use the shared geometry rather
        // than consuming a notice which was never rendered on screen.
        if notice_visible
            && error.is_none()
            && self.notice.is_some()
            && inner.y > popup::body_area(area, "", &footer_text, None).y
        {
            self.notice_seen.set(true);
        }
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
            (0, 8, 0)
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
        let selected_visible = self.selected_position();
        let table_rows = rows.iter().map(|row| {
            let thread = &threads[row.index];
            let selected = row.index == selected_index;
            let current = active_thread == Some(thread.thread_id.as_str());
            let label = self.label(row, thread, selected, current, title_width);
            let status_style = activity_style(thread.activity);
            let mut cells = vec![Cell::from(label).style(status_style)];
            if cwd_width > 0 {
                cells.push(Cell::from(tail_path(&thread.cwd, cwd_width)));
            }
            if state_width > 0 {
                cells.push(
                    Cell::from(fit(&one_line(activity_column_label(thread)), state_width))
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
        let mut selection = self.table_state.borrow_mut();
        selection.select(selected_visible);
        // A taller viewport can fit formerly hidden predecessors. Ordinary
        // mutations retain the selected screen row even with a blank bottom.
        let body_rows = usize::from(table_area.height.saturating_sub(1));
        if body_rows > self.table_body_rows.replace(body_rows) {
            let max_offset = rows.len().saturating_sub(body_rows);
            let offset = selection.offset().min(max_offset);
            *selection.offset_mut() = offset;
        }
        frame.render_stateful_widget(table, table_area, &mut *selection);
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
        if interrupted_by_app_exit(thread) {
            label.push_str("[exit] ");
        }
        if (self.preferred.as_deref() == Some(thread.thread_id.as_str())
            || self.reconfirming_row.as_deref() == Some(thread.thread_id.as_str()))
            && !thread.thread_id.starts_with("draft:")
        {
            label.push_str("[再確認中] ");
        }
        if self.limited_titles.contains(&thread.thread_id) {
            label.push_str("[検索未確認] ");
        }
        let title = one_line(&thread.title);
        label.push_str(if title.width() == 0 {
            "名称未設定"
        } else {
            &title
        });
        if let Some(summary) = thread.revisit.as_ref()
            && let Some(revisit) = crate::presentation::revisit_label(summary)
        {
            // Keep the continuation visible even when the title is long.
            let revisit = if width < 45 {
                format!(
                    "Revisit {}{}",
                    summary.consumed_count,
                    if summary.pending { "+" } else { "" }
                )
            } else {
                revisit
            };
            let reserve = revisit.width().saturating_add(3);
            if width > reserve + 4 && (!interrupted_by_app_exit(thread) || width >= 32) {
                label = fit(&label, width - reserve);
                label.push_str(" · ");
                label.push_str(&revisit);
            }
        }
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
                    "{}ID {} · 親 {} · {}{}",
                    thread
                        .revisit
                        .as_ref()
                        .and_then(crate::presentation::revisit_label)
                        .map(|label| format!("{label} · "))
                        .unwrap_or_default(),
                    one_line(&thread.thread_id),
                    thread
                        .parent_id
                        .as_deref()
                        .map(one_line)
                        .unwrap_or_else(|| "なし".into()),
                    one_line(activity_label(thread)),
                    if interrupted_by_app_exit(thread) {
                        " · Interrupted by app exit"
                    } else {
                        ""
                    }
                ),
                width,
            )),
            Line::from(Span::styled(
                plain_preview(&self.excerpt_text(thread), width),
                Style::default().fg(theme::TEXT),
            )),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn interrupted_by_app_exit(thread: &ThreadSummary) -> bool {
    matches!(
        thread.latest_run_stop_reason.as_deref(),
        Some("previous_instance_interrupted" | "app_shutdown")
    )
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

fn activity_column_label(thread: &ThreadSummary) -> &str {
    if thread.thread_id.starts_with("draft:") {
        &thread.state
    } else {
        thread.activity.column_label()
    }
}
fn activity_label(thread: &ThreadSummary) -> &str {
    if thread.thread_id.starts_with("draft:") {
        &thread.state
    } else {
        thread.activity.label()
    }
}
fn activity_style(phase: crate::activity::ActivityPhase) -> Style {
    use crate::activity::ActivityPhase::*;
    state_style(match phase {
        Preparing | AwaitingModel | Tools | Running | Continuing => "running",
        Retrying | Waiting | Background => "waiting",
        Stopping => "stopping",
        Recovery | Failed => "failed",
        Completed => "completed",
        _ => "idle",
    })
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
    fn deletion_keys_only_request_confirmation_and_preserve_filter_editing() {
        let mut browser = ThreadBrowser {
            visible: true,
            items: vec![thread("first", None), thread("chosen", None)],
            selected: 1,
            filter: "query".into(),
            ..Default::default()
        };
        let generation = browser.generation();
        for key in [
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        ] {
            assert!(matches!(browser.key(key), ThreadOutcome::Delete(id) if id == "chosen"));
            assert_eq!(browser.items.len(), 2, "confirmation is not deletion");
            assert_eq!(browser.filter(), "query");
            assert_eq!(browser.generation(), generation);
        }
        let release =
            KeyEvent::new_with_kind(KeyCode::Delete, KeyModifiers::NONE, KeyEventKind::Release);
        assert!(matches!(browser.key(release), ThreadOutcome::None));
        browser.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(browser.filter(), "queryd");
        browser.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(browser.filter(), "query");
        browser.key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        browser.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(browser.filter(), "queryD ");
        browser.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        browser.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(browser.filter(), "query");
        browser.items[1].thread_id = "draft:local".into();
        assert!(
            matches!(browser.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            ThreadOutcome::Info(message) if message.contains("未送信の下書き"))
        );
        browser.items.clear();
        browser.invalidate_tree();
        assert!(matches!(
            browser.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            ThreadOutcome::None
        ));
    }

    #[test]
    fn confirmed_deletion_selects_visible_neighbor_and_fences_older_pages() {
        let mut browser = ThreadBrowser {
            visible: true,
            items: vec![
                thread("parent", None),
                thread("child", Some("parent")),
                thread("chosen", None),
                thread("next", None),
            ],
            selected: 2,
            filter: "fixture".into(),
            next_cursor: Some("page-two".into()),
            ..Default::default()
        };
        browser.collapsed.insert("parent".into());
        let old_generation = browser.generation();
        browser.remove_deleted(&["chosen".into()]);
        assert_eq!(browser.items[browser.selected].thread_id, "next");
        assert_eq!(browser.filter(), "fixture");
        assert_eq!(browser.next_cursor(), Some("page-two"));
        assert!(browser.collapsed.contains("parent"));
        assert!(!browser.apply(
            old_generation,
            0,
            None,
            vec![thread("chosen", None)],
            vec![],
            None,
            vec![],
            None
        ));
        assert!(!browser.items.iter().any(|row| row.thread_id == "chosen"));
        browser.remove_deleted(&["next".into()]);
        assert_eq!(browser.items[browser.selected].thread_id, "parent");
        browser.remove_deleted(&["parent".into()]);
        assert_eq!(browser.items[browser.selected].thread_id, "child");
        assert!(!browser.collapsed.contains("parent"));
        browser.remove_deleted(&["child".into()]);
        assert!(browser.items.is_empty());
        assert_eq!(browser.selected, 0);
    }

    #[test]
    fn deletion_keeps_surviving_selection_and_reschedules_only_pending_visible_reads() {
        let mut browser = ThreadBrowser {
            visible: true,
            loading: true,
            items: vec![thread("first", None), thread("chosen", None)],
            selected: 1,
            filter: "latest".into(),
            ..Default::default()
        };
        browser.remove_deleted(&["first".into()]);
        assert_eq!(browser.items[browser.selected].thread_id, "chosen");
        let generation = browser.generation();
        assert!(
            matches!(browser.take_filter_request(Instant::now() + Duration::from_secs(1)),
            Some(UiAction::LoadThreads { generation: g, cursor: None, filter })
            if g == generation && filter == "latest")
        );
        assert!(
            browser
                .take_filter_request(Instant::now() + Duration::from_secs(1))
                .is_none()
        );
        browser.close();
        browser.remove_deleted(&["chosen".into()]);
        assert!(browser.filter_wait(Instant::now()).is_none());
        assert!(!browser.loading);
    }

    #[test]
    fn tiny_browser_does_not_consume_a_notice_whose_header_is_hidden() {
        let mut browser = ThreadBrowser::default();
        browser.notice("unseen deletion warning".into());
        browser.open(String::new(), vec![], None);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(8, 5)).unwrap();
        terminal
            .draw(|frame| browser.draw(frame, frame.area(), None, true))
            .unwrap();
        assert!(!browser.notice_seen.get());
        browser.close();
        browser.open(String::new(), vec![], None);
        assert_eq!(browser.notice.as_deref(), Some("unseen deletion warning"));
    }

    #[test]
    fn deletion_notice_and_selected_information_keep_the_next_keys_visible() {
        let mut browser = ThreadBrowser {
            visible: true,
            items: vec![thread("chosen", None)],
            ..Default::default()
        };
        browser.notice("削除できません: 作業開始後に状態が変わりました".into());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| browser.draw(frame, frame.area(), None, true))
            .unwrap();
        let shown = text(terminal.backend().buffer());
        assert!(shown.contains("削除できません"), "{shown}");
        assert!(!shown.contains("取得失敗"), "{shown}");
        assert!(shown.contains("Delete/Ctrl+D"), "{shown}");
        let ThreadOutcome::Info(info) =
            browser.key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))
        else {
            panic!("selected item information expected");
        };
        for key in [
            "Delete / Ctrl+D",
            "fn+delete",
            "D/Space",
            "N/Esc",
            "Backspace",
        ] {
            assert!(info.contains(key), "{info}");
        }
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
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert!(text(terminal.backend().buffer()).contains("12-31 23:59"));
        row.updated_at = "UNKNOWN".into();
        assert_eq!(tree.short_time(&row, 11), "UNKNOWN");
        tree.items = vec![row.clone()];
        tree.invalidate_tree();
        tree.update_times(vec![]);
        assert!(tree.display_times.is_empty(), "stale labels are evicted");
        row.thread_id = "other".into();
        assert_eq!(tree.short_time(&row, 11), "UNKNOWN");
    }

    fn thread(id: &str, parent: Option<&str>) -> ThreadSummary {
        ThreadSummary {
            last_answer: None,
            last_input: None,
            activity: crate::activity::ActivityPhase::Idle,
            latest_run_stop_reason: None,
            revisit: None,
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
    fn every_activity_fits_the_actual_state_column_without_using_selected_details() {
        use crate::activity::ActivityPhase::*;
        let phases = [
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
        ];
        for phase in phases {
            assert!(phase.column_label().width() <= 8, "{phase:?}");
            let mut item = thread("row44", None);
            item.title = "Fixture".into();
            item.activity = phase;
            // Native state intentionally differs: the column must use activity.
            item.state = "completed".into();
            for width in [
                28, 29, 35, 43, 44, 47, 48, 51, 52, 64, 67, 68, 71, 72, 90, 93, 94, 97, 98, 120,
            ] {
                for height in [10, 20] {
                    let buffer = capture(&[item.clone()], 0, width, height, None);
                    let (x, y) = (0..buffer.area.height)
                        .find_map(|y| {
                            (0..buffer.area.width.saturating_sub(2)).find_map(|x| {
                                (buffer[(x, y)].symbol() == "状"
                                    && buffer[(x + 2, y)].symbol() == "態")
                                    .then_some((x, y))
                            })
                        })
                        .unwrap_or_else(|| {
                            panic!(
                                "state header missing {phase:?} {width}x{height}\n{}",
                                text(&buffer)
                            )
                        });
                    // Read only eight cells in the first data row below the
                    // header, never the selected detail/footer text.
                    let mut shown = String::new();
                    let mut col = x;
                    while col < x + 8 {
                        let symbol = buffer[(col, y + 1)].symbol();
                        shown.push_str(symbol);
                        col += symbol.width().max(1) as u16;
                    }
                    assert_eq!(
                        shown.trim_end(),
                        phase.column_label(),
                        "{phase:?} {width}x{height}"
                    );
                    assert!(!shown.contains('…'));
                }
            }
        }
        assert_eq!(Background.column_label(), "背景処理");
        assert_eq!(Idle.column_label(), "未実行");
        assert_eq!(Background.label(), "回答済み背景処理中");
        assert_eq!(Idle.label(), "入力できます");
    }

    #[test]
    fn activity_column_does_not_confuse_completed_answer_with_completed_work() {
        let mut item = thread("background", None);
        item.state = "completed".into();
        item.activity = crate::activity::ActivityPhase::Background;
        assert_eq!(activity_label(&item), item.activity.label());
        assert_ne!(activity_style(item.activity), state_style(&item.state));
        item.activity = crate::activity::ActivityPhase::Continuing;
        assert_eq!(activity_label(&item), "実行継続中");
    }

    #[test]
    fn app_exit_marker_uses_only_latest_reason_and_refresh_clears_it() {
        use ratatui::{Terminal, backend::TestBackend};
        for reason in [
            Some("previous_instance_interrupted"),
            Some("app_shutdown"),
            Some("explicit_stop"),
            None,
        ] {
            let mut item = thread("t", None);
            item.title = "Work".into();
            item.state = if reason.is_some() {
                "stopped"
            } else {
                "completed"
            }
            .into();
            item.latest_run_stop_reason = reason.map(str::to_owned);
            let marked = matches!(
                reason,
                Some("previous_instance_interrupted" | "app_shutdown")
            );
            let mut browser = ThreadBrowser::default();
            browser.open(String::new(), vec![], Some("t".into()));
            assert!(browser.apply(
                browser.generation(),
                0,
                None,
                vec![item.clone()],
                vec![],
                None,
                vec![],
                None
            ));
            for width in [120, 24] {
                let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
                terminal
                    .draw(|frame| browser.draw(frame, frame.area(), None, true))
                    .unwrap();
                let shown = text(terminal.backend().buffer());
                assert_eq!(
                    shown.contains("[exit]"),
                    marked,
                    "{reason:?}, width={width}"
                );
                if width == 120 {
                    assert_eq!(shown.contains("Interrupted by app exit"), marked);
                    assert!(
                        shown.contains(activity_label(&item)),
                        "ordinary state must remain visible: {shown}"
                    );
                }
            }
            item.latest_run_stop_reason = None;
            item.state = "running".into();
            assert!(browser.apply(
                browser.generation(),
                0,
                None,
                vec![item],
                vec![],
                None,
                vec![],
                None
            ));
            let row = &browser.rows()[0];
            assert!(
                !browser
                    .label(row, &browser.items[0], true, false, 80)
                    .contains("[exit]")
            );
        }
    }

    #[test]
    fn revisit_row_reserves_space_and_clears_on_new_run() {
        let mut item = thread("t", None);
        item.title = "Long title ".repeat(20);
        item.revisit = Some(crate::protocol::RevisitSummary {
            pending: true,
            consumed_count: 5,
            last_fired_at: None,
        });
        let browser = ThreadBrowser {
            items: vec![item.clone()],
            ..Default::default()
        };
        let row = &browser.rows()[0];
        assert!(
            browser
                .label(row, &item, true, false, 90)
                .contains("Revisit 5 · next reserved")
        );
        assert!(
            browser
                .label(row, &item, true, false, 24)
                .contains("Revisit 5+")
        );
        item.revisit = None;
        assert!(
            !browser
                .label(row, &item, true, false, 90)
                .contains("Revisit")
        );
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
                item.activity = match *state {
                    "live" => crate::activity::ActivityPhase::Continuing,
                    "draft" | "future_state" => crate::activity::ActivityPhase::Idle,
                    "recovery_wait" => crate::activity::ActivityPhase::Recovery,
                    other => serde_json::from_value(serde_json::json!(other)).unwrap(),
                };
                item
            })
            .collect::<Vec<_>>();
        let mut tree = ThreadBrowser::default();
        tree.open(String::new(), vec![], Some("s7".into()));
        let generation = tree.generation();
        assert!(tree.apply(
            generation,
            0,
            None,
            items.clone(),
            vec![],
            None,
            vec![],
            None
        ));
        assert_eq!(
            tree.table_state.borrow().offset(),
            0,
            "first preferred selection has no prior viewport anchor"
        );
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
                .draw(|frame| tree.draw(frame, frame.area(), None, true))
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
        let refreshed_target = refreshed
            .iter_mut()
            .find(|row| row.thread_id == "s7")
            .unwrap();
        refreshed_target.state = "recovery_wait".into();
        refreshed_target.activity = crate::activity::ActivityPhase::Recovery;
        assert!(tree.apply(generation, 0, None, refreshed, vec![], None, vec![], None));
        assert_eq!(tree.items()[tree.selected()].thread_id, "s7");
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert_eq!(
            find_title(terminal.backend().buffer(), "Q07").fg,
            theme::ATTENTION
        );
        let mut inactive = tree.items().to_vec();
        let inactive_target = inactive
            .iter_mut()
            .find(|row| row.thread_id == "s7")
            .unwrap();
        inactive_target.state = "idle".into();
        inactive_target.activity = crate::activity::ActivityPhase::Idle;
        assert!(tree.apply(generation, 0, None, inactive, vec![], None, vec![], None));
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
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
    fn tree_cache_survives_paints_navigation_and_clock_labels_until_structure_changes() {
        let mut tree = ThreadBrowser {
            items: vec![
                thread("parent", None),
                thread("child", Some("parent")),
                thread("other", None),
            ],
            ..Default::default()
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        for _ in 0..5 {
            terminal
                .draw(|frame| tree.draw(frame, frame.area(), None, true))
                .unwrap();
            tree.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            tree.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            tree.update_times(vec![]);
        }
        assert_eq!(
            tree.tree_rebuilds.get(),
            1,
            "paint, focus and clocks reuse the tree"
        );
        assert!(tree.collapse(0));
        assert_eq!(tree.visible_indices(), [0, 2]);
        assert_eq!(tree.tree_rebuilds.get(), 2);
        assert!(!tree.collapse(0));
        tree.rows();
        assert_eq!(
            tree.tree_rebuilds.get(),
            2,
            "a repeated collapse changes no facts"
        );
        let generation = tree.generation();
        assert!(tree.apply(
            generation,
            0,
            None,
            vec![thread("replacement", None)],
            vec![],
            None,
            vec![],
            None
        ));
        assert_eq!(tree.visible_indices(), [0]);
        assert_eq!(tree.items()[0].thread_id, "replacement");
        assert_eq!(tree.tree_rebuilds.get(), 3);
        assert!(!tree.apply(
            generation.wrapping_add(1),
            0,
            None,
            vec![],
            vec![],
            None,
            vec![],
            None
        ));
        tree.rows();
        assert_eq!(
            tree.tree_rebuilds.get(),
            3,
            "a rejected response changes no cache"
        );
    }

    #[test]
    fn refreshed_tree_and_removal_keep_selected_identity_and_table_screen_row() {
        let items = (0..60)
            .map(|index| thread(&format!("t{index:02}"), None))
            .collect::<Vec<_>>();
        let mut tree = ThreadBrowser {
            items: items.clone(),
            ..Default::default()
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        tree.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        for _ in 0..5 {
            tree.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        let selected = tree.items()[tree.selected()].thread_id.clone();
        let screen_row = tree.selected_screen_row();
        assert!(tree.table_state.borrow().offset() > 0);
        let mut reordered = items;
        reordered.rotate_left(10);
        assert!(tree.apply(
            tree.generation(),
            0,
            None,
            reordered,
            vec![],
            None,
            vec![],
            None
        ));
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert_eq!(tree.items()[tree.selected()].thread_id, selected);
        assert_eq!(tree.selected_screen_row(), screen_row);
        tree.remove_deleted(&["t10".into(), "t11".into()]);
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert_eq!(tree.items()[tree.selected()].thread_id, selected);
        assert_eq!(tree.selected_screen_row(), screen_row);
        let position = tree.selected_position().unwrap();
        let next = tree.items()[tree.visible_indices()[position + 1]]
            .thread_id
            .clone();
        tree.remove_deleted(&[selected]);
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert_eq!(tree.items()[tree.selected()].thread_id, next);
        assert_eq!(tree.selected_screen_row(), screen_row);
    }

    #[test]
    fn growing_the_browser_reveals_rows_that_now_fit_without_losing_selection() {
        let mut tree = ThreadBrowser {
            items: (0..12)
                .map(|index| thread(&format!("t{index:02}"), None))
                .collect(),
            ..Default::default()
        };
        tree.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let mut small =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 15)).unwrap();
        small
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert!(tree.table_state.borrow().offset() > 0);
        let rebuilds = tree.tree_rebuilds.get();
        let mut large =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
        large
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert_eq!(tree.table_state.borrow().offset(), 0);
        assert_eq!(tree.items()[tree.selected()].thread_id, "t11");
        assert_eq!(
            tree.tree_rebuilds.get(),
            rebuilds,
            "resizing is not a tree mutation"
        );
        let shown = text(large.backend().buffer());
        for index in 0..12 {
            assert!(
                shown.contains(&format!("t{index:02}")),
                "all rows fit after resize"
            );
        }
    }

    #[test]
    fn deleting_rows_below_selection_keeps_its_screen_row_with_a_blank_bottom() {
        let mut tree = ThreadBrowser {
            items: (0..40)
                .map(|index| thread(&format!("t{index:02}"), None))
                .collect(),
            ..Default::default()
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        tree.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        for _ in 0..3 {
            tree.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert_eq!(tree.items()[tree.selected()].thread_id, "t36");
        let screen_row = tree.selected_screen_row();
        let offset = tree.table_state.borrow().offset();
        assert!(offset > 0);
        tree.remove_deleted(&["t37".into(), "t38".into(), "t39".into()]);
        terminal
            .draw(|frame| tree.draw(frame, frame.area(), None, true))
            .unwrap();
        assert_eq!(tree.items()[tree.selected()].thread_id, "t36");
        assert_eq!(tree.selected_screen_row(), screen_row);
        assert_eq!(tree.table_state.borrow().offset(), offset);
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
        tree.invalidate_tree();
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
                .draw(frame, frame.area(), Some("t03"), true);
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
            last_answer: None,
            last_input: None,
            activity: crate::activity::ActivityPhase::Idle,
            latest_run_stop_reason: None,
            revisit: None,
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
            tree.invalidate_tree();
            let row = &tree.rows()[0];
            assert!(
                tree.label(row, &thread, false, false, 50)
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
            !browser.apply(original, 0, None, vec![], vec![], None, vec![], None),
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
        assert_eq!(
            requests
                .iter()
                .filter(|action| matches!(action,UiAction::LoadThreads {filter,..} if filter=="a"))
                .count(),
            1
        );
        assert_eq!(requests.iter().filter(|action|matches!(action,UiAction::ObserveBrowser {filter,visible:true,..} if filter=="a")).count(),1);
        for action in requests {
            ui.deferred_actions
                .push(sender.try_send(action).unwrap_err().into_inner());
        }
        for _ in 0..3 {
            let retry = ui.take_deferred_actions_at(now + FILTER_DELAY);
            assert_eq!(
                retry
                    .iter()
                    .filter(
                        |action| matches!(action,UiAction::LoadThreads {filter,..} if filter=="a")
                    )
                    .count(),
                1,
                "queue refusal retains one latest page request"
            );
            assert_eq!(retry.iter().filter(|action|matches!(action,UiAction::ObserveBrowser {filter,visible:true,..} if filter=="a")).count(),1,"queue refusal retains one latest visible scope");
            ui.deferred_actions.extend(retry);
        }
        ui.thread_browser
            .key_at(key(KeyCode::Char('b')), now + Duration::from_secs(1));
        let changed_scope = ui.take_deferred_actions_at(now + Duration::from_secs(1));
        assert!(
            !changed_scope
                .iter()
                .any(|action| matches!(action, UiAction::LoadThreads { .. })),
            "superseded queued page must be dropped before deadline"
        );
        assert!(
            matches!(changed_scope.as_slice(),[UiAction::ObserveBrowser {filter,visible:true,..}] if filter=="ab")
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
        let closed = ui.take_deferred_actions_at(now + Duration::from_secs(4));
        assert!(
            !closed
                .iter()
                .any(|action| matches!(action, UiAction::LoadThreads { .. }))
        );
        assert!(matches!(
            closed.as_slice(),
            [UiAction::ObserveBrowser { visible: false, .. }]
        ));
    }
}

#[cfg(test)]
mod patch_tests {
    use super::*;
    fn row(id: &str, title: &str, at: &str) -> ThreadSummary {
        ThreadSummary {
            last_answer: None,
            last_input: None,
            thread_id: id.into(),
            title: title.into(),
            cwd: "/synthetic".into(),
            parent_id: None,
            state: "idle".into(),
            activity: crate::activity::ActivityPhase::Idle,
            latest_run_stop_reason: None,
            revisit: None,
            updated_at: at.into(),
            excerpt: format!("saved excerpt {id}"),
        }
    }
    fn patch(
        browser: &ThreadBrowser,
        sequence: u64,
        rows: Vec<ThreadSummary>,
        missing: &[&str],
    ) -> BrowserPatch {
        BrowserPatch {
            generation: browser.generation,
            read_sequence: sequence,
            rows,
            display_times: vec![],
            missing_ids: missing.iter().map(|id| (*id).into()).collect(),
            limited_titles: vec![],
            excerpt: None,
        }
    }
    fn initial(items: Vec<ThreadSummary>) -> ThreadBrowser {
        let mut browser = ThreadBrowser::default();
        browser.open(String::new(), vec![], None);
        assert!(browser.apply(
            browser.generation,
            10,
            None,
            items,
            vec![],
            Some("page-two".into()),
            vec![],
            None
        ));
        browser
    }
    #[test]
    fn additions_reorders_and_deletion_preserve_selected_screen_anchor_and_local_drafts() {
        let items = (0..40)
            .map(|n| row(&format!("t{n:02}"), "topic", "2026-10-01"))
            .collect();
        let mut browser = initial(items);
        browser
            .items
            .push(row("draft:local", "unsent draft", "2026-01-01"));
        browser.selected = browser
            .items
            .iter()
            .position(|row| row.thread_id == "t15")
            .unwrap();
        let position = browser.selected_position().unwrap();
        *browser.table_state.borrow_mut().offset_mut() = position - 4;
        let screen = browser.selected_screen_row();
        assert!(browser.patch(patch(
            &browser,
            20,
            vec![row("new", "new topic", "2026-11-01")],
            &["t01"]
        )));
        assert_eq!(browser.items[browser.selected].thread_id, "t15");
        assert_eq!(browser.selected_screen_row(), screen);
        assert_eq!(browser.items[0].thread_id, "new");
        assert!(
            browser
                .items
                .iter()
                .any(|row| row.thread_id == "draft:local")
        );
        assert!(!browser.items.iter().any(|row| row.thread_id == "t01"));
        assert_eq!(
            browser
                .items
                .iter()
                .filter(|row| row.thread_id == "new")
                .count(),
            1
        );
    }
    #[test]
    fn older_pages_cannot_undo_a_patch_or_missing_row_and_paging_keeps_generation() {
        let mut browser = initial(vec![
            row("a", "old a", "2026-01-01"),
            row("b", "old b", "2026-01-01"),
        ]);
        assert!(browser.patch(patch(
            &browser,
            20,
            vec![row("a", "new a", "2026-02-01")],
            &["b"]
        )));
        let generation = browser.generation;
        let outcome = browser.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert!(
            matches!(outcome,ThreadOutcome::Load(action) if matches!(*action,UiAction::LoadThreads {generation:g,cursor:Some(ref c),..} if g==generation && c=="page-two"))
        );
        assert_eq!(browser.generation, generation);
        assert!(browser.apply(
            generation,
            15,
            Some("page-two".into()),
            vec![
                row("a", "stale a", "2026-01-01"),
                row("b", "stale b", "2026-01-01"),
                row("c", "older page c", "2025-01-01")
            ],
            vec![],
            None,
            vec![],
            None
        ));
        assert_eq!(
            browser
                .items
                .iter()
                .find(|row| row.thread_id == "a")
                .unwrap()
                .title,
            "new a"
        );
        assert!(!browser.items.iter().any(|row| row.thread_id == "b"));
        assert!(browser.items.iter().any(|row| row.thread_id == "c"));
        assert!(!browser.apply(
            generation,
            14,
            None,
            vec![row("b", "late first page", "2026-01-01")],
            vec![],
            None,
            vec![],
            None
        ));
        assert!(!browser.items.iter().any(|row| row.thread_id == "b"));
    }
    #[test]
    fn unknown_title_is_explicit_filter_uncertainty_and_known_nonmatches_are_removed() {
        let mut browser = initial(vec![row("old", "needle", "2026-01-01")]);
        browser.filter = "needle".into();
        let mut update = patch(
            &browser,
            20,
            vec![
                row("limited", "名称未設定（元本文の取得範囲外）", "2026-02-01"),
                row("old", "known nonmatch", "2026-01-01"),
                row(
                    "needle-id",
                    "名称未設定（元本文の取得範囲外）",
                    "2026-02-01",
                ),
            ],
            &[],
        );
        update.limited_titles = vec!["limited".into(), "needle-id".into()];
        assert!(browser.patch(update));
        assert!(!browser.items.iter().any(|row| row.thread_id == "old"));
        assert!(browser.limited_titles.contains("limited"));
        assert!(!browser.limited_titles.contains("needle-id"));
        let item = browser
            .items
            .iter()
            .find(|row| row.thread_id == "limited")
            .unwrap();
        let tree_row = browser
            .rows()
            .iter()
            .find(|row| browser.items[row.index].thread_id == "limited")
            .unwrap();
        assert!(
            browser
                .label(tree_row, item, false, false, 100)
                .contains("検索未確認")
        );
    }
    #[test]
    fn patches_preserve_collapse_and_selected_excerpt_distinguishes_stale_error_and_empty() {
        let parent = row("parent", "parent", "2026-01-01");
        let mut child = row("child", "child", "2026-01-01");
        child.parent_id = Some("parent".into());
        let mut browser = initial(vec![parent, child, row("chosen", "chosen", "2026-01-01")]);
        browser.collapsed.insert("parent".into());
        browser.invalidate_tree();
        browser.selected = browser
            .items
            .iter()
            .position(|row| row.thread_id == "chosen")
            .unwrap();
        let mut changed = row("chosen", "chosen updated", "2026-02-01");
        changed.excerpt.clear();
        assert!(browser.patch(patch(&browser, 20, vec![changed], &[])));
        assert!(browser.collapsed.contains("parent"));
        let chosen = browser
            .items
            .iter()
            .find(|row| row.thread_id == "chosen")
            .unwrap();
        assert_eq!(chosen.excerpt, "saved excerpt chosen");
        assert!(
            browser
                .excerpt_text(chosen)
                .contains("以前の抜粋（未更新）")
        );
        let mut failed = patch(&browser, 21, vec![], &[]);
        failed.excerpt = Some(("chosen".into(), Err("fixture failure".into())));
        browser.patch(failed);
        assert!(
            browser
                .excerpt_text(&browser.items[browser.selected])
                .contains("取得失敗: fixture failure")
        );
        let mut empty = patch(&browser, 22, vec![], &[]);
        empty.excerpt = Some(("chosen".into(), Ok(String::new())));
        browser.patch(empty);
        assert!(browser.items[browser.selected].excerpt.is_empty());
        assert!(!browser.stale_excerpts.contains("chosen"));
        let mut old = patch(&browser, 21, vec![], &[]);
        old.excerpt = Some(("chosen".into(), Ok("stale late text".into())));
        browser.patch(old);
        assert!(browser.items[browser.selected].excerpt.is_empty());
    }
    #[test]
    fn observation_changes_once_for_selection_cover_and_close_without_paint_polling() {
        let mut browser = initial(vec![row("a", "a", "now"), row("b", "b", "now")]);
        assert!(matches!(
            browser.observe(true),
            Some(UiAction::ObserveBrowser { visible: true, .. })
        ));
        assert!(browser.observe(true).is_none());
        browser.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            matches!(browser.observe(true),Some(UiAction::ObserveBrowser {selected:Some(id),..}) if id=="a")
        );
        assert!(browser.observe(true).is_none());
        assert!(matches!(
            browser.observe(false),
            Some(UiAction::ObserveBrowser {
                selected: None,
                visible: false,
                ..
            })
        ));
        assert!(browser.observe(false).is_none());
        assert!(matches!(
            browser.observe(true),
            Some(UiAction::ObserveBrowser { visible: true, .. })
        ));
        browser.close();
        assert!(matches!(
            browser.observe(false),
            Some(UiAction::ObserveBrowser { visible: false, .. })
        ));
    }
    #[test]
    fn sequence_metadata_overflow_schedules_one_resync_and_retires_old_generation() {
        let mut browser = initial(vec![row("chosen", "chosen", "now")]);
        browser.items.push(row("draft:local", "draft", "now"));
        browser.filter = "filter".into();
        browser.collapsed.insert("parent".into());
        for n in 0..1999 {
            browser
                .row_versions
                .insert(format!("gone-{n}"), (10, false));
        }
        let generation = browser.generation;
        let update = patch(&browser, 20, vec![], &["one-more"]);
        assert!(browser.patch(update.clone()));
        assert_eq!(browser.generation, generation + 1);
        assert_eq!(browser.row_versions.len(), 0);
        assert_eq!(browser.patch_overlay.len(), 0);
        assert_eq!(browser.filter, "filter");
        assert!(browser.collapsed.contains("parent"));
        assert!(
            browser
                .items
                .iter()
                .any(|row| row.thread_id == "draft:local")
        );
        assert!(!browser.patch(update));
        assert!(matches!(
            browser.take_filter_request(Instant::now() + Duration::from_secs(1)),
            Some(UiAction::LoadThreads { cursor: None, .. })
        ));
        assert!(
            browser
                .take_filter_request(Instant::now() + Duration::from_secs(2))
                .is_none()
        );
    }
    #[test]
    fn exhaustive_first_page_fences_unknown_old_patch_and_retires_old_absent_overlay() {
        let mut browser = initial(vec![row("kept", "kept", "2026-01-01")]);
        assert!(browser.patch(patch(
            &browser,
            15,
            vec![row("old-overlay", "old overlay", "2026-02-01")],
            &[]
        )));
        assert!(browser.patch(patch(
            &browser,
            30,
            vec![row("newer-overlay", "new overlay", "2026-03-01")],
            &[]
        )));
        let generation = browser.generation;
        assert!(browser.apply(
            generation,
            20,
            None,
            vec![row("kept", "fresh kept", "2026-01-01")],
            vec![],
            None,
            vec![],
            None
        ));
        assert!(
            !browser
                .items
                .iter()
                .any(|row| row.thread_id == "old-overlay")
        );
        assert!(
            browser
                .items
                .iter()
                .any(|row| row.thread_id == "newer-overlay")
        );
        browser.patch_failed(generation, "newer observer failure".into());
        let old = patch(
            &browser,
            10,
            vec![row("unknown", "deleted before page20", "2026-01-01")],
            &[],
        );
        assert!(!browser.patch(old));
        assert!(!browser.items.iter().any(|row| row.thread_id == "unknown"));
        assert!(
            browser.patch_error.is_some(),
            "stale reply must not clear newer consumer error"
        );
        assert!(!browser.patch(patch(&browser, 19, vec![], &["newer-overlay"])));
        assert!(
            browser
                .items
                .iter()
                .any(|row| row.thread_id == "newer-overlay")
        );
    }
    #[test]
    fn partial_pages_do_not_infer_absence_and_page_and_patch_errors_have_separate_owners() {
        let mut browser = initial(vec![row("kept", "kept", "2026-01-01")]);
        assert!(browser.apply(
            browser.generation,
            20,
            None,
            vec![row("kept", "kept", "2026-01-01")],
            vec![],
            Some("partial".into()),
            vec![],
            None
        ));
        assert!(browser.patch(patch(
            &browser,
            10,
            vec![row(
                "offpage",
                "older snapshot but not disproved",
                "2025-01-01"
            )],
            &[]
        )));
        let generation = browser.generation;
        browser.failed(generation, "current pagination failed".into());
        browser.patch_failed(generation, "patch failed".into());
        assert!(browser.patch(patch(
            &browser,
            30,
            vec![row("kept", "updated kept", "2026-02-01")],
            &[]
        )));
        assert!(browser.patch_error.is_none());
        assert_eq!(browser.error.as_deref(), Some("current pagination failed"));
        browser.patch_failed(generation, "new patch failed".into());
        assert!(!browser.patch(patch(
            &browser,
            29,
            vec![row("kept", "stale kept", "2026-01-01")],
            &[]
        )));
        assert!(browser.patch_error.is_some());
    }
    #[test]
    fn covered_pagination_retries_same_cursor_once_unless_success_was_already_accepted() {
        for completed in [false, true] {
            let mut browser = initial(vec![row("kept", "kept", "now")]);
            let generation = browser.generation;
            browser.observe(true);
            assert!(matches!(
                browser.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
                ThreadOutcome::Load(_)
            ));
            browser.observe(false);
            assert!(
                browser
                    .take_filter_request(Instant::now() + Duration::from_secs(1))
                    .is_none(),
                "covered scope admits no retry"
            );
            if completed {
                assert!(browser.apply(
                    generation,
                    20,
                    Some("page-two".into()),
                    vec![row("next", "next", "earlier")],
                    vec![],
                    None,
                    vec![],
                    None
                ));
            }
            browser.observe(true);
            let retry = browser.take_filter_request(Instant::now() + Duration::from_secs(1));
            if completed {
                assert!(retry.is_none());
            } else {
                assert!(
                    matches!(retry,Some(UiAction::LoadThreads {generation:g,cursor:Some(ref cursor),..}) if g==generation && cursor=="page-two")
                );
            }
            assert!(
                browser
                    .take_filter_request(Instant::now() + Duration::from_secs(2))
                    .is_none()
            );
        }
    }
    #[test]
    fn covered_expired_filter_does_not_busy_poll_but_resume_keeps_one_request() {
        let mut browser = initial(vec![row("visible", "visible", "now")]);
        browser.observe(true);
        let now = Instant::now();
        browser.key_at(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), now);
        browser.observe(false);
        let future = now + Duration::from_secs(1);
        assert!(
            browser.filter_wait(future).is_none(),
            "a covered popup must allow blocking terminal input"
        );
        assert!(browser.take_filter_request(future).is_none());
        browser.observe(true);
        assert_eq!(browser.filter_wait(future), Some(Duration::ZERO));
        assert!(browser.take_filter_request(future).is_some());
        assert!(browser.take_filter_request(future).is_none());
    }
    #[test]
    fn actual_ui_filter_covered_before_deadline_resumes_one_latest_page_request() {
        use super::super::UiState;
        use crossterm::event::Event;
        let mut ui = UiState::new(&crate::config::Config::default(), std::path::PathBuf::new());
        ui.thread_browser = initial(vec![row("chosen", "answer", "now")]);
        ui.take_deferred_actions();
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        )));
        ui.event(Event::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
        assert!(ui.help_visible);
        let future = Instant::now() + Duration::from_secs(1);
        let covered = ui.take_deferred_actions_at(future);
        assert!(
            !covered
                .iter()
                .any(|action| matches!(action, UiAction::LoadThreads { .. })),
            "deadline while covered must not start a hidden page"
        );
        assert!(
            covered
                .iter()
                .any(|action| matches!(action, UiAction::ObserveBrowser { visible: false, .. }))
        );
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        let resumed = ui.take_deferred_actions_at(future + Duration::from_secs(1));
        assert_eq!(
            resumed
                .iter()
                .filter(|action| matches!(action,UiAction::LoadThreads {filter,..} if filter=="a"))
                .count(),
            1
        );
        assert!(resumed.iter().any(|action|matches!(action,UiAction::ObserveBrowser {filter,visible:true,..} if filter=="a")));
        assert!(
            !ui.take_deferred_actions_at(future + Duration::from_secs(2))
                .iter()
                .any(|action| matches!(action, UiAction::LoadThreads { .. }))
        );
    }
    fn offpage() -> ThreadBrowser {
        let items = (0..80)
            .map(|n| row(&format!("t{n:03}"), "matching topic", "2026-10-01"))
            .collect();
        let mut browser = initial(items);
        browser.selected = browser
            .items
            .iter()
            .position(|row| row.thread_id == "t005")
            .unwrap();
        let position = browser.selected_position().unwrap();
        *browser.table_state.borrow_mut().offset_mut() = position - 5;
        browser
    }
    fn newest_page() -> Vec<ThreadSummary> {
        (30..80)
            .map(|n| row(&format!("t{n:03}"), "matching topic", "2026-10-01"))
            .collect()
    }
    #[test]
    fn resync_and_ctrl_r_keep_one_counted_offpage_selected_row_until_exact_confirmation() {
        for refresh in [false, true] {
            let mut browser = offpage();
            let screen = browser.selected_screen_row();
            if refresh {
                browser.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
            } else {
                assert!(browser.resync(browser.generation, "bounded overflow".into()));
            }
            let generation = browser.generation;
            assert!(
                matches!(browser.observe(true),Some(UiAction::ObserveBrowser {selected:Some(id),..}) if id=="t005")
            );
            assert!(browser.apply(
                generation,
                20,
                None,
                newest_page(),
                vec![],
                Some("page-two".into()),
                vec![],
                None
            ));
            assert_eq!(
                browser.items.len(),
                51,
                "only one previous selected row is pinned"
            );
            assert_eq!(browser.items[browser.selected].thread_id, "t005");
            assert_eq!(browser.selected_screen_row(), screen);
            assert_eq!(browser.preferred.as_deref(), Some("t005"));
            let selected = &browser.items[browser.selected];
            let tree_row = browser
                .rows()
                .iter()
                .find(|row| row.index == browser.selected)
                .unwrap();
            assert!(
                browser
                    .label(tree_row, selected, true, false, 120)
                    .contains("再確認中")
            );
            assert!(browser.excerpt_text(selected).contains("前回の表示"));
            assert!(browser.patch(patch(
                &browser,
                21,
                vec![row("t005", "matching confirmed", "2026-10-01")],
                &[]
            )));
            assert_eq!(browser.items[browser.selected].thread_id, "t005");
            assert_eq!(browser.selected_screen_row(), screen);
            assert!(browser.preferred.is_none());
            assert!(browser.reconfirming_row.is_none());
            assert!(browser.items.len() <= 2000);
        }
    }
    #[test]
    fn initial_preferred_outside_first_page_is_observed_until_matching_patch_arrives() {
        let mut browser = ThreadBrowser::default();
        browser.open(String::new(), vec![], Some("outside".into()));
        assert!(
            matches!(browser.observe(true),Some(UiAction::ObserveBrowser {selected:Some(id),..}) if id=="outside")
        );
        assert!(browser.apply(
            browser.generation,
            20,
            None,
            newest_page(),
            vec![],
            Some("next".into()),
            vec![],
            None
        ));
        assert_eq!(browser.preferred.as_deref(), Some("outside"));
        assert!(
            browser.observe(true).is_none(),
            "same exact preferred consumer is not polled again"
        );
        assert!(browser.patch(patch(
            &browser,
            21,
            vec![row("outside", "matching outside", "2026-01-01")],
            &[]
        )));
        assert_eq!(browser.items[browser.selected].thread_id, "outside");
        assert!(browser.preferred.is_none());
    }
    #[test]
    fn authoritative_missing_nonmatch_or_exhaustive_page_retires_pending_selection() {
        for proof in 0..3 {
            let mut browser = offpage();
            browser.resync(browser.generation, "fixture".into());
            let generation = browser.generation;
            assert!(browser.apply(
                generation,
                20,
                None,
                newest_page(),
                vec![],
                Some("next".into()),
                vec![],
                None
            ));
            match proof {
                0 => {
                    browser.patch(patch(&browser, 21, vec![], &["t005"]));
                }
                1 => {
                    browser.filter = "needle".into();
                    browser.patch(patch(
                        &browser,
                        21,
                        vec![row("t005", "known nonmatch", "2026-10-01")],
                        &[],
                    ));
                }
                _ => {
                    browser.apply(
                        generation,
                        21,
                        None,
                        newest_page(),
                        vec![],
                        None,
                        vec![],
                        None,
                    );
                }
            }
            assert!(browser.preferred.is_none());
            assert!(browser.reconfirming_row.is_none());
            assert!(!browser.items.iter().any(|row| row.thread_id == "t005"));
            assert!(
                !matches!(browser.observe(true),Some(UiAction::ObserveBrowser {selected:Some(id),..}) if id=="t005")
            );
        }
    }
    #[test]
    fn deliberate_selection_cancels_autofocus_but_keeps_unconfirmed_row_label_until_refreshed() {
        let mut browser = offpage();
        browser.resync(browser.generation, "fixture".into());
        browser.apply(
            browser.generation,
            20,
            None,
            newest_page(),
            vec![],
            Some("next".into()),
            vec![],
            None,
        );
        browser.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let chosen = browser.items[browser.selected].thread_id.clone();
        assert_ne!(chosen, "t005");
        assert!(browser.preferred.is_none());
        let old = browser
            .items
            .iter()
            .find(|row| row.thread_id == "t005")
            .unwrap();
        assert!(
            browser.excerpt_text(old).contains("再確認中"),
            "canceling autofocus cannot relabel old facts as fresh"
        );
        browser.patch(patch(
            &browser,
            21,
            vec![row("t005", "matching confirmed", "2026-10-01")],
            &[],
        ));
        assert_eq!(
            browser.items[browser.selected].thread_id, chosen,
            "late preferred result must not snap selection back"
        );
        assert!(browser.reconfirming_row.is_none());
    }
}
