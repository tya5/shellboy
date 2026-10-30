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
        "activity_at" => "最終の状態変化",
        "accepted_at" => "受付日時",
        "previous_state" => "変更前の状態",
        "planned_state" => "予定した状態",
        "startup_state" => "再起動後の状態",
        "call_id" => "呼出しID",
        "group_id" => "呼出し群ID",
        "summary_format" => "要約形式",
        "summary_part_keys" => "要約part参照",
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
pub(crate) fn human_label(pointer: &str, root: &Value) -> String {
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
            labels.push(format!("{} {}", item_name(value, collection), position + 1));
            continue;
        }
        value = value.get(key).unwrap_or(&Value::Null);
        collection = key;
        if !matches!(value, Value::Array(_)) || index + 1 == segments.len() {
            labels.push(field_name(key).to_owned());
        }
    }
    let mut label = labels.join(" › ");
    if value.is_array() {
        label.push_str("（件数）");
    }
    label
}

/// Stable semantic sections; presentation labels are never parsed as machine keys.
pub(crate) fn group(pointer: &str) -> (u8, &'static str) {
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
                value: "[深い構造 · 詳細は原文参照]".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
