//! Typed TUI operation forms. This module never parses arbitrary JSON or runs CLI.
use super::{activity::ActivityPhase, popup, prev_grapheme, theme};
use crate::protocol::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};
use std::path::{Path, PathBuf};

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
    ($($name:ident),+ $(,)?)=> {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub(super) enum OpKind {$($name),+}
        impl OpKind {pub const ALL:&'static [Self]=&[$(Self::$name),+];}
    };
}
operation_kinds! {
ThreadList,
    ThreadGet,
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
    ShellGet,
    ShellRead,
    ShellWrite,
    ShellEof,
    ShellStop,
    AttachmentAdd,
    AttachmentList,
    AttachmentRemove,
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
    pub fn label(self) -> &'static str {
        match self {
            Self::ThreadList => "会話 · 一覧",
            Self::ThreadGet => "会話 · 詳細",
            Self::ThreadCreate => "会話 · 作成",
            Self::ThreadFork => "会話 · 分岐",
            Self::ThreadDelete => "会話 · 削除",
            Self::ThreadSet => "会話 · 設定",
            Self::MessageSend => "依頼 · 送信",
            Self::MessageGet => "依頼 · 割当/回答",
            Self::OperationGet => "操作 · 受付結果",
            Self::RunGet => "作業 · 詳細",
            Self::RunRetry => "作業 · 再試行",
            Self::RunStop => "作業 · 停止",
            Self::TurnGet => "応答処理 · 詳細",
            Self::Wait => "待機 · 条件を観測",
            Self::HistoryList => "履歴 · 一覧",
            Self::HistoryGet => "履歴 · 原文",
            Self::HistorySearch => "履歴 · 検索",
            Self::ShellGet => "shell · 詳細",
            Self::ShellRead => "shell · 原文",
            Self::ShellWrite => "shell · 入力を送る",
            Self::ShellEof => "shell · 入力を閉じる",
            Self::ShellStop => "shell · 停止",
            Self::AttachmentAdd => "添付 · 追加",
            Self::AttachmentList => "添付 · 一覧",
            Self::AttachmentRemove => "添付 · 除外",
            Self::Compact => "圧縮 · 要求",
            Self::IndexStatus => "索引 · 状態",
            Self::IndexBackfill => "索引 · 過去履歴を追加",
            Self::IndexRetry => "索引 · 失敗分を再試行",
            Self::IndexRebuild => "索引 · 作り直す",
            Self::StorageStatus => "保存 · 状態",
            Self::StorageCollect => "保存 · 回収",
            Self::ModelList => "モデル · 一覧",
            Self::HistoryRelations => "履歴 · 関係",
            Self::ThreadDeletePreview => "会話 · 削除影響",
            Self::Status => "作業 · 状態一覧",
        }
    }
    fn wire_name(self) -> &'static str {
        match self {
            Self::ThreadList => "thread.list",
            Self::ThreadGet => "thread.get",
            Self::ThreadCreate => "thread.create",
            Self::ThreadFork => "thread.fork",
            Self::ThreadDelete => "thread.delete",
            Self::ThreadSet => "thread.set",
            Self::MessageSend => "message.send",
            Self::MessageGet => "message.get",
            Self::OperationGet => "operation.get",
            Self::RunGet => "run.get",
            Self::RunRetry => "run.retry",
            Self::RunStop => "run.stop",
            Self::TurnGet => "turn.get",
            Self::Wait => "wait",
            Self::HistoryList => "history.list",
            Self::HistoryGet => "history.get",
            Self::HistorySearch => "history.search",
            Self::ShellGet => "shell.get",
            Self::ShellRead => "shell.read",
            Self::ShellWrite => "shell.write",
            Self::ShellEof => "shell.eof",
            Self::ShellStop => "shell.stop",
            Self::AttachmentAdd => "attachment.add",
            Self::AttachmentList => "attachment.list",
            Self::AttachmentRemove => "attachment.remove",
            Self::Compact => "compact",
            Self::IndexStatus => "index.status",
            Self::IndexBackfill => "index.backfill",
            Self::IndexRetry => "index.retry",
            Self::IndexRebuild => "index.rebuild",
            Self::StorageStatus => "storage.status",
            Self::StorageCollect => "storage.collect",
            Self::ModelList => "model.list",
            Self::HistoryRelations => "history.relations",
            Self::ThreadDeletePreview => "thread.delete_preview",
            Self::Status => "status",
        }
    }
    fn cli(self) -> String {
        format!(
            "shellboy {}",
            self.wire_name().replace('.', " ").replace('_', "-")
        )
    }
    fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::ThreadList => Some("/threads"),
            Self::ThreadFork => Some("/fork"),
            Self::ThreadSet => Some("/model · /effort"),
            Self::MessageSend => Some("Enter"),
            Self::RunRetry => Some("/retry"),
            Self::RunStop => Some("/stop · Ctrl+C"),
            Self::HistorySearch => Some("/search"),
            Self::Compact => Some("/compact"),
            Self::AttachmentAdd => Some("下書きへ: /attach"),
            Self::AttachmentList => Some("/attachments"),
            Self::AttachmentRemove => Some("/detach"),
            Self::IndexStatus => Some("/index"),
            Self::IndexBackfill => Some("/index → B"),
            Self::IndexRetry => Some("/index → T"),
            Self::IndexRebuild => Some("/index → R"),
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
    kind: Kind,
}
impl Field {
    fn new(name: &'static str, label: &'static str, value: impl Into<String>, kind: Kind) -> Self {
        Self {
            name,
            label,
            value: value.into(),
            kind,
        }
    }
    fn displayed(&self) -> &str {
        if !matches!(self.kind, Kind::Boolean | Kind::Choice(_)) {
            return &self.value;
        }
        match self.value.as_str() {
            "true" => "はい",
            "false" => "いいえ",
            "" => "未指定",
            "turn_completed" => "応答処理の回答完了",
            "run_ended" => "作業の終了",
            "shell_ended" => "コマンドの終了",
            "shell_output" => "コマンド出力の更新",
            "threads" => "指定した会話",
            "all" => "全体",
            "raw" => "保存出力",
            "managed_images" => "管理画像",
            "history" => "履歴",
            "index" => "索引",
            "file" => "ファイル",
            "image" => "画像",
            "keep" => "変更しない",
            "set" => "設定する",
            "auto" => "自動",
            "previous" => "直前",
            "next" => "直後",
            "sources" => "出典",
            "replacements" => "置換関係",
            _ => &self.value,
        }
    }
}
pub(super) struct Operations {
    selected: usize,
    form: Option<Form>,
    context: Context,
    page: std::cell::Cell<usize>,
    filter: String,
}
type BuildCommand = fn(&Form, &Path) -> Result<Command, String>;

struct Form {
    operation: OpKind,
    fields: Vec<Field>,
    build: BuildCommand,
    selected: usize,
    conditions: Vec<WaitCondition>,
    error: Option<String>,
}
pub(super) enum Outcome {
    None,
    Close,
    Execute(Command),
}
impl Operations {
    pub fn new(context: Context) -> Self {
        let mut result = Self {
            selected: 0,
            form: None,
            context,
            page: std::cell::Cell::new(8),
            filter: String::new(),
        };
        if let Some(first) = result.rows().first() {
            result.selected = OpKind::ALL.iter().position(|kind| kind == first).unwrap();
        }
        result
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
    fn rows(&self) -> Vec<OpKind> {
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
                        "{} {} {}",
                        kind.label(),
                        kind.cli(),
                        kind.shortcut().unwrap_or("")
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
        let remaining = 128usize.saturating_sub(self.filter.chars().count());
        self.filter
            .extend(text.chars().filter(|ch| !ch.is_control()).take(remaining));
        self.ensure_selection();
    }
    pub fn refresh_activity(&mut self, phase: ActivityPhase, run: String, turn: String) {
        self.context.phase = phase;
        if self.context.follows_current {
            self.context.run = run.clone();
            self.context.turn = turn;
        }
        self.context.current_run = run;
        self.ensure_selection();
    }
    pub fn select(&mut self, operation: OpKind) {
        self.selected = OpKind::ALL
            .iter()
            .position(|kind| *kind == operation)
            .expect("every operation is listed");
        self.form = Some(Form::new(operation, &self.context));
    }
    pub fn paste(&mut self, text: &str) {
        if let Some(form) = &mut self.form {
            form.insert(text);
        } else {
            self.filter_text(text);
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        if let Some(form) = &mut self.form {
            if key.code == KeyCode::Esc {
                self.form = None;
                return Outcome::None;
            }
            match key.code {
                KeyCode::PageUp => {
                    form.selected = form.selected.saturating_sub(self.page.get());
                    return Outcome::None;
                }
                KeyCode::PageDown => {
                    form.selected = form
                        .selected
                        .saturating_add(self.page.get())
                        .min(form.fields.len().saturating_sub(1));
                    return Outcome::None;
                }
                _ => {}
            }
            return form.key(key, &self.context.cwd);
        }
        let rows = self.rows();
        let selected = rows
            .iter()
            .position(|kind| *kind == OpKind::ALL[self.selected])
            .unwrap_or(0);
        let next = match key.code {
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.ensure_selection();
                return Outcome::None;
            }
            KeyCode::Esc | KeyCode::F(8) => return Outcome::Close,
            KeyCode::Up => Some(selected.saturating_sub(1)),
            KeyCode::Down => Some((selected + 1).min(rows.len().saturating_sub(1))),
            KeyCode::PageUp => Some(selected.saturating_sub(self.page.get())),
            KeyCode::PageDown => Some(
                selected
                    .saturating_add(self.page.get())
                    .min(rows.len().saturating_sub(1)),
            ),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(rows.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(kind) = rows.get(selected) {
                    self.form = Some(Form::new(*kind, &self.context));
                }
                return Outcome::None;
            }
            KeyCode::Backspace => {
                self.filter
                    .truncate(prev_grapheme(&self.filter, self.filter.len()));
                self.ensure_selection();
                None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.clear();
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
        if let Some(kind) = next.and_then(|index| rows.get(index)) {
            self.selected = OpKind::ALL
                .iter()
                .position(|candidate| candidate == kind)
                .unwrap();
        }
        Outcome::None
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        if let Some(form) = &self.form {
            let context = if form.operation == OpKind::Wait {
                format!(
                    "{} / {}項目 · 待機条件 {}件（対象は停止しません）",
                    form.selected + 1,
                    form.fields.len(),
                    form.conditions.len()
                )
            } else {
                format!(
                    "{} / {}項目 · 入力値で操作を依頼します",
                    form.selected + 1,
                    form.fields.len()
                )
            };
            let footer = if form.operation == OpKind::Wait {
                "Esc 一覧 · Enter 送信 · Tab/↑↓/PgUp/PgDn 選択\nF9 条件追加 · F10 最後を削除 · ←→ 選択値"
            } else {
                "Esc 一覧 · Enter 送信 · Tab/↑↓/PgUp/PgDn 選択\n←→ 選択値 · Ctrl+U 消去 · Ctrl+J 複数行改行"
            };
            let height = form
                .fields
                .len()
                .saturating_mul(3)
                .saturating_add(7 + usize::from(form.error.is_some()) * 2)
                .min(u16::MAX as usize) as u16;
            let mut body = popup::compact(
                frame,
                area,
                form.operation.label(),
                &context,
                footer,
                height,
            );
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
                            &format!("{}: {}", field.label, field.displayed()),
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
                "新しい会話"
            } else if self.context.phase == ActivityPhase::Recovery {
                "復旧待ちの会話"
            } else if self.context.phase.is_active() {
                "実行中の会話"
            } else {
                "この会話"
            };
            let cli = rows
                .get(selected)
                .map(|kind| kind.cli())
                .unwrap_or_default();
            let context = format!(
                "{source} · {}/{}件 · 絞込み: {}\n{cli}",
                rows.len(),
                OpKind::ALL.len(),
                if self.filter.is_empty() {
                    "未指定"
                } else {
                    &self.filter
                }
            );
            let body = popup::panel(
                frame,
                area,
                "操作",
                &context,
                "文字入力で絞込み · Ctrl+U 解除 · ↑↓/PgUp/PgDn 選択\nEnter 入力フォーム · Esc 絞込み解除/戻る · F8 閉じる",
            );
            self.page
                .set(usize::from(body.height).saturating_sub(1).max(1));
            if rows.is_empty() {
                frame.render_widget(
                    Paragraph::new("該当する操作はありません。Ctrl+Uで絞込みを解除できます"),
                    body,
                );
            } else {
                let mut previous = None;
                let items = rows
                    .iter()
                    .map(|kind| {
                        let section = self.section(*kind);
                        let label = format!(
                            "{}{}",
                            kind.label(),
                            kind.shortcut()
                                .map(|shortcut| format!("  {shortcut}"))
                                .unwrap_or_default()
                        );
                        let text = if previous != Some(section) {
                            let header = match section {
                                0 => "この会話で",
                                1 => "よく使う",
                                _ => "すべての操作",
                            };
                            previous = Some(section);
                            format!("{header}\n{label}")
                        } else {
                            label
                        };
                        popup::item(&text, body.width)
                    })
                    .collect();
                popup::list(frame, body, items, selected);
            }
        }
    }
}
impl Form {
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
            OpKind::ThreadGet => (vec![thread()], |form, _cwd| {
                Ok(Command::ThreadGet(form.thread_target()?))
            }),
            OpKind::ThreadCreate => (
                vec![
                    f("cwd", "作業場所（空=継承/既定）", String::new(), Text),
                    f("connection", "接続（空=継承）", String::new(), Text),
                    f("model", "モデル（空=継承）", String::new(), Text),
                    f("effort", "推論強度（none=解除）", String::new(), Text),
                    f("parent", "親会話（空=独立）", String::new(), Text),
                ],
                |form, cwd| {
                    Ok(Command::ThreadCreate(ThreadCreateArgs {
                        cwd: form.path("cwd", cwd)?,
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
                ],
                |form, cwd| {
                    Ok(Command::ThreadFork(ThreadForkArgs {
                        thread_id: form.owned("thread")?,
                        node_id: form.owned("node")?,
                        cwd: form.path("cwd", cwd)?,
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
                        title: match form.value("title_mode")? {
                            "set" => Some(form.owned("title")?),
                            "auto" => Some(String::new()),
                            "keep" => None,
                            _ => return Err("タイトル操作を選択してください".into()),
                        },
                        thread_id: form.optional("thread")?,
                        cwd: form.path("cwd", cwd)?,
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
                ],
                |form, _cwd| {
                    Ok(Command::MessageSend(MessageSendArgs {
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
                        Choice(&["turn_completed", "run_ended", "shell_ended", "shell_output"]),
                    ),
                    f("target", "対象ID", c.turn.clone(), Text),
                    f("cursor", "出力観測の開始cursor", String::new(), Text),
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
                                return Err("待機条件は最大64件です".into());
                            }
                            conditions.push(form.condition()?);
                        }
                        Command::Wait(WaitArgs {
                            conditions,
                            timeout_ms: form.number("timeout")?,
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
                    ]);
                    v
                },
                |form, _cwd| {
                    Ok(Command::HistoryList(HistoryListArgs {
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
                    f("limit", "取得byte数", "65536".into(), Number),
                    f("cursor", "続きcursor", String::new(), Text),
                    f("encoding", "encoding", DEFAULT_ENCODING.into(), Text),
                ],
                |form, _cwd| {
                    Ok(Command::HistoryGet(HistoryGetArgs {
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
                        kind: AttachmentKind::parse(form.value("kind")?)
                            .ok_or("添付kindを選択してください")?,
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
            OpKind::Compact => (vec![thread()], |form, _cwd| {
                Ok(Command::Compact(form.thread_target()?))
            }),
            OpKind::IndexStatus => (index_fields(operation), |form, _cwd| {
                Ok(Command::IndexStatus(form.index_args()?))
            }),
            OpKind::IndexBackfill => (index_fields(operation), |form, _cwd| {
                Ok(Command::IndexBackfill(form.index_args()?))
            }),
            OpKind::IndexRetry => (index_fields(operation), |form, _cwd| {
                Ok(Command::IndexRetry(form.index_args()?))
            }),
            OpKind::IndexRebuild => (index_fields(operation), |form, _cwd| {
                Ok(Command::IndexRebuild(form.index_args()?))
            }),
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
                        relation: HistoryRelation::parse(form.value("relation")?)
                            .ok_or("関係を選択してください")?,
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
                        thread_id: form.owned("thread")?,
                        descendants: form.boolean("descendants")?,
                    }))
                },
            ),
            OpKind::Status => (
                vec![
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
                        operation_id: None,
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
            operation,
            fields,
            build,
            selected: 0,
            conditions: Vec::new(),
            error: None,
        }
    }
    fn value(&self, name: &str) -> Result<&str, String> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.as_str())
            .ok_or_else(|| {
                format!("必要な入力欄（{name}）が見つかりません。操作は送信していません")
            })
    }
    fn optional(&self, name: &str) -> Result<Option<String>, String> {
        let value = self.value(name)?;
        Ok((!value.is_empty()).then(|| value.to_owned()))
    }
    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<T, String> {
        self.value(name)?
            .parse()
            .map_err(|_| format!("{name}: 正の数値を入力してください"))
    }
    fn boolean(&self, name: &str) -> Result<bool, String> {
        self.value(name)?
            .parse()
            .map_err(|_| format!("{name}: true / false を選択してください"))
    }
    fn path(&self, name: &str, cwd: &Path) -> Result<Option<String>, String> {
        self.optional(name)?
            .map(|value| {
                let path = crate::input_path::relative_to(Path::new(&value), cwd);
                crate::input_path::utf8(&path)
                    .map(str::to_owned)
                    .map_err(|_| {
                        format!(
                            "{name}: パスはUTF-8で表現できる必要があります。操作は送信していません"
                        )
                    })
            })
            .transpose()
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
            return Err("待機対象IDが必要です".into());
        }
        Ok(match self.value("condition")? {
            "turn_completed" => WaitCondition::TurnCompleted { turn_id: id },
            "run_ended" => WaitCondition::RunEnded { run_id: id },
            "shell_ended" => WaitCondition::ShellEnded { execution_id: id },
            "shell_output" => WaitCondition::ShellOutput {
                execution_id: id,
                cursor: self.value("cursor")?.into(),
            },
            _ => return Err("待機条件typeが無効です".into()),
        })
    }
    fn insert(&mut self, text: &str) {
        let bytes = self.fields.iter().map(|f| f.value.len()).sum::<usize>();
        if bytes.saturating_add(text.len()) > 1024 * 1024 {
            self.error = Some("入力field合計は1MiBまでです。既存内容を保持しました".into());
            return;
        }
        let field = &mut self.fields[self.selected];
        if matches!(field.kind, Kind::Text | Kind::Multiline | Kind::Number) {
            field.value.push_str(text);
            self.error = None;
        }
    }
    fn key(&mut self, key: KeyEvent, cwd: &Path) -> Outcome {
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
                Ok(_) => self.error = Some("待機条件は最大64件です".into()),
                Err(error) => self.error = Some(error),
            },
            KeyCode::F(10) if self.operation == OpKind::Wait => {
                self.conditions.pop();
            }
            KeyCode::Enter => match self.command(cwd) {
                Ok(command) => return Outcome::Execute(command),
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
            scope: match self.value("scope")? {
                "" => None,
                value => Some(IndexScope::parse(value).ok_or("scopeを選択してください")?),
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
                StorageCategory::parse(self.value("category")?)
                    .ok_or("categoryを選択してください")?,
            ),
        })
    }
    fn command(&self, cwd: &Path) -> Result<Command, String> {
        let command = (self.build)(self, cwd)?;
        command.validate().map_err(|e| e.to_string())?;
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn human_terms_do_not_replace_cli_operation_identity() {
        for (kind, label, wire) in [
            (OpKind::ThreadGet, "会話 · 詳細", "thread.get"),
            (OpKind::RunGet, "作業 · 詳細", "run.get"),
            (OpKind::TurnGet, "応答処理 · 詳細", "turn.get"),
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
        ];
        assert_eq!(
            OpKind::ALL,
            expected.iter().map(|row| row.0).collect::<Vec<_>>()
        );
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
                        "model" => "model".into(),
                        _ => String::new(),
                    };
                }
            }
            let command = form
                .command(&context.cwd)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(command.name(), name);
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
            assert_eq!(command.requires_receipt(), name != "status");
            assert_eq!(
                command.runtime_cancellable(),
                matches!(
                    name,
                    "model.list"
                        | "history.search"
                        | "index.backfill"
                        | "index.rebuild"
                        | "index.retry"
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
                        | "storage.status"
                )
            );
            let actual = serde_json::to_value(command).unwrap();
            assert_eq!(actual["command"], name);
            for (key, value) in args.as_object().unwrap() {
                assert_eq!(&actual["args"][key], value, "{name} argument {key}");
            }
        }
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
        operations.form.as_mut().unwrap().selected = 4;
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
    fn recovery_candidates_alias_filter_and_empty_results_keep_all_thirty_six_typed_operations() {
        let mut operations = Operations::new(recovery());
        let rows = operations.rows();
        assert_eq!(rows[0], OpKind::RunRetry);
        assert_eq!(rows.len(), 36);
        assert_eq!(
            rows.iter().collect::<std::collections::HashSet<_>>().len(),
            36
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
        assert_eq!(operations.rows().len(), 36);
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
    fn shared_compact_forms_and_grouped_lists_keep_controls_visible_at_small_and_large_sizes() {
        for (width, height) in [(120, 45), (60, 18)] {
            let mut operations = Operations::new(recovery());
            let text = screen(&operations, width, height);
            assert!(text.contains("/retry"));
            assert!(text.contains("絞込み"));
            assert!(text.contains("Enter 入力フォーム"));
            if height > 20 {
                for heading in ["この会話で", "よく使う", "すべての操作"] {
                    assert!(text.contains(heading), "{heading}: {text}");
                }
            }
            operations.select(OpKind::RunGet);
            let text = screen(&operations, width, height);
            assert!(text.contains("作業 · 詳細"));
            assert!(text.contains("Esc 一覧"));
            assert!(text.contains("Enter 送信"));
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
