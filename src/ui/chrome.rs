//! Pure projection of the current conversation into bounded screen chrome.
use super::ThreadView;
use crate::ui::text_width::{UnicodeWidthStr, glyphs};
use unicode_segmentation::UnicodeSegmentation;

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
            frozen_header_label(width, frozen_at, view.language),
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
                format!(
                    "! {}",
                    super::watch::phase_label(phase, view.language, false)
                )
            }
            super::activity::ActivityPhase::Stopped => glyphs::select("■", "#").into(),
            super::activity::ActivityPhase::Completed => "✓".into(),
            super::activity::ActivityPhase::Idle => glyphs::select("·", "|").into(),
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
            right = format!(
                "{}{sep}{right}",
                fit(connection, remaining.min(16), false),
                sep = glyphs::separator()
            );
        }
    }
    let path_width = right_width.saturating_sub(right.width() + usize::from(!right.is_empty()) * 3);
    if path_width >= 5 && !view.cwd.as_os_str().is_empty() {
        let path = fit(&view.cwd.to_string_lossy(), path_width, true);
        if !right.is_empty() {
            right.push_str(glyphs::separator());
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

fn frozen_header_label(
    width: u16,
    frozen_at: Option<&str>,
    language: crate::config::UiLanguage,
) -> String {
    let time = frozen_at
        .and_then(|stamp| stamp.get(11..19))
        .filter(|time| time.is_ascii())
        .map(|time| format!("{time}Z"));
    let mut choices = Vec::new();
    if let Some(stamp) = frozen_at {
        choices.push(match language {
            crate::config::UiLanguage::Ja => glyphs::formatted!(
                "表示固定開始 {stamp} · F2/Esc解除 · Ctrl+L更新",
                "表示固定開始 {stamp} | F2/Esc解除 | Ctrl+L更新"
            ),
            crate::config::UiLanguage::En => glyphs::formatted!(
                "Display frozen at {stamp} · F2/Esc resume · Ctrl+L refresh",
                "Display frozen at {stamp} | F2/Esc resume | Ctrl+L refresh"
            ),
        });
    }
    if let Some(time) = time {
        choices.push(match language {
            crate::config::UiLanguage::Ja => glyphs::formatted!(
                "表示固定開始 {time} · F2/Esc解除 · Ctrl+L更新",
                "表示固定開始 {time} | F2/Esc解除 | Ctrl+L更新"
            ),
            crate::config::UiLanguage::En => glyphs::formatted!(
                "Display frozen at {time} · F2/Esc resume · Ctrl+L refresh",
                "Display frozen at {time} | F2/Esc resume | Ctrl+L refresh"
            ),
        });
        choices.push(match language {
            crate::config::UiLanguage::Ja => glyphs::formatted!(
                "表示固定 {time} · F2/Esc解除",
                "表示固定 {time} | F2/Esc解除"
            ),
            crate::config::UiLanguage::En => glyphs::formatted!(
                "Frozen {time} · F2/Esc resume",
                "Frozen {time} | F2/Esc resume"
            ),
        });
        choices.push(match language {
            crate::config::UiLanguage::Ja => format!("表示固定 {time} F2"),
            crate::config::UiLanguage::En => format!("Frozen {time} F2"),
        });
        choices.push(match language {
            crate::config::UiLanguage::Ja => format!("表示固定 {time}"),
            crate::config::UiLanguage::En => format!("Frozen {time}"),
        });
    } else {
        choices.push(
            match language {
                crate::config::UiLanguage::Ja => glyphs::select(
                    "表示固定 · 時刻未記録 · F2/Esc解除 · Ctrl+L更新",
                    "表示固定 | 時刻未記録 | F2/Esc解除 | Ctrl+L更新",
                ),
                crate::config::UiLanguage::En => glyphs::select(
                    "Display frozen · Time not recorded · F2/Esc resume · Ctrl+L refresh",
                    "Display frozen | Time not recorded | F2/Esc resume | Ctrl+L refresh",
                ),
            }
            .into(),
        );
        choices.push(
            match language {
                crate::config::UiLanguage::Ja => {
                    glyphs::select("表示固定 · F2/Esc解除", "表示固定 | F2/Esc解除")
                }
                crate::config::UiLanguage::En => glyphs::select(
                    "Display frozen · F2/Esc resume",
                    "Display frozen | F2/Esc resume",
                ),
            }
            .into(),
        );
    }
    choices.push(super::i18n::local!(language, "表示固定", "Frozen").into());
    choices.push(super::i18n::local!(language, "固定", "Hold").into());
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

/// Saved reported usage of the Thread's latest adopted attempt plus the effective
/// `auto_at_tokens` (0 = off). Pure projection: no estimate, no clock.
#[derive(Clone, Copy, Debug)]
pub(super) struct ContextBar {
    pub usage: super::ContextUsage,
    pub auto_at: u64,
}
/// 999 -> `999`, 10200 -> `10.2K`, 140000 -> `140K`, 1200000 -> `1.2M` (floored).
pub(super) fn compact_tokens(n: u64) -> String {
    fn scaled(n: u64, unit: u64, suffix: &str) -> String {
        let tenths = n / (unit / 10);
        if tenths >= 1000 || tenths.is_multiple_of(10) {
            format!("{}{suffix}", tenths / 10)
        } else {
            format!("{}.{}{suffix}", tenths / 10, tenths % 10)
        }
    }
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        scaled(n, 1000, "K")
    } else {
        scaled(n, 1_000_000, "M")
    }
}
impl ContextBar {
    /// Input is at least 80% of an enabled threshold.
    pub(super) fn warn(&self) -> bool {
        self.auto_at > 0
            && self
                .usage
                .input_tokens
                .is_some_and(|input| u128::from(input) * 100 >= u128::from(self.auto_at) * 80)
    }
    fn variants(&self, plain: bool, language: crate::config::UiLanguage) -> Vec<String> {
        let u = &self.usage;
        let mark = if plain && self.warn() { "!" } else { "" };
        if u.input_tokens.is_none() && u.output_tokens.is_none() && u.cached_tokens.is_none() {
            return vec![format!(
                "{mark}{}{}",
                glyphs::select("↑", "^"),
                glyphs::select("—", "-")
            )];
        }
        let show = |v: Option<u64>| v.map_or(glyphs::select("—", "-").to_owned(), compact_tokens);
        let up = format!("{mark}{}{}", glyphs::select("↑", "^"), show(u.input_tokens));
        let up_limit = if self.auto_at > 0 {
            format!("{up}/{}", compact_tokens(self.auto_at))
        } else {
            up.clone()
        };
        let down = format!("{}{}", glyphs::select("↓", "v"), show(u.output_tokens));
        let cache = match (u.cached_tokens, u.input_tokens) {
            (Some(cached), Some(input)) if input > 0 => Some(match language {
                crate::config::UiLanguage::Ja => format!(
                    "キャッシュ{}%",
                    ((u128::from(cached) * 100 + u128::from(input) / 2) / u128::from(input))
                        .min(100)
                ),
                crate::config::UiLanguage::En => format!(
                    "Cache {}%",
                    ((u128::from(cached) * 100 + u128::from(input) / 2) / u128::from(input))
                        .min(100)
                ),
            }),
            _ => None,
        };
        let mut out = Vec::new();
        if let Some(cache) = cache {
            out.push(format!("{up_limit} {down} {cache}"));
        }
        out.push(format!("{up_limit} {down}"));
        out.push(up_limit.clone());
        out.push(up);
        out.dedup();
        out
    }
    /// Drop order on narrow widths: cache, output, denominator, then input.
    pub(super) fn text(
        &self,
        width: usize,
        plain: bool,
        language: crate::config::UiLanguage,
    ) -> Option<String> {
        self.variants(plain, language)
            .into_iter()
            .find(|text| text.width() <= width)
    }
}

/// Two quiet footer rows. Caller owns animation and transient-notice lifetime.
pub(super) fn footer_lines(
    view: &ThreadView,
    width: u16,
    hints: &str,
    palette: super::theme::Palette,
    mut activity_spans: Vec<ratatui::text::Span<'static>>,
    notice: Option<(&str, bool)>,
    context: Option<ContextBar>,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::{
        style::Style,
        text::{Line, Span},
    };
    let phase = view.footer_phase();
    let mut facts = Vec::new();
    if let Some(retry) = view
        .activity
        .snapshot
        .as_ref()
        .and_then(|s| s.automatic_retry.as_ref())
    {
        facts.push(retry.label_localized(view.language));
    }
    if let Some(limit) = view
        .activity
        .snapshot
        .as_ref()
        .and_then(|s| s.recovery.as_ref())
        .filter(|r| !r.resolved)
        .and_then(|r| r.limit_summary_localized(view.language))
    {
        facts.push(limit);
    }

    if let Some(reason) = view
        .activity
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.waiting_reason.as_ref())
    {
        facts.push(super::activity::context_wait_label_localized(
            reason,
            view.language,
        ));
    }
    if !view.navigation.follow() {
        facts.push(if view.navigation.unseen() {
            format!("{} new", glyphs::select("↓", "v"))
        } else {
            glyphs::select("↓", "v").into()
        });
    }
    if matches!(
        phase,
        super::activity::ActivityPhase::Completed
            | super::activity::ActivityPhase::Stopped
            | super::activity::ActivityPhase::Failed
    ) && let Some(snapshot) = view.activity.snapshot.as_ref()
        && snapshot.run_id.is_some()
    {
        if let (Some(ended), Some(offset)) = (
            snapshot.timing.ended_at.as_ref(),
            chrono::FixedOffset::east_opt(snapshot.timing.local_offset_seconds),
        ) {
            facts.push(crate::presentation::ended_text(
                Some(ended),
                chrono::Utc::now(),
                offset,
            ));
        } else {
            facts.push(
                super::activity::FooterText::EndedUnknown
                    .localized(view.language)
                    .into(),
            );
        }
    }
    let send = view.sending();
    if let Some((send_phase, count, started)) = send {
        facts.push(format!(
            "{} {count}{}{}",
            crate::presentation::messages::phase_label(send_phase, view.language),
            glyphs::separator(),
            crate::presentation::messages::Message::Elapsed {
                ms: started.elapsed().as_secs().saturating_mul(1000),
                decimal: false,
            }
            .render(view.language)
        ));
    } else if view
        .activity
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.turn_in_progress)
        && phase != super::activity::ActivityPhase::Recovery
        && let Some(elapsed) = view
            .activity
            .phase_time_note_localized(std::time::Instant::now(), view.language)
    {
        facts.push(elapsed);
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
    if let Some(secondary) = view
        .activity
        .secondary_note_localized(std::time::Instant::now(), view.language)
    {
        facts.push(secondary);
    }
    if view.pending_messages.total > 0 {
        facts.push(match view.language {
            crate::config::UiLanguage::Ja => format!("待ち {}", view.pending_messages.total),
            crate::config::UiLanguage::En => format!("Pending {}", view.pending_messages.total),
        });
    }
    if view.tool_calls > 0 {
        facts.push(format!("{} tools", view.tool_calls));
    }
    if view.cancelled_calls > 0 {
        facts.push(format!(
            "{} {}",
            glyphs::select("×", "x"),
            view.cancelled_calls
        ));
    }
    if let Some(snapshot) = view.activity.snapshot.as_ref() {
        if let Some(revisit) = snapshot.revisit.as_ref() {
            facts.push(revisit.clone());
        }
        if phase.is_active()
            && let Some(reduction) = snapshot.context_reduction.as_ref()
        {
            facts.push(reduction.clone());
        }
    }
    if let Some(gap) = view
        .activity
        .receive_gap_note_localized(std::time::Instant::now(), view.language)
    {
        facts.push(gap);
    }
    if view.navigation.is_loading() {
        facts.push(glyphs::ellipsis().into());
    }
    let facts = fit(
        &facts.join(glyphs::separator()),
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
            activity_spans.push(Span::raw(glyphs::separator()));
        }
        activity_spans.push(Span::styled(facts, Style::default().fg(palette.muted())));
    }
    if let Some(bar) = context {
        let used: usize = activity_spans.iter().map(|s| s.content.width()).sum();
        let room = usize::from(width).saturating_sub(used + if used > 0 { 3 } else { 0 });
        if let Some(text) = bar.text(room, palette.plain(), view.language) {
            if used > 0 {
                activity_spans.push(Span::raw(glyphs::separator()));
            }
            let style = if bar.warn() && !palette.plain() {
                Style::default().fg(super::theme::ATTENTION)
            } else {
                Style::default().fg(palette.muted())
            };
            activity_spans.push(Span::styled(text, style));
        }
    }
    let (hint, error) = notice.unwrap_or((hints, false));
    let navigation_hint = (view.navigation.is_loading()
        && view.navigation.request().is_some()
        && !error)
        .then(|| match view.language {
            crate::config::UiLanguage::Ja => format!("履歴読込中{}{hint}", glyphs::separator()),
            crate::config::UiLanguage::En => {
                format!("Loading history{}{hint}", glyphs::separator())
            }
        });
    let hint = navigation_hint.as_deref().unwrap_or(hint);
    let notice_style = if error && !palette.plain() {
        Style::default().fg(super::theme::ATTENTION)
    } else {
        Style::default().fg(palette.muted())
    };
    vec![
        Line::from(activity_spans),
        Line::styled(fit(hint, usize::from(width), false), notice_style).alignment(
            if notice.is_some() {
                ratatui::layout::Alignment::Left
            } else {
                ratatui::layout::Alignment::Right
            },
        ),
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
    let language = view.language;
    let cwd_label = super::i18n::local!(language, "作業場所: ", "Working folder: ");
    let cwd = if view.cwd.as_os_str().is_empty() {
        super::i18n::local!(language, "取得待ち", "Pending").to_owned()
    } else {
        fit(
            &view.cwd.to_string_lossy(),
            usize::from(width).saturating_sub(cwd_label.width()),
            true,
        )
    };
    let keys = match language {
        crate::config::UiLanguage::Ja => glyphs::formatted!(
            "送信: {} · 改行: {}",
            "送信: {} | 改行: {}",
            config.ui.send_key,
            config.ui.newline_keys.join(" / ")
        ),
        crate::config::UiLanguage::En => glyphs::formatted!(
            "Send: {} · Newline: {}",
            "Send: {} | Newline: {}",
            config.ui.send_key,
            config.ui.newline_keys.join(" / ")
        ),
    };
    let commands = match (language, width >= 55) {
        (crate::config::UiLanguage::Ja, true) => glyphs::select(
            "/ コマンド · /image 画像を添付 · F1 操作ガイド",
            "/ コマンド | /image 画像を添付 | F1 操作ガイド",
        ),
        (crate::config::UiLanguage::Ja, false) => {
            glyphs::select("/ コマンド · F1 操作ガイド", "/ コマンド | F1 操作ガイド")
        }
        (crate::config::UiLanguage::En, true) => glyphs::select(
            "/ Commands · /image Attach image · F1 Guide",
            "/ Commands | /image Attach image | F1 Guide",
        ),
        (crate::config::UiLanguage::En, false) => {
            glyphs::select("/ Commands · F1 Guide", "/ Commands | F1 Guide")
        }
    };
    vec![
        Line::from(Span::styled(
            super::i18n::local!(language, "依頼を入力してください", "Enter a request"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(keys),
        Line::from(match language {
            crate::config::UiLanguage::Ja => format!("作業場所: {cwd}"),
            crate::config::UiLanguage::En => format!("Working folder: {cwd}"),
        }),
        Line::from(format!(
            "AGENTS: {}",
            fit(
                &view.agents_path,
                usize::from(width).saturating_sub(8),
                true
            )
        )),
        Line::from(commands),
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
        format!("{}{result}", glyphs::ellipsis())
    } else {
        for grapheme in graphemes {
            if cells + grapheme.width() > width {
                break;
            }
            result.push_str(grapheme);
            cells += grapheme.width();
        }
        format!("{result}{}", glyphs::ellipsis())
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
    fn localized_frozen_banner_and_cache_keep_width_and_symbols() {
        use crate::config::UiLanguage::{En, Ja};
        use crate::ui::text_width;
        let at = "2026-10-30T12:34:56Z";
        for policy in [Some(1), Some(2), None] {
            text_width::set_terminal_width(policy);
            for language in [En, Ja] {
                let wide = frozen_header_label(120, Some(at), language);
                assert!(wide.contains(at));
                assert!(wide.contains("F2/Esc"));
                assert!(wide.contains("Ctrl+L"));
                for width in [8, 16, 24, 40, 80, 120] {
                    assert!(
                        frozen_header_label(width, Some(at), language).width()
                            <= usize::from(width)
                    );
                }
                let cache = ContextBar {
                    usage: crate::ui::ContextUsage {
                        input_tokens: Some(5),
                        output_tokens: None,
                        cached_tokens: Some(1),
                    },
                    auto_at: 100,
                }
                .text(80, false, language)
                .unwrap();
                assert!(cache.contains(if language == Ja {
                    "キャッシュ20%"
                } else {
                    "Cache 20%"
                }));
            }
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn frozen_banner_identifies_entry_and_resume_without_inventing_a_time() {
        let at = "2026-10-30T12:34:56Z";
        let wide = frozen_header_label(100, Some(at), crate::config::UiLanguage::Ja);
        assert!(wide.contains(at) && wide.contains("F2/Esc解除") && wide.contains("Ctrl+L更新"));
        let compact = frozen_header_label(40, Some(at), crate::config::UiLanguage::Ja);
        assert!(compact.contains("12:34:56Z") && compact.contains("F2/Esc解除"));
        for width in [8, 16, 24, 40, 80, 120] {
            let label = frozen_header_label(width, Some(at), crate::config::UiLanguage::Ja);
            assert!(label.width() <= usize::from(width));
            assert!(label.contains("表示固定"));
        }
        assert!(
            frozen_header_label(100, None, crate::config::UiLanguage::Ja).contains("時刻未記録")
        );
    }
    #[test]
    fn empty_conversation_uses_measured_separators_without_changing_bindings() {
        use crate::ui::text_width;
        let mut config = Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
        config.ui.language = crate::config::UiLanguage::Ja;
        config.ui.send_key = "Ctrl+Enter".into();
        config.ui.newline_keys = vec!["Enter".into()];
        for (policy, separator) in [(Some(1), " · "), (Some(2), " | "), (None, " | ")] {
            text_width::set_terminal_width(policy);
            for width in [80, 120] {
                let mut ui = UiState::new(&config, PathBuf::from("/synthetic"));
                let shown = frame_text(&mut ui, width, 24);
                assert!(
                    shown.contains(&format!("送信: Ctrl+Enter{separator}改行: Enter")),
                    "{policy:?}: {shown}"
                );
                assert!(
                    shown.contains(&format!("/ コマンド{separator}/image")),
                    "{policy:?}: {shown}"
                );
                assert_eq!(config.ui.send_key, "Ctrl+Enter");
            }
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn english_welcome_keeps_paths_and_key_bindings() {
        let config = Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
        let mut ui = UiState::new(&config, PathBuf::from("/synthetic/日本語"));
        let shown = frame_text(&mut ui, 120, 24);
        assert!(shown.contains("Enter a request"), "{shown}");
        assert!(shown.contains("Send:"), "{shown}");
        assert!(
            shown.contains("Working folder: /synthetic/日本語"),
            "{shown}"
        );
        assert!(shown.contains("/image Attach image"), "{shown}");
    }

    #[test]
    fn empty_conversation_explains_actual_bindings_cwd_and_unobserved_agents() {
        let mut config = Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
        config.ui.language = crate::config::UiLanguage::Ja;
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
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut ui = UiState::new(&config, PathBuf::from("/synthetic"));
        let before = ui.view().navigation.anchor.clone();
        ui.event(Event::Key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)));
        let shown = frame_text(&mut ui, 60, 20);
        assert!(shown.lines().next().unwrap().contains("表示固定"));
        assert!(shown.contains("F2/Esc解除"));
        ui.apply(UiUpdate::Status {
            thread_id: None,
            text: "background status changed".into(),
        });
        assert_eq!(ui.view().navigation.anchor, before);
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!ui.selection_mode);
        assert_eq!(ui.view().navigation.anchor, before);
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
            let mut config = Config::default();
            config.ui.language = crate::config::UiLanguage::Ja;
            let mut ui = UiState::new(&config, PathBuf::from("/synthetic"));
            for code in [KeyCode::F(1), KeyCode::F(2)] {
                ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
            }
            assert!(ui.help_visible && ui.selection_mode);
            let shown = frame_text(&mut ui, width, height);
            assert!(
                shown.lines().next().unwrap().contains(if width < 10 {
                    "固定"
                } else {
                    "表示固定"
                }),
                "{width}x{height}: {shown}"
            );
        }
    }
    #[test]
    fn narrow_main_screen_keeps_phase_and_minimum_input_history_areas() {
        for (width, height) in [(35, 12), (60, 20), (120, 40)] {
            let mut config = Config::default();
            config.ui.language = crate::config::UiLanguage::Ja;
            let mut ui = UiState::new(&config, PathBuf::from("/synthetic"));
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
                ui.view().navigation.scroll.area.unwrap().height >= 3,
                "a padded source row remains available"
            );
        }
    }
    #[test]
    fn long_titles_preserve_state_and_model_and_short_titles_leave_a_path_suffix() {
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut ui = UiState::new(&config, PathBuf::from("/workspace/project"));
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
                assert!(text.contains('.'));
            }
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].symbol(), " ");
            assert_eq!(buffer[(1, 0)].symbol(), "!");
            assert_eq!(buffer[(width - 1, 0)].symbol(), " ");
        }
        assert_eq!(ui.view().title, original);
        ui.view_mut().title = "短い題名".into();
        ui.view_mut().cwd = PathBuf::from(format!("/{}/project", "long-directory/".repeat(20)));
        assert!(header(ui.view(), 80, false).ends_with("/project"));
        assert!(header(ui.view(), 80, false).contains("."));
        assert!(ui.view().cwd.to_string_lossy().len() > 200);
        assert_eq!(fit("e\u{301}日本語", 3, false), "e\u{301}.");
        assert!(!fit("unsafe\u{1b}[31m\nvalue", 80, false).contains('\u{1b}'));
    }
    #[test]
    fn header_separates_metadata_and_quiet_footer_keeps_queue_and_errors() {
        use crate::config::PaletteMode;
        use ratatui::style::{Modifier, Style};
        use ratatui::text::Span;
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut ui = UiState::new(&config, PathBuf::from("/long/synthetic/project"));
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
            let rows = footer_lines(
                ui.view(),
                100,
                "F1",
                palette,
                vec![Span::raw("✓")],
                None,
                None,
            );
            let text = rows
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(text.contains("待ち 2") && text.contains("F1"));
            assert!(!text.contains("追従") && !text.contains("F6") && !text.contains("F8"));
            let rows = footer_lines(
                ui.view(),
                100,
                "F1",
                palette,
                vec![],
                Some(("saved operation failed", true)),
                None,
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
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut ui = UiState::new(&config, PathBuf::from("/synthetic"));
        ui.select_thread("t".into());
        ui.view_mut().navigation.set_loading(false);
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

#[cfg(test)]
mod context_bar_tests {
    use super::*;
    use crate::ui::ContextUsage;
    fn bar(input: Option<u64>, out: Option<u64>, cached: Option<u64>, auto_at: u64) -> ContextBar {
        ContextBar {
            usage: ContextUsage {
                input_tokens: input,
                output_tokens: out,
                cached_tokens: cached,
            },
            auto_at,
        }
    }
    #[test]
    fn token_units() {
        let shown: Vec<_> = [0, 999, 1000, 10_200, 20_000, 140_000, 999_999, 1_200_000]
            .map(compact_tokens)
            .into();
        assert_eq!(
            shown,
            ["0", "999", "1K", "10.2K", "20K", "140K", "999K", "1.2M"]
        );
    }
    #[test]
    fn known_unknown_disabled_and_narrow() {
        let known = bar(Some(140_000), Some(20_000), Some(120_400), 200_000);
        assert_eq!(
            known
                .text(80, false, crate::config::UiLanguage::Ja)
                .unwrap(),
            "^140K/200K v20K キャッシュ86%"
        );
        // Drop order: cache, output, denominator, input.
        assert_eq!(
            known
                .text(18, false, crate::config::UiLanguage::Ja)
                .unwrap(),
            "^140K/200K v20K"
        );
        assert_eq!(
            known
                .text(14, false, crate::config::UiLanguage::Ja)
                .unwrap(),
            "^140K/200K"
        );
        assert_eq!(
            known.text(9, false, crate::config::UiLanguage::Ja).unwrap(),
            "^140K"
        );
        assert_eq!(known.text(4, false, crate::config::UiLanguage::Ja), None);
        // Disabled threshold hides the denominator; unknown cache hides the percentage.
        assert_eq!(
            bar(Some(5), Some(7), None, 0)
                .text(80, false, crate::config::UiLanguage::Ja)
                .unwrap(),
            "^5 v7"
        );
        assert_eq!(
            bar(Some(5), None, Some(1), 100)
                .text(80, false, crate::config::UiLanguage::Ja)
                .unwrap(),
            "^5/100 v- キャッシュ20%"
        );
        assert_eq!(
            bar(None, None, None, 200_000)
                .text(80, false, crate::config::UiLanguage::Ja)
                .unwrap(),
            "^-"
        );
        assert_eq!(
            bar(None, Some(3), None, 0)
                .text(80, false, crate::config::UiLanguage::Ja)
                .unwrap(),
            "^- v3"
        );
    }
    #[test]
    fn eighty_percent_warns_with_color_or_a_plain_mark() {
        let ok = bar(Some(159_999), None, None, 200_000);
        let hot = bar(Some(160_000), None, None, 200_000);
        assert!(!ok.warn() && hot.warn());
        assert!(!bar(Some(999_999), None, None, 0).warn());
        assert_eq!(
            hot.text(80, true, crate::config::UiLanguage::Ja).unwrap(),
            "!^160K/200K v-"
        );
        assert_eq!(
            ok.text(80, true, crate::config::UiLanguage::Ja).unwrap(),
            "^159K/200K v-"
        );
        use ratatui::style::Color;
        let view = crate::ui::ThreadView::new(&crate::config::Config::default(), "/".into());
        let rich = crate::ui::theme::Palette::from_capabilities(
            crate::config::PaletteMode::Dark,
            false,
            true,
        );
        let line = footer_lines(&view, 80, "", rich, vec![], None, Some(hot));
        let span = line[0].spans.last().unwrap();
        assert_eq!(span.style.fg, Some(crate::ui::theme::ATTENTION));
        assert!(!span.content.contains('!'));
        let plain = crate::ui::theme::Palette::from_capabilities(
            crate::config::PaletteMode::Dark,
            true,
            false,
        );
        let line = footer_lines(&view, 80, "", plain, vec![], None, Some(hot));
        let span = line[0].spans.last().unwrap();
        assert!(span.content.starts_with('!'));
        assert_ne!(span.style.fg, Some(Color::LightRed));
    }
}

#[cfg(test)]
mod terminal_time_tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{
            activity::{ActivityPhase, ActivitySnapshot, SavedTiming},
            view::ThreadView,
        },
    };
    #[test]
    fn ended_footer_uses_saved_date_only_for_terminal_phases() {
        let mut view = ThreadView::new(&Config::default(), std::path::PathBuf::from("/"));
        let at = chrono::DateTime::parse_from_rfc3339("2020-01-02T03:04:00Z").unwrap();
        for phase in [
            ActivityPhase::Completed,
            ActivityPhase::Stopped,
            ActivityPhase::Failed,
            ActivityPhase::AwaitingModel,
        ] {
            view.activity.accept(
                1,
                ActivitySnapshot {
                    run_id: Some("saved".into()),
                    phase,
                    timing: SavedTiming {
                        ended_at: Some(at),
                        local_offset_seconds: 9 * 3600,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let lines = footer_lines(
                &view,
                200,
                "F1",
                crate::ui::theme::Palette::from_capabilities(
                    crate::config::PaletteMode::Dark,
                    false,
                    true,
                ),
                vec![],
                None,
                None,
            );
            let text = lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.as_ref())
                .collect::<String>();
            assert_eq!(
                text.contains("2020-01-02 12:04"),
                phase != ActivityPhase::AwaitingModel,
                "{text}"
            );
        }
    }
}

#[cfg(test)]
mod context_wait_footer_tests {
    use super::*;
    #[test]
    fn footer_shows_saved_boundary_owner_and_removes_it_on_fresh_empty_snapshot() {
        let mut config = crate::config::Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut view = crate::ui::view::ThreadView::new(&config, std::path::PathBuf::from("/"));
        let reason = crate::protocol::RunWaitReason {
            kind: "context_edit_boundary".into(),
            operation_id: "fixture".into(),
            action: "summary".into(),
            accepted_at: "saved".into(),
            operation_count: 1,
        };
        let palette = crate::ui::theme::Palette::from_capabilities(
            crate::config::PaletteMode::Dark,
            false,
            true,
        );
        view.activity.accept(
            1,
            crate::ui::activity::ActivitySnapshot {
                phase: crate::activity::ActivityPhase::Running,
                run_id: Some("r".into()),
                waiting_reason: Some(reason),
                ..Default::default()
            },
        );
        let text = footer_lines(&view, 140, "", palette, vec![], None, None)
            .iter()
            .flat_map(|l| &l.spans)
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.contains("会話要約の完了待ち"), "{text}");
        view.activity.accept(
            2,
            crate::ui::activity::ActivitySnapshot {
                phase: crate::activity::ActivityPhase::AwaitingModel,
                run_id: Some("r".into()),
                ..Default::default()
            },
        );
        let text = footer_lines(&view, 140, "", palette, vec![], None, None)
            .iter()
            .flat_map(|l| &l.spans)
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(!text.contains("会話要約の完了待ち"), "{text}");
    }
}
