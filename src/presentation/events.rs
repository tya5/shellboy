//! Facts from persisted event/tool envelopes, independent of UI layout.
use super::{PresentationField, short};

pub(super) fn event_title(kind: &str) -> &str {
    match kind {
        "revisit" => "Revisit resumed",
        "index_state" => "索引の状態",
        "previous_instance_interrupted" => "前回の中断記録",
        "app_shutdown" => "終了時の保存記録",
        "turn_started" => "応答処理を開始",
        "run_started" => "作業開始",
        "thread_settings" | "connection_changed" | "model_changed" | "settings_changed" => {
            "接続・設定変更"
        }
        "instructions_observed" | "instruction_changed" | "agents_changed" | "agents_reference" => {
            "指示ファイル更新"
        }
        "recovery_resumed" => "復旧再開",
        "context_changed" => "状態変更",
        "child_recovery_wait" => "子の作業が復旧待ち",
        "recovery_wait" => "復旧待ち",
        "child_turn_completed" => "子の回答",
        "child_run_ended" => "子の作業終了",
        "shell_ended" => "コマンド終了",
        "child_report" => "子からの途中報告",
        _ => "その他の状態変更",
    }
}

pub(super) fn revisit_preview(event: &serde_json::Value) -> Option<String> {
    let text = event.pointer("/payload/instruction")?.as_str()?;
    let mut preview: String = text.chars().take(240).collect();
    if preview.len() < text.len() {
        preview.push('…');
    }
    Some(super::sanitize_plain(&preview, 0).0)
}

/// Automatic readiness is a routine observation, not a user setting change.
pub(super) fn routine_index_ready(event: &serde_json::Value) -> bool {
    use serde_json::Value;
    let before = &event["change"]["before"];
    let after = &event["change"]["after"];
    event["kind"] == "index_state"
        && after["active_state"] == "active"
        && after["paused"] == Value::Bool(false)
        && before.get("paused") != Some(&Value::Bool(true))
        && before
            .get("active_space")
            .and_then(Value::as_str)
            .is_none_or(|selected| {
                after.get("active_space").and_then(Value::as_str) == Some(selected)
            })
        && ["profile", "model", "pause_kind"].iter().all(|key| {
            before.get(*key).unwrap_or(&Value::Null) == after.get(*key).unwrap_or(&Value::Null)
        })
}

/// A saved observation is historical, even while the same pause remains current.
pub(super) fn index_pause_notice(event: &serde_json::Value) -> Option<(String, Vec<String>)> {
    let after = event.pointer("/change/after")?;
    if event["kind"] != "index_state" || after["paused"] != true {
        return None;
    }
    let reason = after["pause_kind"].as_str().unwrap_or("理由不明");
    let stopped = after["paused_at"].as_str().unwrap_or("時刻不明");
    let mut body = vec![
        format!("停止記録: {stopped} · 理由: {reason}"),
        "索引更新の停止記録です。現在の状態確認と再試行: /index → T。会話は続行できます。".into(),
    ];
    body.extend(changes(event));
    Some((
        format!("/index → T · 索引更新の停止記録 · {}", short(reason, 80)),
        body,
    ))
}

// Named copy keys for #35; facts and identity are supplied separately.
enum NoticeText {
    ShellEnded,
    ChildAnswer,
    ChildEnded,
    ChildReport,
    Delivered,
    UnknownResult,
}
impl NoticeText {
    fn text(self) -> &'static str {
        match self {
            Self::ShellEnded => "shell 終了",
            Self::ChildAnswer => "子の回答",
            Self::ChildEnded => "子の作業終了",
            Self::ChildReport => "子の途中報告",
            Self::Delivered => "モデルに渡した · Enter 参照先",
            Self::UnknownResult => "結果未記録",
        }
    }
}
fn continuation_title(kind: &str, event: &serde_json::Value) -> Option<String> {
    let key = match kind {
        "shell_ended" => NoticeText::ShellEnded,
        "child_turn_completed" => NoticeText::ChildAnswer,
        "child_run_ended" => NoticeText::ChildEnded,
        "child_report" => NoticeText::ChildReport,
        _ => return None,
    };
    let payload = event.get("payload").unwrap_or(event);
    let identity = payload
        .get(if kind == "shell_ended" {
            "execution_id"
        } else {
            "thread_id"
        })
        .and_then(serde_json::Value::as_str)
        .map(|id| super::sanitize_plain(&id.chars().take(16).collect::<String>(), 0).0)
        .unwrap_or_else(|| "対象未記録".into());
    let mut parts = vec![format!("{} · {identity}", key.text())];
    if let Some(name) = payload.get("title").and_then(serde_json::Value::as_str) {
        parts.push(
            super::sanitize_plain(&short(name, 80), 0)
                .0
                .replace(['\n', '\r'], " "),
        );
    }
    if let Some(command) = payload.get("command").and_then(serde_json::Value::as_str) {
        parts.push(
            super::sanitize_plain(&short(command, 80), 0)
                .0
                .replace(['\n', '\r'], " "),
        );
    }
    if let Some(code) = payload.get("exit_code").and_then(serde_json::Value::as_i64) {
        parts.push(format!("終了 {code}"));
    } else if let Some(state) = payload.get("state").and_then(serde_json::Value::as_str) {
        parts.push(super::sanitize_plain(&short(state, 24), 0).0);
    } else if kind == "shell_ended" || kind == "child_run_ended" {
        parts.push(NoticeText::UnknownResult.text().into());
    }
    if let Some(ms) = payload
        .get("elapsed_ms")
        .and_then(serde_json::Value::as_u64)
    {
        parts.push(format!("{}秒", ms / 1000));
    }
    parts.push(NoticeText::Delivered.text().into());
    Some(parts.join(" · "))
}

pub(super) fn title(kind: &str, event: &serde_json::Value) -> String {
    use serde_json::Value;
    if let Some((title, _)) = index_pause_notice(event) {
        return title;
    }
    if kind == "revisit" {
        return event
            .pointer("/payload/consumed_count")
            .and_then(Value::as_u64)
            .map(|count| format!("Revisit resumed · {count}"))
            .unwrap_or_else(|| "Revisit resumed".into());
    }
    // Canonical Shellboy notification nodes are added when preparing continuation
    // input. This describes receipt of that input, not its sole trigger or a
    // completed upstream request. Identity-less/unknown records stay factual.
    if event
        .get("event_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
        && let Some(title) = continuation_title(kind, event)
    {
        return title;
    }
    if routine_index_ready(event) {
        return "検索索引 · 利用可能".into();
    }
    let first = event.pointer("/change/before") == Some(&Value::Null);
    if first
        && matches!(
            kind,
            "instructions_observed" | "instruction_changed" | "agents_changed" | "agents_reference"
        )
        && let Some(present) = event
            .pointer("/change/after/present")
            .and_then(Value::as_bool)
    {
        return format!(
            "指示ファイル確認 · {}",
            if present { "あり" } else { "なし" }
        );
    }
    if first && kind == "index_state" {
        let after = &event["change"]["after"];
        if after.get("paused") == Some(&Value::Bool(false))
            && [
                "active_space",
                "profile",
                "model",
                "dimension",
                "active_state",
                "pause_kind",
            ]
            .iter()
            .all(|key| after.get(*key) == Some(&Value::Null))
        {
            return "索引の状態 · 未設定".into();
        }
    }
    let mut title = event_title(kind).to_owned();
    let changes = changes(event);
    if !changes.is_empty() {
        title = format!("{} · {}", title, changes.join(" / "));
    }
    title
}

fn saved_state(field: &PresentationField) -> Option<&str> {
    (field.key.ends_with("/state") || field.key.ends_with("/status"))
        .then_some(field.value.as_str())
}
fn output_incomplete(field: &PresentationField) -> bool {
    field.key.ends_with("/output_complete") && field.value == "false"
}
fn nonzero_exit(field: &PresentationField) -> Option<&str> {
    (field.key.ends_with("/exit_code") && field.value.parse::<i64>().is_ok_and(|code| code != 0))
        .then_some(field.value.as_str())
}

pub(super) fn event_description(fields: &[PresentationField]) -> (Vec<String>, Vec<String>) {
    let mut state = Vec::new();
    let mut body = Vec::new();
    let mut hints = Vec::new();
    for field in fields {
        let value = field.value.as_str();
        let short = if let Some(state) = saved_state(field) {
            match state {
                "recovery_wait" => Some("復旧待ち".into()),
                "failed" => Some("失敗".into()),
                "stopped" => Some("停止".into()),
                "cancelled" => Some("取消".into()),
                _ => None,
            }
        } else if let Some(code) = nonzero_exit(field) {
            Some(format!("exit {code}"))
        } else if output_incomplete(field) {
            Some("出力不完全".into())
        } else {
            None
        };
        if let Some(short) = short
            && !state.contains(&short)
        {
            state.push(short);
        }
        if (field.key.contains("/error/") && field.key.ends_with("/message"))
            || field.key.ends_with("/error")
        {
            if value != "null" && !body.iter().any(|s| s == value) {
                body.push(value.to_owned());
            }
        } else if field.key.ends_with("/hint") && !value.is_empty() {
            let hint = format!("対処: {value}");
            if !hints.contains(&hint) {
                hints.push(hint);
            }
        }
    }
    body.extend(hints);
    (state, body)
}
pub(super) fn attention(fields: &[PresentationField]) -> Vec<String> {
    let mut parts = Vec::new();
    for field in fields {
        let value = field.value.as_str();
        let note = if saved_state(field) == Some("recovery_wait") {
            Some("復旧待ち".into())
        } else if output_incomplete(field) {
            Some("出力不完全".into())
        } else if let Some(code) = nonzero_exit(field) {
            Some(format!("exit {code}"))
        } else if field.key.ends_with("/hint")
            || field.key.contains("/error/")
            || (field.key.ends_with("/error") && value != "null")
        {
            Some(short(value, 120))
        } else {
            None
        };
        if let Some(note) = note
            && !parts.contains(&note)
        {
            parts.push(note);
        }
    }
    parts
}

pub(super) fn changes(value: &serde_json::Value) -> Vec<String> {
    use serde_json::Value;
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    let Some(change) = value.get("change") else {
        return Vec::new();
    };
    let before = &change["before"];
    let after = &change["after"];
    let describe = |value: &Value| match value {
        Value::Null => "未設定".into(),
        Value::String(text) => super::sanitize_plain(&short(text, 80), 0).0,
        Value::Bool(value) => {
            if *value {
                "あり".into()
            } else {
                "なし".into()
            }
        }
        Value::Number(number) => number.to_string(),
        _ => "詳細参照".into(),
    };
    let keys: &[(&str, &str)] = match kind {
        "thread_settings" | "settings_changed" | "connection_changed" | "model_changed" => &[
            ("connection", "接続"),
            ("model", "モデル"),
            ("cwd", "作業場所"),
            ("effort", "推論effort"),
            ("title", "件名"),
        ],
        "index_state" => &[
            ("model", "埋め込みモデル"),
            ("profile", "接続"),
            ("active_state", "索引状態"),
            ("paused", "一時停止"),
            ("pause_kind", "停止理由"),
            ("dimension", "次元数"),
        ],
        "instructions_observed" | "instruction_changed" | "agents_changed" | "agents_reference" => {
            &[("path", "指示ファイル"), ("present", "存在")]
        }
        _ => &[],
    };
    keys.iter()
        .filter_map(|(key, label)| {
            let old = before.get(*key);
            let new = after.get(*key);
            let describe = |value: &Value| {
                if kind == "index_state" && *key == "active_state" {
                    match value.as_str() {
                        Some("active") => return "利用可能".into(),
                        Some("paused" | "candidate_paused") => return "一時停止".into(),
                        Some("candidate") => return "準備済み・切替前".into(),
                        Some("inactive") => return "未選択".into(),
                        Some(_) => return "詳細参照".into(),
                        None => {}
                    }
                }
                describe(value)
            };
            if old.unwrap_or(&Value::Null) == new.unwrap_or(&Value::Null) || new.is_none() {
                None
            } else if kind == "index_state" && before.is_null() {
                // Initial observation has no prior state to claim as a change.
                (new != Some(&Value::Bool(false)))
                    .then(|| format!("{label}: {}", describe(new.unwrap())))
            } else {
                Some(format!(
                    "{label}: {} → {}",
                    describe(old.unwrap_or(&Value::Null)),
                    describe(new.unwrap_or(&Value::Null))
                ))
            }
        })
        .collect()
}

pub(super) fn interruption(value: &serde_json::Value, kind: &str) -> (String, Vec<String>) {
    use serde_json::Value;
    if kind == "interrupted_wait" {
        let count = value
            .get("conditions")
            .and_then(Value::as_array)
            .map(Vec::len);
        return (
            "前回の待機観測は中断済み".into(),
            vec![
                count
                    .map(|n| format!("待機条件{n}件 · 対象の現在状態はF6の参照先で確認"))
                    .unwrap_or_else(|| "観測の記録を保持 · F6 原文".into()),
            ],
        );
    }
    if let Some(section) = value.get("section").and_then(Value::as_str) {
        let name = section_label(section);
        let count = value.get("items").and_then(Value::as_array).map(Vec::len);
        return (
            format!("前回の保存記録 · {name}"),
            vec![
                count
                    .map(|n| format!("この原文に{n}件 · 個別の対象はF6 詳細"))
                    .unwrap_or_else(|| "件数は原文で確認".into()),
            ],
        );
    }
    let mut body = Vec::new();
    if let Some(change) = value.get("change") {
        for section in [
            "runs",
            "turns",
            "shells",
            "tool_calls",
            "revisits",
            "owned_children",
            "unreflected_messages",
            "cancelled_waits",
            "pending_operations_with_unknown_targets",
        ] {
            if let Some(count) = change
                .get(section)
                .and_then(|section| section.get("count"))
                .and_then(Value::as_u64)
                .filter(|count| *count > 0)
            {
                body.push(format!("{} {count}件", section_label(section)));
            }
        }
        if change.get("result_uncertain") == Some(&Value::Bool(true)) {
            body.push("外部での実行結果は未確認 · F6で保存済みの対象を確認".into());
        }
        if change.get("automatic_replay") == Some(&Value::Bool(false)) {
            body.push("自動での再実行はしません".into());
        }
    }
    if body.is_empty() {
        body.push("保存した対象はF6 詳細・原文で確認".into());
    }
    let title = match value.get("kind").and_then(Value::as_str) {
        Some("app_shutdown") => "前回の終了時の記録",
        Some("previous_instance_interrupted") => "前回の中断記録",
        _ => "前回処理の保存記録",
    };
    (title.into(), body)
}
fn section_label(section: &str) -> &str {
    match section {
        "runs" => "作業",
        "turns" => "応答処理",
        "shells" => "コマンド",
        "tool_calls" => "中断時のツール",
        "revisits" => "Interrupted Revisit reservations",
        "owned_children" => "子の作業",
        "unreflected_messages" => "未反映の依頼",
        "cancelled_waits" => "中断した待機観測",
        "pending_operations_with_unknown_targets" => "対象不明の受付",
        _ => "対象",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuation_has_saved_identity_outcome_and_duration_without_inventing_name() {
        let event = serde_json::json!({"event_id":"saved", "payload":{"execution_id":"execution-complete-id", "exit_code":7, "elapsed_ms":316000}});
        let title = title("shell_ended", &event);
        assert!(title.contains("execution-comple"));
        assert!(
            title.contains("終了 7") && title.contains("316秒") && title.contains("モデルに渡した")
        );
        assert!(!title.contains("背景") && !title.contains("check.sh"));
    }
}
