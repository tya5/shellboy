//! Reactive terminal UI. Files, database, network, and clipboard work are routed
//! through finite action queues; only terminal event/drawing I/O runs here.
pub mod actions;
mod autosave;
mod update_batch;

use crate::presentation::{NodePresentation, PresentationField};
use crate::{
    config::Config,
    protocol::{
        AttachmentRemoveArgs, Command, ContextEditAction, ContextEditArgs, HistorySearchArgs,
        IndexArgs, IndexScope, StatusArgs, ThreadDeleteArgs, ThreadDetails, ThreadForkArgs,
        ThreadSetArgs, ThreadSummary, ThreadTarget,
    },
};

pub mod activity;
mod bell;
mod chrome;
mod composer;
mod details;
pub(crate) mod draft;
mod help;
mod input;
mod lifecycle;
mod management;
mod models;
#[cfg(test)]
mod send_tests;
pub use management::ManagementPopup;
mod operations;
mod overlay;
mod popup;
mod presentation;
mod redraw;
pub mod startup;
mod terminal_guard;
pub(crate) mod theme;
use terminal_guard::TerminalGuard;
mod thread_deletion;
mod threads;
mod view;
use draft::{adjust_folds, prev_grapheme};
pub use threads::ThreadDisplayTime;
pub use view::ThreadView;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{self, EnterAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiAttachment {
    pub id: String,
    pub path: PathBuf,
    pub kind: String,
    pub state: String,
    /// Byte position of this attachment's UI marker; absent legacy refs append.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PasteFold {
    pub id: u64,
    pub start: usize,
    pub end: usize,
    pub expanded: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditRequestTarget {
    pub thread_id: String,
    pub node_id: String,
    pub run_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DraftSnapshot {
    pub draft_id: String,
    pub revision: u64,
    pub text: String,
    pub cursor: usize,
    pub attachments: Vec<UiAttachment>,
    pub folds: Vec<PasteFold>,
    pub literal_paste: bool,
    #[serde(default)]
    pub edit: Option<EditRequestTarget>,
}
impl Default for DraftSnapshot {
    fn default() -> Self {
        Self {
            draft_id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            text: String::new(),
            cursor: 0,
            attachments: Vec::new(),
            folds: Vec::new(),
            literal_paste: false,
            edit: None,
        }
    }
}
/// One bounded, transient source window used only to verify a completion anchor.
#[derive(Clone, Debug)]
pub struct StreamRebase {
    pub thread_id: String,
    pub generation: u64,
    pub view_revision: u64,
    pub node_id: String,
    pub source: UiNode,
    pub anchor: ViewAnchor,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewAnchor {
    pub node_id: Option<String>,
    pub char_offset: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiNode {
    pub node_id: String,
    pub role: String,
    pub text: String,
    pub base_char_offset: usize,
    pub total_chars: usize,
    #[serde(default)]
    pub reasoning_ranges: Vec<(usize, usize)>,
    #[serde(default)]
    pub presentation: NodePresentation,
}
impl UiNode {
    fn tool_stub(&self) -> Option<Self> {
        let blocks = self
            .presentation
            .blocks
            .iter()
            .filter_map(|block| {
                Some(crate::presentation::PresentationBlock {
                    kind: block.kind.clone(),
                    tool: Some(block.tool.clone()?),
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            return None;
        }
        Some(Self {
            node_id: self.node_id.clone(),
            role: self.role.clone(),
            text: String::new(),
            base_char_offset: 0,
            total_chars: 0,
            reasoning_ranges: vec![],
            presentation: NodePresentation {
                kind: self.presentation.kind.clone(),
                blocks,
                ..Default::default()
            },
        })
    }
    fn refresh_tools(&mut self, source: &Self) -> bool {
        if self.node_id != source.node_id {
            return false;
        }
        let mut changed = false;
        for block in &mut self.presentation.blocks {
            if let Some(tool) = &mut block.tool
                && let Some(incoming) = source
                    .presentation
                    .blocks
                    .iter()
                    .filter_map(|block| block.tool.as_ref())
                    .find(|next| next.call_id == tool.call_id)
                && tool != incoming
            {
                *tool = incoming.clone();
                changed = true;
            }
        }
        if changed {
            self.presentation.projection_stamp = None;
        }
        changed
    }
    fn same_projection(&self, other: &Self) -> bool {
        self.node_id == other.node_id
            && self.presentation.projection_stamp.is_some()
            && self.presentation.projection_stamp == other.presentation.projection_stamp
    }
    /// A failed repeat read must not shrink a paused viewport's known preview.
    /// Keep only the bounded display copy, while publishing the latest read error.
    fn preserve_preview_on_transient_failure(&mut self, previous: &mut Self) {
        use crate::presentation::{BlockKind, OutputStream, PresentationKind};
        let verified = |block: &crate::presentation::PresentationBlock| {
            block.kind == BlockKind::ToolResult
                && matches!(
                    block.stream,
                    Some(OutputStream::Stdout | OutputStream::Stderr)
                )
        };
        if self.node_id != previous.node_id
            || self.text != previous.text
            || self.base_char_offset != previous.base_char_offset
            || self.total_chars != previous.total_chars
            || self.presentation.kind != PresentationKind::ToolResult
            || previous.presentation.kind != PresentationKind::ToolResult
            || !previous.presentation.blocks.iter().any(verified)
            || self.presentation.blocks.iter().any(verified)
            || !self.presentation.fields.iter().any(|field| {
                field.key == "/ui_raw_stream_preview_error" && field.value == "IoFailed"
            })
        {
            return;
        }
        self.presentation.projection_stamp = None;
        self.presentation.blocks = std::mem::take(&mut previous.presentation.blocks);
        self.presentation.links = std::mem::take(&mut previous.presentation.links);
        self.presentation.details_omitted |= previous.presentation.details_omitted;
        self.presentation.metadata_limited |= previous.presentation.metadata_limited;
        self.presentation.body_omitted_chars = previous.presentation.body_omitted_chars;
        if let Some(note) = self
            .presentation
            .fields
            .iter_mut()
            .find(|field| field.key == "/ui_raw_stream_preview")
        {
            note.value = "保持済みの表示用コピーを継続表示。今回のraw再取得はIoFailed。現在のraw可用性は未確認。".into();
        }
    }
    fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.node_id.capacity())
            .saturating_add(self.role.capacity())
    }
    fn evict_body(&mut self) {
        self.text = String::new();
        self.reasoning_ranges = Vec::new();
        self.presentation.clear();
    }
    fn cached_bytes(&self) -> usize {
        self.presentation
            .retained_bytes()
            .saturating_sub(std::mem::size_of::<NodePresentation>())
            .saturating_add(self.text.capacity())
            .saturating_add(
                self.reasoning_ranges
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(usize, usize)>()),
            )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReceiptStatus {
    Accepting,
    Accepted,
    Preparing,
    Held,
    Failed,
    Cancelled,
    Completed,
}
#[derive(Clone, Debug)]
pub struct RecalledPrompt {
    pub ordinal: u64,
    pub node_id: String,
    pub text: String,
}
/// Identity of a UI display request; it never cancels a durable operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagementContext {
    pub epoch: u64,
    pub owner_draft: String,
    pub owner_thread: Option<String>,
}
/// Correlates a slash operation with its immutable UI input, never a core receipt.
#[derive(Clone, Debug)]
pub struct CommandSubmission {
    pub draft_id: String,
    pub revision: u64,
}
/// Copy extent is UI metadata; original text and core receipts are unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyScope {
    OriginalPage { has_more: bool },
    ManagementPage { has_more: bool },
    Value,
    Body,
    CodeBlock,
}
impl CopyScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OriginalPage { has_more: true } => "原文の現在ページ（続きあり）",
            Self::OriginalPage { has_more: false } => "原文の現在ページ（このページのみ）",
            Self::ManagementPage { has_more: true } => "管理詳細の現在ページ（続きあり）",
            Self::ManagementPage { has_more: false } => "管理詳細の現在ページ（このページのみ）",
            Self::Value => "項目の完全な値",
            Self::Body => "本文全体",
            Self::CodeBlock => "コードブロック全体",
        }
    }
}
#[derive(Clone, Debug)]
pub enum UiAction {
    ThreadDeletion {
        request_id: u64,
        args: ThreadDeleteArgs,
        execute: bool,
        known_thread_ids: Vec<String>,
    },
    ManagementIntent {
        context: ManagementContext,
        action: Box<UiAction>,
    },
    RefreshTools {
        thread_id: String,
        generation: u64,
        revision: u64,
        nodes: Vec<UiNode>,
    },
    SubmittedCommand {
        submission: CommandSubmission,
        action: Box<UiAction>,
    },
    FollowManagement {
        request_id: u64,
        pointer: String,
        mode: ManagementFollow,
    },
    LoadReceived {
        thread_id: String,
        page: u32,
    },
    LoadModels {
        connection: String,
        request_id: u64,
    },
    RecallInput {
        thread_id: String,
        generation: u64,
        request_id: u64,
        before_ordinal: Option<u64>,
        before_node_id: Option<String>,
        older: bool,
    },
    RenderWidth {
        width: u16,
    },
    CopyText {
        text: String,
        scope: CopyScope,
    },
    LoadOriginal {
        node_id: String,
        cursor: Option<String>,
        request_id: u64,
    },
    LoadManagementDetail {
        request_id: u64,
        pointer: String,
        cursor: Option<String>,
    },
    CancelWait,

    Send {
        draft_id: String,
        revision: u64,
        thread_id: Option<String>,
        cwd: PathBuf,
        text: String,
        attachments: Vec<UiAttachment>,
        connection: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    LoadEditRequest {
        thread_id: String,
        node_id: String,
        generation: u64,
        draft_revision: u64,
    },
    SendEdited {
        draft_id: String,
        revision: u64,
        source_thread_id: String,
        source_node_id: String,
        cwd: PathBuf,
        text: String,
        attachments: Vec<UiAttachment>,
        connection: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        confirmed_run_id: Option<String>,
    },
    ReloadAttachments {
        thread_id: String,
    },
    IndexPreview(IndexArgs),
    Manage(Command),
    Command(Command),
    RestoreSnapshot {
        draft_id: String,
        revision: u64,
        node_id: String,
        expected_revision: u64,
    },
    LoadHistoryWindow {
        thread_id: String,
        generation: u64,
        view_revision: u64,
        ordinal: u64,
        char_offset: usize,
        from_end: bool,
        selection_revision: u64,
    },
    RebaseStream(Box<StreamRebase>),
    RebaseHistory {
        thread_id: String,
        generation: u64,
        view_revision: u64,
        anchor: ViewAnchor,
    },
    LoadThreads {
        cursor: Option<String>,
        filter: String,
        generation: u64,
    },
    SelectThread {
        thread_id: String,
        generation: u64,
        view_revision: u64,
        known_view: Option<(ViewAnchor, bool)>,
        preview_sources: Vec<UiNode>,
    },
    ActivateDraft {
        draft_id: String,
    },
    ClipboardDecision {
        draft_id: String,
        attachment_id: String,
        accepted: bool,
    },
    DraftAttachments {
        draft_id: String,
        revision: u64,
        attachment_ids: Vec<String>,
    },
    LoadNodeWindow {
        thread_id: String,
        generation: u64,
        node_id: String,
        char_offset: usize,
        view_revision: u64,
    },
    LoadHistory {
        thread_id: String,
        generation: u64,
        before_cursor: Option<String>,
    },
    SaveDraft {
        thread_id: String,
        draft: DraftSnapshot,
        anchor: ViewAnchor,
        follow: bool,
    },
    Copy {
        node_id: String,
        code_block: Option<usize>,
    },
    PasteImage {
        draft_id: String,
        revision: u64,
        thread_id: Option<String>,
        position: usize,
    },
    Cancel {
        thread_id: String,
    },
    Background {
        thread_id: String,
    },
    Quit,
    ForceQuit,
    WaitForShutdown,
}
// Queue capacity is finite; keeping snapshots inline avoids changing the public
// worker protocol while heap-backed bodies remain separately byte-limited.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum UiUpdate {
    ThreadDeletion {
        request_id: u64,
        args: ThreadDeleteArgs,
        execute: bool,
        result: Result<ThreadDeletionReply, crate::protocol::ApiError>,
    },
    /// Committed recovery transition from any Thread, independent of selection.
    RecoveryBell,
    ScopedManagement {
        context: ManagementContext,
        update: Box<UiUpdate>,
    },
    ToolFacts {
        thread_id: String,
        generation: u64,
        revision: u64,
        result: Result<Vec<UiNode>, String>,
    },
    CommandOutcome {
        submission: CommandSubmission,
        result: Result<(), String>,
    },
    Activity {
        thread_id: String,
        generation: u64,
        epoch: u64,
        snapshot: activity::ActivitySnapshot,
        observed_at: Instant,
    },
    ModelReceive {
        thread_id: String,
        generation: u64,
        attempt_id: String,
        observed_at: Instant,
    },
    ActivityUnavailable {
        thread_id: String,
        generation: u64,
        epoch: u64,
    },
    Models {
        request_id: u64,
        result: Result<Vec<String>, String>,
    },
    RecalledInput {
        thread_id: String,
        generation: u64,
        request_id: u64,
        result: Result<Option<RecalledPrompt>, String>,
    },
    OriginalPage {
        request_id: u64,
        body: crate::protocol::HistoryBody,
        display: Vec<Line<'static>>,
    },
    StructuredManagement {
        /// True only for a Status overview; operation receipt lookup is a general report.
        work_status: bool,
        request_id: u64,
        title: String,
        fields: Vec<PresentationField>,
        omitted: bool,
    },
    ManagementDetail {
        request_id: u64,
        pointer: String,
        text: String,
        next_cursor: Option<String>,
    },

    Initialize {
        cwd: PathBuf,
    },
    ThreadListFailed {
        generation: u64,
        error: String,
    },
    Threads {
        generation: u64,
        requested_cursor: Option<String>,
        threads: Vec<ThreadSummary>,
        display_times: Vec<ThreadDisplayTime>,
        next_cursor: Option<String>,
    },
    Thread {
        generation: u64,
        details: ThreadDetails,
        draft: Option<DraftSnapshot>,
    },
    StorageFailed {
        thread_id: String,
        message: String,
    },
    DraftSaveFailed {
        thread_id: String,
        message: String,
    },
    ReceiptSource {
        draft_id: String,
        revision: u64,
        node_id: String,
    },
    RestoredSnapshot {
        draft_id: String,
        revision: u64,
        expected_revision: u64,
        text: String,
        attachments: Vec<UiAttachment>,
    },
    LoadFailed {
        thread_id: String,
        generation: u64,
        view_revision: Option<u64>,
        message: String,
    },
    HistoryInvalidated {
        thread_id: String,
        generation: u64,
        view_revision: Option<u64>,
    },
    HistoryWindow {
        thread_id: String,
        generation: u64,
        view_revision: u64,
        nodes: Vec<UiNode>,
        first_ordinal: u64,
        selection_revision: u64,
        total_nodes: u64,
        anchor: ViewAnchor,
    },
    History {
        thread_id: String,
        generation: u64,
        nodes: Vec<UiNode>,
        before_cursor: Option<String>,
        append: bool,
        first_ordinal: u64,
        selection_revision: u64,
        total_nodes: u64,
    },
    Receipt {
        draft_id: String,
        revision: u64,
        thread_id: Option<String>,
        status: ReceiptStatus,
        message: Option<String>,
    },
    StreamCommitted {
        thread_id: String,
        generation: u64,
        attempt_id: String,
        node_id: String,
    },
    Stream {
        thread_id: String,
        generation: u64,
        node_id: String,
        role: String,
        text: String,
        base_char_offset: usize,
        total_chars: usize,
        reasoning_ranges: Vec<(usize, usize)>,
        presentation: NodePresentation,
        ack: Option<Arc<tokio::sync::Notify>>,
    },
    HistoryRebaseNotice {
        thread_id: String,
        generation: u64,
        view_revision: u64,
        anchor: ViewAnchor,
        message: String,
    },
    ReceivedList {
        title: String,
        fields: Vec<PresentationField>,
    },
    IncompleteResponses {
        thread_id: String,
        generation: u64,
        selection_revision: u64,
        view_revision: Option<u64>,
        root_revision: u64,
        // A saved preview belongs after this selected node, never at the tail
        // of an unrelated history page. This is not a canonical ordinal.
        nodes: Vec<(String, UiNode)>,
    },
    StreamSettled {
        thread_id: String,
        generation: u64,
        attempt_id: String,
        received_source: Option<(String, String)>,
        ack: Option<Arc<tokio::sync::Notify>>,
    },
    ViewState {
        thread_id: String,
        generation: u64,
        view_revision: u64,
        anchor: ViewAnchor,
        follow: bool,
    },
    NodeWindow {
        thread_id: String,
        generation: u64,
        view_revision: u64,
        node: UiNode,
    },
    DraftStatus {
        draft_id: String,
        text: String,
    },
    TerminalRequest {
        bytes: Vec<u8>,
    },
    Status {
        thread_id: Option<String>,
        text: String,
    },
    ClipboardAttachment {
        draft_id: String,
        revision: u64,
        attachment: UiAttachment,
    },
    Progress {
        thread_id: String,
        tool_calls: u64,
        cancelled_calls: u64,
        state: String,
    },
    EditRequestLoaded {
        thread_id: String,
        node_id: String,
        generation: u64,
        draft_revision: u64,
        text: String,
        attachments: Vec<UiAttachment>,
        run_id: Option<String>,
    },
    EditConfirmationRequired {
        draft_id: String,
        revision: u64,
        run_id: String,
    },
    Management {
        title: String,
        text: String,
    },
    IndexPreviewReady {
        args: IndexArgs,
        text: String,
    },
    ShutdownProgress {
        elapsed_ms: u64,
        detail: String,
        force_available: bool,
    },
    ForceShutdown,
    Shutdown,
}
#[derive(Clone, Debug)]
pub struct PendingSend {
    command_cleared: Option<DraftSnapshot>,
    pub snapshot: DraftSnapshot,
    pub status: ReceiptStatus,
    pub message: Option<String>,
    pub source_node_id: Option<String>,
    send: Option<SendProgress>,
}
#[derive(Clone, Debug)]
struct SendProgress {
    started_at: Instant,
    reflected: bool,
}
#[derive(Clone, Debug)]
pub struct ManagementView {
    pub title: String,
    pub text: String,
    pub scroll: u16,
}
#[derive(Clone, Debug)]
pub enum ThreadDeletionReply {
    Preview(serde_json::Value),
    Deleted(Vec<String>),
    DeletedWithWarning { ids: Vec<String>, warning: String },
}
#[derive(Clone, Debug)]
enum Confirmation {
    ThreadDeletion(thread_deletion::DeletionDialog),
    EditStop {
        draft_id: String,
        revision: u64,
        run_id: String,
    },
    Index {
        command: Command,
        detail: String,
    },
    Operation {
        command: Command,
        detail: String,
    },
    ForceQuit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiExitReason {
    Normal,
    Forced,
}

impl UiUpdate {
    /// Prepare a raw page's display on a presentation worker, before UI delivery.
    pub fn original_page(request_id: u64, body: crate::protocol::HistoryBody) -> Self {
        let display = details::original_page_lines(&body);
        Self::OriginalPage {
            request_id,
            body,
            display,
        }
    }
}

// Cache keys contain revisions rather than copies or hashes of retained source
// bodies. Normal input/status drawing therefore does no history-body scan.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HistoryLayoutKey {
    view: String,
    thread: Option<String>,
    history_revision: u64,
    selection_revision: u64,
    anchor: Option<String>,
    expanded: Vec<String>,
    width: u16,
}
struct HistoryLayout {
    text: String,
    styled: Vec<Line<'static>>,
    lines: usize,
    targets: Vec<Arc<str>>,
    sources: Vec<presentation::RowSource>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RenderUnitKey {
    start: usize,
    end: usize,
    width: u16,
    stamps: Vec<([u8; 32], usize, usize)>,
    expanded: Vec<bool>,
    preceding_links: Vec<Arc<str>>,
}
impl RenderUnitKey {
    fn new(
        view: &ThreadView,
        start: usize,
        end: usize,
        width: u16,
        links: &[Arc<str>],
    ) -> Option<Self> {
        Some(Self {
            start,
            end,
            width,
            stamps: view.nodes()[start..end]
                .iter()
                .map(|node| {
                    node.presentation
                        .projection_stamp
                        .map(|stamp| (stamp, node.base_char_offset, node.total_chars))
                })
                .collect::<Option<Vec<_>>>()?,
            expanded: view.nodes()[start..end]
                .iter()
                .map(|node| view.reasoning_expanded.contains(&node.node_id))
                .collect(),
            preceding_links: links.to_vec(),
        })
    }
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.stamps.capacity() * std::mem::size_of::<([u8; 32], usize, usize)>()
            + self.expanded.capacity()
            + self.preceding_links.capacity() * std::mem::size_of::<Arc<str>>()
            + self
                .preceding_links
                .iter()
                .map(|link| link.len())
                .sum::<usize>()
    }
}
struct HistoryLayoutEntry {
    value: Arc<HistoryLayout>,
    bytes: usize,
    touched: u64,
}
#[derive(Default)]
struct HistoryLayoutCache {
    entries: BTreeMap<HistoryLayoutKey, HistoryLayoutEntry>,
    units: BTreeMap<RenderUnitKey, HistoryLayoutEntry>,
    bytes: usize,
    clock: u64,
    #[cfg(test)]
    builds: usize,
    #[cfg(test)]
    unit_hits: usize,
    #[cfg(test)]
    unit_builds: usize,
}
impl HistoryLayoutKey {
    fn new(view_key: &str, view: &ThreadView, width: u16, _height: u16) -> Self {
        Self {
            view: view_key.into(),
            thread: view.thread_id.clone(),
            history_revision: view.history_revision(),
            selection_revision: view.selection_revision,
            anchor: view
                .nodes()
                .get(projection_node_index(view))
                .map(|n| n.node_id.clone()),
            expanded: view.reasoning_expanded.iter().cloned().collect(),
            width,
        }
    }
    fn bytes(&self) -> usize {
        self.view.capacity()
            + self.thread.as_ref().map_or(0, String::capacity)
            + self.anchor.as_ref().map_or(0, String::capacity)
            + self.expanded.capacity() * std::mem::size_of::<String>()
            + self.expanded.iter().map(String::capacity).sum::<usize>()
    }
}
impl HistoryLayoutCache {
    fn evict(&mut self, budget: usize) {
        while self.bytes > budget {
            let whole = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, entry)| (key.clone(), entry.touched));
            let unit = self
                .units
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, entry)| (key.clone(), entry.touched));
            let removed = match (whole, unit) {
                (Some((_whole, wt)), Some((unit, ut))) if ut < wt => self.units.remove(&unit),
                (Some((whole, _)), _) => self.entries.remove(&whole),
                (None, Some((unit, _))) => self.units.remove(&unit),
                (None, None) => break,
            };
            if let Some(old) = removed {
                self.bytes -= old.bytes;
            }
        }
    }
    fn unit(&mut self, key: &RenderUnitKey) -> Option<Arc<HistoryLayout>> {
        #[cfg(test)]
        {
            if self.units.contains_key(key) {
                self.unit_hits += 1;
            }
        }
        self.clock = self.clock.saturating_add(1);
        self.units.get_mut(key).map(|entry| {
            entry.touched = self.clock;
            Arc::clone(&entry.value)
        })
    }
    fn insert_unit(&mut self, key: RenderUnitKey, value: Arc<HistoryLayout>, budget: usize) {
        #[cfg(test)]
        {
            self.unit_builds += 1;
        }
        let bytes = key.bytes()
            + value.text.capacity()
            + presentation::bytes(&value.styled)
            + value.targets.capacity() * std::mem::size_of::<Arc<str>>()
            + value.targets.iter().map(|link| link.len()).sum::<usize>()
            + value.sources.capacity() * std::mem::size_of::<presentation::RowSource>()
            + std::mem::size_of::<HistoryLayout>()
            + std::mem::size_of::<HistoryLayoutEntry>()
            + 128;
        if bytes > budget || budget == 0 {
            return;
        }
        if let Some(old) = self.units.remove(&key) {
            self.bytes -= old.bytes;
        }
        self.evict(budget - bytes);
        self.clock = self.clock.saturating_add(1);
        self.bytes += bytes;
        self.units.insert(
            key,
            HistoryLayoutEntry {
                value,
                bytes,
                touched: self.clock,
            },
        );
    }

    fn discard_old_revision(&mut self, key: &HistoryLayoutKey) {
        let bytes = &mut self.bytes;
        self.entries.retain(|old, entry| {
            if old.view == key.view && old.history_revision != key.history_revision {
                *bytes -= entry.bytes;
                false
            } else {
                true
            }
        });
    }
    fn get(&mut self, key: &HistoryLayoutKey) -> Option<Arc<HistoryLayout>> {
        self.clock = self.clock.saturating_add(1);
        self.entries.get_mut(key).map(|entry| {
            entry.touched = self.clock;
            Arc::clone(&entry.value)
        })
    }
    fn insert(&mut self, key: HistoryLayoutKey, value: Arc<HistoryLayout>, budget: usize) {
        // Include owned key/entry/control-block storage and conservative BTree
        // link overhead, in addition to the actual allocated String capacities.
        let bytes = key.bytes()
            + value.text.capacity()
            + presentation::bytes(&value.styled)
            + value.targets.capacity() * std::mem::size_of::<Arc<str>>()
            + value.targets.iter().map(|url| url.len()).sum::<usize>()
            + value.sources.capacity() * std::mem::size_of::<presentation::RowSource>()
            + std::mem::size_of::<HistoryLayoutKey>()
            + std::mem::size_of::<HistoryLayoutEntry>()
            + std::mem::size_of::<HistoryLayout>()
            + 128;
        if budget == 0 || bytes > budget {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.bytes;
        }
        self.evict(budget - bytes);
        self.clock = self.clock.saturating_add(1);
        self.bytes += bytes;
        self.entries.insert(
            key,
            HistoryLayoutEntry {
                value,
                bytes,
                touched: self.clock,
            },
        );
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ManagementFollow {
    Inspect,
    Read,
    Next,
    Refresh,
}
const HELP_FOOTER: &str = "Esc / Enter / F1 戻る · ↑↓ / PgUp / PgDn 閲覧";
const MANAGEMENT_DETAIL_FOOTER: &str =
    "Esc 戻る · ↑↓/PgUp/PgDn 閲覧\nN 次page · P 先頭 · C このpageをコピー";
const CONFIRMATION_FOOTER: &str = "N / Esc 戻る · Y 実行\n↑↓/PgUp/PgDn 閲覧";
const CONFIRMATION_CONTEXT: &str = "内容を確認してください";
impl Confirmation {
    fn footer(&self) -> &'static str {
        if matches!(self, Self::ThreadDeletion(_)) {
            thread_deletion::FOOTER
        } else {
            CONFIRMATION_FOOTER
        }
    }
    fn layout(&self, area: Rect) -> (String, u16) {
        let text = match self {
                Confirmation::ThreadDeletion(dialog)=>dialog.text(),
                Confirmation::EditStop{run_id,..}=>format!("旧作業 {run_id} を停止しますか？\n停止の終了状態の確定を待ってから元依頼の直前まで新しい会話を分岐し、編集本文を新規送信します。元依頼・旧会話履歴は変更しません。"),
                Confirmation::Index{command,detail}=>format!("索引処理の実行確認 · {}\n{detail}",match command { Command::IndexBackfill(_)=>"backfill", Command::IndexRetry(_)=>"retry", Command::IndexRebuild(_)=>"rebuild", _=>"索引操作" }),
                Confirmation::Operation{detail,..}=>detail.clone(),
                Confirmation::ForceQuit=>"強制終了しますか？\n保存/回収が未完了の可能性があります。プロセスを終了し、次回起動で復旧します。".into(),
            };
        let width = popup::reader_width(popup::body_area(
            area,
            CONFIRMATION_CONTEXT,
            self.footer(),
            None,
        ))
        .max(1);
        let rows = Paragraph::new(text.as_str())
            .wrap(Wrap { trim: false })
            .line_count(width);
        // Border 2 + context 2 + divider 1 + the common footer's at most two rows.
        let overhead = 5 + self.footer().lines().count().clamp(1, 2);
        let height = rows.saturating_add(overhead).clamp(8, u16::MAX as usize) as u16;
        (text, height)
    }
    fn page(&self, area: Rect) -> u16 {
        let (_, height) = self.layout(area);
        popup::body_area(area, CONFIRMATION_CONTEXT, self.footer(), Some(height))
            .height
            .saturating_sub(1)
            .max(1)
    }
}
pub struct UiState {
    bell: bell::Bell,
    autosave: autosave::Autosave,
    batching_updates: bool,
    retention_pending: bool,
    #[cfg(test)]
    retention_passes: usize,
    animation: activity::Animation,
    pub views: BTreeMap<String, ThreadView>,
    pub active: String,
    thread_browser: threads::ThreadBrowser,
    pub selection_mode: bool,
    pub help_visible: bool,
    help_page: help::Page,
    help_scroll: u16,
    viewport: Rect,
    confirmation_scroll: u16,
    pub dirty: bool,
    full_redraw_requested: bool,
    lifecycle: lifecycle::Lifecycle,
    // One static UI rejection, no pasted content; cleared by the next input gesture.
    input_notice: Option<&'static str>,
    pub management: management::ManagementPopup,
    confirmation: Option<Confirmation>,
    config: Config,
    startup_cwd: PathBuf,
    initialized: bool,
    generation: u64,
    next_fold: u64,
    deferred_actions: Vec<UiAction>,
    terminal_requests: Vec<Vec<u8>>,
    layout_cache: HistoryLayoutCache,
    visible_links: Vec<crate::terminal_links::LinkRegion>,
    render_width: u16,
    render_pending: BTreeSet<(String, u64, u64, String, u16, usize)>,
    details: Option<details::Details>,
    operations: Option<operations::Operations>,
    models: Option<models::ModelPicker>,
    next_model_request: u64,
    next_detail_request: u64,
    next_input_request: u64,
    next_thread_deletion_request: u64,
    thread_deletions_inflight: BTreeMap<String, u64>,
    command_rejected: bool,
}
// Preserve source-window I/O intent before viewport reconciliation initializes
// or clamps the display row. These facts live for this draw only.
#[derive(Clone, Copy)]
struct RenderPreflight {
    anchor_changed: bool,
    initializing: bool,
}
impl RenderPreflight {
    fn new(view: &ThreadView) -> Self {
        Self {
            anchor_changed: !view.scroll.initialized
                || view.scroll.anchor != view.anchor
                || view.scroll.revision != view.history_revision(),
            initializing: !view.scroll.initialized,
        }
    }
}

impl UiState {
    pub fn threads(&self) -> &[ThreadSummary] {
        self.thread_browser.items()
    }
    pub fn thread_selection(&self) -> usize {
        self.thread_browser.selected()
    }
    pub fn threads_visible(&self) -> bool {
        self.thread_browser.visible()
    }
    pub fn thread_filter(&self) -> &str {
        self.thread_browser.filter()
    }
    pub fn threads_next_cursor(&self) -> Option<&str> {
        self.thread_browser.next_cursor()
    }
    pub fn list_generation(&self) -> u64 {
        self.thread_browser.generation()
    }
    pub fn new(config: &Config, cwd: PathBuf) -> Self {
        let view = ThreadView::new(config, cwd.clone());
        let active = view.draft.draft_id.clone();
        let initialized = cwd.is_absolute();
        Self {
            bell: bell::Bell::default(),
            animation: activity::Animation::new(Instant::now()),
            views: BTreeMap::from([(active.clone(), view)]),
            active,
            thread_browser: threads::ThreadBrowser::default(),
            selection_mode: false,
            help_visible: false,
            help_page: help::Page::guide(config),
            help_scroll: 0,
            viewport: Rect::default(),
            confirmation_scroll: 0,
            dirty: true,
            full_redraw_requested: false,
            lifecycle: Default::default(),
            input_notice: None,
            management: management::ManagementPopup::default(),
            confirmation: None,
            config: config.clone(),
            startup_cwd: cwd,
            initialized,
            command_rejected: false,
            generation: 0,
            next_fold: 1,
            deferred_actions: Vec::new(),
            terminal_requests: Vec::new(),
            layout_cache: HistoryLayoutCache::default(),
            visible_links: vec![],
            render_width: 0,
            render_pending: BTreeSet::new(),
            details: None,
            operations: None,
            models: None,
            next_model_request: 0,
            autosave: Default::default(),
            batching_updates: false,
            retention_pending: false,
            #[cfg(test)]
            retention_passes: 0,
            next_detail_request: 0,
            next_input_request: 0,
            next_thread_deletion_request: 0,
            thread_deletions_inflight: BTreeMap::new(),
        }
    }
    pub fn view(&self) -> &ThreadView {
        &self.views[&self.active]
    }
    /// Draft, status and viewport edits leave source revisions intact.
    /// Retained history enters through matched updates on its owning view.
    pub fn view_mut(&mut self) -> &mut ThreadView {
        self.active_view_mut()
    }
    fn view_for(&mut self, thread: &str, generation: u64) -> Option<&mut ThreadView> {
        let (key, view) = self
            .views
            .iter_mut()
            .find(|(_, view)| view.matches(thread, generation))?;
        self.autosave.touch(key);
        Some(view)
    }
    fn active_view_mut(&mut self) -> &mut ThreadView {
        self.autosave.touch(&self.active);
        self.views
            .get_mut(&self.active)
            .expect("active view exists")
    }
    #[cfg(test)]
    fn prepare_render_width(&mut self, width: u16) {
        let view = self.view();
        let preflight = RenderPreflight::new(view);
        let visible = self
            .layout_cache
            .entries
            .iter()
            .filter(|(key, _)| {
                key.view == self.active && key.history_revision == view.history_revision()
            })
            .max_by_key(|(_, entry)| entry.touched)
            .map(|(_, entry)| {
                visible_render_nodes(
                    &entry.value,
                    view.scroll.row,
                    view.scroll.height,
                    view.nodes().len(),
                )
            })
            .unwrap_or_default();
        self.prepare_visible_render_width(width, &visible, preflight);
    }
    fn prepare_visible_render_width(
        &mut self,
        width: u16,
        visible: &BTreeSet<usize>,
        preflight: RenderPreflight,
    ) {
        if self.render_width != width {
            self.render_width = width;
            self.render_pending.clear();
            self.deferred_actions.push(UiAction::RenderWidth { width });
        }
        let view = self.view();
        let Some(thread) = view.thread_id.clone() else {
            return;
        };
        if view.scroll.request.is_some() {
            return;
        }
        let mut working = working_node_ids(view);
        working.extend(
            visible
                .iter()
                .filter_map(|index| view.nodes().get(*index).map(|node| node.node_id.clone())),
        );
        working.sort();
        working.dedup();
        let generation = view.generation;
        let revision = view.view_revision;
        let mut requests = view
            .nodes()
            .iter()
            .filter(|node| working.contains(&node.node_id))
            .filter_map(|node| {
                let offset = if view.anchor.node_id.as_ref() == Some(&node.node_id) && !view.follow
                {
                    view.anchor.char_offset
                } else {
                    node.base_char_offset
                };
                let outside = preflight.anchor_changed
                    && !view.follow
                    && view.anchor.node_id.as_ref() == Some(&node.node_id)
                    && (offset < node.base_char_offset
                        || (offset >= node.base_char_offset + node.text.chars().count()
                            && offset < node.total_chars));
                let needed = outside
                    || node.presentation.blocks.iter().any(|block| {
                        matches!(
                            block.kind,
                            crate::presentation::BlockKind::Prose
                                | crate::presentation::BlockKind::Reasoning
                        ) && (block
                            .rendered
                            .as_ref()
                            .is_none_or(|rendered| rendered.width != width)
                            || (preflight.initializing
                                && block.source_exact
                                && offset > block.source_start
                                && offset < block.source_end))
                    });
                needed.then(|| (node.node_id.clone(), offset, outside))
            })
            .collect::<Vec<_>>();
        requests.sort_by_key(|(node, _, _)| !working.contains(node));
        self.render_pending.retain(|(t, g, r, _, w, _)| {
            t == &thread && *g == generation && *r == revision && *w == width
        });
        for (node_id, char_offset, missing_source) in requests {
            if self.render_pending.len() >= 3 {
                break;
            }
            if self.render_pending.insert((
                thread.clone(),
                generation,
                revision,
                node_id.clone(),
                width,
                char_offset,
            )) {
                if missing_source {
                    self.active_view_mut().loading = true;
                }
                self.deferred_actions.push(UiAction::LoadNodeWindow {
                    thread_id: thread.clone(),
                    generation,
                    view_revision: revision,
                    node_id,
                    char_offset,
                });
            }
        }
    }
    fn history_layout(&mut self, width: u16, height: u16) -> Arc<HistoryLayout> {
        let budget = usize::try_from(self.config.cache.layout_bytes).unwrap_or(usize::MAX);
        self.views
            .get_mut(&self.active)
            .expect("active view exists")
            .history_layout(&self.active, &mut self.layout_cache, budget, width, height)
    }
    pub fn take_deferred_actions(&mut self) -> Vec<UiAction> {
        self.take_deferred_actions_at(Instant::now())
    }
    fn take_deferred_actions_at(&mut self, now: Instant) -> Vec<UiAction> {
        let generation = self.thread_browser.generation();
        let visible = self.thread_browser.visible();
        self.deferred_actions.retain(|action| {
            !matches!(action, UiAction::LoadThreads { generation: requested, .. }
                if !visible || *requested != generation)
        });
        if let Some(action) = self.thread_browser.take_filter_request(now) {
            self.deferred_actions.push(action);
        }
        std::mem::take(&mut self.deferred_actions)
    }
    pub fn take_terminal_requests(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.terminal_requests)
    }
    pub fn apply(&mut self, update: UiUpdate) {
        let retains = update.needs_retention();
        self.apply_inner(update);
        self.retention_pending |= retains;
        if !self.batching_updates {
            self.finish_retention();
        }
    }
    fn apply_inner(&mut self, update: UiUpdate) {
        if matches!(update, UiUpdate::RecoveryBell) {
            self.bell.request(self.config.ui.bell, Instant::now());
            return;
        }
        self.touch_save_update(&update);
        if let UiUpdate::ScopedManagement { context, update } = update {
            if self.management.accepts(
                &context,
                &self.view().draft.draft_id,
                self.view().thread_id.as_deref(),
            ) {
                self.apply(*update);
            }
            return;
        }
        if let UiUpdate::CommandOutcome { submission, result } = update {
            self.command_outcome(submission, result);
            return;
        }
        if let UiUpdate::Progress {
            thread_id,
            tool_calls,
            cancelled_calls,
            state,
        } = &update
            && let Some(view) = self
                .views
                .values_mut()
                .find(|view| view.thread_id.as_ref() == Some(thread_id))
            && view.tool_calls == *tool_calls
            && view.cancelled_calls == *cancelled_calls
            && view.state == *state
        {
            return;
        }
        if let UiUpdate::ModelReceive {
            thread_id,
            generation,
            attempt_id,
            observed_at,
        } = update
        {
            let view = self.active_view_mut();
            if view.thread_id.as_deref() == Some(&thread_id)
                && view.generation == generation
                && view.activity.receive_at(&attempt_id, observed_at)
            {
                self.dirty = true;
            }
            return;
        }
        if let UiUpdate::ActivityUnavailable {
            thread_id,
            generation,
            epoch,
        } = update
        {
            let view = self.active_view_mut();
            if view.thread_id.as_deref() == Some(&thread_id)
                && view.generation == generation
                && view.activity.unavailable_at(epoch, Instant::now())
            {
                self.dirty = true;
            }
            return;
        }
        if let UiUpdate::Activity {
            thread_id,
            generation,
            epoch,
            snapshot,
            observed_at,
        } = update
        {
            let view = self.active_view_mut();
            if view.thread_id.as_deref() != Some(&thread_id) || view.generation != generation {
                return;
            }
            let previous_phase = view.activity.phase();
            let changed = view.activity.accept_at(epoch, snapshot, observed_at);
            let phase = view.activity.phase();
            if changed || phase != previous_phase {
                let run = view
                    .activity
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.run_id.clone())
                    .unwrap_or_default();
                let turn = view
                    .activity
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.turn_id.clone())
                    .unwrap_or_default();
                if let Some(operations) = &mut self.operations {
                    operations.refresh_activity(phase, run, turn);
                }
                self.dirty = true;
            }
            return;
        }
        let window_reply = match &update {
            UiUpdate::NodeWindow {
                thread_id,
                generation,
                view_revision,
                node,
            } => self
                .views
                .iter()
                .find(|(_, view)| {
                    view.matches_navigation_reply(
                        thread_id,
                        *generation,
                        *view_revision,
                        Some(&node.node_id),
                    )
                })
                .map(|(key, _)| key.clone()),
            UiUpdate::HistoryWindow {
                thread_id,
                generation,
                view_revision,
                ..
            } => self
                .views
                .iter()
                .find(|(_, view)| {
                    view.matches_navigation_reply(thread_id, *generation, *view_revision, None)
                })
                .map(|(key, _)| key.clone()),
            _ => None,
        };
        match update {
            UiUpdate::ThreadDeletion {
                request_id,
                args,
                execute,
                result,
            } => self.apply_thread_deletion(request_id, args, execute, result),
            UiUpdate::RecoveryBell => unreachable!("handled before selected-view updates"),
            UiUpdate::ScopedManagement { .. } => {
                unreachable!("scoped result handled before normal updates")
            }
            UiUpdate::CommandOutcome { .. } => unreachable!("handled before normal UI updates"),
            UiUpdate::Activity { .. }
            | UiUpdate::ActivityUnavailable { .. }
            | UiUpdate::ModelReceive { .. } => {
                unreachable!("activity handled before general updates")
            }
            UiUpdate::ToolFacts {
                thread_id,
                generation,
                revision,
                result,
            } => {
                if let Some(view) = self
                    .view_for(&thread_id, generation)
                    .filter(|view| view.tool_refresh == Some(revision))
                    && let Some(action) = view.on_tool_facts(result)
                {
                    self.deferred_actions.push(action);
                }
            }
            UiUpdate::Models { request_id, result } => {
                if let Some(picker) = &mut self.models {
                    picker.accept(request_id, result);
                }
            }
            UiUpdate::RecalledInput {
                thread_id,
                generation,
                request_id,
                result,
            } => {
                self.apply_recalled_input(thread_id, generation, request_id, result);
            }
            UiUpdate::OriginalPage {
                request_id,
                body,
                display,
            } => {
                if let Some(details) = &mut self.details {
                    details.accept(request_id, body, display);
                }
            }
            UiUpdate::StructuredManagement {
                work_status,
                request_id,
                title,
                fields,
                omitted,
            } => {
                self.management
                    .open_structured(title, request_id, fields, omitted, work_status);
            }
            UiUpdate::ManagementDetail {
                request_id,
                pointer,
                text,
                next_cursor,
            } => {
                self.management
                    .accept_detail(request_id, pointer, text, next_cursor);
            }
            UiUpdate::Initialize { cwd } => {
                if cwd.is_absolute() {
                    self.startup_cwd = cwd.clone();
                    self.initialized = true;
                    if self.view().thread_id.is_none() {
                        self.active_view_mut().cwd = cwd;
                    }
                } else {
                    self.active_view_mut().status = "起動cwdが未解決です".into();
                }
            }
            UiUpdate::ThreadListFailed { generation, error } => {
                self.thread_browser.failed(generation, error)
            }
            UiUpdate::Threads {
                generation,
                requested_cursor,
                threads,
                display_times,
                next_cursor,
            } => {
                let drafts = self.local_drafts(self.thread_browser.filter());
                let active = self.view().thread_id.clone();
                if !self.thread_browser.apply(
                    generation,
                    requested_cursor,
                    threads,
                    display_times,
                    next_cursor,
                    drafts,
                    active,
                ) {
                    return;
                }
            }
            UiUpdate::Thread {
                generation,
                details,
                draft,
            } => {
                let Some(view) = self.view_for(&details.thread_id, generation) else {
                    return;
                };
                view.on_thread(details, draft);
            }
            UiUpdate::History {
                thread_id,
                generation,
                nodes,
                before_cursor,
                append,
                first_ordinal,
                selection_revision,
                total_nodes,
            } => {
                let is_active = self.view().thread_id.as_deref() == Some(&thread_id);
                let Some(view) = self.view_for(&thread_id, generation) else {
                    return;
                };
                if !view.on_history(
                    nodes,
                    before_cursor,
                    append,
                    first_ordinal,
                    selection_revision,
                    total_nodes,
                ) {
                    return;
                }
                if !view.follow && is_active {
                    view.tool_refresh_dirty = true;
                    if let Some(action) = view.request_tool_refresh() {
                        self.deferred_actions.push(action);
                    }
                }
            }
            UiUpdate::StreamCommitted {
                thread_id,
                generation,
                attempt_id,
                node_id,
            } => {
                let Some(view) = self.view_for(&thread_id, generation) else {
                    return;
                };
                if let Some(action) =
                    view.commit_stream_node(attempt_id, node_id, thread_id, generation)
                {
                    self.deferred_actions.push(action);
                }
            }
            UiUpdate::Stream {
                thread_id,
                generation,
                node_id,
                role,
                text,
                base_char_offset,
                total_chars,
                reasoning_ranges,
                presentation,
                ack,
            } => {
                // Ack means dequeued, including a stale generation rejected below.
                let verified = ack.is_some();
                if let Some(ack) = ack {
                    ack.notify_one();
                }
                if verified && !self.view().matches(&thread_id, generation) {
                    return;
                }
                let Some(view) = self.view_for(&thread_id, generation) else {
                    return;
                };
                let retired = if verified {
                    view.settle_other_streams(&node_id)
                } else {
                    Vec::new()
                };
                view.on_stream(UiNode {
                    node_id,
                    role,
                    text,
                    base_char_offset,
                    total_chars,
                    reasoning_ranges,
                    presentation,
                });
                self.deferred_actions.extend(retired);
            }
            UiUpdate::HistoryRebaseNotice {
                thread_id,
                generation,
                view_revision,
                anchor,
                message,
            } => {
                if let Some(view) = self.view_for(&thread_id, generation)
                    && view.view_revision == view_revision
                    && view.anchor == anchor
                    && !view.follow
                    && !view.loading
                {
                    view.status = message;
                }
            }
            UiUpdate::ReceivedList { title, fields } => {
                self.management
                    .open_structured(title, 0, fields, false, false);
                self.management.mode = management::ManagementMode::Received;
            }
            UiUpdate::IncompleteResponses {
                thread_id,
                generation,
                selection_revision,
                view_revision,
                root_revision,
                nodes,
            } => {
                if let Some(view) = self.view_for(&thread_id, generation)
                    && view.selection_revision == selection_revision
                    && view_revision
                        .is_none_or(|revision| view.received_window_revision == Some(revision))
                    && root_revision >= view.received_revision
                {
                    view.received_revision = root_revision;
                    view.on_incomplete_responses(nodes);
                }
            }
            UiUpdate::StreamSettled {
                thread_id,
                generation,
                attempt_id,
                received_source,
                ack,
            } => {
                if let Some(ack) = ack {
                    ack.notify_one();
                }
                if let Some(view) = self.view_for(&thread_id, generation) {
                    if let Some((source, kind)) = received_source {
                        view.retain_incomplete_stream(&attempt_id, source, &kind);
                    } else if let Some(action) = view.settle_stream(&attempt_id) {
                        self.deferred_actions.push(action);
                    }
                }
            }
            UiUpdate::ViewState {
                thread_id,
                generation,
                view_revision,
                anchor,
                follow,
            } => {
                if let Some(view) = self
                    .view_for(&thread_id, generation)
                    .filter(|view| view.view_revision == view_revision)
                {
                    view.on_view_state(anchor, follow);
                }
            }
            UiUpdate::NodeWindow {
                thread_id,
                generation,
                view_revision,
                node,
            } => {
                self.render_pending
                    .retain(|(thread, generation_key, revision, node_id, _, _)| {
                        thread != &thread_id
                            || *generation_key != generation
                            || *revision != view_revision
                            || node_id != &node.node_id
                    });
                if let Some(view) = self
                    .view_for(&thread_id, generation)
                    .filter(|view| view.view_revision == view_revision)
                {
                    view.on_node_window(view_revision, node);
                }
            }
            UiUpdate::DraftStatus { draft_id, text } => {
                if let Some(view) = self
                    .views
                    .values_mut()
                    .find(|view| view.draft.draft_id == draft_id)
                {
                    view.status = text;
                }
            }
            UiUpdate::TerminalRequest { bytes } => {
                self.terminal_requests.push(bytes);
            }
            UiUpdate::StorageFailed { thread_id, message } => {
                if let Some(view) = self
                    .views
                    .values_mut()
                    .find(|v| v.thread_id.as_deref() == Some(&thread_id))
                {
                    view.save_retry = false;
                    view.status = message;
                }
            }
            UiUpdate::DraftSaveFailed { thread_id, message } => {
                self.retry_save(&thread_id, &message)
            }
            UiUpdate::ReceiptSource {
                draft_id,
                revision,
                node_id,
            } => {
                if let Some(view) = self.views.values_mut().find(|view| {
                    view.pending
                        .get(&revision)
                        .is_some_and(|p| p.snapshot.draft_id == draft_id)
                }) {
                    let pending = view.pending.get_mut(&revision).expect("matching receipt");
                    pending.source_node_id = Some(node_id);
                    pending.snapshot.text = String::new();
                    pending.snapshot.attachments = Vec::new();
                    // Fold ranges and cursor are lightweight source offsets.
                    // Keep them so restoring a large paste stays folded.
                    view.observe_sent_nodes();
                }
            }
            UiUpdate::RestoredSnapshot {
                draft_id,
                revision,
                expected_revision,
                text,
                attachments,
            } => {
                if let Some(view) = self.views.values_mut().find(|v| {
                    v.pending
                        .get(&revision)
                        .is_some_and(|p| p.snapshot.draft_id == draft_id)
                }) && view.draft.revision == expected_revision
                    && view.pending.contains_key(&revision)
                {
                    view.on_restored_snapshot(revision, expected_revision, text, attachments);
                }
            }
            UiUpdate::LoadFailed {
                thread_id,
                generation,
                view_revision,
                message,
            } => {
                if let Some(view) = self
                    .view_for(&thread_id, generation)
                    .filter(|v| view_revision.is_none_or(|revision| revision == v.view_revision))
                {
                    view.on_load_failed(message);
                }
            }
            UiUpdate::HistoryInvalidated {
                thread_id,
                generation,
                view_revision,
            } => {
                let Some(view) = self.view_for(&thread_id, generation) else {
                    return;
                };
                let Some(action) =
                    view.on_history_invalidated(thread_id, generation, view_revision)
                else {
                    return;
                };
                self.deferred_actions.push(action);
            }
            UiUpdate::HistoryWindow {
                thread_id,
                generation,
                view_revision,
                nodes,
                first_ordinal,
                selection_revision,
                total_nodes,
                anchor,
            } => {
                if let Some(view) = self.view_for(&thread_id, generation).filter(|v| {
                    v.view_revision == view_revision && selection_revision >= v.selection_revision
                }) {
                    view.received_window_revision = Some(view_revision);
                    view.on_history_window(
                        nodes,
                        first_ordinal,
                        selection_revision,
                        total_nodes,
                        anchor,
                        window_reply.is_some(),
                    );
                }
            }
            UiUpdate::Receipt {
                draft_id,
                revision,
                thread_id,
                status,
                message,
            } => {
                let Some(view) = self.views.values_mut().find(|v| {
                    v.draft.draft_id == draft_id
                        || v.pending.values().any(|p| p.snapshot.draft_id == draft_id)
                }) else {
                    return;
                };
                if !view.on_receipt(draft_id, revision, thread_id, status, message) {
                    return;
                }
            }
            UiUpdate::Status { thread_id, text } => {
                if let Some(thread_id) = thread_id {
                    if let Some(view) = self
                        .views
                        .values_mut()
                        .find(|v| v.thread_id.as_deref() == Some(&thread_id))
                    {
                        view.status = text;
                    }
                } else {
                    self.active_view_mut().status = text;
                }
            }
            UiUpdate::ClipboardAttachment {
                draft_id,
                revision,
                attachment,
            } => {
                let attachment_id = attachment.id.clone();
                let limit = self.config.limits.draft_bytes as usize;
                let accepted =
                    if let Some(view) = self.views.values_mut().find(|view| {
                        view.draft.draft_id == draft_id && view.draft.revision == revision
                    }) {
                        view.on_clipboard_attachment(attachment, limit)
                    } else {
                        false
                    };
                self.deferred_actions.push(UiAction::ClipboardDecision {
                    draft_id,
                    attachment_id,
                    accepted,
                });
            }
            UiUpdate::Progress {
                thread_id,
                tool_calls,
                cancelled_calls,
                state,
            } => {
                if let Some(view) = self
                    .views
                    .values_mut()
                    .find(|view| view.thread_id.as_deref() == Some(&thread_id))
                {
                    view.on_progress(tool_calls, cancelled_calls, state);
                }
            }
            UiUpdate::EditRequestLoaded {
                thread_id,
                node_id,
                generation,
                draft_revision,
                text,
                attachments,
                run_id,
            } => {
                if self.view().thread_id.as_deref() != Some(&thread_id)
                    || self.view().generation != generation
                    || self.view().draft.revision != draft_revision
                {
                    return;
                }
                if text.len() > self.config.limits.draft_bytes as usize {
                    self.active_view_mut().status =
                        "原依頼が下書き上限を超えています。取得内容を切り詰めず編集を中止しました"
                            .into();
                    return;
                }
                let previous = self.view();
                let mut view = ThreadView::new(&self.config, previous.cwd.clone());
                view.connection = previous.connection.clone();
                view.model = previous.model.clone();
                view.effort = previous.effort.clone();
                view.title = "依頼を編集して分岐".into();
                view.draft.cursor = text.len();
                view.draft.text = text;
                view.draft.literal_paste = true;
                view.draft.attachments = attachments;
                view.draft.edit = Some(EditRequestTarget {
                    thread_id,
                    node_id,
                    run_id,
                });
                view.status = "原依頼・元会話は変更しません。編集後Enterで新しい会話へ送信".into();
                let key = view.draft.draft_id.clone();
                self.views.insert(key.clone(), view);
                self.active = key;
                self.deferred_actions.push(UiAction::ActivateDraft {
                    draft_id: self.view().draft.draft_id.clone(),
                });
            }
            UiUpdate::EditConfirmationRequired {
                draft_id,
                revision,
                run_id,
            } => {
                let active = self.view().draft.draft_id == draft_id;
                if let Some(view) = self
                    .views
                    .values_mut()
                    .find(|v| v.draft.draft_id == draft_id && v.draft.revision == revision)
                {
                    if let Some(edit) = &mut view.draft.edit {
                        edit.run_id = Some(run_id.clone());
                    }
                    if let Some(pending) = view.pending.get_mut(&revision) {
                        pending.status = ReceiptStatus::Failed;
                        pending.message = Some("旧作業停止の確認が必要です".into());
                    }
                    if active {
                        self.confirmation = Some(Confirmation::EditStop {
                            draft_id,
                            revision,
                            run_id,
                        });
                    }
                }
            }
            UiUpdate::IndexPreviewReady { args, text } => {
                if self.management.scope.as_ref() == Some(&args) {
                    self.management.preview = Some((args, text.clone()));
                    if let Some(command) = self.management.pending.take() {
                        self.confirmation = Some(Confirmation::Index {
                            command,
                            detail: format!(
                                "{}\n{text}\n外部embedding APIの費用が発生します。Yで実行 / Nで取消",
                                self.management.index_label()
                            ),
                        });
                    }
                }
            }
            UiUpdate::Management { title, text } => {
                self.management.append_text(title, text);
            }
            UiUpdate::ShutdownProgress {
                elapsed_ms,
                detail,
                force_available,
            } => {
                self.lifecycle
                    .accept_progress((elapsed_ms, detail, force_available));
                self.selection_mode = false;
            }
            UiUpdate::ForceShutdown => {
                self.lifecycle.stop(true);
            }
            UiUpdate::Shutdown => self.lifecycle.stop(false),
        }
        if let Some(key) = window_reply {
            let budget = usize::try_from(self.config.cache.layout_bytes).unwrap_or(usize::MAX);
            let view = self.views.get_mut(&key).expect("matched navigation owner");
            let amount = view.finish_navigation_reply();
            if amount != 0 {
                let actions = view.scroll(amount, &key, &mut self.layout_cache, budget);
                self.deferred_actions.extend(actions);
            }
        }
        // Navigation is consumed before the batch/individual retention boundary.
        self.dirty = true;
    }
    fn command_outcome(&mut self, submission: CommandSubmission, result: Result<(), String>) {
        let Some((key, view)) = self
            .views
            .iter_mut()
            .find(|(_, view)| view.draft.draft_id == submission.draft_id)
        else {
            return;
        };
        self.autosave.touch(key);
        let Some(pending) = view.pending.get_mut(&submission.revision) else {
            return;
        };
        let Some(cleared) = &pending.command_cleared else {
            return;
        };
        if pending.status != ReceiptStatus::Accepting {
            return;
        }
        match result {
            Ok(()) => {
                view.pending.remove(&submission.revision);
            }
            Err(error) => {
                pending.status = ReceiptStatus::Failed;
                pending.message = Some(error.clone());
                if &view.draft == cleared {
                    let next = view.draft.revision + 1;
                    view.draft = pending.snapshot.clone();
                    view.draft.revision = next;
                    view.status = format!("操作は失敗しました · 入力を復元しました: {error}");
                } else {
                    view.status = format!(
                        "操作は失敗しました · 現在の入力を保持 · /restore {} で元の操作を復元: {error}",
                        submission.revision
                    );
                }
            }
        }
        self.dirty = true;
    }
    pub fn action_rejected(&mut self, action: &UiAction) {
        if let UiAction::ManagementIntent { action, .. } = action {
            self.invalidate_management();
            self.action_rejected(action);
            return;
        }
        if let UiAction::SubmittedCommand { submission, action } = action {
            self.command_outcome(
                submission.clone(),
                Err("受付キューが満杯または終了済みです。再操作してください".into()),
            );
            if matches!(action.as_ref(), UiAction::ManagementIntent { .. }) {
                self.invalidate_management();
            }
            return;
        }
        if let UiAction::SaveDraft { thread_id, .. } = action {
            self.retry_save(
                thread_id,
                "下書き保存キューが満杯または終了済みです。本文を保持し再試行します",
            );
            return;
        }
        if matches!(action, UiAction::Quit) {
            // The frontend has not accepted shutdown. Keep editing and retry
            // available; a later rejected duplicate cannot undo an accepted one.
            self.lifecycle.reject_unaccepted();
        }
        if let UiAction::RefreshTools {
            thread_id,
            generation,
            revision,
            ..
        } = action
        {
            self.apply(UiUpdate::ToolFacts {
                thread_id: thread_id.clone(),
                generation: *generation,
                revision: *revision,
                result: Err("tool state read queue is full".into()),
            });
            return;
        }
        let rebase = match action {
            UiAction::RebaseHistory {
                thread_id,
                generation,
                view_revision,
                ..
            } => Some((thread_id, generation, view_revision)),
            UiAction::RebaseStream(request) => Some((
                &request.thread_id,
                &request.generation,
                &request.view_revision,
            )),
            _ => None,
        };
        if let Some((thread_id, generation, view_revision)) = rebase {
            self.apply(UiUpdate::LoadFailed {
                thread_id: thread_id.clone(),
                generation: *generation,
                view_revision: Some(*view_revision),
                message: "履歴の再取得キューが満杯です。スクロールで再試行できます".into(),
            });
            return;
        }
        if let UiAction::ThreadDeletion {
            request_id,
            args,
            execute,
            ..
        } = action
        {
            self.apply_thread_deletion(
                *request_id,
                args.clone(),
                *execute,
                Err(crate::protocol::ApiError::new(
                    crate::protocol::ErrorCode::InvalidState,
                    "処理キューが満杯です。削除は未受付です。もう一度開いてください",
                )),
            );
            return;
        }
        if let UiAction::LoadModels { request_id, .. } = action {
            self.apply(UiUpdate::Models {
                request_id: *request_id,
                result: Err("取得キューが満杯です。Ctrl+Rで再取得してください".into()),
            });
            return;
        }
        if let UiAction::RecallInput {
            thread_id,
            generation,
            request_id,
            ..
        } = action
        {
            self.apply(UiUpdate::RecalledInput {
                thread_id: thread_id.clone(),
                generation: *generation,
                request_id: *request_id,
                result: Err("処理キューが満杯です。もう一度上下キーで操作してください".into()),
            });
            return;
        }
        if let UiAction::Send {
            draft_id, revision, ..
        }
        | UiAction::SendEdited {
            draft_id, revision, ..
        } = action
        {
            self.apply(UiUpdate::Receipt {
                draft_id: draft_id.clone(),
                revision: *revision,
                thread_id: None,
                status: ReceiptStatus::Failed,
                message: Some("受付キューが満杯です。本文を保持しました。再送してください".into()),
            });
        } else {
            self.active_view_mut().status =
                "操作は未受付です。処理キューが空いてから再操作してください".into();
            self.dirty = true;
        }
    }
    fn begin_edit(&mut self) {
        let view = self.active_view_mut();
        if view.composer.edited() {
            view.status = "入力できます".into();
        }
        if view.pending.get(&view.draft.revision).is_some_and(|p| {
            matches!(
                p.status,
                ReceiptStatus::Accepted | ReceiptStatus::Preparing | ReceiptStatus::Held
            )
        }) {
            view.draft.text.clear();
            view.draft.cursor = 0;
            view.draft.attachments.clear();
            view.draft.folds.clear();
            view.draft.literal_paste = false;
            view.draft.edit = None;
        }
        view.draft.revision += 1;
    }
    pub fn insert(&mut self, text: &str, pasted: bool) {
        let text = if pasted {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.to_owned()
        };
        let existing = self.view().draft.text.len();
        if existing.saturating_add(text.len()) > self.config.limits.draft_bytes as usize {
            self.active_view_mut().status =
                "下書きのbyte上限を超えます。貼付けを追加していません".into();
            self.dirty = true;
            return;
        }
        self.begin_edit();
        let fold = pasted
            && (text.chars().count() > self.config.ui.paste_fold_chars
                || text.lines().count() > self.config.ui.paste_fold_lines);
        let fold_id = self.next_fold;
        if fold {
            self.next_fold += 1;
        }
        let draft = &mut self.active_view_mut().draft;
        draft.insert_text(&text, pasted, fold.then_some(fold_id));
        self.dirty = true;
    }
    fn clear_current_draft(&mut self) {
        self.begin_edit();
        let draft = &mut self.active_view_mut().draft;
        draft.text.clear();
        draft.cursor = 0;
        draft.attachments.clear();
        draft.folds.clear();
        draft.literal_paste = false;
        draft.edit = None;
        let action = UiAction::DraftAttachments {
            draft_id: draft.draft_id.clone(),
            revision: draft.revision,
            attachment_ids: vec![],
        };
        self.deferred_actions.push(action);
        self.dirty = true;
    }
    fn delete(&mut self, backwards: bool) {
        self.begin_edit();
        let draft = &mut self.active_view_mut().draft;
        draft.delete_at_cursor(backwards);
        let action = UiAction::DraftAttachments {
            draft_id: draft.draft_id.clone(),
            revision: draft.revision,
            attachment_ids: draft.attachments.iter().map(|a| a.id.clone()).collect(),
        };
        self.deferred_actions.push(action);
        self.dirty = true;
    }
    fn persist_active(&self) -> Vec<UiAction> {
        let view = self.view();
        if !autosave::saveable(view) {
            return vec![];
        }
        view.thread_id
            .as_ref()
            .map(|thread_id| {
                vec![UiAction::SaveDraft {
                    thread_id: thread_id.clone(),
                    draft: view.draft.clone(),
                    anchor: view.anchor.clone(),
                    follow: view.follow,
                }]
            })
            .unwrap_or_default()
    }
    fn local_drafts(&self, filter: &str) -> Vec<ThreadSummary> {
        self.views
            .iter()
            .filter(|(_, view)| {
                view.thread_id.is_none()
                    && (!view.draft.text.is_empty()
                        || !view.draft.attachments.is_empty()
                        || !view.pending.is_empty())
            })
            .map(|(key, view)| ThreadSummary {
                thread_id: format!("draft:{key}"),
                title: "未送信の下書き".into(),
                cwd: view.cwd.to_string_lossy().into(),
                parent_id: None,
                state: "未保存".into(),
                updated_at: String::new(),
                excerpt: view.draft.text.chars().take(80).collect(),
            })
            .filter(|thread| {
                filter.is_empty()
                    || thread
                        .excerpt
                        .to_lowercase()
                        .contains(&filter.to_lowercase())
                    || thread.cwd.to_lowercase().contains(&filter.to_lowercase())
            })
            .collect()
    }
    pub fn select_thread(&mut self, thread_id: String) -> Vec<UiAction> {
        self.operations = None;
        self.invalidate_management();
        self.models = None;
        self.cancel_input_recall();
        self.active_view_mut().cancel_navigation();
        if let Some(key) = thread_id
            .strip_prefix("draft:")
            .filter(|key| self.views.contains_key(*key))
        {
            let key = key.to_owned();
            if let Some(saved) = self.views[&key].thread_id.clone() {
                return self.select_thread(saved);
            }
            let mut actions = self.persist_active();
            self.active = key;
            actions.push(UiAction::ActivateDraft {
                draft_id: self.view().draft.draft_id.clone(),
            });
            self.thread_browser.close();
            self.trim_bodies();
            self.dirty = true;
            return actions;
        }
        let mut actions = self.persist_active();
        self.generation += 1;
        let remembered = self
            .views
            .values()
            .find(|v| v.thread_id.as_deref() == Some(&thread_id))
            .map(|v| (v.anchor.clone(), v.follow));
        let key = if let Some((key, _)) = self
            .views
            .iter()
            .find(|(_, v)| v.thread_id.as_deref() == Some(&thread_id))
        {
            key.clone()
        } else {
            let mut view = ThreadView::new(&self.config, self.startup_cwd.clone());
            view.thread_id = Some(thread_id.clone());
            view.draft_restore_pending = true;
            let key = view.draft.draft_id.clone();
            self.views.insert(key.clone(), view);
            key
        };
        self.active = key;
        self.thread_browser.close();
        let generation = self.generation;
        let view = self.active_view_mut();
        view.generation = generation;
        view.activity = Default::default();
        view.scroll.request = None;
        view.scroll.pending = 0;
        view.loading = true;
        actions.push(UiAction::SelectThread {
            thread_id: thread_id.clone(),
            generation,
            view_revision: view.view_revision,
            known_view: remembered,
            preview_sources: view.provisional_sources(),
        });
        self.trim_bodies();
        self.dirty = true;
        actions
    }
    pub fn new_chat(&mut self) -> Vec<UiAction> {
        self.operations = None;
        self.invalidate_management();
        self.models = None;
        self.cancel_input_recall();
        self.active_view_mut().cancel_navigation();
        let mut actions = self.persist_active();
        let view = ThreadView::new(&self.config, self.startup_cwd.clone());
        let key = view.draft.draft_id.clone();
        self.views.insert(key.clone(), view);
        self.active = key;
        actions.push(UiAction::ActivateDraft {
            draft_id: self.view().draft.draft_id.clone(),
        });
        self.thread_browser.close();
        self.trim_bodies();
        self.dirty = true;
        actions
    }
    /// Animation deadlines only invalidate the visible footer; no worker action
    /// or history revision is produced by the display clock.
    pub fn animation_tick(&mut self, now: Instant) {
        // Commit the existing UI clock before drawing: a delayed observation
        // cannot move an already displayed second back into the past.
        // Display clocks never change a persisted draft or source anchor.
        self.views
            .get_mut(&self.active)
            .expect("active view exists")
            .activity
            .advance_clock(now);
        let visible = self.composer_active();
        let phase = self.view().footer_phase();
        if self
            .animation
            .tick(now, phase, visible, self.config.ui.max_fps)
        {
            self.dirty = true;
        }
    }
    fn submit(&mut self) -> Vec<UiAction> {
        if !self.initialized {
            self.active_view_mut().status = "起動情報の読込み中です".into();
            return vec![];
        }
        self.active_view_mut().composer.stop_recall();
        let text = match draft::decode(
            &self.view().draft.text,
            self.view().draft.attachments.clone(),
        ) {
            Ok((content, _)) => content.text,
            Err(error) => {
                self.active_view_mut().status = error.to_string();
                return vec![];
            }
        };
        if text.starts_with('/') && !self.view().draft.literal_paste {
            let source = self.active.clone();
            let original = self.view().draft.clone();
            {
                let draft = &mut self.active_view_mut().draft;
                draft.clear_text();
                draft.literal_paste = false;
                draft.revision += 1;
            }
            let cleared = self.view().draft.clone();
            self.command_rejected = false;
            let mut actions = self.slash(&text);
            if self.command_rejected {
                if let Some(view) = self.views.get_mut(&source) {
                    view.draft = original;
                }
            } else if actions.len() == 1
                && matches!(
                    actions[0],
                    UiAction::Command(_)
                        | UiAction::Manage(_)
                        | UiAction::ReloadAttachments { .. }
                        | UiAction::Background { .. }
                        | UiAction::LoadEditRequest { .. }
                        | UiAction::Copy { .. }
                        | UiAction::RestoreSnapshot { .. }
                        | UiAction::PasteImage { .. }
                )
            {
                let submission = CommandSubmission {
                    draft_id: original.draft_id.clone(),
                    revision: original.revision,
                };
                if let Some(view) = self.views.get_mut(&source) {
                    view.pending.insert(
                        original.revision,
                        PendingSend {
                            snapshot: original,
                            command_cleared: Some(cleared),
                            status: ReceiptStatus::Accepting,
                            message: None,
                            source_node_id: None,
                            send: None,
                        },
                    );
                }
                let action = actions.pop().unwrap();
                actions.push(UiAction::SubmittedCommand {
                    submission,
                    action: Box::new(action),
                });
            } else if actions
                .iter()
                .any(|action| matches!(action, UiAction::Cancel { .. } | UiAction::CancelWait))
            {
                // A coalesced human control has no per-invocation receipt. Keep
                // its input rather than coupling priority admission to drafts.
                if let Some(view) = self.views.get_mut(&source) {
                    view.draft = original;
                }
            }
            return actions;
        }
        if let Some(edit) = &self.view().draft.edit
            && let Some(run_id) = &edit.run_id
        {
            self.confirmation = Some(Confirmation::EditStop {
                draft_id: self.view().draft.draft_id.clone(),
                revision: self.view().draft.revision,
                run_id: run_id.clone(),
            });
            return vec![];
        }
        self.send_current(None)
    }
    fn send_current(&mut self, confirmed_run_id: Option<String>) -> Vec<UiAction> {
        self.dirty = true;
        let text = self.view().draft.text.clone();
        let view = self.active_view_mut();
        if text.trim().is_empty() && view.draft.attachments.is_empty() {
            return vec![];
        }
        if view
            .pending
            .get(&view.draft.revision)
            .is_some_and(|p| p.status == ReceiptStatus::Accepting)
        {
            view.status = "受付処理中です".into();
            return vec![];
        }
        if view
            .pending
            .get(&view.draft.revision)
            .is_some_and(|p| matches!(p.status, ReceiptStatus::Failed | ReceiptStatus::Cancelled))
        {
            view.draft.revision += 1;
        }
        let draft = view.draft.clone();
        view.pending
            .retain(|_, pending| pending.status != ReceiptStatus::Completed);
        view.pending.insert(
            draft.revision,
            PendingSend {
                command_cleared: None,
                snapshot: draft.clone(),
                status: ReceiptStatus::Accepting,
                message: None,
                source_node_id: None,
                send: Some(SendProgress {
                    started_at: Instant::now(),
                    reflected: false,
                }),
            },
        );
        view.status = "送信要求を受け付け中".into();
        view.cancel_navigation();
        view.follow = true;
        view.unseen = false;
        if let Some(edit) = draft.edit {
            return vec![UiAction::SendEdited {
                draft_id: draft.draft_id,
                revision: draft.revision,
                source_thread_id: edit.thread_id,
                source_node_id: edit.node_id,
                cwd: view.cwd.clone(),
                text,
                attachments: draft.attachments,
                connection: view.connection.clone(),
                model: view.model.clone(),
                effort: view.effort.clone(),
                confirmed_run_id,
            }];
        }
        vec![UiAction::Send {
            draft_id: draft.draft_id,
            revision: draft.revision,
            thread_id: view.thread_id.clone(),
            cwd: view.cwd.clone(),
            text,
            attachments: draft.attachments,
            connection: view.connection.clone(),
            model: view.model.clone(),
            effort: view.effort.clone(),
        }]
    }
    fn slash_path(&self, argument: &str) -> Result<(PathBuf, String), &'static str> {
        let path = crate::input_path::relative_to(Path::new(unquote(argument)), &self.view().cwd);
        let wire = crate::input_path::utf8(&path)
            .map_err(|_| "パスはUTF-8で表現できる必要があります。操作は送信していません")?
            .to_owned();
        Ok((path, wire))
    }
    fn slash(&mut self, text: &str) -> Vec<UiAction> {
        let mut words = text.splitn(2, char::is_whitespace);
        let command = words.next().unwrap_or("");
        let argument = words.next().unwrap_or("").trim();
        self.active_view_mut().status = "操作を準備しました".into();
        let thread_id = self.view().thread_id.clone();
        match command {
            "/actions" | "/operations" => {
                self.open_operations(None);
                vec![]
            }
            "/details" => {
                self.details = Some(details::Details::new(self.view(), false));
                vec![]
            }
            "/links" => {
                self.details = Some(details::Details::new(self.view(), true));
                vec![]
            }
            "/received" => {
                let Some(thread_id) = thread_id else {
                    return self.command_error("保存済みの会話を選択してください");
                };
                let page = if argument.is_empty() {
                    1
                } else {
                    match argument.parse::<u32>() {
                        Ok(page) if page > 0 => page,
                        _ => {
                            return self
                                .command_error("/received または /received 1以上のページ番号");
                        }
                    }
                };
                self.invalidate_management();
                self.management.open_text(ManagementView {
                    title: "受信記録 · 読込み中".into(),
                    text: String::new(),
                    scroll: 0,
                });
                vec![UiAction::LoadReceived { thread_id, page }]
            }
            "/retry" => {
                let Some(snapshot) = &self.view().activity.snapshot else {
                    return self
                        .command_error("現在の復旧対象を読込み中です。F8で状態を確認してください");
                };
                if snapshot.phase != activity::ActivityPhase::Recovery {
                    return self.command_error(
                        "現在の会話は復旧待ちではありません。F8で状態を確認してください",
                    );
                }
                let Some(run_id) = snapshot.run_id.clone() else {
                    return self.command_error("復旧対象が未取得です。F8で状態を確認してください");
                };
                vec![UiAction::Command(Command::RunRetry(
                    crate::protocol::RunTarget { run_id },
                ))]
            }
            "/stop" => thread_id
                .map(|thread_id| vec![UiAction::Cancel { thread_id }])
                .unwrap_or_default(),
            "/cancel-wait" => vec![UiAction::CancelWait],
            "/status" => {
                let (thread_id, all) = match argument {
                    "" => (None, false),
                    "all" => (None, true),
                    "thread" => match thread_id {
                        Some(thread_id) => (Some(thread_id), false),
                        None => {
                            return self.command_error(
                                "現在の会話は未保存です。全体は /status で確認できます",
                            );
                        }
                    },
                    "global" => (None, false),
                    _ => {
                        return self
                            .command_error("/status [all|thread]。条件の指定はF8の作業状態から");
                    }
                };
                self.execute_operation(Command::Status(StatusArgs {
                    thread_id,
                    all,
                    ..Default::default()
                }))
            }
            "/help" => self.show_help(help::Page::guide(&self.config)),
            "/new" => self.new_chat(),
            "/threads" => {
                let drafts = self.local_drafts(argument);
                let preferred = self.view().thread_id.clone();
                vec![self.thread_browser.open(argument.into(), drafts, preferred)]
            }
            "/switch" if !argument.is_empty() => self.select_thread(argument.into()),
            "/compact" => vec![UiAction::Command(Command::Compact(ThreadTarget {
                thread_id,
            }))],
            "/edit" => {
                let node_id = if argument.is_empty() {
                    self.view().anchor.node_id.clone().or_else(|| {
                        self.view()
                            .nodes()
                            .iter()
                            .rev()
                            .find(|node| node.role == "user")
                            .map(|node| node.node_id.clone())
                    })
                } else {
                    Some(argument.into())
                };
                if let (Some(thread_id), Some(node_id)) = (thread_id, node_id) {
                    vec![UiAction::LoadEditRequest {
                        thread_id,
                        node_id,
                        generation: self.view().generation,
                        draft_revision: self.view().draft.revision,
                    }]
                } else {
                    self.command_error("/edit [user-node-id] は保存済み依頼で使えます")
                }
            }
            "/index" => self.index_command(argument),
            "/search" if !argument.is_empty() => {
                self.management.scope = None;
                vec![UiAction::Manage(Command::HistorySearch(
                    HistorySearchArgs {
                        query: argument.into(),
                        limit: 20,
                        ..Default::default()
                    },
                ))]
            }
            "/fork" => {
                let node_id = if argument.is_empty() {
                    self.view()
                        .anchor
                        .node_id
                        .clone()
                        .or_else(|| self.view().nodes().last().map(|n| n.node_id.clone()))
                } else {
                    Some(argument.into())
                };
                if let (Some(thread_id), Some(node_id)) = (thread_id, node_id) {
                    vec![UiAction::Command(Command::ThreadFork(ThreadForkArgs {
                        thread_id,
                        node_id,
                        cwd: None,
                    }))]
                } else {
                    self.command_error("/fork [node-id] は保存済み履歴で使えます")
                }
            }
            "/models" => self.open_models(argument),
            "/model" if argument.is_empty() => {
                let view = self.view();
                self.show_help(help::Page::info(
                    "モデル設定",
                    "現在の会話の設定",
                    format!(
                        "接続: {}\nモデル: {}\n推論強度: {}\n\n/models  一覧からモデルを選ぶ\n/model 接続名 モデル名  名前を指定して変更\n/effort 値  推論強度を変更\n/effort none  推論強度の指定を解除",
                        view.connection.as_deref().unwrap_or("未設定"),
                        view.model.as_deref().unwrap_or("未設定"),
                        view.effort
                            .as_deref()
                            .filter(|value| !value.is_empty())
                            .unwrap_or("未指定")
                    ),
                ))
            }
            "/model" => {
                let parts: Vec<_> = argument.split_whitespace().collect();
                if parts.len() != 2 || !self.config.connections.contains_key(parts[0]) {
                    return self.command_error("/model <connection> <model> を指定してください");
                }
                self.change_model(parts[0].into(), parts[1].into())
            }
            "/effort" if !argument.is_empty() => {
                let effort = if argument == "none" {
                    String::new()
                } else {
                    argument.to_owned()
                };
                if thread_id.is_none() {
                    self.active_view_mut().effort = Some(effort);
                    vec![]
                } else {
                    vec![UiAction::Command(Command::ThreadSet(ThreadSetArgs {
                        thread_id,
                        effort: Some(effort),
                        ..Default::default()
                    }))]
                }
            }
            "/cwd" if argument.is_empty() => self.show_help(help::Page::info(
                "作業フォルダー",
                "現在の会話でコマンドを実行する場所",
                format!("{}\n\n変更するには /cwd パス", self.view().cwd.display()),
            )),
            "/cwd" if !argument.is_empty() => {
                let (path, wire_path) = match self.slash_path(argument) {
                    Ok(path) => path,
                    Err(error) => return self.command_error(error),
                };
                if thread_id.is_none() {
                    self.active_view_mut().cwd = path;
                    vec![]
                } else {
                    vec![UiAction::Command(Command::ThreadSet(ThreadSetArgs {
                        thread_id,
                        cwd: Some(wire_path),
                        ..Default::default()
                    }))]
                }
            }
            "/attach" if !argument.is_empty() => {
                let (path, _) = match self.slash_path(argument) {
                    Ok(path) => path,
                    Err(error) => return self.command_error(error),
                };
                self.begin_edit();
                let position = self.view().draft.cursor;
                self.active_view_mut().draft.insert_attachment(
                    UiAttachment {
                        id: uuid::Uuid::new_v4().to_string(),
                        path,
                        kind: "file".into(),
                        state: "未準備".into(),
                        position: None,
                    },
                    position,
                );
                vec![]
            }
            "/attachments" => {
                let mut text = self
                    .view()
                    .draft
                    .attachments
                    .iter()
                    .map(|a| {
                        format!(
                            "下書き: {} · {} · {}\n{}",
                            a.id,
                            a.kind,
                            a.state,
                            a.path.display()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if text.is_empty() {
                    text = "下書きの添付はありません".into();
                }
                text.push_str("\n\n保存済みselectionはselected/managed/unavailable_reasonで表示します。/detach IDは送信選択だけを解除し、外部ファイルを削除しません。");
                self.management.mode = management::ManagementMode::Attachments;
                self.management.scope = None;
                self.management.open_text(ManagementView {
                    title: "添付管理".into(),
                    text,
                    scroll: 0,
                });
                thread_id
                    .map(|thread_id| {
                        vec![UiAction::Manage(Command::AttachmentList(ThreadTarget {
                            thread_id: Some(thread_id),
                        }))]
                    })
                    .unwrap_or_default()
            }
            "/reload-attachments" => thread_id
                .map(|thread_id| vec![UiAction::ReloadAttachments { thread_id }])
                .unwrap_or_else(|| {
                    self.command_error("保存済み会話で /reload-attachments を実行してください")
                }),
            "/detach" if !argument.is_empty() => {
                if self
                    .view()
                    .draft
                    .attachments
                    .iter()
                    .any(|a| a.id == argument)
                {
                    self.begin_edit();
                    self.active_view_mut().draft.remove_attachment(argument);
                    vec![UiAction::DraftAttachments {
                        draft_id: self.view().draft.draft_id.clone(),
                        revision: self.view().draft.revision,
                        attachment_ids: self
                            .view()
                            .draft
                            .attachments
                            .iter()
                            .map(|a| a.id.clone())
                            .collect(),
                    }]
                } else if let Some(thread_id) = thread_id {
                    vec![UiAction::Command(Command::AttachmentRemove(
                        AttachmentRemoveArgs {
                            thread_id: Some(thread_id),
                            attachment_id: argument.into(),
                        },
                    ))]
                } else {
                    self.command_error("指定IDは下書きに存在しません")
                }
            }
            "/reasoning" => self.toggle_reasoning(argument),
            "/image" | "/paste-image" => {
                let view = self.view();
                vec![UiAction::PasteImage {
                    draft_id: view.draft.draft_id.clone(),
                    revision: view.draft.revision,
                    thread_id: view
                        .thread_id
                        .clone()
                        .or_else(|| view.draft.edit.as_ref().map(|edit| edit.thread_id.clone())),
                    position: view.draft.cursor,
                }]
            }
            "/copy" => {
                let mut parts = argument.split_whitespace();
                let node_id = parts
                    .next()
                    .map(str::to_owned)
                    .or_else(|| self.view().anchor.node_id.clone())
                    .or_else(|| self.view().nodes().last().map(|n| n.node_id.clone()));
                let code_block = parts.next().and_then(|s| s.parse().ok());
                node_id
                    .map(|node_id| {
                        vec![UiAction::Copy {
                            node_id,
                            code_block,
                        }]
                    })
                    .unwrap_or_else(|| self.command_error("コピー対象node-idがありません"))
            }
            "/select" => {
                self.selection_mode = !self.selection_mode;
                vec![]
            }
            "/paste" => self.paste_command(argument),
            "/restore" if argument.is_empty() => {
                let Some(thread_id) = thread_id else {
                    return self.command_error("入力範囲の復元は保存済み会話で /restore を実行してください。下書きの復元は /restore 番号です");
                };
                vec![UiAction::Command(Command::ContextEdit(ContextEditArgs {
                    thread_id: Some(thread_id),
                    action: ContextEditAction::Restore,
                    from_node_id: None,
                    to_node_id: None,
                }))]
            }
            "/restore" => {
                let Some(revision) = argument.parse::<u64>().ok() else {
                    return self.command_error("引数なしの /restore は入力範囲の復元、/restore 番号は失敗した下書きの復元です");
                };
                if let Some(pending) = self.view().pending.get(&revision)
                    && let Some(node_id) = &pending.source_node_id
                {
                    return vec![UiAction::RestoreSnapshot {
                        draft_id: pending.snapshot.draft_id.clone(),
                        revision,
                        node_id: node_id.clone(),
                        expected_revision: self.view().draft.revision,
                    }];
                }
                let snapshot = self
                    .view()
                    .pending
                    .get(&revision)
                    .map(|p| p.snapshot.clone());
                if let Some(snapshot) = snapshot {
                    let next = self.view().draft.revision + 1;
                    self.active_view_mut().draft = snapshot;
                    self.active_view_mut().draft.revision = next;
                    vec![]
                } else {
                    self.command_error("保存された失敗snapshotがありません")
                }
            }
            "/cancel" => thread_id
                .map(|thread_id| vec![UiAction::Cancel { thread_id }])
                .unwrap_or_default(),
            "/background" => thread_id
                .map(|thread_id| vec![UiAction::Background { thread_id }])
                .unwrap_or_default(),
            "/quit" => {
                self.lifecycle.begin();
                self.selection_mode = false;
                let mut actions = self.persist_active();
                actions.push(UiAction::Quit);
                actions
            }
            _ => self.command_error("未知のコマンドです。/help を参照してください"),
        }
    }
    fn paste_command(&mut self, argument: &str) -> Vec<UiAction> {
        let mut parts = argument.split_whitespace();
        let Some(id) = parts.next().and_then(|s| s.parse::<u64>().ok()) else {
            return self.command_error("/paste <id> show|edit|remove を指定してください");
        };
        let mode = parts.next().unwrap_or("show");
        let Some(fold) = self.view().draft.folds.iter().find(|f| f.id == id).cloned() else {
            return self.command_error("貼付けブロックがありません");
        };
        if mode == "remove" {
            let was_pending = self
                .view()
                .pending
                .get(&self.view().draft.revision)
                .is_some_and(|p| p.status == ReceiptStatus::Accepting);
            self.begin_edit();
            if was_pending {
                return vec![];
            }
            let draft = &mut self.active_view_mut().draft;
            draft.adjust_attachments(fold.start, fold.end, 0);
            draft.text.replace_range(fold.start..fold.end, "");
            adjust_folds(&mut draft.folds, fold.start, fold.end, 0);
            draft.cursor = fold.start;
        } else if matches!(mode, "show" | "edit") {
            let draft = &mut self.active_view_mut().draft;
            if let Some(f) = draft.folds.iter_mut().find(|f| f.id == id) {
                f.expanded = true;
            }
            draft.cursor = fold.start;
        } else {
            return self.command_error("show|edit|remove を指定してください");
        }
        vec![]
    }
    fn toggle_reasoning(&mut self, argument: &str) -> Vec<UiAction> {
        let node_id = if argument.is_empty() {
            self.view().anchor.node_id.clone().or_else(|| {
                self.view()
                    .nodes()
                    .iter()
                    .rev()
                    .find(|n| !n.reasoning_ranges.is_empty())
                    .map(|n| n.node_id.clone())
            })
        } else {
            Some(argument.into())
        };
        let Some(node_id) = node_id else {
            return self.command_error("推論テキストを含むnodeがありません");
        };
        let view = self.active_view_mut();
        let Some(node) = view
            .nodes()
            .iter()
            .find(|node| node.node_id == node_id && !node.reasoning_ranges.is_empty())
        else {
            return self.command_error("指定nodeに表示可能な推論テキストがありません");
        };
        let offset = node.reasoning_ranges[0].0;
        let source_start = node.base_char_offset;
        let source_end = source_start + node.text.chars().count();
        if view.reasoning_expanded.remove(&node_id) {
            return vec![];
        }
        view.reasoning_expanded.insert(node_id.clone());
        view.anchor = ViewAnchor {
            node_id: Some(node_id.clone()),
            char_offset: offset,
        };
        view.follow = false;
        view.view_revision += 1;
        if (offset < source_start || offset >= source_end)
            && let Some(thread_id) = view.thread_id.clone()
        {
            return vec![UiAction::LoadNodeWindow {
                thread_id,
                generation: view.generation,
                node_id,
                char_offset: offset,
                view_revision: view.view_revision,
            }];
        }
        vec![]
    }
    fn index_command(&mut self, argument: &str) -> Vec<UiAction> {
        let words: Vec<_> = argument.split_whitespace().collect();
        let (space, parts) = if words.first() == Some(&"space") {
            let Some(id) = words.get(1) else {
                return self.command_error(
                    "/index space SPACE-ID [all|thread|threads ID...] を指定してください",
                );
            };
            (Some((*id).to_owned()), &words[2..])
        } else {
            (None, words.as_slice())
        };
        let mut selection = match parts {
            [] | ["status"] => None,
            ["all"] => Some(IndexArgs {
                scope: Some(IndexScope::All),
                ..Default::default()
            }),
            ["threads", ids @ ..] if !ids.is_empty() => Some(IndexArgs {
                scope: Some(IndexScope::Threads),
                thread_ids: ids.iter().map(|id| (*id).into()).collect(),
                ..Default::default()
            }),
            ["thread"] => self.view().thread_id.clone().map(|id| IndexArgs {
                scope: Some(IndexScope::Threads),
                thread_ids: vec![id],
                ..Default::default()
            }),
            _ => {
                return self.command_error(
                    "/index [status|all|thread|threads THREAD-ID ...] を指定してください",
                );
            }
        };
        if parts == ["thread"] && selection.is_none() {
            return self.command_error("未保存の下書きには索引対象会話がありません");
        }
        if let Some(space) = space {
            let args = selection.get_or_insert_default();
            args.space_id = Some(space);
        }
        self.management.mode = management::ManagementMode::Index;
        self.management.scope = selection.clone();
        self.management.preview = None;
        self.management.open_text(ManagementView{title:"Index管理".into(),text:"pending / running / done / failed / pausedを取得中。B backfill / T retry / R rebuildは明示対象と確認が必要です。\n/search QUERYで検索結果を表示。\n外部embedding APIの利用で費用が発生します。金額は未見積です。".into(),scroll:0});
        let args = selection.clone().unwrap_or_default();
        let mut actions = vec![UiAction::Manage(Command::IndexStatus(args))];
        if let Some(selection) = selection.filter(management::explicit_index_targets) {
            actions.push(UiAction::IndexPreview(selection));
        }
        actions
    }
    fn operation_context(&self, references: bool) -> operations::Context {
        let view = self.view();
        let node_id = self
            .details
            .as_ref()
            .map(|details| details.node_id.clone())
            .or_else(|| view.anchor.node_id.clone())
            .or_else(|| view.nodes().last().map(|node| node.node_id.clone()))
            .unwrap_or_default();
        // An observed empty snapshot is authoritative; do not revive an older
        // ThreadDetails target just because the current projection has no Run.
        let current_run = view
            .activity
            .snapshot
            .as_ref()
            .map_or_else(|| view.run_id.clone(), |snapshot| snapshot.run_id.clone())
            .unwrap_or_default();
        let current_turn = view
            .activity
            .snapshot
            .as_ref()
            .map_or_else(|| view.turn_id.clone(), |snapshot| snapshot.turn_id.clone())
            .unwrap_or_default();
        let mut context = operations::Context {
            phase: view.activity.phase(),
            follows_current: !references,
            thread: view.thread_id.clone().unwrap_or_default(),
            node: node_id.clone(),
            run: current_run.clone(),
            current_run,
            turn: current_turn,
            cwd: view.cwd.clone(),
            ..Default::default()
        };
        if !references {
            return context;
        }
        let mut fields = view
            .nodes()
            .iter()
            .find(|node| node.node_id == node_id)
            .map(|node| node.presentation.fields.clone())
            .unwrap_or_default();
        if let Some(field) = self
            .details
            .as_ref()
            .and_then(|details| details.selected_field(view))
        {
            fields.push(field);
        }
        if let Some(management) = self.management.structured()
            && let Some(field) = management.fields.get(management.selected)
        {
            fields.push(field.clone());
        }
        for field in fields {
            if let Some(target) = operations::TargetKind::from_field_key(&field.key) {
                context.set_reference(target, field.value);
            }
        }
        context
    }
    fn invalidate_management(&mut self) {
        self.management.close();
        if matches!(
            self.confirmation,
            Some(Confirmation::Index { .. } | Confirmation::Operation { .. })
        ) {
            self.confirmation = None;
        }
    }
    fn open_operations(&mut self, selected: Option<operations::OpKind>) {
        let context = self.operation_context(selected.is_some());
        self.invalidate_management();
        let mut operations = operations::Operations::new(context);
        if let Some(selected) = selected {
            operations.select(selected);
        }
        self.operations = Some(operations);
    }
    fn detail_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        let mut details = self.details.take().unwrap();
        let mut next = self.next_detail_request;
        let outcome = details.key(key, self.view(), &mut next);
        self.next_detail_request = next;
        if !matches!(outcome, details::Outcome::Close) {
            self.details = Some(details);
        }
        match outcome {
            details::Outcome::Action(action) => vec![*action],
            details::Outcome::Operation(index) => {
                self.open_operations(Some(index));
                vec![]
            }
            _ => vec![],
        }
    }
    fn execute_operation(&mut self, command: Command) -> Vec<UiAction> {
        self.details = None;
        match &command {
            Command::IndexBackfill(args)
            | Command::IndexRetry(args)
            | Command::IndexRebuild(args) => {
                if !management::explicit_index_targets(args) {
                    return self.command_error("索引の実行対象を明示してください");
                }
                self.management.scope = Some(args.clone());
                self.management.preview = None;
                self.management.pending = Some(command.clone());
                self.management.mode = management::ManagementMode::Index;
                self.management.open_text(ManagementView {
                    title: "索引 · 費用preview".into(),
                    text: "対象件数を確認しています。まだ実行していません".into(),
                    scroll: 0,
                });
                return vec![
                    UiAction::Manage(Command::IndexStatus(args.clone())),
                    UiAction::IndexPreview(args.clone()),
                ];
            }
            Command::ThreadDelete(args) => {
                self.confirmation = Some(Confirmation::Operation {
                    command: command.clone(),
                    detail: format!(
                        "会話 {} を削除します。子孫を含む: {}。現在の実行状態を再確認してから削除します",
                        args.thread_id, args.descendants
                    ),
                });
                return vec![];
            }
            Command::StorageCollect(args) => {
                self.confirmation = Some(Confirmation::Operation {
                    command: command.clone(),
                    detail: format!(
                        "保存データの回収を要求します。category={:?}。保護されたデータを含む回収結果を確認してください",
                        args.category
                    ),
                });
                return vec![];
            }
            _ => {}
        }
        self.management.mode = if matches!(command, Command::Status(_)) {
            management::ManagementMode::Status
        } else {
            management::ManagementMode::General
        };
        self.management.open_text(ManagementView {
            title: "操作 · 受付待ち".into(),
            text: "処理しています。Wは明示waitの観測だけを取消します".into(),
            scroll: 0,
        });
        vec![UiAction::Manage(command)]
    }
    fn management_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        let page = self
            .management
            .page(popup::viewport(self.viewport), &self.view().status);
        let thread_id = self.view().thread_id.clone();
        match self.management.key(key, page, thread_id) {
            management::Outcome::Actions(actions) => actions,
            management::Outcome::Close => {
                self.invalidate_management();
                vec![]
            }
            management::Outcome::Original(node_id) => {
                let mut detail = details::Details::new(self.view(), false);
                let action = detail.open_source(node_id, &mut self.next_detail_request);
                self.details = Some(detail);
                vec![action]
            }
            management::Outcome::Operations => {
                self.open_operations(None);
                vec![]
            }
            management::Outcome::Confirm { command, detail } => {
                self.confirmation = Some(Confirmation::Index { command, detail });
                vec![]
            }
        }
    }
    fn confirmation_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        if matches!(self.confirmation, Some(Confirmation::ThreadDeletion(_))) {
            return self.thread_deletion_key(key);
        }
        let page = self.confirmation.as_ref().map_or(1, |confirmation| {
            confirmation.page(popup::viewport(self.viewport))
        });
        if let Some(scroll) = popup::reader_key(key.code, self.confirmation_scroll, page) {
            self.confirmation_scroll = scroll;
        }

        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N')
        ) {
            self.confirmation_scroll = 0;
            self.confirmation = None;
            return vec![];
        }
        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            return vec![];
        }
        self.confirmation_scroll = 0;
        match self.confirmation.take() {
            Some(Confirmation::EditStop {
                draft_id,
                revision,
                run_id,
            }) => {
                if self.view().draft.draft_id != draft_id || self.view().draft.revision != revision
                {
                    self.active_view_mut().status =
                        "下書きが変更されたため確認を取り消しました。再送してください".into();
                    return vec![];
                }
                self.send_current(Some(run_id))
            }
            Some(Confirmation::Index { command, .. }) => {
                self.management.preview = None;
                vec![UiAction::Manage(command)]
            }
            Some(Confirmation::Operation { command, .. }) => vec![UiAction::Manage(command)],
            Some(Confirmation::ThreadDeletion(_)) => {
                unreachable!("thread deletion owns its confirmation")
            }
            Some(Confirmation::ForceQuit) => vec![UiAction::ForceQuit],
            None => vec![],
        }
    }
    fn shutdown_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        if matches!(self.confirmation, Some(Confirmation::ForceQuit)) {
            return self.confirmation_key(key);
        }
        match key.code {
            KeyCode::Char('w') | KeyCode::Char('W') | KeyCode::Enter => {
                vec![UiAction::WaitForShutdown]
            }
            KeyCode::Char('f') | KeyCode::Char('F')
                if self
                    .lifecycle
                    .progress()
                    .is_some_and(|(_, _, available)| *available) =>
            {
                self.confirmation = Some(Confirmation::ForceQuit);
                vec![]
            }
            _ => vec![],
        }
    }
    fn draw_overlays(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
    ) -> Vec<crate::terminal_links::LinkRegion> {
        let mut link_regions = Vec::new();
        let status = self
            .views
            .get(&self.active)
            .map_or("", |view| view.status.as_str());
        self.management.draw(frame, area, status);
        if let Some(details) = &self.details {
            link_regions = details.draw(frame, area, self.view());
        }
        if let Some(operations) = &self.operations {
            operations.draw(frame, area);
            link_regions.clear();
        }
        if let Some(models) = &mut self.models {
            models.draw(frame, area);
            link_regions.clear();
        }
        if self.is_quitting() {
            let text = if let Some((elapsed, detail, force)) = self.lifecycle.progress() {
                format!(
                    "終了処理中 {}ms\n{}\n{}",
                    elapsed,
                    detail,
                    if *force {
                        "W/Enter: 待機継続  F: 強制終了を選択（保存/回収未完了の可能性）"
                    } else {
                        "正常終了の保存・停止を待っています"
                    }
                )
            } else {
                "終了処理を開始しています。保存・停止を待っています".into()
            };
            let body = popup::compact(
                frame,
                area,
                "終了処理",
                "保存と実行停止の状態",
                "W / Enter 待機継続 · F 強制終了を選択",
                12,
            );
            let lines = text
                .lines()
                .map(|line| Line::from(line.to_owned()))
                .collect::<Vec<_>>();
            popup::reader(frame, body, &lines, 0);
        }
        if let Some(confirmation) = &self.confirmation {
            let (text, height) = confirmation.layout(area);
            let body = popup::compact(
                frame,
                area,
                "実行の確認",
                CONFIRMATION_CONTEXT,
                confirmation.footer(),
                height,
            );
            let lines = text
                .lines()
                .map(|line| Line::from(line.to_owned()))
                .collect::<Vec<_>>();
            self.confirmation_scroll = popup::reader(frame, body, &lines, self.confirmation_scroll);
        }
        if matches!(
            self.top_overlay(),
            Some(
                overlay::Overlay::Shutdown
                    | overlay::Overlay::Confirmation
                    | overlay::Overlay::Selection
            )
        ) {
            link_regions.clear();
        }
        link_regions
    }
    fn command_error(&mut self, message: &str) -> Vec<UiAction> {
        self.command_rejected = true;
        self.active_view_mut().status = format!("コマンドエラー: {message}");
        vec![]
    }
    fn thread_key(&mut self, key: KeyEvent) -> Vec<UiAction> {
        match self.thread_browser.key(key) {
            threads::ThreadOutcome::Delete(thread) => self.begin_thread_deletion(thread),
            threads::ThreadOutcome::None => vec![],
            threads::ThreadOutcome::Load(action) => vec![*action],
            threads::ThreadOutcome::Open(thread) => self.select_thread(thread),
            threads::ThreadOutcome::Info(body) => {
                self.show_help(help::Page::info("会話の情報", "会話一覧で選んだ対象", body))
            }
        }
    }
    /// Reconcile a transient wrapped row against a durable source anchor.
    fn viewport_row(&mut self, layout: &HistoryLayout, width: u16, height: u16) -> usize {
        self.active_view_mut().viewport_row(layout, width, height)
    }
    fn scroll(&mut self, amount: isize) -> Vec<UiAction> {
        let budget = usize::try_from(self.config.cache.layout_bytes).unwrap_or(usize::MAX);
        self.views
            .get_mut(&self.active)
            .expect("active view exists")
            .scroll(amount, &self.active, &mut self.layout_cache, budget)
    }
    fn trim_bodies(&mut self) {
        #[cfg(test)]
        {
            self.retention_passes += 1;
        }
        let budget = usize::try_from(self.config.cache.body_bytes).unwrap_or(usize::MAX);
        for view in self.views.values_mut() {
            view.retain_source_ids();
        }
        // Determine the viewport working set once. These same nodes are exempt
        // from both payload and metadata retention, including at either edge.
        let working = working_node_ids(self.view());
        let mut retained = self
            .views
            .iter()
            .map(|(key, view)| {
                retained_body_bytes(view, if key == &self.active { &working } else { &[] })
            })
            .sum::<usize>();
        if retained <= budget {
            return;
        }
        // Vec::clear/remove leave the allocation alive. Release spare slots
        // before evicting useful content, and account for their actual capacity.
        for view in self.views.values_mut() {
            retained = retained.saturating_sub(view.compact_node_slots());
        }
        let mut inactive = self
            .views
            .iter()
            .filter(|(key, _)| *key != &self.active)
            .map(|(key, view)| (view.generation, key.clone()))
            .collect::<Vec<_>>();
        inactive.sort();
        for (_, key) in inactive {
            if retained <= budget {
                break;
            }
            let view = self.views.get_mut(&key).expect("known inactive view");
            for index in 0..view.nodes().len() {
                if retained <= budget {
                    break;
                }
                retained = retained.saturating_sub(view.evict_node_body(index));
            }
            if retained > budget {
                retained = retained.saturating_sub(retained_body_bytes(view, &[]));
                view.discard_cached_history();
            }
        }
        let view = self.active_view_mut();
        let protected = viewport_node_index(view);
        let mut candidates = (0..view.nodes().len())
            .filter(|index| !working.contains(&view.nodes()[*index].node_id))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|index| (std::cmp::Reverse(index.abs_diff(protected)), *index));
        for index in candidates {
            if retained <= budget {
                break;
            }
            retained = retained.saturating_sub(view.evict_node_body(index));
        }
        view.trim_page_edges(&working, &mut retained, budget);
    }
}
fn merge_navigation_window(view: &mut ThreadView, incoming: &mut Vec<UiNode>, first: &mut u64) {
    view.merge_navigation_window(incoming, first);
}
fn viewport_node_index(view: &ThreadView) -> usize {
    view.viewport_node_index()
}
fn projection_node_index(view: &ThreadView) -> usize {
    view.projection_node_index()
}
fn working_node_ids(view: &ThreadView) -> Vec<String> {
    view.working_node_ids()
}
fn expanded_id_bytes(id: &String) -> usize {
    std::mem::size_of::<String>() + id.capacity() + 48
}
fn retained_body_bytes(view: &ThreadView, working: &[String]) -> usize {
    view.retained_body_bytes(working)
}
fn valid_draft(draft: &DraftSnapshot) -> bool {
    draft.cursor <= draft.text.len()
        && draft.text.is_char_boundary(draft.cursor)
        && draft::decode(&draft.text, draft.attachments.clone()).is_ok()
        && draft.folds.iter().all(|f| {
            f.start <= f.end
                && f.end <= draft.text.len()
                && draft.text.is_char_boundary(f.start)
                && draft.text.is_char_boundary(f.end)
        })
}
fn receipt_label(status: &ReceiptStatus) -> &'static str {
    match status {
        ReceiptStatus::Accepting => "受付処理中",
        ReceiptStatus::Accepted => "受付済み",
        ReceiptStatus::Preparing => "受付済み／準備中",
        ReceiptStatus::Held => "保留",
        ReceiptStatus::Failed => "失敗",
        ReceiptStatus::Cancelled => "取消済み",
        ReceiptStatus::Completed => "完了",
    }
}
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
}
pub fn key_matches(binding: &str, key: KeyEvent) -> bool {
    let mut parts: Vec<_> = binding.split('+').collect();
    let name = parts.pop().unwrap_or("");
    let mut modifiers = KeyModifiers::NONE;
    for part in parts {
        modifiers |= match part.to_ascii_lowercase().as_str() {
            "ctrl" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return false,
        };
    }
    let code = match name.to_ascii_lowercase().as_str() {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "esc" | "escape" => KeyCode::Esc,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "space" => KeyCode::Char(' '),
        _ => {
            if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse().ok()) {
                KeyCode::F(number)
            } else if name.chars().count() == 1 {
                KeyCode::Char(name.chars().next().unwrap().to_ascii_lowercase())
            } else {
                return false;
            }
        }
    };
    let actual = if let KeyCode::Char(ch) = key.code {
        KeyCode::Char(ch.to_ascii_lowercase())
    } else {
        key.code
    };
    actual == code && key.modifiers == modifiers
}

/// Display representation and byte cursor; folded text remains intact in DraftSnapshot.
pub fn draft_display(draft: &DraftSnapshot) -> (String, usize) {
    input::draft_display(draft)
}
pub fn cursor_position(text: &str, byte_cursor: usize, width: u16) -> (u16, u16) {
    input::cursor_position(text, byte_cursor, width)
}
/// Render a cached source window without changing canonical text or offsets.
fn rendered_node_text(node: &UiNode, expanded: bool, start: usize, limit: usize) -> String {
    if node.text.is_empty() && node.total_chars > 0 {
        return "[本文未キャッシュ · スクロールで原文を再取得]".into();
    }
    let start = start.max(node.base_char_offset);
    let mut range = 0;
    while range < node.reasoning_ranges.len() && node.reasoning_ranges[range].1 <= start {
        range += 1;
    }
    let mut hidden_until = 0;
    let mut out = String::new();
    let mut displayed = 0;
    for (index, ch) in node
        .text
        .chars()
        .enumerate()
        .skip(start - node.base_char_offset)
    {
        if displayed >= limit {
            break;
        }
        let global = node.base_char_offset + index;
        if global < hidden_until {
            continue;
        }
        while range < node.reasoning_ranges.len() && node.reasoning_ranges[range].1 <= global {
            range += 1;
        }
        if !expanded
            && let Some(&(begin, end)) = node.reasoning_ranges.get(range)
            && begin <= global
            && global < end
        {
            let label = format!(
                "[推論 {}文字 · 折りたたみ /reasoning]\n",
                end.saturating_sub(begin)
            );
            out.push_str(&label);
            displayed += label.chars().count();
            hidden_until = end;
        } else {
            out.push(ch);
            displayed += 1;
        }
    }
    out
}
fn draw_recovery(frame: &mut Frame<'_>, area: Rect, snapshot: &activity::ActivitySnapshot) {
    let Some(recovery) = &snapshot.recovery else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let current = snapshot.phase == activity::ActivityPhase::Recovery;
    let title = if current {
        "! 復旧待ち · 操作を待っています".to_owned()
    } else {
        format!("! 前回の復旧待ち · {}", snapshot.phase.label())
    };
    let reason = crate::presentation::sanitize_plain(&recovery.message, 0)
        .0
        .replace('\n', " ");
    let next = if current {
        "/retry 同じ内容で再試行 · 入力で新指示 · Ctrl+C 停止 · F8 詳細"
    } else {
        "入力で新しい作業 · F8 作業/応答処理の詳細"
    };
    let mut lines = vec![
        Line::from(title),
        Line::from(format!("{} · {reason}", recovery.stage)),
    ];
    let reduction_row = u16::from(snapshot.context_reduction.is_some() && area.height >= 4);
    if reduction_row > 0
        && let Some(summary) = &snapshot.context_reduction
    {
        lines.push(Line::from(summary.clone()));
    }
    if !current && let Some(reason) = &recovery.stop_reason {
        let label = match reason.as_str() {
            "explicit_stop" => "人による停止",
            "shutdown" | "app_shutdown" => "アプリ終了",
            _ => "停止理由はF8の詳細で確認",
        };
        if area.height >= 4 + reduction_row {
            lines.push(Line::from(label));
        }
    } else if area.height >= 4 + reduction_row {
        lines.push(Line::from(format!(
            "依頼は保存済み · 未反映指示 {}件 · 保存済みツール結果 {}件",
            recovery.pending_messages, recovery.saved_tool_results
        )));
    }
    if area.height >= 5 + reduction_row {
        lines.push(Line::from(format!(
            "動作中shell {}件 · 回収未完了 {}件 · 副作用は詳細で確認",
            recovery.live_shells, recovery.uncollected_shells
        )));
    }
    lines.push(Line::from(next));
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme::WARNING))
            .block(Block::default().borders(Borders::LEFT)),
        area,
    );
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    if state.selection_mode {
        frame.render_widget(Clear, area);
    }
    let title = chrome::header(state.view(), area.width, state.selection_mode);
    frame.render_widget(
        Paragraph::new(title).style(if state.selection_mode {
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        }),
        area,
    );
}
fn draw_conversation(frame: &mut Frame<'_>, area: Rect, history: &HistoryLayout, scroll: usize) {
    let paragraph = Paragraph::new(presentation::borrowed(
        &history.styled[scroll.min(history.styled.len())
            ..history
                .styled
                .len()
                .min(scroll.saturating_add(area.height.saturating_sub(2) as usize))],
    ))
    .wrap(Wrap { trim: false });
    let paragraph =
        paragraph.block(Block::default().padding(ratatui::widgets::Padding::uniform(1)));
    frame.render_widget(paragraph, area);
}
fn draw_input(
    frame: &mut Frame<'_>,
    input_area: Rect,
    state: &UiState,
    input: &input::Display,
    metrics: &input::Metrics,
) -> (u16, u16) {
    let view = state.view();
    let input_width = input_area.width.saturating_sub(2).max(1);
    let input_body_height = input_area.height.saturating_sub(2);
    let (x, y) = metrics.cursor;
    let input_scroll = y.saturating_sub(usize::from(input_body_height.saturating_sub(1)));
    let phase = view
        .activity
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.phase);
    let (primary, secondary) = input::hints(&state.config.ui, phase, !view.draft.folds.is_empty());
    frame.render_widget(
        Paragraph::new(input::visible(
            input,
            input_width,
            input_scroll,
            input_body_height,
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(primary)
                .title_bottom(secondary),
        ),
        input_area,
    );
    (
        input_area.x + 1 + x.min(usize::from(input_width - 1)) as u16,
        input_area.y
            + 1
            + y.saturating_sub(input_scroll)
                .min(usize::from(input_body_height.saturating_sub(1))) as u16,
    )
}
fn draw_status(frame: &mut Frame<'_>, area: Rect, state: &UiState) {
    let view = state.view();
    let follow = if view.follow {
        "追従中"
    } else if view.unseen {
        "追従停止・新着あり · Ctrl+Endで末尾へ"
    } else {
        "追従停止 · Ctrl+Endで末尾へ"
    };
    let mode = if state.selection_mode {
        "F2/Esc解除 · 選択中・表示固定"
    } else {
        "F1 ヘルプ · F6 詳細 · F8 操作 · Ctrl+Q 終了"
    };
    let send = view.sending();
    let activity_phase = view.footer_phase();
    let has_activity = view.activity.snapshot.is_some();
    let has_indicator = has_activity || send.is_some();
    let busy = if has_indicator {
        activity_phase.is_active() || activity_phase == activity::ActivityPhase::Recovery
    } else {
        view.run_id.is_some()
    };
    let attention = matches!(
        activity_phase,
        activity::ActivityPhase::Recovery | activity::ActivityPhase::Failed
    ) || (!has_activity
        && ["失敗", "エラー", "復旧待ち", "異常終了", "error", "failed"]
            .iter()
            .any(|word| view.state.contains(word) || view.status.contains(word)));
    let status_note = if view.status == activity::ActivityPhase::Idle.label()
        || (has_activity
            && (view.status == activity_phase.label()
                || view.status == view.state
                || view.status == "完了"))
    {
        ""
    } else {
        &view.status
    };
    let mut status_parts = vec![follow.to_owned()];
    if activity_phase.is_active()
        && let Some(summary) = view
            .activity
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.context_reduction.as_ref())
    {
        status_parts.insert(0, summary.clone());
    }
    if let Some((phase, count, started)) = send {
        let mut note = String::new();
        if phase != activity_phase {
            note.push_str(phase.label());
        }
        if count > 1 {
            note.push_str(&format!(" {count}件"));
        }
        let seconds = started.elapsed().as_secs();
        if seconds > 0 {
            note.push_str(&format!(" 送信から{seconds}秒"));
        }
        if !note.is_empty() {
            status_parts.insert(0, note.trim().to_owned());
        }
    }
    if activity_phase != activity::ActivityPhase::Recovery
        && busy
        && send.is_none()
        && let Some(elapsed_ms) = view.activity.displayed_elapsed_ms()
        && elapsed_ms > 0
    {
        let seconds = elapsed_ms / 1000;
        status_parts.push(if seconds >= 60 {
            format!("観測経過{}分{}秒", seconds / 60, seconds % 60)
        } else {
            format!("観測経過{seconds}秒")
        });
    }
    if let Some(note) = view.activity.receive_gap_note(Instant::now()) {
        status_parts.push(note);
    }
    if view.tool_calls > 0 {
        status_parts.push(format!("ツール{}回", view.tool_calls));
    }
    if view.cancelled_calls > 0 {
        status_parts.push(format!("取消{}件", view.cancelled_calls));
    }
    if !has_indicator {
        status_parts.insert(
            0,
            if view.thread_id.is_some() {
                "状態を確認中"
            } else {
                "入力できます"
            }
            .into(),
        );
    }
    let status_text = status_parts.join(" · ");
    let style = Style::default().fg(if attention {
        theme::ATTENTION
    } else if busy || view.loading {
        theme::ACCENT
    } else {
        theme::MUTED
    });
    let mut status_spans = Vec::new();
    if has_indicator {
        status_spans.extend(state.animation.spans(activity_phase, style, Instant::now()));
        if activity_phase == activity::ActivityPhase::Retrying
            && let Some(retry) = view
                .activity
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.automatic_retry.as_ref())
        {
            status_spans.push(Span::styled(retry.label(), style));
        }
        status_spans.push(Span::styled(" · ", style));
    }
    if view.loading {
        status_spans.push(Span::styled("履歴 読込み中 · ", style));
    }
    status_spans.push(Span::styled(status_text.to_owned(), style));
    let notice = if let Some(notice) = state.input_notice {
        notice.to_owned()
    } else if !state.selection_mode && !status_note.is_empty() {
        format!("{status_note} · F1 ヘルプ · F8 操作")
    } else {
        mode.to_owned()
    };
    let notice = if state.input_notice.is_some() {
        Line::styled(notice, Style::default().fg(theme::ATTENTION))
    } else {
        Line::from(notice)
    };
    let status = vec![Line::from(status_spans), notice];

    frame.render_widget(Paragraph::new(status), area);
}

fn visible_render_nodes(
    layout: &HistoryLayout,
    row: usize,
    height: u16,
    nodes: usize,
) -> BTreeSet<usize> {
    layout
        .sources
        .iter()
        .skip(row)
        .take(usize::from(height))
        .flat_map(|source| source.node..source.covered_end.min(nodes))
        .collect()
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    let area = frame.area();
    state.viewport = area;
    if area.width < 8 || area.height < 5 {
        frame.render_widget(Paragraph::new("Shellboy · enlarge terminal"), area);
        state.visible_links.clear();
        state.dirty = false;
        return;
    }
    let input = input::display(&state.view().draft);
    let input_width = area.width.saturating_sub(2).max(1);
    let input_metrics = input::measure(&input.text, input.cursor, input_width);
    let input_height = input_metrics
        .rows
        .saturating_add(2)
        .min(usize::from((area.height / 4).clamp(3, 8)))
        .min(usize::from(area.height.saturating_sub(4))) as u16;
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(popup::STATUS_ROWS),
        ])
        .split(area);
    let recovery_height = if state
        .view()
        .activity
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.recovery.is_some())
    {
        6.min(layout[1].height.saturating_sub(3))
    } else {
        0
    };
    let body_area = Rect {
        height: layout[1].height.saturating_sub(recovery_height),
        ..layout[1]
    };
    let recovery_area = Rect {
        y: body_area.bottom(),
        height: recovery_height,
        ..layout[1]
    };
    let render_preflight = RenderPreflight::new(state.view());
    let history = state.history_layout(
        body_area.width.saturating_sub(2),
        body_area.height.saturating_sub(2),
    );
    let scroll = state.viewport_row(
        &history,
        body_area.width.saturating_sub(2),
        body_area.height.saturating_sub(2),
    );
    let visible = visible_render_nodes(
        &history,
        scroll,
        body_area.height.saturating_sub(2),
        state.view().nodes().len(),
    );
    state.prepare_visible_render_width(
        body_area.width.saturating_sub(4).max(1),
        &visible,
        render_preflight,
    );
    state.active_view_mut().scroll.area = Some(body_area);
    let view = state.view();
    if !state.selection_mode {
        draw_header(frame, layout[0], state);
    }
    if view.thread_id.is_none()
        && view.nodes().is_empty()
        && view.provisional_nodes.is_empty()
        && view.pending.is_empty()
    {
        frame.render_widget(
            Paragraph::new(chrome::welcome(
                view,
                &state.config,
                body_area.width.saturating_sub(2),
            ))
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(ratatui::widgets::Padding::uniform(1))),
            body_area,
        );
    } else {
        draw_conversation(frame, body_area, &history, scroll);
    }
    if let Some(snapshot) = &view.activity.snapshot {
        draw_recovery(frame, recovery_area, snapshot);
    }
    let input_area = layout[2];
    let input_body_height = input_area.height.saturating_sub(2);
    let cursor = draw_input(frame, input_area, state, &input, &input_metrics);
    draw_status(frame, layout[3], state);
    state.draw_completions(frame, input_area);
    if input_body_height > 0 && state.composer_active() {
        frame.set_cursor_position(cursor);
    }
    let popup_area = popup::viewport(area);
    if state.thread_browser.visible() {
        let active = state.view().thread_id.as_deref();
        let notice_visible = state.top_overlay() == Some(overlay::Overlay::Threads);
        state
            .thread_browser
            .draw(frame, popup_area, active, notice_visible);
    }
    if state.help_visible {
        let body = popup::panel(
            frame,
            popup_area,
            state.help_page.title,
            state.help_page.context,
            HELP_FOOTER,
        );
        let lines = state.help_page.lines(popup::reader_width(body));
        state.help_scroll = popup::reader(frame, body, &lines, state.help_scroll);
    }
    let overlay_links = state.draw_overlays(frame, popup_area);
    state.visible_links = overlay_links;
    if state.history_links_active() {
        state.visible_links.extend(
            presentation::link_regions(
                &history.styled[scroll.min(history.styled.len())
                    ..history
                        .styled
                        .len()
                        .min(scroll.saturating_add(body_area.height.saturating_sub(2) as usize))],
                &history.targets,
                body_area.width.saturating_sub(2),
                body_area.height.saturating_sub(2),
                0,
            )
            .into_iter()
            .map(|mut region| {
                region.x += body_area.x + 1;
                region.y += body_area.y + 1;
                region
            }),
        );
    }
    if state.selection_mode {
        // The selection owner remains visible even when a minimum-size popup
        // occupies row zero. Its input/footer state is otherwise unchanged.
        draw_header(frame, layout[0], state);
    }
    state.dirty = false;
}
pub const HELP: &str = "操作するときはEscで会話へ戻る · / でコマンド候補を表示";

/// Must run on the main OS thread. Updates should be sent through a bounded
/// std::sync::mpsc::sync_channel; the UI only calls try_recv and try_send.
pub fn run(
    config: &Config,
    actions: impl Into<actions::ActionSender>,
    updates: Receiver<UiUpdate>,
) -> io::Result<UiExitReason> {
    let mut session = TerminalSession::enter()?;
    run_in_session(&mut session, config, actions, updates)
}

/// Reuse one terminal session across initialization and the normal UI.
pub struct TerminalSession {
    terminal: Terminal<crate::terminal_links::HyperlinkBackend<io::Stdout>>,
    link_snapshot: std::rc::Rc<std::cell::RefCell<crate::terminal_links::LinkSnapshot>>,
    _guard: TerminalGuard,
}
impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        let guard = TerminalGuard::new();
        terminal::enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;
        let link_snapshot = std::rc::Rc::new(std::cell::RefCell::new(
            crate::terminal_links::LinkSnapshot::default(),
        ));
        let terminal = Terminal::new(crate::terminal_links::HyperlinkBackend::new(
            io::stdout(),
            std::rc::Rc::clone(&link_snapshot),
        ))?;
        Ok(Self {
            terminal,
            link_snapshot,
            _guard: guard,
        })
    }
}

pub fn run_in_session(
    session: &mut TerminalSession,
    config: &Config,
    actions: impl Into<actions::ActionSender>,
    updates: Receiver<UiUpdate>,
) -> io::Result<UiExitReason> {
    let actions = actions.into();
    let terminal = &mut session.terminal;
    let link_snapshot = &session.link_snapshot;
    let mut state = UiState::new(config, PathBuf::new());
    state.viewport = terminal.get_frame().area();
    let frame_time = Duration::from_secs_f64(1.0 / config.ui.max_fps.max(1) as f64);
    let mut last_draw = Instant::now() - frame_time;
    let mut selecting = false;
    let mut last_save = Instant::now();
    loop {
        state.drain_updates(&updates);
        if state.is_stopped() {
            break;
        }
        state.bell.flush(terminal.backend_mut(), Instant::now())?;
        for action in state.take_deferred_actions() {
            if let Err(error) = actions.try_send(action) {
                state.deferred_actions.push(error.into_inner());
            }
        }
        for bytes in state.take_terminal_requests() {
            use std::io::Write;
            terminal.backend_mut().write_all(&bytes)?;
            terminal.backend_mut().flush()?;
        }
        if state.autosave.pending() && last_save.elapsed() >= Duration::from_millis(250) {
            state.submit_saves(&actions);
            last_save = Instant::now();
        }
        let mode_changed = selecting != state.selection_mode;
        if mode_changed {
            if state.selection_mode {
                execute!(terminal.backend_mut(), DisableMouseCapture)?;
            } else {
                execute!(terminal.backend_mut(), EnableMouseCapture)?;
            }
            selecting = state.selection_mode;
        }
        state.animation_tick(Instant::now());
        if state.draw_due(mode_changed, last_draw.elapsed(), frame_time) {
            redraw::draw_terminal(terminal, &mut state, link_snapshot)?;
            last_draw = Instant::now();
        }
        let wait = if state.dirty && (!state.selection_mode || state.full_redraw_requested) {
            frame_time
                .saturating_sub(last_draw.elapsed())
                .min(Duration::from_millis(25))
        } else {
            Duration::from_millis(25)
        };
        let wait = state
            .thread_browser
            .filter_wait(Instant::now())
            .map_or(wait, |remaining| wait.min(remaining));
        if event::poll(wait)? {
            let actions_to_send = state.event(event::read()?);
            for error in actions.try_send_batch(actions_to_send) {
                state.action_rejected(&error.into_inner());
            }
        }
    }
    Ok(if state.is_forced() {
        UiExitReason::Forced
    } else {
        UiExitReason::Normal
    })
}

#[cfg(test)]
mod layout_cache_tests {
    use super::*;
    fn node(id: &str, text: &str) -> UiNode {
        UiNode {
            presentation: Default::default(),
            node_id: id.into(),
            role: "assistant".into(),
            text: text.into(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
        }
    }
    fn history(ui: &mut UiState, thread: &str, text: &str) {
        let generation = ui.view().generation;
        ui.apply(UiUpdate::History {
            thread_id: thread.into(),
            generation,
            nodes: vec![node("n", text)],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        });
    }
    #[test]
    fn management_selection_is_typed_and_controls_stay_fixed() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        let copied = "full value\n› this is data, not a selection marker";
        ui.apply(UiUpdate::StructuredManagement {
            work_status: false,
            request_id: 44,
            title: "結果".into(),
            omitted: false,
            fields: vec![
                PresentationField {
                    value_omitted: false,
                    display: None,
                    key: "/first".into(),
                    label: "group › first".into(),
                    value: "one".into(),
                },
                PresentationField {
                    value_omitted: false,
                    display: None,
                    key: "/second".into(),
                    label: "group › second".into(),
                    value: copied.into(),
                },
            ],
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let footer = |terminal: &Terminal<TestBackend>| {
            let buffer = terminal.backend().buffer();
            (0..24).find(|y| {
                (0..80)
                    .map(|x| buffer[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("Esc")
            })
        };
        let before = footer(&terminal).unwrap();
        assert!(
            ui.event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)))
                .is_empty()
        );
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        assert_eq!(footer(&terminal), Some(before));
        let actions = ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )));
        assert!(matches!(&actions[..], [UiAction::CopyText { text, .. }] if text == copied));
        assert_eq!(ui.management.structured().unwrap().selected, 1);
    }
    #[test]
    fn long_confirmation_can_be_read_without_accepting_it() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.confirmation = Some(Confirmation::Operation {
            command: Command::ThreadGet(ThreadTarget {
                thread_id: Some("fixture".into()),
            }),
            detail: (0..80).map(|n| format!("CHECK {n:03}\n")).collect(),
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            )))
            .is_empty()
        );
        assert!(ui.confirmation.is_some(), "Enter cannot confirm a mutation");
        assert!(
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::PageDown,
                KeyModifiers::NONE
            )))
            .is_empty()
        );
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        assert!(ui.confirmation_scroll > 0);
        use unicode_width::UnicodeWidthStr;
        let text = (0..24)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < 80 {
                    let symbol = terminal.backend().buffer()[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("N / Esc") && text.contains("Y 実行"));
        assert!(
            ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .is_empty()
        );
        assert!(ui.confirmation.is_none());
    }

    #[test]
    fn completed_markdown_single_rows_remain_monotonic_across_body_reloads() {
        use ratatui::{Terminal, backend::TestBackend};
        let source = (0..2500)
            .map(|i| {
                if i % 80 == 0 {
                    format!(
                        "\n## Section {i}\n\n```rust\nM{i:05} let value = \"{}\";\n",
                        "words ".repeat(5)
                    )
                } else if i % 80 == 40 {
                    format!("M{i:05} final_code_line();\n```\n\n")
                } else {
                    format!(
                        "M{i:05} example paragraph with **bold** and `code` {}\n",
                        "content ".repeat(4)
                    )
                }
            })
            .collect::<String>();
        let payload = serde_json::json!({"text":source});
        let window = |offset: usize| {
            let start = offset.saturating_sub(512);
            let end = (start + 65_536).min(source.len());
            let mut value = node("answer", &source[start..end]);
            value.base_char_offset = start;
            value.total_chars = source.len();
            value.presentation =
                crate::presentation::project("assistant_response", &payload, &value.text, start);
            crate::presentation::render_markdown(&mut value.presentation, 76);
            value
        };
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        *ui.view_mut().nodes_mut_for_test() = vec![window(512)];
        ui.view_mut().total_nodes = 1;
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("answer".into()),
            char_offset: 64_000,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        let mut previous = 0;
        let mut read_count = 0;
        for step in 0..100 {
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            let buffer = terminal.backend().buffer();
            let text = (2..22)
                .map(|y| (1..79).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            let first = text
                .match_indices('M')
                .find_map(|(i, _)| {
                    text.get(i + 1..i + 6)
                        .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap();
            assert!(
                first >= previous,
                "step={step} read_count={read_count} top={previous}->{first}, anchor={:?}",
                ui.view().anchor
            );
            previous = first;
            let mut requests = std::collections::VecDeque::from(ui.take_deferred_actions());
            requests.extend(ui.scroll(1));
            while let Some(action) = requests.pop_front() {
                if let UiAction::LoadNodeWindow {
                    thread_id,
                    generation,
                    view_revision,
                    char_offset,
                    ..
                } = action
                {
                    read_count += 1;
                    assert!(read_count < 30, "bounded navigation must converge");
                    ui.apply(UiUpdate::NodeWindow {
                        thread_id,
                        generation,
                        view_revision,
                        node: window(char_offset),
                    });
                    requests.extend(ui.take_deferred_actions());
                }
            }
        }
        assert!(read_count > 0);
    }

    #[test]
    fn downward_single_rows_never_return_to_an_older_projection() {
        for height in [8, 32, 80, 160] {
            let mut config = Config::default();
            config.cache.body_bytes = u64::MAX;
            let mut ui = UiState::new(&config, std::env::temp_dir());
            ui.select_thread("t".into());
            *ui.view_mut().nodes_mut_for_test() = (0..400)
                .map(|i| {
                    let lines = if i % 7 == 0 { 100 } else { 1 };
                    node(
                        &format!("n{i}"),
                        &(0..lines)
                            .map(|j| format!("NODE{i:04} ROW{j:03} {}\n", "x".repeat(60)))
                            .collect::<String>(),
                    )
                })
                .collect();
            ui.view_mut().total_nodes = 400;
            ui.view_mut().follow = false;
            ui.view_mut().anchor = ViewAnchor {
                node_id: Some("n100".into()),
                char_offset: 0,
            };
            let layout = ui.history_layout(80, height);
            let top = layout.lines.saturating_sub(usize::from(height) + 2);
            let source = layout.sources[top];
            ui.view_mut().anchor = ViewAnchor {
                node_id: Some(ui.view().nodes()[source.node].node_id.clone()),
                char_offset: source.offset,
            };
            ui.active_view_mut().scroll.within = top
                - layout
                    .sources
                    .iter()
                    .position(|r| r.node == source.node && r.offset == source.offset)
                    .unwrap();
            ui.active_view_mut().scroll.anchor = ui.view().anchor.clone();
            let mut previous = (0, 0, 0);
            for step in 0..80 {
                let layout = ui.history_layout(80, height);
                let top = ui.viewport_row(&layout, 80, height);
                let source = layout.sources[top];
                let first = layout
                    .sources
                    .iter()
                    .position(|r| r.node == source.node && r.offset == source.offset)
                    .unwrap();
                let position = (source.node, source.offset, top - first);
                assert!(
                    position >= previous,
                    "height={height} step={step}: {previous:?} -> {position:?}"
                );
                previous = position;
                let actions = ui.scroll(1);
                assert!(actions.is_empty(), "all sources are cached");
            }
        }
    }

    #[test]
    fn retry_footer_renders_saved_counts_and_clears_them_after_success() {
        use ratatui::{Terminal, backend::TestBackend};
        use unicode_width::UnicodeWidthStr;
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let generation = ui.view().generation;
        let update = |phase, automatic_retry, epoch| UiUpdate::Activity {
            thread_id: "t".into(),
            generation,
            epoch,
            observed_at: Instant::now(),
            snapshot: activity::ActivitySnapshot {
                phase,
                automatic_retry,
                ..Default::default()
            },
        };
        ui.apply(update(
            activity::ActivityPhase::Retrying,
            Some(activity::AutomaticRetrySnapshot {
                number: 1,
                limit: 2,
                delay_ms: 60_000,
                partial_response_discarded: true,
            }),
            1,
        ));
        let mut terminal = Terminal::new(TestBackend::new(140, 2)).unwrap();
        terminal
            .draw(|frame| draw_status(frame, frame.area(), &ui))
            .unwrap();
        let line = |terminal: &Terminal<TestBackend>| {
            let buffer = terminal.backend().buffer();
            let mut text = String::new();
            let mut x = 0;
            while x < 140 {
                let symbol = buffer[(x, 0)].symbol();
                text.push_str(symbol);
                // A wide glyph owns its following cell; its placeholder is
                // not a rendered space between Japanese characters.
                x += symbol.width().max(1) as u16;
            }
            text
        };
        assert!(line(&terminal).contains("一時的なエラーのため再試行中（1/2）"));
        assert!(line(&terminal).contains("60秒待機"));
        assert!(line(&terminal).contains("途中応答を保存して作り直し"));
        assert!(line(&terminal).contains("重複課金の可能性"));
        assert_ne!(terminal.backend().buffer()[(0, 0)].fg, theme::ATTENTION);
        ui.apply(update(activity::ActivityPhase::Completed, None, 2));
        terminal
            .draw(|frame| draw_status(frame, frame.area(), &ui))
            .unwrap();
        assert!(line(&terminal).contains("完了"));
        assert!(!line(&terminal).contains("再試行"));
        assert!(!line(&terminal).contains("（1/2）"));
        assert_ne!(terminal.backend().buffer()[(0, 0)].fg, theme::ATTENTION);
    }

    #[test]
    fn receive_gap_updates_are_scoped_and_preserve_history_layout() {
        let at = Instant::now();
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        history(&mut ui, "t", "unchanged conversation");
        let layout = ui.history_layout(80, 20);
        let revision = ui.view().history_revision();
        let generation = ui.view().generation;
        ui.apply(UiUpdate::Activity {
            thread_id: "t".into(),
            generation,
            epoch: 1,
            observed_at: at,
            snapshot: activity::ActivitySnapshot {
                attempt_id: Some("attempt".into()),
                phase: activity::ActivityPhase::AwaitingModel,
                ..Default::default()
            },
        });
        let receive = |thread: &str, generation, attempt: &str| UiUpdate::ModelReceive {
            thread_id: thread.into(),
            generation,
            attempt_id: attempt.into(),
            observed_at: at + Duration::from_secs(31),
        };
        ui.dirty = false;
        ui.apply(receive("other", generation, "attempt"));
        ui.apply(receive("t", generation + 1, "attempt"));
        ui.apply(receive("t", generation, "stale"));
        assert!(!ui.dirty);
        assert!(
            ui.view()
                .activity
                .receive_gap_note(at + Duration::from_secs(31))
                .is_some()
        );
        ui.apply(receive("t", generation, "attempt"));
        assert!(
            ui.dirty,
            "receipt clears the visible silence hint immediately"
        );
        assert_eq!(
            ui.view()
                .activity
                .receive_gap_note(at + Duration::from_secs(31)),
            None
        );
        assert_eq!(ui.view().history_revision(), revision);
        assert!(Arc::ptr_eq(&layout, &ui.history_layout(80, 20)));
        assert!(ui.take_deferred_actions().is_empty());
        ui.select_thread("other".into());
        ui.select_thread("t".into());
        ui.apply(receive("t", generation, "attempt"));
        assert_eq!(
            ui.view()
                .activity
                .receive_gap_note(at + Duration::from_secs(999)),
            None
        );
    }

    #[test]
    fn activity_clock_preserves_history_and_rejects_stale_updates() {
        use activity::{ActivityPhase as Phase, ActivitySnapshot};
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        history(&mut ui, "t", "unchanged history");
        let layout = ui.history_layout(80, 20);
        let revision = ui.view().history_revision();
        let generation = ui.view().generation;
        let snapshot = ActivitySnapshot {
            context_reduction: None,
            automatic_retry: None,
            recovery: None,
            run_id: Some("run".into()),
            turn_id: Some("turn".into()),
            attempt_id: Some("attempt".into()),
            phase: Phase::AwaitingModel,
            turn_in_progress: true,
        };
        let update = |generation, epoch, snapshot| UiUpdate::Activity {
            thread_id: "t".into(),
            generation,
            epoch,
            snapshot,
            observed_at: Instant::now(),
        };
        ui.apply(update(generation, 2, snapshot.clone()));
        ui.dirty = false;
        ui.apply(update(generation, 2, snapshot.clone()));
        assert!(!ui.dirty, "identical facts do not invalidate the screen");
        let now = Instant::now();
        ui.animation_tick(now);
        assert!(ui.dirty);
        ui.dirty = false;
        ui.animation_tick(now + Duration::from_millis(50));
        assert!(!ui.dirty);
        ui.animation_tick(now + Duration::from_millis(100));
        assert!(ui.dirty);
        assert_eq!(ui.view().history_revision(), revision);
        assert!(Arc::ptr_eq(&layout, &ui.history_layout(80, 20)));
        assert!(ui.take_deferred_actions().is_empty());
        ui.help_visible = true;
        ui.dirty = false;
        ui.animation_tick(now + Duration::from_secs(1));
        assert!(!ui.dirty, "overlay pauses animation invalidation");
        ui.help_visible = false;
        ui.selection_mode = true;
        ui.animation_tick(now + Duration::from_secs(2));
        assert!(!ui.dirty, "selection pauses animation invalidation");
        ui.selection_mode = false;
        let completed = ActivitySnapshot {
            phase: Phase::Completed,
            turn_in_progress: false,
            ..snapshot.clone()
        };
        ui.apply(update(generation, 3, completed));
        ui.dirty = false;
        ui.apply(update(generation, 2, snapshot.clone()));
        ui.apply(update(generation + 1, 4, snapshot));
        ui.animation_tick(now + Duration::from_secs(3));
        assert!(
            !ui.dirty,
            "terminal state and rejected stale facts stay quiet"
        );
        assert_eq!(ui.view().activity.phase(), Phase::Completed);
        ui.select_thread("other".into());
        ui.dirty = false;
        ui.animation_tick(now + Duration::from_secs(4));
        assert!(!ui.dirty, "new selection never inherits the old animation");
    }

    #[test]
    fn observation_loss_freezes_only_the_matching_view_without_reversing_a_displayed_second() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let generation = ui.view().generation;
        let at = Instant::now();
        ui.apply(UiUpdate::Activity {
            thread_id: "t".into(),
            generation,
            epoch: 2,
            observed_at: at,
            snapshot: activity::ActivitySnapshot {
                run_id: Some("r".into()),
                turn_id: Some("u".into()),
                phase: activity::ActivityPhase::Running,
                turn_in_progress: true,
                ..Default::default()
            },
        });
        ui.animation_tick(at + Duration::from_secs(100));
        assert_eq!(ui.view().activity.displayed_elapsed_ms(), Some(100_000));
        for (generation, epoch) in [(generation + 1, 2), (generation, 1)] {
            ui.apply(UiUpdate::ActivityUnavailable {
                thread_id: "t".into(),
                generation,
                epoch,
            });
            assert_eq!(
                ui.view().activity.elapsed_ms(at + Duration::from_secs(101)),
                Some(101_000)
            );
        }
        ui.apply(UiUpdate::ActivityUnavailable {
            thread_id: "t".into(),
            generation,
            epoch: 2,
        });
        ui.animation_tick(at + Duration::from_secs(900));
        assert_eq!(ui.view().activity.displayed_elapsed_ms(), Some(100_000));
        assert_eq!(
            ui.view().activity.phase(),
            activity::ActivityPhase::Running,
            "losing an observation does not fail the actual Run"
        );
    }

    #[test]
    fn unchanged_visible_progress_does_not_invalidate_the_screen() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let update = |tool_calls| UiUpdate::Progress {
            thread_id: "t".into(),
            tool_calls,
            cancelled_calls: 0,
            state: "モデル応答待ち".into(),
        };
        ui.apply(update(2));
        ui.dirty = false;
        ui.apply(update(2));
        assert!(!ui.dirty);
        ui.apply(update(3));
        assert!(ui.dirty, "a changed visible count still updates");
    }

    #[test]
    fn draft_and_status_reuse_layout_but_same_length_source_and_reload_invalidate() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        history(&mut ui, "t", "aaaa 日本語");
        let first = ui.history_layout(24, 8);
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        ui.apply(UiUpdate::Status {
            thread_id: Some("t".into()),
            text: "status only".into(),
        });
        let again = ui.history_layout(24, 8);
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(ui.layout_cache.builds, 1);
        let generation = ui.view().generation;
        let view_revision = ui.view().view_revision;
        ui.apply(UiUpdate::NodeWindow {
            thread_id: "t".into(),
            generation,
            view_revision,
            node: node("n", "bbbb 日本語"),
        });
        let replaced = ui.history_layout(24, 8);
        assert!(!Arc::ptr_eq(&first, &replaced));
        assert!(replaced.text.contains("bbbb 日本語"));
        assert!(!replaced.text.contains("aaaa"));
        ui.apply(UiUpdate::NodeWindow {
            thread_id: "t".into(),
            generation,
            view_revision: view_revision + 1,
            node: node("n", "stale window"),
        });
        assert!(Arc::ptr_eq(&replaced, &ui.history_layout(24, 8)));
        ui.view_mut().nodes_mut_for_test()[0].evict_body();
        let evicted = ui.history_layout(24, 8);
        assert!(evicted.text.contains("本文未キャッシュ"));
        ui.apply(UiUpdate::NodeWindow {
            thread_id: "t".into(),
            generation,
            view_revision,
            node: node("n", "cccc 日本語"),
        });
        assert!(ui.history_layout(24, 8).text.contains("cccc 日本語"));
    }
    #[test]
    fn anchor_fold_dimensions_and_cross_thread_switch_have_distinct_layouts() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("a".into());
        history(&mut ui, "a", "hidden 日本語 visible tail words words words");
        ui.view_mut().nodes_mut_for_test()[0].reasoning_ranges = vec![(0, 10)];
        let a = ui.history_layout(20, 8);
        assert_eq!(
            a.lines,
            Paragraph::new(presentation::borrowed(&a.styled))
                .wrap(Wrap { trim: false })
                .line_count(20)
        );
        ui.active_view_mut().reasoning_expanded.insert("n".into());
        let expanded = ui.history_layout(20, 8);
        assert!(!Arc::ptr_eq(&a, &expanded));
        assert!(expanded.text.contains("hidden 日本語"));
        assert!(!Arc::ptr_eq(&expanded, &ui.history_layout(21, 8)));
        assert!(Arc::ptr_eq(&expanded, &ui.history_layout(20, 9)));
        ui.active_view_mut().follow = false;
        ui.active_view_mut().anchor = ViewAnchor {
            node_id: Some("n".into()),
            char_offset: 11,
        };
        let anchored = ui.history_layout(20, 8);
        assert!(
            Arc::ptr_eq(&expanded, &anchored),
            "source navigation reuses the wrapped chunk"
        );
        let anchor = ui.view().anchor.clone();
        ui.select_thread("b".into());
        history(&mut ui, "b", "thread b");
        let b = ui.history_layout(20, 8);
        assert!(b.text.contains("thread b"));
        ui.select_thread("a".into());
        assert_eq!(ui.view().anchor, anchor);
        assert!(Arc::ptr_eq(&anchored, &ui.history_layout(20, 8)));
    }
    #[test]
    fn layout_global_lru_zero_oversize_and_eviction_release_owned_buffers() {
        let ui = UiState::new(&Config::default(), std::env::temp_dir());
        let key = |name: &str| HistoryLayoutKey::new(name, ui.view(), 20, 8);
        let value = || {
            Arc::new(HistoryLayout {
                text: "small".into(),
                styled: vec![],
                targets: vec![],
                sources: vec![],
                lines: 1,
            })
        };
        let mut cache = HistoryLayoutCache::default();
        cache.insert(key("a"), value(), usize::MAX);
        let one = cache.bytes;
        cache.insert(key("b"), value(), 2 * one);
        let owned = cache.get(&key("a")).unwrap();
        let weak = Arc::downgrade(&owned);
        cache.insert(key("c"), value(), 2 * one);
        assert!(cache.get(&key("b")).is_none());
        assert!(cache.get(&key("a")).is_some());
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.bytes <= 2 * one);
        cache.evict(0);
        assert_eq!(cache.bytes, 0);
        assert!(cache.entries.is_empty());
        assert!(
            weak.upgrade().is_some(),
            "current frame can finish after cache eviction"
        );
        drop(owned);
        assert!(weak.upgrade().is_none());
        cache.insert(key("d"), value(), 0);
        cache.insert(key("e"), value(), 1);
        assert!(cache.entries.is_empty());
    }
    #[test]
    fn body_budget_excludes_working_nodes_and_evicts_oldest_inactive_first() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("old".into());
        history(&mut ui, "old", &"a".repeat(512));
        let old = ui.active.clone();
        ui.select_thread("recent".into());
        history(&mut ui, "recent", &"b".repeat(512));
        let recent = ui.active.clone();
        ui.select_thread("active".into());
        *ui.view_mut().nodes_mut_for_test() = vec![
            node("x", &"x".repeat(8192)),
            node("y", &"y".repeat(8192)),
            node("z", &"z".repeat(8192)),
        ];
        let retained = retained_body_bytes(&ui.views[&old], &[])
            + retained_body_bytes(&ui.views[&recent], &[]);
        ui.config.cache.body_bytes = retained as u64;
        ui.trim_bodies();
        assert_eq!(
            ui.views[&old].nodes()[0].text.len(),
            512,
            "active working bodies cannot evict budget-fitting history"
        );
        ui.config.cache.body_bytes = (retained - 512) as u64;
        ui.trim_bodies();
        assert!(ui.views[&old].nodes()[0].text.is_empty());
        assert_eq!(ui.views[&recent].nodes()[0].text.len(), 512);
        assert!(ui.view().nodes().iter().all(|n| n.text.len() == 8192));
    }
    #[test]
    fn projection_work_and_row_metadata_are_bounded_independently_of_retained_history() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        *ui.view_mut().nodes_mut_for_test() = (0..32)
            .map(|i| node(&format!("n{i}"), &"word ".repeat(12_800)))
            .collect();
        ui.view_mut().total_nodes = 32;
        let layout = ui.history_layout(80, 20);
        let indexes = layout
            .sources
            .iter()
            .map(|r| r.node)
            .collect::<BTreeSet<_>>();
        assert!(indexes.len() <= 3);
        assert!(
            indexes
                .iter()
                .map(|i| ui.view().nodes()[*i].text.len())
                .sum::<usize>()
                <= 192 * 1024
        );
        assert!(
            layout.sources.capacity() * std::mem::size_of::<presentation::RowSource>()
                <= ui.layout_cache.bytes
        );
        let weak = Arc::downgrade(&layout);
        ui.layout_cache.evict(0);
        drop(layout);
        assert!(
            weak.upgrade().is_none(),
            "row maps share the evicted layout's lifetime"
        );
    }

    #[test]
    fn row_scrolling_across_short_nodes_reuses_a_stable_projection_focus() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        *ui.view_mut().nodes_mut_for_test() = (0..20)
            .map(|i| node(&format!("n{i}"), &format!("row {i}")))
            .collect();
        ui.view_mut().total_nodes = 20;
        let first = ui.history_layout(20, 8);
        ui.viewport_row(&first, 20, 8);
        assert!(ui.scroll(-3).is_empty());
        let next = ui.history_layout(20, 8);
        assert!(Arc::ptr_eq(&first, &next));
        assert_ne!(ui.view().anchor.node_id.as_deref(), Some("n19"));
        assert_eq!(ui.view().scroll.focus.as_deref(), Some("n19"));
    }
    #[test]
    fn pending_wheel_reply_at_coalesced_event_tail_finishes_without_nested_scroll() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let mut first = node("event-0", "context changed");
        first.presentation.kind = crate::presentation::PresentationKind::Event;
        first.presentation.title = "状態変更".into();
        let mut second = first.clone();
        second.node_id = "event-1".into();
        ui.apply(UiUpdate::History {
            thread_id: "t".into(),
            generation: ui.view().generation,
            nodes: vec![first, second.clone()],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 2,
        });
        let layout = ui.history_layout(80, 16);
        ui.viewport_row(&layout, 80, 16);
        assert_eq!(layout.sources.last().unwrap().node, 0);
        assert_eq!(layout.sources.last().unwrap().covered_end, 2);
        let generation = ui.view().generation;
        let view_revision = ui.view().view_revision;
        let view = ui.active_view_mut();
        view.loading = true;
        view.scroll.pending = 3;
        view.scroll.request = Some((view_revision, Some(second.node_id.clone())));
        ui.apply(UiUpdate::NodeWindow {
            thread_id: "t".into(),
            generation,
            view_revision,
            node: second,
        });
        assert!(!ui.view().loading);
        assert!(ui.view().follow);
        assert_eq!(ui.view().scroll.pending, 0);
        assert!(ui.view().scroll.request.is_none());
        assert!(ui.take_deferred_actions().is_empty());
    }
}

#[cfg(test)]
mod recovery_context_tests {
    use super::*;
    #[test]
    fn past_management_reference_does_not_change_current_recovery_or_normal_operation_context() {
        let mut ui = UiState::new(&Config::default(), PathBuf::from("/tmp"));
        ui.select_thread("current-thread".into());
        ui.view_mut().run_id = Some("stale-view-run".into());
        ui.apply(UiUpdate::Activity {
            thread_id: "current-thread".into(),
            generation: ui.view().generation,
            epoch: 1,
            observed_at: Instant::now(),
            snapshot: activity::ActivitySnapshot {
                run_id: Some("current-run".into()),
                turn_id: Some("current-turn".into()),
                phase: activity::ActivityPhase::Recovery,
                ..Default::default()
            },
        });
        ui.apply(UiUpdate::StructuredManagement {
            work_status: false,
            request_id: 1,
            title: "古い照会結果".into(),
            omitted: false,
            fields: vec![PresentationField {
                value_omitted: false,
                display: None,
                key: "/run_id".into(),
                label: "Run ID".into(),
                value: "past-run".into(),
            }],
        });
        let current = ui.operation_context(false);
        assert_eq!(current.thread, "current-thread");
        assert_eq!(current.run, "current-run");
        let reference = ui.operation_context(true);
        assert_eq!(reference.run, "past-run");
        assert_eq!(reference.current_run, "current-run");
        ui.apply(UiUpdate::Activity {
            thread_id: "current-thread".into(),
            generation: ui.view().generation,
            epoch: 2,
            observed_at: Instant::now(),
            snapshot: activity::ActivitySnapshot::default(),
        });
        assert!(ui.operation_context(false).current_run.is_empty());
        assert!(ui.operation_context(true).current_run.is_empty());
        ui.management_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.management.structured().is_none());
    }
}

#[cfg(test)]
mod tool_refresh_tests {
    use super::*;
    fn tool(state: &str) -> UiNode {
        let payload = serde_json::json!({"output":[{"type":"function_call","name":"shell","call_id":"call","arguments":"{\"command\":\"sleep 30\"}"}]});
        let text = crate::history::text_of_payload(&payload);
        let mut node = UiNode {
            node_id: "call-node".into(),
            role: "assistant".into(),
            text: text.clone(),
            base_char_offset: 500,
            total_chars: 1000,
            reasoning_ranges: vec![],
            presentation: crate::presentation::project("assistant_response", &payload, &text, 0),
        };
        node.presentation.blocks[0].source_start = 500;
        node.presentation.blocks[0].source_end = 500 + text.chars().count();
        node.presentation.blocks[0]
            .tool
            .as_mut()
            .unwrap()
            .current_state = Some(state.into());
        node.presentation.blocks[0]
            .tool
            .as_mut()
            .unwrap()
            .current_exit_code = match state {
            "exited" => Some(7),
            "stopped" => Some(0),
            _ => None,
        };
        node
    }
    fn history(ui: &mut UiState, nodes: Vec<UiNode>) {
        ui.apply(UiUpdate::History {
            thread_id: "t".into(),
            generation: ui.view().generation,
            nodes,
            before_cursor: None,
            append: false,
            first_ordinal: 50,
            selection_revision: 0,
            total_nodes: 60,
        });
    }
    #[test]
    fn pinned_tool_changes_only_metadata_and_refresh_requests_coalesce_without_source_loss() {
        let mut ui = UiState::new(&Config::default(), PathBuf::from("/"));
        ui.select_thread("t".into());
        let original = tool("background_current");
        let view = ui.view_mut();
        *view.nodes_mut_for_test() = vec![original.clone()];
        view.follow = false;
        view.anchor = ViewAnchor {
            node_id: Some("call-node".into()),
            char_offset: 505,
        };
        let anchor = ui.view().anchor.clone();
        history(&mut ui, vec![tool("exited")]);
        let node = &ui.view().nodes()[0];
        assert_eq!(node.text, original.text);
        assert_eq!(node.base_char_offset, 500);
        assert_eq!(node.presentation.blocks[0].source_start, 500);
        assert_eq!(ui.view().anchor, anchor);
        assert_eq!(
            node.presentation.blocks[0]
                .tool
                .as_ref()
                .unwrap()
                .current_state
                .as_deref(),
            Some("exited")
        );
        let requests = ui.take_deferred_actions();
        let [
            UiAction::RefreshTools {
                generation,
                revision,
                nodes,
                ..
            },
        ] = requests.as_slice()
        else {
            panic!("{requests:?}")
        };
        assert!(nodes[0].text.is_empty());
        assert!(nodes[0].presentation.fields.is_empty());
        history(&mut ui, vec![]);
        assert!(ui.take_deferred_actions().is_empty());
        ui.apply(UiUpdate::ToolFacts {
            thread_id: "t".into(),
            generation: *generation,
            revision: revision + 99,
            result: Ok(vec![tool("running_current")]),
        });
        assert_eq!(
            ui.view().nodes()[0].presentation.blocks[0]
                .tool
                .as_ref()
                .unwrap()
                .current_state
                .as_deref(),
            Some("exited")
        );
        ui.apply(UiUpdate::ToolFacts {
            thread_id: "t".into(),
            generation: *generation,
            revision: *revision,
            result: Ok(vec![tool("stopped")]),
        });
        assert_eq!(
            ui.view().nodes()[0].presentation.blocks[0]
                .tool
                .as_ref()
                .unwrap()
                .current_state
                .as_deref(),
            Some("stopped")
        );
        let next = ui.take_deferred_actions();
        assert_eq!(
            next.len(),
            1,
            "one coalesced reread observes changes while the first was pending"
        );
        ui.action_rejected(&next[0]);
        assert!(ui.view().tool_refresh.is_none());
        let tool = ui.view().nodes()[0].presentation.blocks[0]
            .tool
            .as_ref()
            .unwrap();
        assert_eq!(
            tool.current_state.as_deref(),
            Some("observation_unavailable")
        );
        assert_eq!(tool.current_exit_code, None);
        assert!(tool.heading().contains("現在状態は未確認"));
        assert_eq!(ui.view().nodes()[0].text, original.text);
        assert_eq!(ui.view().anchor, anchor);
    }
}

#[cfg(test)]
mod management_tests;

#[cfg(test)]
mod reader_tests;
#[cfg(test)]
mod thread_view_boundary_tests {
    use super::*;
    fn node(id: &str, text: &str) -> UiNode {
        UiNode {
            node_id: id.into(),
            role: "assistant".into(),
            text: text.into(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: Default::default(),
        }
    }
    fn history(ui: &mut UiState, text: &str) {
        ui.apply(UiUpdate::History {
            thread_id: "old".into(),
            generation: ui.view().generation,
            nodes: vec![node("saved", text)],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        });
    }
    #[test]
    fn draft_status_and_source_anchor_edits_reuse_layout_but_source_and_width_do_not() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("old".into());
        history(&mut ui, "original source paragraphs for visual anchors");
        let layout = ui.history_layout(40, 8);
        let revision = ui.view().history_revision();
        ui.view_mut().draft.text = "unsent".into();
        ui.view_mut().status = "local status".into();
        ui.view_mut().save_retry = true;
        // The 250ms loop performs exactly this after SaveDraft queue admission.
        assert!(matches!(
            ui.persist_active().as_slice(),
            [UiAction::SaveDraft { .. }]
        ));
        ui.view_mut().save_retry = false;
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 5,
        };
        assert_eq!(ui.view().history_revision(), revision);
        assert!(Arc::ptr_eq(&layout, &ui.history_layout(40, 8)));
        assert_eq!(ui.layout_cache.builds, 1);
        let resized = ui.history_layout(20, 8);
        assert!(!Arc::ptr_eq(&layout, &resized));
        assert_eq!(ui.view().anchor.char_offset, 5);
        ui.view_mut().follow = true;
        history(&mut ui, "changed retained source");
        let changed = ui.history_layout(40, 8);
        assert!(!Arc::ptr_eq(&layout, &changed));
        assert!(changed.text.contains("changed retained source"));
        ui.view_mut().selection_revision += 1;
        assert!(!Arc::ptr_eq(&changed, &ui.history_layout(40, 8)));
        assert!(ui.layout_cache.bytes <= ui.config.cache.layout_bytes as usize);
    }
    #[test]
    fn background_stream_commit_keeps_temporary_source_until_its_owner_is_rebased() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("old".into());
        let owner = ui.active.clone();
        let generation = ui.view().generation;
        ui.apply(UiUpdate::Stream {
            thread_id: "old".into(),
            generation,
            node_id: "attempt".into(),
            role: "assistant".into(),
            text: "partial source".into(),
            base_char_offset: 0,
            total_chars: 14,
            reasoning_ranges: vec![],
            presentation: Default::default(),
            ack: None,
        });
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("attempt".into()),
            char_offset: 7,
        };
        ui.select_thread("current".into());
        let current_anchor = ui.view().anchor.clone();
        ui.take_deferred_actions();
        let commit = |generation| UiUpdate::StreamCommitted {
            thread_id: "old".into(),
            generation,
            attempt_id: "attempt".into(),
            node_id: "saved".into(),
        };
        ui.apply(commit(generation + 1));
        assert_eq!(ui.views[&owner].nodes()[0].node_id, "attempt");
        ui.apply(commit(generation));
        let old = &ui.views[&owner];
        assert_eq!(old.nodes()[0].node_id, "attempt");
        assert_eq!(old.anchor.node_id.as_deref(), Some("attempt"));
        assert_eq!(old.anchor.char_offset, 7);
        assert!(old.provisional_nodes.contains("attempt"));
        assert_eq!(ui.view().thread_id.as_deref(), Some("current"));
        assert_eq!(ui.view().anchor, current_anchor);
        assert!(
            matches!(ui.take_deferred_actions().as_slice(), [UiAction::RebaseStream(request)] if request.thread_id == "old" && request.generation == generation && request.node_id == "saved" && request.anchor.char_offset == 7 && request.source.text == "partial source")
        );
        ui.apply(commit(generation));
        assert!(ui.take_deferred_actions().is_empty());
        let revision = ui.views[&owner].view_revision;
        ui.apply(UiUpdate::HistoryWindow {
            thread_id: "old".into(),
            generation,
            view_revision: revision,
            nodes: vec![node("saved", "fully saved source")],
            first_ordinal: 3,
            selection_revision: 2,
            total_nodes: 4,
            anchor: ViewAnchor {
                node_id: Some("saved".into()),
                char_offset: 7,
            },
        });
        assert!(!ui.views[&owner].loading);
        assert!(!ui.views[&owner].history_rebase_pending);
        assert!(ui.views[&owner].scroll.request.is_none());
        assert_eq!(ui.views[&owner].first_ordinal, 3);
        assert_eq!(ui.view().anchor, current_anchor);
        assert_eq!(ui.view().thread_id.as_deref(), Some("current"));
    }
    #[test]
    fn background_invalidation_checks_its_generation_and_view_revision() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("old".into());
        history(&mut ui, "saved source");
        let owner = ui.active.clone();
        let generation = ui.view().generation;
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 3,
        };
        ui.select_thread("current".into());
        let revision = ui.views[&owner].view_revision;
        let invalidate = |generation, revision| UiUpdate::HistoryInvalidated {
            thread_id: "old".into(),
            generation,
            view_revision: Some(revision),
        };
        ui.take_deferred_actions();
        ui.apply(invalidate(generation + 1, revision));
        ui.apply(invalidate(generation, revision + 1));
        assert!(!ui.views[&owner].history_rebase_pending);
        assert!(ui.take_deferred_actions().is_empty());
        ui.apply(invalidate(generation, revision));
        assert!(ui.views[&owner].history_rebase_pending);
        assert!(!ui.view().history_rebase_pending);
        assert!(
            matches!(ui.take_deferred_actions().as_slice(), [UiAction::RebaseHistory { thread_id, generation: actual, .. }] if thread_id == "old" && *actual == generation)
        );
        ui.apply(invalidate(generation, revision));
        assert!(ui.take_deferred_actions().is_empty());
    }
}

#[cfg(test)]
mod performance_stream;

#[cfg(test)]
mod stream_performance;
