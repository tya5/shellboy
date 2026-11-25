//! Fenced transport and finite assembly for selected-history display sources.
use crate::protocol::{
    ApiError, Command, ErrorCode, HistoryBody, HistoryDisplayPage, HistoryDisplayPosition,
    HistoryDisplayRange, HistoryDisplayRead, HistoryDisplaySelection, HistoryReadArgs,
    HistoryWindow, HistoryWindowArgs, MAX_BODY_BYTES,
};
use std::path::Path;

const PAGE_BYTES: usize = 8 * 1024;
// A page can end up to three bytes early at a UTF-8 boundary.
const MAX_PAGES: usize = (MAX_BODY_BYTES as usize).div_ceil(PAGE_BYTES - 3);

pub(super) struct Owner<'a> {
    pub data_dir: &'a Path,
    pub instance: &'a str,
}

impl Owner<'_> {
    pub(super) async fn read<T: serde::de::DeserializeOwned>(
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
        let window: HistoryWindow = self.read(Command::HistoryWindow(args)).await?;
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
            let body: HistoryBody = self.read(Command::HistoryRead(args.clone())).await?;
            if body.node_id != node_id {
                return Err(invalid_response());
            }
            let page = body.display.ok_or_else(invalid_response)?;
            let display =
                extent.get_or_insert_with(|| DisplayExtent::new(body.kind.clone(), &page));
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
    pub text: String,
    pub range: HistoryDisplayRange,
    pub total_chars: u64,
    pub reasoning: Vec<HistoryDisplayRange>,
    pub output_limited: bool,
    pub received_recovery_wait: bool,
    end: u64,
}

impl DisplayExtent {
    fn new(kind: String, page: &HistoryDisplayPage) -> Self {
        Self {
            kind,
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

    fn page(start: u64, end: u64) -> HistoryDisplayPage {
        HistoryDisplayPage {
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
        let mut extent = DisplayExtent::new("assistant_response".into(), &first);
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
        let mut extent = DisplayExtent::new("assistant_response".into(), &first);
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
