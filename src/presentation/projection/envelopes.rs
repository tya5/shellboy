//! Decode each persisted envelope once, then apply its role-specific display policy.
use super::*;

pub(super) fn apply(
    p: &mut NodePresentation,
    kind: &str,
    base_char_offset: usize,
    mut envelopes: impl Iterator<Item = Option<Value>>,
) {
    // Shellboy's persisted event/tool output envelopes are JSON inside a text item.
    let mut extra_blocks = Vec::new();
    let original_blocks = std::mem::take(&mut p.blocks);
    for mut b in original_blocks {
        let mut trailing = Vec::new();
        let envelope = envelopes
            .next()
            .flatten()
            .or_else(|| parse_envelope(&b.text));
        if matches!(
            b.kind,
            BlockKind::Event | BlockKind::ToolResult | BlockKind::ToolCall
        ) && let Some(value) = envelope
        {
            b.source_exact = false;
            let fields = fields_of_value(&value);
            p.links
                .retain(|l| l.source_end <= b.source_start || l.source_start >= b.source_end);
            match b.kind {
                BlockKind::Event => event(p, &mut b, &value, &fields, kind),
                BlockKind::ToolCall => call(p, &mut b, &value, base_char_offset),
                _ => result(p, &mut b, &value, &fields, base_char_offset, &mut trailing),
            }
            p.fields.extend(fields);
        }
        extra_blocks.push(b);
        extra_blocks.extend(trailing);
    }
    p.blocks = extra_blocks;
}

fn event(
    p: &mut NodePresentation,
    b: &mut PresentationBlock,
    value: &Value,
    fields: &[PresentationField],
    kind: &str,
) {
    if p.kind == PresentationKind::Interrupted {
        let (title, body) = events::interruption(value, kind);
        p.title = title;
        b.text = body.join("\n");
        prioritize_interruption_identity(p, value, kind);
    } else {
        let event = if value.get("kind").and_then(Value::as_str) == Some("context_changed") {
            value.get("payload").unwrap_or(value)
        } else {
            value
        };
        let event_kind = event
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("context_changed");
        p.event_class = match event_kind {
            "run_started" | "turn_started" => EventClass::Routine,
            "index_state" if events::routine_index_ready(event) => EventClass::Routine,
            "index_state" if event.pointer("/change/after/paused") == Some(&Value::Bool(true)) => {
                EventClass::Attention
            }
            "child_recovery_wait" | "recovery_wait" => EventClass::Attention,
            "recovery_resumed"
            | "child_turn_completed"
            | "child_run_ended"
            | "shell_ended"
            | "child_report"
            | "revisit" => EventClass::Change,
            "index_state"
            | "thread_settings"
            | "settings_changed"
            | "connection_changed"
            | "model_changed"
            | "instructions_observed"
            | "instruction_changed"
            | "agents_reference"
            | "agents_changed" => EventClass::Change,
            _ => EventClass::Unknown,
        };
        p.title = events::title(event_kind, event);
        let (states, body) = if let Some((_, body)) = events::index_pause_notice(event) {
            (Vec::new(), body)
        } else if event_kind == "revisit" {
            (
                Vec::new(),
                events::revisit_preview(event).into_iter().collect(),
            )
        } else {
            event_description(fields)
        };
        let states = states
            .into_iter()
            .filter(|state| !p.title.contains(state))
            .collect::<Vec<_>>();
        if !states.is_empty() {
            p.title = format!("{} · {}", p.title, states.join(" · "));
        }
        b.text = if body.is_empty() {
            p.title.clone()
        } else {
            body.join("\n")
        };
    }
}

// Group identity must survive the generic field tree's item/byte limits. These
// are the same exact saved paths, retained inside the existing field budget.
fn prioritize_interruption_identity(p: &mut NodePresentation, value: &Value, kind: &str) {
    const SECTIONS: &[&str] = &[
        "runs",
        "turns",
        "shells",
        "tool_calls",
        "revisits",
        "owned_children",
        "unreflected_messages",
        "cancelled_waits",
        "pending_operations_with_unknown_targets",
    ];
    fn push(fields: &mut Vec<PresentationField>, key: &str, value: &Value) {
        if fields.len() >= MAX_FIELDS {
            return;
        }
        let Some(raw) = value.as_str() else {
            return;
        };
        if raw.len() > 4096 {
            return;
        }
        let Some(mut field) = fields_of_value(&serde_json::json!({"value":raw}))
            .into_iter()
            .find(|field| field.key == "/value" && !field.value_omitted && field.value == raw)
        else {
            return;
        };
        field.key = key.into();
        field.label = key.into();
        fields.push(field);
    }
    let mut identity = Vec::new();
    if kind == "interruption_details"
        && value["section"]
            .as_str()
            .is_some_and(|section| SECTIONS.contains(&section))
    {
        push(&mut identity, "/section", &value["section"]);
    } else if kind == "interruption_context"
        && matches!(
            value["kind"].as_str(),
            Some("app_shutdown" | "previous_instance_interrupted")
        )
    {
        push(&mut identity, "/kind", &value["kind"]);
        for section in SECTIONS {
            if let Some(references) = value["change"][*section]["detail_node_ids"].as_array() {
                for (index, reference) in references.iter().take(MAX_FIELDS).enumerate() {
                    push(
                        &mut identity,
                        &format!("/change/{section}/detail_node_ids/{index}"),
                        reference,
                    );
                    if identity.len() >= MAX_FIELDS {
                        break;
                    }
                }
            }
            if identity.len() >= MAX_FIELDS {
                break;
            }
        }
    }
    if !identity.is_empty() {
        identity.append(&mut p.fields);
        p.fields = identity;
    }
}

fn call(
    p: &mut NodePresentation,
    b: &mut PresentationBlock,
    value: &Value,
    base_char_offset: usize,
) {
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("shell呼出し · 詳細");
    let (text, omitted) = excerpt(
        &sanitize_plain(command, 0).0,
        b.text.len().max(128),
        base_char_offset > 0,
    );
    if let Some(tool) = &mut b.tool {
        tool.command = short(&text, 512);
    }
    b.text = text;
    p.body_omitted_chars = p.body_omitted_chars.saturating_add(omitted);
}

fn display_reason(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return tool_reason(text);
    }
    let message = ["/message", "/detail", "/error/message", "/error/detail"]
        .iter()
        .find_map(|path| value.pointer(path).and_then(Value::as_str));
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/error/code").and_then(Value::as_str));
    match (message, code) {
        (Some(message), Some(code)) => format!("{} · {}", tool_reason(message), tool_reason(code)),
        (Some(message), None) => tool_reason(message),
        (None, Some(code)) => format!("{} · 詳細はF6", tool_reason(code)),
        (None, None) => "エラー詳細はF6".into(),
    }
}

fn result(
    p: &mut NodePresentation,
    b: &mut PresentationBlock,
    value: &Value,
    fields: &[PresentationField],
    base_char_offset: usize,
    trailing: &mut Vec<PresentationBlock>,
) {
    if let Some(tool) = &mut b.tool {
        tool.status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into();
        tool.command = value
            .get("command")
            .and_then(Value::as_str)
            .map(|s| sanitize_plain(&short(s, 512), 0).0)
            .unwrap_or_default();
        tool.execution_id = value
            .get("execution_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        tool.reason_code = value
            .get("reason")
            .and_then(Value::as_str)
            .map(|s| short(s, 256))
            .unwrap_or_default();
        tool.reason_kind = ToolReasonKind::from_saved(&tool.reason_code);
        tool.reason = value
            .get("reason")
            .filter(|value| !value.is_null())
            .or_else(|| value.get("error").filter(|value| !value.is_null()))
            .map(display_reason)
            .unwrap_or_default();
        if value.get("executed").and_then(Value::as_bool) == Some(false)
            && let Some(prerequisite) = value.get("prerequisite").and_then(Value::as_object)
        {
            let mut facts = Vec::new();
            if let Some(call) = prerequisite.get("call_id").and_then(Value::as_str) {
                facts.push(format!("呼出し {}", sanitize_plain(&short(call, 48), 0).0));
            }
            if let Some(execution) = prerequisite.get("execution_id").and_then(Value::as_str) {
                facts.push(format!(
                    "実行 {}",
                    sanitize_plain(&short(execution, 48), 0).0
                ));
            }
            if let Some(state) = prerequisite.get("state").and_then(Value::as_str) {
                facts.push(tool_state_text(
                    state,
                    prerequisite.get("exit_code").and_then(Value::as_i64),
                ));
            }
            if let Some(outcome) = prerequisite
                .get("wait_release_outcome")
                .and_then(Value::as_str)
            {
                facts.push(format!(
                    "待機結果 {}",
                    sanitize_plain(&short(outcome, 48), 0).0
                ));
            }
            if !facts.is_empty() {
                tool.reason.push_str(" · 前提: ");
                tool.reason.push_str(&facts.join(" · "));
            }
        }
        tool.elapsed_ms = value.get("elapsed_ms").and_then(Value::as_u64);
        tool.exit_code = value.get("exit_code").and_then(Value::as_i64);
        if value
            .get("exit_code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
        {
            tool.status = "failed".into();
        }
    }
    if let Some(state) = value.get("status").and_then(Value::as_str) {
        p.title = format!("shell · {state}");
    }
    let notes = attention(fields);
    if !notes.is_empty() {
        p.title = format!("{} · {}", p.title, notes.join(" · "));
    }
    let mut streams = Vec::new();
    let count = ["output", "stdout", "stderr"]
        .iter()
        .filter(|key| value.get(**key).and_then(Value::as_str).is_some())
        .count()
        .max(1);
    let budget = (b.text.len() / count).max(128);
    let from_end = base_char_offset > 0;
    for key in ["output", "stdout", "stderr"] {
        if let Some(raw) = value.get(key).and_then(Value::as_str) {
            let (text, links) = sanitize_plain(raw, 0);
            let (text, omitted) = excerpt(&text, budget, from_end);
            p.body_omitted_chars = p.body_omitted_chars.saturating_add(omitted);
            for l in links {
                add_link(&mut p.links, &l.label, &l.url, b.source_start, b.source_end);
            }
            streams.push((
                match key {
                    "stdout" => OutputStream::Stdout,
                    "stderr" => OutputStream::Stderr,
                    _ => OutputStream::Combined,
                },
                text,
            ));
        }
    }
    if streams.is_empty() {
        b.text = if value.get("status").and_then(Value::as_str) == Some("cancelled")
            && value.get("executed").and_then(Value::as_bool) == Some(false)
        {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(|message| sanitize_plain(&short(message, 512), 0).0)
                .unwrap_or_else(|| "この呼出しは実行されていません。".into())
        } else {
            match value.get("status").and_then(Value::as_str) {
            Some("not_started") => "前回の終了時点では未開始でした。自動再実行はしていません。",
            Some("interrupted") => "前回の終了により結果は不確かです。再実行前に保存済みの出力と副作用を確認してください。",
            _ => "実行結果 · 詳細",
        }.into()
        };
        b.stream = None;
    } else {
        let (stream, text) = streams.remove(0);
        b.stream = Some(stream);
        b.text = text;
    }
    for (stream, text) in streams {
        trailing.push(PresentationBlock {
            kind: BlockKind::ToolResult,
            stream: Some(stream),
            tool: b.tool.clone(),
            text,
            source_start: b.source_start,
            source_end: b.source_end,
            source_exact: false,
            markdown: vec![],
            rendered: None,
        });
    }
}
