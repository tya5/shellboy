//! Bounded, ephemeral conversation identity and inline interaction state.
use crate::presentation::{PresentationBlock, ToolPresentation};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKey {
    Node(String),
    Tool { group_id: String, call_id: String },
    LocalTool { node_id: String, block_index: usize },
}
impl ItemKey {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::Node(id) => id.capacity(),
                Self::Tool { group_id, call_id } => group_id.capacity() + call_id.capacity(),
                Self::LocalTool { node_id, .. } => node_id.capacity(),
            }
    }
    pub(crate) fn tool(tool: &ToolPresentation, node: &str, block_index: usize) -> Self {
        match tool.group_id.as_ref().filter(|group| !group.is_empty()) {
            Some(group) if !tool.call_id.is_empty() => Self::Tool {
                group_id: group.clone(),
                call_id: tool.call_id.clone(),
            },
            _ => Self::LocalTool {
                node_id: node.into(),
                block_index,
            },
        }
    }
}
#[derive(Clone, Debug)]
pub struct ItemRegion {
    pub key: ItemKey,
    pub source_node_ids: Vec<String>,
    pub primary_node_id: String,
    pub execution_id: Option<String>,
    pub expandable: bool,
    pub default_open: bool,
    pub outcome: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preview {
    pub blocks: Vec<PresentationBlock>,
    pub notice: Option<String>,
}
impl Preview {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.notice.as_ref().map_or(0, String::capacity)
            + self.blocks.capacity() * std::mem::size_of::<PresentationBlock>()
            + self
                .blocks
                .iter()
                .map(|block| block.text.capacity())
                .sum::<usize>()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PreviewStatus {
    #[default]
    Absent,
    Loading,
    Ready,
    Failed,
}
#[derive(Clone, Debug)]
struct Entry {
    explicit: Option<bool>,
    outcome: String,
    request_revision: u64,
    preview: Option<Preview>,
    preview_status: PreviewStatus,
    hydration_attempted_visible: bool,
    touched: u64,
}
#[derive(Clone, Debug, Default)]
pub struct ItemState {
    pub focus: Option<ItemKey>,
    pub active: bool,
    entries: BTreeMap<ItemKey, Entry>,
    clock: u64,
    revision: u64,
}
impl ItemState {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn cache_revision(&self, key: &ItemKey) -> u64 {
        self.entries.get(key).map_or(0, |entry| entry.touched)
    }
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.entries.clear();
        self.focus = None;
        self.active = false;
        self.clock = self.clock.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn retained_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|(key, entry)| {
                key.retained_bytes()
                    + std::mem::size_of::<Entry>()
                    + entry.outcome.capacity()
                    + entry.preview.as_ref().map_or(0, Preview::bytes)
            })
            .sum::<usize>()
            + self.focus.as_ref().map_or(0, ItemKey::retained_bytes)
    }
    pub fn evict_previews_except(&mut self, protected: &[ItemKey]) -> usize {
        let mut freed = 0;
        let changed_at = self.clock.wrapping_add(1);
        for (key, entry) in &mut self.entries {
            if !protected.contains(key)
                && let Some(preview) = entry.preview.take()
            {
                freed += preview.bytes();
                entry.touched = changed_at;
                if entry.preview_status == PreviewStatus::Ready {
                    entry.preview_status = PreviewStatus::Absent;
                }
            }
        }
        if freed > 0 {
            self.clock = changed_at;
            self.revision = self.revision.wrapping_add(1);
        }
        freed
    }
    pub fn is_open(&self, key: &ItemKey, default_open: bool, outcome: &str) -> bool {
        self.entries
            .get(key)
            .filter(|entry| entry.outcome == outcome)
            .and_then(|entry| entry.explicit)
            .unwrap_or(default_open)
    }
    fn touch(&mut self, key: &ItemKey, outcome: &str) -> &mut Entry {
        self.clock = self.clock.wrapping_add(1);
        if !self.entries.contains_key(key)
            && self.entries.len() >= 128
            && let Some(old) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&old);
        }
        let entry = self.entries.entry(key.clone()).or_insert(Entry {
            explicit: None,
            outcome: outcome.into(),
            request_revision: 0,
            preview: None,
            preview_status: PreviewStatus::Absent,
            hydration_attempted_visible: false,
            touched: self.clock,
        });
        if entry.outcome != outcome {
            entry.outcome = outcome.into();
            entry.explicit = None;
            entry.preview = None;
            entry.preview_status = PreviewStatus::Absent;
            entry.hydration_attempted_visible = false;
            entry.request_revision = entry.request_revision.wrapping_add(1);
        }
        entry.touched = self.clock;
        entry
    }
    pub fn toggle(&mut self, key: ItemKey, default_open: bool, outcome: &str) -> bool {
        let open = !self.is_open(&key, default_open, outcome);
        let entry = self.touch(&key, outcome);
        entry.explicit = Some(open);
        if open && entry.preview_status == PreviewStatus::Failed {
            entry.preview_status = PreviewStatus::Absent;
        }
        self.focus = Some(key);
        self.revision = self.revision.wrapping_add(1);
        open
    }
    pub fn request(&mut self, key: &ItemKey, outcome: &str) -> u64 {
        self.touch(key, outcome);
        // A recreated LRU entry must never reuse an old in-flight token.
        let request = self.clock;
        let entry = self.entries.get_mut(key).expect("touched item");
        entry.request_revision = request;
        entry.preview_status = PreviewStatus::Loading;
        entry.hydration_attempted_visible = true;
        self.revision = self.revision.wrapping_add(1);
        request
    }
    pub fn accept_preview(
        &mut self,
        key: &ItemKey,
        request_revision: u64,
        preview: Preview,
        budget: usize,
    ) -> bool {
        if !self.entries.get(key).is_some_and(|entry| {
            entry.request_revision == request_revision
                && entry.preview_status == PreviewStatus::Loading
        }) {
            return false;
        }
        // Per-item and aggregate display copies are finite, never a second raw log.
        let budget = budget.min(8 * 1024 * 1024);
        if preview.bytes() > budget
            || preview.blocks.iter().any(|block| {
                block.tool.is_some() || block.rendered.is_some() || !block.markdown.is_empty()
            })
        {
            return false;
        }
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(key).expect("validated item");
        entry.preview = Some(preview);
        entry.preview_status = PreviewStatus::Ready;
        entry.touched = self.clock;
        self.trim_preview_budget(budget);
        self.revision = self.revision.wrapping_add(1);
        true
    }
    fn trim_preview_budget(&mut self, budget: usize) {
        while self
            .entries
            .values()
            .filter_map(|entry| entry.preview.as_ref())
            .map(Preview::bytes)
            .sum::<usize>()
            > budget
        {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.preview.is_some())
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            self.clock = self.clock.wrapping_add(1);
            let evicted = self.entries.get_mut(&oldest).expect("known item");
            evicted.preview = None;
            if evicted.preview_status == PreviewStatus::Ready {
                evicted.preview_status = PreviewStatus::Absent;
            }
            evicted.touched = self.clock;
        }
    }
    pub fn pending_previews(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.preview_status == PreviewStatus::Loading)
            .count()
    }
    pub fn cancel_pending(&mut self) {
        let mut changed = false;
        for entry in self
            .entries
            .values_mut()
            .filter(|entry| entry.preview_status == PreviewStatus::Loading)
        {
            self.clock = self.clock.wrapping_add(1);
            entry.request_revision = self.clock;
            entry.preview_status = PreviewStatus::Absent;
            entry.hydration_attempted_visible = false;
            entry.touched = self.clock;
            changed = true;
        }
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
    }
    pub fn mark_visible(&mut self, visible: &[ItemKey]) {
        for (key, entry) in &mut self.entries {
            if !visible.contains(key) {
                entry.hydration_attempted_visible = false;
            }
        }
    }
    pub fn needs_visible_preview(&self, key: &ItemKey, outcome: &str) -> bool {
        self.needs_preview(key, outcome)
            && self
                .entries
                .get(key)
                .is_some_and(|entry| !entry.hydration_attempted_visible)
    }
    pub fn preview_loading(&self, key: &ItemKey, outcome: &str) -> bool {
        self.entries.get(key).is_some_and(|entry| {
            entry.outcome == outcome && entry.preview_status == PreviewStatus::Loading
        })
    }
    pub fn needs_preview(&self, key: &ItemKey, outcome: &str) -> bool {
        self.entries.get(key).is_some_and(|entry| {
            entry.outcome == outcome
                && entry.explicit == Some(true)
                && entry.preview_status == PreviewStatus::Absent
        })
    }
    pub fn preview_failed(&self, key: &ItemKey, outcome: &str) -> bool {
        self.entries.get(key).is_some_and(|entry| {
            entry.outcome == outcome && entry.preview_status == PreviewStatus::Failed
        })
    }
    pub fn accept_failed_preview(
        &mut self,
        key: &ItemKey,
        request_revision: u64,
        message: String,
        budget: usize,
    ) -> bool {
        if !self.entries.get(key).is_some_and(|entry| {
            entry.request_revision == request_revision
                && entry.preview_status == PreviewStatus::Loading
        }) {
            return false;
        }
        let budget = budget.min(8 * 1024 * 1024);
        let message = crate::presentation::sanitize_plain(&message, 0)
            .0
            .chars()
            .take(512)
            .collect();
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(key).expect("validated request");
        let mut preview = entry.preview.take().unwrap_or_default();
        preview.notice = Some(message);
        if preview.bytes() > budget {
            preview.blocks = Vec::new();
        }
        entry.preview = (preview.bytes() <= budget).then_some(preview);
        entry.preview_status = PreviewStatus::Failed;
        entry.touched = self.clock;
        self.trim_preview_budget(budget);
        self.revision = self.revision.wrapping_add(1);
        true
    }
    #[cfg(test)]
    pub fn preview(&self, key: &ItemKey) -> Option<&Preview> {
        self.entries.get(key)?.preview.as_ref()
    }
    pub fn matches_outcome(&self, key: &ItemKey, outcome: &str) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| entry.outcome == outcome)
    }
    pub fn preview_for(&self, key: &ItemKey, outcome: &str) -> Option<&Preview> {
        self.entries
            .get(key)
            .filter(|entry| entry.outcome == outcome)?
            .preview
            .as_ref()
    }
    pub fn cancel_request(&mut self, key: &ItemKey) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.request_revision = entry.request_revision.wrapping_add(1);
            if entry.preview_status == PreviewStatus::Loading {
                entry.preview_status = PreviewStatus::Absent;
                entry.hydration_attempted_visible = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_failure_close_survives_refresh_but_not_changed_outcome() {
        let key = ItemKey::Tool {
            group_id: "g".into(),
            call_id: "c".into(),
        };
        let mut state = ItemState::default();
        assert!(state.is_open(&key, true, "failed:1"));
        assert!(!state.toggle(key.clone(), true, "failed:1"));
        assert!(!state.is_open(&key, true, "failed:1"));
        assert!(state.is_open(&key, true, "failed:2"));
        assert!(!state.is_open(&key, false, "success:0"));
    }
    #[test]
    fn item_eviction_and_request_revisions_reject_late_preview() {
        let mut state = ItemState::default();
        let key = ItemKey::Node("first".into());
        let old = state.request(&key, "");
        let new = state.request(&key, "");
        assert!(!state.accept_preview(&key, old, Preview::default(), 1024));
        assert!(state.accept_preview(&key, new, Preview::default(), 1024));
        for i in 0..128 {
            state.request(&ItemKey::Node(i.to_string()), "");
        }
        assert_eq!(state.entries.len(), 128);
        assert!(!state.accept_preview(&key, new, Preview::default(), 1024));
        let reopened = state.request(&key, "");
        assert_ne!(reopened, new);
        assert!(!state.accept_preview(&key, new, Preview::default(), 1024));
    }
    #[test]
    fn preview_eviction_invalidates_item_cache_and_clear_keeps_tokens_monotonic() {
        let mut state = ItemState::default();
        let key = ItemKey::Node("source".into());
        let request = state.request(&key, "");
        assert!(state.accept_preview(&key, request, Preview::default(), 1024));
        let cached = state.cache_revision(&key);
        assert!(state.evict_previews_except(&[]) > 0);
        assert!(state.cache_revision(&key) > cached);
        assert!(state.preview(&key).is_none());
        state.clear();
        let after = state.request(&key, "");
        assert!(after > request);
        assert!(!state.accept_preview(&key, request, Preview::default(), 1024));
    }
    #[test]
    fn many_expanded_previews_fit_small_aggregate_budget_and_rehydrate_safely() {
        let mut state = ItemState::default();
        let budget = 64 * 1024;
        let preview = || Preview {
            blocks: vec![PresentationBlock {
                kind: crate::presentation::BlockKind::ToolResult,
                text: "x".repeat(16 * 1024),
                ..Default::default()
            }],
            notice: None,
        };
        let first = ItemKey::Node("0".into());
        let mut first_token = 0;
        for index in 0..128 {
            let key = ItemKey::Node(index.to_string());
            assert!(state.toggle(key.clone(), false, "done"));
            let token = state.request(&key, "done");
            if index == 0 {
                first_token = token;
            }
            assert!(state.accept_preview(&key, token, preview(), budget));
            let payload = state
                .entries
                .values()
                .filter_map(|entry| entry.preview.as_ref())
                .flat_map(|preview| &preview.blocks)
                .map(|block| block.text.capacity())
                .sum::<usize>();
            assert!(
                payload <= budget,
                "128 open items must never pin 128 raw copies"
            );
        }
        assert!(state.preview(&first).is_none());
        let next = state.request(&first, "done");
        assert_ne!(next, first_token);
        assert!(!state.accept_preview(&first, first_token, preview(), budget));
        assert!(state.accept_preview(&first, next, preview(), budget));
        assert_eq!(
            state.preview(&first).unwrap().blocks[0].text.len(),
            16 * 1024
        );
    }
    #[test]
    fn an_open_evicted_preview_requests_once_and_rejects_duplicate_terminal_replies() {
        let mut state = ItemState::default();
        let key = ItemKey::Node("open".into());
        assert!(state.toggle(key.clone(), false, "done"));
        assert!(state.needs_preview(&key, "done"));
        let first = state.request(&key, "done");
        assert!(!state.needs_preview(&key, "done"));
        assert!(state.accept_preview(&key, first, Preview::default(), 1024));
        assert!(
            !state.accept_preview(&key, first, Preview::default(), 1024),
            "terminal replies are consumed once"
        );
        state.evict_previews_except(&[]);
        assert!(state.is_open(&key, false, "done"));
        assert!(state.needs_preview(&key, "done"));
        assert!(
            !state.accept_preview(&key, first, Preview::default(), 1024),
            "eviction cannot resurrect an old token"
        );
        let second = state.request(&key, "done");
        assert_ne!(first, second);
        assert!(!state.needs_preview(&key, "done"));
        assert!(state.accept_preview(&key, second, Preview::default(), 1024));
        assert!(!state.needs_preview(&key, "done"));
    }
    #[test]
    fn failed_preview_retries_only_after_deliberate_close_and_reopen_even_when_evicted() {
        let mut state = ItemState::default();
        let key = ItemKey::Node("failed".into());
        state.toggle(key.clone(), false, "done");
        let token = state.request(&key, "done");
        assert!(state.accept_failed_preview(&key, token, "temporary read failure".into(), 1024));
        assert!(state.preview_failed(&key, "done"));
        assert!(!state.needs_preview(&key, "done"));
        assert!(!state.accept_failed_preview(&key, token, "duplicate".into(), 1024));
        state.evict_previews_except(&[]);
        assert!(state.preview_for(&key, "done").is_none());
        for _ in 0..100 {
            assert!(
                !state.needs_preview(&key, "done"),
                "animation cannot retry a failed read"
            );
        }
        assert!(!state.toggle(key.clone(), false, "done"));
        state.cancel_request(&key);
        assert!(state.toggle(key.clone(), false, "done"));
        assert!(state.needs_preview(&key, "done"));
        let retry = state.request(&key, "done");
        assert!(state.accept_preview(&key, retry, Preview::default(), 1024));
        assert!(!state.preview_failed(&key, "done"));
        assert!(!state.needs_preview(&key, "done"));
    }
    #[test]
    fn failed_repeat_preview_keeps_a_known_paid_display_copy() {
        let mut state = ItemState::default();
        let key = ItemKey::Node("paid".into());
        state.toggle(key.clone(), false, "done");
        let first = state.request(&key, "done");
        let preview = Preview {
            blocks: vec![PresentationBlock {
                kind: crate::presentation::BlockKind::ToolResult,
                text: "KNOWN_PAID_OUTPUT".into(),
                ..Default::default()
            }],
            notice: None,
        };
        assert!(state.accept_preview(&key, first, preview, 4096));
        let repeat = state.request(&key, "done");
        assert!(state.accept_failed_preview(&key, repeat, "read failed".into(), 4096));
        let kept = state.preview_for(&key, "done").unwrap();
        assert_eq!(kept.blocks[0].text, "KNOWN_PAID_OUTPUT");
        assert_eq!(kept.notice.as_deref(), Some("read failed"));
    }
    #[test]
    fn visible_cache_pressure_does_not_repeat_hydration_until_reentry_or_manual_reopen() {
        let mut state = ItemState::default();
        let keys = (0..4)
            .map(|index| ItemKey::Node(index.to_string()))
            .collect::<Vec<_>>();
        for key in &keys {
            state.toggle(key.clone(), false, "done");
        }
        let preview = || Preview {
            blocks: vec![PresentationBlock {
                kind: crate::presentation::BlockKind::ToolResult,
                text: "x".repeat(16 * 1024),
                ..Default::default()
            }],
            notice: None,
        };
        let mut reads = 0;
        for _ in 0..100 {
            state.mark_visible(&keys);
            for key in &keys {
                if state.needs_visible_preview(key, "done") {
                    let token = state.request(key, "done");
                    reads += 1;
                    assert!(state.preview_loading(key, "done"));
                    assert!(state.accept_preview(key, token, preview(), 20 * 1024));
                }
            }
        }
        assert_eq!(
            reads, 4,
            "a budget fitting one of four visible previews must not cause a read loop"
        );
        assert_eq!(
            state
                .entries
                .values()
                .filter(|entry| entry.preview.is_some())
                .count(),
            1
        );
        assert!(state.needs_preview(&keys[0], "done"));
        assert!(!state.needs_visible_preview(&keys[0], "done"));
        state.toggle(keys[0].clone(), false, "done");
        state.toggle(keys[0].clone(), false, "done");
        let manual = state.request(&keys[0], "done");
        assert!(state.accept_preview(&keys[0], manual, preview(), 20 * 1024));
        state.mark_visible(&[]);
        state.mark_visible(&keys);
        for _ in 0..100 {
            state.mark_visible(&keys);
            for key in &keys {
                if state.needs_visible_preview(key, "done") {
                    let token = state.request(key, "done");
                    reads += 1;
                    assert!(state.accept_preview(key, token, preview(), 20 * 1024));
                }
            }
        }
        assert_eq!(
            reads, 8,
            "each preview gets at most one new attempt after viewport reentry"
        );
        let pending = state.request(&keys[0], "done");
        state.cancel_pending();
        assert!(state.needs_visible_preview(&keys[0], "done"));
        assert!(!state.accept_preview(&keys[0], pending, preview(), 20 * 1024));
    }
    #[test]
    fn repeated_call_labels_in_distinct_groups_are_different_items() {
        let mut tool = ToolPresentation {
            call_id: "same".into(),
            group_id: Some("a".into()),
            ..Default::default()
        };
        let a = ItemKey::tool(&tool, "n", 0);
        tool.group_id = Some("b".into());
        assert_ne!(a, ItemKey::tool(&tool, "n", 0));
        tool.group_id = None;
        assert_ne!(
            ItemKey::tool(&tool, "n", 0),
            ItemKey::tool(&tool, "other", 0)
        );
    }
}
