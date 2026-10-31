//! Pure projection of the current conversation into bounded screen chrome.
use super::ThreadView;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Fixed header surface; metadata occupies a separate right-aligned region.
pub(super) fn header_line(
    view: &ThreadView,
    width: u16,
    selection: bool,
    palette: super::theme::Palette,
    frozen_at: Option<&str>,
) -> ratatui::text::Line<'static> {
    use ratatui::{
        style::{Modifier, Style},
        text::{Line, Span},
    };
    if selection {
        return Line::styled(
            frozen_header_label(width, frozen_at),
            palette.header().add_modifier(Modifier::BOLD),
        );
    }
    let width = usize::from(width);
    let phase = view.footer_phase();
    let marker = if view.thread_id.is_some()
        && view.activity.snapshot.is_none()
        && view.sending().is_none()
    {
        "?".into()
    } else {
        match phase {
            super::activity::ActivityPhase::Recovery | super::activity::ActivityPhase::Failed => {
                format!("! {}", phase.column_label())
            }
            super::activity::ActivityPhase::Stopped => "■".into(),
            super::activity::ActivityPhase::Completed => "✓".into(),
            super::activity::ActivityPhase::Idle => "·".into(),
            _ => "◌".into(),
        }
    };
    let marker = fit(&marker, width, false);
    let model = view.model.as_deref().unwrap_or("");
    let available = width.saturating_sub(marker.width() + 2);
    let reserve = model.width().min(24).min(available / 2);
    let title_width = (width / 2).min(available.saturating_sub(reserve + usize::from(reserve > 0)));
    let title = fit(&view.title, title_width, false);
    let left_width = marker.width() + usize::from(!title.is_empty()) + title.width();
    let right_width = width.saturating_sub(left_width + 1);
    let mut right = fit(model, right_width.min(24), false);
    if let Some(connection) = view.connection.as_deref() {
        let remaining = right_width.saturating_sub(right.width() + 3);
        if remaining >= 3 {
            right = format!("{} · {right}", fit(connection, remaining.min(16), false));
        }
    }
    let path_width = right_width.saturating_sub(right.width() + usize::from(!right.is_empty()) * 3);
    if path_width >= 5 && !view.cwd.as_os_str().is_empty() {
        let path = fit(&view.cwd.to_string_lossy(), path_width, true);
        if !right.is_empty() {
            right.push_str(" · ");
        }
        right.push_str(&path);
    }
    let gap = width.saturating_sub(left_width + right.width());
    let attention = matches!(
        phase,
        super::activity::ActivityPhase::Recovery | super::activity::ActivityPhase::Failed
    );
    let marker_style = if attention && !palette.plain() {
        Style::default()
            .fg(super::theme::ATTENTION)
            .add_modifier(Modifier::BOLD)
    } else {
        palette.interactive()
    };
    let mut spans = vec![Span::styled(marker, marker_style)];
    if !title.is_empty() {
        spans.push(Span::styled(
            format!(" {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(right, Style::default().fg(palette.muted())));
    Line::from(spans).style(palette.header())
}

fn frozen_header_label(width: u16, frozen_at: Option<&str>) -> String {
    let time = frozen_at
        .and_then(|stamp| stamp.get(11..19))
        .filter(|time| time.is_ascii())
        .map(|time| format!("{time}Z"));
    let mut choices = Vec::new();
    if let Some(stamp) = frozen_at {
        choices.push(format!("表示固定開始 {stamp} · F2/Esc解除 · Ctrl+L更新"));
    }
    if let Some(time) = time {
        choices.push(format!("表示固定開始 {time} · F2/Esc解除 · Ctrl+L更新"));
        choices.push(format!("表示固定 {time} · F2/Esc解除"));
        choices.push(format!("表示固定 {time} F2"));
        choices.push(format!("表示固定 {time}"));
    } else {
        choices.push("表示固定 · 時刻未記録 · F2/Esc解除 · Ctrl+L更新".into());
        choices.push("表示固定 · F2/Esc解除".into());
    }
    choices.push("表示固定".into());
    let label = choices
        .into_iter()
        .find(|label| label.width() <= usize::from(width))
        .unwrap_or_default();
    fit(&label, usize::from(width), false)
}

#[cfg(test)]
fn header(view: &ThreadView, width: u16, selection: bool) -> String {
    header_line(
        view,
        width,
        selection,
        super::theme::Palette::from_capabilities(crate::config::PaletteMode::Dark, false, true),
        None,
    )
    .spans
    .iter()
    .map(|span| span.content.as_ref())
    .collect()
}

/// Two quiet footer rows. Caller owns animation and transient-notice lifetime.
pub(super) fn footer_lines(
    view: &ThreadView,
    width: u16,
    selection: bool,
    palette: super::theme::Palette,
    mut activity_spans: Vec<ratatui::text::Span<'static>>,
    notice: Option<(&str, bool)>,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::{
        style::Style,
        text::{Line, Span},
    };
    let phase = view.footer_phase();
    let mut facts = Vec::new();
    if !view.follow {
        facts.push(if view.unseen {
            "↓ new".into()
        } else {
            "↓".into()
        });
    }
    let send = view.sending();
    if let Some((send_phase, count, started)) = send {
        facts.push(format!(
            "{} {count} · {}s",
            send_phase.label(),
            started.elapsed().as_secs()
        ));
    } else if view
        .activity
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.turn_in_progress)
        && phase != super::activity::ActivityPhase::Recovery
        && let Some(elapsed) = view.activity.displayed_elapsed_ms()
    {
        facts.push(format!("{}s", elapsed / 1000));
    }
    if !view
        .activity
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.turn_in_progress)
        && let Some(work) = view.live_work.footer()
    {
        facts.push(work);
    }
    if view.pending_messages.total > 0 {
        facts.push(format!("待ち {}", view.pending_messages.total));
    }
    if view.tool_calls > 0 {
        facts.push(format!("{} tools", view.tool_calls));
    }
    if view.cancelled_calls > 0 {
        facts.push(format!("× {}", view.cancelled_calls));
    }
    if let Some(snapshot) = view.activity.snapshot.as_ref() {
        if let Some(retry) = snapshot.automatic_retry.as_ref() {
            facts.push(retry.label());
        }
        if let Some(revisit) = snapshot.revisit.as_ref() {
            facts.push(revisit.clone());
        }
        if phase.is_active()
            && let Some(reduction) = snapshot.context_reduction.as_ref()
        {
            facts.push(reduction.clone());
        }
    }
    if let Some(gap) = view.activity.receive_gap_note(std::time::Instant::now()) {
        facts.push(gap);
    }
    if view.loading {
        facts.push("…".into());
    }
    let facts = fit(
        &facts.join(" · "),
        usize::from(width).saturating_sub(
            activity_spans
                .iter()
                .map(|s| s.content.width())
                .sum::<usize>()
                + 3,
        ),
        false,
    );
    if !facts.is_empty() {
        if !activity_spans.is_empty() {
            activity_spans.push(Span::raw(" · "));
        }
        activity_spans.push(Span::styled(facts, Style::default().fg(palette.muted())));
    }
    let (hint, error) = notice.unwrap_or((if selection { "F2/Esc" } else { "F1 help" }, false));
    let notice_style = if error && !palette.plain() {
        Style::default().fg(super::theme::ATTENTION)
    } else {
        Style::default().fg(palette.muted())
    };
    vec![
        Line::from(activity_spans),
        Line::styled(fit(hint, usize::from(width), false), notice_style),
    ]
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
    let setup = crate::config_setup::report(
        config,
        view.connection.as_deref(),
        view.model.as_deref(),
        crate::config_setup::credential_set,
    );
    if !setup.missing.is_empty() {
        return setup
            .japanese(true)
            .lines()
            .map(|line| Line::from(crate::presentation::sanitize_plain(line, 0).0))
            .collect();
    }
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
        Line::from(format!(
            "AGENTS: {}",
            fit(
                &view.agents_path,
                usize::from(width).saturating_sub(8),
                true
            )
        )),
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
    fn frozen_banner_identifies_entry_and_resume_without_inventing_a_time() {
        let at = "2026-10-30T12:34:56Z";
        let wide = frozen_header_label(100, Some(at));
        assert!(wide.contains(at) && wide.contains("F2/Esc解除") && wide.contains("Ctrl+L更新"));
        let compact = frozen_header_label(40, Some(at));
        assert!(compact.contains("12:34:56Z") && compact.contains("F2/Esc解除"));
        for width in [8, 16, 24, 40, 80, 120] {
            let label = frozen_header_label(width, Some(at));
            assert!(label.width() <= usize::from(width));
            assert!(label.contains("表示固定"));
        }
        assert!(frozen_header_label(100, None).contains("時刻未記録"));
    }
    #[test]
    fn empty_conversation_explains_actual_bindings_cwd_and_unobserved_agents() {
        let mut config = Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
        config.ui.send_key = "Ctrl+Enter".into();
        config.ui.newline_keys = vec!["Enter".into()];
        let cwd = PathBuf::from("/synthetic/missing-workspace");
        let expected_agents = format!("AGENTS: {}", cwd.join("AGENTS.md").display());
        let mut ui = UiState::new(&config, cwd);
        let shown = frame_text(&mut ui, 100, 24);
        for expected in [
            "依頼を入力",
            "送信: Ctrl+Enter",
            "改行: Enter",
            "/synthetic/missing-workspace",
            expected_agents.as_str(),
        ] {
            assert!(shown.contains(expected), "{expected}: {shown}");
        }
        assert!(!shown.contains("AGENTS.md: あり") && !shown.contains("AGENTS.md: なし"));
    }
    #[test]
    fn first_run_welcome_shows_source_missing_facts_template_and_next_steps() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("chosen.toml");
        let mut config = Config::default();
        config.source = Some(crate::config::ConfigSource {
            path: path.clone(),
            present: false,
        });
        let mut ui = UiState::new(&config, root.path().into());
        let shown = frame_text(&mut ui, 150, 32);
        for expected in [
            "接続が未設定",
            "モデルが未設定",
            path.to_str().unwrap(),
            "default_connection",
            "base_url",
            "api_key_env",
            "config init",
            "config check",
            "shellboy/configuration",
        ] {
            assert!(shown.contains(expected), "{expected}: {shown}");
        }
        ui.active_view_mut().draft.text = "hello".into();
        ui.send_current(None);
        let (&revision, pending) = ui.view().pending.iter().next().unwrap();
        let draft_id = pending.snapshot.draft_id.clone();
        ui.apply(UiUpdate::Receipt {
            draft_id,
            revision,
            thread_id: None,
            status: crate::ui::ReceiptStatus::Failed,
            message: Some("configuration failure".into()),
        });
        let rejected = frame_text(&mut ui, 150, 32);
        for expected in [
            path.to_str().unwrap(),
            "config init",
            "config check",
            "接続が未設定",
        ] {
            assert!(rejected.contains(expected), "{expected}: {rejected}");
        }
        let configured = Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
        let mut ui = UiState::new(&configured, root.path().into());
        assert!(!frame_text(&mut ui, 150, 32).contains("設定が必要"));
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
            assert!(text.starts_with("! 復旧"));
            if width >= 40 {
                assert!(text.contains("mock-model"), "{width}: {text}");
            }
            if width >= 24 {
                assert!(text.contains('…'));
            }
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].symbol(), "!");
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
    #[test]
    fn header_separates_metadata_and_quiet_footer_keeps_queue_and_errors() {
        use crate::config::PaletteMode;
        use ratatui::style::{Modifier, Style};
        use ratatui::text::Span;
        let mut ui = UiState::new(&Config::default(), PathBuf::from("/long/synthetic/project"));
        ui.view_mut().title = "Important conversation".into();
        ui.view_mut().connection = Some("fixture-link".into());
        ui.view_mut().model = Some("fixture-model".into());
        ui.view_mut().pending_messages.total = 2;
        for mode in [PaletteMode::Dark, PaletteMode::Light] {
            let palette = super::super::theme::Palette::from_capabilities(mode, false, true);
            let line = header_line(ui.view(), 100, false, palette, None);
            assert_eq!(line.style, palette.header());
            assert_eq!(line.width(), 100);
            assert!(
                line.spans
                    .iter()
                    .any(|span| span.content.contains("Important")
                        && span.style.add_modifier.contains(Modifier::BOLD))
            );
            let metadata = line.spans.last().unwrap();
            assert!(metadata.content.contains("fixture-model"));
            assert!(metadata.content.contains("fixture-link"));
            assert!(metadata.content.ends_with("/project"));
            assert_eq!(metadata.style.fg, Some(palette.muted()));
            let rows = footer_lines(ui.view(), 100, false, palette, vec![Span::raw("✓")], None);
            let text = rows
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(text.contains("待ち 2") && text.contains("F1 help"));
            assert!(!text.contains("追従") && !text.contains("F6") && !text.contains("F8"));
            let rows = footer_lines(
                ui.view(),
                100,
                false,
                palette,
                vec![],
                Some(("saved operation failed", true)),
            );
            assert_eq!(
                rows[1].style,
                Style::default().fg(super::super::theme::ATTENTION)
            );
            assert!(rows[1].spans[0].content.contains("saved operation failed"));
        }
        let original = ui.view().title.clone();
        ui.view_mut().title = "unsafe\u{1b}[31m\nheader".into();
        let line = header_line(
            ui.view(),
            35,
            false,
            super::super::theme::Palette::from_capabilities(PaletteMode::Dark, false, false),
            None,
        );
        assert!(line.width() <= 35);
        assert!(
            line.spans
                .iter()
                .all(|span| !span.content.chars().any(|ch| matches!(ch, '\n' | '\u{1b}')))
        );
        assert_ne!(ui.view().title, original);
        assert!(
            ui.view().title.contains('\u{1b}'),
            "rendering must not rewrite saved source"
        );
    }
    #[test]
    fn descendant_count_stays_visible_when_other_footer_facts_fill_the_row() {
        let mut ui = UiState::new(&Config::default(), PathBuf::from("/synthetic"));
        ui.select_thread("t".into());
        ui.view_mut().loading = false;
        ui.view_mut().tool_calls = 9999;
        ui.apply(UiUpdate::Activity {
            thread_id: "t".into(),
            generation: ui.view().generation,
            epoch: 1,
            observed_at: std::time::Instant::now(),
            snapshot: ActivitySnapshot {
                phase: ActivityPhase::Running,
                turn_in_progress: true,
                revisit: Some("saved revisit metadata ".repeat(20)),
                ..Default::default()
            },
        });
        let Some(super::super::UiAction::ObserveDescendants { generation, .. }) =
            ui.descendants.observe(vec!["t".into()])
        else {
            panic!("count scope")
        };
        ui.apply(UiUpdate::Descendants {
            generation,
            read_sequence: 1,
            counts: vec![super::super::watch::DescendantCount {
                root: "t".into(),
                available: true,
                active: 2,
                recovery: 1,
                truncated: false,
                cycle: false,
            }],
            error: None,
        });
        let shown = frame_text(&mut ui, 80, 24);
        let footer = shown.lines().nth(22).unwrap();
        assert!(
            footer.contains("子孫") && footer.contains("稼働2") && footer.contains("復旧1"),
            "{footer}"
        );
    }
}
