//! A live model picker; no built-in model/capability catalog.
use super::*;
use crate::protocol::ThreadSetArgs;

const FOOTER: &str = "Esc 戻る · Enter 変更\n↑↓ 選択 · Ctrl+R 再取得 · Ctrl+Y コピー";
pub(super) struct ModelPicker {
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
}
enum Outcome {
    None,
    Close,
    Reload(String),
    Copy(String),
    Select { connection: String, model: String },
}
impl ModelPicker {
    fn key(&mut self, key: KeyEvent, area: Rect) -> Outcome {
        let page = usize::from(popup::body_area(area, "モデル一覧", FOOTER, None).height)
            .saturating_sub(1)
            .max(1);
        if key.code == KeyCode::Esc {
            return Outcome::Close;
        }
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Reload(self.connection.clone());
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

    pub(super) fn accept(&mut self, request: u64, result: Result<Vec<String>, String>) {
        if request != self.request {
            return;
        }
        self.paint.set(None);
        self.loading = false;
        match result {
            Ok(models) => {
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
        if self.loading || self.error.is_some() {
            return false;
        }
        let old = self.selected;
        self.selected = popup::move_selection(old, delta, self.filtered_indices.len());
        old != self.selected
    }
    pub(super) fn paste(&mut self, text: &str) {
        let available = 256usize.saturating_sub(self.query.chars().count());
        self.query
            .extend(text.chars().filter(|c| !c.is_control()).take(available));
        self.selected = 0;
        self.rebuild_filter();
    }
    pub(super) fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        let filtered = self.filtered();
        let context = format!(
            "{} · 絞込み: {} · {} / {}件",
            self.connection,
            self.query,
            if filtered.is_empty() {
                0
            } else {
                self.selected + 1
            },
            filtered.len()
        );
        let body = popup::panel_recorded(frame, area, "モデル", &context, FOOTER, &self.paint);
        if self.loading {
            frame.render_widget(
                Paragraph::new("モデル一覧を取得中…").style(Style::default().fg(theme::ACCENT)),
                body,
            );
        } else if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(format!("取得できません: {error}\nCtrl+Rで再取得"))
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(theme::ATTENTION)),
                body,
            );
        } else if filtered.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.models.is_empty() {
                    "接続先のモデル一覧は空です"
                } else {
                    "一致するモデルがありません"
                }),
                body,
            );
        } else {
            let items = filtered
                .iter()
                .map(|model| {
                    let label = format!(
                        "{}{}",
                        model,
                        if self.current.as_ref() == Some(*model) {
                            "  ● 現在"
                        } else {
                            ""
                        }
                    );
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
            return self.command_error("/models [connection] に設定済みの接続名を指定してください");
        };
        self.next_model_request = self.next_model_request.wrapping_add(1);
        self.models = Some(ModelPicker {
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
            Outcome::Reload(connection) => self.open_models(&connection),
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
    fn prepared_model_filter_and_wheel_never_select_or_reload_a_model() {
        let mut picker = ModelPicker {
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
        };
        picker.accept(1, Ok((0..30).map(|n| format!("model-{n:02}")).collect()));
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
        picker.accept(1, Ok(vec!["other".into()]));
        assert!(picker.filtered_indices.is_empty());
        assert!(picker.paint_bounds().is_none());
        assert!(!picker.wheel(3));
    }
}
