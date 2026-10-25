//! JSON Pointer segment quoting. Splitting/traversal remains with each caller.
pub(crate) fn escape_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}
pub(crate) fn unescape_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_segments_resolve_the_actual_value_and_round_trip_without_normalization() {
        for segment in [
            "",
            "日本語",
            "a/b",
            "~key",
            "~1",
            "~01/~",
            "é/e\u{301}",
            "😀/\n~",
        ] {
            let root = serde_json::json!({(segment):{"value":"original"}});
            let encoded = escape_segment(segment);
            assert_eq!(
                root.pointer(&format!("/{encoded}/value")),
                Some(&serde_json::json!("original"))
            );
            assert_eq!(unescape_segment(&encoded), segment);
        }
    }
    #[test]
    fn unescape_keeps_the_existing_literal_unknown_escape_behavior() {
        assert_eq!(unescape_segment("~01"), "~1");
        assert_eq!(unescape_segment("~2/~"), "~2/~");
    }
}
