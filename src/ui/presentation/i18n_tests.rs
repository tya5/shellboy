use super::*;
use crate::presentation::{MAX_METADATA_BYTES, project};
use crate::ui::{UiState, UiUpdate};
use serde_json::json;

fn node(kind: &str, id: &str, payload: serde_json::Value) -> UiNode {
    let text = crate::history::text_of_payload(&payload);
    UiNode {
        node_id: id.into(),
        role: if kind == "user" { "user" } else { "assistant" }.into(),
        total_chars: text.chars().count(),
        presentation: project(kind, &payload, &text, 0),
        text,
        ..Default::default()
    }
}
fn history(ui: &mut UiState, nodes: Vec<UiNode>) {
    ui.select_thread("fixture-thread".into());
    ui.apply(UiUpdate::History {
        thread_id: "fixture-thread".into(),
        generation: ui.view().generation,
        read_revision: 1,
        total_nodes: nodes.len() as u64,
        nodes,
        pending: Default::default(),
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
    });
}
fn conversation(ui: &mut UiState) {
    history(
        ui,
        vec![
            node("user", "u", json!({"text":"Synthetic request"})),
            node(
                "assistant_response",
                "a",
                json!({"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"Synthetic reasoning"}]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Synthetic answer"}]}]}),
            ),
        ],
    );
}
fn tool(ui: &mut UiState) {
    let saved = json!({"tool":"shell","call_id":"fixture-call","command":"printf fixture","status":"exited","exit_code":7,"elapsed_ms":400,"output":"Synthetic output"});
    history(
        ui,
        vec![node(
            "tool_result",
            "tool",
            json!({"item":{"type":"function_call_output","call_id":"fixture-call","output":saved.to_string()}}),
        )],
    );
}
fn recovery(ui: &mut UiState) {
    let saved = json!({"kind":"previous_instance_interrupted","change":{"result_uncertain":true,"automatic_replay":false,"runs":{"count":1}}});
    history(
        ui,
        vec![node(
            "interruption_context",
            "recovery",
            json!({"text":saved.to_string()}),
        )],
    );
}

#[test]
fn conversation_screen_bilingual() {
    crate::ui::i18n::verify_screen(conversation, |lang| match lang {
        UiLanguage::En => &["Reasoning", "Collapsed /reasoning", "Synthetic answer"],
        UiLanguage::Ja => &["推論", "折りたたみ /reasoning", "Synthetic answer"],
    });
}
#[test]
fn tool_screen_bilingual() {
    crate::ui::i18n::verify_screen(tool, |lang| match lang {
        UiLanguage::En | UiLanguage::Ja => &["printf fixture", "exit 7"],
    });
}
#[test]
fn recovery_screen_bilingual() {
    crate::ui::i18n::verify_screen(recovery, |lang| match lang {
        UiLanguage::En => &[
            "Previous interruption record",
            "Commands will not be replayed automatically",
        ],
        UiLanguage::Ja => &["前回の中断記録", "自動での再実行はしません"],
    });
}
#[test]
fn source_provider_error_and_protocol_are_not_translated() {
    let source = "推論 🙂 e\u{301} /retry Failed 状態変更 https://example.test/全文";
    let payload = json!({"text":source});
    let original = payload.clone();
    let value = node("user", "u", payload.clone());
    let serialized = serde_json::to_value(&value.presentation).unwrap();
    for lang in [UiLanguage::En, UiLanguage::Ja] {
        let mut targets = Vec::new();
        let _ = node_lines(lang, &value, false, 0, 80, &mut targets);
        assert_eq!(value.text, source);
        assert_eq!(value.presentation.blocks[0].text, source);
        assert_eq!(
            serde_json::to_value(&value.presentation).unwrap(),
            serialized
        );
        let error = crate::presentation::ToolPresentation {
            reason: source.into(),
            ..Default::default()
        };
        assert_eq!(
            crate::presentation::messages::tool_reason(&error, lang),
            source
        );
    }
    assert_eq!(payload, original);
    assert!(serialized.get("ui_messages").is_none());
}
#[test]
fn provenance_is_bounded_with_all_other_metadata_and_does_not_change_original() {
    let saved = json!({"kind":"previous_instance_interrupted","change":{"result_uncertain":true,"automatic_replay":false,"runs":{"count":1}}});
    let payload = json!({"text":saved.to_string(),"padding":"x".repeat(MAX_METADATA_BYTES * 2)});
    let before = payload.clone();
    let value = node("interruption_context", "r", payload.clone());
    assert!(value.presentation.metadata_bytes() <= MAX_METADATA_BYTES);
    assert!(value.presentation.ui_messages.retained_bytes() <= 8 * 1024);
    assert_eq!(value.text, crate::history::text_of_payload(&before));
    assert_eq!(payload, before);
    // Even when optional provenance is evicted, canonical safety facts are intact.
    assert!(
        value
            .presentation
            .blocks
            .iter()
            .any(|b| b.text.contains("自動での再実行はしません"))
    );
}

#[test]
fn generated_tool_failure_reason_is_localized_but_unknown_provider_text_is_verbatim() {
    use crate::presentation::{ToolPresentation, ToolReasonKind};
    let tool = ToolPresentation {
        reason: "先行する呼出しの失敗により未実行".into(),
        reason_kind: ToolReasonKind::PriorCallFailed,
        ..Default::default()
    };
    assert_eq!(
        crate::presentation::messages::tool_reason(&tool, UiLanguage::En),
        "Not executed because an earlier call failed"
    );
    assert_eq!(
        crate::presentation::messages::tool_reason(&tool, UiLanguage::Ja),
        tool.reason
    );
}

#[test]
fn displayed_values_clip_on_graphemes_in_terminal_cells_without_changing_source() {
    use crate::presentation::messages::display_value;
    use crate::ui::text_width::UnicodeWidthStr;
    let source = "e\u{301}e\u{301}界界🙂";
    for policy in [Some(1), Some(2), None] {
        crate::ui::text_width::set_terminal_width(policy);
        for width in 0..12 {
            let text = display_value(source, width);
            assert!(text.width() <= width);
            assert!(!text.ends_with("e."));
        }
    }
    crate::ui::text_width::clear_terminal_width();
    assert_eq!(source, "e\u{301}e\u{301}界界🙂");
}

#[test]
fn recovery_banner_preserves_actions_before_long_provider_reason_at_both_widths() {
    let snapshot = super::super::activity::ActivitySnapshot {
        phase: crate::activity::ActivityPhase::Recovery,
        recovery: Some(super::super::activity::RecoverySnapshot {
            message: "RAW provider explanation ".repeat(100),
            stage: "request".into(),
            live_shells: 1,
            uncollected_shells: 1,
            ..Default::default()
        }),
        ..Default::default()
    };
    for language in [UiLanguage::En, UiLanguage::Ja] {
        for width in [79, 119] {
            let lines = recovery_lines(&snapshot, language, width, 6);
            let shown = plain(&lines);
            assert!(shown.contains("/retry") && shown.contains("Ctrl+C") && shown.contains("F8"));
            assert!(lines.len() <= 6);
        }
    }
    assert_eq!(
        snapshot.recovery.unwrap().message,
        "RAW provider explanation ".repeat(100)
    );
}

#[test]
fn recovery_detail_localizes_only_generated_facts() {
    use crate::ui::activity::{
        ActivitySnapshot, ContextReductionSummary, RecoverySnapshot, RecoveryStage,
        RecoveryStageDetails, saved_recovery_reason_unavailable,
    };
    let snapshot = ActivitySnapshot {
        phase: crate::activity::ActivityPhase::Recovery,
        recovery: Some(RecoverySnapshot {
            stage: RecoveryStage::ModelContextLimit
                .localized(UiLanguage::Ja)
                .into(),
            stage_kind: Some(RecoveryStage::ModelContextLimit),
            stage_details: Some(RecoveryStageDetails {
                retries: json!(2),
                retry_limit: json!(3),
                request_bytes: json!(400),
                tool_bytes: json!(75),
            }),
            message: saved_recovery_reason_unavailable(UiLanguage::Ja).into(),
            message_is_fallback: true,
            ..Default::default()
        }),
        context_reduction: Some(ContextReductionSummary {
            before_characters: 90_000,
            after_characters: 40_000,
            spilled_groups: 3,
            omitted_items: 7,
        }),
        ..Default::default()
    };
    let english = plain(&recovery_lines(&snapshot, UiLanguage::En, 120, 20));
    assert!(english.contains("Model request (input capacity exceeded)"));
    assert!(english.contains("Review the saved recovery reason in Details."));
    assert!(english.contains("Context was auto-compacted from 90000 to 40000 characters"));
    let japanese = plain(&recovery_lines(&snapshot, UiLanguage::Ja, 120, 20));
    assert!(japanese.contains("モデル要求（入力容量超過）"));
    assert!(japanese.contains("保存された復旧理由を詳細で確認してください"));
    assert!(japanese.contains("自動縮小済み"));

    let source = ActivitySnapshot {
        phase: crate::activity::ActivityPhase::Recovery,
        recovery: Some(RecoverySnapshot {
            stage: "raw saved stage".into(),
            message: "Provider response: use the supplied reset time".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let shown = plain(&recovery_lines(&source, UiLanguage::En, 120, 16));
    assert!(shown.contains("raw saved stage"));
    assert!(shown.contains("Provider response: use the supplied reset time"));
}

#[test]
fn quiet_event_title_retains_changed_values_and_failure_facts_in_both_languages() {
    let saved = json!({"kind":"model_changed","change":{"before":{"model":"old-model"},"after":{"model":"new-model"}},"payload":{"state":"failed","output_complete":false}});
    let value = node("event", "event", json!({"text":saved.to_string()}));
    for language in [UiLanguage::En, UiLanguage::Ja] {
        let title = label(language, &value);
        assert!(title.contains("old-model") && title.contains("new-model"));
        assert!(title.contains(if language == UiLanguage::En {
            "Failed"
        } else {
            "失敗"
        }));
        assert!(title.contains(if language == UiLanguage::En {
            "Output incomplete"
        } else {
            "出力不完全"
        }));
    }
}

fn recovery_banner(ui: &mut UiState) {
    history(ui, Vec::new());
    ui.apply(UiUpdate::Activity {
        thread_id: "fixture-thread".into(),
        generation: ui.view().generation,
        epoch: 1,
        observed_at: std::time::Instant::now(),
        snapshot: super::super::activity::ActivitySnapshot {
            phase: crate::activity::ActivityPhase::Recovery,
            recovery: Some(super::super::activity::RecoverySnapshot {
                stage: "request".into(),
                message: "Synthetic provider failure".into(),
                pending_messages: 1,
                saved_tool_results: 2,
                live_shells: 1,
                uncollected_shells: 1,
                ..Default::default()
            }),
            ..Default::default()
        },
    });
}
#[test]
fn actual_recovery_banner_screen_bilingual() {
    crate::ui::i18n::verify_screen(recovery_banner, |lang| match lang {
        UiLanguage::En => &[
            "Awaiting recovery",
            "Synthetic provider failure",
            "/retry",
            "Ctrl+C",
            "F8",
        ],
        UiLanguage::Ja => &[
            "復旧待ち",
            "Synthetic provider failure",
            "/retry",
            "Ctrl+C",
            "F8",
        ],
    });
}

#[test]
fn post_enrichment_bounder_drops_translation_before_changing_canonical_projection() {
    use crate::presentation::messages::Message;
    use crate::presentation::{NodePresentation, PresentationField};
    let mut value = NodePresentation::default();
    for index in 0..80 {
        let mut key = format!("/extra/{index}");
        key.shrink_to_fit();
        value.fields.push(PresentationField {
            key,
            value: "x".repeat(600),
            ..Default::default()
        });
    }
    value.fields.shrink_to_fit();
    assert!(value.metadata_bytes() < MAX_METADATA_BYTES);
    let mut canonical = value.clone();
    // Enrichment already happened; this is a direct call to the shared bounder,
    // not the earlier projection::limits::finish path.
    value.ui_messages.title = Some(Message::Raw {
        text: "t".repeat(8 * 1024),
    });
    assert!(value.metadata_bytes() > MAX_METADATA_BYTES);
    value.bound_metadata();
    canonical.bound_metadata();
    assert!(value.ui_messages.limited);
    assert!(value.ui_messages.title.is_none());
    assert!(value.metadata_bytes() <= MAX_METADATA_BYTES);
    assert!(!value.metadata_limited && !value.details_omitted);
    assert_eq!(
        serde_json::to_value(&value).unwrap(),
        serde_json::to_value(&canonical).unwrap()
    );
}

#[test]
fn canonical_only_post_enrichment_pressure_still_runs_the_existing_bounder() {
    use crate::presentation::{NodePresentation, PresentationField};
    let mut value = NodePresentation::default();
    for index in 0..128 {
        value.fields.push(PresentationField {
            key: format!("/extra/{index}"),
            value: "x".repeat(600),
            ..Default::default()
        });
    }
    assert!(value.metadata_bytes() > MAX_METADATA_BYTES);
    value.bound_metadata();
    assert!(value.metadata_bytes() <= MAX_METADATA_BYTES);
    assert!(value.metadata_limited && value.details_omitted);
}

#[test]
fn typed_current_notice_caption_preserves_raw_command_and_title_in_both_languages() {
    use crate::presentation::messages::{Message, Text};
    let saved = json!({"kind":"child_report","payload":{"thread_id":"child"}});
    let mut value = node("event", "n", json!({"text":saved.to_string()}));
    value
        .presentation
        .fields
        .push(crate::presentation::PresentationField {
            key: "/ui_notice/title".into(),
            value: "保存された表示 | 現在の名前: 利用者の題名".into(),
            ..Default::default()
        });
    let before = serde_json::to_value(&value.presentation).unwrap();
    assert!(value.presentation.ui_messages.set_notice(
        Message::ThreadNotice {
            title: Some("利用者の題名 🙂".into())
        },
        None
    ));
    assert!(label(UiLanguage::En, &value).contains("Current name: 利用者の題名 🙂"));
    assert!(label(UiLanguage::Ja, &value).contains("現在の名前: 利用者の題名"));
    assert_eq!(serde_json::to_value(&value.presentation).unwrap(), before);
    let command = Message::CommandNotice {
        command: Some("printf '背景指定あり 🙂'".into()),
        background: Some(false),
    };
    assert_eq!(
        command.render(UiLanguage::En),
        "printf '背景指定あり 🙂' | Background not requested"
    );
    assert_eq!(
        command.render(UiLanguage::Ja),
        "printf '背景指定あり 🙂' | 背景指定なし"
    );
    value
        .presentation
        .ui_messages
        .set_notice(Message::Text(Text::NoticeDeletedThread), None);
    assert!(label(UiLanguage::En, &value).contains("Referenced conversation was deleted"));
}

fn source_error_after_enter(ui: &mut UiState) {
    use crate::presentation::messages::{Message, Text};
    let saved = json!({"kind":"shell_ended","payload":{"execution_id":"missing-execution"}});
    let mut value = node("event", "notice", json!({"text":saved.to_string()}));
    for (key, text) in [
        ("execution_id", "missing-execution"),
        ("source_error", "元の結果を参照できません"),
        ("title", "Synthetic saved notice"),
    ] {
        value
            .presentation
            .fields
            .push(crate::presentation::PresentationField {
                key: format!("/ui_notice/{key}"),
                value: text.into(),
                ..Default::default()
            });
    }
    value.presentation.ui_messages.set_notice(
        Message::Text(Text::NoticeCallUnavailable),
        Some(Text::NoticeSourceUnavailable),
    );
    history(ui, vec![value]);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 28)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, ui)).unwrap();
    for code in [
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyCode::Enter,
    ] {
        assert!(
            ui.event(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
            ))
            .is_empty()
        );
    }
    assert_eq!(
        ui.view().status,
        Text::NoticeSourceUnavailable.render(ui.config.ui.language)
    );
    assert!(ui.view().operation_notice.is_some());
    assert!(ui.details.is_none());
}
#[test]
fn typed_source_error_after_enter_is_visible_on_the_actual_bilingual_screen() {
    crate::ui::i18n::verify_screen(source_error_after_enter, |lang| match lang {
        UiLanguage::En => &["Source result is unavailable"],
        UiLanguage::Ja => &["元の結果を参照できません"],
    });
}

fn activity_fixture(
    ui: &mut UiState,
    phase: crate::activity::ActivityPhase,
    retry: bool,
    waiting: bool,
    quota: bool,
) {
    history(ui, Vec::new());
    let mut snapshot = super::super::activity::ActivitySnapshot {
        phase,
        run_id: Some("fixture-run".into()),
        turn_id: Some("fixture-turn".into()),
        turn_in_progress: !waiting,
        ..Default::default()
    };
    if retry {
        snapshot.automatic_retry = Some(super::super::activity::AutomaticRetrySnapshot {
            number: 1,
            limit: 3,
            delay_ms: 1501,
            partial_response_discarded: true,
        });
    }
    if waiting {
        snapshot.waiting_reason = Some(crate::protocol::RunWaitReason {
            kind: "context_edit_boundary".into(),
            operation_id: "fixture-operation".into(),
            action: "summary".into(),
            accepted_at: "saved".into(),
            operation_count: 1,
        });
    }
    if quota {
        snapshot.recovery = Some(super::super::activity::RecoverySnapshot {
            stage: "request".into(),
            message: "Synthetic limit".into(),
            usage_limit: Some(crate::model::usage_limit::UsageLimit {
                kind: crate::model::usage_limit::LimitKind::UsageLimitReached,
                resets_at: 1_800_000_000,
                limit_window_minutes: Some(60),
                locally_blocked: true,
            }),
            ..Default::default()
        });
    }
    ui.apply(UiUpdate::Activity {
        thread_id: "fixture-thread".into(),
        generation: ui.view().generation,
        epoch: 1,
        observed_at: std::time::Instant::now(),
        snapshot,
    });
}
fn preparing(ui: &mut UiState) {
    activity_fixture(
        ui,
        crate::activity::ActivityPhase::Preparing,
        false,
        false,
        false,
    );
}
fn automatic_retry(ui: &mut UiState) {
    activity_fixture(
        ui,
        crate::activity::ActivityPhase::Retrying,
        true,
        false,
        false,
    );
}
fn context_wait(ui: &mut UiState) {
    activity_fixture(
        ui,
        crate::activity::ActivityPhase::Waiting,
        false,
        true,
        false,
    );
}
fn quota_wait(ui: &mut UiState) {
    activity_fixture(
        ui,
        crate::activity::ActivityPhase::Recovery,
        false,
        false,
        true,
    );
}
fn receiving(ui: &mut UiState) {
    activity_fixture(
        ui,
        crate::activity::ActivityPhase::Receiving,
        false,
        false,
        false,
    );
}
#[test]
fn preparing_run_footer_screen_bilingual() {
    crate::ui::i18n::verify_screen(preparing, |lang| match lang {
        UiLanguage::En => &["Preparing", "Observed"],
        UiLanguage::Ja => &["準備中", "観測"],
    });
}
#[test]
fn retry_run_footer_keeps_billing_consequence_on_bilingual_screen() {
    crate::ui::i18n::verify_screen(automatic_retry, |lang| match lang {
        UiLanguage::En => &["Re-sending may be billed twice"],
        UiLanguage::Ja => &["再送は重複課金の可能性"],
    });
}
#[test]
fn context_wait_run_footer_screen_bilingual() {
    crate::ui::i18n::verify_screen(context_wait, |lang| match lang {
        UiLanguage::En => &["Waiting for conversation summary"],
        UiLanguage::Ja => &["会話要約の完了待ち"],
    });
}
#[test]
fn quota_run_footer_keeps_manual_retry_and_reset_on_bilingual_screen() {
    crate::ui::i18n::verify_screen(quota_wait, |lang| match lang {
        UiLanguage::En => &["Usage limit", "UTC", "manual", "/model"],
        UiLanguage::Ja => &["使用量上限", "UTC", "明示的に再試行", "/model"],
    });
}
#[test]
fn receiving_run_footer_screen_bilingual() {
    crate::ui::i18n::verify_screen(receiving, |lang| match lang {
        UiLanguage::En => &["Receiving answer"],
        UiLanguage::Ja => &["応答受信中"],
    });
}

fn quota_and_retry(ui: &mut UiState) {
    activity_fixture(
        ui,
        crate::activity::ActivityPhase::Recovery,
        true,
        false,
        true,
    );
}
#[test]
fn simultaneous_quota_and_retry_keep_both_critical_facts_visible_bilingually() {
    crate::ui::i18n::verify_screen(quota_and_retry, |lang| match lang {
        UiLanguage::En => &["Re-sending may be billed twice", "UTC", "manual"],
        UiLanguage::Ja => &["再送は重複課金の可能性", "UTC", "明示的に再試行"],
    });
}

fn index_pause_with_saved_time(ui: &mut UiState) {
    let value = json!({"kind":"index_state","change":{"before":{"paused":false},"after":{"paused":true,"paused_at":"2027-01-15T08:00:00.123456789+00:00","pause_kind":"fixture_pause"}}});
    history(
        ui,
        vec![node("event", "pause", json!({"text":value.to_string()}))],
    );
}
#[test]
fn index_pause_keeps_saved_timestamp_reason_and_retry_binding_bilingually() {
    crate::ui::i18n::verify_screen(index_pause_with_saved_time, |_| {
        if crate::ui::text_width::glyphs::narrow() {
            &[
                "/index → T",
                "2027-01-15T08:00:00.123456789+00:00",
                "fixture_pause",
            ]
        } else {
            &[
                "/index > T",
                "2027-01-15T08:00:00.123456789+00:00",
                "fixture_pause",
            ]
        }
    });
}
fn resumed_saved_failures(ui: &mut UiState) {
    let mut value = node(
        "event",
        "resume",
        json!({"text":json!({"kind":"recovery_resumed"}).to_string()}),
    );
    value.presentation.title = "復旧再開 | 再開前にこの応答処理で記録したモデル失敗2件".into();
    value.presentation.blocks[0].text = "直近の保存理由: Raw saved failure".into();
    for (key, text) in [("count", "2"), ("error/message", "Raw saved failure")] {
        value
            .presentation
            .fields
            .push(crate::presentation::PresentationField {
                key: format!("/saved_model_failures/{key}"),
                value: text.into(),
                ..Default::default()
            });
    }
    history(ui, vec![value]);
}
#[test]
fn resumed_saved_failure_count_and_raw_reason_survive_provenance_in_both_languages() {
    crate::ui::i18n::verify_screen(resumed_saved_failures, |language| match language {
        UiLanguage::En => &["2 model failures recorded", "Raw saved failure"],
        UiLanguage::Ja => &["記録したモデル失敗2件", "Raw saved failure"],
    });
}

#[test]
fn content_filter_banner_keeps_provider_category_and_recovery_actions_in_both_languages() {
    use crate::ui::activity::{ActivitySnapshot, RecoverySnapshot};
    let facts = crate::model::content_filter::ContentFilterFacts::from_response(&json!({
        "incomplete_details":{"reason":"content_filter"},
        "content_filter_results":{"protected_material_code":{"filtered":true,"detected":true}}
    }));
    for language in [UiLanguage::En, UiLanguage::Ja] {
        for width in [80, 120] {
            let snapshot = ActivitySnapshot {
                phase: crate::activity::ActivityPhase::Recovery,
                recovery: Some(RecoverySnapshot {
                    content_filter: Some(facts.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let lines = recovery_lines(&snapshot, language, width, 6);
            let text = lines
                .iter()
                .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
                .collect::<String>();
            for expected in [
                "content_filter",
                "protected_material_code",
                "/retry",
                "/detach",
                "cp",
                "curl",
                "F8",
            ] {
                assert!(
                    text.contains(expected),
                    "{language:?}/{width}: missing {expected}: {text}"
                );
            }
        }
    }
}
