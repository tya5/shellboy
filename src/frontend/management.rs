//! One active management response; bounded immutable detail pages, never JSON dumps.
use crate::{config::UiLanguage, ui::i18n};
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
/// Only work status results enable overview navigation; operation lookups do not.
pub(super) fn work_status(command: &Command) -> bool {
    matches!(command, Command::Status(args) if !args.memory)
}
pub(super) fn completion_text(
    language: UiLanguage,
    title: &str,
    stop: bool,
    response: &Result<Value, ApiError>,
) -> String {
    let id = response
        .as_ref()
        .ok()
        .and_then(|value| value.get("request_operation_id"))
        .and_then(Value::as_str)
        .unwrap_or(i18n::local!(language, "未取得", "Not retrieved"));
    let status = match super::management_outcome(response) {
        Ok(()) if stop => i18n::local!(
            language,
            "停止要求を受け付けました",
            "Stop request accepted"
        )
        .to_owned(),
        Ok(()) => i18n::local!(language, "受付結果: 成功", "Admission result: success").to_owned(),
        Err(error) => match language {
            UiLanguage::Ja => format!("受付結果: 失敗（{}）", error.message),
            UiLanguage::En => format!("Admission result: failed ({})", error.message),
        },
    };
    let separator = crate::ui::text_width::glyphs::separator();
    match language {
        UiLanguage::Ja => format!("{status}{separator}{title}{separator}照会ID {id}"),
        UiLanguage::En => format!("{status}{separator}{title}{separator}Request ID {id}"),
    }
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
#[cfg(test)]
pub(super) fn report(value: Value, budget: usize) -> Report {
    report_in(UiLanguage::Ja, value, budget)
}
pub(super) fn report_in(language: UiLanguage, value: Value, budget: usize) -> Report {
    let mut filter_fields = Vec::new();
    presentation::content_filter::append_fields(&mut filter_fields, &value, language);
    let safe = presentation::safe_detail_value(value);
    if safe.get("filters").is_some() && safe.get("items").is_some() {
        return work_report(language, safe, budget);
    }
    let mut fields = presentation::fields::prioritized_fields(
        &safe,
        &[
            "thread_id",
            "run_id",
            "turn_id",
            "operation_id",
            "state",
            "scope",
            "count",
            "complete",
            "selection_exhausted",
            "global_order_complete",
            "notice",
            "view_limits",
            "view_observation",
            "client_observation",
            "/management/coverage_scope",
            "/management/coverage_window/window_complete",
            "/management/coverage_window/scope_exhausted",
            "/management/coverage",
            "/management/definitions",
            "/management/status",
            "/management/auto_history",
            "/management/automatic_history_reason",
            "/management/spaces",
            "/blockers/live_run_count",
            "/blockers/stopping_run_count",
            "root_revision",
            "snapshot_only",
            "deletable_now",
            "updated_at",
            "selection",
            "references",
            "observation",
            "targets",
            "downstream",
        ],
    );
    presentation::fields::decorate(&mut fields, &safe);
    fields.extend(filter_fields);
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
    // Keep primary status/permission facts visible before expansive sections.
    // This ordering is shared by result popups; exact pointers/full detail stay
    // unchanged, and nested per-row states do not crowd out the primary state.
    fn summary_rank(pointer: &str) -> u8 {
        match pointer {
            "/state" | "/paused" | "/management/status/paused" => 0,
            "/deletable_now" | "/snapshot_only" | "/result_uncertain" => 1,
            _ => 2,
        }
    }
    fields.sort_by_cached_key(|field| (summary_rank(&field.key), order(&field.key)));
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
            label: i18n::local!(language, "すべての詳細を見る", "View all details").into(),
            value: i18n::local!(
                language,
                "Enterで項目ごとの原文・参照を確認",
                "Enter checks each item's original text and references"
            )
            .into(),
        },
    );
    if let Some(error) = safe
        .get("error")
        .and_then(|error| serde_json::from_value::<ApiError>(error.clone()).ok())
    {
        let next = match error.code {
            ErrorCode::InvalidState | ErrorCode::TargetUnavailable => {
                i18n::local!(
                    language,
                    "F8で現在の作業を確認してください。同じ内容の再試行は復旧待ちのときだけ使えます。新しい指示や停止とは別の操作です。",
                    "Check current work with F8. Retry the same content only while awaiting recovery. This is separate from new instructions or stopping."
                )
            }
            ErrorCode::ConfigInvalid => {
                i18n::local!(
                    language,
                    "接続・モデル・作業場所の設定を確認し、原因を直してから明示的に再試行してください。設定変更だけでは再開しません。",
                    "Check connection, model, and working directory settings. Fix the cause, then explicitly retry. Settings changes alone do not resume work."
                )
            }
            ErrorCode::SaveFailed => {
                i18n::local!(
                    language,
                    "保存障害のため新しい操作は停止しています。原因と保存状態を確認してください。",
                    "New operations are stopped because saving failed. Check the cause and saved state."
                )
            }
            _ => {
                i18n::local!(
                    language,
                    "原因と対象をF8の詳細で確認し、必要な対処をしてから再操作してください。保存済み依頼と未受付の操作を区別してください。",
                    "Check the cause and target in F8 details before acting again. Distinguish saved requests from operations that were not accepted."
                )
            }
        };
        fields.insert(
            1,
            PresentationField {
                value_omitted: false,
                display: None,
                key: String::new(),
                label: i18n::local!(language, "次にできること", "Available next actions").into(),
                value: next.into(),
            },
        );
        if let Some(field) = fields.iter_mut().find(|field| field.key == "/error/hint") {
            field.label = i18n::local!(
                language,
                "対処の補足（原文）",
                "Additional guidance (original)"
            )
            .into();
        }
    }
    let detail =
        (heap_bytes(&safe).saturating_add(size_of::<[usize; 2]>()) <= budget).then_some(safe);
    if detail.is_none() {
        fields.push(PresentationField { value_omitted: false, display: None, key: String::new(), label:i18n::local!(language, "詳細の保持", "Detail retention").into(), value:i18n::local!(language, "本文cache予算を超えるため詳細は未保持。対象IDで原文pageを取得、または範囲を絞って再照会してください", "Details exceed the body-cache budget and were not retained. Read an original page by ID or narrow the query").into() });
    }
    Report {
        fields,
        omitted: omitted || detail.is_none(),
        detail,
    }
}

fn work_report(language: UiLanguage, safe: Value, budget: usize) -> Report {
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
                i18n::local!(language, "件名未設定", "Untitled").to_owned()
            } else {
                title
            };
            let id = text(item, "id");
            let short: String = id.chars().take(8).collect();
            let revisit = item
                .get("revisit")
                .cloned()
                .and_then(|value| {
                    serde_json::from_value::<crate::protocol::RevisitSummary>(value).ok()
                })
                .as_ref()
                .and_then(presentation::revisit_label)
                .map(|label| crate::ui::text_width::glyphs::formatted!(" · {label}", " | {label}"))
                .unwrap_or_default();
            let marker = match text(item, "state").as_str() {
                "failed" | "incomplete" => crate::ui::text_width::glyphs::select("×", "x"),
                "completed" | "exited" | "applied" => "✓",
                "held" | "recovery_wait" => "◷",
                _ => crate::ui::text_width::glyphs::select("●", "*"),
            };
            fields.push(PresentationField {
                value_omitted: false,
                display: None,
                key: format!("/items/{index}"),
                label: crate::ui::text_width::glyphs::formatted!(
                    "{marker} {} · {}",
                    "{marker} {} | {}",
                    match text(item, "kind").as_str() {
                        "message" => i18n::local!(language, "依頼", "Request"),
                        "run" => i18n::local!(language, "作業", "Work"),
                        "turn" => i18n::local!(language, "応答処理", "Response processing"),
                        "shell" => i18n::local!(language, "コマンド", "Command"),
                        "wait" => i18n::local!(language, "待機", "Wait"),
                        "attempt" | "model_attempt" =>
                            i18n::local!(language, "モデル要求", "Model request"),
                        other => other,
                    },
                    presentation::fields::state_label(&text(item, "state"))
                ),
                value: crate::ui::text_width::glyphs::formatted!(
                    "{title} · {short} · {}{revisit}",
                    "{title} | {short} | {}{revisit}",
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
            label: i18n::local!(language, "該当する保存状態", "Matching saved states").into(),
            value: i18n::local!(
                language,
                "0件（非表示の終端状態は /status all で確認）",
                "0 entries (use /status all for hidden terminal states)"
            )
            .into(),
        });
    }
    fields.push(PresentationField {
        value_omitted: false,
        display: None,
        key: String::new(),
        label: i18n::local!(
            language,
            "観測範囲・全詳細",
            "Observation scope and full details"
        )
        .into(),
        value: i18n::local!(
            language,
            "保存済みの状態。Enterで所属・モデル・参照先・取得時刻を確認",
            "Saved state. Enter checks ownership, model, references, and capture time"
        )
        .into(),
    });
    let detail =
        (heap_bytes(&safe).saturating_add(size_of::<[usize; 2]>()) <= budget).then_some(safe);
    let omitted = detail.is_none();
    if omitted {
        fields.push(PresentationField {
            value_omitted: false,
            display: None,
            key: String::new(),
            label: i18n::local!(language, "詳細未保持", "Details not retained").into(),
            value: i18n::local!(
                language,
                "cache予算を超えています。F8の作業状態で範囲・件数を絞って再取得",
                "The cache budget was exceeded. Narrow scope/count in F8 Work status, then refresh"
            )
            .into(),
        });
    }
    Report {
        fields,
        omitted,
        detail,
    }
}

/// Human-only saved reference targets; no new Core operation or CLI parser.
pub(super) enum FollowTarget {
    Command(Box<Command>),
    Attachment {
        thread_id: String,
        attachment_id: String,
    },
}
pub(super) fn follow_target(
    value: &Value,
    pointer: &str,
    mode: crate::ui::ManagementFollow,
) -> Result<FollowTarget, ApiError> {
    use crate::ui::ManagementFollow;
    if matches!(mode, ManagementFollow::Inspect | ManagementFollow::Read)
        && let Some((parent, "attachment_id")) = pointer.rsplit_once('/')
    {
        let row = value.pointer(parent).ok_or_else(|| {
            ApiError::new(
                ErrorCode::TargetUnavailable,
                "Saved attachment record is unavailable",
            )
        })?;
        if matches!(mode, ManagementFollow::Read) {
            let source=row.get("source_node_id").and_then(Value::as_str).filter(|s|!s.is_empty()).ok_or_else(||ApiError::new(ErrorCode::TargetUnavailable,"Saved attachment source node is unavailable; select its metadata to inspect saved selection"))?;
            return Ok(FollowTarget::Command(Box::new(Command::HistoryGet(
                HistoryGetArgs {
                    node_id: source.into(),
                    final_answer_only: false,
                    cursor: None,
                    encoding: DEFAULT_ENCODING.into(),
                    limit_bytes: MAX_BODY_BYTES,
                },
            ))));
        }
        let id = row
            .get("attachment_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::TargetUnavailable,
                    "Saved attachment identity is unavailable",
                )
            })?;
        let thread=row.get("thread_id").and_then(Value::as_str).filter(|s|!s.is_empty()).ok_or_else(||ApiError::new(ErrorCode::TargetUnavailable,"The selected attachment record has no explicit Thread; inspect its Thread attachment list"))?;
        return Ok(FollowTarget::Attachment {
            thread_id: thread.into(),
            attachment_id: id.into(),
        });
    }
    follow(value, pointer, mode).map(|command| FollowTarget::Command(Box::new(command)))
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
            Command::UsageGet(_)
            | Command::OperationGet(_)
            | Command::RunGet(_)
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
        "node_id" | "source_node_id" | "answer_node_id" | "request_node_id" | "result_node_id" => {
            Ok(Command::HistoryGet(HistoryGetArgs {
                final_answer_only: false,
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
#[cfg(test)]
pub(super) fn index_preview_text(value: &Value) -> String {
    index_preview_text_in(UiLanguage::Ja, value)
}
fn index_preview_text_in(language: UiLanguage, value: &Value) -> String {
    let mut lines = Vec::new();
    for (key, label) in [
        ("sources", i18n::local!(language, "資料数", "Sources")),
        ("segments", i18n::local!(language, "区間数", "Segments")),
        (
            "characters",
            i18n::local!(language, "対象文字数", "Target characters"),
        ),
        (
            "already_indexed",
            i18n::local!(language, "索引済み", "Already indexed"),
        ),
        (
            "unavailable",
            i18n::local!(language, "取得不能", "Unavailable"),
        ),
    ] {
        let count = value
            .get(key)
            .and_then(Value::as_u64)
            .map(|n| n.to_string())
            .unwrap_or_else(|| i18n::local!(language, "未取得", "Not retrieved").into());
        lines.push(format!("{label}: {count}"));
    }
    lines.push(
        match language {
            UiLanguage::Ja => crate::ui::text_width::glyphs::select(
                "API未送信 · 金額は未見積",
                "API未送信 | 金額は未見積",
            ),
            UiLanguage::En => crate::ui::text_width::glyphs::select(
                "No API request sent · Cost not estimated",
                "No API request sent | Cost not estimated",
            ),
        }
        .into(),
    );
    lines.join("\n")
}
#[cfg(test)]
pub(super) fn index_preview_captured_text(
    value: &Value,
    captured_at: chrono::DateTime<chrono::Utc>,
) -> String {
    index_preview_captured_text_in(UiLanguage::Ja, value, captured_at)
}
pub(super) fn index_preview_captured_text_in(
    language: UiLanguage,
    value: &Value,
    captured_at: chrono::DateTime<chrono::Utc>,
) -> String {
    let preview = index_preview_text_in(language, value);
    let captured = captured_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    match language {
        UiLanguage::Ja => {
            format!("{preview}\n取得時刻: {captured}\n再確認: 対象指定時は P、その他は閉じて再操作")
        }
        UiLanguage::En => format!(
            "{preview}\nCaptured at: {captured}\nRecheck: P for a specified target; otherwise close and repeat the operation"
        ),
    }
}
#[cfg(test)]
fn preview_text(value: &Value) -> String {
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
    fn localized_ui_projection_keeps_raw_details_ids_and_captured_time() {
        let captured = chrono::DateTime::parse_from_rfc3339("2026-11-01T12:34:56.789Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for language in [UiLanguage::En, UiLanguage::Ja] {
            for count in [0, 1, 2, 1_000_000] {
                let value = serde_json::json!({"sources":count,"segments":count,"characters":count,"already_indexed":count,"unavailable":count});
                let before = value.clone();
                let text = index_preview_captured_text_in(language, &value, captured);
                assert!(text.contains("2026-11-01T12:34:56.789Z"));
                assert!(text.contains(&format!(
                    "{}: {count}",
                    i18n::local!(language, "資料数", "Sources")
                )));
                assert!(text.contains(i18n::local!(
                    language,
                    "API未送信 | 金額は未見積",
                    "No API request sent | Cost not estimated"
                )));
                assert_eq!(value, before);
            }
            let raw = "RAW_PROVIDER 原文 /path~key id-42";
            let response = Err(ApiError::new(ErrorCode::InvalidArgument, raw));
            assert!(completion_text(language, "RAW_TITLE", false, &response).contains(raw));
            assert!(completion_text(language, "RAW_TITLE", false, &response).contains("RAW_TITLE"));
            let value = serde_json::json!({"error":ApiError::new(ErrorCode::SaveFailed,raw).hint(raw),"raw_status":"native-state","id":"id-42"});
            let expected = crate::presentation::safe_detail_value(value.clone());
            let localized = report_in(language, value.clone(), usize::MAX);
            assert_eq!(localized.detail.as_ref(), Some(&expected));
            let reference = report(value, usize::MAX);
            for (field, baseline) in localized.fields.iter().zip(reference.fields.iter()) {
                assert_eq!(field.key, baseline.key);
                if !field.key.is_empty() {
                    assert_eq!(field.value, baseline.value);
                }
            }
            assert!(localized.fields.iter().any(|field| field.label
                == i18n::local!(language, "次にできること", "Available next actions")));
        }
    }

    #[test]
    fn status_receipt_lookup_does_not_enable_overview_navigation() {
        assert!(work_status(&Command::Status(StatusArgs::default())));
        assert!(
            !work_status(&Command::Status(StatusArgs {
                memory: true,
                ..Default::default()
            })),
            "memory facts do not enable activity-list navigation"
        );
        assert!(!work_status(&Command::OperationGet(OperationTarget {
            operation_id: "fixture:receipt".into(),
        })));
        assert!(!work_status(&Command::ThreadGet(ThreadTarget {
            thread_id: Some("thread".into())
        })));
    }
    #[test]
    fn explicit_observation_completion_and_repeat_route_survive_field_budget() {
        let value = serde_json::json!({
            "caches": (0..500).map(|i| (format!("cache-{i}"), serde_json::json!(i))).collect::<serde_json::Map<_, _>>(),
            "client_observation": {
                "completed_at": "2026-11-01T00:00:00Z",
                "scope": "native memory observation",
                "refresh": "Repeat status --memory explicitly"
            }
        });
        let result = report(value, MAX_FRAME_BYTES);
        assert!(
            result
                .fields
                .iter()
                .any(|field| field.key == "/client_observation/completed_at"
                    && field.value == "2026-11-01T00:00:00Z")
        );
        assert!(
            result
                .fields
                .iter()
                .any(|field| field.key == "/client_observation/refresh"
                    && field.value.contains("explicitly"))
        );
    }
    #[test]
    fn fee_preview_uses_human_structure_while_raw_detail_keeps_pointer_and_values() {
        let value = serde_json::json!({"coverage":{"eligible_sources":3},"sources":5,"segments":7,"characters":0,"already_indexed":2,"unavailable":1});
        let preview = preview_text(&value);
        let index_preview = index_preview_text(&value);
        assert!(index_preview.starts_with("資料数: 5\n区間数: 7\n対象文字数: 0"));
        assert!(index_preview.contains("API未送信 | 金額は未見積"));
        let captured = chrono::DateTime::parse_from_rfc3339("2026-11-01T12:34:56.789Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let text = index_preview_captured_text(&value, captured);
        assert!(text.contains("取得時刻: 2026-11-01T12:34:56.789Z"));
        assert!(text.contains("再確認: 対象指定時は P"));
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
        assert_eq!(report.fields[0].label, "* 依頼 | 受付済み");
        assert!(report.fields[0].value.starts_with("件名未設定"));
        assert!(!report.fields[0].value.contains("T00:00:00Z"));
        assert_eq!(report.detail, Some(value));
    }
    #[test]
    fn work_rows_show_revisit_count_and_pending_without_instruction_text() {
        let value = serde_json::json!({"filters":{},"items":[{"kind":"run","state":"live","thread_title":"Work","id":"run","activity_at":"2026-10-25T00:00:00Z","revisit":{"pending":true,"consumed_count":5}}]});
        let projected = report(value.clone(), MAX_FRAME_BYTES);
        assert!(projected.fields[0].value.contains("↻5+"));
        assert_eq!(projected.detail, Some(value));
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

#[cfg(test)]
mod attachment_reference_tests {
    use super::*;
    use crate::ui::ManagementFollow;
    #[test]
    fn selected_record_owns_attachment_thread_and_source_without_viewer_fallback() {
        let value = serde_json::json!({"thread_id":"viewer","attachments":[{"thread_id":"foreign","attachment_id":"a","source_node_id":"source"}]});
        assert!(
            matches!(follow_target(&value,"/attachments/0/attachment_id",ManagementFollow::Inspect).unwrap(),FollowTarget::Attachment{thread_id,attachment_id} if thread_id=="foreign"&&attachment_id=="a")
        );
        assert!(
            matches!(follow_target(&value,"/attachments/0/attachment_id",ManagementFollow::Read).unwrap(),FollowTarget::Command(command) if matches!(command.as_ref(),Command::HistoryGet(a) if a.node_id=="source"))
        );
        assert!(
            matches!(follow(&value,"/attachments/0/source_node_id",ManagementFollow::Inspect).unwrap(),Command::HistoryGet(a) if a.node_id=="source")
        );
        let missing =
            serde_json::json!({"thread_id":"viewer","attachments":[{"attachment_id":"a"}]});
        assert!(
            follow_target(
                &missing,
                "/attachments/0/attachment_id",
                ManagementFollow::Inspect
            )
            .is_err()
        );
        assert!(
            follow_target(
                &missing,
                "/attachments/0/attachment_id",
                ManagementFollow::Read
            )
            .is_err()
        );
        assert!(
            follow_target(
                &value,
                "/attachments/9/attachment_id",
                ManagementFollow::Inspect
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod saved_review_field_tests {
    use super::*;
    #[test]
    fn primary_index_pause_state_precedes_expansive_management_sections() {
        let value = serde_json::json!({
            "management": {
                "status": {"paused": false, "counts": {
                    "done": {"value": 200, "complete": true},
                    "pending": {"value": 0, "complete": true}
                }},
                "coverage": {"not_indexed_sources": 0},
                "definitions": {"not_indexed_sources": "Current visible source coverage"},
                "spaces": (0..50).map(|i| serde_json::json!({
                    "id": format!("space-{i}"), "state": "active"
                })).collect::<Vec<_>>()
            }
        });
        let result = report(value.clone(), 8 * 1024 * 1024);
        // The common original-detail action stays first; primary saved state
        // must follow it, independent of the number of admitted section rows.
        assert_eq!(result.fields[1].key, "/management/status/paused");
        assert_eq!(result.fields[1].value, "false");
        assert!(result.fields[1].label.ends_with("一時停止"));
        for section in [
            "/management/coverage/not_indexed_sources",
            "/management/definitions/not_indexed_sources",
            "/management/spaces",
        ] {
            assert!(result.fields.iter().any(|field| field.key == section));
        }
        assert_eq!(result.detail, Some(value));
    }
    #[test]
    fn dense_blockers_cannot_hide_snapshot_permission_scope_or_reference_limits() {
        let value = serde_json::json!({"thread_id":"root","snapshot_only":true,"deletable_now":false,"root_revision":4,"blockers":{"live_run_count":50,"stopping_run_count":0,"runs":(0..50).map(|i|serde_json::json!({"id":format!("run-{i}"),"thread_id":format!("child-{i}"),"state":"live"})).collect::<Vec<_>>()},"selection":{"thread_count":50},"references":{"shared_path_nodes":3},"observation":{"references_complete":false},"view_observation":{"observed_at":"read-time"}});
        let report = report(value.clone(), 8 * 1024 * 1024);
        for (key, value) in [
            ("/thread_id", "root"),
            ("/snapshot_only", "true"),
            ("/deletable_now", "false"),
            ("/root_revision", "4"),
            ("/blockers/live_run_count", "50"),
            ("/blockers/stopping_run_count", "0"),
            ("/selection/thread_count", "50"),
            ("/references/shared_path_nodes", "3"),
            ("/observation/references_complete", "false"),
            ("/view_observation/observed_at", "read-time"),
        ] {
            assert_eq!(
                report
                    .fields
                    .iter()
                    .find(|field| field.key == key)
                    .map(|field| field.value.as_str()),
                Some(value),
                "missing {key}"
            );
        }
        assert!(report.omitted);
        assert_eq!(report.detail, Some(value));
    }
}

#[cfg(test)]
mod content_filter_tests {
    use super::*;
    #[test]
    fn turn_and_run_recovery_details_keep_raw_hint_and_localize_only_generated_advice() {
        use crate::{config::UiLanguage, model::content_filter::ContentFilterFacts};
        let facts = ContentFilterFacts::from_response(&serde_json::json!({
            "incomplete_details":{"reason":"content_filter"},
            "content_filter_results":{"protected_material_code":{"filtered":true,"detected":true}}
        }));
        let hint = serde_json::to_string(
            &ApiError::new(ErrorCode::IoFailed, "raw provider reason")
                .details(serde_json::json!({"content_filter":facts})),
        )
        .unwrap();
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let source = serde_json::json!({"run_id":"saved-run","turn_id":"saved-turn","recovery_hint":hint});
            let report = report_in(language, source.clone(), 1024 * 1024);
            let field = report
                .fields
                .iter()
                .find(|f| {
                    f.key.is_empty() && f.value.contains("protected_material_code/filtered=true")
                })
                .unwrap();
            let text = field.displayed();
            for expected in [
                "protected_material_code/filtered=true",
                "/retry",
                "/detach",
                "cp",
                "curl",
            ] {
                assert!(text.contains(expected), "{text}");
            }
            assert!(text.contains(match language {
                UiLanguage::Ja => "不明",
                UiLanguage::En => "Unknown",
            }));
            assert_eq!(
                report.detail.unwrap(),
                presentation::safe_detail_value(source)
            );
        }
    }
}

#[cfg(test)]
mod opaque_filter_string_tests {
    use super::*;

    #[test]
    fn content_filter_json_looking_string_values_are_not_decoded() {
        let raw = r#"{"provider":"still opaque"}"#;
        let facts = crate::model::content_filter::ContentFilterFacts::from_response(
            &serde_json::json!({"incomplete_details":{"reason":"content_filter"},"content_filter_results":{"protected_material_code":{"filtered":true,"diagnostic":raw}}}),
        );
        let hint = serde_json::to_string(
            &ApiError::new(ErrorCode::IoFailed, "raw provider diagnosis")
                .details(serde_json::json!({"content_filter":facts})),
        )
        .unwrap();
        let source = serde_json::json!({"recovery_hint":hint});
        let expected = format!("/diagnostic={}", serde_json::json!(raw));
        for language in [crate::config::UiLanguage::En, crate::config::UiLanguage::Ja] {
            let report = report_in(language, source.clone(), 1024 * 1024);
            assert!(
                report
                    .fields
                    .iter()
                    .any(|f| f.key.is_empty() && f.value.contains(&expected))
            );
            assert_eq!(
                report.detail.unwrap(),
                presentation::safe_detail_value(source.clone())
            );
        }
    }
}

#[cfg(test)]
mod completion_review_tests {
    use super::*;
    #[test]
    fn failed_admission_has_ascii_english_punctuation_and_unchanged_japanese() {
        let response = Err(ApiError::new(ErrorCode::IoFailed, "raw error"));
        let english = completion_text(UiLanguage::En, "fixture", false, &response);
        assert!(english.contains("Admission result: failed (raw error)"));
        assert!(english.is_ascii());
        let japanese = completion_text(UiLanguage::Ja, "fixture", false, &response);
        assert!(japanese.contains("受付結果: 失敗（raw error）"));
    }
}
