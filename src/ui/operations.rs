//! Typed TUI operation forms. This module never parses arbitrary JSON or runs CLI.
use super::i18n::local;
use super::{activity::ActivityPhase, popup, prev_grapheme, theme};
use crate::config::UiLanguage;
use crate::operations::{CompactMode, DefaultOrigin, Effect, FieldId, OperationId};
use crate::protocol::*;
mod client_forms;
mod metadata;
use crate::client_operation::ClientOperation;
#[cfg(test)]
mod owner_tests;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};
use std::path::{Path, PathBuf};

pub(super) fn catalogue_label(id: OperationId, language: UiLanguage) -> String {
    metadata::display_label(id, language)
}
pub(super) fn catalogue_meaning(id: OperationId, language: UiLanguage) -> &'static str {
    metadata::text_for(id.meaning_text(), language)
}

// A smaller initial management page leaves room to inspect each row in the TUI.
const MANAGEMENT_LIST_LIMIT: u32 = 20;
const ATTACHMENT_CHOICES: &[&str] = &[
    AttachmentKind::File.as_str(),
    AttachmentKind::Image.as_str(),
];
const INDEX_SCOPE_CHOICES: &[&str] = &["", IndexScope::Threads.as_str(), IndexScope::All.as_str()];
const STORAGE_CHOICES: &[&str] = &[
    StorageCategory::All.as_str(),
    StorageCategory::Raw.as_str(),
    StorageCategory::ManagedImages.as_str(),
    StorageCategory::History.as_str(),
    StorageCategory::Index.as_str(),
];

// One Rust variant declaration supplies both the enum and its complete list.
// Form fields/builders remain ordinary typed Rust; this is not a form DSL.
macro_rules! operation_kinds {
    ($($name:ident),+ ; $($client:ident),+ $(,)?)=> {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub(super) enum OpKind {$($name),+,$($client),+}
        impl OpKind {fn catalogue(self)->OperationId {match self {$(Self::$name=>OperationId::$name),+,$(Self::$client=>OperationId::$client),+}} #[cfg(test)] pub const OWNER:&'static [Self]=&[$(Self::$name),+];pub const ALL:&'static [Self]=&[$(Self::$name),+,$(Self::$client),+];}
    };
}
operation_kinds! {
ThreadList,
    ThreadGet,
    UsageGet,
    ThreadCreate,
    ThreadFork,
    ThreadDelete,
    ThreadSet,
    MessageSend,
    MessageGet,
    OperationGet,
    RunGet,
    RunRetry,
    RunStop,
    TurnGet,
    Wait,
    HistoryList,
    HistoryGet,
    HistorySearch,
    ShellList,
    ShellGet,
    ShellRead,
    ShellWrite,
    ShellEof,
    ShellStop,
    AttachmentAdd,
    AttachmentList,
    AttachmentRemove,
    CompactList,
    Compact,
    IndexStatus,
    IndexBackfill,
    IndexRetry,
    IndexRebuild,
    StorageStatus,
    StorageCollect,
    ModelList,
    HistoryRelations,
    ThreadDeletePreview,
    Status,
    MessageCancel,
    IndexPreview,
    IndexProbe;
    Exec,ConfigInit,ConfigCheck,DocsList,DocsGet,DocsSearch,RagInit,RagSourceAdd,RagSourceList,RagSourceRemove,RagIndex,RagIngest,RagStatus,RagInspect,RagSearch,RagGet,
}
/// Display-field references; not command routing or a public protocol target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetKind {
    Thread,
    Node,
    Message,
    Operation,
    Run,
    Turn,
    Execution,
}
impl TargetKind {
    pub(super) fn from_field_key(key: &str) -> Option<Self> {
        match key.rsplit(['/', '.']).next().unwrap_or(key) {
            "thread_id" => Some(Self::Thread),
            "node_id" => Some(Self::Node),
            "message_id" => Some(Self::Message),
            "operation_id" | "request_operation_id" => Some(Self::Operation),
            "run_id" | "owner_run_id" => Some(Self::Run),
            "turn_id" => Some(Self::Turn),
            "execution_id" => Some(Self::Execution),
            _ => None,
        }
    }
    pub(super) fn detail_operation(self) -> Option<OpKind> {
        match self {
            Self::Thread => Some(OpKind::ThreadGet),
            Self::Node => None,
            Self::Message => Some(OpKind::MessageGet),
            Self::Operation => Some(OpKind::OperationGet),
            Self::Run => Some(OpKind::RunGet),
            Self::Turn => Some(OpKind::TurnGet),
            Self::Execution => Some(OpKind::ShellGet),
        }
    }
}
impl Context {
    pub(super) fn set_reference(&mut self, target: TargetKind, value: String) {
        match target {
            TargetKind::Thread => self.thread = value,
            TargetKind::Node => self.node = value,
            TargetKind::Message => self.message = value,
            TargetKind::Operation => self.operation = value,
            TargetKind::Run => self.run = value,
            TargetKind::Turn => self.turn = value,
            TargetKind::Execution => self.execution = value,
        }
    }
}
impl OpKind {
    #[cfg(test)]
    pub fn label(self) -> String {
        metadata::display_label(self.catalogue(), UiLanguage::Ja)
    }
    #[cfg(test)]
    fn wire_name(self) -> String {
        self.catalogue()
            .primary_cli()
            .path
            .join(".")
            .replace('-', "_")
    }
    fn label_for(self, language: UiLanguage) -> String {
        metadata::display_label(self.catalogue(), language)
    }
    fn shortcut_for(self, language: UiLanguage) -> Option<&'static str> {
        if self == Self::AttachmentAdd {
            Some(local!(language, "下書きへ: /attach", "To draft: /attach"))
        } else {
            self.shortcut()
        }
    }
    fn display_shortcut_for(self, language: UiLanguage) -> Option<String> {
        self.shortcut_for(language).map(|shortcut| {
            let shortcut = match shortcut {
                "/index > B" => crate::ui::text_width::glyphs::select("/index → B", shortcut),
                "/index > T" => crate::ui::text_width::glyphs::select("/index → T", shortcut),
                "/index > R" => crate::ui::text_width::glyphs::select("/index → R", shortcut),
                _ => shortcut,
            };
            crate::ui::text_width::glyphs::decorate(shortcut)
        })
    }
    fn cli(self) -> String {
        format!("shellboy {}", self.catalogue().primary_cli().path.join(" "))
    }
    fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::ThreadList => Some("/threads"),
            Self::ThreadFork => Some("/fork"),
            Self::ThreadSet => Some("/model | /effort"),
            Self::MessageSend => Some("Enter"),
            Self::RunRetry => Some("/retry"),
            Self::RunStop => Some("/stop | Ctrl+C"),
            Self::HistorySearch => Some("/search"),
            Self::Compact => Some("/compact"),
            Self::AttachmentList => Some("/attachments"),
            Self::AttachmentRemove => Some("/detach"),
            Self::IndexStatus => Some("/index"),
            Self::IndexBackfill => Some("/index > B"),
            Self::IndexRetry => Some("/index > T"),
            Self::IndexRebuild => Some("/index > R"),
            Self::ModelList => Some("/models"),
            Self::Status => Some("/status"),
            _ => None,
        }
    }
}
#[derive(Default)]
pub(super) struct Context {
    pub phase: ActivityPhase,
    pub follows_current: bool,
    pub thread: String,
    pub node: String,
    pub message: String,
    pub operation: String,
    pub run: String,
    pub current_run: String,
    pub turn: String,
    pub execution: String,
    pub cwd: PathBuf,
}
#[derive(Clone, Copy)]
enum Kind {
    Text,
    Number,
    Boolean,
    Choice(&'static [&'static str]),
    Multiline,
}
struct Field {
    name: &'static str,
    label: &'static str,
    value: String,
    initial_value: String,
    kind: Kind,
}
impl Field {
    fn new(name: &'static str, label: &'static str, value: impl Into<String>, kind: Kind) -> Self {
        let value = value.into();
        Self {
            name,
            label,
            initial_value: value.clone(),
            value,
            kind,
        }
    }
    fn displayed_for(&self, language: UiLanguage) -> &str {
        if !matches!(self.kind, Kind::Boolean | Kind::Choice(_)) {
            return &self.value;
        }
        match self.value.as_str() {
            "true" => local!(language, "はい", "Yes"),
            "false" => local!(language, "いいえ", "No"),
            "" => local!(language, "未指定", "Not specified"),
            "thread_settled" => local!(
                language,
                "会話の完了・復旧・停止",
                "Conversation completed, awaiting recovery, or stopped"
            ),
            "turn_completed" => local!(language, "応答処理の回答完了", "Turn answer completed"),
            "run_ended" => local!(language, "作業の終了", "Run ended"),
            "shell_ended" => local!(language, "コマンドの終了", "Command ended"),
            "shell_output" => local!(language, "コマンド出力の更新", "Command output updated"),
            "threads" => local!(language, "指定した会話", "Specified conversations"),
            "all" => local!(language, "全体", "All"),
            "raw" => local!(language, "保存出力", "Saved output"),
            "managed_images" => local!(language, "管理画像", "Managed images"),
            "history" => local!(language, "履歴", "History"),
            "index" => local!(language, "索引", "Index"),
            "file" => local!(language, "ファイル", "File"),
            "image" => local!(language, "画像", "Image"),
            "keep" => local!(language, "変更しない", "Keep unchanged"),
            "set" => local!(language, "設定する", "Set"),
            "auto" => local!(language, "自動", "Automatic"),
            "deferred" => local!(language, "次の境界で圧縮", "Compact at next boundary"),
            "maximal" => local!(language, "自動で最大圧縮", "Maximum automatic compaction"),
            "spill" => local!(language, "指定範囲を退避", "Spill selected range"),
            "summary" => local!(language, "指定範囲を要約", "Summarize selected range"),
            "trim" => local!(language, "指定末尾まで除去", "Trim through selected end"),
            "omit" => local!(language, "指定範囲を省略", "Omit selected range"),
            "restore" => local!(
                language,
                "省略・退避を復元",
                "Restore omitted or spilled range"
            ),
            "previous" => local!(language, "直前", "Previous"),
            "next" => local!(language, "直後", "Next"),
            "sources" => local!(language, "出典", "Sources"),
            "replacements" => local!(language, "置換関係", "Replacements"),
            _ => &self.value,
        }
    }
    #[cfg(test)]
    fn displayed(&self) -> &str {
        self.displayed_for(UiLanguage::Ja)
    }
}

pub(super) struct Operations {
    language: UiLanguage,
    selected: usize,
    form: Option<Form>,
    context: Context,
    page: std::cell::Cell<usize>,
    filter: String,
    cached_rows: std::cell::OnceCell<Vec<OpKind>>,
    paint: std::cell::Cell<Option<popup::PaintBounds>>,
    #[cfg(test)]
    row_builds: std::cell::Cell<usize>,
}
type BuildCommand = fn(&Form, &Path) -> Result<Command, String>;
#[derive(Clone, Copy)]
enum FormBuilder {
    Owner(BuildCommand),
    Client(fn(&Form, &Path) -> Result<ClientOperation, String>),
}

struct Form {
    language: UiLanguage,
    operation: OpKind,
    fields: Vec<Field>,
    build: FormBuilder,
    selected: usize,
    conditions: Vec<WaitCondition>,
    error: Option<String>,
    follows_current: bool,
    help: bool,
    help_scroll: std::cell::Cell<u16>,
    help_page: std::cell::Cell<u16>,
    client_confirmation: Option<ClientOperation>,
    reader: std::cell::RefCell<popup::ReaderCache>,
    help_revision: u64,
}
pub(super) enum Outcome {
    None,
    Close,
    Execute(Command),
    Client(ClientOperation),
}
impl Operations {
    pub fn new(context: Context) -> Self {
        let mut result = Self {
            language: UiLanguage::Ja,
            selected: 0,
            form: None,
            context,
            page: std::cell::Cell::new(8),
            filter: String::new(),
            cached_rows: std::cell::OnceCell::new(),
            paint: std::cell::Cell::new(None),
            #[cfg(test)]
            row_builds: std::cell::Cell::new(0),
        };
        if let Some(first) = result.rows().first() {
            result.selected = OpKind::ALL.iter().position(|kind| kind == first).unwrap();
        }
        result
    }
    pub(super) fn set_language(&mut self, language: UiLanguage) {
        if self.language == language {
            return;
        }
        self.language = language;
        self.cached_rows.take();
        if let Some(form) = &mut self.form {
            form.language = language;
            let mut reader = form.reader.borrow_mut();
            reader.set_language(language);
            reader.clear();
            form.help_revision = form.help_revision.wrapping_add(1);
        }
        self.ensure_selection();
    }
    fn suggested(&self) -> &'static [OpKind] {
        use OpKind::*;
        if self.context.thread.is_empty() {
            return &[ThreadCreate, ModelList, Status];
        }
        if self.context.phase == ActivityPhase::Recovery && !self.context.current_run.is_empty() {
            return &[RunRetry, RunStop, RunGet, TurnGet, MessageSend];
        }
        if self.context.phase == ActivityPhase::Stopping {
            return &[RunGet, Status];
        }
        if self.context.phase.is_active() && !self.context.current_run.is_empty() {
            return &[RunGet, RunStop, Status];
        }
        &[MessageSend, HistoryList, HistorySearch]
    }
    fn common(&self) -> &'static [OpKind] {
        use OpKind::*;
        if self.context.thread.is_empty() {
            &[ModelList, Status]
        } else {
            &[
                ThreadFork,
                Compact,
                AttachmentAdd,
                ThreadSet,
                ModelList,
                Status,
            ]
        }
    }
    fn section(&self, kind: OpKind) -> u8 {
        if self.suggested().contains(&kind) {
            0
        } else if self.common().contains(&kind) {
            1
        } else {
            2
        }
    }
    fn rows(&self) -> &[OpKind] {
        self.cached_rows.get_or_init(|| {
            #[cfg(test)]
            self.row_builds.set(self.row_builds.get() + 1);
            self.build_rows()
        })
    }
    fn build_rows(&self) -> Vec<OpKind> {
        let query = self.filter.to_lowercase();
        let mut rows = Vec::with_capacity(OpKind::ALL.len());
        for &kind in self
            .suggested()
            .iter()
            .chain(self.common())
            .chain(OpKind::ALL)
        {
            if !rows.contains(&kind)
                && (query.is_empty()
                    || format!(
                        "{} {} {} {}",
                        kind.label_for(self.language),
                        kind.cli(),
                        kind.shortcut_for(self.language).unwrap_or(""),
                        metadata::text_for(kind.catalogue().meaning_text(), self.language)
                    )
                    .to_lowercase()
                    .contains(&query))
            {
                rows.push(kind);
            }
        }
        rows
    }
    fn ensure_selection(&mut self) {
        let rows = self.rows();
        if !rows.contains(&OpKind::ALL[self.selected])
            && let Some(kind) = rows.first()
        {
            self.selected = OpKind::ALL
                .iter()
                .position(|candidate| candidate == kind)
                .unwrap();
        }
    }
    fn filter_text(&mut self, text: &str) {
        let before = self.filter.len();
        let remaining = 128usize.saturating_sub(self.filter.chars().count());
        self.filter
            .extend(text.chars().filter(|ch| !ch.is_control()).take(remaining));
        if self.filter.len() != before {
            self.cached_rows.take();
        }
        self.ensure_selection();
    }
    pub fn refresh_activity(&mut self, phase: ActivityPhase, run: String, turn: String) {
        let previous = self.suggested();
        self.context.phase = phase;
        if self.context.follows_current {
            self.context.run = run.clone();
            self.context.turn = turn;
        }
        self.context.current_run = run;
        if previous != self.suggested() {
            self.cached_rows.take();
        }
        self.ensure_selection();
    }
    pub fn select(&mut self, operation: OpKind) {
        self.selected = OpKind::ALL
            .iter()
            .position(|kind| *kind == operation)
            .expect("every operation is listed");
        let mut form = Form::new(operation, &self.context);
        form.language = self.language;
        form.reader.get_mut().set_language(self.language);
        self.form = Some(form);
    }
    pub fn paste(&mut self, text: &str) {
        if let Some(form) = &mut self.form {
            form.insert(text);
        } else {
            self.filter_text(text);
        }
    }
    pub(super) fn hint_context(&self) -> super::key_hints::Context {
        use super::key_hints::Context;
        match &self.form {
            Some(form) if form.client_confirmation.is_some() => Context::Confirmation,
            Some(form) if form.help => Context::Help,
            Some(form) => Context::Form(form.operation.catalogue().enter_verb(form.effect())),
            None => Context::Popup,
        }
    }
    pub(super) fn owns_form_help(&self) -> bool {
        self.form
            .as_ref()
            .is_some_and(|form| form.client_confirmation.is_none())
    }
    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        if let Some(form) = &mut self.form {
            if form.client_confirmation.is_some() {
                return form.key(key, &self.context.cwd);
            }
            if form.help {
                if matches!(key.code, KeyCode::Esc | KeyCode::F(1)) {
                    form.help = false;
                    form.reader.borrow_mut().clear();
                    return Outcome::None;
                }
                let current = form.help_scroll.get();
                let next = match key.code {
                    KeyCode::Home => Some(0),
                    KeyCode::End => Some(u16::MAX),
                    _ => popup::reader_key(key.code, current, form.help_page.get()),
                };
                if let Some(next) = next {
                    form.help_scroll.set(next);
                }
                return Outcome::None;
            }
            if key.code == KeyCode::F(1) {
                form.help = true;
                form.help_scroll.set(0);
                return Outcome::None;
            }
            if key.code == KeyCode::Esc {
                self.form = None;
                return Outcome::None;
            }
            match key.code {
                KeyCode::PageUp => {
                    form.selected = form.selected.saturating_sub(self.page.get());
                    form.help_revision = form.help_revision.wrapping_add(1);
                    return Outcome::None;
                }
                KeyCode::PageDown => {
                    form.selected = form
                        .selected
                        .saturating_add(self.page.get())
                        .min(form.fields.len().saturating_sub(1));
                    form.help_revision = form.help_revision.wrapping_add(1);
                    return Outcome::None;
                }
                _ => {}
            }
            return form.key(key, &self.context.cwd);
        }
        let selected = self
            .rows()
            .iter()
            .position(|kind| *kind == OpKind::ALL[self.selected])
            .unwrap_or(0);
        let count = self.rows().len();
        let next = match key.code {
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.cached_rows.take();
                self.ensure_selection();
                return Outcome::None;
            }
            KeyCode::Esc | KeyCode::F(8) => return Outcome::Close,
            KeyCode::Up => Some(selected.saturating_sub(1)),
            KeyCode::Down => Some((selected + 1).min(count.saturating_sub(1))),
            KeyCode::PageUp => Some(selected.saturating_sub(self.page.get())),
            KeyCode::PageDown => Some(
                selected
                    .saturating_add(self.page.get())
                    .min(count.saturating_sub(1)),
            ),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(count.saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(kind) = self.rows().get(selected).copied() {
                    let mut form = Form::new(kind, &self.context);
                    form.language = self.language;
                    form.reader.get_mut().set_language(self.language);
                    self.form = Some(form);
                }
                return Outcome::None;
            }
            KeyCode::Backspace => {
                self.filter
                    .truncate(prev_grapheme(&self.filter, self.filter.len()));
                self.cached_rows.take();
                self.ensure_selection();
                None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.clear();
                self.cached_rows.take();
                self.ensure_selection();
                None
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter_text(&ch.to_string());
                None
            }
            _ => None,
        };
        if let Some(kind) = next.and_then(|index| self.rows().get(index)).copied() {
            self.selected = OpKind::ALL
                .iter()
                .position(|candidate| *candidate == kind)
                .unwrap();
        }
        Outcome::None
    }

    pub(super) fn paint_bounds(&self) -> Option<popup::PaintBounds> {
        self.paint.get()
    }
    pub(super) fn wheel(&mut self, delta: isize) -> bool {
        if let Some(form) = &mut self.form {
            if form.help || form.client_confirmation.is_some() {
                let old = form.help_scroll.get();
                let next = popup::move_reader(old, delta, form.reader.borrow().bounds().maximum);
                form.help_scroll.set(next);
                return old != next;
            }
            let next = popup::move_selection(form.selected, delta, form.fields.len());
            if next == form.selected {
                return false;
            }
            form.selected = next;
            form.help_revision = form.help_revision.wrapping_add(1);
            return true;
        }
        let Some(old) = self
            .rows()
            .iter()
            .position(|kind| *kind == OpKind::ALL[self.selected])
        else {
            return false;
        };
        let next = popup::move_selection(old, delta, self.rows().len());
        if old == next {
            return false;
        }
        let kind = self.rows()[next];
        self.selected = OpKind::ALL
            .iter()
            .position(|candidate| *candidate == kind)
            .expect("listed operation");
        true
    }
    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        let sep = crate::ui::text_width::glyphs::separator();
        if let Some(form) = &self.form {
            if let Some(operation) = &form.client_confirmation {
                let label = form.operation.label_for(form.language);
                let title = match form.language {
                    UiLanguage::En => format!("{label}{sep}Confirm"),
                    UiLanguage::Ja => format!("{label}{sep}確認"),
                };
                let footer = crate::ui::text_width::glyphs::controls(local!(
                    form.language,
                    "Y 実行 | N/Esc 入力欄へ | ^v/PgUp/PgDn 説明を読む",
                    "Y Run | N/Esc Back to form | ^v/PgUp/PgDn Scroll"
                ));
                let body = popup::panel_recorded(
                    frame,
                    area,
                    &title,
                    &form.operation.cli(),
                    &footer,
                    &self.paint,
                );
                form.help_page.set(body.height.max(1));
                form.help_scroll.set(form.reader.borrow_mut().render(
                    frame,
                    body,
                    form.help_revision.wrapping_mul(2).wrapping_add(1),
                    form.help_scroll.get(),
                    || client_forms::confirmation_lines_for(operation, form.language),
                ));
                return;
            }
            if form.help {
                let label = form.operation.label_for(form.language);
                let title = match form.language {
                    UiLanguage::En => format!("{label}{sep}Guide"),
                    UiLanguage::Ja => format!("{label}{sep}使い方"),
                };
                let footer = crate::ui::text_width::glyphs::controls(local!(
                    form.language,
                    "Esc/F1 入力欄へ | ^v/PgUp/PgDn 説明を読む",
                    "Esc/F1 Back to form | ^v/PgUp/PgDn Scroll"
                ));
                let body = popup::panel_recorded(
                    frame,
                    area,
                    &title,
                    &form.operation.cli(),
                    &footer,
                    &self.paint,
                );
                form.help_page.set(body.height.max(1));
                form.help_scroll.set(form.reader.borrow_mut().render(
                    frame,
                    body,
                    form.help_revision.wrapping_mul(2),
                    form.help_scroll.get(),
                    || form.help_lines(),
                ));
                return;
            }
            let selected = form.selected + 1;
            let count = form.fields.len();
            let field_context = if form.fields.is_empty() {
                local!(form.language, "入力項目なし", "No input fields").into()
            } else {
                match form.language {
                    UiLanguage::En => format!("Field {selected}/{count}"),
                    UiLanguage::Ja => format!("{selected}/{count}項目"),
                }
            };
            let effect = metadata::text_for(form.effect().text(), form.language);
            let conditions = form.conditions.len();
            let context = match (form.language, form.operation == OpKind::Wait, conditions) {
                (UiLanguage::En, true, 1) => {
                    format!("{effect}{sep}{field_context}{sep}1 observation condition")
                }
                (UiLanguage::En, true, _) => {
                    format!("{effect}{sep}{field_context}{sep}{conditions} observation conditions")
                }
                (UiLanguage::Ja, true, _) => {
                    format!("{effect}{sep}{field_context}{sep}観測条件 {conditions}件")
                }
                (_, false, _) => format!("{effect}{sep}{field_context}"),
            };
            let verb = metadata::text_for(
                form.operation.catalogue().enter_verb(form.effect()).text(),
                form.language,
            );
            let footer = match (form.language, form.operation == OpKind::Wait) {
                (UiLanguage::En, true) => crate::ui::text_width::glyphs::formatted!(
                    "Esc List{sep}Enter {verb}{sep}F1 Guide and IDs\nTab/↑↓/PgUp/PgDn Select{sep}F9 Add condition{sep}F10 Remove",
                    "Esc List{sep}Enter {verb}{sep}F1 Guide and IDs\nTab/^v/PgUp/PgDn Select{sep}F9 Add condition{sep}F10 Remove"
                ),
                (UiLanguage::En, false) => crate::ui::text_width::glyphs::formatted!(
                    "Esc List{sep}Enter {verb}{sep}F1 Guide and IDs\nTab/↑↓/PgUp/PgDn Select{sep}←→ Change choice{sep}Ctrl+U Clear",
                    "Esc List{sep}Enter {verb}{sep}F1 Guide and IDs\nTab/^v/PgUp/PgDn Select{sep}<> Change choice{sep}Ctrl+U Clear"
                ),
                (UiLanguage::Ja, true) => crate::ui::text_width::glyphs::formatted!(
                    "Esc 一覧{sep}Enter {verb}{sep}F1 説明とIDの探し方\nTab/↑↓/PgUp/PgDn 選択{sep}F9条件追加{sep}F10削除",
                    "Esc 一覧{sep}Enter {verb}{sep}F1 説明とIDの探し方\nTab/^v/PgUp/PgDn 選択{sep}F9条件追加{sep}F10削除"
                ),
                (UiLanguage::Ja, false) => crate::ui::text_width::glyphs::formatted!(
                    "Esc 一覧{sep}Enter {verb}{sep}F1 説明とIDの探し方\nTab/↑↓/PgUp/PgDn 選択{sep}←→選択値{sep}Ctrl+U消去",
                    "Esc 一覧{sep}Enter {verb}{sep}F1 説明とIDの探し方\nTab/^v/PgUp/PgDn 選択{sep}<>選択値{sep}Ctrl+U消去"
                ),
            };
            let height = form
                .fields
                .len()
                .saturating_mul(3)
                .saturating_add(11 + usize::from(form.error.is_some()) * 2)
                .min(u16::MAX as usize) as u16;
            let mut body = popup::compact_recorded(
                frame,
                area,
                &form.operation.label_for(form.language),
                &context,
                &footer,
                height,
                &self.paint,
            );
            if body.height >= 4 {
                let info_height = body.height.saturating_sub(2).min(3);
                let info = vec![
                    Line::from(metadata::text_for(
                        form.operation.catalogue().meaning_text(),
                        form.language,
                    )),
                    Line::from(form.field_hint()),
                ];
                frame.render_widget(
                    Paragraph::new(info).wrap(ratatui::widgets::Wrap { trim: false }),
                    Rect {
                        height: info_height,
                        ..body
                    },
                );
                body.y += info_height;
                body.height -= info_height;
            }
            if let Some(error) = &form.error {
                let lines = super::presentation::hanging_wrap(
                    vec![Line::from(crate::presentation::sanitize_plain(error, 0).0)],
                    body.width.saturating_sub(1).max(1),
                );
                let height = (lines.len().min(usize::from(body.height.saturating_sub(1)))) as u16;
                frame.render_widget(
                    Paragraph::new(lines).style(Style::default().fg(theme::ATTENTION)),
                    Rect { height, ..body },
                );
                body.y += height;
                body.height -= height;
            }
            self.page
                .set(usize::from(body.height).saturating_sub(1).max(1));
            let fields = form
                .fields
                .iter()
                .map(|field| {
                    popup::item(
                        &crate::presentation::sanitize_plain(
                            &format!(
                                "{}: {}",
                                form.field_label_for(field),
                                field.displayed_for(form.language)
                            ),
                            0,
                        )
                        .0,
                        body.width,
                    )
                })
                .collect();
            popup::list(frame, body, fields, form.selected);
        } else {
            let rows = self.rows();
            let selected = rows
                .iter()
                .position(|kind| *kind == OpKind::ALL[self.selected])
                .unwrap_or(0);
            let source = if self.context.thread.is_empty() {
                local!(self.language, "新しい会話", "New conversation")
            } else if self.context.phase == ActivityPhase::Recovery {
                local!(
                    self.language,
                    "復旧待ちの会話",
                    "Conversation awaiting recovery"
                )
            } else if self.context.phase.is_active() {
                local!(self.language, "実行中の会話", "Running conversation")
            } else {
                local!(self.language, "この会話", "Current conversation")
            };
            let cli = rows
                .get(selected)
                .map(|kind| kind.cli())
                .unwrap_or_default();
            let position = if rows.is_empty() { 0 } else { selected + 1 };
            let count = rows.len();
            let context = match (self.language, self.filter.is_empty()) {
                (UiLanguage::En, true) => {
                    format!(
                        "{cli}{sep}{position}/{count}{sep}CLI actions{sep}{source}{sep}Filter: none"
                    )
                }
                (UiLanguage::En, false) => format!(
                    "{cli}{sep}{position}/{count}{sep}CLI actions{sep}{source}{sep}Filter: {}",
                    self.filter
                ),
                (UiLanguage::Ja, true) => format!(
                    "{cli}{sep}{position}/{count}件{sep}CLIと同じ操作{sep}{source}{sep}絞込み: 未指定"
                ),
                (UiLanguage::Ja, false) => format!(
                    "{cli}{sep}{position}/{count}件{sep}CLIと同じ操作{sep}{source}{sep}絞込み: {}",
                    self.filter
                ),
            };
            let title = local!(self.language, "操作", "Actions");
            let footer = crate::ui::text_width::glyphs::controls(local!(
                self.language,
                "絞込み | ^v/PgUp/PgDn | Ctrl+U解除 | Esc戻る/F8閉\nEnter 入力フォーム | [変更]保存 [制御]処理",
                "Filter | ^v/PgUp/PgDn | Ctrl+U Clear | Esc Back/F8 Close\nEnter Form | [Change] Saved data [Control] Work"
            ));
            let body = popup::panel_recorded(frame, area, title, &context, &footer, &self.paint);
            self.page
                .set(usize::from(body.height).saturating_sub(1).max(1));
            if rows.is_empty() {
                frame.render_widget(
                    Paragraph::new(local!(
                        self.language,
                        "該当する操作はありません。Ctrl+Uで絞込みを解除できます",
                        "No actions match. Ctrl+U clears the filter."
                    )),
                    body,
                );
            } else {
                let mut previous = None;
                let mut items = Vec::new();
                let mut selected_row = 0;
                for (action, kind) in rows.iter().enumerate() {
                    let section = self.section(*kind);
                    if self.filter.is_empty() && previous != Some(section) {
                        let header = match section {
                            0 => local!(self.language, "この会話で", "For this conversation"),
                            1 => local!(self.language, "よく使う", "Common actions"),
                            _ => local!(self.language, "その他の操作", "Other actions"),
                        };
                        items.push(popup::Item::new(
                            Line::from(header).style(Style::default().fg(theme::MUTED)),
                        ));
                        previous = Some(section);
                    }
                    if action == selected {
                        selected_row = items.len();
                    }
                    let label = format!(
                        "{}{}{}",
                        metadata::text_for(
                            kind.catalogue().spec().effect.list_marker(),
                            self.language
                        ),
                        kind.label_for(self.language),
                        kind.display_shortcut_for(self.language)
                            .map(|shortcut| format!("  {shortcut}"))
                            .unwrap_or_default()
                    );
                    items.push(popup::Item::new(label));
                }
                popup::list(frame, body, items, selected_row);
            }
        }
    }
}
impl Form {
    fn field_label_for<'a>(&self, field: &'a Field) -> &'a str {
        if self.language == UiLanguage::Ja {
            return field.label;
        }
        if field.name == "db" {
            return "Independent RAG DB (empty = launch data-dir/rag.db)";
        }
        if field.name == "input_mode" {
            return "Text input source";
        }
        self.metadata_id(field.name)
            .and_then(|id| self.operation.catalogue().spec().field(id))
            .map(|field| {
                metadata::text_for(
                    field.field.label_text(self.operation.catalogue()),
                    self.language,
                )
            })
            .unwrap_or(field.label)
    }
    fn new(operation: OpKind, c: &Context) -> Self {
        use Kind::*;
        let f = |name, label, value, kind| Field::new(name, label, value, kind);
        let thread = || f("thread", "会話ID", c.thread.clone(), Text);
        let node = || f("node", "node ID", c.node.clone(), Text);
        let run = || f("run", "作業ID", c.run.clone(), Text);
        let execution = || f("execution", "execution ID", c.execution.clone(), Text);
        let optional_thread = thread;
        let list = || {
            vec![
                f("limit", "件数", MANAGEMENT_LIST_LIMIT.to_string(), Number),
                f("cursor", "続きcursor（空=先頭）", String::new(), Text),
            ]
        };
        let index_fields = |operation: OpKind| {
            vec![
                f(
                    "scope",
                    "対象（空=未指定）",
                    if operation == OpKind::IndexStatus {
                        String::new()
                    } else {
                        "threads".into()
                    },
                    Choice(INDEX_SCOPE_CHOICES),
                ),
                f("space", "索引領域ID（空=現在）", String::new(), Text),
                f("threads", "会話ID（空白区切り）", c.thread.clone(), Text),
                f("sources", "出典node ID（空白区切り）", String::new(), Text),
            ]
        };
        let (fields, build): (Vec<Field>, BuildCommand) = match operation {
            OpKind::Exec
            | OpKind::ConfigInit
            | OpKind::ConfigCheck
            | OpKind::DocsList
            | OpKind::DocsGet
            | OpKind::DocsSearch
            | OpKind::RagInit
            | OpKind::RagSourceAdd
            | OpKind::RagSourceList
            | OpKind::RagSourceRemove
            | OpKind::RagIndex
            | OpKind::RagIngest
            | OpKind::RagStatus
            | OpKind::RagInspect
            | OpKind::RagSearch
            | OpKind::RagGet => return client_forms::new(operation, c),
            OpKind::ThreadList => (
                {
                    let mut v = list();
                    v.push(f("query", "絞込み", String::new(), Text));
                    v
                },
                |form, _cwd| {
                    Ok(Command::ThreadList(ThreadListArgs {
                        limit: form.number("limit")?,
                        cursor: form.optional("cursor")?,
                        query: form.optional("query")?,
                    }))
                },
            ),
            OpKind::ShellList => (
                vec![
                    thread(),
                    f("since", "開始日時", String::new(), Text),
                    f("until", "終了日時（未満）", String::new(), Text),
                    f("command_contains", "コマンドを含む", String::new(), Text),
                    f("state", "状態", String::new(), Text),
                    f("exit_code", "終了コード", String::new(), Number),
                    f("limit", "件数", "50".into(), Number),
                    f("cursor", "続きcursor", String::new(), Text),
                ],
                |form, _| {
                    Ok(Command::ShellList(ShellListArgs {
                        thread_id: form.optional("thread")?,
                        since: form.optional("since")?,
                        until: form.optional("until")?,
                        command_contains: form.optional("command_contains")?,
                        state: form.optional("state")?,
                        exit_code: form.optional_number("exit_code")?,
                        limit: form.number("limit")?,
                        cursor: form.optional("cursor")?,
                    }))
                },
            ),
            OpKind::CompactList => (
                vec![
                    thread(),
                    f("since", "開始日時", String::new(), Text),
                    f("until", "終了日時（未満）", String::new(), Text),
                    f("limit", "件数", "50".into(), Number),
                ],
                |form, _| {
                    Ok(Command::CompactList(CompactListArgs {
                        thread_id: form.optional("thread")?,
                        since: form.optional("since")?,
                        until: form.optional("until")?,
                        limit: form.number("limit")?,
                    }))
                },
            ),
            OpKind::ThreadGet => (vec![thread()], |form, _cwd| {
                Ok(Command::ThreadGet(form.thread_target()?))
            }),
            OpKind::UsageGet => (
                vec![
                    thread(),
                    f(
                        "attempt",
                        "モデル試行ID（他の指定は空に）",
                        String::new(),
                        Text,
                    ),
                    f("since", "開始日時", String::new(), Text),
                    f("until", "終了日時（未満）", String::new(), Text),
                    f("run", "作業ID（指定時は会話を空に）", String::new(), Text),
                    f(
                        "turn",
                        "応答処理ID（指定時は会話を空に）",
                        String::new(),
                        Text,
                    ),
                    f("children", "子会話を含む", "false".into(), Boolean),
                    f(
                        "group_by",
                        "内訳（空=合計）",
                        String::new(),
                        Choice(&["", "hour", "day", "thread"]),
                    ),
                    f("timezone", "時間帯（内訳指定時）", String::new(), Text),
                    f("limit", "区切り上限（内訳指定時）", String::new(), Number),
                    f("cursor", "続きcursor（同じ内訳）", String::new(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::UsageGet(UsageGetArgs {
                        attempt_id: form.optional("attempt")?.map(String::into_boxed_str),
                        since: form.optional("since")?,
                        until: form.optional("until")?,
                        thread_id: form.thread_target()?.thread_id,
                        run_id: form.optional("run")?,
                        turn_id: form.optional("turn")?,
                        include_children: form.boolean("children")?,
                        group_by: form.optional("group_by")?.map(String::into_boxed_str),
                        timezone: form.optional("timezone")?.map(String::into_boxed_str),
                        limit: form.optional_number("limit")?,
                        cursor: form.optional("cursor")?.map(String::into_boxed_str),
                    }))
                },
            ),
            OpKind::ThreadCreate => (
                vec![
                    f("cwd", "作業場所（空=継承/既定）", String::new(), Text),
                    f("agents", "AGENTS参照（空=継承/既定）", String::new(), Text),
                    f("connection", "接続（空=継承）", String::new(), Text),
                    f("model", "モデル（空=継承）", String::new(), Text),
                    f("effort", "推論強度（none=解除）", String::new(), Text),
                    f("parent", "親会話（空=独立）", String::new(), Text),
                ],
                |form, cwd| {
                    Ok(Command::ThreadCreate(ThreadCreateArgs {
                        agents: form.optional("agents")?,
                        cwd: form.target_path("cwd", cwd)?,
                        connection: form.optional("connection")?,
                        model: form.optional("model")?,
                        effort: form.effort()?,
                        parent_id: form.optional("parent")?,
                    }))
                },
            ),
            OpKind::ThreadFork => (
                vec![
                    thread(),
                    node(),
                    f("cwd", "作業場所（空=継承）", String::new(), Text),
                    f("agents", "AGENTS参照（空=継承）", String::new(), Text),
                ],
                |form, cwd| {
                    Ok(Command::ThreadFork(ThreadForkArgs {
                        agents: form.optional("agents")?,
                        thread_id: form.owned("thread")?,
                        node_id: form.owned("node")?,
                        cwd: form.target_path("cwd", cwd)?,
                    }))
                },
            ),
            OpKind::ThreadDelete => (
                vec![
                    thread(),
                    f("descendants", "子孫も対象", "false".into(), Boolean),
                ],
                |form, _cwd| {
                    Ok(Command::ThreadDelete(ThreadDeleteArgs {
                        preview_marker: None,
                        thread_id: form.owned("thread")?,
                        descendants: form.boolean("descendants")?,
                    }))
                },
            ),
            OpKind::ThreadSet => (
                vec![
                    thread(),
                    f(
                        "title_mode",
                        "タイトル操作",
                        "keep".into(),
                        Choice(&["keep", "set", "auto"]),
                    ),
                    f("title", "タイトル（set時）", String::new(), Text),
                    f("cwd", "新cwd（空=変更なし）", String::new(), Text),
                    f("agents", "AGENTS参照（空=変更なし）", String::new(), Text),
                    f("connection", "接続（空=変更なし）", String::new(), Text),
                    f("model", "モデル（空=変更なし）", String::new(), Text),
                    f(
                        "effort",
                        "effort（空=変更なし/none=解除）",
                        String::new(),
                        Text,
                    ),
                ],
                |form, cwd| {
                    Ok(Command::ThreadSet(ThreadSetArgs {
                        agents: form.optional("agents")?,
                        title: match form.value("title_mode")? {
                            "set" => Some(form.owned("title")?),
                            "auto" => Some(String::new()),
                            "keep" => None,
                            _ => {
                                return Err(local!(
                                    form.language,
                                    "タイトル操作を選択してください",
                                    "Select a title action"
                                )
                                .into());
                            }
                        },
                        thread_id: form.optional("thread")?,
                        cwd: form.target_path("cwd", cwd)?,
                        connection: form.optional("connection")?,
                        model: form.optional("model")?,
                        effort: form.effort()?,
                    }))
                },
            ),
            OpKind::MessageSend => (
                vec![
                    thread(),
                    f("text", "依頼本文（Ctrl+J改行）", String::new(), Multiline),
                    f(
                        "sender_label",
                        "送信者の表示名（任意）",
                        String::new(),
                        Text,
                    ),
                    f(
                        "deliver",
                        "届けるとき（空=次の区切り）",
                        String::new(),
                        Choice(&["", "now", "complete"]),
                    ),
                ],
                |form, _cwd| {
                    Ok(Command::MessageSend(MessageSendArgs {
                        sender_label: form.optional("sender_label")?,
                        deliver: form
                            .optional("deliver")?
                            .and_then(|value| Deliver::parse(&value))
                            .unwrap_or_default(),
                        thread_id: form.optional("thread")?,
                        text: form.owned("text")?,
                    }))
                },
            ),
            OpKind::MessageGet => (
                vec![f("message", "message ID", c.message.clone(), Text)],
                |form, _cwd| {
                    Ok(Command::MessageGet(MessageTarget {
                        message_id: form.owned("message")?,
                    }))
                },
            ),
            OpKind::MessageCancel => (
                vec![f(
                    "messages",
                    "取消する入力ID（空白区切り）",
                    String::new(),
                    Text,
                )],
                |form, _| {
                    Ok(Command::MessageCancel(MessageCancelArgs {
                        message_ids: form
                            .value("messages")?
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect(),
                    }))
                },
            ),
            OpKind::OperationGet => (
                vec![f("operation", "operation ID", c.operation.clone(), Text)],
                |form, _cwd| {
                    Ok(Command::OperationGet(OperationTarget {
                        operation_id: form.owned("operation")?,
                    }))
                },
            ),
            OpKind::RunGet => (vec![run()], |form, _cwd| {
                Ok(Command::RunGet(form.run_target()?))
            }),
            OpKind::RunRetry => (
                { vec![f("run", "現在の作業ID", c.current_run.clone(), Text)] },
                |form, _cwd| Ok(Command::RunRetry(form.run_target()?)),
            ),
            OpKind::RunStop => (
                { vec![f("run", "現在の作業ID", c.current_run.clone(), Text)] },
                |form, _cwd| Ok(Command::RunStop(form.run_target()?)),
            ),
            OpKind::TurnGet => (
                vec![f("turn", "応答処理ID", c.turn.clone(), Text)],
                |form, _cwd| {
                    Ok(Command::TurnGet(TurnTarget {
                        turn_id: form.owned("turn")?,
                    }))
                },
            ),
            OpKind::Wait => (
                vec![
                    f(
                        "condition",
                        "観測する条件",
                        "turn_completed".into(),
                        Choice(&[
                            "turn_completed",
                            "run_ended",
                            "shell_ended",
                            "shell_output",
                            "thread_settled",
                        ]),
                    ),
                    f("target", "対象ID", c.turn.clone(), Text),
                    f("cursor", "出力観測の開始cursor", String::new(), Text),
                    f("after", "前回waitのcursor", String::new(), Text),
                    f(
                        "settled_states",
                        "会話の観測状態（空白区切り）",
                        crate::operations::DEFAULT_SETTLEMENT_STATES.into(),
                        Text,
                    ),
                    f(
                        "timeout",
                        "観測の待機上限 ms",
                        DEFAULT_WAIT_TIMEOUT_MS.to_string(),
                        Number,
                    ),
                ],
                |form, _cwd| {
                    Ok({
                        let mut conditions = form.conditions.clone();
                        if !form.value("target")?.is_empty() {
                            if conditions.len() >= 64 {
                                return Err(local!(
                                    form.language,
                                    "待機条件は最大64件です",
                                    "At most 64 wait conditions are allowed"
                                )
                                .into());
                            }
                            conditions.push(form.condition()?);
                        }
                        Command::Wait(WaitArgs {
                            conditions,
                            timeout_ms: form.number("timeout")?,
                            after: form.optional("after")?,
                        })
                    })
                },
            ),
            OpKind::HistoryList => (
                {
                    let mut v = vec![thread()];
                    v.extend(list());
                    v.extend([
                        f("run", "作業（空=全体）", String::new(), Text),
                        f("since", "開始日時（RFC3339）", String::new(), Text),
                        f("state", "状態（空=全体）", String::new(), Text),
                        f(
                            "excerpt_chars",
                            "抜粋文字数（0=省略）",
                            "512".into(),
                            Number,
                        ),
                    ]);
                    v
                },
                |form, _cwd| {
                    Ok(Command::HistoryList(HistoryListArgs {
                        excerpt_chars: form.number("excerpt_chars")?,
                        thread_id: form.owned("thread")?,
                        limit: form.number("limit")?,
                        cursor: form.optional("cursor")?,
                        run_id: form.optional("run")?,
                        since: form.optional("since")?,
                        state: form.optional("state")?,
                    }))
                },
            ),
            OpKind::HistoryGet => (
                vec![
                    node(),
                    f("final_answer_only", "最終回答のみ", "false".into(), Boolean),
                    f("limit", "取得byte数", "65536".into(), Number),
                    f("cursor", "続きcursor", String::new(), Text),
                    f("encoding", "encoding", DEFAULT_ENCODING.into(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::HistoryGet(HistoryGetArgs {
                        final_answer_only: form.boolean("final_answer_only")?,
                        node_id: form.owned("node")?,
                        limit_bytes: form.number("limit")?,
                        cursor: form.optional("cursor")?,
                        encoding: form.owned("encoding")?,
                    }))
                },
            ),
            OpKind::HistorySearch => (
                vec![
                    f("query", "検索語", String::new(), Text),
                    f("limit", "件数", DEFAULT_SEARCH_LIMIT.to_string(), Number),
                    f("thread", "会話（空=全体）", String::new(), Text),
                    f("kinds", "種類（空白区切り）", String::new(), Text),
                    f(
                        "exclude_kinds",
                        "除外する種類（空白区切り）",
                        String::new(),
                        Text,
                    ),
                    f("from", "開始日時（RFC3339・含む）", String::new(), Text),
                    f("to", "終了日時（RFC3339・含まない）", String::new(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::HistorySearch(HistorySearchArgs {
                        query: form.owned("query")?,
                        limit: form.number("limit")?,
                        thread_id: form.optional("thread")?,
                        kinds: form
                            .value("kinds")?
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect(),
                        exclude_kinds: form
                            .value("exclude_kinds")?
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect(),
                        from: form.optional("from")?,
                        to: form.optional("to")?,
                    }))
                },
            ),
            OpKind::ShellGet => (vec![execution()], |form, _cwd| {
                Ok(Command::ShellGet(form.execution_target()?))
            }),
            OpKind::ShellRead => (
                vec![
                    execution(),
                    f("cursor", "続きcursor", String::new(), Text),
                    f("encoding", "文字コード（空=保存値）", String::new(), Text),
                    f("limit", "取得byte数", "65536".into(), Number),
                ],
                |form, _cwd| {
                    Ok(Command::ShellRead(ShellReadArgs {
                        execution_id: form.owned("execution")?,
                        cursor: form.optional("cursor")?,
                        encoding: form.optional("encoding")?,
                        limit_bytes: form.number("limit")?,
                    }))
                },
            ),
            OpKind::ShellWrite => (
                vec![
                    execution(),
                    f("text", "stdin本文（Ctrl+J改行）", String::new(), Multiline),
                    f("encoding", "encoding", DEFAULT_ENCODING.into(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::ShellWrite(ShellWriteArgs {
                        execution_id: form.owned("execution")?,
                        text: form.owned("text")?,
                        encoding: form.owned("encoding")?,
                    }))
                },
            ),
            OpKind::ShellEof => (vec![execution()], |form, _cwd| {
                Ok(Command::ShellEof(form.execution_target()?))
            }),
            OpKind::ShellStop => (vec![execution()], |form, _cwd| {
                Ok(Command::ShellStop(form.execution_target()?))
            }),
            OpKind::AttachmentAdd => (
                vec![
                    optional_thread(),
                    f(
                        "path",
                        "絶対パスまたは作業場所からの相対パス",
                        String::new(),
                        Text,
                    ),
                    f(
                        "kind",
                        "添付の種類",
                        "file".into(),
                        Choice(ATTACHMENT_CHOICES),
                    ),
                ],
                |form, cwd| {
                    Ok(Command::AttachmentAdd(AttachmentAddArgs {
                        thread_id: form.optional("thread")?,
                        path: form.path("path", cwd)?.unwrap_or_default(),
                        kind: AttachmentKind::parse(form.value("kind")?).ok_or(local!(
                            form.language,
                            "添付kindを選択してください",
                            "Select an attachment kind"
                        ))?,
                    }))
                },
            ),
            OpKind::AttachmentList => (vec![thread()], |form, _cwd| {
                Ok(Command::AttachmentList(form.thread_target()?))
            }),
            OpKind::AttachmentRemove => (
                vec![
                    thread(),
                    f("attachment", "attachment ID", String::new(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::AttachmentRemove(AttachmentRemoveArgs {
                        thread_id: form.optional("thread")?,
                        attachment_id: form.owned("attachment")?,
                    }))
                },
            ),
            OpKind::Compact => {
                (
                    vec![
                        thread(),
                        f(
                            "action",
                            "圧縮・編集方法",
                            "deferred".into(),
                            Choice(&[
                                "deferred", "maximal", "spill", "summary", "trim", "omit",
                                "restore",
                            ]),
                        ),
                        f("from_node", "範囲先頭node ID", String::new(), Text),
                        f("to_node", "範囲末尾node ID", String::new(), Text),
                        f(
                            "max_chars",
                            "要約文字数上限（空=設定値）",
                            String::new(),
                            Number,
                        ),
                        f(
                            "max_percent",
                            "要約割合上限（空=設定値）",
                            String::new(),
                            Number,
                        ),
                    ],
                    |form, _| {
                        let action = match form.value("action")? {
                            "deferred" => {
                                if form.optional("from_node")?.is_some()
                                    || form.optional("to_node")?.is_some()
                                    || form.optional("max_chars")?.is_some()
                                    || form.optional("max_percent")?.is_some()
                                {
                                    return Err(
                                    local!(form.language, "範囲を指定するときは圧縮・編集方法を選択してください", "Select a compaction or editing method when specifying a range").into()
                                );
                                }
                                return Ok(Command::Compact(form.thread_target()?));
                            }
                            "summary" => ContextEditAction::Summary,
                            "maximal" => ContextEditAction::Maximal,
                            "spill" => ContextEditAction::Spill,
                            "trim" => ContextEditAction::Trim,
                            "omit" => ContextEditAction::Omit,
                            "restore" => ContextEditAction::Restore,
                            _ => {
                                return Err(local!(
                                    form.language,
                                    "圧縮・編集方法を選択してください",
                                    "Select a compaction or editing method"
                                )
                                .into());
                            }
                        };
                        Ok(Command::ContextEdit(ContextEditArgs {
                            thread_id: form.optional("thread")?,
                            action,
                            from_node_id: form.optional("from_node")?,
                            to_node_id: form.optional("to_node")?,
                            max_chars: form.optional_number("max_chars")?,
                            max_percent: form.optional_number("max_percent")?,
                        }))
                    },
                )
            }
            OpKind::IndexStatus => (index_fields(operation), |form, _cwd| {
                Ok(Command::IndexStatus(form.index_args()?))
            }),
            OpKind::IndexPreview => (index_fields(operation), |form, _| {
                Ok(Command::IndexPreview(form.index_args()?))
            }),
            OpKind::IndexProbe => (
                vec![f(
                    "dimensions",
                    "要求次元（空=設定値）",
                    String::new(),
                    Number,
                )],
                |form, _| {
                    Ok(Command::IndexProbe(IndexProbeArgs {
                        dimensions: form.optional_number("dimensions")?,
                    }))
                },
            ),
            OpKind::IndexBackfill => (index_fields(operation), |form, _cwd| {
                Ok(Command::IndexBackfill(form.index_args()?))
            }),
            OpKind::IndexRetry => (index_fields(operation), |form, _cwd| {
                Ok(Command::IndexRetry(form.index_args()?))
            }),
            OpKind::IndexRebuild => (
                {
                    let mut fields = index_fields(operation);
                    fields.push(f(
                        "dimensions",
                        "要求次元（空=設定値）",
                        String::new(),
                        Number,
                    ));
                    fields
                },
                |form, _cwd| Ok(Command::IndexRebuild(form.index_args()?)),
            ),
            OpKind::StorageStatus => (
                vec![f(
                    "category",
                    "保存データの種類",
                    "all".into(),
                    Choice(STORAGE_CHOICES),
                )],
                |form, _cwd| Ok(Command::StorageStatus(form.storage_args()?)),
            ),
            OpKind::StorageCollect => (
                vec![f(
                    "category",
                    "保存データの種類",
                    "all".into(),
                    Choice(STORAGE_CHOICES),
                )],
                |form, _cwd| Ok(Command::StorageCollect(form.storage_args()?)),
            ),
            OpKind::ModelList => (
                vec![
                    optional_thread(),
                    f("connection", "接続（空=会話/既定）", String::new(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::ModelList(ModelListArgs {
                        connection: form.optional("connection")?,
                        thread_id: form.optional("thread")?,
                    }))
                },
            ),
            OpKind::HistoryRelations => (
                vec![
                    node(),
                    f(
                        "relation",
                        "関係",
                        "previous".into(),
                        Choice(HistoryRelation::NAMES),
                    ),
                    f("limit", "件数", DEFAULT_LIST_LIMIT.to_string(), Number),
                    f("cursor", "続きcursor（空=先頭）", String::new(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::HistoryRelations(HistoryRelationsArgs {
                        node_id: form.owned("node")?,
                        relation: HistoryRelation::parse(form.value("relation")?).ok_or(local!(
                            form.language,
                            "関係を選択してください",
                            "Select a relationship"
                        ))?,
                        limit: form.number("limit")?,
                        cursor: form.optional("cursor")?,
                    }))
                },
            ),
            OpKind::ThreadDeletePreview => (
                vec![
                    thread(),
                    f("descendants", "子孫も対象", "false".into(), Boolean),
                ],
                |form, _cwd| {
                    Ok(Command::ThreadDeletePreview(ThreadDeleteArgs {
                        preview_marker: None,
                        thread_id: form.owned("thread")?,
                        descendants: form.boolean("descendants")?,
                    }))
                },
            ),
            OpKind::Status => (
                vec![
                    f(
                        "memory",
                        "メモリを観測（絞込みと併用不可）",
                        "false".into(),
                        Boolean,
                    ),
                    f("thread", "会話（空=全体）", c.thread.clone(), Text),
                    f("run", "所有作業（空=指定なし）", String::new(), Text),
                    f("since", "記録日時以降（RFC3339）", String::new(), Text),
                    f("state", "保存された状態（空白区切り）", String::new(), Text),
                    f("all", "終端状態を含む", "false".into(), Boolean),
                    f("limit", "件数", DEFAULT_LIST_LIMIT.to_string(), Number),
                    f("cursor", "続きcursor", String::new(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::Status(StatusArgs {
                        memory: form.boolean("memory")?,
                        thread_id: form.optional("thread")?,
                        run_id: form.optional("run")?,
                        since: form.optional("since")?,
                        state: form
                            .value("state")?
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect(),
                        all: form.boolean("all")?,
                        limit: form.number("limit")?,
                        cursor: form.optional("cursor")?,
                    }))
                },
            ),
        };
        Self {
            language: UiLanguage::Ja,
            operation,
            fields,
            build: FormBuilder::Owner(build),
            selected: 0,
            conditions: Vec::new(),
            error: None,
            follows_current: c.follows_current,
            help: false,
            help_scroll: std::cell::Cell::new(0),
            help_page: std::cell::Cell::new(1),
            client_confirmation: None,
            reader: std::cell::RefCell::new(popup::ReaderCache::default()),
            help_revision: 0,
        }
    }
    fn value(&self, name: &str) -> Result<&str, String> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.as_str())
            .ok_or_else(|| match self.language {
                UiLanguage::Ja => {
                    format!("必要な入力欄（{name}）が見つかりません。操作は送信していません")
                }
                UiLanguage::En => {
                    format!("Required field {name} is missing. The operation was not submitted.")
                }
            })
    }
    fn optional(&self, name: &str) -> Result<Option<String>, String> {
        let value = self.value(name)?;
        Ok((!value.is_empty()).then(|| value.to_owned()))
    }
    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<T, String> {
        self.value(name)?.parse().map_err(|_| match self.language {
            UiLanguage::Ja => format!("{name}: 正の数値を入力してください"),
            UiLanguage::En => format!("{name}: Enter a positive number"),
        })
    }
    fn optional_number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, String> {
        self.optional(name)?
            .map(|value| {
                value.parse().map_err(|_| match self.language {
                    UiLanguage::Ja => format!("{name}: 数値を入力してください"),
                    UiLanguage::En => format!("{name}: Enter a number"),
                })
            })
            .transpose()
    }
    fn boolean(&self, name: &str) -> Result<bool, String> {
        self.value(name)?.parse().map_err(|_| match self.language {
            UiLanguage::Ja => format!("{name}: true / false を選択してください"),
            UiLanguage::En => format!("{name}: Select true or false"),
        })
    }
    fn path(&self, name: &str, cwd: &Path) -> Result<Option<String>, String> {
        self.optional(name)?
            .map(|value| {
                let path = crate::input_path::relative_to(Path::new(&value), cwd);
                crate::input_path::utf8(&path)
                    .map(str::to_owned)
                    .map_err(|_| {
                        match self.language { UiLanguage::Ja => format!("{name}: パスはUTF-8で表現できる必要があります。操作は送信していません"), UiLanguage::En => format!("{name}: The path must be representable as UTF-8. The operation was not submitted.") }
                    })
            })
            .transpose()
    }

    fn target_path(&self, name: &str, cwd: &Path) -> Result<Option<String>, String> {
        // Check representability locally, but leave relative resolution to Control:
        // this form may target a Thread other than the currently displayed one.
        self.path(name, cwd)?;
        self.optional(name)
    }

    fn effort(&self) -> Result<Option<String>, String> {
        Ok(self.optional("effort")?.map(|value| {
            if value == "none" {
                String::new()
            } else {
                value
            }
        }))
    }
    fn condition(&self) -> Result<WaitCondition, String> {
        let id = self.value("target")?.to_owned();
        if id.is_empty() {
            return Err(local!(
                self.language,
                "待機対象IDが必要です",
                "A wait target ID is required"
            )
            .into());
        }
        Ok(match self.value("condition")? {
            "thread_settled" => WaitCondition::ThreadSettled {
                thread_ids: id.split_whitespace().map(str::to_owned).collect(),
                states: self
                    .value("settled_states")?
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect(),
            },
            "turn_completed" => WaitCondition::TurnCompleted { turn_id: id },
            "run_ended" => WaitCondition::RunEnded { run_id: id },
            "shell_ended" => WaitCondition::ShellEnded { execution_id: id },
            "shell_output" => WaitCondition::ShellOutput {
                execution_id: id,
                cursor: self.value("cursor")?.into(),
            },
            _ => {
                return Err(local!(
                    self.language,
                    "待機条件typeが無効です",
                    "The wait condition type is invalid"
                )
                .into());
            }
        })
    }
    fn insert(&mut self, text: &str) {
        if self.client_confirmation.is_some() {
            return;
        }
        let bytes = self.fields.iter().map(|f| f.value.len()).sum::<usize>();
        if bytes.saturating_add(text.len()) > 1024 * 1024 {
            self.error = Some(
                local!(
                    self.language,
                    "入力field合計は1MiBまでです。既存内容を保持しました",
                    "Total field input is limited to 1 MiB. Existing content was retained."
                )
                .into(),
            );
            return;
        }
        let Some(field) = self.fields.get_mut(self.selected) else {
            return;
        };
        if matches!(field.kind, Kind::Text | Kind::Multiline | Kind::Number) {
            field.value.push_str(text);
            self.help_revision = self.help_revision.wrapping_add(1);
            self.error = None;
        }
    }
    fn key(&mut self, key: KeyEvent, cwd: &Path) -> Outcome {
        if self.client_confirmation.is_some() {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    return Outcome::Client(
                        self.client_confirmation
                            .take()
                            .expect("confirmed client request"),
                    );
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                    self.client_confirmation = None;
                    self.help_scroll.set(0);
                }
                _ => {
                    if let Some(next) =
                        popup::reader_key(key.code, self.help_scroll.get(), self.help_page.get())
                    {
                        self.help_scroll.set(next);
                    }
                }
            }
            return Outcome::None;
        }
        if self.fields.is_empty() && key.code != KeyCode::Enter {
            return Outcome::None;
        }
        self.help_revision = self.help_revision.wrapping_add(1);
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.selected = (self.selected + 1) % self.fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.selected = (self.selected + self.fields.len() - 1) % self.fields.len()
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if matches!(
                    self.fields[self.selected].kind,
                    Kind::Boolean | Kind::Choice(_)
                ) =>
            {
                let field = &mut self.fields[self.selected];
                let options = match field.kind {
                    Kind::Boolean => &["false", "true"][..],
                    Kind::Choice(values) => values,
                    _ => unreachable!(),
                };
                let old = options.iter().position(|s| *s == field.value).unwrap_or(0);
                let next = if key.code == KeyCode::Left {
                    (old + options.len() - 1) % options.len()
                } else {
                    (old + 1) % options.len()
                };
                field.value = options[next].into();
            }
            KeyCode::Backspace => {
                let field = &mut self.fields[self.selected];
                if !matches!(field.kind, Kind::Boolean | Kind::Choice(_)) {
                    let start = prev_grapheme(&field.value, field.value.len());
                    field.value.truncate(start);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.fields[self.selected].value.clear()
            }
            KeyCode::Char('j')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(self.fields[self.selected].kind, Kind::Multiline) =>
            {
                self.insert("\n")
            }
            KeyCode::F(9) if self.operation == OpKind::Wait => match self.condition() {
                Ok(condition) if self.conditions.len() < 64 => {
                    self.conditions.push(condition);
                    if let Some(target) = self.fields.iter_mut().find(|f| f.name == "target") {
                        target.value.clear();
                    }
                }
                Ok(_) => {
                    self.error = Some(
                        local!(
                            self.language,
                            "待機条件は最大64件です",
                            "At most 64 wait conditions are allowed"
                        )
                        .into(),
                    )
                }
                Err(error) => self.error = Some(error),
            },
            KeyCode::F(10) if self.operation == OpKind::Wait => {
                self.conditions.pop();
            }
            KeyCode::Enter => match self.invocation(cwd) {
                Ok(Outcome::Client(operation)) if operation.effect() != Effect::Inspect => {
                    self.client_confirmation = Some(operation);
                    self.help_scroll.set(0);
                    self.error = None;
                }
                Ok(outcome) => return outcome,
                Err(error) => self.error = Some(error),
            },
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&ch.to_string())
            }
            _ => {}
        }
        Outcome::None
    }
    fn owned(&self, name: &str) -> Result<String, String> {
        Ok(self.value(name)?.to_owned())
    }
    fn thread_target(&self) -> Result<ThreadTarget, String> {
        Ok(ThreadTarget {
            thread_id: self.optional("thread")?,
        })
    }
    fn run_target(&self) -> Result<RunTarget, String> {
        Ok(RunTarget {
            run_id: self.owned("run")?,
        })
    }
    fn execution_target(&self) -> Result<ExecutionTarget, String> {
        Ok(ExecutionTarget {
            execution_id: self.owned("execution")?,
        })
    }
    fn index_args(&self) -> Result<IndexArgs, String> {
        Ok(IndexArgs {
            dimensions: if self.operation == OpKind::IndexRebuild {
                self.optional_number("dimensions")?
            } else {
                None
            },
            scope: match self.value("scope")? {
                "" => None,
                value => Some(IndexScope::parse(value).ok_or(local!(
                    self.language,
                    "scopeを選択してください",
                    "Select a scope"
                ))?),
            },
            space_id: self.optional("space")?,
            thread_ids: self
                .value("threads")?
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            source_ids: self
                .value("sources")?
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        })
    }
    fn storage_args(&self) -> Result<StorageArgs, String> {
        Ok(StorageArgs {
            category: Some(
                StorageCategory::parse(self.value("category")?).ok_or(local!(
                    self.language,
                    "categoryを選択してください",
                    "Select a category"
                ))?,
            ),
        })
    }
    fn invocation(&self, cwd: &Path) -> Result<Outcome, String> {
        match self.build {
            FormBuilder::Owner(_) => self.command(cwd).map(Outcome::Execute),
            FormBuilder::Client(build) => {
                if self
                    .fields
                    .iter()
                    .map(|field| field.value.len())
                    .sum::<usize>()
                    > 1024 * 1024
                {
                    return Err(local!(
                        self.language,
                        "フォームの入力合計は1MiBまでです。本文ファイル入力も利用できます",
                        "Total form input is limited to 1 MiB. A text file can also be used."
                    )
                    .into());
                }
                build(self, cwd).map(Outcome::Client)
            }
        }
    }
    fn command(&self, cwd: &Path) -> Result<Command, String> {
        let FormBuilder::Owner(build) = self.build else {
            return Err(local!(
                self.language,
                "この操作は独立クライアントとして実行します",
                "This operation runs as a standalone client"
            )
            .into());
        };
        let command = build(self, cwd)?;
        command.validate().map_err(|e| e.to_string())?;
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayed_shortcuts_follow_width_without_changing_canonical_values() {
        for (policy, arrow, separator) in [
            (Some(1), "→", " · "),
            (Some(2), ">", " | "),
            (None, ">", " | "),
        ] {
            crate::ui::text_width::set_terminal_width(policy);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let kind = OpKind::IndexRetry;
                assert_eq!(kind.shortcut(), Some("/index > T"));
                assert_eq!(
                    kind.display_shortcut_for(language).as_deref(),
                    Some(format!("/index {arrow} T").as_str())
                );
                assert_eq!(OpKind::ThreadSet.shortcut(), Some("/model | /effort"));
                assert_eq!(
                    OpKind::ThreadSet.display_shortcut_for(language),
                    Some(format!("/model{separator}/effort"))
                );
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn all_operation_ui_labels_use_width_invariant_symbols_without_changing_cli_labels() {
        use crate::ui::text_width::UnicodeWidthStr;
        for &kind in OpKind::ALL {
            crate::ui::text_width::set_terminal_width(Some(1));
            let narrow_label = kind.label();
            let narrow = narrow_label.as_str().width();
            crate::ui::text_width::set_terminal_width(Some(2));
            let wide_label = kind.label();
            let wide = wide_label.as_str().width_cjk();
            assert_eq!(narrow, wide, "{}", narrow_label);
            assert_eq!(narrow_label.replace(" · ", " | "), wide_label);
        }
        crate::ui::text_width::clear_terminal_width();
        assert_eq!(OpKind::ThreadGet.label(), "会話 | 詳細");
        assert_eq!(OpKind::ThreadGet.catalogue().label(), "会話 · 詳細");
    }

    #[test]
    fn human_terms_do_not_replace_cli_operation_identity() {
        for (kind, label, wire) in [
            (OpKind::ThreadGet, "会話 | 詳細", "thread.get"),
            (OpKind::UsageGet, "使用量 | 合計", "usage.get"),
            (OpKind::RunGet, "作業 | 詳細", "run.get"),
            (OpKind::TurnGet, "応答処理 | 詳細", "turn.get"),
        ] {
            assert_eq!(kind.label(), label);
            assert_eq!(kind.wire_name(), wire);
        }
    }
    #[test]
    fn every_operation_builds_its_independent_wire_contract_and_distinct_target_arguments() {
        use serde_json::json;
        let id = |n| format!("00000000-0000-4000-8000-{n:012}");
        let context = Context {
            thread: id(1),
            node: id(2),
            message: id(3),
            operation: id(4),
            run: id(5),
            current_run: id(6),
            turn: id(7),
            execution: id(8),
            cwd: PathBuf::from("/tmp"),
            ..Default::default()
        };
        // Independent public-wire expectations, not derived from enum labels,
        // builder functions, or repeated identical target fixtures.
        let expected = [
            (
                OpKind::ThreadList,
                "thread.list",
                json!({"limit":20,"query":"synthetic text"}),
            ),
            (OpKind::ThreadGet, "thread.get", json!({"thread_id":id(1)})),
            (
                OpKind::UsageGet,
                "usage.get",
                json!({"thread_id":id(1),"run_id":null,"turn_id":null,"include_children":false}),
            ),
            (
                OpKind::ThreadCreate,
                "thread.create",
                json!({"model":"model"}),
            ),
            (
                OpKind::ThreadFork,
                "thread.fork",
                json!({"thread_id":id(1),"node_id":id(2)}),
            ),
            (
                OpKind::ThreadDelete,
                "thread.delete",
                json!({"thread_id":id(1),"descendants":false}),
            ),
            (
                OpKind::ThreadSet,
                "thread.set",
                json!({"thread_id":id(1),"model":"model"}),
            ),
            (
                OpKind::MessageSend,
                "message.send",
                json!({"thread_id":id(1),"text":"synthetic text"}),
            ),
            (
                OpKind::MessageGet,
                "message.get",
                json!({"message_id":id(3)}),
            ),
            (
                OpKind::OperationGet,
                "operation.get",
                json!({"operation_id":id(4)}),
            ),
            (OpKind::RunGet, "run.get", json!({"run_id":id(5)})),
            (OpKind::RunRetry, "run.retry", json!({"run_id":id(6)})),
            (OpKind::RunStop, "run.stop", json!({"run_id":id(6)})),
            (OpKind::TurnGet, "turn.get", json!({"turn_id":id(7)})),
            (
                OpKind::Wait,
                "wait",
                json!({"conditions":[{"type":"turn_completed","turn_id":id(7)}],"timeout_ms":30000}),
            ),
            (
                OpKind::HistoryList,
                "history.list",
                json!({"thread_id":id(1),"limit":20}),
            ),
            (
                OpKind::HistoryGet,
                "history.get",
                json!({"node_id":id(2),"limit_bytes":65536,"encoding":"UTF-8"}),
            ),
            (
                OpKind::HistorySearch,
                "history.search",
                json!({"query":"synthetic text","limit":10,"thread_id":null}),
            ),
            (
                OpKind::ShellList,
                "shell.list",
                json!({"thread_id":id(1),"limit":50}),
            ),
            (OpKind::ShellGet, "shell.get", json!({"execution_id":id(8)})),
            (
                OpKind::ShellRead,
                "shell.read",
                json!({"execution_id":id(8),"limit_bytes":65536}),
            ),
            (
                OpKind::ShellWrite,
                "shell.write",
                json!({"execution_id":id(8),"text":"synthetic text","encoding":"UTF-8"}),
            ),
            (OpKind::ShellEof, "shell.eof", json!({"execution_id":id(8)})),
            (
                OpKind::ShellStop,
                "shell.stop",
                json!({"execution_id":id(8)}),
            ),
            (
                OpKind::AttachmentAdd,
                "attachment.add",
                json!({"thread_id":id(1),"path":context.cwd.join("synthetic.txt").to_string_lossy(),"kind":"file"}),
            ),
            (
                OpKind::AttachmentList,
                "attachment.list",
                json!({"thread_id":id(1)}),
            ),
            (
                OpKind::AttachmentRemove,
                "attachment.remove",
                json!({"thread_id":id(1),"attachment_id":id(9)}),
            ),
            (
                OpKind::CompactList,
                "compact.list",
                json!({"thread_id":id(1),"limit":50}),
            ),
            (OpKind::Compact, "compact", json!({"thread_id":id(1)})),
            (
                OpKind::IndexStatus,
                "index.status",
                json!({"scope":null,"thread_ids":[id(1)]}),
            ),
            (
                OpKind::IndexBackfill,
                "index.backfill",
                json!({"scope":"threads","thread_ids":[id(1)]}),
            ),
            (
                OpKind::IndexRetry,
                "index.retry",
                json!({"scope":"threads","thread_ids":[id(1)]}),
            ),
            (
                OpKind::IndexRebuild,
                "index.rebuild",
                json!({"scope":"threads","thread_ids":[id(1)]}),
            ),
            (
                OpKind::StorageStatus,
                "storage.status",
                json!({"category":"all"}),
            ),
            (
                OpKind::StorageCollect,
                "storage.collect",
                json!({"category":"all"}),
            ),
            (OpKind::ModelList, "model.list", json!({"thread_id":id(1)})),
            (
                OpKind::HistoryRelations,
                "history.relations",
                json!({"node_id":id(2),"relation":"previous","limit":50}),
            ),
            (
                OpKind::ThreadDeletePreview,
                "thread.delete_preview",
                json!({"thread_id":id(1),"descendants":false}),
            ),
            (
                OpKind::Status,
                "status",
                json!({"thread_id":id(1),"limit":50,"all":false}),
            ),
            (
                OpKind::MessageCancel,
                "message.cancel",
                json!({"message_ids":[id(3)]}),
            ),
            (
                OpKind::IndexPreview,
                "index.preview",
                json!({"scope":"threads","thread_ids":[id(1)]}),
            ),
            (OpKind::IndexProbe, "index.probe", json!({})),
        ];
        assert_eq!(
            OpKind::OWNER,
            expected.iter().map(|row| row.0).collect::<Vec<_>>()
        );
        // Expected durable intentions, independent of dispatch ownership and
        // the read-only classifier: wait also owns saved lifecycle state.
        let receipt_commands = std::collections::HashSet::from([
            "thread.create",
            "thread.fork",
            "thread.delete",
            "thread.set",
            "message.send",
            "message.cancel",
            "run.retry",
            "run.stop",
            "wait",
            "shell.write",
            "shell.eof",
            "shell.stop",
            "attachment.add",
            "attachment.remove",
            "compact",
            "index.backfill",
            "index.retry",
            "index.rebuild",
            "index.probe",
            "storage.collect",
        ]);
        let mut names = std::collections::HashSet::new();
        for (operation, name, args) in expected {
            assert!(names.insert(name), "duplicate wire contract {name}");
            assert_eq!(operation.wire_name(), name);
            let mut form = Form::new(operation, &context);
            let mut fields = std::collections::HashSet::new();
            for field in &mut form.fields {
                assert!(
                    fields.insert(field.name),
                    "duplicate field in {name}: {}",
                    field.name
                );
                if field.value.is_empty() {
                    field.value = match field.name {
                        "text" | "query" => "synthetic text".into(),
                        "path" => "synthetic.txt".into(),
                        "attachment" => id(9),
                        "messages" => id(3),
                        "model" => "model".into(),
                        _ => String::new(),
                    };
                }
            }
            let command = form
                .command(&context.cwd)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(command.name(), name);
            assert_eq!(
                OperationId::from_owner_command(&command),
                Some(operation.catalogue()),
                "form and result must use the same typed operation identity"
            );
            use crate::protocol::CommandRoute;
            assert_eq!(
                command.route(),
                if name == "status" {
                    CommandRoute::StatusProjection
                } else if matches!(
                    name,
                    "model.list"
                        | "shell.write"
                        | "shell.eof"
                        | "shell.stop"
                        | "shell.get"
                        | "shell.read"
                        | "wait"
                        | "history.search"
                        | "index.status"
                        | "index.preview"
                        | "index.probe"
                        | "index.backfill"
                        | "index.retry"
                        | "index.rebuild"
                        | "storage.status"
                        | "storage.collect"
                ) {
                    CommandRoute::Runtime
                } else {
                    CommandRoute::Database
                }
            );
            assert_eq!(
                command.requires_receipt(),
                receipt_commands.contains(name),
                "{name} receipt contract"
            );
            assert_eq!(
                command.runtime_cancellable(),
                matches!(
                    name,
                    "model.list"
                        | "history.search"
                        | "index.backfill"
                        | "index.rebuild"
                        | "index.retry"
                        | "index.preview"
                        | "index.probe"
                        | "storage.collect"
                        | "storage.status"
                        | "shell.get"
                        | "shell.read"
                )
            );
            assert_eq!(
                command.runtime_allowed_while_stopping(),
                matches!(name, "shell.stop" | "shell.eof")
            );
            assert_eq!(
                command.changes_context(),
                matches!(
                    name,
                    "thread.create"
                        | "thread.fork"
                        | "thread.delete"
                        | "thread.set"
                        | "message.send"
                        | "message.cancel"
                        | "run.retry"
                        | "run.stop"
                        | "attachment.add"
                        | "attachment.remove"
                        | "compact"
                )
            );
            assert_eq!(
                command.is_read_only(),
                matches!(
                    name,
                    "status"
                        | "model.list"
                        | "thread.list"
                        | "thread.get"
                        | "usage.get"
                        | "shell.list"
                        | "compact.list"
                        | "thread.delete_preview"
                        | "message.get"
                        | "operation.get"
                        | "run.get"
                        | "turn.get"
                        | "history.list"
                        | "history.get"
                        | "history.search"
                        | "history.relations"
                        | "shell.get"
                        | "shell.read"
                        | "attachment.list"
                        | "index.status"
                        | "index.preview"
                        | "storage.status"
                )
            );
            let actual = serde_json::to_value(command).unwrap();
            assert_eq!(actual["command"], name);
            for (key, value) in args.as_object().unwrap() {
                assert_eq!(&actual["args"][key], value, "{name} argument {key}");
            }
        }
        assert!(
            receipt_commands.is_subset(&names),
            "every receipt operation has a form fixture"
        );
    }
    #[test]
    fn unknown_or_missing_field_is_an_error_while_an_existing_blank_remains_optional() {
        let mut form = Form::new(OpKind::ThreadCreate, &Context::default());
        assert_eq!(form.optional("connection").unwrap(), None);
        assert!(form.value("conection").unwrap_err().contains("conection"));
        form.fields.retain(|field| field.name != "connection");
        let before = form
            .fields
            .iter()
            .map(|field| field.value.clone())
            .collect::<Vec<_>>();
        assert!(matches!(
            form.key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                Path::new("/")
            ),
            Outcome::None
        ));
        assert!(form.error.as_ref().unwrap().contains("connection"));
        assert_eq!(
            before,
            form.fields
                .iter()
                .map(|field| field.value.clone())
                .collect::<Vec<_>>()
        );
        let mut required = Form::new(OpKind::ThreadGet, &Context::default());
        required.fields.clear();
        assert!(
            required
                .command(Path::new("/"))
                .unwrap_err()
                .contains("thread")
        );
    }
    #[test]
    fn retry_and_stop_target_the_current_run_not_a_past_reference() {
        let context = Context {
            run: "past-reference".into(),
            current_run: "current-recovery".into(),
            ..Default::default()
        };
        assert!(
            matches!(Form::new(OpKind::RunGet,&context).command(&context.cwd).unwrap(), Command::RunGet(target) if target.run_id == "past-reference")
        );
        for kind in [OpKind::RunRetry, OpKind::RunStop] {
            assert!(
                matches!(Form::new(kind,&context).command(&context.cwd).unwrap(), Command::RunRetry(target) | Command::RunStop(target) if target.run_id == "current-recovery")
            );
        }
    }
    #[test]
    fn history_filters_and_delete_boolean_keep_the_public_contract() {
        let context = Context {
            thread: "thread".into(),
            ..Default::default()
        };
        let mut history = Form::new(OpKind::HistoryList, &context);
        for (name, value) in [
            ("run", "run"),
            ("since", "2026-10-25T00:00:00Z"),
            ("state", "running"),
        ] {
            history
                .fields
                .iter_mut()
                .find(|field| field.name == name)
                .unwrap()
                .value = value.into();
        }
        assert!(
            matches!(history.command(&context.cwd).unwrap(), Command::HistoryList(args)
            if args.run_id.as_deref() == Some("run") && args.since.as_deref() == Some("2026-10-25T00:00:00Z") && args.state.as_deref() == Some("running"))
        );
        history
            .fields
            .iter_mut()
            .find(|field| field.name == "state")
            .unwrap()
            .value = "applied".into();
        assert!(
            history
                .command(&context.cwd)
                .unwrap_err()
                .contains("saved shell execution state")
        );
        for operation in [OpKind::ThreadDelete, OpKind::ThreadDeletePreview] {
            let mut form = Form::new(operation, &context);
            for value in ["true", "false", ""] {
                form.fields
                    .iter_mut()
                    .find(|field| field.name == "descendants")
                    .unwrap()
                    .value = value.into();
                match form.command(&context.cwd) {
                    Ok(Command::ThreadDelete(args) | Command::ThreadDeletePreview(args)) => {
                        assert_eq!(args.descendants, value == "true")
                    }
                    Err(_) => assert!(value.is_empty()),
                    other => panic!("unexpected command: {other:?}"),
                }
                if value.is_empty() {
                    assert!(form.command(&context.cwd).is_err());
                }
            }
        }
    }
    #[test]
    fn search_form_has_explicit_optional_scope_and_typed_filters() {
        let context = Context {
            thread: "current-thread".into(),
            ..Default::default()
        };
        let mut form = Form::new(OpKind::HistorySearch, &context);
        let set = |form: &mut Form, name: &str, value: &str| {
            form.fields
                .iter_mut()
                .find(|f| f.name == name)
                .unwrap()
                .value = value.into();
        };
        set(&mut form, "query", "needle");
        assert!(
            matches!(form.command(&context.cwd).unwrap(), Command::HistorySearch(a) if a.thread_id.is_none())
        );
        set(&mut form, "thread", "target");
        set(&mut form, "kinds", "user assistant");
        set(&mut form, "exclude_kinds", "tool");
        set(&mut form, "from", "2026-01-01T00:00:00Z");
        assert!(
            matches!(form.command(&context.cwd).unwrap(), Command::HistorySearch(a) if a.thread_id.as_deref()==Some("target") && a.kinds == ["user", "assistant"] && a.exclude_kinds == ["tool"] && a.from.is_some())
        );
        set(&mut form, "to", "invalid");
        assert!(form.command(&context.cwd).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn form_paths_reject_non_utf8_base_without_changing_representable_absolute_targets() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let cwd = std::env::temp_dir().join(OsString::from_vec(b"bad-\xff".to_vec()));
        let context = Context {
            cwd: cwd.clone(),
            thread: "thread".into(),
            ..Default::default()
        };
        for (kind, field) in [(OpKind::ThreadSet, "cwd"), (OpKind::AttachmentAdd, "path")] {
            let mut form = Form::new(kind, &context);
            form.fields
                .iter_mut()
                .find(|input| input.name == field)
                .unwrap()
                .value = "relative".into();
            assert!(form.command(&cwd).unwrap_err().contains("UTF-8"));
            let valid = std::env::temp_dir().join("valid-target");
            form.fields
                .iter_mut()
                .find(|input| input.name == field)
                .unwrap()
                .value = valid.to_str().unwrap().into();
            let command = form.command(&cwd).unwrap();
            match command {
                Command::ThreadSet(args) => assert_eq!(args.cwd.as_deref(), valid.to_str()),
                Command::AttachmentAdd(args) => assert_eq!(args.path, valid.to_str().unwrap()),
                _ => panic!("wrong operation"),
            }
        }
    }
    #[test]
    fn field_references_share_detail_routing_without_changing_current_run() {
        for (key, operation) in [
            ("nested/thread_id", OpKind::ThreadGet),
            ("message_id", OpKind::MessageGet),
            ("request_operation_id", OpKind::OperationGet),
            ("nested.owner_run_id", OpKind::RunGet),
            ("turn_id", OpKind::TurnGet),
            ("execution_id", OpKind::ShellGet),
        ] {
            let target = TargetKind::from_field_key(key).unwrap();
            assert_eq!(target.detail_operation(), Some(operation));
            let mut context = Context {
                current_run: "current".into(),
                ..Default::default()
            };
            context.set_reference(target, "selected".into());
            let form = Form::new(operation, &context);
            let command = serde_json::to_value(form.command(&context.cwd).unwrap()).unwrap();
            assert!(
                command["args"]
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|value| value == "selected"),
                "{key}: {command}"
            );
            assert_eq!(context.current_run, "current");
        }
        assert!(TargetKind::from_field_key("source_node_id").is_none());
        assert_eq!(
            TargetKind::from_field_key("node_id")
                .unwrap()
                .detail_operation(),
            None
        );
    }
    #[test]
    fn agents_forms_preserve_relative_target_paths() {
        let context = Context {
            thread: "target".into(),
            node: "source".into(),
            ..Default::default()
        };
        for kind in [OpKind::ThreadCreate, OpKind::ThreadFork, OpKind::ThreadSet] {
            let mut form = Form::new(kind, &context);
            form.fields
                .iter_mut()
                .find(|f| f.name == "agents")
                .unwrap()
                .value = "roles/coder.md".into();
            let value = serde_json::to_value(form.command(&context.cwd).unwrap()).unwrap();
            assert_eq!(value["args"]["agents"], "roles/coder.md");
        }
    }
    #[test]
    fn title_form_distinguishes_keep_set_and_auto() {
        let context = Context::default();
        let mut form = Form::new(OpKind::ThreadSet, &context);
        let set = |form: &mut Form, name: &str, value: &str| {
            form.fields
                .iter_mut()
                .find(|f| f.name == name)
                .unwrap()
                .value = value.into();
        };
        set(&mut form, "model", "unchanged-model");
        assert!(
            matches!(form.command(&context.cwd).unwrap(), Command::ThreadSet(a) if a.title.is_none())
        );
        set(&mut form, "title_mode", "set");
        set(&mut form, "title", "調査");
        assert!(
            matches!(form.command(&context.cwd).unwrap(), Command::ThreadSet(a) if a.title.as_deref() == Some("調査"))
        );
        set(&mut form, "title_mode", "auto");
        assert!(
            matches!(form.command(&context.cwd).unwrap(), Command::ThreadSet(a) if a.title.as_deref() == Some(""))
        );
    }
    #[test]
    fn cleared_or_unknown_choices_are_rejected_without_sending_or_losing_input() {
        let context = Context::default();
        for (kind, name, expected_error) in [
            (
                OpKind::ThreadSet,
                "title_mode",
                "タイトル操作を選択してください",
            ),
            (
                OpKind::Status,
                "all",
                "all: true / false を選択してください",
            ),
        ] {
            for value in ["", "unknown"] {
                let mut form = Form::new(kind, &context);
                let selected = form
                    .fields
                    .iter()
                    .position(|field| field.name == name)
                    .unwrap();
                form.selected = selected;
                if value.is_empty() {
                    form.key(
                        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                        &context.cwd,
                    );
                } else {
                    form.fields[selected].value = value.into();
                }
                assert!(matches!(
                    form.key(
                        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                        &context.cwd
                    ),
                    Outcome::None
                ));
                assert_eq!(form.error.as_deref(), Some(expected_error));
                assert_eq!(form.value(name).unwrap(), value);
                assert_eq!(form.selected, selected);
                // Explicit selection repairs the form without retyping the other fields.
                form.key(
                    KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                    &context.cwd,
                );
                assert!(matches!(
                    form.key(
                        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                        &context.cwd
                    ),
                    Outcome::Execute(_)
                ));
            }
        }
    }
    #[test]
    fn wait_accumulation_is_bounded_and_forms_scroll_to_selected_field() {
        let mut form = Form::new(
            OpKind::Wait,
            &Context {
                turn: "turn".into(),
                ..Default::default()
            },
        );
        for _ in 0..65 {
            form.fields
                .iter_mut()
                .find(|f| f.name == "target")
                .unwrap()
                .value = "turn".into();
            form.key(
                KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
                Path::new("/tmp"),
            );
        }
        assert_eq!(form.conditions.len(), 64);
        assert!(form.error.is_some());
        let mut operations = Operations::new(Context::default());
        operations.select(OpKind::ThreadCreate);
        operations.form.as_mut().unwrap().selected = operations
            .form
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .position(|f| f.name == "parent")
            .unwrap();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(32, 9)).unwrap();
        terminal
            .draw(|frame| operations.draw(frame, frame.area()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.replace(' ', "").contains("親会話"));
    }
    #[test]
    fn shared_form_layout_keeps_validation_and_wait_actions_visible() {
        let mut operations = Operations::new(Context::default());
        operations.select(OpKind::Wait);
        operations.form.as_mut().unwrap().error = Some("待機対象IDが必要です".into());
        for (width, height) in [(80, 24), (32, 12), (8, 5)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| operations.draw(frame, frame.area()))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("Esc"));
            if width >= 32 {
                assert!(text.replace(' ', "").contains("待機対象IDが必要です"));
            }
            if width >= 80 {
                assert!(text.contains("F9") && text.contains("F10"));
            }
        }
    }
    #[test]
    fn multiline_field_value_survives_list_paging_and_typed_execution() {
        let mut operations = Operations::new(Context::default());
        operations.select(OpKind::MessageSend);
        let form = operations.form.as_mut().unwrap();
        form.fields
            .iter_mut()
            .find(|field| field.name == "text")
            .unwrap()
            .value = "one\n日本語 second\nlast".into();
        let before = form
            .fields
            .iter()
            .map(|field| field.value.clone())
            .collect::<Vec<_>>();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| operations.draw(frame, frame.area()))
            .unwrap();
        operations.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        let form = operations.form.as_ref().unwrap();
        assert_eq!(
            form.fields
                .iter()
                .map(|field| field.value.clone())
                .collect::<Vec<_>>(),
            before
        );
        assert!(
            matches!(form.command(Path::new("/tmp")).unwrap(),Command::MessageSend(args)if args.text=="one\n日本語 second\nlast")
        );
    }
}

#[cfg(test)]
mod entry_tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn recovery() -> Context {
        Context {
            thread: "thread".into(),
            run: "current".into(),
            current_run: "current".into(),
            turn: "turn".into(),
            phase: ActivityPhase::Recovery,
            follows_current: true,
            ..Default::default()
        }
    }
    fn screen(operations: &Operations, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| operations.draw(frame, frame.area()))
            .unwrap();
        let mut text = String::new();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                let symbol = terminal.backend().buffer()[(x, y)].symbol();
                text.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            text.push('\n');
        }
        text
    }
    #[test]
    fn all_guide_navigation_captions_match_actual_display_labels() {
        fn captions(text: &str) -> Vec<&str> {
            let mut found = Vec::new();
            for line in text
                .lines()
                .filter(|line| !line.trim_start().starts_with("```"))
            {
                for (open, close) in [('`', '`'), ('“', '”'), ('「', '」')] {
                    let mut rest = line;
                    while let Some(start) = rest.find(open) {
                        rest = &rest[start + open.len_utf8()..];
                        let Some(end) = rest.find(close) else { break };
                        let quoted = &rest[..end];
                        // A CLI usage alternative is not an F8 caption.
                        if (quoted.contains(" | ") || quoted.contains(" · "))
                            && !quoted.contains("--")
                        {
                            found.push(quoted);
                        }
                        rest = &rest[end + close.len_utf8()..];
                    }
                }
            }
            found
        }
        // Use real operation labels and a real projection for a generated title.
        // The received caption is an exact source literal: this does not repeat
        // an expected label in a whitelist that could drift with the guide.
        let envelope = serde_json::json!({"section":"shells","items":[]});
        let text = envelope.to_string();
        let payload = serde_json::json!({"text":text});
        let saved = crate::presentation::project("interruption_details", &payload, &text, 0);
        let received_source = include_str!("../frontend/received.rs");
        let management_source = include_str!("management.rs");
        let actual = |caption: &str| {
            OpKind::ALL.iter().any(|kind| kind.label() == caption)
                || saved.title == caption
                || received_source.contains(&format!("\"{caption}\""))
                || management_source.contains(&format!("\"{caption}\""))
        };
        // A wrong caption must be rejected even if another correct caption is
        // present elsewhere in the same guide (the old test's blind spot).
        let wrong = captions("Use `会話 | 詳細`, never `Thread | 詳細`.");
        assert!(actual(wrong[0]));
        assert!(!actual(wrong[1]));
        let mut documents = crate::docs::documents();
        assert_eq!(documents.len(), 14);
        assert!(
            documents
                .iter()
                .any(|doc| doc.id == "shellboy/resource-usage")
        );
        documents.push(crate::docs::Document {
            id: "user-guide.ja.md".into(),
            parent_id: String::new(),
            title: String::new(),
            summary: String::new(),
            text: include_str!("../../docs/guides/user-guide.ja.md").into(),
        });
        let mut quoted = 0;
        for guide in documents {
            for caption in captions(&guide.text) {
                quoted += 1;
                assert!(
                    actual(caption),
                    "{} quotes nonexistent UI label: {caption}",
                    guide.id
                );
            }
        }
        assert!(quoted >= 20, "the scan must cover the full guide set");
    }
    #[test]
    fn recovery_candidates_alias_filter_and_empty_results_keep_all_owner_typed_operations() {
        let mut operations = Operations::new(recovery());
        let rows = operations.rows();
        assert_eq!(rows[0], OpKind::RunRetry);
        assert_eq!(rows.len(), 58);
        assert_eq!(
            rows.iter().collect::<std::collections::HashSet<_>>().len(),
            58
        );
        operations.paste("/retry");
        assert_eq!(operations.rows(), vec![OpKind::RunRetry]);
        operations.key(key(KeyCode::Enter));
        let form = operations.form.as_ref().unwrap();
        assert_eq!(form.operation, OpKind::RunRetry);
        assert_eq!(form.value("run").unwrap(), "current");
        operations.key(key(KeyCode::Esc));
        assert!(!operations.filter.is_empty());
        operations.key(key(KeyCode::Esc));
        assert!(operations.filter.is_empty());
        assert_eq!(operations.rows().len(), 58);
        operations.paste("no-such-operation-不存在");
        assert!(operations.rows().is_empty());
        operations.key(key(KeyCode::Enter));
        assert!(operations.form.is_none());
        operations.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        operations.paste(&"あ".repeat(200));
        assert_eq!(operations.filter.chars().count(), 128);
        operations.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        operations.paste("thread delete-preview");
        assert_eq!(operations.rows(), vec![OpKind::ThreadDeletePreview]);
    }
    #[test]
    fn activity_changes_refresh_future_defaults_without_overwriting_a_form_being_edited() {
        let mut operations = Operations::new(recovery());
        operations.key(key(KeyCode::Enter));
        operations.form.as_mut().unwrap().fields[0].value = "chosen-by-user".into();
        operations.refresh_activity(
            ActivityPhase::Completed,
            "new-run".into(),
            "new-turn".into(),
        );
        assert_eq!(
            operations.form.as_ref().unwrap().value("run").unwrap(),
            "chosen-by-user"
        );
        operations.key(key(KeyCode::Esc));
        assert!(!operations.suggested().contains(&OpKind::RunRetry));
        operations.paste("/stop");
        operations.key(key(KeyCode::Enter));
        assert_eq!(
            operations.form.as_ref().unwrap().value("run").unwrap(),
            "new-run"
        );
        let mut reference = Operations::new(Context {
            run: "past-reference".into(),
            current_run: "old-current".into(),
            follows_current: false,
            ..Default::default()
        });
        reference.refresh_activity(
            ActivityPhase::Running,
            "new-current".into(),
            "new-turn".into(),
        );
        reference.select(OpKind::RunGet);
        assert_eq!(
            reference.form.as_ref().unwrap().value("run").unwrap(),
            "past-reference"
        );
        reference.select(OpKind::RunStop);
        assert_eq!(
            reference.form.as_ref().unwrap().value("run").unwrap(),
            "new-current"
        );
    }
    #[test]
    fn grouped_action_list_keeps_selection_and_scrollbar_at_both_ends() {
        crate::ui::text_width::set_terminal_width(None);
        for (width, height) in [(60, 18), (120, 45)] {
            let mut operations = Operations::new(recovery());
            operations.set_language(UiLanguage::En);
            let actions = operations.rows().to_vec();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| operations.draw(frame, frame.area()))
                .unwrap();
            let body = operations.paint_bounds().unwrap().body;
            assert!(actions.len() > usize::from(body.height));
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(body.x + 2, body.y)].symbol(), "F");
            assert_eq!(buffer[(body.x + 2, body.y)].fg, theme::MUTED);
            assert_eq!(
                buffer[(body.x, body.y + 1)].symbol(),
                crate::ui::text_width::glyphs::SELECTED
            );
            assert_eq!(buffer[(body.x, body.y + 1)].fg, theme::SELECTION_FOREGROUND);
            assert_eq!(buffer[(body.x, body.y + 1)].bg, theme::SELECTION_BACKGROUND);
            assert_eq!(buffer[(body.right() - 1, body.y)].fg, theme::ACCENT);

            let mut previous_section = None;
            for expected in &actions {
                assert_eq!(OpKind::ALL[operations.selected], *expected);
                let section = operations.section(*expected);
                if previous_section != Some(section) {
                    terminal
                        .draw(|frame| operations.draw(frame, frame.area()))
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let selected_y = (body.y..body.bottom())
                        .find(|&y| {
                            buffer[(body.x, y)].symbol() == crate::ui::text_width::glyphs::SELECTED
                        })
                        .expect("a selected action row");
                    let selected_row = (body.x..body.right() - 1)
                        .map(|x| buffer[(x, selected_y)].symbol())
                        .collect::<String>();
                    let prefix = expected
                        .label_for(UiLanguage::En)
                        .chars()
                        .take(6)
                        .collect::<String>();
                    assert!(
                        selected_row.contains(&prefix),
                        "section {section}: {selected_row:?}"
                    );
                    assert_eq!(buffer[(body.x, selected_y)].bg, theme::SELECTION_BACKGROUND);
                    previous_section = Some(section);
                }
                operations.key(key(KeyCode::Down));
            }
            assert_eq!(
                previous_section,
                Some(2),
                "all three sections were traversed"
            );
            assert_eq!(OpKind::ALL[operations.selected], *actions.last().unwrap());
            terminal
                .draw(|frame| operations.draw(frame, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let last_y = body.bottom() - 1;
            assert_eq!(
                buffer[(body.x, last_y)].symbol(),
                crate::ui::text_width::glyphs::SELECTED
            );
            assert_eq!(buffer[(body.x, last_y)].fg, theme::SELECTION_FOREGROUND);
            assert_eq!(buffer[(body.x, last_y)].bg, theme::SELECTION_BACKGROUND);
            assert_eq!(buffer[(body.right() - 1, last_y)].fg, theme::ACCENT);
            let last_row = (body.x..body.right() - 1)
                .map(|x| buffer[(x, last_y)].symbol())
                .collect::<String>();
            let label = actions.last().unwrap().label_for(UiLanguage::En);
            let prefix = label.chars().take(6).collect::<String>();
            assert!(last_row.contains(&prefix), "{width}x{height}: {last_row:?}");

            operations.paste(&actions.last().unwrap().cli());
            assert!(!operations.rows().is_empty());
            assert!(operations.rows().len() <= usize::from(body.height));
            terminal
                .draw(|frame| operations.draw(frame, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert!((body.y..body.bottom()).all(|y| buffer[(body.right() - 1, y)].symbol() == " "));
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn shared_compact_forms_and_grouped_lists_keep_controls_visible_at_small_and_large_sizes() {
        for (width, height) in [(120, 45), (60, 18)] {
            let mut operations = Operations::new(recovery());
            let text = screen(&operations, width, height);
            assert!(text.contains("/retry"));
            assert!(text.contains("絞込み"));
            assert!(text.contains("Enter 入力フォーム"));
            if height > 20 {
                for heading in ["この会話で", "よく使う", "その他の操作"] {
                    assert!(text.contains(heading), "{heading}: {text}");
                }
            }
            operations.select(OpKind::RunGet);
            let text = screen(&operations, width, height);
            assert!(text.contains("作業 | 詳細"));
            assert!(text.contains("Esc 一覧"));
            assert!(text.contains("Enter 表示"));
            if height > 20 {
                assert!(
                    text.lines().take(8).all(|line| line.trim().is_empty()),
                    "one-field form must use the common compact frame"
                );
            }
        }
    }
    #[test]
    fn human_choice_values_do_not_change_the_typed_boolean_argument() {
        let mut form = Form::new(
            OpKind::ThreadDelete,
            &Context {
                thread: "thread".into(),
                ..Default::default()
            },
        );
        let descendants = form
            .fields
            .iter_mut()
            .find(|field| field.name == "descendants")
            .unwrap();
        descendants.value = "true".into();
        assert_eq!(descendants.displayed(), "はい");
        let Command::ThreadDelete(args) = form.command(Path::new("/")).unwrap() else {
            panic!()
        };
        assert!(args.descendants);
    }
}

#[cfg(test)]
mod language_rendering_tests {
    use super::{Command, Context, Form, OpKind};
    use crate::{config::UiLanguage, ui::UiState};
    use std::path::Path;

    #[test]
    fn every_english_form_field_has_an_english_label() {
        for &operation in OpKind::ALL {
            let mut form = Form::new(operation, &Context::default());
            form.language = UiLanguage::En;
            for field in &form.fields {
                let label = form.field_label_for(field);
                assert!(label.is_ascii(), "{operation:?} {}: {label}", field.name);
            }
        }
    }

    #[test]
    fn localized_validation_and_choices_preserve_command_values() {
        let mut form = Form::new(OpKind::DocsGet, &Context::default());
        form.language = UiLanguage::En;
        let error = match form.invocation(Path::new("/")) {
            Err(error) => error,
            Ok(_) => panic!("document is required"),
        };
        assert!(
            error.contains("document") && error.contains("required"),
            "{error}"
        );
        form.language = UiLanguage::Ja;
        let error = match form.invocation(Path::new("/")) {
            Err(error) => error,
            Ok(_) => panic!("document is required"),
        };
        assert!(error.contains("入力が必要"), "{error}");

        let mut form = Form::new(
            OpKind::ThreadDelete,
            &Context {
                thread: "thread-fixture".into(),
                ..Default::default()
            },
        );
        let field = form
            .fields
            .iter_mut()
            .find(|field| field.name == "descendants")
            .unwrap();
        field.value = "true".into();
        assert_eq!(field.displayed_for(UiLanguage::En), "Yes");
        assert_eq!(field.displayed_for(UiLanguage::Ja), "はい");
        for language in [UiLanguage::En, UiLanguage::Ja] {
            form.language = language;
            let Command::ThreadDelete(args) = form.command(Path::new("/")).unwrap() else {
                panic!("delete command")
            };
            assert_eq!(args.thread_id, "thread-fixture");
            assert!(args.descendants);
        }
    }

    #[test]
    fn single_wait_condition_is_bilingual_at_standard_widths() {
        fn configure(ui: &mut UiState) {
            ui.open_operations(Some(OpKind::Wait));
            let form = ui.operations.as_mut().unwrap().form.as_mut().unwrap();
            form.conditions
                .push(crate::protocol::WaitCondition::TurnCompleted {
                    turn_id: "turn-fixture".into(),
                });
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["1 observation condition", "F9"],
                UiLanguage::Ja => &["観測条件 1件", "F9"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn form_guide_is_bilingual_at_standard_widths_and_retains_safety_details() {
        fn configure(ui: &mut UiState) {
            use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
            ui.open_operations(Some(OpKind::ThreadDelete));
            ui.event(Event::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Guide", "Selected field:", "Thread ID", "Esc"],
                UiLanguage::Ja => &["使い方", "選択項目:", "会話ID", "Esc"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
        let mut form = Form::new(OpKind::ThreadDelete, &Context::default());
        for (language, warning) in [
            (
                UiLanguage::En,
                "Review deletion targets and running work before deleting",
            ),
            (UiLanguage::Ja, "削除対象と実行状態を確認してから削除します"),
        ] {
            form.language = language;
            let text = form
                .help_lines()
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains(warning), "{text}");
        }
    }

    #[test]
    fn client_confirmation_shows_cost_and_preserves_input_in_both_languages() {
        fn configure(ui: &mut UiState) {
            ui.open_operations(Some(OpKind::Exec));
            let operations = ui.operations.as_mut().expect("operations open");
            let form = operations.form.as_mut().expect("exec form open");
            for (name, value) in [("schema", "schema.json"), ("text", "PRIVATE-REQUEST")] {
                form.fields
                    .iter_mut()
                    .find(|field| field.name == name)
                    .unwrap()
                    .value = value.into();
            }
            let operation = match form.invocation(Path::new("/")) {
                Ok(super::Outcome::Client(operation)) => operation,
                _ => panic!("client operation must be prepared"),
            };
            assert!(
                matches!(&operation, super::ClientOperation::Exec(options) if options.text.as_deref() == Some("PRIVATE-REQUEST"))
            );
            form.client_confirmation = Some(operation);
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Model API calls", "does not stop", "Y Run"],
                UiLanguage::Ja => &["モデルAPIとtool", "対象Runを停止しません", "Y 実行"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }

    #[test]
    fn operations_list_is_bilingual_at_standard_widths() {
        fn configure(ui: &mut UiState) {
            ui.open_operations(None);
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Actions", "Esc"],
                UiLanguage::Ja => &["CLIと同じ操作", "Esc"],
            }
        }
        super::super::i18n::verify_screen(configure, expected);
    }
}
