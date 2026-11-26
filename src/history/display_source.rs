//! Canonical text and reasoning coordinates for conversation display.
use crate::protocol::{
    ApiError, ErrorCode, HistoryDisplayPage, HistoryDisplayPosition, HistoryDisplayRange,
    HistoryDisplayRead, HistoryReadArgs, MAX_BODY_BYTES,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

/// Canonical history text plus visible reasoning ranges; opaque continuations
/// are excluded by the same known-field rule as the history reader.
pub fn display_payload(payload: &Value) -> (String, Vec<(usize, usize)>) {
    let canonical = crate::history::text_of_payload(payload);
    let mut rendered = String::new();
    let mut seen = false;
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    crate::history::payload::visit(payload, |leaf| {
        if seen {
            rendered.push('\n');
        }
        seen = true;
        rendered.push_str(leaf.text);
        if leaf.context.reasoning && leaf.end > leaf.start {
            if let Some(last) = ranges.last_mut()
                && last.1 + 1 == leaf.start
            {
                last.1 = leaf.end;
            } else {
                ranges.push((leaf.start, leaf.end));
            }
        }
    });
    // A future history extractor change must never corrupt canonical anchors.
    if rendered != canonical {
        ranges.clear();
    }
    (canonical, ranges)
}
/// Prefer received answer deltas. Alternatives are mutually exclusive so the
/// final event and item-done copies never repeat the same paid response.
pub(crate) fn received_body(payload: &Value) -> (String, Vec<(usize, usize)>) {
    fn fallback(payload: &Value, reasoning: bool) -> String {
        let wanted = if reasoning { "reasoning" } else { "message" };
        let messages = |items: Vec<&Value>| {
            items
                .into_iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some(wanted))
                .map(|item| display_payload(item).0)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let final_text = || {
            payload
                .pointer("/raw_response/output")
                .and_then(Value::as_array)
                .map(|items| messages(items.iter().collect()))
                .unwrap_or_default()
        };
        let done_text = || {
            let mut items = payload
                .get("completed_items")
                .and_then(Value::as_object)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|(index, item)| {
                            index.parse::<u64>().ok().map(|index| (index, item))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            items.sort_by_key(|(index, _)| *index);
            messages(items.into_iter().map(|(_, item)| item).collect())
        };
        let observation =
            payload.get("classification").and_then(Value::as_str) == Some("stream_difference");
        let first = if observation {
            done_text()
        } else {
            final_text()
        };
        if !first.is_empty() {
            first
        } else if observation {
            final_text()
        } else {
            done_text()
        }
    }

    let channel = |key: &str, reasoning| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| fallback(payload, reasoning))
    };
    let reasoning = channel("partial_reasoning", true);
    let answer = channel("partial_text", false);
    let count = reasoning.chars().count();
    let ranges = if count > 0 { vec![(0, count)] } else { vec![] };
    let text = if reasoning.is_empty() {
        answer
    } else if answer.is_empty() {
        reasoning
    } else {
        format!("{reasoning}\n{answer}")
    };
    (text, ranges)
}

pub(crate) const MAX_ANCHORED_BODY_BYTES: usize = 8 * 1024;
fn open_code_fence_before(text: &str, end: usize) -> Option<usize> {
    let mut open: Option<(usize, u8, usize)> = None;
    let mut line_start = 0;
    for line in text[..end].split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let indent = content.bytes().take_while(|byte| *byte == b' ').count();
        if indent <= 3 {
            let tail = &content[indent..];
            let marker = tail.as_bytes().first().copied();
            if matches!(marker, Some(b'`' | b'~')) {
                let count = tail
                    .bytes()
                    .take_while(|byte| Some(*byte) == marker)
                    .count();
                if count >= 3 {
                    if let Some((_, open_marker, open_count)) = open {
                        if marker == Some(open_marker)
                            && count >= open_count
                            && tail[count..].trim().is_empty()
                        {
                            open = None;
                        }
                    } else {
                        open = Some((line_start, marker.unwrap(), count));
                    }
                }
            }
        }
        line_start += line.len();
    }
    open.map(|(start, _, _)| start)
}
pub(crate) fn extent(text: &str, offset: Option<usize>) -> (std::ops::Range<usize>, usize) {
    let total = text.chars().count();
    let mut limit = offset.map_or(MAX_BODY_BYTES as usize, |_| MAX_ANCHORED_BODY_BYTES);
    let mut start = if let Some(offset) = offset {
        text.char_indices()
            .nth(offset.min(total).saturating_sub(512))
            .map_or(text.len(), |(index, _)| index)
    } else {
        text.ceil_char_boundary(text.len().saturating_sub(MAX_BODY_BYTES as usize))
    };
    if offset.is_some()
        && let Some(fence_start) = open_code_fence_before(text, start)
        && start - fence_start <= MAX_BODY_BYTES as usize
    {
        let requested_end = start + limit;
        start = fence_start;
        limit = (requested_end - start).min(MAX_BODY_BYTES as usize);
    }
    let end = text.floor_char_boundary((start + limit).min(text.len()));
    (start..end, total)
}

pub(super) fn read_page(
    db: &Connection,
    args: &HistoryReadArgs,
    kind: &str,
    display: &HistoryDisplayRead,
    continuation: Option<(usize, u64)>,
) -> Result<(String, HistoryDisplayPage, usize, usize), ApiError> {
    let expired = || ApiError::new(ErrorCode::CursorExpired, "Display source selection changed");
    if let Some(selection) = &display.selection {
        let actual: Option<i64> = db
            .query_row(
                "SELECT t.selection_revision FROM thread t JOIN thread_path p ON p.thread_id=t.id
             WHERE t.id=?1 AND p.node_id=?2 AND t.deleted_at IS NULL",
                rusqlite::params![selection.thread_id, args.node_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(super::sql_error)?;
        if actual.and_then(|revision| u64::try_from(revision).ok()) != Some(selection.revision) {
            return Err(expired());
        }
    }
    let (payload, reason, output_limited, received_recovery_wait): (Option<String>, Option<String>, bool, bool) = db.query_row(
        "SELECT n.payload_json,n.deleted_reason,n.output_limit_attempt_id IS NOT NULL,
         CASE WHEN n.kind='incomplete_response' THEN EXISTS(SELECT 1 FROM turn u WHERE u.id=n.turn_id AND u.state='recovery_wait') ELSE 0 END
         FROM node n WHERE n.id=?1",
        [&args.node_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).map_err(super::sql_error)?;
    let (decoded, source, mut reasoning) = decode(kind, payload.as_deref(), reason.as_deref());
    let total = source.chars().count();
    if kind == "reasoning" && total > 0 {
        reasoning = vec![(0, total)];
    }
    let offset = match display.position {
        HistoryDisplayPosition::Tail => None,
        HistoryDisplayPosition::Absolute { offset } => {
            Some(usize::try_from(offset).unwrap_or(usize::MAX))
        }
        HistoryDisplayPosition::FromEnd { offset } => {
            Some(total.saturating_sub(usize::try_from(offset).unwrap_or(usize::MAX)))
        }
    };
    let (extent, _) = extent(&source, offset);
    let start = match continuation {
        None => extent.start,
        Some((start, mark)) if mark == extent.end as u64 && extent.contains(&start) => start,
        _ => return Err(expired()),
    };
    let limit = (args.limit_bytes as usize).min(MAX_ANCHORED_BODY_BYTES);
    let (text, _) = super::body_page::projected(&source[..extent.end], start, limit)?;
    let end = start + text.len();
    let char_at = |byte| source[..byte].chars().count() as u64;
    let page = HistoryDisplayRange {
        start: char_at(start),
        end: char_at(end),
    };
    let reasoning = reasoning
        .into_iter()
        .filter_map(|(start, end)| {
            let start = (start as u64).max(page.start);
            let end = (end as u64).min(page.end);
            (start < end).then_some(HistoryDisplayRange { start, end })
        })
        .collect();
    let range = HistoryDisplayRange {
        start: char_at(extent.start),
        end: char_at(extent.end),
    };
    let outline = continuation.is_none().then(|| {
        crate::presentation::display_outline(
            &args.node_id,
            kind,
            &decoded,
            &source[extent.clone()],
            &range,
        )
    });
    let projection = HistoryDisplayPage {
        outline,
        extent: range,
        page,
        total_chars: total as u64,
        reasoning,
        output_limited,
        received_recovery_wait,
    };
    Ok((text, projection, start, extent.end))
}

pub(crate) fn decode(
    kind: &str,
    payload: Option<&str>,
    reason: Option<&str>,
) -> (Value, String, Vec<(usize, usize)>) {
    let Some(payload) = payload.and_then(|payload| serde_json::from_str::<Value>(payload).ok())
    else {
        let reason = reason.unwrap_or("stored payload unavailable or malformed");
        return (
            serde_json::json!({"tombstone_reason": reason}),
            format!("[original unavailable: {reason}]"),
            vec![],
        );
    };
    let (text, reasoning) = if matches!(kind, "incomplete_response" | "model_stream_observation") {
        received_body(&payload)
    } else {
        display_payload(&payload)
    };
    (payload, text, reasoning)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extent's text, its starting character, and the source length.
    fn extent_window(text: &str, offset: Option<usize>) -> (String, usize, usize) {
        let (range, total) = extent(text, offset);
        (
            text[range.clone()].into(),
            text[..range.start].chars().count(),
            total,
        )
    }

    #[test]
    fn anchored_history_body_windows_are_smaller_and_centered_near_the_anchor() {
        let text = "0123456789界".repeat(5000);
        let (window, base, total) = extent_window(&text, Some(30_000));
        assert!(window.len() <= MAX_ANCHORED_BODY_BYTES);
        assert!(base <= 30_000 && base + window.chars().count() > 30_000);
        assert_eq!(total, text.chars().count());

        let (tail, _, _) = extent_window(&text, None);
        assert!(tail.len() <= MAX_BODY_BYTES as usize);
        assert!(tail.len() > window.len());
    }

    #[test]
    fn anchored_code_window_keeps_its_fence_open_and_close_delimiters() {
        let prefix = "ordinary text\n".repeat(900);
        let code = format!("```rust\n{}\n```\n", "let value = 42;\n".repeat(400));
        let source = format!("{prefix}{code}{}", "after code\n".repeat(9000));
        let anchor = prefix.chars().count()
            + "```rust\n".chars().count()
            + 200 * "let value = 42;\n".chars().count();
        let (window, base, _) = extent_window(&source, Some(anchor));
        assert!(base <= anchor);
        assert!(window.starts_with("```rust\n"));
        assert!(window.contains("\n```\n"));
        assert!(window.len() <= MAX_BODY_BYTES as usize);
    }
}
