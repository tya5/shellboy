//! Pure composer display and cursor projection; canonical input stays unchanged.
use super::{DraftSnapshot, draft, theme};
use crate::config::UiLanguage;
use crate::ui::text_width::UnicodeWidthStr;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub(super) fn available_newline_keys(
    keys: &[String],
    keyboard_enhancement_enabled: bool,
) -> Vec<String> {
    keys.iter()
        .filter(|key| keyboard_enhancement_enabled || !is_shift_enter_binding(key.as_str()))
        .cloned()
        .collect()
}

fn is_shift_enter_binding(binding: &str) -> bool {
    let mut parts = binding.split('+').map(str::trim);
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(modifier), Some(key), None)
            if modifier.eq_ignore_ascii_case("shift") && key.eq_ignore_ascii_case("enter")
    )
}

#[cfg(test)]
mod keyboard_enhancement_tests {
    use super::available_newline_keys;

    #[test]
    fn unsupported_terminal_hides_shift_enter_but_keeps_ctrl_j() {
        let configured = vec!["Ctrl+J".to_owned(), "Shift+Enter".to_owned()];
        assert_eq!(
            available_newline_keys(&configured, false),
            vec!["Ctrl+J".to_owned()]
        );
        assert_eq!(available_newline_keys(&configured, true), configured);
    }
}

/// Display representation and byte cursor; folded text remains intact in DraftSnapshot.
pub(super) struct Display {
    pub text: String,
    pub cursor: usize,
    chips: Vec<Range<usize>>,
}
pub(super) fn draft_display(draft: &DraftSnapshot) -> (String, usize) {
    let display = display(draft, UiLanguage::En);
    (display.text, display.cursor)
}
fn paste_chip(language: UiLanguage, id: u64, lines: usize, chars: usize) -> String {
    match language {
        UiLanguage::Ja => crate::ui::text_width::glyphs::formatted!(
            "[貼付け{id} · {lines}行/{chars}字]",
            "[貼付け{id} | {lines}行/{chars}字]"
        ),
        UiLanguage::En => match (lines == 1, chars == 1) {
            (true, true) => crate::ui::text_width::glyphs::formatted!(
                "[Paste {id} · 1 line/1 char]",
                "[Paste {id} | 1 line/1 char]"
            ),
            (true, false) => crate::ui::text_width::glyphs::formatted!(
                "[Paste {id} · 1 line/{chars} chars]",
                "[Paste {id} | 1 line/{chars} chars]"
            ),
            (false, true) => crate::ui::text_width::glyphs::formatted!(
                "[Paste {id} · {lines} lines/1 char]",
                "[Paste {id} | {lines} lines/1 char]"
            ),
            (false, false) => crate::ui::text_width::glyphs::formatted!(
                "[Paste {id} · {lines} lines/{chars} chars]",
                "[Paste {id} | {lines} lines/{chars} chars]"
            ),
        },
    }
}
fn attachment_chip(language: UiLanguage, image: bool, name: &str) -> String {
    match (language, image) {
        (UiLanguage::Ja, true) => format!("[画像: {name}]"),
        (UiLanguage::Ja, false) => format!("[添付: {name}]"),
        (UiLanguage::En, true) => format!("[Image: {name}]"),
        (UiLanguage::En, false) => format!("[Attachment: {name}]"),
    }
}
pub(super) fn display(draft: &DraftSnapshot, language: UiLanguage) -> Display {
    let mut chips = Vec::new();
    let mut text = String::new();
    let mut source = 0;
    let mut cursor = 0;
    let mut replacements: Vec<_> = draft
        .folds
        .iter()
        .filter(|f| !f.expanded)
        .map(|fold| {
            let content = draft.text.get(fold.start..fold.end).unwrap_or("");
            (
                fold.start,
                fold.end,
                paste_chip(
                    language,
                    fold.id,
                    content.lines().count(),
                    content.chars().count(),
                ),
            )
        })
        .collect();
    replacements.extend(draft.attachments.iter().filter_map(|a| {
        let start = a.position?;
        if !draft
            .text
            .get(start..)
            .is_some_and(|s| s.starts_with(draft::MARKER))
        {
            return None;
        }
        let name = a.path.file_name().unwrap_or_default().to_string_lossy();
        Some((
            start,
            start + draft::MARKER.len(),
            attachment_chip(language, a.kind == "image", &name),
        ))
    }));
    replacements.sort_by_key(|r| r.0);
    for (start, end, placeholder) in replacements {
        if start < source || end > draft.text.len() {
            continue;
        }
        text.push_str(&draft.text[source..start]);
        if draft.cursor >= source && draft.cursor <= start {
            cursor = text.len() - (start - draft.cursor);
        }
        let chip_start = text.len();
        text.push_str(&placeholder);
        chips.push(chip_start..text.len());
        if draft.cursor > start && draft.cursor <= end {
            cursor = text.len();
        }
        source = end;
    }
    text.push_str(&draft.text[source..]);
    if draft.cursor >= source {
        cursor = text.len() - (draft.text.len() - draft.cursor);
    }
    Display {
        text,
        cursor,
        chips,
    }
}
pub(super) fn cursor_position(text: &str, byte_cursor: usize, width: u16) -> (u16, u16) {
    let metrics = measure(text, byte_cursor, width);
    (
        metrics.cursor.0.min(u16::MAX as usize) as u16,
        metrics.cursor.1.min(u16::MAX as usize) as u16,
    )
}

pub(super) struct Metrics {
    pub rows: usize,
    pub cursor: (usize, usize),
}
// The same walk supplies geometry and visible text; a full row followed by an
// explicit newline advances once, while the end-of-input caret owns a blank row.
fn walk(
    text: &str,
    width: u16,
    mut glyph: impl FnMut(usize, &str, usize, usize),
) -> (usize, usize) {
    let width = usize::from(width.max(1));
    let (mut x, mut y) = (0, 0);
    for (byte, value) in text.grapheme_indices(true) {
        if matches!(value, "\n" | "\r\n" | "\r") {
            glyph(byte, value, x, y);
            x = 0;
            y += 1;
            continue;
        }
        let cells = value.width().min(width);
        if x + cells > width {
            x = 0;
            y += 1;
        }
        glyph(byte, value, x, y);
        x += cells;
    }
    (x, y)
}
pub(super) fn measure(text: &str, cursor: usize, width: u16) -> Metrics {
    let width = usize::from(width.max(1));
    let cursor = cursor.min(text.len());
    let mut position = None;
    let end = walk(
        text,
        width.min(u16::MAX as usize) as u16,
        |byte, glyph, x, y| {
            if position.is_none() && cursor <= byte {
                position = Some(if x == width { (0, y + 1) } else { (x, y) });
            } else if position.is_none() && cursor < byte + glyph.len() {
                let after = if matches!(glyph, "\n" | "\r\n" | "\r") {
                    (0, y + 1)
                } else {
                    (x + glyph.width().min(width), y)
                };
                position = Some(if after.0 == width {
                    (0, after.1 + 1)
                } else {
                    after
                });
            }
        },
    );
    let end = if end.0 == width { (0, end.1 + 1) } else { end };
    Metrics {
        rows: end.1 + 1,
        cursor: position.unwrap_or(end),
    }
}
pub(super) fn visible(
    display: &Display,
    width: u16,
    start: usize,
    height: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default(); usize::from(height)];
    let mut chip = 0;
    walk(&display.text, width, |byte, glyph, _, row| {
        while display
            .chips
            .get(chip)
            .is_some_and(|range| range.end <= byte)
        {
            chip += 1;
        }
        if matches!(glyph, "\n" | "\r\n" | "\r")
            || row < start
            || row >= start.saturating_add(usize::from(height))
        {
            return;
        }
        let style = if display
            .chips
            .get(chip)
            .is_some_and(|range| range.contains(&byte))
        {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let spans = &mut lines[row - start].spans;
        if let Some(last) = spans.last_mut().filter(|last| last.style == style) {
            last.content.to_mut().push_str(glyph);
        } else {
            spans.push(Span::styled(glyph.to_owned(), style));
        }
    });
    lines
}

#[cfg(test)]
pub(super) fn hints(
    config: &crate::config::Ui,
    phase: Option<super::activity::ActivityPhase>,
    folded: bool,
) -> (String, String) {
    use super::activity::ActivityPhase;
    let action = match phase {
        Some(ActivityPhase::Recovery) => "新しい指示で再開",
        Some(ActivityPhase::Stopping) => "保留送信",
        Some(phase) if phase.is_active() => "追加指示（次の区切り）",
        _ => "送信",
    };
    let mut primary = format!("{} {action}", config.send_key);
    if action == "送信"
        && let Some(newline) = config.newline_keys.first()
    {
        primary.push_str(&format!(
            "{}{newline} 改行",
            crate::ui::text_width::glyphs::separator()
        ));
    }
    let mut secondary = Vec::new();
    if phase.is_some_and(|phase| phase.is_active()) {
        secondary.push("Ctrl+C 中断");
        if phase == Some(ActivityPhase::Tools) {
            secondary.push("Ctrl+B 背景化");
        }
    }
    if !config.send_key.eq_ignore_ascii_case("Ctrl+U")
        && !config
            .newline_keys
            .iter()
            .any(|key| key.eq_ignore_ascii_case("Ctrl+U"))
    {
        secondary.push("Ctrl+U 消去");
    }
    if folded {
        secondary.push("/paste 番号 show|edit|remove");
    }
    (
        primary,
        secondary.join(crate::ui::text_width::glyphs::separator()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{
            EditRequestTarget, ReceiptStatus, UiAction, UiAttachment, UiState, UiUpdate,
            activity::ActivityPhase,
        },
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};
    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }
    #[test]
    fn measured_width_preserves_user_content_and_drives_cursor() {
        let text = "A\u{00b7}\u{2192}👩\u{200d}💻";
        for measured in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(measured);
            let draft = DraftSnapshot {
                text: text.into(),
                cursor: text.len(),
                ..Default::default()
            };
            let display = display(&draft, UiLanguage::En);
            assert_eq!(display.text, text);
            assert_eq!(visible(&display, 120, 0, 1)[0].to_string(), text);
            assert_eq!(
                measure(text, text.len(), 120).cursor,
                (crate::ui::text_width::UnicodeWidthStr::width(text), 0)
            );
            for line in visible(&display, 4, 0, 20) {
                assert!(
                    line.width() <= 4,
                    "wrapped input overflows under {measured:?}"
                );
            }
            assert_eq!(draft.text, text);
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn character_wrap_matches_visible_words_unicode_caret_and_large_scroll_without_a_full_buffer() {
        let display = Display {
            text: "abcde fgh".into(),
            cursor: 6,
            chips: vec![],
        };
        let metrics = measure(&display.text, display.cursor, 8);
        assert_eq!(metrics.cursor, (6, 0));
        let lines = visible(&display, 8, 0, 3);
        assert_eq!(lines[0].to_string(), "abcde fg");
        assert_eq!(lines[1].to_string(), "h");
        for (text, cursor, width, expected) in [
            ("ab\n", 3, 2, (0, 1)),
            ("a\r\nb", 3, 8, (0, 1)),
            ("ab", 2, 2, (0, 1)),
            ("a😀", 1, 2, (0, 1)),
            ("日e\u{301}😀", "日e\u{301}".len(), 20, (3, 0)),
            ("日本", 6, 3, (2, 1)),
        ] {
            assert_eq!(measure(text, cursor, width).cursor, expected, "{text:?}");
        }
        let display = Display {
            text: format!("{}LAST", "\n".repeat(70_000)),
            cursor: 70_004,
            chips: vec![],
        };
        let metrics = measure(&display.text, display.cursor, 20);
        assert_eq!(metrics.cursor, (4, 70_000));
        assert_eq!(metrics.rows, 70_001);
        let lines = visible(&display, 20, 69_998, 3);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].to_string(), "LAST");
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("abcde fgh", false);
        ui.view_mut().draft.cursor = 6;
        let mut terminal = Terminal::new(TestBackend::new(10, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut ui))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(
            terminal.backend().buffer()[(cursor.x, cursor.y)].symbol(),
            "f",
            "OS cursor is on the same rendered glyph"
        );
    }
    #[test]
    fn folded_paste_uses_projected_height_and_typed_chip_style_without_changing_send_text() {
        let mut config = Config::default();
        config.ui.language = UiLanguage::Ja;
        let mut ui = UiState::new(&config, std::env::temp_dir());
        // This assertion exercises the rich palette deliberately. The host may
        // set NO_COLOR/TERM=dumb; never mutate process-wide environment in tests.
        let palette = super::super::theme::Palette::from_capabilities(
            crate::config::PaletteMode::Dark,
            false,
            true,
        );
        ui.palette = palette;
        ui.view_mut().palette = palette;
        let original = "source line 日本語\n".repeat(40);
        ui.insert(&original, true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut ui))
            .unwrap();
        assert_eq!(
            ui.view().navigation.scroll.area.unwrap().height,
            13,
            "one folded line uses a three-row input box"
        );
        let chip = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .find(|cell| cell.symbol() == "貼")
            .unwrap();
        assert_eq!(chip.fg, Color::Cyan);
        assert!(chip.modifier.contains(Modifier::BOLD));
        assert_eq!(ui.view().draft.text, original);
        ui.view_mut().draft.folds[0].expanded = true;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut ui))
            .unwrap();
        assert_eq!(
            ui.view().navigation.scroll.area.unwrap().height,
            10,
            "expanded input is capped at one quarter of 24 rows"
        );
        let actions = ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            actions
                .iter()
                .any(|action| matches!(action,UiAction::Send{text,..} if text==&original))
        );
        for (width, height) in [(8, 5), (20, 10), (60, 20), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut ui))
                .unwrap();
        }
    }
    #[test]
    fn clear_keeps_accepted_snapshots_and_rejects_late_receipts_while_custom_bindings_win() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.insert("original request", false);
        let revision = ui.view().draft.revision;
        ui.event(key(KeyCode::Enter, KeyModifiers::NONE));
        let draft_id = ui.view().draft.draft_id.clone();
        ui.insert("\nnew draft\n".repeat(6).as_str(), true);
        ui.view_mut().draft.attachments.push(UiAttachment {
            id: "local".into(),
            path: "kept-external-file".into(),
            kind: "file".into(),
            state: "ready".into(),
            position: None,
        });
        ui.view_mut().draft.edit = Some(EditRequestTarget {
            thread_id: "source".into(),
            node_id: "node".into(),
            run_id: None,
        });
        ui.event(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(ui.view().draft.text.is_empty());
        assert!(ui.view().draft.attachments.is_empty());
        assert!(ui.view().draft.folds.is_empty());
        assert!(ui.view().draft.edit.is_none());
        assert_eq!(ui.view().draft.cursor, 0);
        assert!(!ui.view().draft.literal_paste);
        assert!(ui.view().draft.revision > revision);
        assert_eq!(
            ui.view().pending[&revision].snapshot.text,
            "original request"
        );
        assert!(ui.take_deferred_actions().iter().any(|action|matches!(action,UiAction::DraftAttachments{attachment_ids,..} if attachment_ids.is_empty())));
        ui.apply(UiUpdate::Receipt {
            draft_id,
            revision,
            thread_id: None,
            status: ReceiptStatus::Failed,
            message: Some("late refusal".into()),
        });
        assert!(
            ui.view().draft.text.is_empty(),
            "a late receipt cannot restore cleared input"
        );
        let mut config = Config::default();
        config.ui.send_key = "Ctrl+U".into();
        let mut ui = UiState::new(&config, std::env::temp_dir());
        ui.insert("configured send", false);
        assert!(
            ui.event(key(KeyCode::Char('u'), KeyModifiers::CONTROL))
                .iter()
                .any(|action| matches!(action,UiAction::Send{text,..} if text=="configured send"))
        );
        assert!(!hints(&config.ui, None, false).1.contains("Ctrl+U 消去"));
        let config = Config::default();
        assert!(
            hints(&config.ui, Some(ActivityPhase::Receiving), false)
                .0
                .contains("追加指示（次の区切り）")
        );
        assert!(
            hints(&config.ui, Some(ActivityPhase::Recovery), false)
                .0
                .contains("新しい指示で再開")
        );
        assert!(
            hints(&config.ui, Some(ActivityPhase::Stopping), false)
                .0
                .contains("保留送信")
        );
    }
}

#[cfg(test)]
mod language_tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{PasteFold, UiAction, UiAttachment, UiState},
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn composer(ui: &mut UiState) {
        ui.insert(&"pasted\n".repeat(40), true);
        let draft = &mut ui.view_mut().draft;
        let position = draft.text.len();
        draft.text.push_str(draft::MARKER);
        draft.cursor = draft.text.len();
        draft.attachments.push(UiAttachment {
            id: "attachment-id".into(),
            path: "picture.png".into(),
            kind: "image".into(),
            state: "ready".into(),
            position: Some(position),
        });
    }
    #[test]
    fn composer_screen_is_registered_in_both_languages() {
        crate::ui::i18n::verify_screen(composer, |language| match language {
            UiLanguage::En => &["Paste 1", "40 lines/280 chars", "Image: picture.png"],
            UiLanguage::Ja => &["貼付け1", "40行/280字", "画像: picture.png"],
        });
    }
    #[test]
    fn paste_chip_uses_measured_separator_and_keeps_raw_draft() {
        let raw = "user | · → │ data";
        for observed in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(observed);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let text = format!("A{raw}Z");
                let draft = DraftSnapshot {
                    text: text.clone(),
                    cursor: 1 + raw.len(),
                    folds: vec![PasteFold {
                        id: 7,
                        start: 1,
                        end: 1 + raw.len(),
                        expanded: false,
                    }],
                    ..Default::default()
                };
                let projected = display(&draft, language);
                assert_eq!(draft.text, text);
                assert!(
                    projected
                        .text
                        .contains(crate::ui::text_width::glyphs::separator()),
                    "{}",
                    projected.text
                );
                assert!(projected.text.starts_with('A') && projected.text.ends_with('Z'));
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn paste_counts_are_complete_zero_one_and_many_messages() {
        for (lines, chars, en) in [
            (0, 0, "[Paste 7 | 0 lines/0 chars]"),
            (1, 1, "[Paste 7 | 1 line/1 char]"),
            (1, 2, "[Paste 7 | 1 line/2 chars]"),
            (2, 1, "[Paste 7 | 2 lines/1 char]"),
            (2, 42, "[Paste 7 | 2 lines/42 chars]"),
        ] {
            assert_eq!(paste_chip(UiLanguage::En, 7, lines, chars), en);
            assert_eq!(
                paste_chip(UiLanguage::Ja, 7, lines, chars),
                format!("[貼付け7 | {lines}行/{chars}字]")
            );
        }
    }
    #[test]
    fn localized_chips_project_the_same_source_caret_without_mutating_draft_or_send() {
        for language in [UiLanguage::En, UiLanguage::Ja] {
            for observed in [Some(1), Some(2), None] {
                crate::ui::text_width::set_terminal_width(observed);
                let raw = "日本語·👩‍💻\n".repeat(40);
                let text = format!("A{raw}Z{}", draft::MARKER);
                let mut draft = DraftSnapshot {
                    text,
                    cursor: 1 + raw.len(),
                    folds: vec![PasteFold {
                        id: 1,
                        start: 1,
                        end: 1 + raw.len(),
                        expanded: false,
                    }],
                    attachments: vec![UiAttachment {
                        id: "raw-id".into(),
                        path: "原文·👩‍💻.png".into(),
                        kind: "image".into(),
                        state: "raw-state".into(),
                        position: Some(2 + raw.len()),
                    }],
                    ..Default::default()
                };
                let before = serde_json::to_value(&draft).unwrap();
                let projection = display(&draft, language);
                assert!(projection.text[projection.cursor..].starts_with('Z'));
                assert!(projection.text.contains("原文·👩‍💻.png"));
                assert!(!projection.text.contains(&raw));
                for width in [80, 120] {
                    let metrics = measure(&projection.text, projection.cursor, width);
                    let visible = visible(&projection, width, 0, metrics.rows as u16);
                    let (x, y) = metrics.cursor;
                    let line = visible[y].to_string();
                    let before_z = line.split('Z').next().unwrap();
                    assert_eq!(before_z.width(), x);
                }
                assert_eq!(serde_json::to_value(&draft).unwrap(), before);
                let mut config = Config::default();
                config.ui.language = language;
                let mut ui = UiState::new(&config, std::env::temp_dir());
                assert!(crate::ui::valid_draft(&draft));
                draft.cursor = draft.text.len();
                ui.view_mut().draft = draft.clone();
                let actions = ui.event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )));
                assert!(actions.iter().any(|action| matches!(action, UiAction::Send { text, attachments, .. } if text == &draft.text && attachments == &draft.attachments)));
            }
        }
        crate::ui::text_width::set_terminal_width(None);
    }
}
