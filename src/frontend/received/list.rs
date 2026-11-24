//! Bounded human-facing received-record list projection.
use super::*;

use crate::protocol::{Command, HistoryReceivedArgs, HistoryReceivedPage, HistoryReceivedRow};

pub(super) async fn read(
    data_dir: &std::path::Path,
    language: UiLanguage,
    thread: String,
    cursor: Option<String>,
) -> Result<(String, Vec<crate::presentation::PresentationField>), ApiError> {
    let continued = cursor.is_some();
    let page: HistoryReceivedPage = super::super::inspection_history::native_read(
        data_dir,
        Command::HistoryReceived(HistoryReceivedArgs {
            thread_id: thread.clone(),
            cursor,
        }),
    )
    .await?;
    presentation_work(move || {
        let title = title(
            language,
            page.items.len(),
            continued || page.next_cursor.is_some(),
        );
        let mut fields = Vec::new();
        if continued {
            fields.push(crate::presentation::PresentationField {
                key: "/received/cursor/".into(),
                label: i18n::local!(language, "最初から", "Start over").into(),
                value: thread.clone(),
                ..Default::default()
            });
        }
        for HistoryReceivedRow {
            node_id,
            turn_id,
            at,
            kind,
            state,
            request,
            request_limited,
            excerpt,
            excerpt_limited,
        } in page.items
        {
            let limited_request = request_limited.then(|| {
                i18n::local!(language, "依頼本文は表示上限外", "Request preview omitted").into()
            });
            let limited_excerpt = excerpt_limited.then(|| {
                i18n::local!(language, "応答本文は表示上限外", "Response preview omitted").into()
            });
            fields.push(crate::presentation::PresentationField {
                key: format!("/received/source/{node_id}"),
                label: crate::ui::text_width::glyphs::formatted!(
                    "{} · {} · Turn {}",
                    "{} | {} | Turn {}",
                    crate::presentation::fields::local_time(&at).unwrap_or_else(|| i18n::local!(
                        language,
                        "時刻不明",
                        "Time unknown"
                    )
                    .into()),
                    classification(language, &kind, &state),
                    turn_id.chars().take(8).collect::<String>(),
                ),
                display: Some(display(
                    language,
                    &request.or(limited_request),
                    excerpt.or(limited_excerpt).as_deref(),
                )),
                value: node_id,
                ..Default::default()
            });
        }
        if let Some(token) = page.next_cursor {
            fields.push(crate::presentation::PresentationField {
                key: format!("/received/cursor/{token}"),
                label: i18n::local!(language, "続き", "More").into(),
                value: thread,
                ..Default::default()
            });
        }
        (title, fields)
    })
    .await
}

fn title(language: UiLanguage, count: usize, partial: bool) -> String {
    let separator = crate::ui::text_width::glyphs::separator();
    let plus = if partial { "+" } else { "" };
    match language {
        UiLanguage::Ja => {
            format!("受信記録{separator}{count}{plus}件{separator}Enter 原文 / Esc 閉じる")
        }
        UiLanguage::En => {
            let records = if count == 1 && !partial {
                "record"
            } else {
                "records"
            };
            format!(
                "Received records{separator}{count}{plus} {records}{separator}Enter Original / Esc Close"
            )
        }
    }
}

fn classification(language: UiLanguage, kind: &str, state: &str) -> &'static str {
    if kind == "model_stream_observation" {
        return i18n::local!(language, "差分・最終確定済み", "Delta; finalized");
    }
    match state {
        "recovery_wait" => i18n::local!(language, "復旧待ち", "Awaiting recovery"),
        "completed" => i18n::local!(language, "保存済み・回復後", "Saved; recovered"),
        "stopped" | "failed" => i18n::local!(language, "保存済み・中断", "Saved; interrupted"),
        "preparing" | "running" | "waiting" => {
            i18n::local!(language, "保存済み・処理中", "Saved; in progress")
        }
        _ => i18n::local!(language, "保存済み・状態不明", "Saved; state unknown"),
    }
}

fn display(language: UiLanguage, request: &Option<String>, excerpt: Option<&str>) -> String {
    let single_line = |text: &str| {
        crate::presentation::sanitize_plain(text, 0)
            .0
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let request = request
        .as_deref()
        .map(single_line)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| i18n::local!(language, "依頼本文なし", "No request text").into());
    let excerpt = excerpt.map(single_line).filter(|text| !text.is_empty());
    // The request remains the primary identifier even for paid empty receipts.
    excerpt
        .map_or_else(
            || request.clone(),
            |text| {
                let separator = crate::ui::text_width::glyphs::separator();
                match language {
                    UiLanguage::Ja => format!("{request}{separator}受信: {text}"),
                    UiLanguage::En => format!("{request}{separator}Received: {text}"),
                }
            },
        )
        .chars()
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE thread(id TEXT,title TEXT,deleted_at TEXT);
          CREATE TABLE turn(id TEXT,thread_id TEXT,state TEXT,created_at TEXT DEFAULT 'now');
          CREATE INDEX turns_by_thread ON turn(thread_id,created_at);
          CREATE TABLE node(id TEXT,turn_id TEXT,created_at TEXT,kind TEXT,payload_json TEXT,deleted_reason TEXT);
          CREATE TABLE receipt(turn_id TEXT,target_thread_id TEXT,message_node_id TEXT,created_at TEXT);
          CREATE INDEX node_turn ON node(turn_id,id);
          CREATE INDEX receipt_thread_order ON receipt(target_thread_id);
          INSERT INTO thread VALUES('thread','UNRELATED THREAD TITLE',NULL);
          INSERT INTO turn(id,thread_id,state) VALUES('turn-a','thread','recovery_wait'),('turn-b','thread','completed'),('turn-stop','thread','stopped'),('turn-unknown','thread','unknown');").unwrap();
        db
    }

    fn record(db: &rusqlite::Connection, id: &str, turn: &str, kind: &str, payload: Value) {
        db.execute(
            "INSERT INTO node VALUES(?1,?2,'2026-01-02T03:04:05Z',?3,?4,NULL)",
            rusqlite::params![id, turn, kind, payload.to_string()],
        )
        .unwrap();
    }

    fn request(db: &rusqlite::Connection, id: &str, turn: &str, text: &str, at: &str) {
        record(
            db,
            id,
            turn,
            "user",
            json!({"content":[{"type":"input_text","text":text}]}),
        );
        db.execute(
            "INSERT INTO receipt VALUES(?1,'thread',?2,?3)",
            rusqlite::params![turn, id, at],
        )
        .unwrap();
    }

    #[test]
    fn english_and_japanese_labels_keep_user_text_and_plural_counts() {
        for (total, expected) in [
            (0, "0 records"),
            (1, "1 record"),
            (2, "2 records"),
            (35, "35 records"),
        ] {
            assert!(title(UiLanguage::En, total, false).contains(expected));
            assert!(title(UiLanguage::Ja, total, false).contains(&format!("{total}件")));
        }
        for state in [
            "recovery_wait",
            "completed",
            "stopped",
            "failed",
            "preparing",
            "running",
            "waiting",
            "unknown",
        ] {
            assert_ne!(
                classification(UiLanguage::En, "incomplete_response", state),
                classification(UiLanguage::Ja, "incomplete_response", state)
            );
        }
        assert_eq!(
            classification(UiLanguage::En, "model_stream_observation", "completed"),
            "Delta; finalized"
        );
        assert_eq!(display(UiLanguage::En, &None, None), "No request text");
        let shown = display(UiLanguage::En, &Some("依頼".into()), Some("応答"));
        assert!(shown.contains("依頼") && shown.contains("応答") && shown.contains("Received:"));
    }

    #[test]
    fn requests_bind_same_turn_and_classification_does_not_claim_missing_success() {
        let db = fixture();
        request(&db, "request-a", "turn-a", "first request", "2026-01-01");
        request(
            &db,
            "request-a-later",
            "turn-a",
            "later input",
            "2026-01-02",
        );
        request(&db, "request-b", "turn-b", "second request", "2026-01-03");
        record(
            &db,
            "a",
            "turn-a",
            "incomplete_response",
            json!({"partial_text":"received a"}),
        );
        record(
            &db,
            "b",
            "turn-b",
            "incomplete_response",
            json!({"partial_text":"received b"}),
        );
        record(
            &db,
            "stopped",
            "turn-stop",
            "incomplete_response",
            json!({}),
        );
        record(
            &db,
            "unknown",
            "turn-unknown",
            "incomplete_response",
            json!({}),
        );
        record(
            &db,
            "difference",
            "turn-b",
            "model_stream_observation",
            json!({}),
        );
        let rows = page(&db, None).unwrap().items;
        assert_eq!(rows.len(), 5);
        let a = rows.iter().find(|row| row.node_id == "a").unwrap();
        let b = rows.iter().find(|row| row.node_id == "b").unwrap();
        assert_eq!(a.request.as_deref(), Some("first request"));
        assert_eq!(b.request.as_deref(), Some("second request"));
        assert_eq!(
            classification(UiLanguage::Ja, &a.kind, &a.state),
            "復旧待ち"
        );
        assert_eq!(
            classification(UiLanguage::Ja, &b.kind, &b.state),
            "保存済み・回復後"
        );
        assert!(
            display(UiLanguage::Ja, &a.request, a.excerpt.as_deref()).starts_with("first request")
        );
        let stopped = rows.iter().find(|row| row.node_id == "stopped").unwrap();
        assert_eq!(
            classification(UiLanguage::Ja, &stopped.kind, &stopped.state),
            "保存済み・中断"
        );
        assert_eq!(
            display(UiLanguage::Ja, &stopped.request, stopped.excerpt.as_deref()),
            "依頼本文なし"
        );
        let unknown = rows.iter().find(|row| row.node_id == "unknown").unwrap();
        assert_eq!(
            classification(UiLanguage::Ja, &unknown.kind, &unknown.state),
            "保存済み・状態不明"
        );
        let difference = rows.iter().find(|row| row.node_id == "difference").unwrap();
        assert_eq!(
            classification(UiLanguage::Ja, &difference.kind, &difference.state),
            "差分・最終確定済み"
        );
        assert!(
            difference.excerpt.is_none(),
            "empty records remain reachable"
        );
    }

    #[test]
    fn sql_bounds_projection_and_pages_without_materializing_full_body() {
        let db = fixture();
        let long = "依頼".repeat(50000);
        request(&db, "request", "turn-a", &long, "now");
        for i in 0..35 {
            record(
                &db,
                &format!("record-{i}"),
                "turn-a",
                "incomplete_response",
                json!({"partial_text":long}),
            );
        }
        let first = page(&db, None).unwrap();
        let second = page(&db, first.next_cursor.clone()).unwrap();
        assert_eq!(first.items.len(), 32);
        assert_eq!(second.items.len(), 3);
        assert!(second.next_cursor.is_none());
        let ids = first
            .items
            .iter()
            .chain(&second.items)
            .map(|row| &row.node_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 35);
        assert!(
            first
                .items
                .iter()
                .chain(&second.items)
                .all(|row| row.request_limited
                    && row.excerpt_limited
                    && row.request.is_none()
                    && row.excerpt.is_none())
        );
        db.execute("UPDATE thread SET deleted_at='now'", [])
            .unwrap();
        assert!(page(&db, None).is_err());
    }

    fn page(
        db: &rusqlite::Connection,
        cursor: Option<String>,
    ) -> Result<HistoryReceivedPage, crate::storage::StorageError> {
        crate::history::received::read(
            db,
            &HistoryReceivedArgs {
                thread_id: "thread".into(),
                cursor,
            },
        )
    }

    #[test]
    fn incomplete_receipt_prefix_omits_request_without_losing_the_saved_record() {
        let db = fixture();
        request(
            &db,
            "request",
            "turn-a",
            "plausible but not proven earliest",
            "now",
        );
        for i in 0..129 {
            db.execute(
                "INSERT INTO receipt VALUES(NULL,'thread',?1,'now')",
                [format!("other-{i}")],
            )
            .unwrap();
        }
        record(
            &db,
            "response",
            "turn-a",
            "incomplete_response",
            json!({"partial_text":"saved"}),
        );
        let first = page(&db, None).unwrap();
        assert_eq!(first.items[0].node_id, "response");
        assert_eq!(first.items[0].excerpt.as_deref(), Some("saved"));
        assert!(first.items[0].request_limited);
        assert!(first.items[0].request.is_none());
    }

    #[test]
    fn empty_coverage_has_a_continuation_before_the_first_match() {
        let db = fixture();
        for i in 0..205 {
            record(
                &db,
                &format!("input-{i:03}"),
                "turn-unknown",
                "user",
                json!({}),
            );
        }
        record(
            &db,
            "received",
            "turn-unknown",
            "incomplete_response",
            json!({"partial_text":"saved"}),
        );
        let first = page(&db, None).unwrap();
        assert!(first.items.is_empty());
        assert!(first.next_cursor.is_some());
        let second = page(&db, first.next_cursor).unwrap();
        assert_eq!(second.items[0].node_id, "received");
        assert_eq!(second.items[0].excerpt.as_deref(), Some("saved"));
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn row_text_strips_terminal_controls_and_collapses_lines() {
        let request = Some("\u{1b}[31m依頼\u{1b}[0m\n\t次行\u{7}".into());
        let shown = display(UiLanguage::Ja, &request, Some("\u{1b}[2J回答\r\n続き"));
        assert_eq!(shown, "依頼 次行 | 受信: 回答 続き");
        assert!(!shown.chars().any(char::is_control));
        assert_eq!(
            display(UiLanguage::Ja, &Some(" \n\t".into()), Some("")),
            "依頼本文なし"
        );
    }
}
