//! Read-only display of saved failed responses; never a model replay source.
use super::*;
use crate::presentation::{EventClass, PresentationKind};
use rusqlite::OptionalExtension;

pub(super) const ROLE: &str = "received_response";
pub(super) const TITLE: &str = "未確定の受信本文 · モデル再送対象外";
const SAVED_BYTES: usize = 1024 * 1024;

pub(super) fn source(
    db: &rusqlite::Connection,
    attempt: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    db.query_row(r#"SELECT n.id,n.kind FROM model_attempt a JOIN node n ON n.turn_id=a.turn_id
      WHERE a.id=?1 AND n.kind IN ('incomplete_response','model_stream_observation') AND n.deleted_reason IS NULL
      AND (json_extract(n.payload_json,'$.attempt_id')=a.id OR
           (json_type(n.payload_json,'$.attempt_id') IS NULL AND n.payload_json=a.error_json))
      ORDER BY CASE WHEN json_extract(n.payload_json,'$.attempt_id')=a.id THEN 0 ELSE 1 END,n.rowid DESC LIMIT 1"#,
      [attempt],|row|Ok((row.get(0)?,row.get(1)?))).optional()
}

/// Prefer received answer deltas. Alternatives are mutually exclusive so the
/// final event and item-done copies never repeat the same paid response.
pub(super) fn body(payload: &Value) -> (String, Vec<(usize, usize)>) {
    fn fallback(payload: &Value, reasoning: bool) -> String {
        let wanted = if reasoning { "reasoning" } else { "message" };
        let messages = |items: Vec<&Value>| {
            items
                .into_iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some(wanted))
                .map(|item| display_payload(item).0)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let final_text = || {
            payload
                .pointer("/raw_response/output")
                .and_then(Value::as_array)
                .map(|items| messages(items.iter().collect()))
                .unwrap_or_default()
        };
        let done_text = || {
            let mut items = payload
                .get("completed_items")
                .and_then(Value::as_object)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|(index, item)| {
                            index.parse::<u64>().ok().map(|index| (index, item))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            items.sort_by_key(|(index, _)| *index);
            messages(items.into_iter().map(|(_, item)| item).collect())
        };
        let observation =
            payload.get("classification").and_then(Value::as_str) == Some("stream_difference");
        let first = if observation {
            done_text()
        } else {
            final_text()
        };
        if !first.is_empty() {
            first
        } else if observation {
            final_text()
        } else {
            done_text()
        }
    }

    let channel = |key: &str, reasoning| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| fallback(payload, reasoning))
    };
    let reasoning = channel("partial_reasoning", true);
    let answer = channel("partial_text", false);
    let count = reasoning.chars().count();
    let ranges = if count > 0 { vec![(0, count)] } else { vec![] };
    let text = if reasoning.is_empty() {
        answer
    } else if answer.is_empty() {
        reasoning
    } else {
        format!("{reasoning}\n{answer}")
    };
    (text, ranges)
}

pub(super) fn project(kind: &str, mut node: UiNode, width: u16, omitted: bool) -> UiNode {
    let mut p = crate::presentation::project_stream_window(
        &node.text,
        node.base_char_offset,
        &node.reasoning_ranges,
        width,
    );
    let observation = kind == "model_stream_observation";
    p.kind = if observation {
        PresentationKind::Event
    } else {
        PresentationKind::Interrupted
    };
    p.event_class = if observation {
        EventClass::Change
    } else {
        EventClass::Attention
    };
    p.title = format!(
        "{} · /received 一覧",
        if observation {
            "ストリーム差分 · 最終応答は確定済み"
        } else {
            TITLE
        }
    );
    p.provisional_stream = false;
    for block in &mut p.blocks {
        block.source_exact = false;
    }
    p.body_extent_unknown = omitted;
    p.fields.push(crate::presentation::PresentationField {
        key: "/source_node_id".into(),
        label: "保存原文".into(),
        value: node.node_id.clone(),
        ..Default::default()
    });
    node.role = ROLE.into();
    node.presentation = p;
    streams::seal_node(&mut node);
    node
}

fn preview_visible(kind: &str, payload: &Value, text: &str) -> bool {
    if kind != "model_stream_observation" {
        return true;
    }
    if payload
        .get("delta_differences")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        return !text.is_empty();
    }
    let only_metadata = payload
        .get("differences")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.get("field").and_then(Value::as_str) == Some("metadata"))
        });
    !only_metadata && !text.is_empty()
}

struct PreviewRow {
    id: String,
    kind: String,
    partial: String,
    reasoning: String,
    total: i64,
    saved: Option<String>,
    differences: Option<String>,
    delta_differences: Option<String>,
}
pub(super) async fn refresh(
    control: &Control,
    updates: &SyncSender<UiUpdate>,
    thread: &str,
    generation: u64,
    width: u16,
) -> Result<(), ApiError> {
    let target = thread.to_owned();
    let rows=control.store.database.call(move|db|{
        let mut statement=db.prepare(r#"SELECT n.id,n.kind,
          coalesce(substr(json_extract(n.payload_json,'$.partial_text'),1,16384),''),
          coalesce(substr(json_extract(n.payload_json,'$.partial_reasoning'),1,16384),''),
          coalesce(length(json_extract(n.payload_json,'$.partial_text')),0)+coalesce(length(json_extract(n.payload_json,'$.partial_reasoning')),0),
          CASE WHEN length(CAST(n.payload_json AS BLOB))<=?2 THEN n.payload_json END,
          substr(json_extract(n.payload_json,'$.differences'),1,4096),
          substr(json_extract(n.payload_json,'$.delta_differences'),1,4096)
          FROM node n JOIN turn u ON u.id=n.turn_id JOIN thread t ON t.id=u.thread_id
          WHERE u.thread_id=?1 AND t.deleted_at IS NULL
            AND n.kind IN ('incomplete_response','model_stream_observation')
            AND n.deleted_reason IS NULL AND json_valid(n.payload_json)
          ORDER BY n.rowid DESC LIMIT 2"#)?;
        let rows=statement.query_map(rusqlite::params![target,SAVED_BYTES as i64],|r|Ok(PreviewRow{
            id:r.get(0)?,kind:r.get(1)?,partial:r.get(2)?,reasoning:r.get(3)?,total:r.get(4)?,saved:r.get(5)?,differences:r.get(6)?,delta_differences:r.get(7)?
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).await.map_err(storage_error)?;
    let nodes=presentation_work(move||rows.into_iter().rev().filter_map(|row|{
        let full_saved=row.saved.is_some();
        let payload=row.saved.and_then(|s|serde_json::from_str::<Value>(&s).ok()).unwrap_or_else(||serde_json::json!({"classification":if row.kind=="model_stream_observation"{"stream_difference"}else{"incomplete_response"},"partial_text":row.partial,"partial_reasoning":row.reasoning,"differences":row.differences.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"delta_differences":row.delta_differences.and_then(|s|serde_json::from_str::<Value>(&s).ok())}));
        let (mut text,mut reasoning)=body(&payload);
        if !preview_visible(&row.kind,&payload,&text){return None;}
        let known=text.chars().count();
        let omitted=!full_saved && row.total>known as i64;
        if !full_saved && text.is_empty(){text="保存原文は表示予算を超えています。F6から原文を確認してください".into();}
        let(text,base,total)=window(&text,None);
        reasoning.retain(|range|range.1>base && range.0<base+text.chars().count());
        let mut node=project(&row.kind,UiNode{node_id:row.id,text,base_char_offset:base,total_chars:total,reasoning_ranges:reasoning,..Default::default()},width,!full_saved);
        if omitted{node.presentation.body_omitted_chars=(row.total as usize).saturating_sub(known);}
        streams::seal_node(&mut node);
        Some(node)
    }).collect()).await?;
    update(
        updates,
        UiUpdate::IncompleteResponses {
            thread_id: thread.into(),
            generation,
            nodes,
        },
    )
    .await;
    Ok(())
}

pub(super) async fn list(
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
    let (total,rows)=control.store.database.call(move|db|{
        let total:i64=db.query_row("SELECT count(*) FROM node n JOIN turn u ON u.id=n.turn_id JOIN thread t ON t.id=u.thread_id WHERE u.thread_id=?1 AND t.deleted_at IS NULL AND n.kind IN ('incomplete_response','model_stream_observation') AND n.deleted_reason IS NULL",[&count_thread],|r|r.get(0))?;
        let mut statement=db.prepare("SELECT n.id,u.id,n.created_at,n.kind,substr(json_extract(n.payload_json,'$.partial_text'),1,80) FROM node n JOIN turn u ON u.id=n.turn_id JOIN thread t ON t.id=u.thread_id WHERE u.thread_id=?1 AND t.deleted_at IS NULL AND n.kind IN ('incomplete_response','model_stream_observation') AND n.deleted_reason IS NULL ORDER BY n.rowid DESC LIMIT 32 OFFSET ?2")?;
        let rows=statement.query_map(rusqlite::params![count_thread,i64::from(page.saturating_sub(1))*32],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((total,rows))
    }).await.map_err(storage_error)?;
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
        for (id, turn, at, kind, excerpt) in rows {
            fields.push(crate::presentation::PresentationField {
                key: format!("/received/source/{id}"),
                label: format!(
                    "{at} · {} · {} · {}",
                    if kind == "model_stream_observation" {
                        "差分・最終確定済み"
                    } else {
                        "未確定"
                    },
                    turn.chars().take(8).collect::<String>(),
                    excerpt
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect::<String>()
                ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn paid_channels_choose_one_source_and_never_show_tool_or_encrypted_metadata() {
        let payload = json!({"partial_reasoning":"考え","partial_text":"回答","raw_response":{"output":[{"type":"message","content":[{"type":"output_text","text":"回答"}]},{"type":"function_call","arguments":"PRIVATE_TOOL"},{"type":"reasoning","encrypted_content":"PRIVATE_OPAQUE","summary":[{"type":"summary_text","text":"考え"}]}]}});
        assert_eq!(body(&payload), ("考え\n回答".into(), vec![(0, 2)]));
        let mut fallback = payload;
        fallback
            .as_object_mut()
            .unwrap()
            .remove("partial_reasoning");
        fallback.as_object_mut().unwrap().remove("partial_text");
        assert_eq!(body(&fallback), ("考え\n回答".into(), vec![(0, 2)]));
        let mut differing = json!({"raw_response":{"output":[{"type":"message","content":[{"type":"output_text","text":"最終"}]}]},"completed_items":{"0":{"type":"message","content":[{"type":"output_text","text":"途中"}]}}});
        assert_eq!(body(&differing).0, "最終");
        differing["classification"] = json!("stream_difference");
        assert_eq!(body(&differing).0, "途中");
        assert!(!preview_visible(
            "model_stream_observation",
            &json!({"differences":[{"field":"metadata"}]}),
            "same final answer"
        ));
        assert!(!preview_visible(
            "model_stream_observation",
            &json!({"differences":[{"field":"arguments"}]}),
            ""
        ));
        assert!(preview_visible(
            "model_stream_observation",
            &json!({"differences":[{"field":"text"}]}),
            "paid differing text"
        ));
        assert!(preview_visible("incomplete_response", &json!({}), ""));
        assert!(preview_visible(
            "model_stream_observation",
            &json!({"differences":[{"field":"metadata"}],"delta_differences":["text"]}),
            "paid delta"
        ));

        assert_eq!(body(&json!({"completed_items":{"10":{"type":"message","content":[{"type":"refusal","refusal":"後"}]},"2":{"type":"message","content":[{"type":"output_text","text":"先"}]}}})).0,"先\n後");
    }
    #[test]
    fn audit_source_binds_attempt_and_turn_with_legacy_fallback() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE model_attempt(id TEXT,turn_id TEXT,error_json TEXT); CREATE TABLE node(id TEXT,turn_id TEXT,kind TEXT,payload_json TEXT,deleted_reason TEXT); INSERT INTO model_attempt VALUES('a','u','{}');INSERT INTO node VALUES('legacy','u','incomplete_response','{}',NULL);").unwrap();
        assert_eq!(source(&db, "a").unwrap().unwrap().0, "legacy");
        db.execute(
            "UPDATE model_attempt SET error_json=?1",
            [r#"{"attempt_id":"other"}"#],
        )
        .unwrap();
        db.execute(
            "INSERT INTO node VALUES('wrong','u','incomplete_response',?1,NULL)",
            [r#"{"attempt_id":"other"}"#],
        )
        .unwrap();
        assert!(source(&db, "a").unwrap().is_none());
        db.execute("UPDATE model_attempt SET error_json=NULL", [])
            .unwrap();
        db.execute(
            "INSERT INTO node VALUES('exact','u','incomplete_response',?1,NULL)",
            [r#"{"attempt_id":"a"}"#],
        )
        .unwrap();
        db.execute(
            "INSERT INTO node VALUES('wrong-turn','elsewhere','incomplete_response',?1,NULL)",
            [r#"{"attempt_id":"a"}"#],
        )
        .unwrap();
        assert_eq!(source(&db, "a").unwrap().unwrap().0, "exact");
    }
    #[tokio::test]
    async fn saved_previews_survive_reopen_and_all_old_sources_are_paged_without_replay() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config.clone()),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        owner.db.call(|db|{
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','fixture','fixture','fixture','fixture','2026-01-01T00:00:00Z'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','2026-01-01T00:00:00Z');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','failed','2026-01-01T00:00:00Z');")?;
            for i in 0..35 {
                let kind=if i==34{"model_stream_observation"}else{"incomplete_response"};
                let body=json!({"partial_text":format!("有料本文{i}"),"partial_reasoning":"理由","attempt_id":format!("a{i}")}).to_string();
                db.execute("INSERT INTO node(id,turn_id,kind,payload_json,payload_bytes,created_at)VALUES(?1,'u',?2,?3,length(?3),'2026-01-01T00:00:00Z')",rusqlite::params![format!("n{i}"),kind,body])?;
            }
            Ok(())
        }).await.unwrap();
        owner.shutdown().await.unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        refresh(&control, &tx, "t", 7, 80).await.unwrap();
        let UiUpdate::IncompleteResponses { nodes, .. } = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(
            nodes.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>(),
            ["n33", "n34"]
        );
        assert_eq!(nodes[0].text, "理由\n有料本文33");
        assert_eq!(nodes[0].reasoning_ranges, [(0, 2)]);
        assert!(nodes[0].presentation.title.contains("未確定"));
        assert!(nodes[1].presentation.title.contains("確定済み"));
        let (title, first) = list(&control, "t".into(), 1).await.unwrap();
        assert!(title.contains("35件"));
        assert_eq!(
            first
                .iter()
                .filter(|f| f.key.starts_with("/received/source/"))
                .count(),
            32
        );
        assert!(first.iter().any(|f| f.key == "/received/page/2"));
        let (_, second) = list(&control, "t".into(), 2).await.unwrap();
        assert!(second.iter().any(|f| f.value == "n0"));
        assert!(list(&control, "t".into(), 0).await.is_err());
        let raw = crate::history::History::new(owner.db.clone())
            .get(crate::protocol::HistoryGetArgs {
                node_id: "n0".into(),
                limit_bytes: 65536,
                cursor: None,
                encoding: "UTF-8".into(),
            })
            .await
            .unwrap();
        assert!(raw.text.contains("有料本文0"));
        assert!(raw.text.contains("attempt_id"));
        let count = owner
            .db
            .call(|db| {
                Ok(db.query_row("SELECT count(*) FROM thread_path", [], |r| {
                    r.get::<_, i64>(0)
                })?)
            })
            .await
            .unwrap();
        assert_eq!(count, 0);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
