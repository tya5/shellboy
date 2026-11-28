//! Selected-history pages, positioned windows and node hydration from the
//! native owner: one fenced window observation plus positioned display reads.
use super::native_history::{Owner, SelectedWindow};
use super::native_page::KnownSubmission;
use super::*;
use crate::protocol::{HistoryDisplayPosition, HistoryWindowArgs};

pub(super) struct HistoryReader<'a> {
    owner: Owner<'a>,
    language: UiLanguage,
    updates: &'a dyn UpdateTarget,
    width: u16,
    known: Vec<KnownSubmission>,
}
pub(super) struct Reply {
    thread: String,
    generation: u64,
}
impl Reply {
    pub(super) fn new(thread: impl Into<String>, generation: u64) -> Self {
        Self {
            thread: thread.into(),
            generation,
        }
    }
    pub(super) async fn failed(&self, updates: &dyn UpdateTarget, error: ApiError) {
        load_failed(updates, self.thread.clone(), self.generation, error).await;
    }
}
pub(super) struct WindowReply {
    target: Reply,
    view_revision: u64,
}
impl WindowReply {
    pub(super) fn new(thread: impl Into<String>, generation: u64, view_revision: u64) -> Self {
        Self {
            target: Reply::new(thread, generation),
            view_revision,
        }
    }
    pub(super) async fn failed(&self, updates: &dyn UpdateTarget, error: ApiError) {
        load_failed_at(
            updates,
            self.target.thread.clone(),
            self.target.generation,
            Some(self.view_revision),
            error,
        )
        .await;
    }
    pub(super) async fn deliver_node(&self, updates: &dyn UpdateTarget, node: UiNode) {
        update(
            updates,
            UiUpdate::NodeWindow {
                thread_id: self.target.thread.clone(),
                generation: self.target.generation,
                view_revision: self.view_revision,
                node,
            },
        )
        .await;
    }
    async fn deliver(&self, updates: &dyn UpdateTarget, page: Page, anchor: ViewAnchor) {
        let Page { nodes, info, .. } = page;
        stream_commits(
            updates,
            &self.target.thread,
            self.target.generation,
            info.committed,
        )
        .await;
        update(
            updates,
            UiUpdate::HistoryWindow {
                thread_id: self.target.thread.clone(),
                generation: self.target.generation,
                view_revision: self.view_revision,
                nodes,
                pending: info.pending,
                read_revision: info.read_revision,
                first_ordinal: info.first,
                selection_revision: info.revision,
                total_nodes: info.total,
                anchor,
            },
        )
        .await;
    }
}
pub(super) struct WindowRequest {
    ordinal: u64,
    position: NodePosition,
    expected_revision: u64,
}
impl WindowRequest {
    pub(super) fn new(ordinal: u64, offset: usize, from_end: bool, expected_revision: u64) -> Self {
        Self {
            ordinal,
            position: if from_end {
                NodePosition::FromEnd(offset)
            } else {
                NodePosition::At(offset)
            },
            expected_revision,
        }
    }
}
pub(super) struct HistoryInfo {
    pub(super) first: u64,
    pub(super) revision: u64,
    pub(super) total: u64,
    pub(super) committed: Vec<(String, String)>,
    pub(super) pending: PendingMessages,
    /// The owner's observation admission order for this selected window.
    pub(super) read_revision: u64,
}

/// A page cap shared with the window positions the TUI requests.
const PAGE_NODES: u32 = 50;

/// Where a window is anchored: the path's end, one exact ordinal, or a node.
pub(super) enum Anchor {
    Tail,
    Ordinal(u64),
    Node(String),
}

pub(super) fn window_args(
    thread: &str,
    anchor: Anchor,
    before: u32,
    after: u32,
    observe_node_ids: Vec<String>,
) -> HistoryWindowArgs {
    let (ordinal, node_id) = match anchor {
        Anchor::Tail => (None, None),
        Anchor::Ordinal(ordinal) => (Some(ordinal), None),
        Anchor::Node(node) => (None, Some(node)),
    };
    HistoryWindowArgs {
        thread_id: thread.to_owned(),
        include_observation: true,
        observe_node_ids,
        items: false,
        ordinal,
        node_id,
        expect_node_id: None,
        before,
        after,
        excerpt_chars: 0,
    }
}

/// One projected selected page with the facts and queue it was read with.
pub(super) struct Page {
    pub(super) nodes: Vec<UiNode>,
    pub(super) info: HistoryInfo,
    pub(super) anchor: Option<ViewAnchor>,
    pub(super) observation: crate::protocol::HistoryWindowObservation,
}

fn display_position(position: NodePosition) -> HistoryDisplayPosition {
    match position {
        NodePosition::At(offset) => HistoryDisplayPosition::Absolute {
            offset: offset as u64,
        },
        NodePosition::FromEnd(offset) => HistoryDisplayPosition::FromEnd {
            offset: offset as u64,
        },
    }
}

/// Read and project one selected window. `wanted` names the node whose
/// extent follows a source position; every other node shows its tail.
#[allow(clippy::too_many_arguments)]
pub(super) async fn read_page(
    language: UiLanguage,
    owner: &Owner<'_>,
    width: u16,
    known: &[KnownSubmission],
    args: HistoryWindowArgs,
    expected_revision: Option<u64>,
    keep: impl FnOnce(&[crate::protocol::HistorySnippet]) -> std::ops::Range<usize>,
    wanted: impl Fn(&crate::protocol::HistorySnippet) -> Option<NodePosition>,
) -> Result<Page, ApiError> {
    let thread = args.thread_id.clone();
    let window = owner
        .selected(args, expected_revision, keep, |source| {
            wanted(source).map_or(HistoryDisplayPosition::Tail, display_position)
        })
        .await?;
    project_page(language, owner, width, known, &thread, window, wanted).await
}

#[allow(clippy::too_many_arguments)]
async fn project_page(
    language: UiLanguage,
    owner: &Owner<'_>,
    width: u16,
    known: &[KnownSubmission],
    thread: &str,
    window: SelectedWindow,
    wanted: impl Fn(&crate::protocol::HistorySnippet) -> Option<NodePosition>,
) -> Result<Page, ApiError> {
    let SelectedWindow {
        revision,
        total,
        observation,
        nodes,
    } = window;
    let pending = super::native_page::queue(owner, thread, &observation.pending, known).await?;
    let first = nodes
        .first()
        .map_or(0, |node| node.source.selection.ordinal);
    let anchor = nodes.iter().find_map(|node| {
        wanted(&node.source).map(|position| ViewAnchor {
            node_id: Some(node.source.node_id.clone()),
            char_offset: position.offset(node.display.total_chars as usize),
        })
    });
    let projected = presentation_work(move || {
        nodes
            .into_iter()
            .map(|node| {
                node.display
                    .into_ui_node(node.source.node_id, width, language)
            })
            .collect::<Vec<_>>()
    })
    .await?;
    let (mut projected, observation) = presentation_work(move || {
        let mut projected = projected;
        native_history::apply_observation(&mut projected, &observation, language);
        (projected, observation)
    })
    .await?;
    streams::seal_nodes(&mut projected).await?;
    stamp_sources(&mut projected, observation.read_sequence);
    let committed = observation
        .committed_attempts
        .iter()
        .filter(|binding| projected.iter().any(|node| node.node_id == binding.node_id))
        .map(|binding| (binding.attempt_id.clone(), binding.node_id.clone()))
        .collect();
    Ok(Page {
        nodes: projected,
        info: HistoryInfo {
            first,
            revision,
            total,
            committed,
            pending,
            read_revision: observation.read_sequence,
        },
        anchor,
        observation,
    })
}

/// A client-local continuation for the page before `first` at `revision`.
pub(super) fn before_cursor(
    thread: &str,
    first: u64,
    revision: u64,
) -> Result<Option<String>, ApiError> {
    (first > 0)
        .then(|| {
            PageCursor {
                version: 1,
                scope: "ui_history".into(),
                filter: thread.into(),
                last_sort_key: first.to_string(),
                watermark: 0,
                revision,
                encoding: None,
            }
            .encode()
        })
        .transpose()
}

fn decode_before(thread: &str, token: &str) -> Result<(u64, u64), ApiError> {
    use base64::Engine;
    let cursor = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PageCursor>(&bytes).ok())
        .filter(|cursor| {
            cursor.version == 1 && cursor.scope == "ui_history" && cursor.filter == thread
        })
        .ok_or_else(expired_history_cursor)?;
    let first = cursor
        .last_sort_key
        .parse::<u64>()
        .map_err(|_| expired_history_cursor())?;
    Ok((first, cursor.revision))
}

/// The selected Thread no longer exists for this client view.
fn unavailable(error: ApiError) -> ApiError {
    if error.code == ErrorCode::ThreadNotFound {
        ApiError::new(
            ErrorCode::TargetUnavailable,
            "The selected Thread is no longer available",
        )
    } else {
        error
    }
}

fn not_on_path(error: &ApiError) -> bool {
    error.code == ErrorCode::CursorExpired && error.message == "node is not on the selected path"
}

impl<'a> HistoryReader<'a> {
    pub(super) fn new(
        owner: Owner<'a>,
        language: UiLanguage,
        updates: &'a dyn UpdateTarget,
        width: u16,
    ) -> Self {
        Self {
            owner,
            language,
            updates,
            width,
            known: Vec::new(),
        }
    }
    /// Draft operations this client submitted to the page's Thread and has not
    /// yet seen settled; the installed queue settles them by exact receipt.
    pub(super) fn with_submissions(mut self, known: Vec<KnownSubmission>) -> Self {
        self.known = known;
        self
    }
    async fn page_at(
        &self,
        args: HistoryWindowArgs,
        expected_revision: Option<u64>,
        wanted: impl Fn(&crate::protocol::HistorySnippet) -> Option<NodePosition>,
    ) -> Result<Page, ApiError> {
        read_page(
            self.language,
            &self.owner,
            self.width,
            &self.known,
            args,
            expected_revision,
            |items| 0..items.len(),
            wanted,
        )
        .await
        .map_err(unavailable)
    }
    pub(super) async fn saved(
        owner: &Owner<'_>,
        language: UiLanguage,
        width: u16,
        known: Vec<KnownSubmission>,
        draft: (String, u64),
        generation: u64,
        value: &Value,
    ) -> Result<UiUpdate, ApiError> {
        let (draft_id, revision) = draft;
        let thread = value["thread_id"]
            .as_str()
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "Accepted input has no Thread"))?
            .to_owned();
        let page = read_page(
            language,
            owner,
            width,
            &known,
            window_args(&thread, Anchor::Tail, PAGE_NODES - 1, 0, Vec::new()),
            None,
            |items| 0..items.len(),
            |_| None,
        )
        .await
        .map_err(unavailable)?;
        let before_cursor = before_cursor(&thread, page.info.first, page.info.revision)?;
        let Page { nodes, info, .. } = page;
        let history = UiUpdate::History {
            thread_id: thread.clone(),
            generation,
            nodes,
            pending: info.pending,
            read_revision: info.read_revision,
            before_cursor,
            append: false,
            first_ordinal: info.first,
            selection_revision: info.revision,
            total_nodes: info.total,
        };
        Ok(UiUpdate::SavedMessage {
            draft_id,
            revision,
            generation,
            thread_id: thread,
            status: if value["delivery_status"] == "held" {
                ReceiptStatus::Held
            } else {
                ReceiptStatus::Accepted
            },
            history: Box::new(history),
        })
    }
    pub(super) async fn rebase(
        &self,
        reply: &WindowReply,
        anchor: ViewAnchor,
    ) -> Result<(), ApiError> {
        let thread = &reply.target.thread;
        let wanted_offset = anchor.char_offset;
        let anchored = match &anchor.node_id {
            Some(node) => {
                let target = node.clone();
                match self
                    .page_at(
                        window_args(
                            thread,
                            Anchor::Node(node.clone()),
                            10,
                            PAGE_NODES - 11,
                            Vec::new(),
                        ),
                        None,
                        move |source| {
                            (source.node_id == target).then_some(NodePosition::At(wanted_offset))
                        },
                    )
                    .await
                {
                    Err(error) if not_on_path(&error) => None,
                    result => Some(result?),
                }
            }
            None => None,
        };
        let page = match anchored {
            Some(page) => page,
            // The anchor left the selected path: show the latest page from its start.
            None => {
                let first = std::sync::OnceLock::new();
                self.page_at(
                    window_args(thread, Anchor::Tail, PAGE_NODES - 1, 0, Vec::new()),
                    None,
                    move |source| {
                        (first.get_or_init(|| source.node_id.clone()) == &source.node_id)
                            .then_some(NodePosition::At(0))
                    },
                )
                .await?
            }
        };
        let anchor = page.anchor.clone().unwrap_or_default();
        let received = received::read_native(&self.owner, &page, self.width, self.language).await?;
        reply.deliver(self.updates, page, anchor).await;
        update(
            self.updates,
            UiUpdate::IncompleteResponses {
                thread_id: reply.target.thread.clone(),
                generation: reply.target.generation,
                selection_revision: received.selection_revision,
                view_revision: Some(reply.view_revision),
                root_revision: received.root_revision,
                nodes: received.nodes,
            },
        )
        .await;
        Ok(())
    }
    pub(super) async fn window(
        &self,
        reply: &WindowReply,
        request: WindowRequest,
    ) -> Result<(), ApiError> {
        let WindowRequest {
            ordinal,
            position,
            expected_revision,
        } = request;
        let page = self
            .page_at(
                window_args(
                    &reply.target.thread,
                    Anchor::Ordinal(ordinal),
                    10,
                    PAGE_NODES - 11,
                    Vec::new(),
                ),
                Some(expected_revision),
                move |source| (source.selection.ordinal == ordinal).then_some(position),
            )
            .await
            .map_err(|error| {
                // Past the end of an unchanged path: the ordinal is gone. Past
                // the end of a changed path: the requested selection expired.
                let unchanged = error
                    .details
                    .as_ref()
                    .and_then(|details| details.get("revision"))
                    .and_then(Value::as_u64)
                    == Some(expected_revision);
                if error.code == ErrorCode::CursorExpired
                    && error.message == "position is past the end of the selected path"
                {
                    if unchanged {
                        ApiError::new(
                            ErrorCode::TargetUnavailable,
                            "Requested history ordinal is no longer selected",
                        )
                    } else {
                        expired_history_cursor()
                    }
                } else {
                    error
                }
            })?;
        let anchor = page.anchor.clone().ok_or_else(|| {
            ApiError::new(
                ErrorCode::TargetUnavailable,
                "Requested history ordinal is no longer selected",
            )
        })?;
        let received = received::read_native(&self.owner, &page, self.width, self.language).await?;
        reply.deliver(self.updates, page, anchor).await;
        update(
            self.updates,
            UiUpdate::IncompleteResponses {
                thread_id: reply.target.thread.clone(),
                generation: reply.target.generation,
                selection_revision: received.selection_revision,
                view_revision: Some(reply.view_revision),
                root_revision: received.root_revision,
                nodes: received.nodes,
            },
        )
        .await;
        Ok(())
    }
    pub(super) async fn page(
        &self,
        reply: &Reply,
        before: Option<String>,
        anchor: Option<ViewAnchor>,
    ) -> Result<(), ApiError> {
        let thread = &reply.thread;
        let append = before.is_some();
        let page = if let Some(token) = before {
            let (first, revision) = match decode_before(thread, &token) {
                Ok(cursor) => cursor,
                // An unavailable Thread outranks a continuation it cannot own.
                Err(expired) => {
                    self.owner
                        .window(window_args(thread, Anchor::Tail, 0, 0, Vec::new()))
                        .await
                        .map_err(unavailable)?;
                    return Err(expired);
                }
            };
            let last = first.checked_sub(1).ok_or_else(expired_history_cursor)?;
            self.page_at(
                window_args(thread, Anchor::Ordinal(last), PAGE_NODES - 1, 0, Vec::new()),
                Some(revision),
                |_| None,
            )
            .await?
        } else {
            let anchored = match anchor.as_ref().and_then(|anchor| anchor.node_id.clone()) {
                // The page is the last 50 nodes ending at most 49 after a
                // saved reading anchor.
                Some(node) => match read_page(
                    self.language,
                    &self.owner,
                    self.width,
                    &self.known,
                    window_args(
                        thread,
                        Anchor::Node(node.clone()),
                        PAGE_NODES - 1,
                        PAGE_NODES - 1,
                        Vec::new(),
                    ),
                    None,
                    |items| {
                        let at = items.iter().position(|item| item.node_id == node);
                        let end = at.map_or(items.len(), |at| {
                            (at + PAGE_NODES as usize).min(items.len())
                        });
                        end.saturating_sub(PAGE_NODES as usize)..end
                    },
                    |_| None,
                )
                .await
                .map_err(unavailable)
                {
                    Err(error) if not_on_path(&error) => None,
                    result => Some(result?),
                },
                None => None,
            };
            match anchored {
                Some(page) => page,
                None => {
                    self.page_at(
                        window_args(thread, Anchor::Tail, PAGE_NODES - 1, 0, Vec::new()),
                        None,
                        |_| None,
                    )
                    .await?
                }
            }
        };
        let before_cursor = before_cursor(thread, page.info.first, page.info.revision)?;
        let received = received::read_native(&self.owner, &page, self.width, self.language).await?;
        let Page { nodes, info, .. } = page;
        let selection_revision = info.revision;
        stream_commits(self.updates, thread, reply.generation, info.committed).await;
        update(
            self.updates,
            UiUpdate::History {
                thread_id: thread.clone(),
                generation: reply.generation,
                nodes,
                pending: info.pending,
                read_revision: info.read_revision,
                before_cursor,
                append,
                first_ordinal: info.first,
                selection_revision,
                total_nodes: info.total,
            },
        )
        .await;
        update(
            self.updates,
            UiUpdate::IncompleteResponses {
                thread_id: thread.to_owned(),
                generation: reply.generation,
                selection_revision: received.selection_revision,
                view_revision: None,
                root_revision: received.root_revision,
                nodes: received.nodes,
            },
        )
        .await;
        Ok(())
    }
    /// Bounded selected-item facts. No off-page body is installed into history
    /// and no shell output bytes are read. Each source projects from its owner
    /// display extent at offset 0; current execution facts are one saved read.
    pub(super) async fn links(
        &self,
        thread_id: &str,
        node_ids: Vec<String>,
    ) -> Result<DetailSourceSnapshot, ApiError> {
        if node_ids.is_empty()
            || node_ids.len() > 2
            || node_ids.iter().any(|id| id.is_empty() || id.len() > 256)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Detail source requires one or two bounded node IDs",
            ));
        }
        async {
            let owner = self.owner;
            let mut displays = Vec::with_capacity(node_ids.len());
            for id in &node_ids {
                use crate::protocol::HistoryDisplayNode as Node;
                let display = match owner
                    .display(id, None, HistoryDisplayPosition::Absolute { offset: 0 })
                    .await
                {
                    Ok(display) => Some(display).filter(|display| {
                        !matches!(display.outline.node, Node::Reclaimed { .. } | Node::HiddenAdmission)
                    }),
                    Err(error) if error.code == ErrorCode::TargetUnavailable => None,
                    Err(error) => return Err(error),
                };
                displays.push(display);
            }
            let observation = owner.window(HistoryWindowArgs {
                thread_id: thread_id.to_owned(), include_observation: true,
                observe_node_ids: node_ids.clone(), items: false, ordinal: None,
                node_id: None, expect_node_id: None, before: 0, after: 0, excerpt_chars: 0,
            }).await?.observation.ok_or_else(|| ApiError::new(ErrorCode::IoFailed, "Missing detail observation"))?;
            let read_revision = observation.read_sequence;
            let observed_at = chrono::DateTime::parse_from_rfc3339(&observation.observed_at)
                .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Invalid detail observation time"))?
                .with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S UTC").to_string();
            let current = node_ids.iter().map(|id| {
                let mut calls = observation.tools.calls.iter().filter(|call| &call.node_id == id).collect::<Vec<_>>();
                calls.sort_by_key(|call| (call.ordinal, &call.call_id));
                let truncated = calls.len() > 16 || observation.tools.limited_node_ids.contains(id);
                let fields = calls.into_iter().take(16).map(|call| (
                    (call.call_id.len() <= 512).then(|| call.call_id.clone()),
                    call.shell_state.as_ref().and(call.execution_id.clone()),
                    call.shell_state.clone(), call.shell_exit_code,
                    call.output_revision.map(|(_, complete)| complete),
                    call.retention_generation, call.has_output,
                )).collect::<Vec<_>>();
                (fields, truncated)
            }).collect::<Vec<_>>();
            let rows = node_ids
                .into_iter()
                .zip(displays)
                .zip(current)
                .map(|((id, display), current)| (id, display, current))
                .collect::<Vec<_>>();
            let width=self.width;
            let language = self.language;
            presentation_work(move || {
                use crate::ui::i18n::local;
                let mut sources=Vec::with_capacity(rows.len());
                for (node_id,row,(current,truncated)) in rows {
                    let unavailable = |reason: &str| DetailSourceFacts {
                        node_id:node_id.clone(),kind:Default::default(),fields:vec![],links:vec![],
                        unavailable_reason:Some(reason.into()),
                    };
                    let Some(display)=row else {
                        sources.push(unavailable(local!(language, "出典が削除・回収済み、または取得上限を超えています。O 原文で確認", "Source was deleted, collected, or exceeds the retrieval limit. Press O for the original")));
                        continue;
                    };
                    let mut node=display.into_ui_node(node_id.clone(),width,language);
                    let mut fields=Vec::new();
                    let field=|key:String,label:&str,value:String| crate::presentation::PresentationField {
                        key,label:label.into(),value,display:None,value_omitted:false,
                    };
                    fields.push(field("/ui/detail_preview_bounds".into(),local!(language, "詳細の取得範囲", "Detail retrieval limits"),crate::ui::text_width::glyphs::select(local!(language, "最大16呼出し・128項目・128リンク、既存metadata上限内 · O原文で確認", "Up to 16 calls, 128 fields and 128 links within metadata limits · O for original"), local!(language, "最大16呼出し・128項目・128リンク、既存metadata上限内 | O原文で確認", "Up to 16 calls, 128 fields and 128 links within metadata limits | O for original")).into()));
                    if truncated {
                        fields.push(field("/current_tools/truncated".into(),local!(language, "現在の実行情報", "Current execution details"),crate::ui::text_width::glyphs::select(local!(language, "先頭16呼出しまで · 詳細はCLIで確認", "First 16 calls · use CLI for details"), local!(language, "先頭16呼出しまで | 詳細はCLIで確認", "First 16 calls | use CLI for details")).into()));
                    }
                    for (index,(call,execution,state,code,complete,retention,has_output)) in current.into_iter().take(16).enumerate() {
                        let prefix=format!("/current_tools/{index}");
                        let mut call_field=field(format!("{prefix}/call_id"),local!(language, "呼出しID", "Call ID"),call.clone().unwrap_or_default());
                        if call.is_none() {call_field.display=Some(crate::ui::text_width::glyphs::select(local!(language, "IDが表示上限を超えています · O原文", "ID exceeds display limit · O for original"), local!(language, "IDが表示上限を超えています | O原文", "ID exceeds display limit | O for original")).into());call_field.value_omitted=true;}
                        fields.push(call_field);
                        fields.push(field(format!("{prefix}/execution_id"),local!(language, "実行ID", "Execution ID"),execution.unwrap_or_else(||local!(language, "未取得", "Not retrieved").into())));
                        fields.push(field(format!("{prefix}/state"),local!(language, "現在の実行状態", "Current execution state"),state.unwrap_or_else(||local!(language, "利用不可・未実行", "Unavailable or not started").into())));
                        fields.push(field(format!("{prefix}/exit_code"),local!(language, "現在の終了コード", "Current exit code"),code.map_or(local!(language, "未確定", "Not yet known").into(),|v|v.to_string())));
                        fields.push(field(format!("{prefix}/output_complete"),local!(language, "出力回収完了", "Output collection complete"),complete.map_or(local!(language, "未取得", "Not retrieved").into(),|v|v.to_string())));
                        fields.push(field(format!("{prefix}/retention_generation"),local!(language, "出力保持世代", "Output retention generation"),retention.map_or(local!(language, "未取得", "Not retrieved").into(),|v|v.to_string())));
                        fields.push(field(format!("{prefix}/output_available"),local!(language, "保存出力あり", "Saved output available"),has_output.to_string()));
                    }
                    fields.extend(node.presentation.fields.into_iter().take(128usize.saturating_sub(fields.len())));
                    node.presentation.blocks=Vec::new();
                    node.presentation.fields=fields;
                    node.presentation.links.truncate(128);
                    node.presentation.bound_metadata();
                    sources.push(DetailSourceFacts {node_id,kind:node.presentation.kind,fields:node.presentation.fields,
                        links:node.presentation.links,unavailable_reason:None});
                }
                Ok(DetailSourceSnapshot {read_revision,observed_at,sources})
            }).await?
        }.await
    }

    /// Hydrate one node at a source offset from the owner: its extent by ID and
    /// the selected window's facts for it, ordered by that observation.
    pub(super) async fn node(
        &self,
        node_id: String,
        offset: usize,
        viewed_thread: &str,
    ) -> Result<UiNode, ApiError> {
        async {
            let (display, observation) = self
                .owner
                .node(
                    viewed_thread,
                    &node_id,
                    HistoryDisplayPosition::Absolute {
                        offset: offset as u64,
                    },
                )
                .await
                .map_err(unavailable)?;
            let width = self.width;
            let language = self.language;
            let node =
                presentation_work(move || display.into_ui_node(node_id, width, language)).await?;
            let (mut node, read_sequence) = presentation_work(move || {
                let mut node = node;
                native_history::apply_observation(
                    std::slice::from_mut(&mut node),
                    &observation,
                    language,
                );
                (node, observation.read_sequence)
            })
            .await?;
            streams::seal_nodes(std::slice::from_mut(&mut node)).await?;
            stamp_source(&mut node, read_sequence);
            Ok(node)
        }
        .await
    }
}
// Enrichment can rebuild a presentation; freshness and eviction remain source facts.
fn stamp_source(node: &mut UiNode, read_revision: u64) {
    node.presentation.read_revision = read_revision;
    node.presentation.body_evicted = false;
}
pub(super) fn stamp_sources(nodes: &mut [UiNode], read_revision: u64) {
    for node in nodes {
        stamp_source(node, read_revision);
    }
}

#[cfg(test)]
mod detail_link_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detail_current_execution_and_availability_follow_saved_source_identity() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, receive) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
        owner.db.call(|db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');")?;
            for group in ["g","other"] {
                let attempt=format!("attempt-{group}");
                let node=format!("call-{group}");
                db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),?1,'u',0,0,'p','m',0,'completed','{}','now')",[&attempt])?;
                db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,'u','completed','now')",rusqlite::params![group,attempt])?;
                db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','assistant_response','now',?2)",rusqlite::params![node,json!({"group_id":group,"text":"[source](https://example.test/source)","output":[{"type":"function_call","call_id":"same","name":"shell","arguments":"{}"}]}).to_string()])?;
                db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,'same',0,'shell','{}','completed')",[group])?;
            }
            db.execute_batch("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('t',0,'call-g'),('t',1,'call-other');")?;
            db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('other',?1,1,'shell','{}','completed')",["x".repeat(200_000)])?;
            db.execute_batch("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','r','u','exec','running','/','{}','UTF-8','UTF-8','closed','now'); UPDATE tool_call SET operation_id='exec' WHERE group_id='g'; INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('exec',0,'stdout',0,X'616263','now');")?;
            Ok(())
        }).await.unwrap();
        let (updates, _) = std::sync::mpsc::sync_channel(8);
        let reader = HistoryReader::new(
            selected_owner(&control),
            control.store.config.ui.language,
            &updates,
            80,
        );
        let first = reader
            .links("t", vec!["call-g".into(), "call-other".into()])
            .await
            .unwrap();
        let get = |source: &DetailSourceFacts, key: &str| {
            source
                .fields
                .iter()
                .find(|f| f.key == key)
                .map(|f| f.value.clone())
        };
        assert_eq!(
            first.sources[0].kind,
            crate::presentation::PresentationKind::Assistant
        );
        let bounded_id = first.sources[1]
            .fields
            .iter()
            .find(|field| field.key == "/current_tools/1/call_id")
            .unwrap();
        assert!(bounded_id.value_omitted && bounded_id.value.is_empty());
        assert!(bounded_id.display.as_deref().unwrap().contains("limit"));
        assert_eq!(
            get(&first.sources[0], "/current_tools/0/state").as_deref(),
            Some("running")
        );
        assert_eq!(
            get(&first.sources[0], "/current_tools/0/output_available").as_deref(),
            Some("true")
        );
        assert_eq!(
            get(&first.sources[1], "/current_tools/0/execution_id").as_deref(),
            Some("Not retrieved")
        );
        owner.db.call(|db| {
            db.execute_batch("UPDATE shell_execution SET state='exited',exit_code=7,output_complete=1,retention_generation=1 WHERE id='exec'; DELETE FROM output_chunk WHERE execution_id='exec'; UPDATE node SET deleted_reason='gc',payload_json=NULL WHERE id='call-other';")?; Ok(())
        }).await.unwrap();
        let second = reader
            .links("t", vec!["call-g".into(), "call-other".into()])
            .await
            .unwrap();
        assert!(second.read_revision > first.read_revision);
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/state").as_deref(),
            Some("exited")
        );
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/exit_code").as_deref(),
            Some("7")
        );
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/output_available").as_deref(),
            Some("false")
        );
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/retention_generation").as_deref(),
            Some("1")
        );
        assert!(second.sources[1].unavailable_reason.is_some());
        assert!(second.sources[1].links.is_empty());
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offwindow_sources_are_bounded_and_unavailable_is_not_an_empty_link_list() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, receive) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
        owner.db.call(|db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/a','p','m','now')", [])?;
            let call=json!({"text":"[call source](https://example.test/call)"}).to_string();
            let result=json!({"item":{"type":"function_call_output","call_id":"call","output":json!({"status":"exited","exit_code":0,"output":"https://example.test/result"}).to_string()}}).to_string();
            for (id,kind,payload) in [("call","assistant_response",call),("result","tool_result",result),("large","assistant_response",json!({"text":"x".repeat(100_000)}).to_string())] {
                db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'now',?3,length(CAST(?3 AS BLOB)))",rusqlite::params![id,kind,payload])?;
            }
            db.execute("INSERT INTO node(id,kind,created_at,deleted_reason)VALUES('deleted','tool_result','now','gc')",[])?;
            Ok(())
        }).await.unwrap();
        let (updates, _incoming) = std::sync::mpsc::sync_channel(8);
        let reader = HistoryReader::new(
            selected_owner(&control),
            control.store.config.ui.language,
            &updates,
            80,
        );
        let snapshot = reader
            .links("t", vec!["call".into(), "result".into()])
            .await
            .unwrap();
        assert_eq!(snapshot.sources.len(), 2);
        let links = snapshot
            .sources
            .iter()
            .flat_map(|source| &source.links)
            .collect::<Vec<_>>();
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://example.test/call")
        );
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://example.test/result")
        );
        for ids in [vec![], vec!["call".into(); 3], vec!["x".repeat(257)]] {
            assert_eq!(
                reader.links("t", ids).await.unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
        // Positioned display pages have no whole-payload limit: a large source
        // is available; reclaimed and absent sources are not.
        let large = reader
            .links("t", vec!["call".into(), "large".into()])
            .await
            .unwrap();
        assert!(large.sources[1].unavailable_reason.is_none());
        for id in ["deleted", "absent"] {
            let snapshot = reader
                .links("t", vec!["call".into(), id.into()])
                .await
                .unwrap();
            assert_eq!(snapshot.sources[0].node_id, "call");
            assert!(snapshot.sources[0].unavailable_reason.is_none());
            let unavailable = &snapshot.sources[1];
            assert_eq!(unavailable.node_id, id);
            assert!(unavailable.unavailable_reason.is_some());
            assert!(unavailable.fields.is_empty() && unavailable.links.is_empty());
        }
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod read_stamping_tests {
    use super::*;
    use serde_json::json;

    fn rows() -> Vec<(&'static str, &'static str, Value)> {
        vec![
            (
                "text",
                "user",
                json!({"role":"user","content":[{"type":"input_text","text":"synthetic source"}]}),
            ),
            (
                "tool",
                "tool_result",
                json!({"item":{"type":"function_call_output","call_id":"synthetic-call","output":json!({"status":"cancelled","executed":false,"reason":"human_turn_interrupted","message":"Not executed"}).to_string()}}),
            ),
            (
                "tool_finished",
                "tool_result",
                json!({"item":{"type":"function_call_output","call_id":"finished-call","output":json!({"status":"exited","exit_code":0,"output":"synthetic tool output"}).to_string()}}),
            ),
            ("empty", "event", json!({"text":""})),
        ]
    }

    #[tokio::test]
    async fn native_page_pairs_pending_with_one_observation_and_continues_before_it() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        owner.db.call(|db| {
            let tx = db.transaction()?;
            for ordinal in 0..51 {
                crate::history::append_node(
                    &tx, "thread", &format!("source-{ordinal}"), None, "user",
                    &json!({"role":"user","content":[{"type":"input_text","text":format!("body-{ordinal}")}]}), "now",
                )?;
            }
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('pending','user','now',?1)", [json!({"role":"user","content":[{"type":"input_text","text":"waiting"}]}).to_string()])?;
            tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,updated_at,path_admission)VALUES('receipt','operation','null','thread','pending','held','now','now','unadmitted')", [])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let (updates, incoming) = std::sync::mpsc::sync_channel(32);
        let reader = HistoryReader::new(
            selected_owner(&control),
            control.store.config.ui.language,
            &updates,
            80,
        );
        reader
            .page(&Reply::new("thread", 1), None, None)
            .await
            .unwrap();
        let history = |incoming: &std::sync::mpsc::Receiver<UiUpdate>| loop {
            if let UiUpdate::History {
                nodes,
                pending,
                read_revision,
                before_cursor,
                first_ordinal,
                total_nodes,
                append,
                ..
            } = incoming.try_recv().unwrap()
            {
                break (
                    nodes,
                    pending,
                    read_revision,
                    before_cursor,
                    first_ordinal,
                    total_nodes,
                    append,
                );
            }
        };
        let (nodes, pending, revision, cursor, first, total, append) = history(&incoming);
        assert_eq!((first, total, append), (1, 51, false));
        assert_eq!(nodes.len(), 50);
        assert_eq!(nodes.first().unwrap().node_id, "source-1");
        assert_eq!(nodes.last().unwrap().node_id, "source-50");
        assert_eq!(pending.total, 1);
        assert_eq!(pending.items[0].message_id, "pending");
        assert!(revision > 0);
        assert!(
            nodes
                .iter()
                .all(|node| node.presentation.read_revision == revision)
        );
        reader
            .page(&Reply::new("thread", 1), cursor, None)
            .await
            .unwrap();
        let (older, _, later, cursor, first, _, append) = history(&incoming);
        assert_eq!((first, append, cursor), (0, true, None));
        assert_eq!(
            older
                .iter()
                .map(|node| node.node_id.as_str())
                .collect::<Vec<_>>(),
            ["source-0"]
        );
        assert!(later > revision, "the owner orders each observation");
        control.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn ephemeral_window_facts_do_not_change_serialized_content_or_projection_identity() {
        let mut node = UiNode {
            node_id: "source".into(),
            role: "assistant".into(),
            text: "retained text".into(),
            total_chars: 13,
            presentation: crate::presentation::project(
                "assistant_response",
                &json!({"text":"retained text"}),
                "retained text",
                0,
            ),
            ..Default::default()
        };
        streams::seal_node(&mut node);
        let before = node.presentation.projection_stamp;
        let serialized = serde_json::to_value(&node).unwrap();
        node.presentation.read_revision = 901;
        node.presentation.body_evicted = true;
        streams::seal_node(&mut node);
        assert_eq!(node.presentation.projection_stamp, before);
        assert_eq!(serde_json::to_value(&node).unwrap(), serialized);
        let decoded: UiNode = serde_json::from_value(serialized).unwrap();
        assert_eq!(decoded.presentation.read_revision, 0);
        assert!(!decoded.presentation.body_evicted);
    }

    #[tokio::test]
    async fn node_hydration_and_enriched_pages_share_the_existing_serialized_read_sequence() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                for (id, kind, payload) in rows() {
                    crate::history::append_node(&tx, "thread", id, None, kind, &payload, "now")?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let (updates, incoming) = std::sync::mpsc::sync_channel(32);
        let reader = HistoryReader::new(
            selected_owner(&control),
            control.store.config.ui.language,
            &updates,
            80,
        );
        reader
            .page(&Reply::new("thread", 1), None, None)
            .await
            .unwrap();
        let first = loop {
            match incoming.try_recv().unwrap() {
                UiUpdate::History {
                    nodes,
                    read_revision,
                    ..
                } => {
                    assert_eq!(nodes.len(), 4);
                    for node in nodes {
                        assert_eq!(node.presentation.read_revision, read_revision);
                        assert!(!node.presentation.body_evicted);
                    }
                    break read_revision;
                }
                _ => continue,
            }
        };
        assert!(first > 0);
        let hydrated = reader
            .node("tool_finished".into(), 0, "thread")
            .await
            .unwrap();
        assert!(hydrated.presentation.read_revision > first);
        assert!(!hydrated.presentation.body_evicted);
        let empty = reader.node("empty".into(), 0, "thread").await.unwrap();
        assert!(empty.presentation.read_revision > hydrated.presentation.read_revision);
        assert_eq!(empty.total_chars, 0);
        assert!(!empty.presentation.body_evicted);
        reader
            .page(&Reply::new("thread", 1), None, None)
            .await
            .unwrap();
        let last = loop {
            match incoming.try_recv().unwrap() {
                UiUpdate::History {
                    nodes,
                    read_revision,
                    ..
                } => {
                    for node in nodes {
                        assert_eq!(node.presentation.read_revision, read_revision);
                        assert!(!node.presentation.body_evicted);
                    }
                    break read_revision;
                }
                _ => continue,
            }
        };
        assert!(last > empty.presentation.read_revision);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
