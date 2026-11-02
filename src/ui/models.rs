//! A live model picker; no built-in model/capability catalog.
use super::i18n::local;
use super::*;
use crate::config::UiLanguage;
use crate::protocol::ThreadSetArgs;

pub(super) struct ModelPicker {
    language: UiLanguage,
    paint: std::cell::Cell<Option<popup::PaintBounds>>,
    filtered_indices: Vec<usize>,
    request: u64,
    connection: String,
    current: Option<String>,
    models: Vec<String>,
    query: String,
    selected: usize,
    loading: bool,
    error: Option<String>,
    captured_at: Option<chrono::DateTime<chrono::Utc>>,
    last_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    notice_scroll: Option<u16>,
}
enum Outcome {
    None,
    Close,
    Reload(String),
    Copy(String),
    Select { connection: String, model: String },
}
fn notice_height(body_height: u16) -> u16 {
    // Keep two rows for the selected model's bounded head and tail preview.
    body_height.saturating_sub(2).min(3)
}
impl ModelPicker {
    fn footer(&self) -> String {
        crate::ui::text_width::glyphs::decorate(local!(
            self.language,
            "Esc 戻る | Enter 変更\n^v 選択 | Ctrl+R 再取得 | Ctrl+Y コピー",
            "Esc Back | Enter Change\n^v Select | Ctrl+R Refresh | Ctrl+Y Copy"
        ))
    }
    fn notice_text(&self, notice: &super::observation_notice::ObservationNotice) -> String {
        use super::observation_notice::{Freshness, NextAction, ObservationKind};
        if self.language == UiLanguage::Ja {
            return crate::ui::text_width::glyphs::decorate(&notice.text());
        }
        let state = match (notice.kind, notice.freshness) {
            (ObservationKind::Models, Freshness::Captured) => {
                "The saved model list reflects its capture time."
            }
            (ObservationKind::Models, Freshness::Stale) => {
                "The model list may be stale while refresh is running."
            }
            (ObservationKind::Models, Freshness::FailedRefresh) => {
                "Refresh failed; the previous model list is shown."
            }
            (ObservationKind::Models, Freshness::Unavailable) => {
                "The model list has not been captured."
            }
            (_, Freshness::Captured) => "The saved result reflects its capture time.",
            (_, Freshness::Stale) => "The saved result may be stale.",
            (_, Freshness::FailedRefresh) => "Refresh failed; the previous result is shown.",
            (_, Freshness::Unavailable) => "The observed scope is unavailable.",
        };
        let time = |value: chrono::DateTime<chrono::Utc>| {
            value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        let timing = match (notice.captured_at, notice.last_attempt_at) {
            (Some(captured), Some(attempted)) => format!(
                "Captured at {}; last requested at {}.",
                time(captured),
                time(attempted)
            ),
            (Some(captured), None) => format!("Captured at {}.", time(captured)),
            (None, Some(attempted)) => format!(
                "Capture time unavailable; last requested at {}.",
                time(attempted)
            ),
            (None, None) => "Capture and request times are unavailable.".into(),
        };
        if notice.next_action == NextAction::RefreshModels {
            format!("{state}\n{timing}\nPress Ctrl+R to refresh.")
        } else {
            format!("{state}\n{timing}")
        }
    }
    fn key(&mut self, key: KeyEvent, area: Rect) -> Outcome {
        let title = local!(self.language, "モデル一覧", "Models");
        let body_height = popup::body_area(area, title, &self.footer(), None).height;
        let page = usize::from(body_height.saturating_sub(notice_height(body_height)))
            .saturating_sub(1)
            .max(1);
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.notice_scroll = self.notice_scroll.is_none().then_some(0);
            return Outcome::None;
        }
        if let Some(scroll) = &mut self.notice_scroll {
            match key.code {
                KeyCode::Esc => self.notice_scroll = None,
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll = scroll.saturating_add(1).min(512),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(body_height.max(1)),
                KeyCode::PageDown => *scroll = scroll.saturating_add(body_height.max(1)).min(512),
                _ => {}
            }
            return Outcome::None;
        }
        if key.code == KeyCode::Esc {
            return Outcome::Close;
        }
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return if self.loading {
                Outcome::None
            } else {
                Outcome::Reload(self.connection.clone())
            };
        }
        if key.code == KeyCode::Char('y') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if !self.loading
                && self.error.is_none()
                && let Some(model) = self.filtered().get(self.selected)
            {
                return Outcome::Copy((*model).clone());
            }
            return Outcome::None;
        }
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.filtered().len().saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(page),
            KeyCode::PageDown => {
                self.selected = (self.selected + page).min(self.filtered().len().saturating_sub(1))
            }
            KeyCode::Backspace => {
                let boundary = prev_grapheme(&self.query, self.query.len());
                self.query.truncate(boundary);
                self.rebuild_filter();
                self.selected = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.paste(&c.to_string())
            }
            KeyCode::Enter if !self.loading && self.error.is_none() => {
                if let Some(model) = self.filtered().get(self.selected) {
                    let (connection, model) = (self.connection.clone(), (*model).clone());
                    return Outcome::Select { connection, model };
                }
            }
            _ => {}
        }
        Outcome::None
    }

    pub(super) fn accept(
        &mut self,
        request: u64,
        completed_at: chrono::DateTime<chrono::Utc>,
        result: Result<Vec<String>, String>,
    ) {
        if request != self.request || !self.loading {
            return;
        }
        self.paint.set(None);
        self.loading = false;
        self.last_attempt_at = Some(completed_at);
        match result {
            Ok(models) => {
                self.captured_at = Some(completed_at);
                self.models = models;
                self.rebuild_filter();
                self.selected = self
                    .current
                    .as_ref()
                    .and_then(|current| self.filtered().iter().position(|id| *id == current))
                    .unwrap_or(0);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
    fn rebuild_filter(&mut self) {
        let query = self.query.to_lowercase();
        self.filtered_indices = self
            .models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| model.to_lowercase().contains(&query).then_some(index))
            .collect();
    }
    fn filtered(&self) -> Vec<&String> {
        self.filtered_indices
            .iter()
            .filter_map(|index| self.models.get(*index))
            .collect()
    }
    pub(super) fn paint_bounds(&self) -> Option<popup::PaintBounds> {
        self.paint.get()
    }
    pub(super) fn wheel(&mut self, delta: isize) -> bool {
        if let Some(scroll) = &mut self.notice_scroll {
            let next = scroll
                .saturating_add_signed(delta.clamp(-512, 512) as i16)
                .min(512);
            let changed = *scroll != next;
            *scroll = next;
            return changed;
        }
        if self.loading || self.error.is_some() {
            return false;
        }
        let old = self.selected;
        self.selected = popup::move_selection(old, delta, self.filtered_indices.len());
        old != self.selected
    }
    pub(super) fn paste(&mut self, text: &str) {
        if self.notice_scroll.is_some() {
            return;
        }
        let available = 256usize.saturating_sub(self.query.chars().count());
        self.query
            .extend(text.chars().filter(|c| !c.is_control()).take(available));
        self.selected = 0;
        self.rebuild_filter();
    }
    pub(super) fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        let sep = crate::ui::text_width::glyphs::separator();
        let filtered = self.filtered();
        let notice = super::observation_notice::ObservationNotice {
            kind: super::observation_notice::ObservationKind::Models,
            captured_at: self.captured_at,
            last_attempt_at: self.last_attempt_at,
            freshness: if self.captured_at.is_none() {
                super::observation_notice::Freshness::Unavailable
            } else if self.loading {
                super::observation_notice::Freshness::Stale
            } else if self.error.is_some() {
                super::observation_notice::Freshness::FailedRefresh
            } else {
                super::observation_notice::Freshness::Captured
            },
            next_action: if self.loading {
                super::observation_notice::NextAction::None
            } else {
                super::observation_notice::NextAction::RefreshModels
            },
            confirmation_enabled: false,
        };
        let position = if filtered.is_empty() {
            0
        } else {
            self.selected + 1
        };
        let count = filtered.len();
        let context = match (self.language, count) {
            (UiLanguage::En, 1) => format!(
                "{}{sep}Filter: {}{sep}{position} / 1 model",
                self.connection, self.query
            ),
            (UiLanguage::En, _) => format!(
                "{}{sep}Filter: {}{sep}{position} / {count} models",
                self.connection, self.query
            ),
            (UiLanguage::Ja, _) => format!(
                "{}{sep}絞込み: {}{sep}{position} / {count}件",
                self.connection, self.query
            ),
        };
        use super::text_width::UnicodeWidthStr;
        let available = popup::body_area(area, &context, &self.footer(), None).width as usize;
        let context = if context.as_str().width() > available {
            match (self.language, count) {
                (UiLanguage::En, 1) => format!(
                    "{position} / 1 model{sep}{}{sep}Filter: {}",
                    self.connection, self.query
                ),
                (UiLanguage::En, _) => format!(
                    "{position} / {count} models{sep}{}{sep}Filter: {}",
                    self.connection, self.query
                ),
                (UiLanguage::Ja, _) => format!(
                    "{position} / {count}件{sep}{}{sep}絞込み: {}",
                    self.connection, self.query
                ),
            }
        } else {
            context
        };
        let title = local!(self.language, "Ctrl+T 情報", "Ctrl+T Details");
        let body = popup::panel_recorded(frame, area, title, &context, &self.footer(), &self.paint);
        let notice_text = self.notice_text(&notice);
        if let Some(scroll) = self.notice_scroll {
            let captured = self.captured_at.map_or_else(
                || local!(self.language, "取得時刻なし", "Capture time unavailable").to_owned(),
                |time| {
                    if body.width < 24 {
                        match self.language {
                            UiLanguage::En => format!(
                                "Captured UTC\n{}",
                                time.format("%Y-%m-%d\n%H:%M:%S\n%.3f UTC")
                            ),
                            UiLanguage::Ja => {
                                format!("取得 UTC\n{}", time.format("%Y-%m-%d\n%H:%M:%S\n%.3f UTC"))
                            }
                        }
                    } else {
                        let timestamp = time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        match self.language {
                            UiLanguage::En => format!("Captured UTC {timestamp}"),
                            UiLanguage::Ja => format!("取得 UTC {timestamp}"),
                        }
                    }
                },
            );
            frame.render_widget(
                Paragraph::new(format!("{captured}\n{notice_text}"))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                body,
            );
            return;
        }
        let notice_height = notice_height(body.height);
        frame.render_widget(
            Paragraph::new(notice_text).wrap(Wrap { trim: false }),
            Rect::new(body.x, body.y, body.width, notice_height),
        );
        let body = Rect::new(
            body.x,
            body.y + notice_height,
            body.width,
            body.height - notice_height,
        );
        if self.loading && self.captured_at.is_none() {
            frame.render_widget(
                Paragraph::new(local!(
                    self.language,
                    "モデル一覧を取得中.",
                    "Loading models."
                ))
                .style(Style::default().fg(theme::ACCENT)),
                body,
            );
        } else if self.captured_at.is_none()
            && let Some(error) = &self.error
        {
            frame.render_widget(
                Paragraph::new(match self.language {
                    UiLanguage::En => format!("Could not retrieve models: {error}\nCtrl+R Refresh"),
                    UiLanguage::Ja => format!("取得できません: {error}\nCtrl+Rで再取得"),
                })
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(theme::ATTENTION)),
                body,
            );
        } else if filtered.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.models.is_empty() {
                    local!(
                        self.language,
                        "接続先のモデル一覧は空です",
                        "The connection has no models"
                    )
                } else {
                    local!(
                        self.language,
                        "一致するモデルがありません",
                        "No models match"
                    )
                }),
                body,
            );
        } else {
            let items = filtered
                .iter()
                .map(|model| {
                    let label = match (self.language, self.current.as_ref() == Some(*model)) {
                        (UiLanguage::En, true) => format!("{model}  * Current"),
                        (UiLanguage::Ja, true) => format!("{model}  * 現在"),
                        (_, false) => (*model).clone(),
                    };
                    popup::item(&label, body.width)
                })
                .collect();
            popup::list(frame, body, items, self.selected);
        }
    }
}
impl UiState {
    pub(super) fn open_models(&mut self, argument: &str) -> Vec<UiAction> {
        let connection = if argument.is_empty() {
            self.view()
                .connection
                .clone()
                .or_else(|| self.config.default_connection.clone())
        } else {
            Some(argument.to_owned())
        };
        let Some(connection) = connection.filter(|name| self.config.connections.contains_key(name))
        else {
            let message = local!(
                self.config.ui.language,
                "/models [connection] に設定済みの接続名を指定してください",
                "Specify a configured connection name with /models [connection]"
            );
            return self.command_error(message);
        };
        self.next_model_request = self.next_model_request.wrapping_add(1);
        self.models = Some(ModelPicker {
            language: self.config.ui.language,
            paint: Default::default(),
            filtered_indices: Vec::new(),
            request: self.next_model_request,
            current: (self.view().connection.as_ref() == Some(&connection))
                .then(|| self.view().model.clone())
                .flatten(),
            connection: connection.clone(),
            models: vec![],
            query: String::new(),
            selected: 0,
            loading: true,
            error: None,
            captured_at: None,
            last_attempt_at: None,
            notice_scroll: None,
        });
        vec![UiAction::LoadModels {
            connection,
            request_id: self.next_model_request,
        }]
    }
    pub(super) fn models_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        let Some(picker) = self.models.as_mut() else {
            return vec![];
        };
        match picker.key(key, popup::viewport(self.viewport)) {
            Outcome::None => vec![],
            Outcome::Close => {
                self.models = None;
                vec![]
            }
            Outcome::Reload(connection) => {
                self.next_model_request = self.next_model_request.wrapping_add(1);
                let picker = self.models.as_mut().expect("model picker is open");
                picker.request = self.next_model_request;
                picker.loading = true;
                picker.paint.set(None);
                vec![UiAction::LoadModels {
                    connection,
                    request_id: self.next_model_request,
                }]
            }
            Outcome::Copy(text) => vec![UiAction::CopyText {
                text,
                scope: super::CopyScope::Value,
            }],
            Outcome::Select { connection, model } => {
                self.models = None;
                self.change_model(connection, model)
            }
        }
    }
    pub(super) fn change_model(&mut self, connection: String, model: String) -> Vec<UiAction> {
        if let Some(thread_id) = self.view().thread_id.clone() {
            vec![UiAction::Command(Command::ThreadSet(ThreadSetArgs {
                thread_id: Some(thread_id),
                connection: Some(connection),
                model: Some(model),
                ..Default::default()
            }))]
        } else {
            let view = self.active_view_mut();
            view.connection = Some(connection);
            view.model = Some(model);
            vec![]
        }
    }
}

#[cfg(test)]
mod wheel_tests {
    use super::*;
    #[test]
    fn failed_refresh_retains_facts_and_ignores_late_or_duplicate_results() {
        let mut picker = ModelPicker {
            language: UiLanguage::Ja,
            paint: Default::default(),
            filtered_indices: vec![],
            request: 1,
            connection: "mock".into(),
            current: None,
            models: vec![],
            query: String::new(),
            selected: 0,
            loading: true,
            error: None,
            captured_at: None,
            last_attempt_at: None,
            notice_scroll: None,
        };
        picker.accept(1, chrono::Utc::now(), Ok(vec!["saved-model".into()]));
        let captured = picker.captured_at;
        picker.request = 2;
        picker.loading = true;
        picker.accept(1, chrono::Utc::now(), Ok(vec!["late-model".into()]));
        assert!(picker.loading);
        assert_eq!(picker.models, ["saved-model"]);
        picker.accept(2, chrono::Utc::now(), Err("offline".into()));
        assert_eq!(picker.models, ["saved-model"]);
        assert_eq!(picker.captured_at, captured);
        assert!(picker.last_attempt_at >= captured);
        assert!(matches!(
            picker.key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                Rect::new(0, 0, 80, 24)
            ),
            Outcome::None
        ));
        picker.accept(2, chrono::Utc::now(), Ok(vec!["duplicate".into()]));
        assert_eq!(picker.models, ["saved-model"]);
        assert_eq!(picker.error.as_deref(), Some("offline"));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen: String = (0..24)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < 80 {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
                }
                line
            })
            .collect();
        assert!(screen.contains("saved-model"));
        assert!(screen.contains("再取得に失敗"));
        assert!(screen.contains("取得 UTC"));
    }

    #[test]
    fn prepared_model_filter_and_wheel_never_select_or_reload_a_model() {
        let mut picker = ModelPicker {
            language: UiLanguage::Ja,
            paint: Default::default(),
            filtered_indices: vec![],
            request: 1,
            connection: "mock".into(),
            current: None,
            models: vec![],
            query: String::new(),
            selected: 0,
            loading: true,
            error: None,
            captured_at: None,
            last_attempt_at: None,
            notice_scroll: None,
        };
        picker.accept(
            1,
            chrono::Utc::now(),
            Ok((0..30).map(|n| format!("model-{n:02}")).collect()),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, frame.area()))
            .unwrap();
        assert!(picker.paint_bounds().is_some());
        assert!(picker.wheel(3));
        assert_eq!(picker.filtered()[picker.selected], "model-03");
        assert!(picker.wheel(-100));
        assert!(!picker.wheel(-3));
        assert_eq!(picker.selected, 0);
        picker.paste("model-2");
        assert_eq!(picker.filtered_indices.len(), 10);
        assert!(picker.wheel(3));
        assert_eq!(picker.filtered()[picker.selected], "model-23");
        picker.request = 2;
        picker.loading = true;
        picker.accept(2, chrono::Utc::now(), Ok(vec!["other".into()]));
        assert!(picker.filtered_indices.is_empty());
        assert!(picker.paint_bounds().is_none());
        assert!(!picker.wheel(3));
    }
}

#[cfg(test)]
mod language_rendering_tests {
    use super::*;
    use crate::config::UiLanguage;

    #[test]
    fn japanese_notices_keep_original_wording_for_all_states() {
        use super::super::observation_notice::{
            Freshness, NextAction, ObservationKind, ObservationNotice,
        };
        let picker = ModelPicker {
            language: UiLanguage::Ja,
            paint: Default::default(),
            filtered_indices: vec![],
            request: 1,
            connection: "mock".into(),
            current: None,
            models: vec![],
            query: String::new(),
            selected: 0,
            loading: false,
            error: None,
            captured_at: None,
            last_attempt_at: None,
            notice_scroll: None,
        };
        for width in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(width);
            for freshness in [
                Freshness::Captured,
                Freshness::Stale,
                Freshness::FailedRefresh,
                Freshness::Unavailable,
            ] {
                let notice = ObservationNotice {
                    kind: ObservationKind::Models,
                    captured_at: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
                    last_attempt_at: Some(
                        chrono::DateTime::from_timestamp(1_700_000_001, 0).unwrap(),
                    ),
                    freshness,
                    next_action: NextAction::RefreshModels,
                    confirmation_enabled: false,
                };
                assert_eq!(
                    picker.notice_text(&notice),
                    crate::ui::text_width::glyphs::decorate(&notice.text())
                );
            }
        }
        crate::ui::text_width::set_terminal_width(None);
    }

    #[test]
    fn model_error_is_bilingual_without_rewriting_provider_text() {
        fn configure(ui: &mut UiState) {
            ui.models = Some(ModelPicker {
                language: ui.config.ui.language,
                paint: Default::default(),
                filtered_indices: vec![],
                request: 1,
                connection: "mock".into(),
                current: None,
                models: vec![],
                query: String::new(),
                selected: 0,
                loading: false,
                error: Some("PROVIDER-ERROR: opaque detail".into()),
                captured_at: None,
                last_attempt_at: None,
                notice_scroll: None,
            });
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Could not retrieve", "PROVIDER-ERROR: opaque detail"],
                UiLanguage::Ja => &["取得できません", "PROVIDER-ERROR: opaque detail"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn narrow_model_picker_keeps_count_and_primary_actions_visible() {
        use crate::ui::text_width::UnicodeWidthStr;
        use ratatui::{Terminal, backend::TestBackend};
        for observed in [Some(1), Some(2), None] {
            crate::ui::text_width::set_terminal_width(observed);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let picker = ModelPicker {
                    language,
                    paint: Default::default(),
                    filtered_indices: vec![0, 1],
                    request: 1,
                    connection: "proxy".into(),
                    current: None,
                    models: vec!["model-a".into(), "model-b".into()],
                    query: String::new(),
                    selected: 1,
                    loading: false,
                    error: None,
                    captured_at: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
                    last_attempt_at: None,
                    notice_scroll: None,
                };
                let mut terminal = Terminal::new(TestBackend::new(35, 18)).unwrap();
                terminal
                    .draw(|frame| picker.draw(frame, frame.area()))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let mut screen = String::new();
                for y in 0..18 {
                    let mut x = 0;
                    while x < 35 {
                        let symbol = buffer[(x, y)].symbol();
                        screen.push_str(symbol);
                        x += symbol.width().max(1) as u16;
                    }
                    screen.push('\n');
                }
                let (count, back, select) = match language {
                    UiLanguage::En => ("2 / 2 models", "Esc Back", "Enter Change"),
                    UiLanguage::Ja => ("2 / 2件", "Esc 戻る", "Enter 変更"),
                };
                for phrase in [count, back, select] {
                    assert!(
                        screen.contains(phrase),
                        "{observed:?} {language:?}: missing {phrase:?}: {screen}"
                    );
                }
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn multiple_model_count_is_bilingual_at_standard_widths() {
        fn configure(ui: &mut UiState) {
            ui.models = Some(ModelPicker {
                language: ui.config.ui.language,
                paint: Default::default(),
                filtered_indices: vec![0, 1],
                request: 1,
                connection: "mock".into(),
                current: None,
                models: vec!["model-one".into(), "model-two".into()],
                query: String::new(),
                selected: 0,
                loading: false,
                error: None,
                captured_at: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
                last_attempt_at: None,
                notice_scroll: None,
            });
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["1 / 2 models", "model-one"],
                UiLanguage::Ja => &["1 / 2件", "model-one"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn single_model_count_is_bilingual_at_standard_widths() {
        fn configure(ui: &mut UiState) {
            ui.models = Some(ModelPicker {
                language: ui.config.ui.language,
                paint: Default::default(),
                filtered_indices: vec![0],
                request: 1,
                connection: "mock".into(),
                current: None,
                models: vec!["fixture-model".into()],
                query: String::new(),
                selected: 0,
                loading: false,
                error: None,
                captured_at: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
                last_attempt_at: None,
                notice_scroll: None,
            });
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["1 / 1 model", "fixture-model"],
                UiLanguage::Ja => &["1 / 1件", "fixture-model"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn loading_model_picker_is_bilingual_at_standard_widths() {
        fn configure(ui: &mut UiState) {
            ui.models = Some(ModelPicker {
                language: ui.config.ui.language,
                paint: Default::default(),
                filtered_indices: vec![],
                request: 1,
                connection: "mock".into(),
                current: None,
                models: vec![],
                query: String::new(),
                selected: 0,
                loading: true,
                error: None,
                captured_at: None,
                last_attempt_at: None,
                notice_scroll: None,
            });
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["0 / 0 models", "Loading models", "Ctrl+R"],
                UiLanguage::Ja => &["0 / 0件", "モデル一覧を取得中", "Ctrl+R"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }
}
