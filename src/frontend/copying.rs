//! Bounded canonical copy preparation in existing read/presentation workers.
use super::*;
use crate::ui::{CopyBlock, CopyOffer};
const SOURCE_BYTES: usize = 1024 * 1024;
const SOURCE_PAGES: usize = 64;
const BLOCKS: usize = 128;

#[derive(Debug)]
enum CopyError {
    Limit,
    ReferenceChanged,
    Unavailable(String),
    Stalled,
    SourceChanged,
    MissingBlock,
}
impl CopyError {
    fn error(self) -> ApiError {
        let (code, message) = match self {
            Self::Limit => (ErrorCode::InputLimit,
                "Copy retrieval limit exceeded. Press o to open the source and n to continue（also available through history get）".into()),
            Self::ReferenceChanged => (ErrorCode::TargetUnavailable,
                "The source reference has changed".into()),
            Self::Unavailable(reason) => (ErrorCode::OutputExpired,
                format!("Cannot retrieve the source: {reason}")),
            Self::Stalled => (ErrorCode::IoFailed,
                "Source pagination is not advancing".into()),
            Self::SourceChanged => (ErrorCode::TargetUnavailable,
                "The source has changed. Press b to select the code block again".into()),
            Self::MissingBlock => (ErrorCode::InvalidArgument,
                "The selected code block does not exist".into()),
        };
        ApiError::new(code, message)
    }
}
fn limit() -> ApiError {
    CopyError::Limit.error()
}
async fn source(control: &Control, node_id: String, budget: usize) -> Result<String, ApiError> {
    let history = crate::history::History::new(control.store.database.clone());
    let mut text = String::new();
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..SOURCE_PAGES {
        let body = history
            .get(HistoryGetArgs {
                final_answer_only: false,
                node_id: node_id.clone(),
                limit_bytes: MAX_BODY_BYTES,
                cursor,
                encoding: DEFAULT_ENCODING.into(),
            })
            .await?;
        if body.node_id != node_id {
            return Err(CopyError::ReferenceChanged.error());
        }
        if let Some(reason) = body.tombstone_reason {
            return Err(CopyError::Unavailable(reason).error());
        }
        if text.len().saturating_add(body.text.len()) > budget {
            return Err(limit());
        }
        text.push_str(&body.text);
        let Some(next) = body.next_cursor else {
            return Ok(text);
        };
        if !seen.insert(next.clone()) {
            return Err(CopyError::Stalled.error());
        }
        cursor = Some(next);
    }
    Err(limit())
}
fn summaries(text: &str) -> Result<Vec<CopyBlock>, ApiError> {
    let mut blocks = Vec::new();
    for block in crate::presentation::markdown_code_blocks(text) {
        if blocks.len() == BLOCKS {
            return Err(limit());
        }
        blocks.push(CopyBlock {
            index: blocks.len(),
            chars: block.text.chars().count(),
            preview: crate::presentation::sanitize_plain(
                &block
                    .text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>(),
                0,
            )
            .0,
        });
    }
    Ok(blocks)
}
pub(super) fn source_hash(text: &str) -> String {
    crate::digest::sha256_hex(text.as_bytes())
}
pub(super) async fn choices(control: &Control, node_id: String) -> Result<CopyOffer, ApiError> {
    let text = source(control, node_id, SOURCE_BYTES).await?;
    presentation_work(move || {
        Ok(CopyOffer {
            source_hash: source_hash(&text),
            blocks: summaries(&text)?,
        })
    })
    .await?
}
pub(super) async fn text(
    control: &Control,
    node_id: String,
    block: Option<usize>,
    expected_hash: Option<String>,
) -> Result<String, ApiError> {
    if block.is_some_and(|index| index >= BLOCKS) {
        return Err(limit());
    }
    let text = source(
        control,
        node_id,
        if block.is_some() {
            SOURCE_BYTES
        } else {
            MAX_BODY_BYTES as usize
        },
    )
    .await?;
    presentation_work(move || {
        if expected_hash
            .as_ref()
            .is_some_and(|expected| expected != &source_hash(&text))
        {
            return Err(CopyError::SourceChanged.error());
        }
        let result = if let Some(index) = block {
            crate::presentation::markdown_code_blocks(&text)
                .nth(index)
                .map(|b| b.text)
                .ok_or_else(|| CopyError::MissingBlock.error())?
        } else {
            text
        };
        if result.len() > MAX_BODY_BYTES as usize {
            return Err(limit());
        }
        Ok(result)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_copy_failures_are_english_without_changing_codes_or_source_reason() {
        let reason = "fixture reason: \u{4fdd}\u{5b58}\u{6e08}\u{307f} \u{25c9}";
        for (failure, code, expected) in [
            (CopyError::Limit, ErrorCode::InputLimit,
             "Copy retrieval limit exceeded. Press o to open the source and n to continue（also available through history get）".to_owned()),
            (CopyError::ReferenceChanged, ErrorCode::TargetUnavailable,
             "The source reference has changed".into()),
            (CopyError::Unavailable(reason.into()), ErrorCode::OutputExpired,
             format!("Cannot retrieve the source: {reason}")),
            (CopyError::Stalled, ErrorCode::IoFailed,
             "Source pagination is not advancing".into()),
            (CopyError::SourceChanged, ErrorCode::TargetUnavailable,
             "The source has changed. Press b to select the code block again".into()),
            (CopyError::MissingBlock, ErrorCode::InvalidArgument,
             "The selected code block does not exist".into()),
        ] {
            let error = failure.error();
            assert_eq!(error.code, code);
            assert_eq!(error.message, expected);
            assert!(error.hint.is_none());
            assert!(error.details.is_none());
        }
    }

    #[test]
    fn summaries_count_unicode_and_retain_parser_order_without_rendered_labels() {
        let items = summaries("text\n```rust\n界e\u{301}\n```\n\n    indented\n").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].chars, 4);
        assert_eq!(items[0].preview, "界e\u{301}");
        assert_eq!(items[1].index, 1);
        assert!(summaries(&"```\nx\n```\n\n".repeat(129)).is_err());
    }
}

#[cfg(test)]
mod image_guidance_tests {
    use super::*;
    #[test]
    fn image_failure_preserves_facts_and_offers_file_attachment_without_claiming_acquisition() {
        let mut error = ApiError::new(ErrorCode::InvalidState, "fixture image unavailable");
        error.hint = Some("original hint".into());
        error.details = Some(serde_json::json!({"fixture":true}));
        let result = super::super::image_acquisition_error(error.clone());
        assert_eq!(result.code, error.code);
        assert_eq!(result.details, error.details);
        assert_eq!(result.hint, error.hint);
        assert!(
            result.message.contains("fixture image unavailable")
                && result.message.contains("/attach")
        );
    }
}
