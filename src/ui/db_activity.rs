use crate::{
    config::UiLanguage,
    presentation::messages::Message,
    storage::{DbWorkerActivity, SLOW_JOB_THRESHOLD},
};
use std::time::Instant;

pub(super) fn status_label(
    activity: &DbWorkerActivity,
    now: Instant,
    language: UiLanguage,
) -> Option<String> {
    if let Some(no_progress) = activity.no_progress.first() {
        let operation = operation_label(no_progress.kind, language);
        return Some(match language {
            UiLanguage::Ja => format!("DB! {operation} 進捗なし"),
            UiLanguage::En => format!("DB! {operation} no progress"),
        });
    }
    let active = activity.active.as_ref()?;
    let elapsed = now.saturating_duration_since(active.started);
    if elapsed <= SLOW_JOB_THRESHOLD {
        return None;
    }
    let elapsed = Message::Elapsed {
        ms: elapsed.as_secs().saturating_mul(1000),
        decimal: false,
    }
    .render(language);
    Some(format!(
        "DB {} {elapsed}",
        operation_label(active.kind, language)
    ))
}

fn operation_label(kind: &str, language: UiLanguage) -> String {
    let operation = kind
        .strip_suffix("::{{closure}}")
        .unwrap_or(kind)
        .rsplit('.')
        .next()
        .unwrap_or(kind);
    let (english, japanese) = match operation {
        "execute" => ("command execution", "コマンド実行"),
        "owner_shutdown" => ("shutdown", "終了処理"),
        "status" => ("status", "状態確認"),
        "model_list" => ("model list", "モデル一覧"),
        "thread_list" => ("thread list", "会話一覧"),
        "thread_get" => ("thread read", "会話読取"),
        "usage_get" => ("usage read", "使用量読取"),
        "compact_list" => ("compact list", "要約一覧"),
        "compact_preview" => ("compact preview", "要約プレビュー"),
        "shell_list" => ("shell list", "shell一覧"),
        "thread_create" => ("thread create", "会話作成"),
        "thread_fork" => ("thread fork", "会話分岐"),
        "thread_delete" => ("thread delete", "会話削除"),
        "thread_delete_preview" => ("thread delete preview", "会話削除確認"),
        "thread_set" => ("thread update", "会話更新"),
        "message_send" => ("message send", "メッセージ送信"),
        "message_cancel" => ("message cancel", "メッセージ取消"),
        "message_get" => ("message read", "メッセージ読取"),
        "operation_get" => ("operation read", "操作記録読取"),
        "run_get" => ("run read", "実行読取"),
        "run_retry" => ("run retry", "実行再試行"),
        "run_stop" => ("run stop", "実行停止"),
        "revisit_set" => ("revisit update", "再訪更新"),
        "revisit_get" => ("revisit read", "再訪読取"),
        "revisit_cancel" => ("revisit cancel", "再訪取消"),
        "turn_get" => ("turn read", "ターン読取"),
        "wait" => ("wait", "待機"),
        "history_list" => ("history list", "履歴一覧"),
        "history_get" => ("history read", "履歴読取"),
        "history_search" => ("history search", "履歴検索"),
        "history_relations" => ("history relations", "履歴関連読取"),
        "shell_get" => ("shell read", "shell読取"),
        "shell_read" => ("shell output read", "shell出力読取"),
        "shell_write" => ("shell write", "shell書込"),
        "shell_eof" => ("shell close input", "shell入力終了"),
        "shell_stop" => ("shell stop", "shell停止"),
        "attachment_add" => ("attachment add", "添付追加"),
        "attachment_list" => ("attachment list", "添付一覧"),
        "attachment_remove" => ("attachment remove", "添付削除"),
        "compact" => ("compact", "要約"),
        "context_edit" => ("context edit", "文脈編集"),
        "index_status" => ("index status", "索引状態"),
        "index_preview" => ("index preview", "索引プレビュー"),
        "index_backfill" => ("index backfill", "索引補完"),
        "index_retry" => ("index retry", "索引再試行"),
        "index_rebuild" => ("index rebuild", "索引再構築"),
        "index_probe" => ("index probe", "索引確認"),
        "storage_status" => ("storage status", "保存領域状態"),
        "storage_collect" => ("storage collect", "保存領域回収"),
        "collect_receipts" => ("receipt cleanup", "受領記録回収"),
        "collect" => ("garbage collection", "GC回収"),
        "raw_batch" => ("raw batch", "本文バッチ"),
        "begin_raw" => ("raw batch start", "本文回収開始"),
        "raw_step" => ("raw batch step", "本文回収"),
        _ => {
            let fallback = operation.replace('_', " ");
            return fallback;
        }
    };
    match language {
        UiLanguage::Ja => japanese.into(),
        UiLanguage::En => english.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ActiveDbJob, DbJobLane, NoProgressJob};
    use std::time::{Duration, SystemTime};

    fn active(started: Instant) -> DbWorkerActivity {
        DbWorkerActivity {
            active: Some(ActiveDbJob {
                id: 1,
                kind: "control.db.execute_kind.message_send",
                lane: DbJobLane::Foreground,
                started_at: SystemTime::now(),
                started,
            }),
            no_progress: Vec::new(),
        }
    }

    #[test]
    fn known_db_operations_have_distinct_english_and_japanese_labels() {
        for operation in [
            "execute",
            "owner_shutdown",
            "status",
            "model_list",
            "thread_list",
            "thread_get",
            "usage_get",
            "compact_list",
            "compact_preview",
            "shell_list",
            "thread_create",
            "thread_fork",
            "thread_delete",
            "thread_delete_preview",
            "thread_set",
            "message_send",
            "message_cancel",
            "message_get",
            "operation_get",
            "run_get",
            "run_retry",
            "run_stop",
            "revisit_set",
            "revisit_get",
            "revisit_cancel",
            "turn_get",
            "wait",
            "history_list",
            "history_get",
            "history_search",
            "history_relations",
            "shell_get",
            "shell_read",
            "shell_write",
            "shell_eof",
            "shell_stop",
            "attachment_add",
            "attachment_list",
            "attachment_remove",
            "compact",
            "context_edit",
            "index_status",
            "index_preview",
            "index_backfill",
            "index_retry",
            "index_rebuild",
            "index_probe",
            "storage_status",
            "storage_collect",
            "collect_receipts",
            "collect",
            "raw_batch",
            "begin_raw",
            "raw_step",
        ] {
            let english = operation_label(operation, UiLanguage::En);
            let japanese = operation_label(operation, UiLanguage::Ja);
            assert_ne!(english, japanese, "{operation}");
            assert!(
                !english
                    .chars()
                    .any(|ch| ('\u{3040}'..='\u{30ff}').contains(&ch)),
                "{english}"
            );
        }
    }

    #[test]
    fn unknown_operation_fallback_remains_a_neutral_identifier() {
        assert_eq!(
            operation_label("future_operation", UiLanguage::En),
            "future operation"
        );
        assert_eq!(
            operation_label("future_operation", UiLanguage::Ja),
            "future operation"
        );
    }

    #[test]
    fn active_jobs_are_hidden_until_the_display_threshold_in_both_languages() {
        let started = Instant::now();
        let activity = active(started);
        assert_eq!(
            status_label(
                &activity,
                started + Duration::from_millis(999),
                UiLanguage::En
            ),
            None
        );
        assert_eq!(
            status_label(&activity, started + Duration::from_secs(1), UiLanguage::En),
            None
        );
        assert_eq!(
            status_label(
                &activity,
                started + Duration::from_millis(1_001),
                UiLanguage::En
            )
            .as_deref(),
            Some("DB message send 1s")
        );
        assert_eq!(
            status_label(
                &activity,
                started + Duration::from_millis(1_001),
                UiLanguage::Ja
            )
            .as_deref(),
            Some("DB メッセージ送信 1秒")
        );

        let mut generic_operation = active(started);
        generic_operation.active.as_mut().unwrap().kind = "control.db.Store.execute";
        assert_eq!(
            status_label(
                &generic_operation,
                started + Duration::from_millis(1_001),
                UiLanguage::En
            )
            .as_deref(),
            Some("DB command execution 1s")
        );
        assert_eq!(
            status_label(
                &generic_operation,
                started + Duration::from_millis(1_001),
                UiLanguage::Ja
            )
            .as_deref(),
            Some("DB コマンド実行 1秒")
        );
    }

    #[test]
    fn no_progress_takes_priority_and_is_localized() {
        let mut activity = active(Instant::now());
        activity.no_progress.push(NoProgressJob {
            kind: "gc.GcService.collect",
            deferrals: 2,
        });
        assert_eq!(
            status_label(&activity, Instant::now(), UiLanguage::En).as_deref(),
            Some("DB! garbage collection no progress")
        );
        assert_eq!(
            status_label(&activity, Instant::now(), UiLanguage::Ja).as_deref(),
            Some("DB! GC回収 進捗なし")
        );
    }
}
