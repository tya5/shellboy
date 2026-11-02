//! Local operation vocabulary; no source reads or clock-dependent hint formatting.
use super::i18n::local;
use super::{Config, UiState, overlay::Overlay};
use crate::ui::text_width::UnicodeWidthStr;
use std::cell::RefCell;

pub(super) fn readonly_key(mut key: crossterm::event::KeyEvent) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyCode, KeyModifiers};
    if !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && let KeyCode::Char(ch) = key.code
    {
        key.code = KeyCode::Char(ch.to_ascii_lowercase());
    }
    key
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Help,
    Commands,
    History,
    Threads,
    Send,
    Newline,
    Interrupt,
    Stop,
    Toggle,
    Original,
    Copy,
    Code,
    CopyBody,
    Back,
    SelectionBack,
    LongCopy,
    Move,
    NextPage,
    FirstPage,
    Fields,
    Operations,
    Choose,
    Complete,
    NextField,
    Display,
    Execute,
    Submit,
    Refresh,
}
impl Action {
    // Bindings remain input identities, independent of translated descriptions.
    fn static_binding(self, language: crate::config::UiLanguage) -> &'static str {
        match self {
            Self::Help => "F1",
            Self::Commands => "/",
            Self::History => "Tab",
            Self::Threads => "Ctrl+T",
            Self::Send | Self::Newline => "",
            Self::Interrupt | Self::Back => "Esc",
            Self::Stop => "Ctrl+C",
            Self::Toggle => "Enter/Space",
            Self::Original => "o",
            Self::Copy => "c",
            Self::Code => "b",
            Self::CopyBody => "0",
            Self::SelectionBack => "F2/Esc",
            Self::LongCopy => local!(language, "会話の履歴で c", "History c"),
            Self::Move => "^v",
            Self::NextPage => "n",
            Self::FirstPage => "p",
            Self::Fields => "v",
            Self::Operations => "a",
            Self::Choose | Self::Display | Self::Execute | Self::Submit => "Enter",
            Self::Complete | Self::NextField => "Tab",
            Self::Refresh => "r",
        }
    }
    pub fn spec(self) -> (&'static str, &'static str) {
        (
            self.static_binding(crate::config::UiLanguage::Ja),
            self.short_label(crate::config::UiLanguage::Ja),
        )
    }
    pub fn description(self, language: crate::config::UiLanguage) -> &'static str {
        match self {
            Self::Help => local!(language, "操作ガイド", "Open the operation guide"),
            Self::Commands => local!(
                language,
                "コマンド候補を表示。^vで選び、Tabで補完",
                "Show commands; select with ^v and complete with Tab"
            ),
            Self::History => local!(language, "履歴", "History"),
            Self::Threads => local!(language, "会話", "Threads"),
            Self::Send => local!(
                language,
                "次の区切りでメッセージを渡す",
                "Deliver the message at the next boundary"
            ),
            Self::Newline => local!(
                language,
                "入力中に改行する",
                "Insert a newline in the draft"
            ),
            Self::Interrupt => local!(
                language,
                "現在のTurnを中断する（Runと背景の停止はCtrl+C）",
                "Interrupt the current Turn; Ctrl+C stops the Run and its background work"
            ),
            Self::Stop => local!(
                language,
                "Runと所有する背景の作業を停止する",
                "Stop the Run and its background work"
            ),
            Self::Toggle => local!(language, "開閉", "Toggle"),
            Self::Original => local!(language, "原文", "Original"),
            Self::Copy => local!(language, "コピー", "Copy"),
            Self::Code => local!(
                language,
                "保存された原文のコードの塊を番号で選んでコピーする",
                "Select and copy a numbered code block from the saved original"
            ),
            Self::CopyBody => local!(language, "本文全体", "Full text"),
            Self::Back | Self::SelectionBack => local!(language, "戻る", "Back"),
            Self::LongCopy => local!(language, "長い回答のコピー", "Copy a long answer"),
            Self::Move | Self::Choose => local!(language, "選択", "Select"),
            Self::NextPage => local!(language, "次page", "Next page"),
            Self::FirstPage => local!(language, "先頭再取得", "Reload first page"),
            Self::Fields => local!(language, "要点/内部項目", "Summary / fields"),
            Self::Operations => local!(language, "操作", "Actions"),
            Self::Complete => local!(language, "補完", "Complete"),
            Self::NextField => local!(language, "次の項目", "Next field"),
            Self::Display => local!(language, "表示", "Show"),
            Self::Execute => local!(language, "実行", "Run"),
            Self::Submit => local!(language, "送信", "Send"),
            Self::Refresh => local!(language, "再取得", "Refresh"),
        }
    }
    fn short_label(self, language: crate::config::UiLanguage) -> &'static str {
        match self {
            Self::Help => local!(language, "操作ガイド", "Open the operation guide"),
            Self::Commands => local!(language, "コマンド", "Commands"),
            Self::History => local!(language, "履歴", "History"),
            Self::Threads => local!(language, "会話", "Threads"),
            Self::Send | Self::Submit => local!(language, "送信", "Send"),
            Self::Newline => local!(language, "改行", "Newline"),
            Self::Interrupt => local!(language, "中断", "Interrupt"),
            Self::Stop => local!(language, "停止", "Stop"),
            Self::Toggle => local!(language, "開閉", "Toggle"),
            Self::Original => local!(language, "原文", "Original"),
            Self::Copy => local!(language, "コピー", "Copy"),
            Self::Code => local!(language, "コードをコピー", "Copy code"),
            Self::CopyBody => local!(language, "本文全体", "Full text"),
            Self::Back | Self::SelectionBack => local!(language, "戻る", "Back"),
            Self::LongCopy => local!(language, "長い回答のコピー", "Copy a long answer"),
            Self::Move | Self::Choose => local!(language, "選択", "Select"),
            Self::NextPage => local!(language, "次page", "Next page"),
            Self::FirstPage => local!(language, "先頭再取得", "Reload first page"),
            Self::Fields => local!(language, "要点/内部項目", "Summary / fields"),
            Self::Operations => local!(language, "操作", "Actions"),
            Self::Complete => local!(language, "補完", "Complete"),
            Self::NextField => local!(language, "次の項目", "Next field"),
            Self::Display => local!(language, "表示", "Show"),
            Self::Execute => local!(language, "実行", "Run"),
            Self::Refresh => local!(language, "再取得", "Refresh"),
        }
    }
    pub fn binding(self, config: &Config) -> String {
        match self {
            Self::Send => config.ui.send_key.clone(),
            Self::Newline => config.ui.newline_keys.join("/"),
            _ => self.static_binding(config.ui.language).into(),
        }
    }
    pub fn matches(self, code: crossterm::event::KeyCode) -> bool {
        let binding = self.spec().0;
        binding.len() == 1 && code == crossterm::event::KeyCode::Char(binding.as_bytes()[0] as char)
    }
    fn label(self, config: &Config) -> String {
        if self == Self::Help {
            "F1".into()
        } else {
            format!(
                "{} {}",
                self.binding(config),
                self.short_label(config.ui.language)
            )
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Context {
    Empty,
    Typing,
    Running,
    History,
    LiveWork,
    Selection,
    Completion,
    Details,
    Fields,
    Original { next: bool },
    OriginalPending,
    CodeChoices,
    CodePending,
    CodeBody,
    Help,
    Popup,
    Management,
    Form(crate::operations::EnterVerb),
    Confirmation,
    Shutdown,
}
impl Context {
    fn actions(self) -> &'static [Action] {
        use Action::*;
        match self {
            Self::Empty => &[Commands, History, Threads],
            Self::Typing => &[Send, Newline],
            Self::Running => &[Interrupt, Stop],
            Self::History => &[Copy, Code, Original],
            Self::LiveWork => &[Toggle, Back],
            Self::Selection => &[SelectionBack, LongCopy],
            Self::Completion => &[Move, Complete, Back],
            Self::Details => &[Original, Copy, Back],
            Self::Fields => &[Original, Copy, Fields],
            Self::Original { next: true } => &[Copy, NextPage, FirstPage],
            Self::Original { next: false } => &[Copy, FirstPage, Back],
            Self::OriginalPending => &[FirstPage, Back],
            Self::CodeChoices => &[Move, Copy, Back],
            Self::CodePending => &[Back],
            Self::CodeBody => &[CopyBody, Back],
            Self::Help => &[Back],
            Self::Popup => &[Move, Choose, Back],
            Self::Management => &[Operations, Back],
            Self::Form(crate::operations::EnterVerb::Display) => &[NextField, Display, Back],
            Self::Form(crate::operations::EnterVerb::Execute) => &[NextField, Execute, Back],
            Self::Form(crate::operations::EnterVerb::Send) => &[NextField, Submit, Back],
            Self::Confirmation | Self::Shutdown => &[],
        }
    }
}

pub(super) fn detail_footer(context: Context) -> &'static str {
    use std::sync::LazyLock;
    static FOOTERS: LazyLock<[String; 5]> = LazyLock::new(|| {
        use Action::*;
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        [
            vec![Refresh, Original, Copy, Operations],
            vec![Refresh, Original, Copy, Fields, Operations],
            vec![Copy, NextPage, FirstPage, Original],
            vec![Copy, FirstPage, Original],
            vec![FirstPage, Original],
        ]
        .map(|actions| {
            crate::ui::text_width::glyphs::formatted!(
                "Esc 戻る · ↑↓/PgUp/PgDn 移動\n{}",
                "Esc 戻る | ^v/PgUp/PgDn 移動\n{}",
                actions
                    .iter()
                    .map(|a| a.label(&config))
                    .collect::<Vec<_>>()
                    .join(crate::ui::text_width::glyphs::separator())
            )
        })
    });
    &FOOTERS[match context {
        Context::Fields => 1,
        Context::Original { next: true } => 2,
        Context::Original { next: false } => 3,
        Context::OriginalPending => 4,
        _ => 0,
    }]
}

pub(super) fn detail_footer_for(
    context: Context,
    language: crate::config::UiLanguage,
) -> &'static str {
    use crate::config::UiLanguage;
    if language == UiLanguage::Ja {
        return detail_footer(context);
    }
    match context {
        Context::Fields => {
            "Esc Back | ^v/PgUp/PgDn Move\nr Refresh | o Original | c Copy | v Fields | a Actions"
        }
        Context::Original { next: true } => {
            "Esc Back | ^v/PgUp/PgDn Move\nc Copy | n Next page | p First page | o Original"
        }
        Context::Original { next: false } => {
            "Esc Back | ^v/PgUp/PgDn Move\nc Copy | p First page | o Original"
        }
        Context::OriginalPending => "Esc Back | ^v/PgUp/PgDn Move\np First page | o Original",
        _ => "Esc Back | ^v/PgUp/PgDn Move\nr Refresh | o Original | c Copy | a Actions",
    }
}

#[derive(Default)]
pub(super) struct Cache(RefCell<Option<Entry>>);
struct Entry {
    context: Context,
    width: u16,
    send: String,
    newline: Vec<String>,
    language: crate::config::UiLanguage,
    text: String,
}
impl Cache {
    pub fn text(&self, context: Context, width: u16, config: &Config) -> std::cell::Ref<'_, str> {
        let mut entry = self.0.borrow_mut();
        if !entry.as_ref().is_some_and(|e| {
            e.context == context
                && e.width == width
                && e.send == config.ui.send_key
                && e.newline == config.ui.newline_keys
                && e.language == config.ui.language
        }) {
            let mut labels = context
                .actions()
                .iter()
                .map(|a| a.label(config))
                .collect::<Vec<_>>();
            let text = if matches!(context, Context::Confirmation | Context::Shutdown) {
                String::new()
            } else {
                loop {
                    let text = labels
                        .iter()
                        .map(String::as_str)
                        .chain(["F1"])
                        .collect::<Vec<_>>()
                        .join(crate::ui::text_width::glyphs::separator());
                    if text.width() <= usize::from(width) {
                        break text;
                    }
                    if labels.pop().is_none() {
                        break String::new();
                    }
                }
            };
            *entry = Some(Entry {
                context,
                width,
                send: config.ui.send_key.clone(),
                newline: config.ui.newline_keys.clone(),
                language: config.ui.language,
                text,
            });
        }
        drop(entry);
        std::cell::Ref::map(self.0.borrow(), |e| e.as_ref().unwrap().text.as_str())
    }
}
impl UiState {
    pub(super) fn hint_context(&self) -> Context {
        match self.top_overlay() {
            Some(Overlay::Shutdown) => Context::Shutdown,
            Some(Overlay::Confirmation) => Context::Confirmation,
            Some(Overlay::Selection) => Context::Selection,
            Some(Overlay::Help) => Context::Help,
            Some(Overlay::Details) => self.details.as_ref().unwrap().hint_context(),
            Some(Overlay::Management) => Context::Management,
            Some(Overlay::Operations) => self.operations.as_ref().unwrap().hint_context(),
            Some(_) => Context::Popup,
            None if self.view().items.active => Context::History,
            None if self.view().live_work.active => Context::LiveWork,
            None if self.has_completions() => Context::Completion,
            None if self
                .view()
                .activity
                .snapshot
                .as_ref()
                .is_some_and(|s| s.turn_in_progress && s.turn_id.is_some())
                && self.view().thread_id.is_some() =>
            {
                Context::Running
            }
            None if self.view().draft.text.is_empty() => Context::Empty,
            None => Context::Typing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_text_keeps_every_original_binding_and_japanese_short_label() {
        let cases = [
            (Action::Help, "F1", "操作ガイド"),
            (Action::Commands, "/", "コマンド"),
            (Action::History, "Tab", "履歴"),
            (Action::Threads, "Ctrl+T", "会話"),
            (Action::Send, "", "送信"),
            (Action::Newline, "", "改行"),
            (Action::Interrupt, "Esc", "中断"),
            (Action::Stop, "Ctrl+C", "停止"),
            (Action::Toggle, "Enter/Space", "開閉"),
            (Action::Original, "o", "原文"),
            (Action::Copy, "c", "コピー"),
            (Action::Code, "b", "コードをコピー"),
            (Action::CopyBody, "0", "本文全体"),
            (Action::Back, "Esc", "戻る"),
            (Action::SelectionBack, "F2/Esc", "戻る"),
            (Action::LongCopy, "会話の履歴で c", "長い回答のコピー"),
            (Action::Move, "^v", "選択"),
            (Action::NextPage, "n", "次page"),
            (Action::FirstPage, "p", "先頭再取得"),
            (Action::Fields, "v", "要点/内部項目"),
            (Action::Operations, "a", "操作"),
            (Action::Choose, "Enter", "選択"),
            (Action::Complete, "Tab", "補完"),
            (Action::NextField, "Tab", "次の項目"),
            (Action::Display, "Enter", "表示"),
            (Action::Execute, "Enter", "実行"),
            (Action::Submit, "Enter", "送信"),
            (Action::Refresh, "r", "再取得"),
        ];
        for (action, binding, label) in cases {
            assert_eq!(action.spec(), (binding, label), "{action:?}");
        }
        let mut config = Config::default();
        assert_eq!(Action::LongCopy.binding(&config), "History c");
        config.ui.language = crate::config::UiLanguage::Ja;
        assert_eq!(Action::LongCopy.binding(&config), "会話の履歴で c");
    }
    #[test]
    fn every_width_keeps_whole_actions_and_help_last() {
        for context in [
            Context::Empty,
            Context::Typing,
            Context::Running,
            Context::History,
            Context::Selection,
            Context::Original { next: true },
            Context::Popup,
            Context::Form(crate::operations::EnterVerb::Display),
            Context::Form(crate::operations::EnterVerb::Execute),
            Context::Form(crate::operations::EnterVerb::Send),
        ] {
            let cache = Cache::default();
            for width in 0..180 {
                let text = cache.text(context, width, &Config::default());
                assert!(text.width() <= usize::from(width));
                if width >= 2 {
                    assert!(text.ends_with("F1"), "{text}");
                }
                assert!(!text.contains('.'));
            }
        }
    }
    #[test]
    fn language_changes_invalidate_the_hint_cache() {
        let cache = Cache::default();
        let mut config = Config::default();
        assert!(cache.text(Context::Typing, 120, &config).contains("Send"));
        config.ui.language = crate::config::UiLanguage::Ja;
        assert!(cache.text(Context::Typing, 120, &config).contains("送信"));
        assert!(
            detail_footer_for(Context::Details, crate::config::UiLanguage::En).contains("Back")
        );
    }
    #[test]
    fn binding_changes_invalidate_but_clock_repaints_reuse_the_string() {
        let cache = Cache::default();
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let pointer = cache.text(Context::Typing, 120, &config).as_ptr();
        assert_eq!(pointer, cache.text(Context::Typing, 120, &config).as_ptr());
        config.ui.send_key = "Ctrl+Enter".into();
        config.ui.newline_keys = vec!["Enter".into()];
        let text = cache.text(Context::Typing, 120, &config);
        assert!(text.contains("Ctrl+Enter 送信"));
        assert!(text.contains("Enter 改行"));
    }
    #[test]
    fn form_cache_tracks_catalogue_verb_and_never_uses_the_composer_send_binding() {
        use crate::operations::EnterVerb;
        let cache = Cache::default();
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        config.ui.send_key = "Ctrl+Enter".into();
        for (verb, expected) in [
            (EnterVerb::Display, "Enter 表示"),
            (EnterVerb::Execute, "Enter 実行"),
            (EnterVerb::Send, "Enter 送信"),
        ] {
            let context = Context::Form(verb);
            let text = cache.text(context, 120, &config);
            assert!(text.contains(expected), "{text}");
            assert!(
                !text.contains("Ctrl+Enter"),
                "form Enter is independent of composer keys"
            );
            let pointer = text.as_ptr();
            drop(text);
            assert_eq!(pointer, cache.text(context, 120, &config).as_ptr());
        }
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    fn ui() -> UiState {
        UiState::new(&Config::default(), std::path::PathBuf::from("/"))
    }
    fn key(ui: &mut UiState, code: KeyCode) {
        ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }
    #[test]
    fn help_is_an_escape_route_without_discarding_the_underlying_detail() {
        let mut ui = ui();
        ui.details = Some(super::super::details::Details::new(ui.view(), false));
        assert_eq!(ui.hint_context(), Context::Details);
        key(&mut ui, KeyCode::F(1));
        assert_eq!(ui.top_overlay(), Some(Overlay::Help));
        assert!(ui.details.is_some());
        key(&mut ui, KeyCode::Esc);
        assert_eq!(ui.top_overlay(), Some(Overlay::Details));
        assert!(ui.details.is_some());
    }
    #[test]
    fn footer_never_overwrites_a_notice_even_in_history_focus() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = ui();
        ui.active_view_mut().items.active = true;
        ui.input_notice = Some("synthetic failure");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| super::super::draw(f, &mut ui)).unwrap();
        let buffer = terminal.backend().buffer();
        let last = (0..80)
            .map(|x| buffer[(x, 23)].symbol())
            .collect::<String>();
        assert!(last.contains("synthetic failure"), "{last}");
        assert!(!last.contains("コピー"), "{last}");
    }
    #[test]
    fn detail_pages_only_advertise_real_continuations() {
        assert!(!detail_footer(Context::Original { next: false }).contains("n 次page"));
        assert!(detail_footer(Context::Original { next: true }).contains("n 次page"));
        assert!(detail_footer(Context::Fields).contains("v 要点/内部項目"));
        assert!(!detail_footer(Context::OriginalPending).contains("c コピー"));
    }
}

#[cfg(test)]
mod readonly_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    #[test]
    fn readonly_case_normalization_never_rewrites_control_or_alt_chords() {
        for letter in ['O', 'C', 'N', 'P', 'V', 'A', 'R'] {
            assert_eq!(
                readonly_key(KeyEvent::new(KeyCode::Char(letter), KeyModifiers::SHIFT)).code,
                KeyCode::Char(letter.to_ascii_lowercase())
            );
            for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
                let key = KeyEvent::new(KeyCode::Char(letter), modifiers | KeyModifiers::SHIFT);
                assert_eq!(readonly_key(key), key);
            }
        }
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn selection_tip_does_not_toggle_an_existing_history_focus_back_to_composer() {
        let cache = Cache::default();
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let text = cache.text(Context::Selection, 100, &config);
        assert!(text.contains("F2/Esc 戻る"));
        assert!(text.contains("会話の履歴で c"));
        assert!(!text.contains("Tab"));
    }
}
