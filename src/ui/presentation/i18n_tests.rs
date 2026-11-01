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
        UiLanguage::En => &["printf fixture", "Exit 7"],
        UiLanguage::Ja => &["printf fixture", "終了 7"],
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
