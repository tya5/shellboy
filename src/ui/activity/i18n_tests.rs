use super::*;

#[test]
fn phase_display_is_bilingual_without_changing_phase_or_motion() {
    let now = Instant::now();
    let animation = Animation::new(now);
    for phase in [
        ActivityPhase::Idle,
        ActivityPhase::Sending,
        ActivityPhase::Reflecting,
        ActivityPhase::Preparing,
        ActivityPhase::AwaitingModel,
        ActivityPhase::Retrying,
        ActivityPhase::Receiving,
        ActivityPhase::Tools,
        ActivityPhase::Running,
        ActivityPhase::Waiting,
        ActivityPhase::Background,
        ActivityPhase::Continuing,
        ActivityPhase::Stopping,
        ActivityPhase::Recovery,
        ActivityPhase::Failed,
        ActivityPhase::Stopped,
        ActivityPhase::Completed,
    ] {
        let before = (phase.label(), phase.motion(), phase.is_active());
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let shown = animation
                .spans_localized(phase, Style::default(), now, language)
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>();
            assert!(shown.contains(phase_label(phase, language)));
        }
        assert_eq!((phase.label(), phase.motion(), phase.is_active()), before);
    }
}
#[test]
fn retry_display_keeps_billing_consequence_before_optional_counts_and_preserves_facts() {
    for partial in [false, true] {
        let retry = AutomaticRetrySnapshot {
            number: 1,
            limit: 3,
            delay_ms: 1501,
            partial_response_discarded: partial,
        };
        let before = retry.clone();
        assert!(
            retry
                .label_localized(UiLanguage::En)
                .starts_with("Re-sending may be billed twice")
        );
        assert!(retry.label_localized(UiLanguage::En).contains("1/3 in 2s"));
        assert!(
            retry
                .label_localized(UiLanguage::Ja)
                .starts_with("再送は重複課金の可能性")
        );
        assert!(retry.label_localized(UiLanguage::Ja).contains("2秒待機"));
        assert_eq!(
            retry
                .label_localized(UiLanguage::En)
                .contains("Partial response saved"),
            partial
        );
        assert_eq!(retry, before);
    }
}
#[test]
fn quota_summary_keeps_provider_reset_timestamp_window_and_raw_message() {
    use crate::model::usage_limit::{LimitKind, UsageLimit};
    for kind in [LimitKind::UsageLimitReached, LimitKind::RateLimitRetryAfter] {
        for window in [None, Some(1), Some(60)] {
            let recovery = RecoverySnapshot {
                message: "素文 provider error 🙂".into(),
                usage_limit: Some(UsageLimit {
                    kind,
                    resets_at: 1_800_000_000,
                    limit_window_minutes: window,
                    locally_blocked: true,
                }),
                ..Default::default()
            };
            let before = recovery.clone();
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let shown = recovery.limit_summary_localized(language).unwrap();
                assert!(shown.contains("UTC"));
                assert!(!shown.contains("automatically"));
            }
            assert_eq!(recovery, before);
        }
    }
    let invalid = RecoverySnapshot {
        usage_limit: Some(UsageLimit {
            kind: LimitKind::UsageLimitReached,
            resets_at: i64::MAX,
            limit_window_minutes: None,
            locally_blocked: true,
        }),
        ..Default::default()
    };
    assert_eq!(invalid.limit_summary_localized(UiLanguage::En), None);
    assert!(
        FooterText::RecoveryActions
            .localized(UiLanguage::En)
            .contains("manual")
    );
}
#[test]
fn localized_waiting_labels_do_not_mutate_saved_owner_protocol() {
    for action in [
        "summary", "spill", "restore", "omit", "trim", "maximal", "future",
    ] {
        let reason = crate::protocol::RunWaitReason {
            kind: "context_edit_boundary".into(),
            operation_id: "fixture".into(),
            action: action.into(),
            accepted_at: "saved".into(),
            operation_count: 1,
        };
        let before = serde_json::to_value(&reason).unwrap();
        assert!(context_wait_label_localized(&reason, UiLanguage::En).starts_with("Waiting for"));
        assert_eq!(
            context_wait_label_localized(&reason, UiLanguage::Ja),
            context_wait_label(&reason)
        );
        assert_eq!(serde_json::to_value(&reason).unwrap(), before);
    }
}
#[test]
fn observed_age_and_unknown_saved_age_remain_distinct_in_both_languages() {
    let now = Instant::now();
    let mut view = ActivityView::default();
    view.accept_at(
        1,
        ActivitySnapshot {
            phase: ActivityPhase::Preparing,
            run_id: Some("r".into()),
            turn_id: Some("u".into()),
            turn_in_progress: true,
            ..Default::default()
        },
        now,
    );
    let before = view.snapshot.clone();
    assert_eq!(
        view.phase_time_note_localized(now + Duration::from_secs(3), UiLanguage::En)
            .as_deref(),
        Some("Observed 3s")
    );
    assert_eq!(
        view.phase_time_note_localized(now + Duration::from_secs(3), UiLanguage::Ja)
            .as_deref(),
        Some("観測 3秒")
    );
    assert_eq!(view.snapshot, before);
    view.accept_at(
        2,
        ActivitySnapshot {
            phase: ActivityPhase::AwaitingModel,
            run_id: Some("r".into()),
            turn_id: Some("u".into()),
            attempt_id: Some("a".into()),
            timing: SavedTiming {
                attempt_started_at: Some("invalid".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        now,
    );
    assert_eq!(
        view.phase_time_note_localized(now + Duration::from_secs(999), UiLanguage::En)
            .as_deref(),
        Some("Elapsed time unavailable")
    );
    assert_eq!(
        view.phase_time_note_localized(now + Duration::from_secs(999), UiLanguage::Ja)
            .as_deref(),
        Some("経過未取得")
    );
}

#[test]
fn recovery_diagnostics_have_complete_localized_text_and_keep_raw_messages() {
    let stage = RecoverySnapshot {
        stage: RecoveryStage::ModelContextLimit
            .localized(UiLanguage::Ja)
            .to_owned(),
        stage_kind: Some(RecoveryStage::ModelContextLimit),
        stage_details: Some(RecoveryStageDetails {
            retries: serde_json::json!(2),
            retry_limit: serde_json::json!(3),
            request_bytes: serde_json::json!(400),
            tool_bytes: serde_json::json!(75),
        }),
        message: saved_recovery_reason_unavailable(UiLanguage::Ja).into(),
        message_is_fallback: true,
        ..Default::default()
    };
    let english = stage.stage_localized(UiLanguage::En);
    assert_eq!(
        english,
        "Model request (input capacity exceeded) | Retry 2/3; request: 400 B; tool: 75 B"
    );
    assert!(english.is_ascii());
    assert_eq!(
        stage.message_localized(UiLanguage::En),
        "Review the saved recovery reason in Details."
    );
    assert_eq!(
        stage.message_localized(UiLanguage::Ja),
        "保存された復旧理由を詳細で確認してください"
    );
    let separator = crate::ui::text_width::glyphs::separator();
    assert_eq!(
        stage.stage_localized(UiLanguage::Ja),
        format!(
            "モデル要求（入力容量超過）{separator}retry 2/3{separator}request 400B{separator}tool 75B"
        )
    );

    let raw = RecoverySnapshot {
        message: "Provider response: use the supplied reset time".into(),
        ..Default::default()
    };
    assert_eq!(
        raw.message_localized(UiLanguage::En),
        "Provider response: use the supplied reset time"
    );
    assert_eq!(
        raw.message_localized(UiLanguage::Ja),
        "Provider response: use the supplied reset time"
    );
}

#[test]
fn context_reduction_facts_are_localized_only_for_display() {
    let summary = ContextReductionSummary {
        before_characters: 90_000,
        after_characters: 40_000,
        spilled_groups: 3,
        omitted_items: 7,
    };
    assert!(
        summary
            .localized(UiLanguage::En)
            .contains("from 90000 to 40000 characters")
    );
    assert!(
        summary
            .localized(UiLanguage::En)
            .contains("3 groups and omitting 7 items")
    );
    assert!(summary.localized(UiLanguage::Ja).contains("spill 3組"));
    assert!(summary.localized(UiLanguage::Ja).contains("省略 7件"));
}
