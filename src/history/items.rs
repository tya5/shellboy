//! Authoritative saved item projection for opt-in history windows.
use crate::protocol::{HistoryExecutionLink, HistoryItem};
use serde_json::Value;

pub const NODE_SOURCE_BYTES: usize = 1024 * 1024;
pub const ITEM_TEXT_BYTES: usize = 16 * 1024;
pub const RESPONSE_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const ITEM_LINK_LIMIT: usize = 8;

pub fn project(_node: &str, _kind: &str, _payload: &Value, _links: &[HistoryExecutionLink]) -> Vec<HistoryItem> {
    Vec::new()
}

pub fn text_bytes(items: &[HistoryItem]) -> usize {
    items.iter().map(|item| match item { HistoryItem::Text { text, .. } => text.len(), _ => 0 }).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn mixed_saved_order_and_secrets() {
        let payload = json!({"output":[
            {"type":"message","content":[{"type":"output_text","text":"first"}]},
            {"type":"function_call","call_id":"call","arguments":"SECRET_ARGS"},
            {"type":"reasoning","summary":[{"text":"SECRET_REASON"}],"encrypted_content":"SECRET_OPAQUE"},
            {"type":"message","content":[{"type":"output_text","text":""},{"type":"refusal","refusal":"last"}]}
        ]});
        let items = project("node", "assistant_response", &payload, &[]);
        assert_eq!(items.len(), 5);
        assert!(matches!(&items[0], HistoryItem::Text{text,..} if text=="first"));
        assert!(matches!(&items[1], HistoryItem::ToolCall{..}));
        assert!(matches!(&items[2], HistoryItem::ReasoningUnavailable{..}));
        assert!(matches!(&items[3], HistoryItem::Text{text,..} if text.is_empty()));
        assert!(matches!(&items[4], HistoryItem::Text{text,..} if text=="last"));
        assert!(!serde_json::to_string(&items).unwrap().contains("SECRET"));
    }
    #[test]
    fn utf8_prefix_and_unknown_barrier() {
        let payload = json!({"output":[{"type":"message","content":[{"type":"output_text","text":"界".repeat(6000)}]},{"type":"future","text":"SECRET_UNKNOWN"}]});
        let items = project("node", "assistant_response", &payload, &[]);
        assert!(matches!(&items[0], HistoryItem::Text{text,text_complete:false,..} if text.len()<=ITEM_TEXT_BYTES && text.len()%3==0));
        assert!(matches!(&items[1], HistoryItem::Unavailable{..}));
    }
}
