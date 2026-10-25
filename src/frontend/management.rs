//! One active management response; bounded immutable detail pages, never JSON dumps.
use crate::{
    presentation::{self, PresentationField},
    protocol::*,
};
use serde_json::Value;
use std::mem::size_of;

/// The UI that issued an action, distinct from IDs referenced by that action.
pub(super) enum NoticeOwner {
    Thread(String),
    Draft(String),
}
impl NoticeOwner {
    pub(super) fn capture(
        context: Option<&crate::ui::ManagementContext>,
        submission: Option<&crate::ui::CommandSubmission>,
        thread: Option<&str>,
        draft: Option<&str>,
    ) -> Option<Self> {
        if let Some(context) = context {
            return Some(
                context
                    .owner_thread
                    .clone()
                    .map_or_else(|| Self::Draft(context.owner_draft.clone()), Self::Thread),
            );
        }
        submission
            .map(|source| Self::Draft(source.draft_id.clone()))
            .or_else(|| thread.map(|id| Self::Thread(id.into())))
            .or_else(|| draft.map(|id| Self::Draft(id.into())))
    }
    pub(super) fn update(self, text: String) -> crate::ui::UiUpdate {
        match self {
            Self::Thread(thread_id) => crate::ui::UiUpdate::Status {
                thread_id: Some(thread_id),
                text,
            },
            Self::Draft(draft_id) => crate::ui::UiUpdate::DraftStatus { draft_id, text },
        }
    }
}
/// Operation lookup uses Status's receipt-free route but is not an overview page.
pub(super) fn work_status(command: &Command) -> bool {
    matches!(command, Command::Status(args) if args.operation_id.is_none())
}
pub(super) fn completion_text(
    title: &str,
    stop: bool,
    response: &Result<Value, ApiError>,
) -> String {
    let id = response
        .as_ref()
        .ok()
        .and_then(|value| value.get("request_operation_id"))
        .and_then(Value::as_str);
    let status = match super::management_outcome(response) {
        Ok(()) if stop => "停止要求を受け付けました".to_owned(),
        Ok(()) => "受付結果: 成功".to_owned(),
        Err(error) => format!("受付結果: 失敗（{}）", error.message),
    };
    format!("{status} · {title} · 照会ID {}", id.unwrap_or("未取得"))
}

pub(super) struct Report {
    pub fields: Vec<PresentationField>,
    pub omitted: bool,
    pub detail: Option<Value>,
}
fn heap_bytes(value: &Value) -> usize {
    size_of::<Value>().saturating_add(match value {
        Value::String(text) => text.capacity(),
        Value::Array(items) => {
            items.capacity() * size_of::<Value>() + items.iter().map(heap_bytes).sum::<usize>()
        }
        // Conservative per-entry BTree node/link allowance; actual API frame is finite.
        Value::Object(items) => items
            .iter()
            .map(|(key, value)| key.capacity() + heap_bytes(value) + size_of::<[usize; 16]>())
            .sum(),
        _ => 0,
    })
}
pub(super) fn report(value: Value, budget: usize) -> Report {
    let safe = presentation::safe_detail_value(value);
    if safe.get("filters").is_some() && safe.get("items").is_some() {
        return work_report(safe, budget);
    }
    let mut fields = presentation::fields_of_value(&safe);
    presentation::fields::decorate(&mut fields, &safe);
    fn order(pointer: &str) -> (String, u8, String) {
        let (parent, key) = pointer.rsplit_once('/').unwrap_or(("", pointer));
        let parent = parent
            .split('/')
            .map(|part| {
                part.parse::<usize>()
                    .map(|n| format!("{n:020}"))
                    .unwrap_or_else(|_| part.to_owned())
            })
            .collect::<Vec<_>>()
            .join("/");
        let rank = match key {
            "title" => 0,
            "state" | "status" => 1,
            "thread_id" | "node_id" | "execution_id" => 2,
            "cwd" => 3,
            "request_operation_id" => 90,
            _ => 20,
        };
        (parent, rank, key.to_owned())
    }
    fields.sort_by_cached_key(|field| order(&field.key));
    fn count(value: &Value) -> usize {
        match value {
            Value::Object(items) => items.values().map(count).sum(),
            Value::Array(items) => 1 + items.iter().map(count).sum::<usize>(),
            _ => 1,
        }
    }
    let omitted = count(&safe) > fields.len() || fields.iter().any(|field| field.value_omitted);
    fields.insert(
        0,
        PresentationField {
            value_omitted: false,
            display: None,
            key: String::new(),
            label: "すべての詳細を見る".into(),
            value: "Enterで項目ごとの原文・参照を確認".into(),
        },
    );
    if let Some(error) = safe
        .get("error")
        .and_then(|error| serde_json::from_value::<ApiError>(error.clone()).ok())
    {
        let next = match error.code {
            ErrorCode::InvalidState | ErrorCode::TargetUnavailable => {
                "F8で現在の作業を確認してください。同じ内容の再試行は復旧待ちのときだけ使えます。新しい指示や停止とは別の操作です。"
            }
            ErrorCode::ConfigInvalid => {
                "接続・モデル・作業場所の設定を確認し、原因を直してから明示的に再試行してください。設定変更だけでは再開しません。"
            }
            ErrorCode::SaveFailed => {
                "保存障害のため新しい操作は停止しています。原因と保存状態を確認してください。"
            }
            _ => {
                "原因と対象をF8の詳細で確認し、必要な対処をしてから再操作してください。保存済み依頼と未受付の操作を区別してください。"
            }
        };
        fields.insert(
            1,
            PresentationField {
                value_omitted: false,
                display: None,
                key: String::new(),
                label: "次にできること".into(),
                value: next.into(),
            },
        );
        if let Some(field) = fields.iter_mut().find(|field| field.key == "/error/hint") {
            field.label = "対処の補足（原文）".into();
        }
    }
    let detail =
        (heap_bytes(&safe).saturating_add(size_of::<[usize; 2]>()) <= budget).then_some(safe);
    if detail.is_none() {
        fields.push(PresentationField { value_omitted: false, display: None, key: String::new(), label:"詳細の保持".into(), value:"本文cache予算を超えるため詳細は未保持。対象IDで原文pageを取得、または範囲を絞って再照会してください".into() });
    }
    Report {
        fields,
        omitted: omitted || detail.is_none(),
        detail,
    }
}

fn work_report(safe: Value, budget: usize) -> Report {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut fields = Vec::new();
    if let Some(items) = safe.get("items").and_then(Value::as_array) {
        for (index, item) in items.iter().enumerate() {
            let title: String = text(item, "thread_title").chars().take(36).collect();
            let title = if title.trim().is_empty() {
                "件名未設定".to_owned()
            } else {
                title
            };
            let id = text(item, "id");
            let short: String = id.chars().take(8).collect();
            let marker = match text(item, "state").as_str() {
                "failed" | "incomplete" => "×",
                "completed" | "exited" | "applied" => "✓",
                "held" | "recovery_wait" => "◷",
                _ => "●",
            };
            fields.push(PresentationField {
                value_omitted: false,
                display: None,
                key: format!("/items/{index}"),
                label: format!(
                    "{marker} {} · {}",
                    match text(item, "kind").as_str() {
                        "message" => "依頼",
                        "run" => "作業",
                        "turn" => "応答処理",
                        "shell" => "コマンド",
                        "wait" => "待機",
                        "attempt" | "model_attempt" => "モデル要求",
                        other => other,
                    },
                    presentation::fields::state_label(&text(item, "state"))
                ),
                value: format!(
                    "{title} · {short} · {}",
                    presentation::fields::local_time(&text(item, "activity_at"))
                        .unwrap_or_else(|| text(item, "activity_at"))
                ),
            });
        }
    }
    if fields.is_empty() {
        fields.push(PresentationField {
            value_omitted: false,
            display: None,
            key: "/filters".into(),
            label: "該当する保存状態".into(),
            value: "0件（非表示の終端状態は /status all で確認）".into(),
        });
    }
    fields.push(PresentationField {
        value_omitted: false,
        display: None,
        key: String::new(),
        label: "観測範囲・全詳細".into(),
        value: "保存済みの状態。Enterで所属・モデル・参照先・取得時刻を確認".into(),
    });
    let detail =
        (heap_bytes(&safe).saturating_add(size_of::<[usize; 2]>()) <= budget).then_some(safe);
    let omitted = detail.is_none();
    if omitted {
        fields.push(PresentationField {
            value_omitted: false,
            display: None,
            key: String::new(),
            label: "詳細未保持".into(),
            value: "cache予算を超えています。F8の作業状態で範囲・件数を絞って再取得".into(),
        });
    }
    Report {
        fields,
        omitted,
        detail,
    }
}

/// Resolve only explicit saved identifiers and read commands; never execute mutations from a view.
pub(super) fn follow(
    value: &Value,
    pointer: &str,
    mode: crate::ui::ManagementFollow,
) -> Result<Command, ApiError> {
    use crate::ui::ManagementFollow;
    let unavailable = || {
        ApiError::new(
            ErrorCode::InvalidArgument,
            "この項目に対応する参照先がありません。IDの項目を選択してください",
        )
    };
    if matches!(mode, ManagementFollow::Next | ManagementFollow::Refresh) {
        let mut args: StatusArgs =
            serde_json::from_value(value.get("filters").cloned().ok_or_else(unavailable)?)
                .map_err(|_| unavailable())?;
        args.cursor = if matches!(mode, ManagementFollow::Next) {
            Some(
                value
                    .get("next_cursor")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ApiError::new(ErrorCode::InvalidArgument, "次の一覧pageはありません")
                    })?
                    .into(),
            )
        } else {
            None
        };
        return Ok(Command::Status(args));
    }
    let selected = value.pointer(pointer).ok_or_else(unavailable)?;
    if let Some(targets) = selected.get("targets").and_then(Value::as_array) {
        let command: Command = if matches!(mode, ManagementFollow::Read) {
            targets
                .iter()
                .filter_map(|value| serde_json::from_value::<Command>(value.clone()).ok())
                .find(|command| matches!(command, Command::ShellRead(_)))
                .ok_or_else(unavailable)?
        } else {
            serde_json::from_value(targets.first().cloned().ok_or_else(unavailable)?)
                .map_err(|_| unavailable())?
        };
        return match command {
            Command::RunGet(_)
            | Command::TurnGet(_)
            | Command::MessageGet(_)
            | Command::ThreadGet(_)
            | Command::HistoryGet(_)
            | Command::ShellGet(_)
            | Command::ShellRead(_) => Ok(command),
            _ => Err(unavailable()),
        };
    }
    let id = selected.as_str().ok_or_else(unavailable)?.to_owned();
    match pointer.rsplit('/').next().unwrap_or("") {
        "run_id" | "owner_run_id" => Ok(Command::RunGet(RunTarget { run_id: id })),
        "turn_id" => Ok(Command::TurnGet(TurnTarget { turn_id: id })),
        "thread_id" | "origin_thread_id" | "parent_thread_id" => {
            Ok(Command::ThreadGet(ThreadTarget {
                thread_id: Some(id),
            }))
        }
        "message_id" => Ok(Command::MessageGet(MessageTarget { message_id: id })),
        "execution_id" if matches!(mode, ManagementFollow::Read) => {
            Ok(Command::ShellRead(ShellReadArgs {
                execution_id: id,
                cursor: None,
                encoding: None,
                limit_bytes: MAX_BODY_BYTES,
            }))
        }
        "execution_id" => Ok(Command::ShellGet(ExecutionTarget { execution_id: id })),
        "node_id" | "answer_node_id" | "request_node_id" | "result_node_id" => {
            Ok(Command::HistoryGet(HistoryGetArgs {
                node_id: id,
                cursor: None,
                encoding: DEFAULT_ENCODING.into(),
                limit_bytes: MAX_BODY_BYTES,
            }))
        }
        "operation_id" | "request_operation_id" => {
            Ok(Command::OperationGet(OperationTarget { operation_id: id }))
        }
        _ => Err(unavailable()),
    }
}
/// Structural field tree used for explicit detail pages and small fee previews.
fn tree(value: &Value, path: &str, labels: Option<&Value>, emit: &mut impl FnMut(&str)) {
    let label = labels
        .map(|root| presentation::fields::human_label(path, root))
        .unwrap_or_else(|| path.to_owned());
    match value {
        Value::Object(items) => {
            if items.is_empty() {
                emit(&format!("{label}: （空の項目）\n"));
            }
            for (key, value) in items {
                tree(
                    value,
                    &format!("{path}/{}", crate::json_pointer::escape_segment(key)),
                    labels,
                    emit,
                );
            }
        }
        Value::Array(items) => {
            emit(&format!("{label}: {}件\n", items.len()));
            for (index, value) in items.iter().enumerate() {
                tree(value, &format!("{path}/{index}"), labels, emit);
            }
        }
        Value::String(text) => {
            if !path.is_empty() {
                emit(&label);
                emit(": ");
            }
            emit(text);
            emit("\n");
        }
        _ => emit(&format!("{label}: {value}\n")),
    }
}
pub(super) fn preview_text(value: &Value) -> String {
    let mut text = String::new();
    tree(value, "", Some(value), &mut |part| text.push_str(part));
    text
}
pub(super) fn detail_page(
    value: &Value,
    pointer: &str,
    cursor: Option<&str>,
) -> Result<(String, Option<String>), ApiError> {
    let selected = value.pointer(pointer).ok_or_else(|| {
        ApiError::new(
            ErrorCode::TargetUnavailable,
            "Selected result field is unavailable; reload the result",
        )
    })?;
    let offset = cursor
        .map(|cursor| {
            PageCursor::decode(cursor, "ui_management_detail", pointer, 0, Some("UTF-8"))?
                .last_sort_key
                .parse::<usize>()
                .map_err(|_| {
                    ApiError::new(ErrorCode::CursorExpired, "Invalid management detail cursor")
                })
        })
        .transpose()?
        .unwrap_or(0);
    let limit = offset.saturating_add(MAX_BODY_BYTES as usize);
    let mut text = String::with_capacity(MAX_BODY_BYTES as usize);
    let mut position = 0usize;
    let mut invalid = false;
    // Walk fragments without constructing an unbounded flattened duplicate.
    let mut emit = |part: &str| {
        let start = position;
        position = position.saturating_add(part.len());
        if position <= offset || start >= limit {
            return;
        }
        let lower = offset.saturating_sub(start);
        let mut upper = limit.saturating_sub(start).min(part.len());
        if !part.is_char_boundary(lower) {
            invalid = true;
            return;
        }
        upper = part.floor_char_boundary(upper);
        if lower <= upper {
            text.push_str(&part[lower..upper]);
        }
    };
    if let Value::String(value) = selected {
        emit(value);
    } else {
        tree(selected, "", None, &mut emit);
    }
    if invalid || offset > position {
        return Err(ApiError::new(
            ErrorCode::CursorExpired,
            "Management detail cursor is outside this field",
        ));
    }
    let end = offset + text.len();
    Ok((
        text,
        (end < position)
            .then(|| {
                PageCursor {
                    version: 1,
                    scope: "ui_management_detail".into(),
                    filter: pointer.into(),
                    last_sort_key: end.to_string(),
                    watermark: 0,
                    revision: 0,
                    encoding: Some("UTF-8".into()),
                }
                .encode()
            })
            .transpose()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_receipt_lookup_does_not_enable_overview_navigation() {
        assert!(work_status(&Command::Status(StatusArgs::default())));
        assert!(!work_status(&Command::Status(StatusArgs {
            operation_id: Some("fixture:receipt".into()),
            ..Default::default()
        })));
        assert!(!work_status(&Command::ThreadGet(ThreadTarget {
            thread_id: Some("thread".into())
        })));
    }
    #[test]
    fn fee_preview_uses_human_structure_while_raw_detail_keeps_pointer_and_values() {
        let value = serde_json::json!({"coverage":{"eligible_sources":3},"sources":5,"segments":7,"characters":0,"already_indexed":2,"unavailable":1});
        let preview = preview_text(&value);
        assert!(preview.contains("索引状況 › 対象資料数: 3"), "{preview}");
        for text in [
            "資料数: 5",
            "区間数: 7",
            "対象文字数: 0",
            "索引済み: 2",
            "取得不能: 1",
        ] {
            assert!(preview.contains(text), "{preview}");
        }
        assert!(!preview.contains("/coverage/") && !preview.contains("eligible_sources"));
        let (raw, cursor) = detail_page(&value, "", None).unwrap();
        assert!(cursor.is_none());
        assert!(raw.contains("/coverage/eligible_sources: 3"), "{raw}");
        assert_eq!(
            detail_page(&value, "/coverage/eligible_sources", None)
                .unwrap()
                .0,
            ": 3\n"
        );
    }
    #[test]
    fn status_rows_never_have_blank_titles_or_raw_state_labels() {
        let value = serde_json::json!({"filters":{},"items":[{"kind":"message","state":"accepted","thread_title":"","id":"complete-message-id","activity_at":"2026-10-25T00:00:00Z"}]});
        let report = report(value.clone(), MAX_FRAME_BYTES);
        assert_eq!(report.fields[0].label, "● 依頼 · 受付済み");
        assert!(report.fields[0].value.starts_with("件名未設定"));
        assert!(!report.fields[0].value.contains("T00:00:00Z"));
        assert_eq!(report.detail, Some(value));
    }
    #[test]
    fn work_rows_keep_full_details_and_resolve_only_read_targets() {
        let args = StatusArgs {
            run_id: Some("owner".into()),
            ..Default::default()
        };
        let get = Command::ShellGet(ExecutionTarget {
            execution_id: "shell-full-id".into(),
        });
        let raw = Command::ShellRead(ShellReadArgs {
            execution_id: "shell-full-id".into(),
            cursor: None,
            encoding: None,
            limit_bytes: MAX_BODY_BYTES,
        });
        let value = serde_json::json!({"filters":args,"next_cursor":"page-2","items":[{"kind":"shell","state":"running","thread_title":"調査","id":"shell-full-id","activity_at":"2026-10-25T00:00:00Z","targets":[get,raw]}]});
        let report = report(value.clone(), MAX_FRAME_BYTES);
        assert_eq!(report.fields[0].key, "/items/0");
        assert!(report.fields[0].value.contains("調査"));
        assert_eq!(report.detail, Some(value.clone()));
        assert_eq!(
            follow(&value, "/items/0", crate::ui::ManagementFollow::Inspect).unwrap(),
            get
        );
        assert_eq!(
            follow(&value, "/items/0", crate::ui::ManagementFollow::Read).unwrap(),
            raw
        );
        assert!(
            matches!(follow(&value,"",crate::ui::ManagementFollow::Next).unwrap(),Command::Status(a) if a.cursor.as_deref()==Some("page-2") && a.run_id.as_deref()==Some("owner"))
        );
        assert!(
            matches!(follow(&value,"",crate::ui::ManagementFollow::Refresh).unwrap(),Command::Status(a) if a.cursor.is_none())
        );
        let dangerous =
            serde_json::json!({"targets":[Command::RunStop(RunTarget{run_id:"owner".into()})]});
        assert!(follow(&dangerous, "", crate::ui::ManagementFollow::Inspect).is_err());
    }
    use serde_json::json;
    #[test]
    fn field_preview_keeps_full_safe_detail_and_utf8_pages() {
        let mut object = serde_json::Map::new();
        for index in 0..150 {
            object.insert(format!("field-{index:03}"), json!(index));
        }
        let text = "日本語😀".repeat(24000);
        object.insert("a/b".into(), json!({"~key":text}));
        object.insert("encrypted_content".into(), json!("opaque"));
        let report = report(Value::Object(object), 8 * 1024 * 1024);
        assert!(report.omitted);
        let detail = report.detail.unwrap();
        assert!(detail.get("encrypted_content").is_none());
        assert_eq!(detail.pointer("/field-149"), Some(&json!(149)));
        let mut actual = String::new();
        let mut cursor = None;
        loop {
            let (page, next) = detail_page(&detail, "/a~1b/~0key", cursor.as_deref()).unwrap();
            assert!(page.len() <= MAX_BODY_BYTES as usize);
            actual.push_str(&page);
            if next.is_none() {
                break;
            }
            cursor = next;
        }
        assert_eq!(actual, text);
        assert!(detail_page(&detail, "/a~1b/~0key", Some("1")).is_err());
        let (_, next) = detail_page(&detail, "/a~1b/~0key", None).unwrap();
        assert_eq!(
            detail_page(&detail, "/field-149", next.as_deref())
                .unwrap_err()
                .code,
            ErrorCode::CursorExpired
        );
    }
    #[test]
    fn known_fields_have_human_labels_and_keep_exact_pointer_identity() {
        let report = report(
            json!({"items":[{"thread_id":"t","title":"調査","cwd":"/project","state":"idle"}],"future":{"a/b":true}}),
            65536,
        );
        let label = |key: &str| {
            report
                .fields
                .iter()
                .find(|field| field.key == key)
                .unwrap()
                .label
                .as_str()
        };
        assert_eq!(label("/items/0/cwd"), "会話 1 › 作業フォルダー");
        assert_eq!(label("/items/0/title"), "会話 1 › 件名");
        assert_eq!(label("/items/0/state"), "会話 1 › 状態");
        let keys = report
            .fields
            .iter()
            .filter(|field| field.key.starts_with("/items/0/"))
            .map(|field| field.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "/items/0/title",
                "/items/0/state",
                "/items/0/thread_id",
                "/items/0/cwd"
            ]
        );
        assert_eq!(label("/future/a~1b"), "future › a/b");
        assert_eq!(
            report.detail.unwrap().pointer("/items/0/title"),
            Some(&json!("調査"))
        );
    }
    #[test]
    fn zero_cache_budget_explicitly_declines_retention() {
        let report = report(json!({"future_field":{"unknown":"still visible"}}), 0);
        assert!(report.omitted && report.detail.is_none());
        assert!(report.fields.iter().any(|field|field.key=="/future_field/unknown"&&field.value=="still visible"));
        assert!(
            report
                .fields
                .iter()
                .any(|field| field.value.contains("未保持"))
        );
    }
}

#[cfg(test)]
mod recovery_hint_tests {
    use super::*;
    #[test]
    fn human_recovery_advice_keeps_the_original_hint_and_identifiers_accessible() {
        let value = serde_json::json!({"request_operation_id":"receipt-id","error":{"code":"invalid_state","message":"Run is not live","hint":"original machine hint"}});
        let report = report(value.clone(), 1024 * 1024);
        assert!(
            report
                .fields
                .iter()
                .any(|field| field.label == "次にできること" && field.value.contains("復旧待ち"))
        );
        assert!(
            report
                .fields
                .iter()
                .any(|field| field.key == "/error/hint" && field.value == "original machine hint")
        );
        assert_eq!(report.detail.unwrap(), value);
    }
}
