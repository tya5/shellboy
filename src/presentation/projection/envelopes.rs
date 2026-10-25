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
            "index_state" if event.pointer("/change/after/paused") == Some(&Value::Bool(true)) => {
                EventClass::Attention
            }
            "child_recovery_wait" | "recovery_wait" => EventClass::Attention,
            "recovery_resumed"
            | "child_turn_completed"
            | "child_run_ended"
            | "shell_ended"
            | "child_report" => EventClass::Change,
            "index_state"
            | "thread_settings"
            | "settings_changed"
            | "connection_changed"
            | "model_changed"
            | "instructions_observed"
            | "instruction_changed"
            | "agents_changed" => EventClass::Change,
            _ => EventClass::Unknown,
        };
        p.title = events::title(event_kind, event);
        let (states, body) = event_description(fields);
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
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .map(|s| tool_reason(&s))
            .unwrap_or_default();
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
        b.text = "実行結果 · 詳細".into();
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
