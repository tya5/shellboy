//! Bounded canonical copy preparation in existing read/presentation workers.
use super::*;
use crate::ui::{CopyBlock, CopyOffer};
use sha2::{Digest, Sha256};
const SOURCE_BYTES: usize = 1024 * 1024;
const SOURCE_PAGES: usize = 64;
const BLOCKS: usize = 128;

fn limit() -> ApiError {
    ApiError::new(
        ErrorCode::InputLimit,
        "コピーの取得上限を超えました。oで原文を開き、nで続きを確認してください（history getでも取得できます）",
    )
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
            return Err(ApiError::new(
                ErrorCode::TargetUnavailable,
                "原文の参照先が変わりました",
            ));
        }
        if let Some(reason) = body.tombstone_reason {
            return Err(ApiError::new(
                ErrorCode::OutputExpired,
                format!("原文を取得できません: {reason}"),
            ));
        }
        if text.len().saturating_add(body.text.len()) > budget {
            return Err(limit());
        }
        text.push_str(&body.text);
        let Some(next) = body.next_cursor else {
            return Ok(text);
        };
        if !seen.insert(next.clone()) {
            return Err(ApiError::new(ErrorCode::IoFailed, "原文の続きが進みません"));
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
    format!("{:x}", Sha256::digest(text.as_bytes()))
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
            return Err(ApiError::new(
                ErrorCode::TargetUnavailable,
                "原文が更新されました。bでコードの塊を選び直してください",
            ));
        }
        let result = if let Some(index) = block {
            crate::presentation::markdown_code_blocks(&text)
                .nth(index)
                .map(|b| b.text)
                .ok_or_else(|| {
                    ApiError::new(ErrorCode::InvalidArgument, "指定したコードの塊がありません")
                })?
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
