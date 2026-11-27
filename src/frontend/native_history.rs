//! Fenced transport and finite assembly for selected-history display sources.
use crate::protocol::{
    ApiError, Command, ErrorCode, HistoryBody, HistoryDisplayOutline, HistoryDisplayPage,
    HistoryDisplayPosition, HistoryDisplayRange, HistoryDisplayRead, HistoryDisplaySelection,
    HistoryReadArgs, HistorySnippet, HistoryWindow, HistoryWindowArgs, HistoryWindowObservation,
    MAX_BODY_BYTES, Request, Response,
};
use std::path::Path;

const PAGE_BYTES: usize = 8 * 1024;
// A page can end up to three bytes early at a UTF-8 boundary.
const MAX_PAGES: usize = (MAX_BODY_BYTES as usize).div_ceil(PAGE_BYTES - 3);

#[derive(Clone, Copy)]
pub(crate) struct Owner<'a> {
    pub data_dir: &'a Path,
    pub instance: &'a str,
}

impl Owner<'_> {
    pub(super) fn operation(&self, command: Command) -> Request {
        Request::new(
            format!("{}:{}", self.instance, uuid::Uuid::new_v4()),
            command,
        )
    }

    pub(super) async fn dispatch(
        &self,
        request: Request,
    ) -> Result<Response<serde_json::Value>, ApiError> {
        crate::ipc::call_selected_operation(
            self.data_dir,
            self.instance,
            request.command,
            Some(request.operation_id),
        )
        .await
    }

    pub(crate) async fn call<T: serde::de::DeserializeOwned>(
        &self,
        command: Command,
    ) -> Result<T, ApiError> {
        let response =
            crate::ipc::call_selected_operation(self.data_dir, self.instance, command, None)
                .await?;
        if !response.ok {
            return Err(response.error.unwrap_or_else(invalid_response));
        }
        serde_json::from_value(response.result.ok_or_else(invalid_response)?)
            .map_err(|_| invalid_response())
    }

    pub(super) async fn window(
        &self,
        mut args: HistoryWindowArgs,
    ) -> Result<HistoryWindow, ApiError> {
        args.include_observation = true;
        let thread = args.thread_id.clone();
        let window: HistoryWindow = self.call(Command::HistoryWindow(args)).await?;
        if window.thread_id != thread || window.observation.is_none() {
            return Err(invalid_response());
        }
        Ok(window)
    }

    /// One fenced window, then each node's extent at that selection revision,
    /// keeping the window's mutable facts from the same owner observation.
    pub(super) async fn selected(
        &self,
        args: HistoryWindowArgs,
        expected_revision: Option<u64>,
        keep: impl FnOnce(&[HistorySnippet]) -> std::ops::Range<usize>,
        position: impl Fn(&HistorySnippet) -> HistoryDisplayPosition,
    ) -> Result<SelectedWindow, ApiError> {
        let mut window = self.window(args).await?;
        if expected_revision.is_some_and(|revision| window.revision != revision) {
            return Err(source_changed());
        }
        let revision = window.revision;
        let selection = HistoryDisplaySelection {
            thread_id: window.thread_id,
        };
        // Hydrate only the kept contiguous part; the observation is unchanged.
        let kept = keep(&window.items);
        if kept.start > kept.end || kept.end > window.items.len() {
            return Err(invalid_response());
        }
        let mut nodes = Vec::with_capacity(kept.len());
        for source in window.items.drain(kept) {
            if source.selection.thread_id != selection.thread_id
                || source.selection.ordinal >= window.total
                || nodes.last().is_some_and(|node: &SelectedNode| {
                    node.source.selection.ordinal.checked_add(1) != Some(source.selection.ordinal)
                })
            {
                return Err(invalid_response());
            }
            let display = self
                .display(&source.node_id, Some(selection.clone()), position(&source))
                .await?;
            if display.kind != source.kind
                || display.source_revision != source.provenance.source_revision
            {
                return Err(source_changed());
            }
            nodes.push(SelectedNode { source, display });
        }
        Ok(SelectedWindow {
            revision,
            total: window.total,
            observation: window.observation.take().ok_or_else(invalid_response)?,
            nodes,
        })
    }

    /// One positioned node by ID, plus the selected window's facts for it. A
    /// received response is never on a path: its observation is the selected
    /// tail's, which orders the install but has no facts for it. Any other
    /// node must still be on the selected path.
    pub(super) async fn node(
        &self,
        thread_id: &str,
        node_id: &str,
        position: HistoryDisplayPosition,
    ) -> Result<(DisplayExtent, HistoryWindowObservation), ApiError> {
        use crate::protocol::HistoryDisplayNode as Node;
        let display = self.display(node_id, None, position).await?;
        let received = matches!(display.outline.node, Node::Received);
        let mut window = self
            .window(HistoryWindowArgs {
                thread_id: thread_id.to_owned(),
                include_observation: true,
                observe_node_ids: Vec::new(),
                items: false,
                ordinal: None,
                node_id: (!received).then(|| node_id.to_owned()),
                expect_node_id: None,
                before: 0,
                after: 0,
                excerpt_chars: 0,
            })
            .await?;
        if !received {
            match window.items.as_slice() {
                [item] if item.node_id == node_id => {
                    if item.kind != display.kind
                        || item.provenance.source_revision != display.source_revision
                    {
                        return Err(source_changed());
                    }
                }
                _ => return Err(invalid_response()),
            }
        }
        let observation = window.observation.take().ok_or_else(invalid_response)?;
        Ok((display, observation))
    }

    pub(super) async fn display(
        &self,
        node_id: &str,
        selection: Option<HistoryDisplaySelection>,
        position: HistoryDisplayPosition,
    ) -> Result<DisplayExtent, ApiError> {
        let mut args = HistoryReadArgs {
            display: Some(HistoryDisplayRead {
                position,
                selection,
            }),
            end_message_only: false,
            parts: false,
            node_id: node_id.to_owned(),
            limit_bytes: PAGE_BYTES as u32,
            cursor: None,
            encoding: "UTF-8".into(),
        };
        let mut extent: Option<DisplayExtent> = None;
        for _ in 0..MAX_PAGES {
            let body: HistoryBody = self.call(Command::HistoryRead(args.clone())).await?;
            if body.node_id != node_id {
                return Err(invalid_response());
            }
            let mut page = body.display.ok_or_else(invalid_response)?;
            let display = match &mut extent {
                Some(display) if display.source_revision != body.provenance.source_revision => {
                    return Err(source_changed());
                }
                Some(display) if page.outline.is_none() => display,
                Some(_) => return Err(invalid_response()),
                None => {
                    let outline = page.outline.take().ok_or_else(invalid_response)?;
                    validate_outline(&page.extent, &outline)?;
                    extent.insert(DisplayExtent::new(
                        body.kind.clone(),
                        body.provenance.source_revision,
                        &page,
                        outline,
                    ))
                }
            };
            let more = body.next_cursor.is_some();
            display.push(&body.kind, &page, &body.text, more)?;
            if !more {
                return Ok(extent.expect("page installed"));
            }
            if body.next_cursor == args.cursor {
                return Err(invalid_response());
            }
            args.cursor = body.next_cursor;
        }
        Err(invalid_response())
    }
}

pub(super) struct SelectedWindow {
    pub revision: u64,
    pub total: u64,
    pub observation: HistoryWindowObservation,
    pub nodes: Vec<SelectedNode>,
}

pub(super) struct SelectedNode {
    pub source: HistorySnippet,
    pub display: DisplayExtent,
}

pub(super) struct DisplayExtent {
    pub kind: String,
    pub source_revision: i64,
    pub outline: HistoryDisplayOutline,
    pub text: String,
    pub range: HistoryDisplayRange,
    pub total_chars: u64,
    pub reasoning: Vec<HistoryDisplayRange>,
    pub output_limited: bool,
    pub received_recovery_wait: bool,
    end: u64,
}

impl DisplayExtent {
    pub(super) fn project(&self) -> crate::presentation::NodePresentation {
        let mut projection = crate::presentation::project_display(
            &self.kind,
            &self.text,
            self.range.start as usize,
            &self.outline,
        );
        projection.output_limited = self.output_limited;
        projection
    }

    /// Client layout of one verified extent; only width and language are local.
    pub(super) fn into_ui_node(
        self,
        node_id: String,
        width: u16,
        language: crate::config::UiLanguage,
    ) -> crate::ui::UiNode {
        use crate::protocol::HistoryDisplayNode as Node;
        if matches!(self.outline.node, Node::HiddenAdmission) {
            return crate::ui::UiNode {
                node_id,
                role: "context".into(),
                ..Default::default()
            };
        }
        let reasoning_ranges = self
            .reasoning
            .iter()
            .map(|range| (range.start as usize, range.end as usize))
            .collect();
        let base_char_offset = self.range.start as usize;
        let total_chars = self.total_chars as usize;
        if matches!(self.outline.node, Node::Received) {
            return super::received::project(
                &self.kind,
                crate::ui::UiNode {
                    node_id,
                    text: self.text,
                    base_char_offset,
                    total_chars,
                    reasoning_ranges,
                    ..Default::default()
                },
                width,
                false,
                self.received_recovery_wait,
                language,
            );
        }
        let mut presentation = self.project();
        crate::presentation::render_markdown(&mut presentation, width.max(1));
        crate::ui::UiNode {
            presentation,
            node_id,
            role: super::node_role(&self.kind).into(),
            text: self.text,
            base_char_offset,
            total_chars,
            reasoning_ranges,
        }
    }

    fn new(
        kind: String,
        source_revision: i64,
        page: &HistoryDisplayPage,
        outline: HistoryDisplayOutline,
    ) -> Self {
        Self {
            kind,
            source_revision,
            outline,
            text: String::new(),
            range: page.extent.clone(),
            total_chars: page.total_chars,
            reasoning: Vec::new(),
            output_limited: page.output_limited,
            received_recovery_wait: page.received_recovery_wait,
            end: page.extent.start,
        }
    }

    fn push(
        &mut self,
        kind: &str,
        page: &HistoryDisplayPage,
        text: &str,
        more: bool,
    ) -> Result<(), ApiError> {
        if self.kind != kind
            || self.range != page.extent
            || self.total_chars != page.total_chars
            || self.output_limited != page.output_limited
            || self.received_recovery_wait != page.received_recovery_wait
            || self.range.end > self.total_chars
            || self.end != page.page.start
            || page.page.start.checked_add(text.chars().count() as u64) != Some(page.page.end)
            || page.page.end > self.range.end
            || more != (page.page.end < self.range.end)
            || (more && text.is_empty())
            || text.len() > PAGE_BYTES
            || self.text.len() + text.len() > MAX_BODY_BYTES as usize
        {
            return Err(invalid_response());
        }
        let mut previous = page.page.start;
        for range in &page.reasoning {
            if range.start < previous || range.start >= range.end || range.end > page.page.end {
                return Err(invalid_response());
            }
            previous = range.end;
        }
        for range in &page.reasoning {
            if let Some(last) = self.reasoning.last_mut()
                && last.end == range.start
            {
                last.end = range.end;
            } else {
                self.reasoning.push(range.clone());
            }
        }
        self.text.push_str(text);
        self.end = page.page.end;
        Ok(())
    }
}

/// Mutable selected-window facts from the same owner transaction as the window.
pub(super) fn apply_observation(
    nodes: &mut [crate::ui::UiNode],
    observation: &HistoryWindowObservation,
    language: crate::config::UiLanguage,
) {
    super::tools::apply_window(nodes, &observation.tools);
    super::recovery::apply_window(nodes, &observation.recovery, language);
    super::usage::apply_window(nodes, &observation.usage);
    super::message_metadata::apply_pinned(nodes, &observation.pinned_sources);
    super::compaction_markers::apply(nodes, &observation.compactions);
    for marker in &observation.branches {
        if marker.current < marker.alternatives.len()
            && let Some(node) = nodes
                .iter_mut()
                .find(|node| node.node_id == marker.anchor_node_id)
        {
            node.presentation.branch_marker =
                Some(crate::presentation::branch_marker::BranchMarker::from_facts(marker));
        }
    }
}

fn validate_outline(
    extent: &HistoryDisplayRange,
    outline: &HistoryDisplayOutline,
) -> Result<(), ApiError> {
    let in_extent = |range: &HistoryDisplayRange| {
        range.start >= extent.start && range.start < range.end && range.end <= extent.end
    };
    let mut previous = extent.start;
    for block in &outline.blocks {
        if !in_extent(&block.range) || block.range.start < previous {
            return Err(invalid_response());
        }
        previous = block.range.end;
    }
    if outline.links.iter().any(|link| !in_extent(&link.range)) {
        return Err(invalid_response());
    }
    Ok(())
}

fn source_changed() -> ApiError {
    ApiError::new(ErrorCode::CursorExpired, "Selected history source changed")
}

fn invalid_response() -> ApiError {
    ApiError::new(
        ErrorCode::ProtocolMismatch,
        "Invalid selected history response",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The saved-payload projection the TUI used before native extents: the
    /// parity oracle for the owner's display reads.
    struct SavedNode {
        id: String,
        kind: String,
        payload: Option<String>,
        reason: Option<String>,
        received_recovery_wait: bool,
        output_limited: bool,
    }
    impl SavedNode {
        fn into_ui_node(
            self,
            position: Option<super::super::NodePosition>,
            width: u16,
            language: crate::config::UiLanguage,
        ) -> crate::ui::UiNode {
            use crate::ui::UiNode;
            if crate::history::is_legacy_admission_node(&self.id, &self.kind) {
                return UiNode {
                    node_id: self.id,
                    role: "context".into(),
                    ..UiNode::default()
                };
            }
            let (payload, text, mut reasoning_ranges) = crate::history::display_source::decode(
                &self.kind,
                self.payload.as_deref(),
                self.reason.as_deref(),
            );
            let offset = position.map(|position| position.offset(text.chars().count()));
            if self.kind == "reasoning" && !text.is_empty() {
                reasoning_ranges = vec![(0, text.chars().count())];
            }
            let (extent, total_chars) = crate::history::display_source::extent(&text, offset);
            let base_char_offset = text[..extent.start].chars().count();
            let text = text[extent].to_owned();
            let window_end = base_char_offset + text.chars().count();
            reasoning_ranges.retain(|range| range.1 > base_char_offset && range.0 < window_end);
            if matches!(
                self.kind.as_str(),
                "incomplete_response" | "model_stream_observation"
            ) {
                return super::super::received::project(
                    &self.kind,
                    UiNode {
                        node_id: self.id,
                        text,
                        base_char_offset,
                        total_chars,
                        reasoning_ranges,
                        ..Default::default()
                    },
                    width,
                    false,
                    self.received_recovery_wait,
                    language,
                );
            }
            let mut presentation =
                crate::presentation::project(&self.kind, &payload, &text, base_char_offset);
            presentation.output_limited = self.output_limited;
            crate::presentation::render_markdown(&mut presentation, width.max(1));
            UiNode {
                presentation,
                node_id: self.id,
                role: super::super::node_role(&self.kind).into(),
                text,
                base_char_offset,
                total_chars,
                reasoning_ranges,
            }
        }
    }

    #[tokio::test]
    async fn native_display_hydrates_the_exact_window_until_selection_changes() {
        use crate::{app::DataOwner, control::Control, ipc::LocalServer};
        use serde_json::json;

        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let data = DataOwner::start(None, Some(data_dir.clone()), root.path().into())
            .await
            .unwrap();
        let text = "\u{754c}".repeat(4500);
        let saved = text.clone();
        data.db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','test','/','','p','m','2026-01-01T00:00:00Z')", [])?;
            crate::history::append_node(&tx,"t","thought",None,"reasoning",&json!({"text": saved}),"2026-01-01T00:00:00Z")?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let control = Control::new(&data, root.path().into());
        let (sender, receiver) = tokio::sync::mpsc::channel(16);
        let server = LocalServer::bind(&data, sender).await.unwrap();
        let dispatch = tokio::spawn(control.clone().serve(receiver));
        control.start().await.unwrap();
        let owner = Owner {
            data_dir: &data_dir,
            instance: &control.store.instance_id,
        };
        let window = owner
            .selected(
                serde_json::from_value(json!({"thread_id":"t"})).unwrap(),
                None,
                |items| 0..items.len(),
                |_| HistoryDisplayPosition::Tail,
            )
            .await
            .unwrap();
        assert_eq!(window.observation.pending.total, 0);
        assert_eq!(window.nodes.len(), 1);
        let node = &window.nodes[0].source.node_id;
        let observed = owner
            .window(serde_json::from_value(json!({"thread_id":"t"})).unwrap())
            .await
            .unwrap();
        assert_eq!(observed.total, 1);
        let selected = HistoryDisplaySelection {
            thread_id: observed.thread_id,
        };
        let extent = &window.nodes[0].display;
        assert_eq!(extent.text, text);
        let projection = extent.project();
        assert_eq!(projection.blocks.len(), 1);
        assert_eq!(
            projection.blocks[0].kind,
            crate::presentation::BlockKind::Reasoning
        );
        assert_eq!(projection.blocks[0].text, text);
        assert_eq!(
            (
                projection.blocks[0].source_start,
                projection.blocks[0].source_end
            ),
            (0, 4500)
        );
        assert_eq!(extent.outline.blocks.len(), 1);
        assert_eq!(
            extent.outline.blocks[0].role,
            crate::protocol::HistoryDisplayRole::Reasoning
        );
        assert_eq!(
            extent.outline.blocks[0].range,
            HistoryDisplayRange {
                start: 0,
                end: 4500
            }
        );
        assert_eq!(
            extent.reasoning,
            [HistoryDisplayRange {
                start: 0,
                end: 4500
            }]
        );

        data.db
            .call(|db| {
                let tx = db.transaction()?;
                crate::history::append_node(
                    &tx,
                    "t",
                    "later",
                    None,
                    "user",
                    &json!({"text":"later"}),
                    "2026-01-01T00:00:01Z",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        // An append does not displace an earlier node from the selected path.
        owner
            .display(node, Some(selected.clone()), HistoryDisplayPosition::Tail)
            .await
            .unwrap();
        let wrong_owner = Owner {
            data_dir: &data_dir,
            instance: "different-instance",
        };
        let refused = wrong_owner
            .display(node, Some(selected), HistoryDisplayPosition::Tail)
            .await;
        assert!(matches!(refused, Err(error) if error.code == ErrorCode::ProtocolMismatch));
        server.shutdown().await.unwrap();
        dispatch.await.unwrap();
        drop(control);
        data.shutdown().await.unwrap();
    }

    /// Screen parity: every saved node kind the conversation shows projects to the
    /// same UI node from the owner's positioned extent as from its saved payload.
    #[tokio::test]
    async fn native_display_projects_the_same_ui_nodes_as_saved_payloads() {
        use crate::{app::DataOwner, control::Control, ipc::LocalServer};
        use serde_json::json;

        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let data = DataOwner::start(None, Some(data_dir.clone()), root.path().into())
            .await
            .unwrap();
        let envelope =
            json!({"event_id":"ended","kind":"shell_ended","payload":{"execution_id":"exec"}});
        let spill_args = json!({"command":"git status","description":"状態を確認"}).to_string();
        let mut long = String::new();
        for section in 0..400 {
            long.push_str(&format!("## Section {section}\n\nParagraph with [link {section}](https://example.test/{section}) and *emphasis*.\n\n```rust\nfn section_{section}() {{}}\n```\n\n"));
        }
        let nodes = vec![
            (
                "user",
                "user",
                json!({"role":"user","content":[{"type":"input_text","text":"hello **world** [x](https://example.test/x)"}]}),
            ),
            (
                "answer",
                "assistant_response",
                json!({"text":"# Title\n\nSome *markdown* with [link](https://example.test/a)\n\n```rust\nfn x() {}\n```\n"}),
            ),
            (
                "call",
                "assistant_response",
                json!({"group_id":"g","output":[{"type":"function_call","name":"shell","call_id":"c1","arguments":json!({"command":"ls -la","description":"一覧"}).to_string()}]}),
            ),
            (
                "result",
                "tool_result",
                json!({"group_id":"g","item":{"type":"function_call_output","call_id":"c1","output":json!({"status":"exited","exit_code":0,"output":"a\nb"}).to_string()}}),
            ),
            ("thought", "reasoning", json!({"text":"thinking about it"})),
            (
                "event",
                "event",
                json!({"event_id":"ended","role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {envelope}")}]}),
            ),
            (
                "spill",
                "spill",
                json!({"output":[
                {"type":"function_call","name":"shell","call_id":"c","arguments":spill_args},
                {"type":"function_call_output","call_id":"c","output":json!({"omitted":true,"call_id":"c","status":"exited","exit_code":7,"execution_id":"e","original_node_id":"r"}).to_string()}
            ],"spill_version":1,"source_node_ids":["a","r"]}),
            ),
            (
                "trim",
                "spill",
                json!({"text":"same visible notice","context_trim":{"omitted_nodes":3,"first_node_id":"a","last_node_id":"b","reason":"limit"}}),
            ),
            (
                "summary",
                "summary",
                json!({"text":"summary body","source_node_ids":["a","b"]}),
            ),
            (
                "received",
                "incomplete_response",
                json!({"attempt_id":"x","partial_text":"partial answer","partial_reasoning":"partial thought"}),
            ),
            (
                "interrupted",
                "interruption_context",
                json!({"text":"interrupted context"}),
            ),
            (
                "reclaimed",
                "assistant_response",
                json!({"text":"will be reclaimed"}),
            ),
            ("long", "assistant_response", json!({"text":long})),
        ];
        let saved = nodes.clone();
        data.db
            .call(move |db| {
                let tx = db.transaction()?;
                tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','test','/','','p','m','2026-01-01T00:00:00Z')", [])?;
                for (id, kind, payload) in &saved {
                    crate::history::append_node(&tx, "t", id, None, kind, payload, "2026-01-01T00:00:00Z")?;
                }
                tx.execute("UPDATE node SET payload_json=NULL,deleted_reason='gc' WHERE id='reclaimed'", [])?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let control = Control::new(&data, root.path().into());
        let (sender, receiver) = tokio::sync::mpsc::channel(16);
        let server = LocalServer::bind(&data, sender).await.unwrap();
        let dispatch = tokio::spawn(control.clone().serve(receiver));
        control.start().await.unwrap();
        let owner = Owner {
            data_dir: &data_dir,
            instance: &control.store.instance_id,
        };
        let language = crate::config::UiLanguage::En;
        let saved_node = |id: &str| {
            let id = id.to_owned();
            data.db.call(move |db| {
                Ok(db.query_row(
                    "SELECT kind,payload_json,deleted_reason,output_limit_attempt_id IS NOT NULL FROM node WHERE id=?1",
                    [&id],
                    |row| {
                        Ok(SavedNode {
                            id: id.clone(),
                            kind: row.get(0)?,
                            payload: row.get(1)?,
                            reason: row.get(2)?,
                            received_recovery_wait: false,
                            output_limited: row.get(3)?,
                        })
                    },
                )?)
            })
        };
        let mut positions = nodes.iter().map(|(id, ..)| (*id, None)).collect::<Vec<_>>();
        positions.extend([
            ("long", Some(40_000)),
            ("long", Some(3)),
            ("answer", Some(4)),
        ]);
        let mut mismatches = Vec::new();
        for (id, offset) in positions {
            let mut expected = [saved_node(id).await.unwrap().into_ui_node(
                offset.map(super::super::NodePosition::At),
                80,
                language,
            )];
            // Without owner facts a saved payload group is not shown.
            for tool in expected[0]
                .presentation
                .blocks
                .iter_mut()
                .filter_map(|block| block.tool.as_mut())
            {
                tool.group_id = None;
            }
            let position = offset.map_or(HistoryDisplayPosition::Tail, |offset| {
                HistoryDisplayPosition::Absolute {
                    offset: offset as u64,
                }
            });
            let mut native = [owner
                .display(id, None, position)
                .await
                .unwrap()
                .into_ui_node(id.into(), 80, language)];
            super::super::tools::apply_window(&mut native, &no_tools());
            let (native, expected) = (comparable(&native[0]), comparable(&expected[0]));
            if native != expected {
                mismatches.push(format!(
                    "{id} at {offset:?}: {}",
                    difference(&native, &expected)
                ));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
        server.shutdown().await.unwrap();
        dispatch.await.unwrap();
        drop(control);
        data.shutdown().await.unwrap();
    }

    /// The selected window's owner facts annotate the same saved tool, notice,
    /// recovery and usage rows as the frontend's former transaction readers.
    #[tokio::test]
    async fn native_window_facts_annotate_like_saved_source_readers() {
        use crate::{app::DataOwner, control::Control, ipc::LocalServer};
        use serde_json::json;

        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let data = DataOwner::start(None, Some(data_dir.clone()), root.path().into())
            .await
            .unwrap();
        let ended =
            json!({"event_id":"ended","kind":"shell_ended","payload":{"execution_id":"exec"}});
        let resume = json!({"event_id":"resume","kind":"context_changed","payload":{"kind":"recovery_resumed","change":{"run_id":"r","turn_id":"u"}}});
        let nodes = vec![
            (
                "call",
                "assistant_response",
                json!({"group_id":"g","output":[{"type":"function_call","name":"shell","call_id":"c1","arguments":json!({"command":"ls -la","description":"一覧"}).to_string()}]}),
            ),
            (
                "result",
                "tool_result",
                json!({"group_id":"g","item":{"type":"function_call_output","call_id":"c1","output":json!({"status":"exited","exit_code":0,"output":"a\nb"}).to_string()}}),
            ),
            (
                "ended-event",
                "event",
                json!({"event_id":"ended","role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {ended}")}]}),
            ),
            (
                "resume-event",
                "event",
                json!({"event_id":"resume","role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {resume}")}]}),
            ),
            (
                "answer",
                "assistant_response",
                json!({"text":"final answer"}),
            ),
        ];
        let saved = nodes.clone();
        data.db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','test','/','','p','m','2026-01-01T00:00:00Z');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:09Z');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:09Z');
                INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('conversation','t','00000000-0000-4000-8000-00000000000a','u',0,0,'p','m',0,'completed','{}','{\"input_tokens\":140,\"input_tokens_details\":{\"cached_tokens\":17}}','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z');
                INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','00000000-0000-4000-8000-00000000000a','u','completed','2026-01-01T00:00:00Z');
                INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,exit_code,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','r','u','exec','exited',0,'/','{}','UTF-8','UTF-8','closed','2026-01-01T00:00:00Z');
                INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g','c1',0,'shell','{\"command\":\"ls -la\",\"description\":\"一覧\",\"background\":true}','completed','exec');")?;
            tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('ended','shell_ended','{}','t',?1,'2026-01-01T00:00:02Z')", [json!({"execution_id":"exec"}).to_string()])?;
            tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('resume','context_changed','{}','t',?1,'2026-01-01T00:00:03Z')", [json!({"kind":"recovery_resumed","change":{"run_id":"r","turn_id":"u"}}).to_string()])?;
            let failure = json!({"error":crate::model::ModelError::new("provider","saved failure")}).to_string();
            tx.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES('conversation','t','00000000-0000-4000-8000-0000000000ff','u',0,0,'p','m',0,'failed','{}',?1,'2026-01-01T00:00:00Z','2026-01-01T00:00:01Z')", [failure])?;
            for (id, kind, payload) in &saved {
                crate::history::append_node(&tx, "t", id, Some("u"), kind, payload, "2026-01-01T00:00:02Z")?;
            }
            tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','c1','result','exited')", [])?;
            tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('00000000-0000-4000-8000-00000000000a',0,'message','completed','{}','answer')", [])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let control = Control::new(&data, root.path().into());
        let (sender, receiver) = tokio::sync::mpsc::channel(16);
        let server = LocalServer::bind(&data, sender).await.unwrap();
        let dispatch = tokio::spawn(control.clone().serve(receiver));
        control.start().await.unwrap();
        let owner = Owner {
            data_dir: &data_dir,
            instance: &control.store.instance_id,
        };
        let language = crate::config::UiLanguage::En;
        let window = owner
            .selected(
                serde_json::from_value(json!({"thread_id":"t","excerpt_chars":0})).unwrap(),
                None,
                |items| 0..items.len(),
                |_| HistoryDisplayPosition::Tail,
            )
            .await
            .unwrap();
        assert_eq!(
            window.observation.tools.notices.len(),
            1,
            "{:?}",
            window.observation.tools.notices
        );
        let mut native = window
            .nodes
            .into_iter()
            .map(|node| node.display.into_ui_node(node.source.node_id, 80, language))
            .collect::<Vec<_>>();
        apply_observation(&mut native, &window.observation, language);

        // The saved tool, notice, recovery and usage rows annotate the nodes.
        let call = native
            .iter()
            .find(|node| node.node_id == "call")
            .and_then(|node| node.presentation.blocks[0].tool.clone())
            .unwrap();
        assert_eq!(call.status, "completed");
        assert_eq!(call.group_id.as_deref(), Some("g"));
        assert_eq!(call.execution_id.as_deref(), Some("exec"));
        assert_eq!(call.result_node_id.as_deref(), Some("result"));
        assert!(call.background);
        let answer = native.iter().find(|node| node.node_id == "answer").unwrap();
        assert!(answer.presentation.fields.iter().any(|field| field.key
            == crate::presentation::fields::ATTEMPT_CACHED_TOKENS
            && field.value == "17"));
        let notice = native
            .iter()
            .find(|node| node.node_id == "ended-event")
            .unwrap();
        assert!(notice.presentation.ui_messages.notice.is_some());
        let resumed = native
            .iter()
            .find(|node| node.node_id == "resume-event")
            .unwrap();
        assert!(resumed.presentation.title.starts_with("Resume"));
        server.shutdown().await.unwrap();
        dispatch.await.unwrap();
        drop(control);
        data.shutdown().await.unwrap();
    }

    fn no_tools() -> crate::protocol::HistoryWindowTools {
        crate::protocol::HistoryWindowTools {
            calls: Vec::new(),
            limited_node_ids: Vec::new(),
            notices: Vec::new(),
        }
    }

    /// Wall-clock elapsed observations differ between two reads by design;
    /// every other projected fact, field labels included, must match.
    fn comparable(node: &crate::ui::UiNode) -> crate::ui::UiNode {
        let mut node = node.clone();
        for tool in node
            .presentation
            .blocks
            .iter_mut()
            .filter_map(|block| block.tool.as_mut())
        {
            tool.elapsed_observation = None;
        }
        node
    }

    /// Name the differing parts so a parity failure reports every gap at once.
    fn difference(native: &crate::ui::UiNode, expected: &crate::ui::UiNode) -> String {
        let (n, e) = (&native.presentation, &expected.presentation);
        let mut parts = Vec::new();
        let mut check = |name: &str, same: bool| {
            if !same {
                parts.push(name.to_owned());
            }
        };
        check("role", native.role == expected.role);
        check("text", native.text == expected.text);
        check(
            "coordinates",
            (native.base_char_offset, native.total_chars)
                == (expected.base_char_offset, expected.total_chars),
        );
        check(
            "reasoning",
            native.reasoning_ranges == expected.reasoning_ranges,
        );
        check("kind/title", (&n.kind, &n.title) == (&e.kind, &e.title));
        check("ui_messages", n.ui_messages == e.ui_messages);
        check("blocks", n.blocks == e.blocks);
        let keyed = |p: &crate::presentation::NodePresentation| {
            p.fields
                .iter()
                .map(|f| {
                    (
                        f.key.clone(),
                        f.value.clone(),
                        f.value_omitted,
                        f.display.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        check("field values", keyed(n) == keyed(e));
        let labels = |p: &crate::presentation::NodePresentation| {
            p.fields.iter().map(|f| f.label.clone()).collect::<Vec<_>>()
        };
        check("field labels", labels(n) == labels(e));
        check("links", n.links == e.links);
        check(
            "limits",
            (
                n.metadata_limited,
                n.details_omitted,
                n.body_omitted_chars,
                n.body_extent_unknown,
            ) == (
                e.metadata_limited,
                e.details_omitted,
                e.body_omitted_chars,
                e.body_extent_unknown,
            ),
        );
        let rest = |p: &crate::presentation::NodePresentation| {
            let mut p = p.clone();
            p.blocks.clear();
            p.fields.clear();
            p.links.clear();
            p.ui_messages = Default::default();
            p.title.clear();
            p
        };
        check("other", rest(n) == rest(e));
        if parts.iter().any(|part| part == "blocks") {
            parts.push(format!(
                "native blocks {:?} expected {:?}",
                n.blocks
                    .iter()
                    .map(|b| (
                        b.kind.clone(),
                        b.source_start,
                        b.source_end,
                        b.tool.is_some()
                    ))
                    .collect::<Vec<_>>(),
                e.blocks
                    .iter()
                    .map(|b| (
                        b.kind.clone(),
                        b.source_start,
                        b.source_end,
                        b.tool.is_some()
                    ))
                    .collect::<Vec<_>>()
            ));
        }
        if parts.iter().any(|part| part == "links") {
            parts.push(format!("native links {:?} expected {:?}", n.links, e.links));
        }
        if parts.iter().any(|part| part == "blocks") {
            for (a, b) in n.blocks.iter().zip(&e.blocks) {
                if a != b {
                    parts.push(format!("native block {a:?} expected {b:?}"));
                }
            }
        }
        if parts.iter().any(|part| part == "field values") {
            let (a, b) = (keyed(n), keyed(e));
            parts.push(format!(
                "native-only {:?} expected-only {:?}",
                a.iter()
                    .filter(|f| !b.contains(f))
                    .take(6)
                    .collect::<Vec<_>>(),
                b.iter()
                    .filter(|f| !a.contains(f))
                    .take(6)
                    .collect::<Vec<_>>()
            ));
        }
        if parts.iter().any(|part| part == "kind/title") {
            parts.push(format!(
                "native {:?} {:?} expected {:?} {:?}",
                n.kind, n.title, e.kind, e.title
            ));
        }
        parts.join("; ")
    }

    fn outline() -> HistoryDisplayOutline {
        HistoryDisplayOutline {
            node: crate::protocol::HistoryDisplayNode::Ordinary,
            blocks: Vec::new(),
            fields: Vec::new(),
            links: Vec::new(),
            limited: false,
        }
    }

    fn page(start: u64, end: u64) -> HistoryDisplayPage {
        HistoryDisplayPage {
            outline: None,
            extent: HistoryDisplayRange { start: 40, end: 44 },
            page: HistoryDisplayRange { start, end },
            total_chars: 50,
            reasoning: vec![HistoryDisplayRange { start, end }],
            output_limited: false,
            received_recovery_wait: false,
        }
    }

    #[test]
    fn native_display_assembles_contiguous_unicode_and_reasoning_coordinates() {
        let first = page(40, 42);
        let mut extent = DisplayExtent::new("assistant_response".into(), 1, &first, outline());
        extent
            .push("assistant_response", &first, "a\u{1f980}", true)
            .unwrap();
        extent
            .push("assistant_response", &page(42, 44), "bc", false)
            .unwrap();
        assert_eq!(extent.text, "a\u{1f980}bc");
        assert_eq!(
            extent.reasoning,
            [HistoryDisplayRange { start: 40, end: 44 }]
        );
    }

    #[test]
    fn native_display_rejects_outline_outside_its_extent() {
        use crate::protocol::{HistoryDisplayBlock, HistoryDisplayRole};
        let mut outline = outline();
        let extent = HistoryDisplayRange { start: 14, end: 20 };
        for (start, end) in [(14, 16), (16, 20)] {
            outline.blocks.push(HistoryDisplayBlock {
                range: HistoryDisplayRange { start, end },
                role: HistoryDisplayRole::Prose,
                tool: None,
                event_kind: None,
                fields: Vec::new(),
            });
        }
        validate_outline(&extent, &outline).unwrap();
        outline.blocks[1].range.start = 15;
        assert_eq!(
            validate_outline(&extent, &outline).unwrap_err().code,
            ErrorCode::ProtocolMismatch
        );
        outline.blocks[1].range.start = 16;
        outline.blocks[1].range.end = 21;
        assert_eq!(
            validate_outline(&extent, &outline).unwrap_err().code,
            ErrorCode::ProtocolMismatch
        );
    }

    #[test]
    fn native_display_refuses_partial_or_discontinuous_extent() {
        let first = page(40, 42);
        let mut extent = DisplayExtent::new("assistant_response".into(), 1, &first, outline());
        assert_eq!(
            extent
                .push("assistant_response", &first, "ab", false)
                .unwrap_err()
                .code,
            ErrorCode::ProtocolMismatch
        );
        extent
            .push("assistant_response", &first, "ab", true)
            .unwrap();
        assert_eq!(
            extent
                .push("assistant_response", &page(43, 44), "d", false)
                .unwrap_err()
                .code,
            ErrorCode::ProtocolMismatch
        );
        assert_eq!(extent.text, "ab");
    }
}
