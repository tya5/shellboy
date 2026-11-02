//! Complete UI messages, separate from CLI JSON and model/provider payloads.
use crate::{config::UiLanguage, ui::CopyScope};
use std::fmt::Display;
pub(super) fn cli_stopped(language: UiLanguage, error: impl Display) -> String {
    match language {
        UiLanguage::Ja => {
            format!("CLI受付が停止しました。本体を再起動して復旧してください: {error}")
        }
        UiLanguage::En => format!("CLI intake stopped. Restart Shellboy to recover: {error}"),
    }
}
pub(super) fn settings_changed(language: UiLanguage, connection: &str, model: &str) -> String {
    match language {
        UiLanguage::Ja => format!("設定を変更しました | {connection} / {model}"),
        UiLanguage::En => format!("Settings changed | {connection} / {model}"),
    }
}
pub(super) fn not_accepted(language: UiLanguage, title: &str, error: impl Display) -> String {
    match language {
        UiLanguage::Ja => format!("未受付 | {title} | {error}"),
        UiLanguage::En => format!("Not accepted | {title} | {error}"),
    }
}
pub(super) fn attachments_reloaded(language: UiLanguage, thread_id: &str) -> String {
    match language {
        UiLanguage::Ja => format!("添付を明示再読込み: {thread_id}（API送信なし）"),
        UiLanguage::En => {
            format!("Attachments explicitly reloaded: {thread_id} (nothing sent to the API)")
        }
    }
}
pub(super) fn accepted(language: UiLanguage, title: &str) -> String {
    match language {
        UiLanguage::Ja => format!("{title}を受け付けました"),
        UiLanguage::En => format!("Accepted: {title}"),
    }
}
pub(super) fn not_applied(language: UiLanguage, title: &str, error: impl Display) -> String {
    match language {
        UiLanguage::Ja => format!("{title}は未適用 | {error}"),
        UiLanguage::En => format!("{title} not applied | {error}"),
    }
}
pub(super) fn save_failed(language: UiLanguage, error: impl Display) -> String {
    match language {
        UiLanguage::Ja => format!(
            "正本DBの保存に失敗しました。新規実行を停止し資源を回収しています。下書き本文は画面に保持しています: {error}"
        ),
        UiLanguage::En => format!(
            "Saving the canonical database failed. New execution stopped; collecting resources. Draft text remains on screen: {error}"
        ),
    }
}
pub(super) fn copied(
    language: UiLanguage,
    scope: CopyScope,
    characters: usize,
    terminal_request: bool,
) -> String {
    match (language, scope, terminal_request, characters) {
        (UiLanguage::Ja, CopyScope::OriginalPage { has_more: true }, false, _) => {
            format!("原文の現在ページ（続きあり）をコピーしました（{characters}文字）")
        }
        (UiLanguage::En, CopyScope::OriginalPage { has_more: true }, false, 1) => {
            format!("Copied the current original page (more available) ({characters} character)")
        }
        (UiLanguage::En, CopyScope::OriginalPage { has_more: true }, false, _) => {
            format!("Copied the current original page (more available) ({characters} characters)")
        }
        (UiLanguage::Ja, CopyScope::OriginalPage { has_more: true }, true, _) => format!(
            "OSC52: 原文の現在ページ（続きあり）のコピー要求を端末へ送りました（{characters}文字・完了未確認）"
        ),
        (UiLanguage::En, CopyScope::OriginalPage { has_more: true }, true, 1) => format!(
            "OSC52: completion unconfirmed for copying the current original page (more available) ({characters} character). Request sent."
        ),
        (UiLanguage::En, CopyScope::OriginalPage { has_more: true }, true, _) => format!(
            "OSC52: completion unconfirmed for copying the current original page (more available) ({characters} characters). Request sent."
        ),
        (UiLanguage::Ja, CopyScope::OriginalPage { has_more: false }, false, _) => {
            format!("原文の現在ページ（このページのみ）をコピーしました（{characters}文字）")
        }
        (UiLanguage::En, CopyScope::OriginalPage { has_more: false }, false, 1) => {
            format!("Copied the current original page (this page only) ({characters} character)")
        }
        (UiLanguage::En, CopyScope::OriginalPage { has_more: false }, false, _) => {
            format!("Copied the current original page (this page only) ({characters} characters)")
        }
        (UiLanguage::Ja, CopyScope::OriginalPage { has_more: false }, true, _) => format!(
            "OSC52: 原文の現在ページ（このページのみ）のコピー要求を端末へ送りました（{characters}文字・完了未確認）"
        ),
        (UiLanguage::En, CopyScope::OriginalPage { has_more: false }, true, 1) => format!(
            "OSC52: completion unconfirmed for copying the current original page (this page only) ({characters} character). Request sent."
        ),
        (UiLanguage::En, CopyScope::OriginalPage { has_more: false }, true, _) => format!(
            "OSC52: completion unconfirmed for copying the current original page (this page only) ({characters} characters). Request sent."
        ),
        (UiLanguage::Ja, CopyScope::ManagementPage { has_more: true }, false, _) => {
            format!("管理詳細の現在ページ（続きあり）をコピーしました（{characters}文字）")
        }
        (UiLanguage::En, CopyScope::ManagementPage { has_more: true }, false, 1) => format!(
            "Copied the current management-detail page (more available) ({characters} character)"
        ),
        (UiLanguage::En, CopyScope::ManagementPage { has_more: true }, false, _) => format!(
            "Copied the current management-detail page (more available) ({characters} characters)"
        ),
        (UiLanguage::Ja, CopyScope::ManagementPage { has_more: true }, true, _) => format!(
            "OSC52: 管理詳細の現在ページ（続きあり）のコピー要求を端末へ送りました（{characters}文字・完了未確認）"
        ),
        (UiLanguage::En, CopyScope::ManagementPage { has_more: true }, true, 1) => format!(
            "OSC52: completion unconfirmed for copying the current management-detail page (more available) ({characters} character). Request sent."
        ),
        (UiLanguage::En, CopyScope::ManagementPage { has_more: true }, true, _) => format!(
            "OSC52: completion unconfirmed for copying the current management-detail page (more available) ({characters} characters). Request sent."
        ),
        (UiLanguage::Ja, CopyScope::ManagementPage { has_more: false }, false, _) => {
            format!("管理詳細の現在ページ（このページのみ）をコピーしました（{characters}文字）")
        }
        (UiLanguage::En, CopyScope::ManagementPage { has_more: false }, false, 1) => format!(
            "Copied the current management-detail page (this page only) ({characters} character)"
        ),
        (UiLanguage::En, CopyScope::ManagementPage { has_more: false }, false, _) => format!(
            "Copied the current management-detail page (this page only) ({characters} characters)"
        ),
        (UiLanguage::Ja, CopyScope::ManagementPage { has_more: false }, true, _) => format!(
            "OSC52: 管理詳細の現在ページ（このページのみ）のコピー要求を端末へ送りました（{characters}文字・完了未確認）"
        ),
        (UiLanguage::En, CopyScope::ManagementPage { has_more: false }, true, 1) => format!(
            "OSC52: completion unconfirmed for copying the current management-detail page (this page only) ({characters} character). Request sent."
        ),
        (UiLanguage::En, CopyScope::ManagementPage { has_more: false }, true, _) => format!(
            "OSC52: completion unconfirmed for copying the current management-detail page (this page only) ({characters} characters). Request sent."
        ),
        (UiLanguage::Ja, CopyScope::Value, false, _) => {
            format!("項目の完全な値をコピーしました（{characters}文字）")
        }
        (UiLanguage::En, CopyScope::Value, false, 1) => {
            format!("Copied the complete field value ({characters} character)")
        }
        (UiLanguage::En, CopyScope::Value, false, _) => {
            format!("Copied the complete field value ({characters} characters)")
        }
        (UiLanguage::Ja, CopyScope::Value, true, _) => format!(
            "OSC52: 項目の完全な値のコピー要求を端末へ送りました（{characters}文字・完了未確認）"
        ),
        (UiLanguage::En, CopyScope::Value, true, 1) => format!(
            "OSC52: completion unconfirmed for copying the complete field value ({characters} character). Request sent."
        ),
        (UiLanguage::En, CopyScope::Value, true, _) => format!(
            "OSC52: completion unconfirmed for copying the complete field value ({characters} characters). Request sent."
        ),
        (UiLanguage::Ja, CopyScope::Body, false, _) => {
            format!("本文全体をコピーしました（{characters}文字）")
        }
        (UiLanguage::En, CopyScope::Body, false, 1) => {
            format!("Copied the complete body ({characters} character)")
        }
        (UiLanguage::En, CopyScope::Body, false, _) => {
            format!("Copied the complete body ({characters} characters)")
        }
        (UiLanguage::Ja, CopyScope::Body, true, _) => {
            format!("OSC52: 本文全体のコピー要求を端末へ送りました（{characters}文字・完了未確認）")
        }
        (UiLanguage::En, CopyScope::Body, true, 1) => format!(
            "OSC52: completion unconfirmed for copying the complete body ({characters} character). Request sent."
        ),
        (UiLanguage::En, CopyScope::Body, true, _) => format!(
            "OSC52: completion unconfirmed for copying the complete body ({characters} characters). Request sent."
        ),
        (UiLanguage::Ja, CopyScope::CodeBlock { index }, false, _) => format!(
            "コードの塊 {index}をコピーしました（{characters}文字）",
            index = index.saturating_add(1)
        ),
        (UiLanguage::En, CopyScope::CodeBlock { index }, false, 1) => format!(
            "Copied code block {index} ({characters} character)",
            index = index.saturating_add(1)
        ),
        (UiLanguage::En, CopyScope::CodeBlock { index }, false, _) => format!(
            "Copied code block {index} ({characters} characters)",
            index = index.saturating_add(1)
        ),
        (UiLanguage::Ja, CopyScope::CodeBlock { index }, true, _) => format!(
            "OSC52: コードの塊 {index}のコピー要求を端末へ送りました（{characters}文字・完了未確認）",
            index = index.saturating_add(1)
        ),
        (UiLanguage::En, CopyScope::CodeBlock { index }, true, 1) => format!(
            "OSC52: completion unconfirmed for copying code block {index} ({characters} character). Request sent.",
            index = index.saturating_add(1)
        ),
        (UiLanguage::En, CopyScope::CodeBlock { index }, true, _) => format!(
            "OSC52: completion unconfirmed for copying code block {index} ({characters} characters). Request sent.",
            index = index.saturating_add(1)
        ),
    }
}

pub(super) fn preview_notice(
    language: UiLanguage,
    observed_at: chrono::DateTime<chrono::Utc>,
    notices: &[&str],
) -> String {
    let time = observed_at.format("%Y-%m-%d %H:%M:%S");
    let extra = if notices.is_empty() {
        String::new()
    } else {
        let separator = match language {
            UiLanguage::Ja => "。",
            UiLanguage::En => " ",
        };
        format!(" | {}", notices.join(separator))
    };
    match language {
        UiLanguage::Ja => format!("取得 {time} UTC | 閉じて開くと再取得{extra}"),
        UiLanguage::En => format!("Captured {time} UTC | Close and reopen to refresh.{extra}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::i18n;
    #[test]
    fn frontend_copy_and_settings_diagnostics_render_in_the_selected_language() {
        use crate::ui::{
            UiState, UiUpdate, draw,
            text_width::{self, UnicodeWidthStr},
        };
        use ratatui::{Terminal, backend::TestBackend};
        for policy in [Some(1), Some(2), None] {
            text_width::set_terminal_width(policy);
            for width in [80, 120] {
                for language in [UiLanguage::En, UiLanguage::Ja] {
                    for message in [
                        copied(language, CopyScope::Body, 1, true),
                        settings_changed(language, "CONNECTION_ID_42", "MODEL_ID_42"),
                    ] {
                        let mut config = crate::config::Config::default();
                        config.ui.language = language;
                        let mut ui = UiState::new(&config, std::path::PathBuf::new());
                        let copy = message.starts_with("OSC52");
                        ui.apply(UiUpdate::Status {
                            thread_id: None,
                            text: message,
                        });
                        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
                        let buffer = terminal.backend().buffer();
                        let mut shown = String::new();
                        for y in 0..30 {
                            let mut x = 0;
                            while x < width {
                                let symbol = buffer[(x, y)].symbol();
                                shown.push_str(symbol);
                                x += symbol.width().max(1) as u16;
                            }
                            shown.push('\n');
                        }
                        if copy {
                            assert!(
                                shown.contains(i18n::local!(
                                    language,
                                    "完了未確認",
                                    "completion unconfirmed"
                                )),
                                "{width} {language:?}: {shown}"
                            );
                        } else {
                            assert!(shown.contains("CONNECTION_ID_42"));
                            assert!(shown.contains("MODEL_ID_42"));
                        }
                    }
                }
            }
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn complete_messages_preserve_raw_values_and_copy_scope_counts() {
        let raw = "RAW 原文 id/path provider-error";
        for language in [UiLanguage::En, UiLanguage::Ja] {
            for value in [
                cli_stopped(language, raw),
                not_accepted(language, raw, raw),
                not_applied(language, raw, raw),
                save_failed(language, raw),
            ] {
                assert!(value.ends_with(raw));
            }
            assert_eq!(settings_changed(language, raw, raw).matches(raw).count(), 2);
            assert!(attachments_reloaded(language, raw).contains(raw));
            assert!(accepted(language, raw).contains(raw));
            for scope in [
                CopyScope::OriginalPage { has_more: true },
                CopyScope::OriginalPage { has_more: false },
                CopyScope::ManagementPage { has_more: true },
                CopyScope::ManagementPage { has_more: false },
                CopyScope::Value,
                CopyScope::Body,
                CopyScope::CodeBlock { index: 0 },
                CopyScope::CodeBlock { index: usize::MAX },
            ] {
                for count in [0, 1, 2, 1_000_000] {
                    let os = copied(language, scope, count, false);
                    let terminal = copied(language, scope, count, true);
                    assert!(os.contains(&count.to_string()));
                    assert!(terminal.contains(&count.to_string()));
                    assert!(terminal.contains(i18n::local!(
                        language,
                        "完了未確認",
                        "completion unconfirmed"
                    )));
                    assert!(!os.contains("OSC52"));
                    if language == UiLanguage::En {
                        assert!(os.contains(if count == 1 {
                            "(1 character)"
                        } else {
                            "characters)"
                        }));
                    }
                }
            }
        }
        for count in [0, 1, 2, 1_000_000] {
            let value = super::super::tool_group_notice(None, count, UiLanguage::En);
            if count == 0 {
                assert_eq!(value, "No remaining unexecuted calls");
            } else {
                assert!(value.contains(&count.to_string()));
                assert!(value.ends_with(if count == 1 {
                    "tool call"
                } else {
                    "tool calls"
                }));
            }
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn copy_notifications_use_ascii_english_complete_sentences_and_exact_japanese() {
        for scope in [
            CopyScope::OriginalPage { has_more: true },
            CopyScope::OriginalPage { has_more: false },
            CopyScope::ManagementPage { has_more: true },
            CopyScope::ManagementPage { has_more: false },
            CopyScope::Body,
            CopyScope::Value,
            CopyScope::CodeBlock { index: 0 },
            CopyScope::CodeBlock { index: usize::MAX },
        ] {
            for count in [0, 1, 2, 1_000_000] {
                for terminal in [false, true] {
                    let english = copied(UiLanguage::En, scope, count, terminal);
                    assert!(english.is_ascii(), "{english}");
                    assert_eq!(english.contains("Request sent."), terminal);
                    assert_eq!(english.contains("completion unconfirmed"), terminal);
                }
            }
        }
        for (language, os, terminal) in [
            (
                UiLanguage::En,
                "Copied the complete body (12 characters)",
                "OSC52: completion unconfirmed for copying the complete body (12 characters). Request sent.",
            ),
            (
                UiLanguage::Ja,
                "本文全体をコピーしました（12文字）",
                "OSC52: 本文全体のコピー要求を端末へ送りました（12文字・完了未確認）",
            ),
        ] {
            assert_eq!(copied(language, CopyScope::Body, 12, false), os);
            assert_eq!(copied(language, CopyScope::Body, 12, true), terminal);
            let guide = match language {
                UiLanguage::En => include_str!("../../docs/guides/user-guide.md"),
                UiLanguage::Ja => include_str!("../../docs/guides/user-guide.ja.md"),
            };
            assert!(guide.contains(terminal));
        }
        assert_eq!(
            copied(
                UiLanguage::En,
                CopyScope::OriginalPage { has_more: true },
                1,
                false
            ),
            "Copied the current original page (more available) (1 character)"
        );
        assert_eq!(
            copied(UiLanguage::En, CopyScope::CodeBlock { index: 0 }, 1, true),
            "OSC52: completion unconfirmed for copying code block 1 (1 character). Request sent."
        );
    }

    #[test]
    fn preview_joins_complete_english_sentences_without_translating_raw_text() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            preview_notice(
                UiLanguage::En,
                at,
                &["First complete sentence.", "Second complete sentence."]
            ),
            "Captured 2026-11-02 00:00:00 UTC | Close and reopen to refresh. | First complete sentence. Second complete sentence."
        );
        assert_eq!(
            preview_notice(UiLanguage::Ja, at, &["既存の文", "次の文"]),
            "取得 2026-11-02 00:00:00 UTC | 閉じて開くと再取得 | 既存の文。次の文"
        );
        let raw = "RAW 原文 provider text";
        assert!(preview_notice(UiLanguage::En, at, &[raw]).ends_with(raw));
    }
}
