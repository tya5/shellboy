//! Complete UI messages; dynamic values belong to the message, not fragments.
use crate::config::UiLanguage;

macro_rules! local {
    ($language:expr, $ja:literal, $en:literal) => {
        match $language {
            $crate::config::UiLanguage::Ja => $ja,
            $crate::config::UiLanguage::En => $en,
        }
    };
}
pub(super) use local;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
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
    pub(crate) fn render(self, language: UiLanguage) -> String {
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
            (UiLanguage::En, Self::TerminalWidth { observed: Some(2) }) => "Ambiguous character width: 2 columns (measured); differs from Ratatui default (1).".into(),
            (UiLanguage::En, Self::TerminalWidth { .. }) => "Ambiguous character width: unknown; reserving 2 columns.".into(),
            (UiLanguage::Ja, Self::TerminalWidth { observed: Some(1) }) => "曖昧な文字の幅: 1列（測定済み）".into(),
            (UiLanguage::Ja, Self::TerminalWidth { observed: Some(2) }) => "曖昧な文字の幅: 2列（測定済み、Ratatui既定値1列と不一致）".into(),
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
    fn emoji_variation_bases_are_rejected_without_rejecting_ascii_digits() {
        assert!(crate::ui::text_width::is_emoji_variation_base('\u{21aa}'));
        assert!(crate::ui::text_width::is_emoji_variation_base('\u{2764}'));
        assert!(crate::ui::text_width::is_emoji_variation_base('\u{231a}'));
        assert!(!crate::ui::text_width::is_emoji_variation_base('1'));
        assert!(!crate::ui::text_width::is_emoji_variation_base('▸'));
    }

    #[test]
    fn fixed_messages_select_the_explicit_language() {
        assert_eq!(local!(UiLanguage::Ja, "日本語", "English"), "日本語");
        assert_eq!(local!(UiLanguage::En, "日本語", "English"), "English");
    }

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
            assert!(
                !matches!(value, '\u{fe0f}' | '\u{200d}')
                    && !('\u{1f000}'..='\u{1faff}').contains(&value)
                    && !crate::ui::text_width::is_emoji_variation_base(value),
                "emoji in generated UI symbol {value:?}"
            );
            crate::ui::text_width::set_terminal_width(Some(1));
            let narrow = value.width();
            crate::ui::text_width::set_terminal_width(Some(2));
            let wide = value.width_cjk();
            assert_eq!(narrow, wide, "ambiguous UI symbol {value:?}");
        }
    }
    crate::ui::text_width::set_terminal_width(policy);
}

/// Register a synthetic UI-only screen and its essential translated phrases.
/// Stage B screens reuse the same sizes, terminal policies and border checks.
#[cfg(test)]
pub(super) fn verify_screen(
    configure: fn(&mut super::UiState),
    expected: fn(UiLanguage) -> &'static [&'static str],
) {
    use crate::ui::text_width::UnicodeWidthStr;
    use ratatui::{Terminal, backend::TestBackend};
    for observed in [Some(1), Some(2), None] {
        crate::ui::text_width::set_terminal_width(observed);
        for width in [80, 120] {
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let mut config = crate::config::Config::default();
                config.ui.language = language;
                let mut ui = super::UiState::new(&config, std::path::PathBuf::new());
                configure(&mut ui);
                let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                terminal.draw(|frame| super::draw(frame, &mut ui)).unwrap();
                let buffer = terminal.backend().buffer();
                assert_invariant_ui_symbols(buffer, observed);
                if let Some(bounds) = ui.aux_popup_paint.get() {
                    let bounds = bounds.outer;
                    for y in bounds.y + 1..bounds.bottom() - 1 {
                        assert_eq!(buffer[(bounds.x, y)].symbol(), "|", "left border at {y}");
                        assert_eq!(
                            buffer[(bounds.right() - 1, y)].symbol(),
                            "|",
                            "right border at {y}"
                        );
                    }
                }
                let mut screen = String::new();
                for y in 0..30 {
                    let mut x = 0;
                    while x < width {
                        let value = buffer[(x, y)].symbol();
                        screen.push_str(value);
                        x += value.width().max(1) as u16;
                    }
                    screen.push('\n');
                }
                for phrase in expected(language) {
                    assert!(
                        screen.contains(phrase),
                        "terminal={observed:?} width={width} missing {phrase:?}: {screen}"
                    );
                }
            }
        }
    }
    crate::ui::text_width::clear_terminal_width();
}

#[cfg(test)]
mod rendering_tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{Confirmation, UiState},
    };

    #[test]
    fn force_quit_is_complete_in_both_languages_and_widths() {
        fn configure(ui: &mut UiState) {
            ui.confirmation = Some(Confirmation::ForceQuit);
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Confirm force quit", "Saving and cleanup", "Y Force quit"],
                UiLanguage::Ja => &["強制終了の確認", "保存や回収", "Y 強制終了"],
            }
        }
        super::verify_screen(configure, expected);
    }

    #[test]
    fn representative_help_and_operations_screens_share_the_width_checks() {
        fn show_help(ui: &mut UiState) {
            let config = ui.config.clone();
            ui.show_help(super::super::help::Page::guide(&config));
        }
        fn show_operations(ui: &mut UiState) {
            ui.open_operations(None);
        }
        fn help_expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Common actions", "F1 Back"],
                UiLanguage::Ja => &["よく使う操作", "F1 戻る"],
            }
        }
        fn operations_expected(_: UiLanguage) -> &'static [&'static str] {
            &["CLIと同じ操作", "Esc"]
        }
        super::verify_screen(show_help, help_expected);
        super::verify_screen(show_operations, operations_expected);
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
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ui::i18n::rendering_tests::locale_subprocess_fixture",
                ])
                .env("SHELLBOY_TEST_LOCALE", "1")
                .env("LANG", locale)
                .env("LC_ALL", locale)
                .output()
                .unwrap();
            // Keep the nested harness summary out of the parent's test output;
            // the CI timing runner counts one summary per executable.
            assert!(
                output.status.success(),
                "locale {locale}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
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
