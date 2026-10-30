use serde_json::json;
use shellboy::{history::text_of_payload, presentation::*};

#[test]
fn trim_markers_remain_visible_replacement_notices_with_immutable_source_text() {
    let notice = "Input prefix removed: first..last. Files and shell effects are unchanged. Use shellboy history get --node first to inspect only needed context.";
    let payload = json!({
        "context_trim":{"version":1,"omitted_nodes":2,"first_node_id":"first","last_node_id":"last","estimated_tokens":1000},
        "source_node_ids":["first","last"],
        "item":{"role":"developer","content":[{"type":"input_text","text":notice}]},
    });
    let before = payload.clone();
    let text = text_of_payload(&payload);
    assert_eq!(text, notice);
    for kind in ["spill", "context_trim"] {
        let projected = project(kind, &payload, &text, 0);
        assert_eq!(projected.kind, PresentationKind::Spill);
        assert_eq!(projected.title, "入力範囲を縮小（原文は保持）");
        assert!(
            projected
                .blocks
                .iter()
                .any(|block| block.text.contains("Files and shell effects"))
        );
        assert!(
            projected
                .fields
                .iter()
                .any(|field| field.key == "/context_trim/omitted_nodes" && field.value == "2")
        );
    }
    let user = project("user", &payload, &text, 0);
    assert_eq!(
        user.kind,
        PresentationKind::User,
        "user data cannot masquerade as a saved trim marker"
    );
    let plain_spill = project("spill", &json!({"context_trim":false}), "ordinary spill", 0);
    assert_eq!(plain_spill.title, "原文参照");
    assert_eq!(payload, before);
}

#[test]
fn persisted_result_notifications_explain_continuation_and_keep_the_original_evidence() {
    for (kind, expected) in [
        ("shell_ended", "コマンド結果を受けて続行"),
        ("child_turn_completed", "子の回答を受けて続行"),
        ("child_run_ended", "子の作業終了を受けて続行"),
        ("child_report", "子の途中報告を受けて続行"),
    ] {
        let event = json!({"event_id":"saved-notification","kind":kind,"payload":{"execution_id":"source-execution","thread_id":"source-thread"}});
        let payload =
            json!({"content":[{"type":"input_text","text":format!("Shellboy event v1: {event}")}]});
        let original = payload.clone();
        let p = project("event", &payload, &text_of_payload(&payload), 0);
        assert_eq!(p.title, expected, "{kind}");
        assert!(
            p.fields
                .iter()
                .any(|field| field.key == "/event_id" && field.value == "saved-notification")
        );
        assert_eq!(payload, original);
        let user = project("user", &payload, &text_of_payload(&payload), 0);
        assert!(
            !user.title.contains("続行"),
            "ordinary user text is not a notification"
        );
    }
}

#[test]
fn missing_notification_identity_and_unknown_events_never_invent_a_continuation_cause() {
    for id in [
        None,
        Some(json!(null)),
        Some(json!("")),
        Some(json!("  ")),
        Some(json!(7)),
    ] {
        let mut event = json!({"kind":"shell_ended","payload":{"exit_code":0}});
        if let Some(id) = id {
            event["event_id"] = id;
        }
        let payload = json!({"text":format!("Shellboy event v1: {event}")});
        let p = project("event", &payload, &text_of_payload(&payload), 0);
        assert!(!p.title.contains("続行"), "{event}");
    }
    for event in [
        json!({"event_id":"known-id","kind":"unknown_new_kind","payload":{}}),
        json!({"event_id":"known-id","kind":"context_changed","payload":{"kind":"turn_started","change":{"turn_id":"turn"}}}),
    ] {
        let payload = json!({"text":format!("Shellboy event v1: {event}")});
        let p = project("event", &payload, &text_of_payload(&payload), 0);
        assert!(!p.title.contains("続行"), "{event}");
    }
}

#[test]
fn mixed_assistant_items_preserve_kind_unicode_source_and_payload() {
    let payload = json!({"output":[
        {"type":"reasoning","summary":[{"type":"summary_text","text":"調べます🦀"}],"encrypted_content":"DO_NOT_DISPLAY"},
        {"type":"message","content":[{"type":"output_text","text":"## 結果\n**成功** [資料](https://example.com/日本語?q=a&b=c)"}]},
        {"type":"function_call","name":"shell","call_id":"c1","arguments":"{\"command\":\"printf '# plain'\",\"background\":true}"}
    ]});
    let original = payload.clone();
    let text = text_of_payload(&payload);
    let p = project("assistant_response", &payload, &text, 0);
    assert_eq!(payload, original);
    assert_eq!(p.kind, PresentationKind::Assistant);
    assert_eq!(
        p.blocks.iter().map(|b| &b.kind).collect::<Vec<_>>(),
        vec![
            &BlockKind::Reasoning,
            &BlockKind::Prose,
            &BlockKind::ToolCall
        ]
    );
    assert_eq!(p.blocks[1].source_start, "調べます🦀\n".chars().count());
    assert!(
        p.blocks[1]
            .markdown
            .iter()
            .any(|t| t.kind == MarkdownKind::Heading(2))
    );
    assert!(p.blocks[2].markdown.is_empty());
    assert_eq!(p.blocks[2].text, "printf '# plain'");
    assert!(
        p.links
            .iter()
            .any(|l| l.url == "https://example.com/日本語?q=a&b=c" && l.label == "資料")
    );
    assert!(
        !serde_json::to_string(&p)
            .unwrap()
            .contains("DO_NOT_DISPLAY")
    );
}

#[test]
fn persisted_string_events_and_tool_results_become_structured() {
    let event = json!({"content":[{"type":"input_text","text":format!("Shellboy event v1: {}",json!({"kind":"thread_settings","change":{"before":{"model":"A"},"after":{"model":"B"}},"observed_at":"2026-10-24T00:00:00Z"}))}]});
    let p = project("event", &event, &text_of_payload(&event), 0);
    assert_eq!(p.title, "接続・設定変更 · モデル: A → B");
    assert_eq!(p.blocks[0].text, p.title);
    assert!(
        p.fields
            .iter()
            .any(|f| f.key.ends_with("/change/after/model") && f.value == "B")
    );
    let result = json!({"item":{"type":"function_call_output","call_id":"c1","output":json!({"status":"failed","exit_code":2,"output_complete":false,"output":"# plain\n* not Markdown","reason":"output_limit"}).to_string()}});
    let p = project("tool_result", &result, &text_of_payload(&result), 0);
    assert!(p.title.starts_with("shell · failed"));
    assert!(p.title.contains("exit 2") && p.title.contains("出力不完全"));
    assert_eq!(p.blocks[0].text, "# plain\n* not Markdown");
    assert_eq!(p.blocks[0].stream, Some(OutputStream::Combined));
    assert!(p.blocks[0].markdown.is_empty());
    assert!(
        p.fields
            .iter()
            .any(|f| f.key.ends_with("/exit_code") && f.value == "2")
    );
}

#[test]
fn plain_controls_are_not_terminal_commands_and_full_links_survive() {
    let raw = "# raw\x1b[31mred\x1b[0m \x1b]8;;https://example.com/auth?long=123456789\x1b\\sign in\x1b]8;;\x1b\\\x1b]52;c;ZXh0cmE=\x07\nhttps://example.org/a?q=1&b=2";
    let saved = raw.as_bytes().to_vec();
    let (text, links) = sanitize_plain(raw, 7);
    assert_eq!(saved, raw.as_bytes());
    assert!(!text.contains('\x1b'));
    assert!(!text.contains("ZXh0cmE"));
    assert!(text.contains("# rawred sign in"));
    assert!(
        links
            .iter()
            .any(|l| l.label == "sign in" && l.url == "https://example.com/auth?long=123456789")
    );
    assert!(
        links
            .iter()
            .any(|l| l.url == "https://example.org/a?q=1&b=2")
    );
    assert_eq!(links.len(), 2);
    assert!(
        sanitize_plain("\x1b]8;;file:///tmp/secret\x07x\x1b]8;;\x07", 0)
            .1
            .is_empty()
    );
}

#[test]
fn markdown_has_structure_without_network_and_raw_user_stays_plain() {
    let text = "# 日本語🦀\n\n> quote\n\n- **bold** *em* `code`\n\n```rust\nlet x = 1;\n```\n\n| 名前 | 値 |\n|---|---|\n| a | [label](https://example.com/full) |\n\n<script>plain</script>";
    let p = project("summary", &json!({"text":text}), text, 0);
    let tokens = &p.blocks[0].markdown;
    for kind in [
        MarkdownKind::Heading(1),
        MarkdownKind::Quote,
        MarkdownKind::ListItem,
        MarkdownKind::Strong,
        MarkdownKind::Emphasis,
        MarkdownKind::Code,
        MarkdownKind::CodeBlock("rust".into()),
        MarkdownKind::TableCell,
    ] {
        assert!(tokens.iter().any(|t| t.kind == kind), "missing {kind:?}");
    }
    assert!(tokens.iter().any(|t| t.text.contains("<script>")));
    assert!(
        tokens
            .iter()
            .all(|t| t.source_start <= t.source_end && t.source_end <= text.chars().count())
    );
    let user = project("user", &json!({"text":text}), text, 0);
    assert!(user.blocks[0].markdown.is_empty());
    assert_eq!(user.blocks[0].text, text);
}

#[test]
fn partial_windows_keep_global_offsets_and_unknown_is_not_an_answer() {
    let payload = json!({"output":[{"type":"message","content":[{"type":"output_text","text":"日本語🦀abcdef"}]}]});
    let p = project("assistant_response", &payload, "語🦀abc", 2);
    assert_eq!(p.blocks[0].text, "語🦀abc");
    assert_eq!((p.blocks[0].source_start, p.blocks[0].source_end), (2, 7));
    let unknown = project("future_kind", &json!({"status":"new","count":4}), "", 0);
    assert_eq!(unknown.kind, PresentationKind::Unknown);
    assert!(unknown.title.contains("future_kind"));
    assert!(unknown.blocks.is_empty());
    assert!(!unknown.fields.is_empty());
    let tombstone = project(
        "tool_result",
        &json!({"tombstone_reason":"raw_retention_age"}),
        "unavailable",
        0,
    );
    assert!(tombstone.title.contains("raw_retention_age"));
}

#[test]
fn detail_tree_redacts_nested_json_credentials_and_has_real_json_pointers() {
    let value = json!({"a/b":{"~key":[{"ok":true,"encrypted_content":"OPAQUE","Authorization":"BEARER","apiKey":"KEY"}]},"output":"{\"origin_token\":\"ORIGIN\",\"result\":{\"code\":3}}"});
    let safe = safe_detail_value(value.clone());
    let encoded = safe.to_string();
    for secret in ["OPAQUE", "BEARER", "KEY", "ORIGIN"] {
        assert!(!encoded.contains(secret));
    }
    let fields = fields_of_value(&value);
    for f in &fields {
        assert!(
            safe.pointer(&f.key).is_some(),
            "invalid field pointer {}",
            f.key
        );
    }
    assert!(fields.iter().any(|f| f.key == "/a~1b/~0key/0/ok"));
}

#[test]
fn bounded_metadata_evicts_and_large_markdown_falls_back_without_losing_body() {
    let text = "**many** ".repeat(10000);
    let p = project("assistant_response", &json!({"text":text}), &text, 0);
    assert_eq!(p.blocks[0].text, text);
    assert!(p.blocks[0].markdown.is_empty());
    let mut p = p;
    assert!(p.retained_bytes() >= p.blocks[0].text.len());
    p.clear();
    assert_eq!(p, NodePresentation::default());
    let value = json!(
        (0..500)
            .map(|n| (format!("key{n}"), json!("x".repeat(5000))))
            .collect::<serde_json::Map<_, _>>()
    );
    let p = project("unknown", &value, "", 0);
    assert!(p.fields.len() <= 128);
    assert!(p.details_omitted);
}

#[test]
fn links_preserve_balanced_parentheses_and_terminal_markup_is_removed_from_prose() {
    let text = "日本語 [label](https://example.com/a_(b)) https://example.org/a_(c).";
    let p = project("assistant_response", &json!({"text":text}), text, 0);
    assert!(p.links.iter().any(|l| l.url == "https://example.org/a_(c)"));
    let raw = "x \u{1b}[31mred\u{1b}[0m";
    let p = project("assistant_response", &json!({"text":raw}), raw, 0);
    assert!(
        p.blocks
            .iter()
            .flat_map(|b| &b.markdown)
            .all(|t| !t.text.contains('\u{1b}'))
    );
    let large = "word ".repeat(30000);
    let p = project(
        "assistant_response",
        &json!({"text":large,"metadata":(0..150).map(|i|(i.to_string(),json!("field ".repeat(300)))).collect::<serde_json::Map<_,_>>()}),
        &large,
        0,
    );
    assert!(p.metadata_bytes() <= MAX_METADATA_BYTES);
    assert_eq!(p.blocks[0].text, large);
}

#[test]
fn split_shell_streams_and_recovery_events_keep_failure_information_visible() {
    let value = json!({"status":"exited","exit_code":101,"output_complete":false,"stdout":"# raw stdout\n","stderr":"assertion failed\n"});
    let payload = json!({"item":{"type":"function_call_output","output":value.to_string()}});
    let canonical = text_of_payload(&payload);
    let p = project("tool_result", &payload, &canonical, 0);
    assert_eq!(p.blocks.len(), 2);
    assert_eq!(p.blocks[0].stream, Some(OutputStream::Stdout));
    assert_eq!(p.blocks[1].stream, Some(OutputStream::Stderr));
    assert_eq!(p.blocks[0].text, "# raw stdout\n");
    assert_eq!(p.blocks[1].text, "assertion failed\n");
    assert!(
        p.blocks
            .iter()
            .all(|b| b.markdown.is_empty() && !b.source_exact)
    );
    assert!(p.requires_attention());
    assert!(p.title.contains("exit 101") && p.title.contains("出力不完全"));
    let inner = json!({"kind":"child_recovery_wait","payload":{"state":"recovery_wait","error":{"code":"io_failed","message":"資料を読めません","hint":"参照先を確認"}}});
    let payload = json!({"text":format!("Shellboy event v1: {inner}")});
    let p = project("event", &payload, &text_of_payload(&payload), 0);
    assert!(p.title.contains("復旧待ち"));
    assert!(!p.title.contains("資料を読めません") && !p.title.contains("参照先を確認"));
    assert!(
        p.blocks[0].text.contains("資料を読めません") && p.blocks[0].text.contains("参照先を確認")
    );
    assert!(
        p.fields
            .iter()
            .any(|f| f.key.ends_with("/error/message") && f.value == "資料を読めません")
    );
    assert!(p.requires_attention());
}

#[test]
fn partial_json_window_still_projects_bounded_decoded_output_and_keeps_original() {
    let raw = "日本語 stdout line\n".repeat(5000);
    let payload = json!({"item":{"type":"function_call_output","output":json!({"status":"exited","exit_code":7,"output_complete":true,"stdout":raw}).to_string()}});
    let canonical = text_of_payload(&payload);
    let start = canonical.chars().count() - 2048;
    let window: String = canonical.chars().skip(start).collect();
    let saved = payload.clone();
    let p = project("tool_result", &payload, &window, start);
    assert_eq!(payload, saved);
    assert!(p.title.contains("exit 7"));
    assert!(p.blocks[0].text.contains("日本語 stdout line"));
    assert!(!p.blocks[0].text.contains("\\n\\"));
    assert!(p.blocks[0].text.len() < 4096);
    assert!(p.details_omitted);
    assert!(!p.blocks[0].source_exact);
}

#[test]
fn oss_markdown_renderer_suppresses_heading_markers_and_keeps_style_and_plain_tools() {
    use ratatui::style::Modifier;
    let text = "# 見出し\n\n**強調** と *斜体*、`code`、[資料](https://example.test/doc)。\n\n```rust\nlet 日本語 = 1;\n```\n\n| 項目 | 値 |\n|---|---|\n| abc | 日本語🦀の長い値 |";
    let mut p = project("assistant_response", &json!({"text":text}), text, 0);
    let before = p.retained_bytes();
    render_markdown(&mut p, 28);
    let rendered = p.blocks[0].rendered.as_ref().unwrap();
    assert_eq!(rendered.width, 28);
    let visible = rendered
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.starts_with("見出し"));
    assert!(!visible.contains("# 見出し"));
    assert!(!visible.contains("**強調**"));
    assert!(visible.contains("rust") && visible.contains("let 日本語 = 1;"));
    assert!(
        rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(
        rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
    assert!(p.retained_bytes() > before);
    assert!(p.metadata_bytes() <= MAX_METADATA_BYTES);
    let mut tool = project(
        "tool_result",
        &json!({"text":"# raw **stars**"}),
        "# raw **stars**",
        0,
    );
    render_markdown(&mut tool, 28);
    assert!(tool.blocks[0].rendered.is_none());
    assert_eq!(tool.blocks[0].text, "# raw **stars**");
}

#[test]
fn oss_tables_reflow_at_worker_width_and_japanese_punctuation_does_not_enter_url() {
    let text = "文章と[参照](https://example.test/complete/path?key=value)。\n\n| 名前 | 説明 |\n|---|---|\n| row | abcdefghijklmnopqrstuvwxyz日本語🦀 |";
    let mut p = project("summary", &json!({"text":text}), text, 0);
    assert_eq!(p.links.len(), 1);
    assert_eq!(
        p.links[0].url,
        "https://example.test/complete/path?key=value"
    );
    render_markdown(&mut p, 22);
    let first = p.blocks[0].rendered.as_ref().unwrap().clone();
    render_markdown(&mut p, 48);
    let second = p.blocks[0].rendered.as_ref().unwrap();
    assert_eq!(second.width, 48);
    assert_ne!(first.lines, second.lines);
    let table_lines = first
        .lines
        .iter()
        .filter(|l| l.spans.iter().any(|s| s.content.contains('│')))
        .collect::<Vec<_>>();
    assert!(!table_lines.is_empty());
    assert!(table_lines.iter().all(|l| l.width() <= 22));
}

#[test]
fn large_oss_render_is_bounded_and_remembers_plain_fallback_at_the_same_width() {
    let text = "**強調** と `code`\n\n".repeat(3000);
    let mut p = project("assistant_response", &json!({"text":text}), &text, 0);
    render_markdown(&mut p, 42);
    let rendered = p.blocks[0].rendered.as_ref().unwrap();
    assert_eq!(rendered.width, 42);
    assert!(rendered.omitted);
    assert!(rendered.lines.is_empty());
    assert_eq!(p.blocks[0].text, text);
    assert!(p.metadata_bytes() <= MAX_METADATA_BYTES);
    assert!(p.details_omitted);
}

#[test]
fn oss_code_highlighting_uses_bundled_theme_with_language_caption_and_no_terminal_markers() {
    use ratatui::style::Color;
    let text = "```rust\nfn main() { let message = \"hello\"; println!(\"{}\", message); }\n```\n\n```unknown-language\nkeep <every> line\n\nsecond line\n```";
    let mut p = project("assistant_response", &json!({"text":text}), text, 0);
    render_markdown(&mut p, 80);
    let rendered = p.blocks[0].rendered.as_ref().unwrap();
    assert!(!rendered.omitted);
    let lines = &rendered.lines;
    let visible = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("rust") && visible.contains("unknown-language"));
    let caption = lines
        .iter()
        .find(|line| line.spans.iter().any(|s| s.content.contains("rust")))
        .expect("caption identifies its role without color");
    let caption_text = caption
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(caption_text.ends_with("rust"));
    assert!(
        caption_text.starts_with("    "),
        "language caption belongs at the upper right"
    );

    assert!(visible.contains("keep <every> line") && visible.contains("second line"));
    assert!(!visible.contains("shellboy-code-fence"));
    assert!(!visible.contains('\u{e000}'));
    assert!(!visible.contains("──"));
    let source_line = lines
        .iter()
        .find(|line| line.spans.iter().any(|s| s.content.contains("main")))
        .unwrap();
    let colors = source_line
        .spans
        .iter()
        .filter_map(|span| match span.style.fg {
            Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        colors.len() >= 2,
        "OSS syntax colors must remain intact: {source_line:?}"
    );
    assert_eq!(source_line.style.bg, Some(Color::Rgb(30, 34, 40)));
    assert_eq!(p.blocks[0].text, text);
    assert!(p.metadata_bytes() <= MAX_METADATA_BYTES);
}

#[test]
fn attention_event_keeps_title_short_and_preserves_error_details_as_separate_plain_lines() {
    let value = json!({"kind":"shell_ended","payload":{"exit_code":7,"output_complete":false,"error":{"code":"io_failed","message":"シェルが失敗しました","hint":"保存された出力を確認してください"}}});
    let payload = json!({"text":format!("Shellboy event v1: {value}")});
    let p = project("event", &payload, &text_of_payload(&payload), 0);
    assert!(p.title.starts_with("コマンド終了"));
    assert!(p.title.contains("exit 7") && p.title.contains("出力不完全"));
    assert!(!p.title.contains("io_failed"));
    assert!(!p.title.contains("シェルが失敗しました"));
    assert_eq!(p.blocks[0].kind, BlockKind::Event);
    assert_eq!(
        p.blocks[0].text,
        "シェルが失敗しました\n対処: 保存された出力を確認してください"
    );
    assert!(!p.blocks[0].source_exact);
    assert!(p.requires_attention());
    assert!(
        p.fields
            .iter()
            .any(|f| f.key.ends_with("/error/code") && f.value == "io_failed")
    );
}

#[test]
fn each_tool_result_keeps_its_own_identity_state_reason_and_partial_source_span() {
    let payload = json!({"output":[
        {"type":"function_call_output","call_id":"one","output":json!({"command":"echo one","status":"exited","exit_code":0,"output":"one"}).to_string()},
        {"type":"function_call_output","call_id":"two","output":json!({"command":"sleep 30","status":"running","reason":"human_backgrounded","execution_id":"execution-two"}).to_string()},
        {"type":"function_call_output","call_id":"three","output":json!({"status":"cancelled","reason":"Previous tool failed or foreground wait was interrupted"}).to_string()}
    ]});
    let original = payload.clone();
    let text = text_of_payload(&payload);
    let p = project("tool_result", &payload, &text, 0);
    let tools = p
        .blocks
        .iter()
        .filter_map(|block| block.tool.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.call_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
    assert!(tools[0].heading().contains("終了 0"));
    assert!(tools[1].heading().contains("背景継続（返却時）"));
    assert_eq!(tools[1].execution_id.as_deref(), Some("execution-two"));
    assert_eq!(tools[1].reason, "利用者の操作で背景へ移行");
    assert!(tools[2].heading().contains("未実行・取消"));
    assert!(tools[2].reason.contains("先行する呼出し"));
    let base = p.blocks[1].source_start + 3;
    let partial = text.chars().skip(base).take(12).collect::<String>();
    let p = project("tool_result", &payload, &partial, base);
    assert_eq!(p.blocks[0].tool.as_ref().unwrap().call_id, "two");
    assert_eq!(p.blocks[0].source_start, base);
    assert_eq!(p.blocks[0].source_end, base + 12);
    assert!(!p.blocks[0].source_exact);
    assert_eq!(payload, original);
    assert_eq!(text_of_payload(&payload), text);
}

#[test]
fn old_serialized_blocks_have_no_tool_context_and_metadata_stays_bounded() {
    let block: PresentationBlock=serde_json::from_value(json!({"kind":"Plain","text":"old","source_start":0,"source_end":3,"source_exact":true,"markdown":[]})).unwrap();
    assert!(block.tool.is_none());
    let payload = json!({"output":(0..128).map(|i|json!({"type":"function_call","name":"shell","call_id":format!("call-{i}"),"arguments":json!({"command":"x".repeat(2000)}).to_string()})).collect::<Vec<_>>()});
    let text = text_of_payload(&payload);
    let projected = project("assistant_response", &payload, &text, 0);
    assert!(projected.metadata_bytes() <= MAX_METADATA_BYTES);
    assert!(projected.details_omitted);
    assert_eq!(text_of_payload(&payload), text);
}

#[test]
fn known_summary_layout_uses_body_ranges_and_source_counts_without_rewriting_originals() {
    let first = "目的と制約 日本語🦀";
    let second = "現在の状態と次の作業";
    let body = format!(
        "### Part 1\n{first}\n\nShellboy source nodes: source-one, source-two\n\n### Part 2\n{second}\n\nShellboy source nodes: source-three"
    );
    let payload = json!({"item":{"role":"developer","content":[{"type":"input_text","text":body}]},"source_node_ids":["source-one","source-two","source-three"],"summary_part_keys":["part-one","part-two"],"summary_format":"shellboy-summary-v1"});
    let saved = payload.clone();
    let canonical = text_of_payload(&payload);
    let p = project("summary", &payload, &canonical, 0);
    assert_eq!(p.title, "要約 · 置換3件 · 出典3件 · F6→O 原文");
    assert_eq!(
        p.blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        [first, second]
    );
    assert_eq!(
        p.blocks[1].source_start,
        body[..body.find(second).unwrap()].chars().count()
    );
    assert!(p.blocks.iter().all(|block| block.source_exact));
    assert!(p.fields.iter().any(|f| f.value == "source-three"));
    let base = body[..body.find(second).unwrap()].chars().count() + 2;
    let window = canonical.chars().skip(base).take(4).collect::<String>();
    let p = project("summary", &payload, &window, base);
    assert_eq!(p.blocks[0].text, window);
    assert_eq!(p.blocks[0].source_start, base);
    assert_eq!(p.blocks[0].source_end, base + 4);
    let footer = body.find("Shellboy source nodes").unwrap();
    let base = body[..footer].chars().count();
    let window = canonical.chars().skip(base).take(18).collect::<String>();
    let p = project("summary", &payload, &window, base);
    assert!(p.blocks.iter().all(|block| block.text.is_empty()));
    assert_eq!(payload, saved);
    assert_eq!(text_of_payload(&payload), canonical);
}

#[test]
fn summary_decoder_rejects_ambiguous_or_unknown_layout_without_deleting_text() {
    let body = "### Part 1\n本文\n\nShellboy source nodes: real-source";
    let base = json!({"item":{"content":[{"type":"input_text","text":body}]},"source_node_ids":["real-source"],"summary_part_keys":["part"],"summary_format":"shellboy-summary-v1"});
    let mut unknown = base.clone();
    unknown["summary_format"] = json!("future-summary-v99");
    let text = text_of_payload(&unknown);
    assert_eq!(project("summary", &unknown, &text, 0).blocks[0].text, text);
    let mut mismatch = base.clone();
    mismatch["source_node_ids"] = json!(["different-source"]);
    let text = text_of_payload(&mismatch);
    assert_eq!(project("summary", &mismatch, &text, 0).blocks[0].text, text);
    let mut ambiguous = base;
    ambiguous["item"]["content"][0]["text"] = json!(
        "### Part 1\n利用者の文章\n\nShellboy source nodes: real-source\n\nShellboy source nodes: real-source"
    );
    let text = text_of_payload(&ambiguous);
    assert_eq!(
        project("summary", &ambiguous, &text, 0).blocks[0].text,
        text
    );
}

#[test]
fn human_events_keep_important_differences_and_unknown_kinds_in_details_only() {
    let payload = json!({"text":format!("Shellboy event v1: {}",json!({"kind":"context_changed","payload":{"kind":"index_state","change":{"before":{"model":"old-model","paused":false},"after":{"model":"new-model","paused":true,"pause_kind":"provider_limit"}}}}))});
    let canonical = text_of_payload(&payload);
    let p = project("event", &payload, &canonical, 0);
    assert_eq!(p.event_class, EventClass::Attention);
    assert!(p.requires_attention());
    assert!(p.title.starts_with("索引の状態"));
    assert!(p.title.contains("old-model → new-model"));
    assert!(p.title.contains("一時停止"));
    assert!(!p.title.contains("index_state"));
    assert!(p.fields.iter().any(|field| field.value == "provider_limit"));
    assert_eq!(text_of_payload(&payload), canonical);
    let payload = json!({"text":format!("Shellboy event v1: {}",json!({"kind":"future_internal_event_v42","target_id":"full-target-id"}))});
    let p = project("event", &payload, &text_of_payload(&payload), 0);
    assert_eq!(p.title, "その他の状態変更");
    assert_eq!(p.event_class, EventClass::Unknown);
    assert!(
        p.fields
            .iter()
            .any(|field| field.value == "future_internal_event_v42")
    );
    assert!(p.fields.iter().any(|field| field.value == "full-target-id"));
}

#[test]
fn interruption_metadata_uses_recorded_counts_without_inventing_restore_or_process_results() {
    let value = json!({"kind":"previous_instance_interrupted","snapshot_node_id":"complete-snapshot-node","change":{"runs":{"count":2,"items":[{"run_id":"complete-run-id"}]},"shells":{"count":1},"tool_calls":{"count":2,"detail_node_ids":["tool-call-details"]},"unreflected_messages":{"count":3},"result_uncertain":true,"automatic_replay":false,"os_processes_inspected":false}});
    let payload = json!({"text":value.to_string()});
    let text = text_of_payload(&payload);
    let p = project("interruption_context", &payload, &text, 0);
    assert_eq!(p.kind, PresentationKind::Interrupted);
    assert_eq!(p.title, "前回の中断記録");
    let shown = p
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "作業 2件",
        "コマンド 1件",
        "中断時のツール 2件",
        "未反映の依頼 3件",
        "実行結果は未確認",
        "自動での再実行はしません",
    ] {
        assert!(shown.contains(expected));
    }
    assert!(!shown.contains("complete-run-id"));
    assert!(!shown.contains("/restore"));
    assert!(
        p.fields
            .iter()
            .any(|field| field.value == "complete-run-id")
    );
    assert!(
        p.fields
            .iter()
            .any(|field| field.key == "/change/tool_calls/detail_node_ids/0"
                && field.value == "tool-call-details")
    );
    assert_eq!(text_of_payload(&payload), text);
}

#[test]
fn absent_tool_reason_never_displays_null_and_nonnull_error_remains_visible() {
    for (reason, error, expected) in [
        (serde_json::Value::Null, serde_json::Value::Null, ""),
        (
            serde_json::Value::Null,
            json!("fixture failure"),
            "fixture failure",
        ),
        (
            json!("output_limit"),
            json!("unused fallback"),
            "出力上限に到達",
        ),
    ] {
        let payload = json!({"item":{"type":"function_call_output","call_id":"call","output":json!({"status":"completed","output":"done","reason":reason,"error":error}).to_string()}});
        let p = project("tool_result", &payload, &text_of_payload(&payload), 0);
        let tool = p
            .blocks
            .iter()
            .find_map(|block| block.tool.as_ref())
            .unwrap();
        assert_eq!(tool.reason, expected);
        assert_ne!(tool.reason, "null");
    }
}

#[test]
fn first_observations_are_compact_but_real_null_transitions_and_raw_fields_remain() {
    let project_event = |event: serde_json::Value| {
        let payload = json!({"text":format!("Shellboy event v1: {}",json!({"kind":"context_changed","payload":event}))});
        let text = text_of_payload(&payload);
        let presentation = project("event", &payload, &text, 0);
        assert_eq!(
            text_of_payload(&payload),
            text,
            "canonical source is unchanged"
        );
        presentation
    };
    let p = project_event(
        json!({"kind":"index_state","change":{"before":null,"after":{
        "active_space":null,"profile":null,"model":null,"dimension":null,"active_state":null,
        "paused":false,"pause_kind":null}}}),
    );
    assert_eq!(p.title, "索引の状態 · 未設定");
    assert_eq!(p.event_class, EventClass::Change);
    assert!(
        p.fields
            .iter()
            .any(|f| f.key.ends_with("/dimension") && f.value == "null")
    );
    let path = format!("/{}/AGENTS.md", "long-directory/".repeat(20));
    for present in [false, true] {
        let p = project_event(
            json!({"kind":"instructions_observed","change":{"before":null,
            "after":{"path":path,"hash":null,"present":present}}}),
        );
        assert_eq!(
            p.title,
            format!(
                "指示ファイル確認 · {}",
                if present { "あり" } else { "なし" }
            )
        );
        assert!(!p.title.contains(&path));
        assert!(
            p.fields
                .iter()
                .any(|f| f.key.ends_with("/path") && f.value == path)
        );
    }
    let p = project_event(
        json!({"kind":"index_state","change":{"before":null,"after":{
        "model":"configured-model","paused":true,"pause_kind":"provider_limit",
        "future":{"preserved":"original"}}}}),
    );
    assert_eq!(p.event_class, EventClass::Attention);
    assert!(p.title.contains("configured-model"));
    assert!(p.title.contains("provider_limit"));
    assert!(!p.title.contains('→'));
    assert!(p.fields.iter().any(|f| f.value == "original"));
    let p = project_event(
        json!({"kind":"index_state","change":{"before":{"model":"old-model"},
        "after":{"model":null,"profile":null,"paused":false}}}),
    );
    assert!(p.title.contains("old-model → 未設定"));
    assert!(!p.title.contains("未設定 → 未設定"));
    // Absent information does not justify the compact all-unconfigured claim.
    let p = project_event(json!({"kind":"index_state","change":{"before":null,"after":{}}}));
    assert_eq!(p.title, "索引の状態");
    let p = project_event(
        json!({"kind":"instructions_observed","change":{"before":null,
        "after":{"present":null}}}),
    );
    assert!(!p.title.contains("なし"));
}

#[test]
fn shell_outcome_marker_and_attention_use_current_code_without_overwriting_saved_code() {
    for (code, marker, attention, label) in [
        (Some(0), "✓", false, "終了 0"),
        (Some(7), "✗", true, "終了 7"),
        (None, "·", false, "終了（コード不明）"),
    ] {
        let tool = ToolPresentation {
            ordinal: Some(1),
            command: "printf fixture".into(),
            status: "completed".into(),
            current_state: Some("exited".into()),
            current_exit_code: code,
            ..Default::default()
        };
        assert!(
            tool.heading()
                .starts_with(&format!("{marker} #2 printf fixture"))
        );
        assert!(tool.heading().contains(label));
        assert_eq!(tool.requires_attention(), attention);
        assert_eq!(tool.exit_code, None);
    }
    let mut tool = ToolPresentation {
        status: "running".into(),
        current_state: Some("exited".into()),
        current_exit_code: Some(7),
        ..Default::default()
    };
    assert!(tool.heading().starts_with("✗"));
    assert!(tool.heading().contains("終了 7 · 返却時 背景継続"));
    tool.status = "failed".into();
    tool.exit_code = Some(7);
    tool.current_exit_code = Some(0);
    assert!(tool.heading().contains("終了 0 · 返却時 失敗 7"));
    assert!(tool.heading().starts_with("✗"));
    tool.current_exit_code = None;
    assert!(
        tool.heading()
            .contains("終了（コード不明） · 返却時 失敗 7")
    );
    tool.observation_unavailable();
    assert_eq!(tool.current_exit_code, None);
    assert_eq!(tool.exit_code, Some(7));
    assert!(tool.heading().contains("失敗 7 · 現在状態は未確認"));
    assert!(!tool.heading().contains("現在 終了"));
    tool.clear_observation();
    assert_eq!(tool.current_state, None);
    assert_eq!(tool.current_exit_code, None);
    assert_eq!(tool.exit_code, Some(7));
}

#[test]
fn field_value_omission_is_explicit_for_unicode_depth_and_legacy_values() {
    use shellboy::presentation::{PresentationField, fields_of_value};
    let legacy: PresentationField = serde_json::from_value(json!({
        "key":"/note","label":"note","value":"[詳細は原文参照]"
    }))
    .unwrap();
    assert!(!legacy.value_omitted);
    for count in [1024, 1025] {
        let fields = fields_of_value(&json!({"text":"あ".repeat(count)}));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].value_omitted, count > 1024);
    }
    let mut deep = json!("deep original");
    for _ in 0..12 {
        deep = json!({"child":deep});
    }
    let fields = fields_of_value(&deep);
    assert_eq!(fields.len(), 1);
    assert!(fields[0].value_omitted);
    let fields = fields_of_value(&json!({"長".repeat(300):"complete value"}));
    assert_eq!(fields[0].value, "complete value");
    assert!(
        !fields[0].value_omitted,
        "short label must not truncate its value"
    );
}

#[test]
fn transformed_output_tracks_exact_unicode_omission_and_preserves_the_payload() {
    let output = "あ".repeat(4000);
    let raw = serde_json::json!({"stdout":output,"status":"exited","exit_code":0}).to_string();
    let payload =
        serde_json::json!({"item":{"type":"function_call_output","call_id":"c","output":raw}});
    let original = payload.clone();
    let full = shellboy::history::text_of_payload(&payload);
    let window: String = full.chars().take(80).collect();
    let p = project("tool_result", &payload, &window, 0);
    let body = p
        .blocks
        .iter()
        .find(|b| b.kind == BlockKind::ToolResult)
        .unwrap();
    let retained = body.text.split('\n').next().unwrap();
    assert!(retained.chars().all(|ch| ch == 'あ'));
    assert_eq!(p.body_omitted_chars, 4000 - retained.chars().count());
    assert!(!p.body_extent_unknown);
    assert!(p.metadata_limited);
    assert_eq!(payload, original);
    assert_eq!(shellboy::history::text_of_payload(&payload), full);
    let old: NodePresentation=serde_json::from_value(serde_json::json!({"kind":"Unknown","title":"legacy","blocks":[],"fields":[],"links":[],"details_omitted":true})).unwrap();
    assert!(old.details_omitted);
    assert!(!old.metadata_limited && !old.body_extent_unknown);
    assert_eq!(old.body_omitted_chars, 0);
}

#[test]
fn incomplete_terminal_controls_keep_the_remaining_body_readable() {
    for (raw, expected) in [
        (
            "前\x1b]title without terminator\n後続本文",
            "前title without terminator\n後続本文",
        ),
        ("前\x1b[31;\n後続本文", "前31;\n後続本文"),
        ("\x1b[31;未完\n次行", "31;未完\n次行"),
        ("\x1b]title\x1b[31mvisible\x1b[0m", "titlevisible"),
        ("\x1b]one\x1b]two\nvisible", "onetwo\nvisible"),
    ] {
        let (text, _) = sanitize_plain(raw, 0);
        assert_eq!(text, expected, "{raw:?}");
        assert!(!text.contains('\x1b'));
    }
}

#[test]
fn incomplete_osc_is_not_an_osc_link_and_bare_url_offsets_stay_original() {
    let raw = "日本\x1b]8;;https://safe.example/path\nvisible";
    let (text, links) = sanitize_plain(raw, 11);
    assert_eq!(text, "日本8;;https://safe.example/path\nvisible");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].label, "https://safe.example/path");
    assert_eq!(
        links[0].source_start,
        11 + raw[..raw.find("https").unwrap()].chars().count()
    );
    assert_eq!(
        links[0].source_end - links[0].source_start,
        links[0].label.chars().count()
    );
}

#[test]
fn repeated_unterminated_escape_introducers_preserve_the_tail() {
    for introducer in ["\x1b]", "\x1b[31;"] {
        let raw = format!("{}\n残る本文", introducer.repeat(4096));
        let (text, links) = sanitize_plain(&raw, 0);
        assert!(text.ends_with("\n残る本文"));
        assert!(!text.contains('\x1b'));
        assert!(links.is_empty());
    }
}

#[test]
fn code_tabs_have_visible_indentation_without_changing_the_source_or_copy() {
    let source = "```text\n\tfirst\n日\tsecond\n```";
    let payload =
        json!({"output":[{"type":"message","content":[{"type":"output_text","text":source}]}]});
    let text = text_of_payload(&payload);
    let mut projected = project("assistant_response", &payload, &text, 0);
    let original_blocks = projected
        .blocks
        .iter()
        .map(|block| (block.text.clone(), block.source_start, block.source_end))
        .collect::<Vec<_>>();
    shellboy::presentation::render_markdown(&mut projected, 40);
    assert_eq!(
        projected
            .blocks
            .iter()
            .map(|block| (block.text.clone(), block.source_start, block.source_end))
            .collect::<Vec<_>>(),
        original_blocks
    );
    let lines = projected
        .blocks
        .iter()
        .filter_map(|block| block.rendered.as_ref())
        .flat_map(|rendered| &rendered.lines)
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        lines.iter().any(|line| line.starts_with("      first")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("  日  second")),
        "{lines:?}"
    );
    assert!(lines.iter().all(|line| !line.contains('\t')));
    assert_eq!(text_of_payload(&payload), text);
    assert_eq!(
        shellboy::frontend::fenced_code_block(source, 0).as_deref(),
        Some("\tfirst\n日\tsecond\n")
    );
}

#[test]
fn expanded_tab_bytes_are_counted_in_the_existing_render_budget() {
    let text = format!("```text\n{}\n```", "\t".repeat(20_000));
    let mut projected = project("assistant_response", &json!({"text":text}), &text, 0);
    render_markdown(&mut projected, 42);
    assert_eq!(projected.blocks[0].text, text);
    assert!(projected.blocks[0].rendered.as_ref().unwrap().omitted);
    assert!(
        projected.blocks[0]
            .rendered
            .as_ref()
            .unwrap()
            .lines
            .is_empty()
    );
    assert!(projected.metadata_bytes() <= MAX_METADATA_BYTES);
}

#[test]
fn diagnostic_containers_and_saved_scalar_envelopes_keep_distinct_interpretations() {
    let scalar = "Shellboy event v1: 42";
    assert_eq!(safe_detail_value(json!(scalar)), json!(scalar));
    let payload = json!({"content":[{"type":"input_text","text":scalar}]});
    let p = project("event", &payload, &text_of_payload(&payload), 0);
    assert!(!p.blocks[0].source_exact);
    assert!(
        p.fields
            .iter()
            .any(|field| field.key.is_empty() && field.value == "42")
    );
    assert_eq!(
        safe_detail_value(json!(
            "Shellboy event v1:  {\"visible\":true,\"api_key\":\"secret\"} "
        )),
        json!({"visible":true})
    );
}

#[test]
fn event_states_and_tool_attention_preserve_their_own_failure_policies() {
    let event = json!({"content":[{"type":"input_text","text":format!("Shellboy event v1: {}", json!({"kind":"shell_ended","payload":{"state":"failed","exit_code":7,"output_complete":false}}))}]});
    let p = project("event", &event, &text_of_payload(&event), 0);
    assert!(p.title.contains("失敗"));
    assert!(p.title.contains("exit 7"));
    assert!(p.title.contains("出力不完全"));
    let tool = json!({"item":{"type":"function_call_output","output":json!({"status":"failed","error":{"status":"failed"},"output_complete":false,"exit_code":7,"stdout":"output"}).to_string()}});
    let p = project("tool_result", &tool, &text_of_payload(&tool), 0);
    assert!(!p.title.contains("失敗"));
    // A status inside error remains diagnostic content even when its value is
    // not one of the states which receives a tool attention label.
    assert_eq!(p.title.matches("failed").count(), 2);
    assert!(p.title.contains("exit 7") && p.title.contains("出力不完全"));
}

#[test]
fn answer_and_summary_share_oss_table_quote_and_link_rules() {
    let text = "> 引用と **強調**\n\n| 項目 | 参照 |\n|---|---|\n| 日本語🦀 | [資料](https://example.test/full?q=日本語) |";
    for width in [18, 38, 78] {
        let mut answer = project("assistant_response", &json!({"text":text}), text, 0);
        let mut summary = project("summary", &json!({"text":text}), text, 0);
        render_markdown(&mut answer, width);
        render_markdown(&mut summary, width);
        assert_eq!(answer.blocks[0].rendered, summary.blocks[0].rendered);
        assert_eq!(answer.blocks[0].text, text);
        assert_eq!(summary.blocks[0].text, text);
        assert_eq!(answer.links, summary.links);
        assert!(
            answer
                .links
                .iter()
                .any(|link| link.url == "https://example.test/full?q=日本語")
        );
        let rows = &answer.blocks[0].rendered.as_ref().unwrap().lines;
        assert!(rows.iter().all(|line| line.width() <= usize::from(width)));
        assert!(
            rows.iter()
                .any(|line| line.spans.iter().any(|span| span.content.contains('>')))
        );
    }
}

#[test]
fn markdown_content_uses_terminal_foreground_and_structure_on_dark_and_light_bases() {
    use ratatui::style::{Color, Modifier, Style};
    let source =
        "# Heading\n\n> Quote\n\n| Column |\n|---|\n| body |\n\n[Label](https://example.com/path)";
    let mut p = project("assistant_response", &json!({"text":source}), source, 0);
    render_markdown(&mut p, 80);
    let rendered = p.blocks[0].rendered.as_ref().unwrap();
    let visible = rendered
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        visible.contains("> Quote"),
        "OSS quote prefix remains: {visible}"
    );
    assert!(visible.contains("Column") && visible.contains("body"));
    for foreground in [Color::White, Color::Black] {
        let base = Style::default().fg(foreground);
        for (word, modifier) in [
            ("Heading", Some(Modifier::BOLD)),
            ("Quote", None),
            ("Column", Some(Modifier::BOLD)),
            ("Label", Some(Modifier::UNDERLINED)),
        ] {
            let (line, span) = rendered
                .lines
                .iter()
                .find_map(|line| {
                    line.spans
                        .iter()
                        .find(|span| span.content.contains(word))
                        .map(|span| (line, span))
                })
                .unwrap();
            let style = base.patch(line.style).patch(span.style);
            assert_eq!(style.fg, Some(Color::Reset), "{word} on {foreground:?}");
            if let Some(modifier) = modifier {
                assert!(
                    style.add_modifier.contains(modifier),
                    "{word} retains structure"
                );
            }
        }
    }
    assert_eq!(p.blocks[0].text, source);
    assert!(
        p.links
            .iter()
            .any(|link| link.url == "https://example.com/path")
    );
}

#[test]
fn structured_tool_errors_have_readable_summaries_and_preserve_original_fields() {
    for (error, expected) in [
        (
            json!({"code":"permission_denied","message":"Cannot open fixture"}),
            "Cannot open fixture · permission_denied",
        ),
        (
            json!({"error":{"message":"Nested failure"}}),
            "Nested failure",
        ),
        (json!({"code":"fixture_code"}), "fixture_code · 詳細はF6"),
        (json!([{"message":"unknown array"}]), "エラー詳細はF6"),
        (
            json!({"arbitrary":"private-shaped-detail"}),
            "エラー詳細はF6",
        ),
    ] {
        let payload = json!({"item":{"type":"function_call_output","call_id":"call","output":json!({"status":"failed","error":error}).to_string()}});
        let canonical = text_of_payload(&payload);
        let p = project("tool_result", &payload, &canonical, 0);
        let tool = p
            .blocks
            .iter()
            .find_map(|block| block.tool.as_ref())
            .unwrap();
        assert_eq!(tool.reason, expected);
        assert!(!tool.reason.contains('{') && !tool.reason.contains('['));
        assert_eq!(
            text_of_payload(&payload),
            canonical,
            "source JSON remains intact"
        );
        assert!(p.fields.iter().any(|field| field.key.contains("/error")));
    }
}

#[test]
fn terminal_tools_plain_code_and_index_readiness_avoid_internal_noise() {
    use shellboy::presentation::ToolPresentation;
    let terminal = ToolPresentation {
        command: "echo test".into(),
        status: "running".into(),
        current_state: Some("stopped".into()),
        ..Default::default()
    };
    let heading = terminal.heading();
    assert!(heading.contains("停止") && heading.contains("返却時 背景継続"));
    assert!(!heading.contains("現在"));
    for language in ["", "text", "plaintext"] {
        let source = format!("```{language}\nunique-code-body\n```\n");
        let payload =
            json!({"output":[{"type":"message","content":[{"type":"output_text","text":source}]}]});
        let mut p = project(
            "assistant_response",
            &payload,
            &text_of_payload(&payload),
            0,
        );
        shellboy::presentation::render_markdown(&mut p, 80);
        let shown = p
            .blocks
            .iter()
            .filter_map(|block| block.rendered.as_ref())
            .flat_map(|rendered| &rendered.lines)
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("unique-code-body"));
        assert!(!shown.contains("言語:"));
        assert!(
            !shown
                .lines()
                .any(|line| matches!(line.trim(), "text" | "plaintext"))
        );
    }
    let event = json!({"kind":"index_state","change":{"before":{"active_state":null,"paused":false},"after":{"active_state":"active","paused":false}}});
    let payload = json!({"text":format!("Shellboy event v1: {event}")});
    let p = project("event", &payload, &text_of_payload(&payload), 0);
    assert_eq!(p.event_class, EventClass::Routine);
    assert_eq!(p.title, "検索索引 · 利用可能");
    assert!(
        p.fields.iter().any(|field| field.value == "active"),
        "raw state remains in details"
    );
    let selection = json!({"kind":"index_state","change":{
        "before":{"active_space":"previous","active_state":"active","profile":"p","model":"m","paused":false},
        "after":{"active_space":"selected","active_state":"active","profile":"p","model":"m","paused":false}
    }});
    let payload = json!({"text":format!("Shellboy event v1: {selection}")});
    let changed = project("event", &payload, &text_of_payload(&payload), 0);
    assert_eq!(
        changed.event_class,
        EventClass::Change,
        "a different selected index is not routine readiness"
    );
    assert!(changed.fields.iter().any(|field| field.value == "selected"));
}

#[test]
fn interrupted_tool_results_show_uncertainty_without_fabricated_success() {
    for (status, label, text) in [
        ("interrupted", "中断・結果不確か", "再実行前"),
        ("not_started", "未開始", "未開始でした"),
    ] {
        let output = json!({"status":status,"reason":"previous_instance_interrupted","result_uncertain":status=="interrupted","output_complete":false});
        let payload = json!({"group_id":"g","item":{"type":"function_call_output","call_id":"c","output":output.to_string()}});
        let original = payload.clone();
        let projected = project("tool_result", &payload, &text_of_payload(&payload), 0);
        let result = projected.blocks.iter().find(|b| b.tool.is_some()).unwrap();
        let tool = result.tool.as_ref().unwrap();
        assert!(tool.heading().contains(label));
        assert!(tool.requires_attention());
        assert!(result.text.contains(text));
        assert_eq!(tool.exit_code, None);
        assert_eq!(payload, original);
    }
}

#[test]
fn revisit_event_names_the_continuation_and_shows_instruction_without_changing_source() {
    let event = json!({"event_id":"event-revisit","kind":"revisit","payload":{"run_id":"run","instruction":"Check original request A.\nCompare evidence B.","set_operation_id":"instance:reserve","consumed_count":5}});
    let payload = json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {event}")}],"event_id":"event-revisit"});
    let original = payload.clone();
    let text = text_of_payload(&payload);
    let projected = project("event", &payload, &text, 0);
    assert_eq!(projected.title, "Revisit resumed · 5");
    assert_eq!(projected.event_class, EventClass::Change);
    assert!(
        projected
            .blocks
            .iter()
            .any(|b| b.text.contains("Check original request A."))
    );
    assert!(
        projected
            .fields
            .iter()
            .any(|f| f.value == "instance:reserve")
    );
    assert_eq!(payload, original);
    let interruption = json!({"kind":"previous_instance_interrupted","change":{"revisits":{"count":1,"detail_node_ids":["revisit-details"]}}});
    let payload = json!({"text":interruption.to_string()});
    let projected = project(
        "interruption_context",
        &payload,
        &text_of_payload(&payload),
        0,
    );
    assert!(
        projected
            .blocks
            .iter()
            .any(|b| b.text.contains("Interrupted Revisit reservations 1"))
    );
    assert!(
        projected
            .fields
            .iter()
            .any(|f| f.key == "/change/revisits/detail_node_ids/0" && f.value == "revisit-details")
    );
}

#[test]
fn structured_cancellation_renders_saved_cause_and_prerequisite_without_own_execution() {
    for (reason, label, state, exit_code) in [
        (
            "previous_call_failed",
            "失敗により未実行",
            "exited",
            Some(7),
        ),
        (
            "previous_output_incomplete",
            "出力が不完全",
            "exited",
            Some(0),
        ),
        (
            "previous_wait_released_by_new_input",
            "追加の指示",
            "running",
            None,
        ),
        (
            "previous_wait_released_by_human_background",
            "利用者が",
            "running",
            None,
        ),
    ] {
        let output = json!({
            "status":"cancelled", "executed":false, "reason":reason,
            "message":"Saved explanation: later call was not executed.",
            "prerequisite":{"call_id":"prior-call","execution_id":"prior-execution","state":state,"exit_code":exit_code},
        });
        let payload = json!({"item":{"type":"function_call_output","call_id":"cancelled-call","output":output.to_string()}});
        let before = payload.clone();
        let text = text_of_payload(&payload);
        let projected = project("tool_result", &payload, &text, 0);
        let block = &projected.blocks[0];
        let tool = block.tool.as_ref().unwrap();
        assert_eq!(tool.call_id, "cancelled-call");
        assert_eq!(tool.reason_code, reason);
        assert!(tool.reason.contains(label), "{}", tool.reason);
        assert!(tool.reason.contains("prior-call"));
        assert!(tool.reason.contains("prior-execution"));
        assert!(
            tool.execution_id.is_none(),
            "a prerequisite execution is not the cancelled call's execution"
        );
        assert!(tool.heading().contains("未実行・取消"));
        assert_eq!(
            block.text,
            "Saved explanation: later call was not executed."
        );
        if state == "running" {
            assert!(tool.reason.contains("返却時"));
            assert!(!tool.reason.contains("失敗"));
        } else {
            assert!(
                tool.reason
                    .contains(&format!("終了 {}", exit_code.unwrap()))
            );
        }
        assert!(
            projected
                .fields
                .iter()
                .any(|f| f.key == "/prerequisite/execution_id" && f.value == "prior-execution")
        );
        assert_eq!(payload, before);
        assert_eq!(text_of_payload(&payload), text);
        assert!(projected.metadata_bytes() <= MAX_METADATA_BYTES);
    }
}

#[test]
fn structured_cancellation_does_not_invent_unknown_prerequisite_facts() {
    let output = json!({"status":"cancelled","executed":false,"reason":"tool_group_validation_failed","message":"Invalid group"});
    let payload = json!({"item":{"type":"function_call_output","call_id":"unstarted","output":output.to_string()}});
    let projected = project("tool_result", &payload, &text_of_payload(&payload), 0);
    let tool = projected.blocks[0].tool.as_ref().unwrap();
    assert_eq!(tool.reason_kind, ToolReasonKind::GroupValidation);
    assert!(!tool.reason.contains("前提:"));
    assert!(tool.execution_id.is_none());
    assert_eq!(projected.blocks[0].text, "Invalid group");
}
