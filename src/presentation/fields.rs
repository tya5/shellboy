//! Shared human labels; exact JSON pointers and values remain authoritative.
use super::{MAX_FIELDS, MAX_METADATA_BYTES, PresentationField, sanitize_plain, secret, short};
use serde_json::Value;
use std::mem::size_of;

fn field_name(key: &str) -> &str {
    match key {
        "command" => "コマンド",
        "model_id" => "モデル",
        "profile_id" => "接続",
        "observed_at" => "観測日時",
        "completed_at" => "取得完了日時",
        "client_observation" => "明示要求の観測",
        "refresh" => "再取得方法",
        "activity_at" => "最終の状態変化",
        "accepted_at" => "受付日時",
        "previous_state" => "変更前の状態",
        "planned_state" => "予定した状態",
        "startup_state" => "再起動後の状態",
        "call_id" => "呼出しID",
        "group_id" => "呼出し群ID",
        "summary_format" => "要約形式",
        "summary_part_keys" => "要約part参照",
        "summary_reduce_keys" => "要約reduce参照",
        "elapsed_ms" => "実行時間（ms）",
        "item" => "元の項目",
        "output" => "返却結果",
        "stdout" => "通常出力",
        "stderr" => "エラー出力",
        "role" => "発話者",
        "type" => "項目の種別",
        "thread_id" => "会話ID",
        "node_id" => "履歴node ID",
        "run_id" => "作業ID",
        "turn_id" => "応答処理ID",
        "execution_id" => "実行ID",
        "message_id" => "依頼ID",
        "attachment_id" => "添付ID",
        "operation_id" => "操作ID",
        "request_operation_id" => "この操作の受付ID",
        "owner_run_id" => "所有する親作業",
        "parent_id" => "親会話",
        "title" => "件名",
        "cwd" => "作業フォルダー",
        "agents_path" => "指示ファイル参照",
        "state" | "status" => "状態",
        "updated_at" => "更新日時",
        "created_at" => "作成日時",
        "started_at" => "開始日時",
        "ended_at" => "終了日時",
        "excerpt" => "抜粋",
        "text" => "本文",
        "kind" => "種類",
        "path" => "ファイルパス",
        "connection" | "profile" => "接続",
        "model" => "モデル",
        "effort" => "推論effort",
        "reason" => "理由",
        "message" => "内容",
        "hint" => "次の操作",
        "code" => "コード",
        "error" => "エラー",
        "details" => "詳細",
        "management" => "管理情報",
        "coverage" => "索引状況",
        "model_attempts" => "通常のモデル要求",
        "compaction" => "要約使用量",
        "total" => "合計使用量",
        "compactions" => "要約回数",
        "requests" => "要求数",
        "map_requests" => "map要求数",
        "reduce_requests" => "reduce要求数",
        "summary_requests" => "要約要求数",
        "definitions" => "項目の定義",
        "spaces" => "索引空間",
        "selected_space" => "選択中の空間",
        "active_space" => "検索中の空間",
        "space_id" => "空間ID",
        "pending" => "保留中",
        "running" => "処理中",
        "done" => "完了",
        "failed" => "失敗",
        "paused" => "一時停止",
        "paused_at" => "停止記録の日時",
        "pause_id" => "停止記録ID",
        "pause_kind" => "停止理由",
        "requested_dimensions" => "要求次元数",
        "dimension" => "実測次元数",
        "int8_scale" => "8ビット化の倍率",
        "retained" => "削除せず保持",
        "partial" => "未完成の索引",
        "retained_remove_with" => "保持中の索引を削除する操作",
        "retained_vector_payload_bytes" => "保持中のベクトル本体（bytes）",
        "vector_payload_bytes" => "ベクトル本体（bytes）",
        "spaces_total" => "保持空間数",
        "spaces_truncated" => "空間一覧の省略あり",
        "probes" => "接続検証の記録",
        "not_indexed_sources" => "未索引の資料数",
        "partially_indexed_sources" => "一部索引済みの資料数",
        "fully_indexed_sources" => "全体索引済みの資料数",
        "coverage_unverified_sources" => "全体範囲未確認の資料数",
        "eligible_sources" => "対象資料数",
        "unavailable_sources" => "取得不能の資料数",
        "not_indexable_sources" => "索引対象外の資料数",
        "done_segments" => "完了区間数",
        "pending_segments" => "保留区間数",
        "failed_segments" => "失敗区間数",
        "stale_segments" => "旧版区間数",
        "sources" => "資料数",
        "segments" => "区間数",
        "characters" => "対象文字数",
        "already_indexed" => "索引済み",
        "unavailable" => "取得不能",
        "logical_live_bytes" => "論理使用量（bytes）",
        "reclaimable_bytes" => "回収可能量の下限（bytes）",
        "db_bytes" => "DBサイズ（bytes）",
        "wal_bytes" => "WALサイズ（bytes）",
        "managed_bytes" => "管理ファイル（bytes）",
        "log_bytes" => "診断log（bytes）",
        "warning" => "容量警告",
        "limit_reached" => "容量上限",
        "database_reusable_bytes" => "DB内再利用可能量（bytes）",
        "logical_history_bytes" => "履歴の論理量（bytes）",
        "logical_raw_bytes" => "rawの論理量（bytes）",
        "logical_index_bytes" => "索引の論理量（bytes）",
        "logical_managed_bytes" => "管理ファイルの論理量（bytes）",
        "delivery_status" => "依頼の適用状態",
        "assignment" => "割当",
        "answer_node_id" => "回答node ID",
        "end_message_node_id" => "終了メッセージnode ID",
        "selected" => "選択中",
        "managed" => "管理対象",
        "output_complete" => "出力回収完了",
        "exit_code" => "終了コード",
        "raw_bytes" => "raw読取り量（bytes）",
        "bytes_written" => "書込み量（bytes）",
        "stdin_open" => "stdin開放中",
        "encoding" => "文字encoding",
        "decode_errors" => "文字変換エラー",
        "cursor" => "継続cursor",
        "next_cursor" => "次page cursor",
        "before" => "変更前",
        "after" => "変更後",
        "matched" => "成立した条件",
        "snapshot" => "観測時の状態",
        "conditions" => "待機条件",
        "items" => "項目",
        "attachments" => "添付",
        "scope" => "対象範囲",
        "distance" => "検索距離",
        "spans" => "索引区間",
        "context" => "実行・保存情報",
        "lifetime_ms" => "生成から終了までの経過時間（ms）",
        "failure_reason" => "失敗理由",
        "recovery_reason" => "復旧・中断理由",
        "stop_reason" => "終了理由",
        "hold_reason" => "保留理由",
        "thread_scope" => "会話内の検索範囲",
        "on_current_path" => "現在の採用経路上",
        "current_instance" => "現在のowner instance",
        "instance_id" => "受付instance ID",
        "result_uncertain" => "外部副作用の結果が不確実",
        "replay_policy" => "同じ操作IDの再送規則",
        "targets" => "追跡先",
        "downstream" => "受付後の処理",
        "deletable_now" => "現在の削除可否",
        "snapshot_only" => "照会時点だけの判定",
        "root_revision" => "参照rootの版",
        "selection" => "選択範囲",
        "thread_count" => "対象会話数",
        "descendant_thread_count" => "生存子孫会話数",
        "surviving_descendant_thread_count" => "削除範囲外の子孫会話数",
        "latest_updated_at" => "対象内の最終更新日時",
        "blockers" => "削除を阻む作業",
        "live_run_count" => "実行中作業数",
        "stopping_run_count" => "停止処理中作業数",
        "runs" => "作業一覧",
        "truncated" => "一覧の残りあり",
        "reference_scope" => "参照数の集計範囲",
        "references" => "直接参照",
        "selected_path_nodes" => "対象経路のnode数",
        "shared_path_nodes" => "他会話と共有するnode数",
        "surviving_path_thread_count" => "共有先の生存会話数",
        "completed_replacement_source_nodes" => "completed置換の出典node数",
        "surviving_replacement_source_nodes" => "生存会話の置換出典node数",
        "attachment_reference_count" => "添付参照数",
        "external_attachment_reference_count" => "外部添付参照数",
        "managed_attachment_reference_count" => "管理添付参照数",
        "reclaimable_reason" => "回収容量を算定できない理由",
        "provenance" => "出典情報",
        "origin_thread_id" => "生成元会話ID",
        "source_revision" => "原文revision",
        "replacement_id" => "置換記録ID",
        "source_hash" => "記録時の出典hash",
        "content_hash" => "区間内容hash",
        "body_kind" => "原文本文種別",
        "start" => "開始位置",
        "end" => "終了位置",
        "previous_node_ids" => "前のnode",
        "next_node_ids" => "後のnode",
        "source_node_ids" => "出典node",
        "tombstone_reason" => "原文回収理由",
        _ => key,
    }
}
fn item_name(value: &Value, collection: &str) -> &'static str {
    if value.get("thread_id").is_some() && value.get("cwd").is_some() {
        "会話"
    } else if value.get("distance").is_some() {
        "検索結果"
    } else if value.get("node_id").is_some() {
        "履歴項目"
    } else {
        match collection {
            "spaces" => "索引空間",
            "attachments" => "添付",
            "conditions" | "matched" => "条件",
            _ => "項目",
        }
    }
}
fn english_key(value: &str) -> String {
    match value {
        "input_bytes" => return "Input bytes (UTF-8)".into(),
        "input_tokens" => return "Input tokens (provider-reported)".into(),
        "cached_tokens" => return "Cached input tokens (provider-reported)".into(),
        "model_attempts" => return "Model Attempts".into(),
        "compaction" => return "Summary Usage".into(),
        "total" => return "Combined Total".into(),
        "compactions" => return "Compactions".into(),
        "requests" => return "Requests".into(),
        "map_requests" => return "Map Requests".into(),
        "reduce_requests" => return "Reduce Requests".into(),
        "summary_requests" => return "Summary Requests".into(),
        "current_source" => return "Current Source".into(),
        "repeat_result" => return "Repeat Result".into(),
        "limited_fields" => return "Limited Fields".into(),
        "captured_result_at" => return "Captured Result At".into(),
        "body_available" => return "Body Available".into(),
        "body_hash" => return "Body Hash".into(),
        "source_revision" => return "Source Revision".into(),
        "tombstone_reason" => return "Tombstone Reason".into(),
        _ => {}
    }
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| match part {
            "id" => "ID".to_owned(),
            "ids" => "IDs".to_owned(),
            "api" => "API".to_owned(),
            "db" => "DB".to_owned(),
            "ms" => "ms".to_owned(),
            "bytes" => "bytes".to_owned(),
            other => {
                let mut chars = other.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(chars).collect()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn display_label(key: &str, label: &str, language: crate::config::UiLanguage) -> String {
    if language == crate::config::UiLanguage::Ja || label.is_ascii() {
        return label.to_owned();
    }
    let segments: Vec<_> = key.trim_start_matches('/').split(['/', '.']).collect();
    let numeric = segments
        .iter()
        .enumerate()
        .filter_map(|(index, segment)| {
            segment
                .parse::<usize>()
                .ok()
                .map(|position| (index, position))
        })
        .next();
    let leaf = segments.last().copied().unwrap_or("");
    let mut english = if let Some((index, position)) = numeric {
        let collection = index
            .checked_sub(1)
            .and_then(|i| segments.get(i))
            .copied()
            .unwrap_or("");
        let singular = match collection {
            "answers" => "Answer",
            "items" | "output" if segments.contains(&"content") => "Answer",
            "items" | "output" => "Item",
            "messages" => "Message",
            "nodes" => "Node",
            "runs" => "Run",
            "tools" => "Tool",
            "attachments" => "Attachment",
            "threads" => "Thread",
            "turns" => "Turn",
            other => other.strip_suffix('s').unwrap_or(other),
        };
        format!(
            "{singular} {} › {}",
            position.saturating_add(1),
            english_key(leaf)
        )
    } else {
        english_key(leaf)
    };
    if english.is_empty() {
        english = "Field".into();
    }
    let suffix = match label {
        "モデル要求ID（使用量の記録元）" => " (usage source)",
        _ if label.contains("件数") => " (count)",
        _ => "",
    };
    english.push_str(suffix);
    english
}

pub(crate) fn display_value(
    key: &str,
    value: &str,
    displayed: &str,
    language: crate::config::UiLanguage,
) -> String {
    use crate::config::UiLanguage::{En, Ja};
    if language == Ja {
        return displayed.to_owned();
    }
    let key = key.rsplit(['/', '.']).next().unwrap_or(key);
    if matches!(
        key,
        "state" | "status" | "previous_state" | "startup_state" | "planned_state"
    ) {
        return state_label_for(value, En).to_owned();
    }
    match displayed {
        "未確定" => "Undetermined".into(),
        "保持済みの表示用コピーを継続表示。今回のraw再取得はIoFailed。現在のraw可用性は未確認。" => {
            "Showing the retained display copy. Raw refresh failed (IoFailed); current raw availability is unknown.".into()
        }
        "[深い構造 · 詳細は原文参照]" | "[深い構造 | 詳細は原文参照]" => {
            "[Deep structure; see original for details]".into()
        }
        other => other.to_owned(),
    }
}

pub(crate) fn human_label(pointer: &str, root: &Value) -> String {
    human_label_for(pointer, root, crate::config::UiLanguage::Ja)
}

pub(crate) fn human_label_for(
    pointer: &str,
    root: &Value,
    language: crate::config::UiLanguage,
) -> String {
    let segments: Vec<_> = pointer
        .trim_start_matches('/')
        .split('/')
        .map(crate::json_pointer::unescape_segment)
        .collect();
    let mut labels = Vec::new();
    let mut value = root;
    let mut collection = "";
    for (index, key) in segments.iter().enumerate() {
        if let Value::Array(items) = value
            && let Ok(position) = key.parse::<usize>()
        {
            value = items.get(position).unwrap_or(&Value::Null);
            let name = match language {
                crate::config::UiLanguage::Ja => item_name(value, collection).to_owned(),
                crate::config::UiLanguage::En => match collection {
                    "answers" => "Answer".into(),
                    "items" | "output" => "Item".into(),
                    "messages" => "Message".into(),
                    "nodes" => "Node".into(),
                    "runs" => "Run".into(),
                    "tools" => "Tool".into(),
                    "attachments" => "Attachment".into(),
                    "threads" => "Thread".into(),
                    "turns" => "Turn".into(),
                    other => english_key(other),
                },
            };
            labels.push(format!("{name} {}", position + 1));
            continue;
        }
        value = value.get(key).unwrap_or(&Value::Null);
        collection = key;
        if !matches!(value, Value::Array(_)) || index + 1 == segments.len() {
            labels.push(match language {
                crate::config::UiLanguage::Ja => field_name(key).to_owned(),
                crate::config::UiLanguage::En => english_key(key),
            });
        }
    }
    let mut label = labels.join(" › ");
    if value.is_array() {
        label.push_str(match language {
            crate::config::UiLanguage::Ja => "（件数）",
            crate::config::UiLanguage::En => " (count)",
        });
    }
    label
}

/// Stable semantic sections; presentation labels are never parsed as machine keys.
pub(crate) fn group_label(
    pointer: &str,
    language: crate::config::UiLanguage,
) -> (u8, &'static str) {
    let (id, japanese) = group(pointer);
    let label = match language {
        crate::config::UiLanguage::Ja => japanese,
        crate::config::UiLanguage::En => match id {
            0 => "Key facts",
            1 => "Execution",
            2 => "Output",
            3 => "References",
            5 => "Summary Usage",
            6 => "Combined Total",
            7 => "Model Attempts",
            _ => "Other",
        },
    };
    (id, label)
}

pub(crate) fn group(pointer: &str) -> (u8, &'static str) {
    let segments = pointer.split('/').collect::<Vec<_>>();
    if segments.contains(&"compaction") {
        return (5, "要約使用量");
    }
    if segments.contains(&"total") {
        return (6, "合計使用量");
    }
    if segments.contains(&"model_attempts") {
        return (7, "通常のモデル要求");
    }
    let key = pointer.rsplit('/').next().unwrap_or(pointer);
    if matches!(key, "model_id" | "profile_id") {
        return (1, "実行");
    }
    if key.ends_with("_id")
        || key.ends_with("_ids")
        || key.ends_with("_hash")
        || matches!(
            key,
            "cursor" | "next_cursor" | "path" | "source_hash" | "body_hash"
        )
    {
        return (3, "参照");
    }
    if matches!(
        key,
        "command"
            | "cwd"
            | "agents_path"
            | "model"
            | "model_id"
            | "connection"
            | "profile"
            | "effort"
            | "exit_code"
            | "elapsed_ms"
            | "lifetime_ms"
            | "created_at"
            | "started_at"
            | "ended_at"
            | "updated_at"
            | "observed_at"
            | "activity_at"
            | "cached_tokens"
            | "input_tokens"
            | "input_bytes"
    ) {
        return (1, "実行");
    }
    if matches!(
        key,
        "text"
            | "output"
            | "stdout"
            | "stderr"
            | "output_complete"
            | "raw_bytes"
            | "encoding"
            | "decode_errors"
    ) {
        return (2, "出力");
    }
    if matches!(
        key,
        "state"
            | "status"
            | "kind"
            | "title"
            | "reason"
            | "message"
            | "hint"
            | "code"
            | "error"
            | "failure_reason"
            | "stop_reason"
            | "recovery_reason"
            | "result_uncertain"
    ) {
        return (0, "要点");
    }
    (4, "その他")
}
/// F6 request size fields (#186) for the adopted attempt of an answer. The
/// saved values are language-neutral: counts, JSON, or a fixed unknown marker.
pub(crate) const ATTEMPT_INPUT_BYTES: &str = "/ui_model_attempt/input_bytes";
pub(crate) const ATTEMPT_INPUT_TOKENS: &str = "/ui_model_attempt/input_tokens";
pub(crate) const ATTEMPT_CACHED_TOKENS: &str = "/ui_model_attempt/cached_tokens";
pub(crate) const ATTEMPT_ID: &str = "/ui_model_attempt/attempt_id";
/// Not saved for this request (bytes before #186, or no request record).
pub(crate) const NOT_RECORDED: &str = "未記録";
/// The provider did not report this count.
pub(crate) const NOT_REPORTED: &str = "未報告";
pub(crate) const NOT_REPORTED_NO_REQUEST: &str = "未報告（要求の記録なし）";
pub(crate) const NOT_REPORTED_AMBIGUOUS: &str = "未報告（要求との対応が一意でない）";

/// The label of a request size field in `language`, if `key` is one.
pub(crate) fn attempt_label(
    key: &str,
    language: crate::config::UiLanguage,
) -> Option<&'static str> {
    use crate::config::UiLanguage::{En, Ja};
    Some(match (key, language) {
        (ATTEMPT_INPUT_BYTES, Ja) => "入力のバイト数（UTF-8）",
        (ATTEMPT_INPUT_BYTES, En) => "Input bytes (UTF-8)",
        (ATTEMPT_INPUT_TOKENS, Ja) => "入力トークン（提供元の報告）",
        (ATTEMPT_INPUT_TOKENS, En) => "Input tokens (provider-reported)",
        (ATTEMPT_CACHED_TOKENS, Ja) => "キャッシュ済み入力トークン（提供元の報告）",
        (ATTEMPT_CACHED_TOKENS, En) => "Cached input tokens (provider-reported)",
        (ATTEMPT_ID, Ja) => "モデル要求ID（使用量の記録元）",
        (ATTEMPT_ID, En) => "Model request ID (usage source)",
        _ => return None,
    })
}

/// The displayed value of a request size field. Bytes and tokens are shown
/// separately as measured or reported; neither is ever derived from the other.
pub(crate) fn attempt_value(
    key: &str,
    value: &str,
    language: crate::config::UiLanguage,
) -> Option<String> {
    use crate::config::UiLanguage::En;
    let unknown = |text: &str| -> Option<String> {
        Some(
            match (text, language) {
                (NOT_RECORDED, En) => "Not recorded",
                (NOT_REPORTED, En) => "Not reported",
                (NOT_REPORTED_NO_REQUEST, En) => "Not reported (no request record)",
                (NOT_REPORTED_AMBIGUOUS, En) => "Not reported (request not uniquely matched)",
                (text, _) if text.parse::<u64>().is_ok() => return None,
                (text, _) => text,
            }
            .to_owned(),
        )
    };
    match key {
        ATTEMPT_INPUT_BYTES => {
            let Some(bytes) = serde_json::from_str::<Value>(value)
                .ok()
                .and_then(|saved| crate::model::wire_bytes::InputBytes::from_value(&saved))
            else {
                return unknown(value);
            };
            Some(input_bytes_text(&bytes, language))
        }
        ATTEMPT_INPUT_TOKENS | ATTEMPT_CACHED_TOKENS => match value.parse::<u64>() {
            Ok(count) => Some(digits(count)),
            Err(_) => unknown(value),
        },
        _ => None,
    }
}

/// `合計 5,452 / システム 1,491 / ...`: the total, then every nonzero source.
pub(crate) fn input_bytes_text(
    bytes: &crate::model::wire_bytes::InputBytes,
    language: crate::config::UiLanguage,
) -> String {
    use crate::config::UiLanguage::{En, Ja};
    use crate::model::wire_bytes::Category::*;
    let mut parts = vec![format!(
        "{} {}",
        match language {
            Ja => "合計",
            En => "Total",
        },
        digits(bytes.total)
    )];
    for category in crate::model::wire_bytes::Category::ALL {
        let count = bytes.get(category);
        if count == 0 {
            continue;
        }
        let name = match (category, language) {
            (System, Ja) => "システム",
            (System, En) => "System",
            (Instructions, Ja) => "指示",
            (Instructions, En) => "Instructions",
            (Context, Ja) => "Shellboy context",
            (Context, En) => "Shellboy context",
            (User, Ja) => "利用者",
            (User, En) => "User",
            (Assistant, Ja) => "モデルの発言",
            (Assistant, En) => "Assistant",
            (Reasoning, Ja) => "推論",
            (Reasoning, En) => "Reasoning",
            (ToolCall, Ja) => "ツール呼出し",
            (ToolCall, En) => "Tool calls",
            (ToolResult, Ja) => "ツール結果",
            (ToolResult, En) => "Tool results",
            (SpillPlaceholder, Ja) => "spill参照",
            (SpillPlaceholder, En) => "Spill placeholders",
            (Summary, Ja) => "要約",
            (Summary, En) => "Summary",
            (Notice, Ja) => "通知・イベント",
            (Notice, En) => "Notices",
            (Attachment, Ja) => "添付",
            (Attachment, En) => "Attachments",
            (Other, Ja) => "その他",
            (Other, En) => "Other",
        };
        parts.push(format!("{name} {}", digits(count)));
    }
    parts.join(" / ")
}

/// A count with ASCII thousands separators.
fn digits(count: u64) -> String {
    let text = count.to_string();
    let mut out = String::with_capacity(text.len() + text.len() / 3);
    for (index, ch) in text.chars().enumerate() {
        if index > 0 && (text.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn state_label_for(value: &str, language: crate::config::UiLanguage) -> &str {
    use crate::config::UiLanguage::Ja;
    if language == Ja {
        return state_label(value);
    }
    match value {
        "accepted" => "Accepted",
        "held" => "Held",
        "applied" => "Applied",
        "completed" => "Completed",
        "running" => "Running",
        "starting" => "Starting",
        "preparing" => "Preparing",
        "inflight" => "In flight",
        "waiting" => "Waiting",
        "recovery_wait" => "Waiting for recovery",
        "pending" => "Pending",
        "live" => "Active",
        "stopping" => "Stopping",
        "stopped" => "Stopped",
        "failed" => "Failed",
        "incomplete" => "Incomplete",
        "cancelled" => "Cancelled",
        "not_started" => "Not started",
        "interrupted" => "Interrupted; outcome uncertain",
        "exited" => "Exited",
        "idle" => "Idle",
        "paused" => "Paused",
        other => other,
    }
}

pub(crate) fn state_label(value: &str) -> &str {
    match value {
        "accepted" => "受付済み",
        "held" => "保留中",
        "applied" => "反映済み",
        "completed" => "完了",
        "running" => "実行中",
        "starting" => "起動中",
        "preparing" => "準備中",
        "inflight" => "要求中",
        "waiting" => "待機中",
        "recovery_wait" => "復旧待ち",
        "pending" => "順番待ち",
        "live" => "継続中",
        "stopping" => "停止処理中",
        "stopped" => "停止済み",
        "failed" => "失敗",
        "incomplete" => "未完了",
        "cancelled" => "取消済み",
        "not_started" => "未開始",
        "interrupted" => "中断・結果不確か",
        "exited" => "終了",
        "idle" => "待機中",
        "paused" => "一時停止",
        other => other,
    }
}
pub(crate) fn local_time(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S %:z")
                .to_string()
        })
}
/// Called only on the bounded presentation worker, never during frame drawing.
pub(crate) fn decorate(fields: &mut Vec<super::PresentationField>, root: &Value) {
    fields.retain(|field| {
        let key = field.key.rsplit('/').next().unwrap_or(&field.key);
        !(matches!(
            key,
            "reason" | "hint" | "error" | "failure_reason" | "stop_reason"
        ) && field.value == "null")
    });
    for field in fields {
        field.label = human_label(&field.key, root);
        let key = field.key.rsplit('/').next().unwrap_or(&field.key);
        field.display = if key.ends_with("_at") {
            local_time(&field.value)
        } else if matches!(
            key,
            "state" | "status" | "previous_state" | "startup_state" | "planned_state"
        ) {
            let label = state_label(&field.value);
            (label != field.value).then(|| label.to_owned())
        } else if key == "exit_code" && field.value == "null" {
            Some("未確定".into())
        } else {
            None
        };
    }
}

/// Safe bounded field tree. Dot/index paths identify fields, not invented body offsets.
pub fn fields_of_value(value: &Value) -> Vec<PresentationField> {
    fn walk(
        value: &Value,
        path: &str,
        out: &mut Vec<PresentationField>,
        used: &mut usize,
        depth: usize,
    ) {
        if out.len() >= MAX_FIELDS || *used >= MAX_METADATA_BYTES {
            return;
        }
        if depth > 8 {
            out.push(PresentationField {
                value_omitted: true,
                display: None,
                key: path.into(),
                label: short(path, 256),
                value: crate::ui::text_width::glyphs::select(
                    "[深い構造 · 詳細は原文参照]",
                    "[深い構造 | 詳細は原文参照]",
                )
                .into(),
            });
            return;
        }
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if secret(key) {
                        continue;
                    }
                    let path = if path.is_empty() {
                        format!("/{}", crate::json_pointer::escape_segment(key))
                    } else {
                        format!("{path}/{}", crate::json_pointer::escape_segment(key))
                    };
                    walk(value, &path, out, used, depth + 1);
                }
            }
            Value::Array(items) => {
                out.push(PresentationField {
                    value_omitted: false,
                    display: None,
                    key: path.to_owned(),
                    label: format!("{path} 件数"),
                    value: items.len().to_string(),
                });
                for (i, value) in items.iter().take(MAX_FIELDS).enumerate() {
                    walk(value, &format!("{path}/{i}"), out, used, depth + 1);
                }
            }
            _ => {
                if let Value::String(s) = value
                    && let Some(parsed) = super::parse_container_detail(s)
                {
                    walk(&parsed, path, out, used, depth + 1);
                    return;
                }
                let text = match value {
                    Value::String(s) => sanitize_plain(&short(s, 1024), 0).0,
                    _ => value.to_string(),
                };
                let label = short(path, 256);
                let key = path.to_owned();
                *used += key.len() + label.len() + text.len() + size_of::<PresentationField>();
                if *used <= MAX_METADATA_BYTES {
                    out.push(PresentationField {
                        value_omitted: value
                            .as_str()
                            .is_some_and(|text| text.chars().count() > 1024),
                        display: None,
                        key,
                        label,
                        value: text,
                    });
                }
            }
        }
    }
    let mut fields = vec![];
    walk(value, "", &mut fields, &mut 0, 0);
    fields
}

/// Reserve common metadata roots or exact pointers before a large list consumes the bounded field
/// window. Reuses the existing collector without cloning/reordering source JSON.
pub(crate) fn prioritized_fields(value: &Value, roots: &[&str]) -> Vec<PresentationField> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut used = 0usize;
    for (prefix, source) in roots
        .iter()
        .filter_map(|key| {
            if key.starts_with('/') {
                value.pointer(key).map(|part| ((*key).to_owned(), part))
            } else {
                value.get(*key).map(|part| {
                    (
                        format!("/{}", crate::json_pointer::escape_segment(key)),
                        part,
                    )
                })
            }
        })
        .chain(std::iter::once((String::new(), value)))
    {
        for mut field in fields_of_value(source) {
            if !prefix.is_empty() {
                let count_label = field.label.ends_with(" 件数");
                field.key = format!("{prefix}{}", field.key);
                field.label = if count_label {
                    format!("{} 件数", short(&field.key, 256))
                } else {
                    short(&field.key, 256)
                };
            }
            if seen.contains(&field.key) {
                continue;
            }
            let bytes = field
                .key
                .len()
                .saturating_add(field.label.len())
                .saturating_add(field.value.len())
                .saturating_add(size_of::<PresentationField>());
            if out.len() >= MAX_FIELDS || used.saturating_add(bytes) > MAX_METADATA_BYTES {
                return out;
            }
            used += bytes;
            seen.insert(field.key.clone());
            out.push(field);
        }
    }
    out
}

/// Translate only the owned reason code; raw saved values and other reasons
/// remain available unchanged for copying and source inspection.
pub(crate) fn index_recovery_value(
    key: &str,
    value: &str,
    language: crate::config::UiLanguage,
) -> Option<String> {
    if key.rsplit('/').next() != Some("reason") {
        return None;
    }
    let code = "index_manual_rebuild_required";
    let parts = value.split("; ").collect::<Vec<_>>();
    if !parts.contains(&code) {
        return None;
    }
    let label = match language {
        crate::config::UiLanguage::Ja => {
            "古い索引データを復旧できませんでした。このプロファイルの手動再構築が必要です"
        }
        crate::config::UiLanguage::En => {
            "Legacy index work could not be recovered; manual rebuild required for this profile"
        }
    };
    Some(
        parts
            .into_iter()
            .map(|part| if part == code { label } else { part })
            .collect::<Vec<_>>()
            .join("; "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_labels_follow_language_without_changing_group_identity() {
        use crate::config::UiLanguage::{En, Ja};
        for (pointer, japanese, english) in [
            ("/state", "要点", "Key facts"),
            ("/command", "実行", "Execution"),
            ("/stdout", "出力", "Output"),
            ("/node_id", "参照", "References"),
            ("/unknown", "その他", "Other"),
        ] {
            let original = group(pointer);
            assert_eq!(group_label(pointer, Ja), original);
            assert_eq!(original.1, japanese);
            assert_eq!(group_label(pointer, En), (original.0, english));
            assert!(english.is_ascii());
        }
    }
    #[test]
    fn grouped_usage_sections_are_distinct_and_localized() {
        use crate::config::UiLanguage::{En, Ja};
        for (pointer, japanese, english) in [
            (
                "/items/0/model_attempts/completed/count",
                "通常のモデル要求",
                "Model Attempts",
            ),
            (
                "/items/0/compaction/input_tokens/known_sum",
                "要約使用量",
                "Summary Usage",
            ),
            (
                "/items/0/total/input_tokens/known_sum",
                "合計使用量",
                "Combined Total",
            ),
        ] {
            assert_eq!(group_label(pointer, Ja).1, japanese);
            assert_eq!(group_label(pointer, En).1, english);
        }
        let raw = serde_json::json!({"items":[{"compaction":{"map_requests":2}}]});
        assert_eq!(
            human_label_for("/items/0/compaction/map_requests", &raw, Ja),
            "項目 1 › 要約使用量 › map要求数"
        );
        assert_eq!(
            human_label_for("/items/0/compaction/map_requests", &raw, En),
            "Item 1 › Summary Usage › Map Requests"
        );
    }
    #[test]
    fn generated_field_labels_follow_language_without_changing_pointers_or_values() {
        use crate::config::UiLanguage::{En, Ja};
        let raw = serde_json::json!({"thread_id":"thread-full-id","created_at":"opaque-time","items":[{"run_id":"run-full-id"}]});
        assert_eq!(human_label_for("/thread_id", &raw, Ja), "会話ID");
        assert_eq!(human_label_for("/thread_id", &raw, En), "Thread ID");
        assert_eq!(
            human_label_for("/items/0/run_id", &raw, En),
            "Item 1 › Run ID"
        );
        assert_eq!(state_label_for("running", En), "Running");
        assert_eq!(state_label_for("running", Ja), "実行中");
        assert_eq!(display_label("/items", "項目（件数）", En), "Items (count)");
        assert_eq!(
            display_label("/answers/0/text", "本文", En),
            "Answer 1 › Text"
        );
        assert_eq!(
            display_label(
                "/ui_model_attempt/input_bytes",
                "入力のバイト数（UTF-8）",
                En
            ),
            "Input bytes (UTF-8)"
        );
        assert_eq!(
            display_label(
                "/ui_model_attempt/input_tokens",
                "入力トークン（提供元の報告）",
                En
            ),
            "Input tokens (provider-reported)"
        );
        assert_eq!(display_value("/state", "running", "実行中", En), "Running");
        assert_eq!(
            display_value(
                "/ui_raw_stream_preview",
                "",
                "保持済みの表示用コピーを継続表示。今回のraw再取得はIoFailed。現在のraw可用性は未確認。",
                En,
            ),
            "Showing the retained display copy. Raw refresh failed (IoFailed); current raw availability is unknown."
        );
        assert_eq!(
            display_value("/exit_code", "null", "未確定", En),
            "Undetermined"
        );
        assert_eq!(
            display_value("/deep", "", "[深い構造 · 詳細は原文参照]", En),
            "[Deep structure; see original for details]"
        );
        for state in [
            "accepted",
            "held",
            "applied",
            "completed",
            "running",
            "starting",
            "preparing",
            "inflight",
            "waiting",
            "recovery_wait",
            "pending",
            "live",
            "stopping",
            "stopped",
            "failed",
            "incomplete",
            "cancelled",
            "not_started",
            "interrupted",
            "exited",
            "idle",
            "paused",
        ] {
            assert!(state_label_for(state, En).is_ascii(), "{state}");
            assert_eq!(state_label_for(state, Ja), state_label(state));
        }
        assert_eq!(display_label("/thread_id", "会話ID", En), "Thread ID");
        assert_eq!(display_label("/thread_id", "会話ID", Ja), "会話ID");
        let mut fields = fields_of_value(&raw);
        decorate(&mut fields, &raw);
        let thread = fields
            .iter()
            .find(|field| field.key == "/thread_id")
            .unwrap();
        assert_eq!(thread.key, "/thread_id");
        assert_eq!(thread.value, "thread-full-id");
        assert_eq!(display_label(&thread.key, &thread.label, En), "Thread ID");
        assert_eq!(display_label(&thread.key, &thread.label, Ja), "会話ID");
    }

    #[test]
    fn human_terms_leave_canonical_identifiers_and_states_unchanged() {
        let raw = serde_json::json!({"thread_id":"thread-full-id", "run_id":"run-full-id", "turn_id":"turn-full-id", "state":"running"});
        let mut fields = fields_of_value(&raw);
        decorate(&mut fields, &raw);
        for (key, label, value) in [
            ("/thread_id", "会話ID", "thread-full-id"),
            ("/run_id", "作業ID", "run-full-id"),
            ("/turn_id", "応答処理ID", "turn-full-id"),
        ] {
            let field = fields.iter().find(|field| field.key == key).unwrap();
            assert_eq!(field.label, label);
            assert_eq!(field.value, value);
        }
        let state = fields.iter().find(|field| field.key == "/state").unwrap();
        assert_eq!(state.value, "running");
        assert_eq!(state.displayed(), "実行中");
    }
    #[test]
    fn local_display_preserves_exact_values_and_only_hides_meaningless_nulls() {
        let value = serde_json::json!({"execution_id":"full-execution-id","state":"running","started_at":"2026-10-25T00:00:00Z","reason":null,"exit_code":null,"future":"opaque-value"});
        let mut fields = crate::presentation::fields_of_value(&value);
        decorate(&mut fields, &value);
        assert!(!fields.iter().any(|f| f.key == "/reason"));
        let state = fields.iter().find(|f| f.key == "/state").unwrap();
        assert_eq!(state.value, "running");
        assert_eq!(state.displayed(), "実行中");
        let time = fields.iter().find(|f| f.key == "/started_at").unwrap();
        assert_eq!(time.value, "2026-10-25T00:00:00Z");
        let local =
            chrono::DateTime::parse_from_str(time.displayed(), "%Y-%m-%d %H:%M:%S %:z").unwrap();
        assert_eq!(
            local.timestamp(),
            chrono::DateTime::parse_from_rfc3339(&time.value)
                .unwrap()
                .timestamp()
        );
        assert_eq!(
            fields
                .iter()
                .find(|f| f.key == "/exit_code")
                .unwrap()
                .displayed(),
            "未確定"
        );
        assert_eq!(
            fields
                .iter()
                .find(|f| f.key == "/execution_id")
                .unwrap()
                .value,
            "full-execution-id"
        );
        assert_eq!(
            fields
                .iter()
                .find(|f| f.key == "/future")
                .unwrap()
                .displayed(),
            "opaque-value"
        );
    }
    #[test]
    fn metadata_roots_survive_large_arrays_without_duplicate_pointers_or_larger_budget() {
        let value = serde_json::json!({"attachments":(0..1000).map(|i|serde_json::json!({"attachment_id":format!("item-{i}"),"source_node_id":"source","path":"/synthetic","state":"active"})).collect::<Vec<_>>(),"scope":{"kind":"thread"},"count":{"value":1000,"relation":"lower_bound"},"thread_id":"actual","view_observation":{"observed_at":"saved-time"}});
        let fields =
            prioritized_fields(&value, &["thread_id", "scope", "count", "view_observation"]);
        for key in [
            "/thread_id",
            "/scope/kind",
            "/count/value",
            "/count/relation",
            "/view_observation/observed_at",
        ] {
            assert!(fields.iter().any(|f| f.key == key), "missing {key}");
        }
        assert!(
            fields
                .iter()
                .any(|f| f.key == "/attachments/0/attachment_id")
        );
        let unique = fields
            .iter()
            .map(|f| &f.key)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), fields.len());
        assert!(fields.len() <= MAX_FIELDS);
        assert!(
            fields
                .iter()
                .map(|f| f.key.len()
                    + f.label.len()
                    + f.value.len()
                    + size_of::<PresentationField>())
                .sum::<usize>()
                <= MAX_METADATA_BYTES
        );
        for f in &fields {
            assert!(value.pointer(&f.key).is_some(), "{}", f.key);
        }
    }
}
