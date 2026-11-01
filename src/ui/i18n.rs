//! Complete UI messages; dynamic values belong to the message, not fragments.
use crate::config::UiLanguage;

#[derive(Clone, Copy, Debug)]
pub(super) enum Message {
    ForceQuitTitle,
    ForceQuitContext,
    ForceQuitBody,
    ForceQuitFooter,
    TerminalWidthHeading,
    TerminalWidth {
        observed: Option<u8>,
    },
    #[allow(dead_code)]
    DeletedConversations {
        count: usize,
    },
}

impl Message {
    pub(super) fn render(self, language: UiLanguage) -> String {
        match (language, self) {
            (UiLanguage::En, Self::ForceQuitTitle) => "Confirm force quit".into(),
            (UiLanguage::Ja, Self::ForceQuitTitle) => "強制終了の確認".into(),
            (UiLanguage::En, Self::ForceQuitContext) => "Review the consequences".into(),
            (UiLanguage::Ja, Self::ForceQuitContext) => "影響を確認してください".into(),
            (UiLanguage::En, Self::ForceQuitBody) => "Force quit?\nSaving and cleanup may be incomplete. The process will exit and recover on the next start.".into(),
            (UiLanguage::Ja, Self::ForceQuitBody) => "強制終了しますか？\n保存や回収が未完了の可能性があります。プロセスを終了し、次回起動で復旧します。".into(),
            (UiLanguage::En, Self::ForceQuitFooter) => "N / Esc Back | Y Force quit\nUp / Down / PgUp / PgDn Scroll".into(),
            (UiLanguage::Ja, Self::ForceQuitFooter) => "N / Esc 戻る | Y 強制終了\n上下キー / PgUp / PgDn 閲覧".into(),
            (UiLanguage::En, Self::TerminalWidthHeading) => "Terminal display".into(),
            (UiLanguage::Ja, Self::TerminalWidthHeading) => "端末の表示".into(),
            (UiLanguage::En, Self::TerminalWidth { observed: Some(1) }) => "Ambiguous character width: 1 column (measured).".into(),
            (UiLanguage::En, Self::TerminalWidth { observed: Some(2) }) => "Ambiguous character width: 2 columns (measured).".into(),
            (UiLanguage::En, Self::TerminalWidth { .. }) => "Ambiguous character width: unknown; reserving 2 columns.".into(),
            (UiLanguage::Ja, Self::TerminalWidth { observed: Some(1) }) => "曖昧な文字の幅: 1列（測定済み）".into(),
            (UiLanguage::Ja, Self::TerminalWidth { observed: Some(2) }) => "曖昧な文字の幅: 2列（測定済み）".into(),
            (UiLanguage::Ja, Self::TerminalWidth { .. }) => "曖昧な文字の幅: 不明（2列を確保）".into(),
            (UiLanguage::En, Self::DeletedConversations { count: 1 }) => "Deleted 1 conversation".into(),
            (UiLanguage::En, Self::DeletedConversations { count }) => format!("Deleted {count} conversations"),
            (UiLanguage::Ja, Self::DeletedConversations { count }) => format!("会話を{count}件削除しました"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_messages_handle_zero_one_and_many() {
        assert_eq!(
            Message::DeletedConversations { count: 0 }.render(UiLanguage::En),
            "Deleted 0 conversations"
        );
        assert_eq!(
            Message::DeletedConversations { count: 1 }.render(UiLanguage::En),
            "Deleted 1 conversation"
        );
        assert_eq!(
            Message::DeletedConversations { count: 2 }.render(UiLanguage::En),
            "Deleted 2 conversations"
        );
        assert_eq!(
            Message::DeletedConversations { count: 1 }.render(UiLanguage::Ja),
            "会話を1件削除しました"
        );
    }
}

/// Use on synthetic UI-only fixtures, not on cells containing user/model text.
#[cfg(test)]
pub(super) fn assert_invariant_ui_symbols(buffer: &ratatui::buffer::Buffer, policy: Option<u8>) {
    use unicode_width::UnicodeWidthChar;
    for cell in &buffer.content {
        for value in cell.symbol().chars() {
            unicode_width::set_terminal_width(Some(1));
            let narrow = value.width();
            unicode_width::set_terminal_width(Some(2));
            let wide = value.width();
            assert_eq!(narrow, wide, "ambiguous UI symbol {value:?}");
        }
    }
    unicode_width::set_terminal_width(policy);
}

#[cfg(test)]
mod rendering_tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{self, Confirmation, UiState},
    };
    use ratatui::{Terminal, backend::TestBackend};
    use unicode_width::UnicodeWidthStr;

    fn render_fixture(language: UiLanguage, width: u16) -> String {
        let mut config = Config::default();
        config.ui.language = language;
        let mut ui = UiState::new(&config, std::path::PathBuf::new());
        ui.confirmation = Some(Confirmation::ForceQuit);
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut ui)).unwrap();
        let buffer = terminal.backend().buffer();
        let observed = unicode_width::terminal_width_observation();
        super::assert_invariant_ui_symbols(buffer, observed);
        let bounds = ui.aux_popup_paint.get().expect("confirmation bounds").outer;
        for y in bounds.y + 1..bounds.bottom() - 1 {
            assert_eq!(buffer[(bounds.x, y)].symbol(), "|", "left border at {y}");
            assert_eq!(
                buffer[(bounds.right() - 1, y)].symbol(),
                "|",
                "right border at {y}"
            );
        }
        let mut result = String::new();
        for y in 0..30 {
            let mut x = 0;
            while x < width {
                let value = buffer[(x, y)].symbol();
                result.push_str(value);
                x += value.width().max(1) as u16;
            }
            result.push('\n');
        }
        result
    }

    #[test]
    fn force_quit_is_complete_in_both_languages_and_widths() {
        for observed in [Some(1), Some(2), None] {
            unicode_width::set_terminal_width(observed);
            for width in [80, 120] {
                for (language, title, warning, action) in [
                    (
                        UiLanguage::En,
                        "Confirm force quit",
                        "Saving and cleanup",
                        "Y Force quit",
                    ),
                    (UiLanguage::Ja, "強制終了の確認", "保存や回収", "Y 強制終了"),
                ] {
                    let screen = render_fixture(language, width);
                    for expected in [title, warning, action] {
                        assert!(
                            screen.contains(expected),
                            "terminal={observed:?} width={width} missing {expected:?}: {screen}"
                        );
                    }
                }
            }
        }
        unicode_width::clear_terminal_width();
    }

    #[test]
    fn locale_subprocess_fixture() {
        if std::env::var_os("SHELLBOY_TEST_LOCALE").is_some() {
            assert_eq!(Config::parse("").unwrap().ui.language, UiLanguage::En);
        }
    }

    #[test]
    fn language_configuration_is_strict_and_locale_independent() {
        for locale in ["ja_JP.UTF-8", "en_US.UTF-8", "C"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ui::i18n::rendering_tests::locale_subprocess_fixture",
                ])
                .env("SHELLBOY_TEST_LOCALE", "1")
                .env("LANG", locale)
                .env("LC_ALL", locale)
                .status()
                .unwrap();
            assert!(status.success(), "locale {locale}");
        }
        assert_eq!(Config::parse("").unwrap().ui.language, UiLanguage::En);
        assert_eq!(
            Config::parse("[ui]\nlanguage='ja'").unwrap().ui.language,
            UiLanguage::Ja
        );
        assert_eq!(
            Config::parse("[ui]\nlanguage='en'").unwrap().ui.language,
            UiLanguage::En
        );
        assert!(Config::parse("[ui]\nlanguage='fr'").is_err());
        assert!(Config::parse("[ui]\nambiguous_width=2").is_err());
    }
}
