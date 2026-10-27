//! Bounded human-facing received-record list projection.
use super::*;

pub(super) async fn read(
    control: &Control,
    thread: String,
    page: u32,
) -> Result<(String, Vec<crate::presentation::PresentationField>), ApiError> {
    if page == 0 {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Page must be positive",
        ));
    }
    let count_thread = thread.clone();
    let (total, rows) = control
        .store
        .database
        .call(move |db| Ok(query(db, &count_thread, page)?))
        .await
        .map_err(storage_error)?;
    presentation_work(move || {
        let mut fields = Vec::new();
        if page > 1 {
            fields.push(crate::presentation::PresentationField {
                key: format!("/received/page/{}", page - 1),
                label: "前の32件".into(),
                value: thread.clone(),
                ..Default::default()
            });
        }
        for row in rows {
            let Row {
                id,
                turn,
                at,
                kind,
                state,
                request,
                excerpt,
            } = row;
            fields.push(crate::presentation::PresentationField {
                key: format!("/received/source/{id}"),
                label: format!(
                    "{} · {} · Turn {}",
                    crate::presentation::fields::local_time(&at)
                        .unwrap_or_else(|| "時刻不明".into()),
                    classification(&kind, &state),
                    turn.chars().take(8).collect::<String>(),
                ),
                display: Some(display(&request, excerpt.as_deref())),
                value: id,
                ..Default::default()
            });
        }
        if u64::from(page) * 32 < total as u64 {
            fields.push(crate::presentation::PresentationField {
                key: format!("/received/page/{}", page + 1),
                label: "次の32件".into(),
                value: thread,
                ..Default::default()
            });
        }
        (
            format!("受信記録 · {total}件 · {page}ページ · Enter 原文 / Esc 閉じる"),
            fields,
        )
    })
    .await
}

struct Row {
    id: String,
    turn: String,
    at: String,
    kind: String,
    state: String,
    request: Option<String>,
    excerpt: Option<String>,
}

fn query(db: &rusqlite::Connection, thread: &str, page: u32) -> rusqlite::Result<(i64, Vec<Row>)> {
    let total = db.query_row("SELECT count(*) FROM node n JOIN turn u ON u.id=n.turn_id JOIN thread t ON t.id=u.thread_id WHERE u.thread_id=?1 AND t.deleted_at IS NULL AND n.kind IN ('incomplete_response','model_stream_observation') AND n.deleted_reason IS NULL", [thread], |r| r.get(0))?;
    // The receipt assignment, not the Thread title or a nearby message, binds
    // the request to this Turn. Multiple inputs use the first assigned receipt.
    // Both JSON excerpts are bounded in SQLite before reaching Rust.
    let mut statement = db.prepare(r#"SELECT n.id,u.id,n.created_at,n.kind,u.state,
      (SELECT substr(coalesce(
        (SELECT json_extract(c.value,'$.text') FROM json_each(m.payload_json,'$.content') c
         WHERE json_extract(c.value,'$.type') IN ('input_text','text')
           AND json_type(c.value,'$.text')='text' ORDER BY c.key LIMIT 1),
        CASE WHEN json_type(m.payload_json,'$.text')='text' THEN json_extract(m.payload_json,'$.text') END),1,80)
       FROM receipt r JOIN node m ON m.id=r.message_node_id
       WHERE r.turn_id=u.id AND r.target_thread_id=u.thread_id AND m.deleted_reason IS NULL
       ORDER BY r.created_at,r.rowid LIMIT 1),
      CASE WHEN json_type(n.payload_json,'$.partial_text')='text'
        THEN substr(json_extract(n.payload_json,'$.partial_text'),1,80) END
      FROM node n JOIN turn u ON u.id=n.turn_id JOIN thread t ON t.id=u.thread_id
      WHERE u.thread_id=?1 AND t.deleted_at IS NULL
        AND n.kind IN ('incomplete_response','model_stream_observation') AND n.deleted_reason IS NULL
      ORDER BY n.rowid DESC LIMIT 32 OFFSET ?2"#)?;
    let rows = statement
        .query_map(
            rusqlite::params![thread, i64::from(page.saturating_sub(1)) * 32],
            |r| {
                Ok(Row {
                    id: r.get(0)?,
                    turn: r.get(1)?,
                    at: r.get(2)?,
                    kind: r.get(3)?,
                    state: r.get(4)?,
                    request: r.get(5)?,
                    excerpt: r.get(6)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((total, rows))
}

fn classification(kind: &str, state: &str) -> &'static str {
    if kind == "model_stream_observation" {
        return "差分・最終確定済み";
    }
    match state {
        "recovery_wait" => "復旧待ち",
        "completed" => "保存済み・回復後",
        "stopped" | "failed" => "保存済み・中断",
        "preparing" | "running" | "waiting" => "保存済み・処理中",
        _ => "保存済み・状態不明",
    }
}

fn display(request: &Option<String>, excerpt: Option<&str>) -> String {
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
        .unwrap_or_else(|| "依頼本文なし".into());
    let excerpt = excerpt.map(single_line).filter(|text| !text.is_empty());
    // The request remains the primary identifier even for paid empty receipts.
    excerpt
        .map_or_else(
            || request.clone(),
            |text| format!("{request} · 受信: {text}"),
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
          CREATE TABLE turn(id TEXT,thread_id TEXT,state TEXT);
          CREATE TABLE node(id TEXT,turn_id TEXT,created_at TEXT,kind TEXT,payload_json TEXT,deleted_reason TEXT);
          CREATE TABLE receipt(turn_id TEXT,target_thread_id TEXT,message_node_id TEXT,created_at TEXT);
          INSERT INTO thread VALUES('thread','UNRELATED THREAD TITLE',NULL);
          INSERT INTO turn VALUES('turn-a','thread','recovery_wait'),('turn-b','thread','completed'),('turn-stop','thread','stopped'),('turn-unknown','thread','unknown');").unwrap();
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
        let (total, rows) = query(&db, "thread", 1).unwrap();
        assert_eq!(total, 5);
        let a = rows.iter().find(|row| row.id == "a").unwrap();
        let b = rows.iter().find(|row| row.id == "b").unwrap();
        assert_eq!(a.request.as_deref(), Some("first request"));
        assert_eq!(b.request.as_deref(), Some("second request"));
        assert_eq!(classification(&a.kind, &a.state), "復旧待ち");
        assert_eq!(classification(&b.kind, &b.state), "保存済み・回復後");
        assert!(display(&a.request, a.excerpt.as_deref()).starts_with("first request"));
        let stopped = rows.iter().find(|row| row.id == "stopped").unwrap();
        assert_eq!(
            classification(&stopped.kind, &stopped.state),
            "保存済み・中断"
        );
        assert_eq!(
            display(&stopped.request, stopped.excerpt.as_deref()),
            "依頼本文なし"
        );
        let unknown = rows.iter().find(|row| row.id == "unknown").unwrap();
        assert_eq!(
            classification(&unknown.kind, &unknown.state),
            "保存済み・状態不明"
        );
        let difference = rows.iter().find(|row| row.id == "difference").unwrap();
        assert_eq!(
            classification(&difference.kind, &difference.state),
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
        let (total, first) = query(&db, "thread", 1).unwrap();
        let (_, second) = query(&db, "thread", 2).unwrap();
        assert_eq!(total, 35);
        assert_eq!(first.len(), 32);
        assert_eq!(second.len(), 3);
        assert_eq!(first[0].id, "record-34");
        assert_eq!(second[2].id, "record-0");
        for row in first.iter().chain(&second) {
            assert_eq!(row.request.as_ref().unwrap().chars().count(), 80);
            assert_eq!(row.excerpt.as_ref().unwrap().chars().count(), 80);
            assert!(
                display(&row.request, row.excerpt.as_deref())
                    .chars()
                    .count()
                    <= 80
            );
        }
        assert!(query(&db, "thread", 3).unwrap().1.is_empty());
        db.execute("UPDATE thread SET deleted_at='now'", [])
            .unwrap();
        assert_eq!(query(&db, "thread", 1).unwrap().0, 0);
    }

    #[test]
    fn row_text_strips_terminal_controls_and_collapses_lines() {
        let request = Some("\u{1b}[31m依頼\u{1b}[0m\n\t次行\u{7}".into());
        let shown = display(&request, Some("\u{1b}[2J回答\r\n続き"));
        assert_eq!(shown, "依頼 次行 · 受信: 回答 続き");
        assert!(!shown.chars().any(char::is_control));
        assert_eq!(display(&Some(" \n\t".into()), Some("")), "依頼本文なし");
    }
}
