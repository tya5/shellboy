//! Pure projection of the current conversation into bounded screen chrome.
use super::ThreadView;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn header(view: &ThreadView, width: u16, selection: bool) -> String {
    if selection {
        let label = if width >= 38 {
            "表示固定（選択中） · F2/Esc解除"
        } else if width >= 22 {
            "表示固定 · F2/Esc解除"
        } else {
            "表示固定"
        };
        return fit(label, usize::from(width), false);
    }
    let width = usize::from(width);
    let phase = if view.activity.snapshot.is_some() || view.sending().is_some() {
        view.footer_phase().label()
    } else if view.thread_id.is_some() {
        "状態を確認中"
    } else {
        "入力できます"
    };
    let mut text = fit(&format!("[{phase}]"), width, false);
    let model = view.model.as_deref().unwrap_or("モデル未設定");
    let available = width.saturating_sub(text.width() + 3);
    let model_reserve = if available >= 9 {
        model.width().min(available / 2)
    } else {
        0
    };
    let title_width = (width / 2)
        .min(available.saturating_sub(model_reserve + if model_reserve > 0 { 3 } else { 0 }));
    let mut append = |value: &str, limit: usize, tail: bool| {
        let remaining = width.saturating_sub(text.width() + 3);
        if remaining >= 3 {
            let value = fit(value, remaining.min(limit), tail);
            if !value.is_empty() {
                text.push_str(" · ");
                text.push_str(&value);
            }
        }
    };
    append(&view.title, title_width, false);
    append(model, usize::MAX, false);
    append(&view.cwd.to_string_lossy(), usize::MAX, true);
    text
}

/// No filesystem observation: only settings and the worker-provided cwd are projected.
pub(super) fn welcome(
    view: &ThreadView,
    config: &crate::config::Config,
    width: u16,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::{
        style::{Modifier, Style},
        text::{Line, Span},
    };
    let cwd = if view.cwd.as_os_str().is_empty() {
        "取得待ち".to_owned()
    } else {
        fit(
            &view.cwd.to_string_lossy(),
            usize::from(width).saturating_sub("作業場所: ".width()),
            true,
        )
    };
    vec![
        Line::from(Span::styled(
            "依頼を入力してください",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "送信: {} · 改行: {}",
            config.ui.send_key,
            config.ui.newline_keys.join(" / ")
        )),
        Line::from(format!("作業場所: {cwd}")),
        Line::from("AGENTS.md: 未確認"),
        Line::from("/ でコマンド候補 · F1 操作ガイド"),
    ]
}

// Sanitize display values, preserve graphemes, and make every cut explicit.
// Source titles/models/paths remain untouched for details and full-value copy.
fn fit(text: &str, width: usize, tail: bool) -> String {
    if width == 0 {
        return String::new();
    }
    let safe = crate::presentation::sanitize_plain(text, 0).0;
    let safe = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    if safe.width() <= width {
        return safe;
    }
    let mut result = String::new();
    let mut cells = 1;
    let graphemes = safe.graphemes(true).collect::<Vec<_>>();
    if tail {
        for grapheme in graphemes.into_iter().rev() {
            if cells + grapheme.width() > width {
                break;
            }
            result.insert_str(0, grapheme);
            cells += grapheme.width();
        }
        format!("…{result}")
    } else {
        for grapheme in graphemes {
            if cells + grapheme.width() > width {
                break;
            }
            result.push_str(grapheme);
            cells += grapheme.width();
        }
        format!("{result}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{
            UiState, UiUpdate,
            activity::{ActivityPhase, ActivitySnapshot},
        },
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;

    fn frame_text(ui: &mut UiState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, ui)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            text.push('\n');
        }
        text
    }
    #[test]
    fn empty_conversation_explains_actual_bindings_cwd_and_unobserved_agents() {
        let mut config = Config::default();
        config.ui.send_key = "Ctrl+Enter".into();
        config.ui.newline_keys = vec!["Enter".into()];
        let mut ui = UiState::new(&config, PathBuf::from("/synthetic/missing-workspace"));
        let shown = frame_text(&mut ui, 100, 24);
        for expected in [
            "依頼を入力",
            "送信: Ctrl+Enter",
            "改行: Enter",
            "/synthetic/missing-workspace",
            "AGENTS.md: 未確認",
        ] {
            assert!(shown.contains(expected), "{expected}: {shown}");
        }
        assert!(!shown.contains("AGENTS.md: あり") && !shown.contains("AGENTS.md: なし"));
    }
    #[test]
    fn selection_fixed_header_is_visible_and_exit_preserves_the_source_anchor() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut ui = UiState::new(&Config::default(), PathBuf::from("/synthetic"));
        let before = ui.view().anchor.clone();
        ui.event(Event::Key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)));
        let shown = frame_text(&mut ui, 60, 20);
        assert!(shown.lines().next().unwrap().contains("表示固定"));
        assert!(shown.contains("F2/Esc解除"));
        ui.apply(UiUpdate::Status {
            thread_id: None,
            text: "background status changed".into(),
        });
        assert_eq!(ui.view().anchor, before);
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!ui.selection_mode);
        assert_eq!(ui.view().anchor, before);
        assert!(
            !frame_text(&mut ui, 60, 20)
                .lines()
                .next()
                .unwrap()
                .contains("表示固定")
        );
    }
    #[test]
    fn minimum_selection_banner_remains_visible_above_a_readonly_popup() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        for (width, height) in [(8, 5), (80, 5), (24, 10), (60, 20)] {
            let mut ui = UiState::new(&Config::default(), PathBuf::from("/synthetic"));
            for code in [KeyCode::F(1), KeyCode::F(2)] {
                ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
            }
            assert!(ui.help_visible && ui.selection_mode);
            let shown = frame_text(&mut ui, width, height);
            assert!(
                shown.lines().next().unwrap().contains("表示固定"),
                "{width}x{height}: {shown}"
            );
        }
    }
    #[test]
    fn narrow_main_screen_keeps_phase_and_minimum_input_history_areas() {
        for (width, height) in [(35, 12), (60, 20), (120, 40)] {
            let mut ui = UiState::new(&Config::default(), PathBuf::from("/synthetic"));
            ui.select_thread("t".into());
            ui.apply(UiUpdate::Activity {
                thread_id: "t".into(),
                generation: ui.view().generation,
                epoch: 1,
                observed_at: std::time::Instant::now(),
                snapshot: ActivitySnapshot {
                    phase: ActivityPhase::Recovery,
                    ..Default::default()
                },
            });
            let shown = frame_text(&mut ui, width, height);
            assert!(
                shown.lines().next().unwrap().contains("復旧待ち"),
                "{width}x{height}: {shown}"
            );
            assert!(
                shown.contains("Enter"),
                "the input's main action remains readable: {shown}"
            );
            assert!(
                ui.view().scroll.area.unwrap().height >= 3,
                "a padded source row remains available"
            );
        }
    }
    #[test]
    fn long_titles_preserve_state_and_model_and_short_titles_leave_a_path_suffix() {
        let mut ui = UiState::new(&Config::default(), PathBuf::from("/workspace/project"));
        ui.select_thread("thread".into());
        ui.view_mut().title = "長い題名とe\u{301}を含む依頼".repeat(16);
        ui.view_mut().model = Some("mock-model".into());
        ui.apply(UiUpdate::Activity {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            epoch: 1,
            observed_at: std::time::Instant::now(),
            snapshot: ActivitySnapshot {
                phase: ActivityPhase::Recovery,
                ..Default::default()
            },
        });
        let original = ui.view().title.clone();
        for width in [8, 24, 40, 60, 80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut ui))
                .unwrap();
            let text = header(ui.view(), width, false);
            assert!(text.width() <= usize::from(width), "{width}: {text}");
            assert!(text.starts_with("[復旧"));
            if width >= 40 {
                assert!(text.contains("mock-model"), "{width}: {text}");
            }
            if width >= 24 {
                assert!(text.contains('…'));
            }
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].symbol(), "[");
            assert_eq!(buffer[(0, 0)].fg, super::super::theme::TEXT);
        }
        assert_eq!(ui.view().title, original);
        ui.view_mut().title = "短い題名".into();
        ui.view_mut().cwd = PathBuf::from(format!("/{}/project", "long-directory/".repeat(20)));
        assert!(header(ui.view(), 80, false).ends_with("/project"));
        assert!(header(ui.view(), 80, false).contains("…"));
        assert!(ui.view().cwd.to_string_lossy().len() > 200);
        assert_eq!(fit("e\u{301}日本語", 3, false), "e\u{301}…");
        assert!(!fit("unsafe\u{1b}[31m\nvalue", 80, false).contains('\u{1b}'));
    }
}
