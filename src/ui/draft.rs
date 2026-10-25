//! Local attachment chips and canonical input order. No filesystem/DB work.
use super::{DraftSnapshot, PasteFold, UiAttachment};
use crate::{
    attachments::DraftContent,
    protocol::{ApiError, ErrorCode},
};
use crossterm::event::KeyCode;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const MARKER: &str = "\u{fffc}";
fn invalid() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidArgument,
        "Draft attachment positions or source elements are inconsistent; retain the draft and inspect its source",
    )
}
impl DraftSnapshot {
    pub(super) fn insert_text(&mut self, text: &str, pasted: bool, fold_id: Option<u64>) {
        self.snap_cursor(true);
        let start = self.cursor;
        adjust_folds(&mut self.folds, start, start, text.len());
        self.adjust_attachments(start, start, text.len());
        self.text.insert_str(start, text);
        self.cursor += text.len();
        if let Some(id) = fold_id {
            self.folds.push(PasteFold {
                id,
                start,
                end: start + text.len(),
                expanded: false,
            });
            self.folds.sort_by_key(|f| f.start);
        }
        self.literal_paste |= pasted;
    }
    pub(super) fn delete_at_cursor(&mut self, backwards: bool) {
        let (mut start, mut end) = if backwards {
            (prev_grapheme(&self.text, self.cursor), self.cursor)
        } else {
            (self.cursor, next_grapheme(&self.text, self.cursor))
        };
        for fold in self.folds.iter().filter(|fold| !fold.expanded) {
            if start < fold.end && end > fold.start {
                start = start.min(fold.start);
                end = end.max(fold.end);
            }
        }
        if start != end {
            adjust_folds(&mut self.folds, start, end, 0);
            self.adjust_attachments(start, end, 0);
            self.text.replace_range(start..end, "");
            self.cursor = start;
        }
    }
    pub(super) fn move_cursor(&mut self, key: KeyCode) {
        match key {
            KeyCode::Left => self.cursor = prev_grapheme(&self.text, self.cursor),
            KeyCode::Right => self.cursor = next_grapheme(&self.text, self.cursor),
            KeyCode::Home => {
                self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |p| p + 1)
            }
            KeyCode::End => {
                self.cursor += self.text[self.cursor..]
                    .find('\n')
                    .unwrap_or(self.text.len() - self.cursor)
            }
            KeyCode::Up => move_vertical(self, false),
            KeyCode::Down => move_vertical(self, true),
            _ => return,
        }
        self.snap_cursor(matches!(key, KeyCode::Right | KeyCode::End | KeyCode::Down));
    }
    fn snap_cursor(&mut self, forward: bool) {
        if let Some(fold) = self
            .folds
            .iter()
            .find(|fold| !fold.expanded && fold.start < self.cursor && self.cursor < fold.end)
        {
            self.cursor = if forward { fold.end } else { fold.start };
        }
    }

    pub(super) fn clear_text(&mut self) {
        self.attachments
            .sort_by_key(|a| a.position.unwrap_or(self.text.len()));
        self.text.clear();
        for attachment in &mut self.attachments {
            if attachment.position.is_some() {
                attachment.position = Some(self.text.len());
                self.text.push_str(MARKER);
            }
        }
        self.cursor = self.text.len();
        self.folds.clear();
    }
    pub(super) fn adjust_attachments(&mut self, start: usize, end: usize, added: usize) {
        self.attachments.retain_mut(|attachment| {
            let Some(position) = attachment.position.as_mut() else {
                return true;
            };
            if *position >= end {
                *position = *position + added - (end - start);
                true
            } else {
                *position < start
            }
        });
    }
    pub(super) fn insert_attachment(&mut self, mut attachment: UiAttachment, position: usize) {
        adjust_folds(&mut self.folds, position, position, MARKER.len());
        self.adjust_attachments(position, position, MARKER.len());
        self.text.insert_str(position, MARKER);
        if self.cursor >= position {
            self.cursor += MARKER.len();
        }
        attachment.position = Some(position);
        self.attachments.push(attachment);
    }
    pub(super) fn remove_attachment(&mut self, id: &str) {
        let position = self
            .attachments
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.position);
        if let Some(position) = position {
            let end = position + MARKER.len();
            adjust_folds(&mut self.folds, position, end, 0);
            self.adjust_attachments(position, end, 0);
            self.text.replace_range(position..end, "");
            if self.cursor > position {
                self.cursor = self.cursor.saturating_sub(MARKER.len()).max(position);
            }
        } else {
            self.attachments.retain(|a| a.id != id);
        }
    }
}

pub(crate) fn decode(
    text: &str,
    mut attachments: Vec<UiAttachment>,
) -> Result<(DraftContent, Vec<UiAttachment>), ApiError> {
    let mut ids = HashSet::new();
    let mut positions = HashSet::new();
    for a in &attachments {
        if !ids.insert(&a.id) {
            return Err(invalid());
        }
        if let Some(p) = a.position
            && (!text.is_char_boundary(p) || !text[p..].starts_with(MARKER) || !positions.insert(p))
        {
            return Err(invalid());
        }
    }
    attachments.sort_by_key(|a| a.position.unwrap_or(text.len()));
    let mut plain = String::new();
    let mut source = 0;
    let mut offsets = Vec::new();
    for a in &attachments {
        let position = a.position.unwrap_or(text.len());
        plain.push_str(&text[source..position]);
        offsets.push(plain.len());
        source = position + a.position.map_or(0, |_| MARKER.len());
    }
    plain.push_str(&text[source..]);
    let content = DraftContent {
        text: plain,
        positions: offsets,
    };
    content.validate(attachments.len())?;
    Ok((content, attachments))
}

/// Rebuild from the source's ordered references, never SQL ID order or text flattening.
pub(crate) fn restore(
    payload: &Value,
    attachments: Vec<UiAttachment>,
    selected_only: bool,
) -> Result<(String, Vec<UiAttachment>), ApiError> {
    let count = attachments.len();
    let mut records: HashMap<_, _> = attachments.into_iter().map(|a| (a.id.clone(), a)).collect();
    if records.len() != count {
        return Err(invalid());
    }
    if let Some(text) = payload.get("content").and_then(Value::as_str) {
        if !records.is_empty() {
            return Err(invalid());
        }
        return Ok((text.into(), Vec::new()));
    }
    let elements = payload
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let mut text = String::new();
    let mut ordered = Vec::new();
    for element in elements {
        match element.get("type").and_then(Value::as_str) {
            Some("input_text") => text.push_str(
                element
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?,
            ),
            Some("attachment_ref") => {
                let id = element
                    .get("attachment_id")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let mut attachment = records.remove(id).ok_or_else(invalid)?;
                if selected_only && attachment.state != "active" {
                    continue;
                }
                attachment.position = Some(text.len());
                text.push_str(MARKER);
                ordered.push(attachment);
            }
            _ => return Err(invalid()),
        }
    }
    if !records.is_empty() {
        return Err(invalid());
    }
    Ok((text, ordered))
}

pub(super) fn adjust_folds(folds: &mut Vec<PasteFold>, start: usize, end: usize, added: usize) {
    folds.retain_mut(|fold| {
        if end <= fold.start {
            fold.start = fold.start + added - (end - start);
            fold.end = fold.end + added - (end - start);
            true
        } else {
            fold.end <= start
        }
    });
}
pub(super) fn prev_grapheme(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}
fn next_grapheme(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(text.len(), |(i, _)| cursor + i)
}
fn move_vertical(draft: &mut DraftSnapshot, down: bool) {
    let start = draft.text[..draft.cursor].rfind('\n').map_or(0, |i| i + 1);
    let column = draft.text[start..draft.cursor].graphemes(true).count();
    let target = if down {
        draft.text[draft.cursor..]
            .find('\n')
            .map(|i| draft.cursor + i + 1)
    } else {
        start
            .checked_sub(1)
            .map(|end| draft.text[..end].rfind('\n').map_or(0, |i| i + 1))
    };
    if let Some(target) = target {
        let line = draft.text[target..].split('\n').next().unwrap_or("");
        draft.cursor = target
            + line
                .grapheme_indices(true)
                .nth(column)
                .map_or(line.len(), |(i, _)| i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{UiAction, UiState, UiUpdate},
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    fn attachment(id: &str, position: Option<usize>) -> UiAttachment {
        UiAttachment {
            id: id.into(),
            path: format!("/{id}.png").into(),
            kind: "image".into(),
            state: "active".into(),
            position,
        }
    }
    #[test]
    fn collapsed_paste_cursor_skips_hidden_graphemes_and_editing_is_atomic() {
        fn folded() -> DraftSnapshot {
            let mut draft = DraftSnapshot::default();
            draft.insert_text("前", false, None);
            draft.insert_text("日👨‍👩‍👧‍👦\n中e\u{301}\n後", true, Some(1));
            draft.insert_text("末", false, None);
            draft
        }
        let mut draft = folded();
        let (start, end) = (draft.folds[0].start, draft.folds[0].end);
        let original = draft.text.clone();
        draft.cursor = end;
        for (key, expected) in [
            (KeyCode::Left, start),
            (KeyCode::Right, end),
            (KeyCode::Home, start),
            (KeyCode::End, end),
            (KeyCode::Up, start),
            (KeyCode::Down, end),
        ] {
            draft.move_cursor(key);
            assert_eq!(draft.cursor, expected, "{key:?}");
        }
        assert_eq!(draft.text, original);
        draft.cursor = start + "日".len(); // A legacy saved cursor inside a collapsed block.
        draft.insert_text("追加", false, None);
        assert_eq!(&draft.text[end..], "追加末");
        assert_eq!(draft.folds[0].end, end);
        for backwards in [false, true] {
            let mut draft = folded();
            draft.cursor = if backwards { end } else { start };
            draft.delete_at_cursor(backwards);
            assert_eq!(draft.text, "前末");
            assert!(draft.folds.is_empty());
            assert_eq!(draft.cursor, start);
        }
        let mut draft = folded();
        draft.folds[0].expanded = true;
        draft.cursor = end;
        draft.move_cursor(KeyCode::Left);
        assert_eq!(draft.cursor, end - "後".len());
        draft.delete_at_cursor(false);
        assert_eq!(&draft.text[start..], "日👨‍👩‍👧‍👦\n中e\u{301}\n末");
    }
    #[test]
    fn deleting_fold_preserves_attachment_offsets_and_actual_send_text() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("前", false);
        ui.insert(&"日本語\n".repeat(50), true);
        let fold = ui.view().draft.folds[0].clone();
        ui.apply(UiUpdate::ClipboardAttachment {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            attachment: attachment("after", Some(fold.end)),
        });
        ui.view_mut().draft.cursor = fold.start;
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Delete,
            KeyModifiers::NONE,
        )));
        let draft = &ui.view().draft;
        assert_eq!(draft.text, format!("前{MARKER}"));
        assert_eq!(draft.attachments[0].position, Some("前".len()));
        assert_eq!(
            decode(&draft.text, draft.attachments.clone())
                .unwrap()
                .0
                .text,
            "前"
        );
        assert!(ui.take_deferred_actions().iter().any(|action| matches!(action, UiAction::DraftAttachments {attachment_ids,..} if attachment_ids == &["after".to_owned()])));
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(actions.iter().any(|action| matches!(action, UiAction::Send{text,attachments,..} if text == &format!("前{MARKER}") && attachments[0].id == "after")));
    }
    #[test]
    fn marker_is_owned_by_id_position_and_literal_unicode_stays_plain_text() {
        let text = format!("literal {MARKER} 前{MARKER}後");
        let at = format!("literal {MARKER} 前").len();
        let attachments = vec![attachment("a", Some(at)), attachment("legacy", None)];
        let (content, ordered) = decode(&text, attachments.clone()).unwrap();
        assert_eq!(content.text, format!("literal {MARKER} 前後"));
        assert_eq!(content.positions, [at, content.text.len()]);
        assert_eq!(
            ordered.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["a", "legacy"]
        );
        let draft = DraftSnapshot {
            text,
            attachments,
            cursor: at,
            ..DraftSnapshot::default()
        };
        let saved = serde_json::to_string(&draft).unwrap();
        let restored: DraftSnapshot = serde_json::from_str(&saved).unwrap();
        assert_eq!(draft, restored);
        assert_eq!(
            decode(&restored.text, restored.attachments).unwrap().0.text,
            content.text
        );
        assert!(decode("前", vec![attachment("a", Some(1))]).is_err());
        assert!(
            decode(
                MARKER,
                vec![attachment("a", Some(0)), attachment("b", Some(0))]
            )
            .is_err()
        );
        assert!(decode("normal", vec![attachment("a", Some(0))]).is_err());
        let old: UiAttachment =
            serde_json::from_value(json!({"id":"old","path":"/old","kind":"file","state":"ready"}))
                .unwrap();
        assert_eq!(old.position, None);
        assert_eq!(
            decode("旧本文", vec![old]).unwrap().0.positions,
            ["旧本文".len()]
        );
    }
    #[test]
    fn unicode_edits_move_chips_and_backspace_removes_the_reference_with_its_marker() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("前後", false);
        ui.view_mut().draft.cursor = "前".len();
        ui.apply(UiUpdate::ClipboardAttachment {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            attachment: attachment("a", Some(3)),
        });
        ui.insert("中", false);
        assert_eq!(ui.view().draft.text, format!("前{MARKER}中後"));
        ui.view_mut().draft.cursor = 0;
        ui.insert("頭", true);
        assert_eq!(ui.view().draft.attachments[0].position, Some(6));
        let (display, _) = crate::ui::draft_display(&ui.view().draft);
        assert!(display.contains("頭前[画像: a.png]中後"));
        ui.view_mut().draft.cursor = 9;
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE,
        )));
        assert_eq!(ui.view().draft.text, "頭前中後");
        assert!(ui.view().draft.attachments.is_empty());
        assert!(ui.take_deferred_actions().iter().any(|a|matches!(a,UiAction::DraftAttachments{attachment_ids,..}if attachment_ids.is_empty())));
    }
    #[test]
    fn restore_uses_canonical_source_order_and_drops_only_explicitly_unselected_refs() {
        let payload = json!({"role":"user","content":[
            {"type":"input_text","text":format!("前{MARKER}")},
            {"type":"attachment_ref","attachment_id":"b"},
            {"type":"input_text","text":"中"},
            {"type":"attachment_ref","attachment_id":"a"},
            {"type":"input_text","text":"後"}
        ]});
        let records = vec![attachment("a", None), attachment("b", None)];
        let (text, ordered) = restore(&payload, records.clone(), false).unwrap();
        let (content, ordered) = decode(&text, ordered).unwrap();
        assert_eq!(
            content
                .elements(&ordered.iter().map(|a| a.id.clone()).collect::<Vec<_>>())
                .unwrap(),
            payload["content"].as_array().unwrap().clone()
        );
        let mut selected = records.clone();
        selected[0].state = "excluded".into();
        assert_eq!(
            restore(&payload, selected, true)
                .unwrap()
                .1
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        assert!(restore(&payload, vec![attachment("b", None)], false).is_err());
        assert!(
            restore(
                &payload,
                vec![attachment("b", None), attachment("b", None)],
                false
            )
            .is_err()
        );
    }
    #[test]
    fn rejected_send_retains_ordered_snapshot_and_restore_does_not_use_edited_positions() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("前後", false);
        ui.view_mut().draft.cursor = 3;
        ui.apply(UiUpdate::ClipboardAttachment {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            attachment: attachment("a", Some(3)),
        });
        let snapshot = ui.view().draft.clone();
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(actions.first(), Some(UiAction::Send { .. })));
        ui.action_rejected(&actions[0]);
        assert_eq!(ui.view().draft, snapshot);
        ui.insert("編集", false);
        ui.slash(&format!("/restore {}", snapshot.revision));
        let restored = &ui.view().draft;
        assert_eq!(restored.text, snapshot.text);
        assert_eq!(restored.attachments, snapshot.attachments);
    }
    #[test]
    fn slash_operations_keep_owned_chips_and_rejection_restores_original_positions() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        ui.apply(UiUpdate::ClipboardAttachment {
            draft_id: ui.view().draft.draft_id.clone(),
            revision: ui.view().draft.revision,
            attachment: attachment("a", Some(0)),
        });
        ui.insert("/attachments", false);
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(ui.view().draft.text, MARKER);
        assert_eq!(ui.view().draft.attachments[0].position, Some(0));
        assert!(ui.management.is_some());
        ui.invalidate_management();
        ui.insert("/compact", false);
        let original = ui.view().draft.clone();
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(
            actions.first(),
            Some(UiAction::SubmittedCommand { .. })
        ));
        assert_eq!(ui.view().draft.text, MARKER);
        ui.action_rejected(&actions[0]);
        let mut recovered = ui.view().draft.clone();
        assert!(recovered.revision > original.revision);
        recovered.revision = original.revision;
        assert_eq!(recovered, original);
        ui.view_mut().draft.text = MARKER.into();
        ui.view_mut().draft.cursor = MARKER.len();
        ui.insert("/detach a", false);
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(ui.view().draft.attachments.is_empty());
        assert!(ui.view().draft.text.is_empty());
    }
}
