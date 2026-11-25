//! Fenced transport and finite assembly for selected-history display sources.
use crate::protocol::{
    ApiError, Command, ErrorCode, HistoryBody, HistoryDisplayOutline, HistoryDisplayPage,
    HistoryDisplayPosition, HistoryDisplayRange, HistoryDisplayRead, HistoryDisplaySelection,
    HistoryReadArgs, HistoryWindow, HistoryWindowArgs, MAX_BODY_BYTES,
};
use std::path::Path;

const PAGE_BYTES: usize = 8 * 1024;
// A page can end up to three bytes early at a UTF-8 boundary.
const MAX_PAGES: usize = (MAX_BODY_BYTES as usize).div_ceil(PAGE_BYTES - 3);

pub(crate) struct Owner<'a> {
    pub data_dir: &'a Path,
    pub instance: &'a str,
}

impl Owner<'_> {
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
        let mut extent = None;
        for _ in 0..MAX_PAGES {
            let body: HistoryBody = self.call(Command::HistoryRead(args.clone())).await?;
            if body.node_id != node_id {
                return Err(invalid_response());
            }
            let mut page = body.display.ok_or_else(invalid_response)?;
            let display = match &mut extent {
                Some(display) if page.outline.is_none() => display,
                Some(_) => return Err(invalid_response()),
                None => {
                    let outline = page.outline.take().ok_or_else(invalid_response)?;
                    extent.insert(DisplayExtent::new(body.kind.clone(), &page, outline))
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

pub(super) struct DisplayExtent {
    pub kind: String,
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
    fn new(kind: String, page: &HistoryDisplayPage, outline: HistoryDisplayOutline) -> Self {
        Self {
            kind,
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

fn invalid_response() -> ApiError {
    ApiError::new(
        ErrorCode::ProtocolMismatch,
        "Invalid selected history response",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .window(serde_json::from_value(json!({"thread_id":"t"})).unwrap())
            .await
            .unwrap();
        assert_eq!(window.items.len(), 1);
        let node = &window.items[0].node_id;
        let selected = HistoryDisplaySelection {
            thread_id: window.thread_id,
            revision: window.revision,
        };
        let extent = owner
            .display(node, Some(selected.clone()), HistoryDisplayPosition::Tail)
            .await
            .unwrap();
        assert_eq!(extent.text, text);
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
        let stale = owner
            .display(node, Some(selected.clone()), HistoryDisplayPosition::Tail)
            .await;
        assert!(matches!(stale, Err(error) if error.code == ErrorCode::CursorExpired));
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
        let mut extent = DisplayExtent::new("assistant_response".into(), &first, outline());
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
    fn native_display_refuses_partial_or_discontinuous_extent() {
        let first = page(40, 42);
        let mut extent = DisplayExtent::new("assistant_response".into(), &first, outline());
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
