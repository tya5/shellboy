//! Human label for the existing saved safe-boundary owner, not a new phase.
pub(crate) fn label(reason: &crate::protocol::RunWaitReason) -> String {
    let (_, text) = match reason.action.as_str() {
        "summary" => ("ui.activity.wait.summary", "会話要約の完了待ち"),
        "spill" => ("ui.activity.wait.spill", "出力退避の完了待ち"),
        "restore" => ("ui.activity.wait.restore", "履歴復元の完了待ち"),
        "omit" | "trim" | "maximal" => ("ui.activity.wait.edit", "履歴編集の完了待ち"),
        _ => ("ui.activity.wait.compact", "会話圧縮の完了待ち"),
    };
    text.into()
}

pub(crate) fn label_localized(
    reason: &crate::protocol::RunWaitReason,
    language: crate::config::UiLanguage,
) -> String {
    if language == crate::config::UiLanguage::Ja {
        return label(reason);
    }
    match reason.action.as_str() {
        "summary" => "Waiting for conversation summary",
        "spill" => "Waiting for output spill",
        "restore" => "Waiting for history restoration",
        "omit" | "trim" | "maximal" => "Waiting for history editing",
        _ => "Waiting for conversation compaction",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_wait_actions_select_language_without_changing_saved_reason() {
        let mut reason = crate::protocol::RunWaitReason {
            kind: "context_edit_boundary".into(),
            operation_id: "opaque-operation".into(),
            action: String::new(),
            accepted_at: "saved-time".into(),
            operation_count: 7,
        };
        for (action, expected) in [
            ("summary", "Waiting for conversation summary"),
            ("spill", "Waiting for output spill"),
            ("restore", "Waiting for history restoration"),
            ("omit", "Waiting for history editing"),
            ("trim", "Waiting for history editing"),
            ("maximal", "Waiting for history editing"),
            ("unknown-provider-action", "Waiting for conversation compaction"),
        ] {
            reason.action = action.into();
            assert_eq!(label_localized(&reason, crate::config::UiLanguage::En), expected);
            assert_eq!(label_localized(&reason, crate::config::UiLanguage::Ja), label(&reason));
            assert_eq!(reason.action, action);
            assert_eq!(reason.operation_id, "opaque-operation");
            assert_eq!(reason.accepted_at, "saved-time");
            assert_eq!(reason.operation_count, 7);
        }
    }
    #[test]
    fn labels_describe_saved_owner_not_background_completion_or_time() {
        let mut reason = crate::protocol::RunWaitReason {
            kind: "context_edit_boundary".into(),
            operation_id: "synthetic".into(),
            action: "summary".into(),
            accepted_at: "saved".into(),
            operation_count: 1,
        };
        assert_eq!(label(&reason), "会話要約の完了待ち");
        assert_eq!(
            label_localized(&reason, crate::config::UiLanguage::En),
            "Waiting for conversation summary"
        );
        assert_eq!(
            label_localized(&reason, crate::config::UiLanguage::Ja),
            "会話要約の完了待ち"
        );
        reason.action = "compact".into();
        assert_eq!(label(&reason), "会話圧縮の完了待ち");
    }
}
