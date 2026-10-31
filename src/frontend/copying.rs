//! Bounded canonical copy preparation in existing read/presentation workers.
use super::*;
use crate::ui::CopyBlock;
const SOURCE_BYTES: usize = 1024 * 1024;
const SOURCE_PAGES: usize = 64;
const BLOCKS: usize = 128;

fn limit() -> ApiError {
    ApiError::new(ErrorCode::InputLimit,
        "コピーの取得上限を超えました。oで原文を開き、nで続きを確認してください（history getでも取得できます）")
}
async fn source(control: &Control, node_id: String, budget: usize) -> Result<String, ApiError> {
    let history = crate::history::History::new(control.store.database.clone());
    let mut text = String::new();
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..SOURCE_PAGES {
        let body = history.get(HistoryGetArgs { final_answer_only: false,
            node_id: node_id.clone(), limit_bytes: MAX_BODY_BYTES,
            cursor, encoding: DEFAULT_ENCODING.into() }).await?;
        if body.node_id != node_id { return Err(ApiError::new(ErrorCode::TargetUnavailable, "原文の参照先が変わりました")); }
        if let Some(reason) = body.tombstone_reason {
            return Err(ApiError::new(ErrorCode::OutputExpired, format!("原文を取得できません: {reason}")));
        }
        if text.len().saturating_add(body.text.len()) > budget { return Err(limit()); }
        text.push_str(&body.text);
        let Some(next) = body.next_cursor else { return Ok(text); };
        if !seen.insert(next.clone()) { return Err(ApiError::new(ErrorCode::IoFailed, "原文の続きが進みません")); }
        cursor = Some(next);
    }
    Err(limit())
}
fn summaries(text: &str) -> Result<Vec<CopyBlock>, ApiError> {
    let mut blocks = Vec::new();
    for block in crate::presentation::markdown_code_blocks(text) {
        if blocks.len() == BLOCKS { return Err(limit()); }
        blocks.push(CopyBlock {
            index: blocks.len(),
            chars: block.text.chars().count(),
            preview: crate::presentation::sanitize_plain(
                &block.text.lines().next().unwrap_or("").chars().take(80).collect::<String>(), 0).0,
        });
    }
    Ok(blocks)
}
pub(super) async fn choices(control: &Control, node_id: String) -> Result<Vec<CopyBlock>, ApiError> {
    let text = source(control, node_id, SOURCE_BYTES).await?;
    presentation_work(move || summaries(&text)).await?
}
pub(super) async fn text(control: &Control, node_id: String, block: Option<usize>) -> Result<String, ApiError> {
    let text = source(control, node_id, if block.is_some() { SOURCE_BYTES } else { MAX_BODY_BYTES as usize }).await?;
    if let Some(index) = block {
        presentation_work(move || {
            let text = crate::presentation::markdown_code_blocks(&text).nth(index)
                .map(|b| b.text).ok_or_else(|| ApiError::new(ErrorCode::InvalidArgument, "指定したコードの塊がありません"))?;
            if text.len() > MAX_BODY_BYTES as usize { return Err(limit()); }
            Ok(text)
        }).await?
    } else { Ok(text) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summaries_count_unicode_and_retain_parser_order_without_rendered_labels() {
        let items = summaries("text\n```rust\n界e\u{301}\n```\n\n    indented\n").unwrap();
        assert_eq!(items.len(),2);
        assert_eq!(items[0].chars,4);
        assert_eq!(items[0].preview,"界e\u{301}");
        assert_eq!(items[1].index,1);
        assert!(summaries(&"```\nx\n```\n\n".repeat(129)).is_err());
    }
}
