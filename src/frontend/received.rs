//! Read-only display of saved failed responses; never a model replay source.
use super::*;
mod list;
use crate::presentation::{EventClass, PresentationKind};
use rusqlite::OptionalExtension;

pub(super) const ROLE: &str = "received_response";
pub(super) const TITLE: &str = "途中で切れた応答（保存済み・会話には含めない）";
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

pub(super) fn project(
    kind: &str,
    mut node: UiNode,
    width: u16,
    omitted: bool,
    recovery_wait: bool,
) -> UiNode {
    let mut p = crate::presentation::project_stream_window(
        &node.text,
        node.base_char_offset,
        &node.reasoning_ranges,
        width,
    );
    let observation = kind == "model_stream_observation";
    p.kind = if observation || !recovery_wait {
        PresentationKind::Event
    } else {
        PresentationKind::Interrupted
    };
    p.event_class = if observation || !recovery_wait {
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
    if text.is_empty() {
        return false;
    }
    if kind != "model_stream_observation" {
        return true;
    }
    // Mismatch classifications describe JSON structure as well as prose. A
    // metadata/content/call-id difference must not duplicate a paid answer.
    let Some(final_response) = payload.get("raw_response") else {
        return false;
    };
    if !final_response.get("output").is_some_and(Value::is_array) {
        return false;
    }
    let final_body = body(&serde_json::json!({"raw_response": final_response}));
    body(payload) != final_body
}

struct PreviewRow {
    id: String,
    after: Option<String>,
    kind: String,
    partial: String,
    reasoning: String,
    total: i64,
    saved: Option<String>,
    differences: Option<String>,
    delta_differences: Option<String>,
    recovery_wait: bool,
}
#[derive(Clone, Copy)]
pub(super) struct Scope {
    pub(super) selection_revision: u64,
    pub(super) view_revision: Option<u64>,
}
pub(super) struct RawSnapshot {
    rows: Vec<PreviewRow>,
    selection_revision: u64,
    root_revision: u64,
}

pub(super) struct Prepared {
    pub(super) nodes: Vec<(String, UiNode)>,
    pub(super) selection_revision: u64,
    pub(super) root_revision: u64,
}

/// Read in the caller's transaction; at most two bounded saved responses.
pub(super) fn query_tx(
    db: &rusqlite::Connection,
    thread: &str,
    scope: Scope,
) -> Result<Option<RawSnapshot>, crate::storage::StorageError> {
    let selection =
        crate::history::selection_revision(db, thread, crate::history::ThreadScope::Live)?;
    if selection as u64 != scope.selection_revision {
        return Ok(None);
    }
    let root_revision = db
        .query_row(
            "SELECT value FROM app_meta WHERE key='root_revision'",
            [],
            |row| row.get::<_, String>(0),
        )?
        .parse::<i64>()
        .ok()
        .and_then(|revision| u64::try_from(revision).ok())
        .ok_or_else(|| {
            crate::storage::StorageError::Schema("root revision metadata is malformed".into())
        })?;
    let mut statement=db.prepare(r#"SELECT n.id,n.kind,
          coalesce(substr(json_extract(n.payload_json,'$.partial_text'),1,16384),''),
          coalesce(substr(json_extract(n.payload_json,'$.partial_reasoning'),1,16384),''),
          coalesce(length(json_extract(n.payload_json,'$.partial_text')),0)+coalesce(length(json_extract(n.payload_json,'$.partial_reasoning')),0),
          CASE WHEN length(CAST(n.payload_json AS BLOB))<=?2 THEN n.payload_json END,
          substr(json_extract(n.payload_json,'$.differences'),1,4096),
          substr(json_extract(n.payload_json,'$.delta_differences'),1,4096),
          (SELECT p.node_id FROM thread_path p JOIN node prior ON prior.id=p.node_id
           WHERE p.thread_id=u.thread_id AND prior.turn_id=n.turn_id AND prior.rowid<n.rowid
           ORDER BY p.ordinal DESC LIMIT 1),u.state='recovery_wait'
          FROM node n JOIN turn u ON u.id=n.turn_id JOIN thread t ON t.id=u.thread_id
          WHERE u.thread_id=?1 AND t.deleted_at IS NULL
            AND n.kind IN ('incomplete_response','model_stream_observation')
            AND n.deleted_reason IS NULL AND json_valid(n.payload_json)
          ORDER BY n.rowid DESC LIMIT 2"#)?;
    let rows = statement
        .query_map(rusqlite::params![thread, SAVED_BYTES as i64], |r| {
            Ok(PreviewRow {
                id: r.get(0)?,
                after: r.get(8)?,
                kind: r.get(1)?,
                partial: r.get(2)?,
                reasoning: r.get(3)?,
                total: r.get(4)?,
                saved: r.get(5)?,
                differences: r.get(6)?,
                delta_differences: r.get(7)?,
                recovery_wait: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(RawSnapshot {
        rows,
        selection_revision: selection as u64,
        root_revision,
    }))
}

pub(super) fn prepare(snapshot: RawSnapshot, width: u16) -> Prepared {
    let RawSnapshot {
        rows,
        selection_revision,
        root_revision,
    } = snapshot;
    let nodes = rows.into_iter().rev().filter_map(|row|{
        let after=row.after?;
        let full_saved=row.saved.is_some();
        let payload=row.saved.and_then(|s|serde_json::from_str::<Value>(&s).ok()).unwrap_or_else(||serde_json::json!({"classification":if row.kind=="model_stream_observation"{"stream_difference"}else{"incomplete_response"},"partial_text":row.partial,"partial_reasoning":row.reasoning,"differences":row.differences.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"delta_differences":row.delta_differences.and_then(|s|serde_json::from_str::<Value>(&s).ok())}));
        let (text,mut reasoning)=body(&payload);
        if !preview_visible(&row.kind,&payload,&text){return None;}
        let known=text.chars().count();
        let omitted=!full_saved && row.total>known as i64;
        let(text,base,total)=window(&text,None);
        reasoning.retain(|range|range.1>base && range.0<base+text.chars().count());
        let mut node=project(&row.kind,UiNode{node_id:row.id,text,base_char_offset:base,total_chars:total,reasoning_ranges:reasoning,..Default::default()},width,!full_saved,row.recovery_wait);
        if omitted{node.presentation.body_omitted_chars=(row.total as usize).saturating_sub(known);}
        streams::seal_node(&mut node);
        Some((after,node))
    }).collect();
    Prepared {
        nodes,
        selection_revision,
        root_revision,
    }
}

pub(super) async fn apply(
    updates: &dyn UpdateTarget,
    thread: &str,
    generation: u64,
    scope: Scope,
    prepared: Prepared,
) {
    update(
        updates,
        UiUpdate::IncompleteResponses {
            thread_id: thread.into(),
            generation,
            selection_revision: prepared.selection_revision,
            view_revision: scope.view_revision,
            root_revision: prepared.root_revision,
            nodes: prepared.nodes,
        },
    )
    .await;
}

pub(super) async fn refresh(
    control: &Control,
    updates: &dyn UpdateTarget,
    thread: &str,
    generation: u64,
    width: u16,
    scope: Scope,
) -> Result<(), ApiError> {
    let target = thread.to_owned();
    let snapshot = control
        .store
        .database
        .call(move |db| query_tx(db, &target, scope))
        .await
        .map_err(storage_error)?;
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    let prepared = presentation_work(move || prepare(snapshot, width)).await?;
    apply(updates, thread, generation, scope, prepared).await;
    Ok(())
}

pub(super) async fn list(
    control: &Control,
    thread: String,
    page: u32,
) -> Result<(String, Vec<crate::presentation::PresentationField>), ApiError> {
    list::read(control, thread, page).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn prepared_saved_response_retains_snapshot_fences_and_paid_body() {
        let prepared = prepare(
            RawSnapshot {
                selection_revision: 7,
                root_revision: 11,
                rows: vec![PreviewRow {
                    id: "saved-response".into(),
                    after: Some("anchor".into()),
                    kind: "incomplete_response".into(),
                    partial: "bounded fallback".into(),
                    reasoning: String::new(),
                    total: 100_000,
                    saved: Some(
                        json!({"partial_text":"保存済み回答🦀","partial_reasoning":"保存済み理由"})
                            .to_string(),
                    ),
                    differences: None,
                    delta_differences: None,
                    recovery_wait: true,
                }],
            },
            80,
        );
        assert_eq!(
            (prepared.selection_revision, prepared.root_revision),
            (7, 11)
        );
        assert_eq!(prepared.nodes.len(), 1);
        let (anchor, node) = &prepared.nodes[0];
        assert_eq!(anchor, "anchor");
        assert_eq!(node.node_id, "saved-response");
        assert_eq!(node.text, "保存済み理由\n保存済み回答🦀");
        assert!(!node.presentation.body_extent_unknown);
        assert!(node.presentation.requires_attention());
    }
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
            &json!({"partial_text":"paid differing text","raw_response":{"output":[]}}),
            "paid differing text"
        ));
        assert!(!preview_visible("incomplete_response", &json!({}), ""));
        assert!(preview_visible(
            "model_stream_observation",
            &json!({"partial_text":"paid delta","raw_response":{"output":[]},"differences":[{"field":"metadata"}],"delta_differences":["text"]}),
            "paid delta"
        ));

        assert_eq!(body(&json!({"completed_items":{"10":{"type":"message","content":[{"type":"refusal","refusal":"後"}]},"2":{"type":"message","content":[{"type":"output_text","text":"先"}]}}})).0,"先\n後");
    }
    #[test]
    fn empty_receipts_and_structural_differences_do_not_invent_or_duplicate_body() {
        let message =
            json!({"type":"message","content":[{"type":"output_text","text":"same answer"}]});
        let mut payload = json!({"partial_text":"same answer","completed_items":{"0":message},"raw_response":{"output":[message]},"differences":[{"field":"content"},{"field":"call_id"},{"field":"arguments"}],"delta_differences":[]});
        assert!(!preview_visible(
            "model_stream_observation",
            &payload,
            &body(&payload).0
        ));
        payload["partial_text"] = json!("different paid answer");
        assert!(preview_visible(
            "model_stream_observation",
            &payload,
            &body(&payload).0
        ));
        payload["partial_text"] = json!("");
        assert!(!preview_visible(
            "model_stream_observation",
            &payload,
            &body(&payload).0
        ));
        let empty = json!({"partial_text":"","partial_reasoning":"","completed_items":{},"classification":"http_500"});
        assert!(body(&empty).0.is_empty());
        assert!(!preview_visible(
            "incomplete_response",
            &empty,
            &body(&empty).0
        ));
        let reason = json!({"partial_reasoning":"paid reasoning","partial_text":"same answer","raw_response":{"output":[message,{"type":"reasoning","summary":[{"type":"summary_text","text":"final reasoning"}]}]}});
        assert!(preview_visible(
            "model_stream_observation",
            &reason,
            &body(&reason).0
        ));
    }
    #[test]
    fn done_only_prose_and_reasoning_are_compared_to_final_channels() {
        let message =
            |text| json!({"type":"message","content":[{"type":"output_text","text":text}]});
        let reasoning =
            |text| json!({"type":"reasoning","summary":[{"type":"summary_text","text":text}]});
        let mut payload = json!({"classification":"stream_difference","partial_text":"","partial_reasoning":"","completed_items":{"0":message("done answer"),"1":reasoning("done reasoning")},"raw_response":{"output":[message("final answer"),reasoning("final reasoning")]}});
        assert_eq!(body(&payload).0, "done reasoning\ndone answer");
        assert!(preview_visible(
            "model_stream_observation",
            &payload,
            &body(&payload).0
        ));
        payload["raw_response"]["output"] =
            json!([message("done answer"), reasoning("final reasoning")]);
        assert!(
            preview_visible("model_stream_observation", &payload, &body(&payload).0),
            "reasoning-only difference remains visible"
        );
        payload["raw_response"]["output"] =
            json!([message("done answer"), reasoning("done reasoning")]);
        assert!(!preview_visible(
            "model_stream_observation",
            &payload,
            &body(&payload).0
        ));
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
            db.execute_batch("INSERT INTO node(id,turn_id,kind,payload_json,payload_bytes,created_at)VALUES('anchor','u','user_message','{}',2,'2026-01-01T00:00:00Z'); INSERT INTO thread_path(thread_id,node_id,ordinal)VALUES('t','anchor',0);")?;
            for i in 0..35 {
                let kind=if i==34{"model_stream_observation"}else{"incomplete_response"};
                let body=json!({"partial_text":format!("有料本文{i}"),"partial_reasoning":"理由","attempt_id":format!("a{i}"),"raw_response":{"output":[]}}).to_string();
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
        refresh(
            &control,
            &tx,
            "t",
            7,
            80,
            Scope {
                selection_revision: 0,
                view_revision: None,
            },
        )
        .await
        .unwrap();
        let UiUpdate::IncompleteResponses { nodes, .. } = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(
            nodes
                .iter()
                .map(|(_, n)| n.node_id.as_str())
                .collect::<Vec<_>>(),
            ["n33", "n34"]
        );
        assert_eq!(nodes[0].1.text, "理由\n有料本文33");
        assert_eq!(nodes[0].1.reasoning_ranges, [(0, 2)]);
        assert!(nodes[0].1.presentation.title.contains("途中で切れた応答"));
        assert!(nodes[1].1.presentation.title.contains("確定済み"));
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
                final_answer_only: false,
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
        assert_eq!(count, 1);
        // A later successful Turn must not move an older paid preview behind
        // the new answer, and an HTTP failure with no body adds no preview.
        owner.db.call(|db| {
            db.execute_batch(r#"INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('later','t','r','completed','2026-01-01T00:01:00Z');
                INSERT INTO node(id,turn_id,kind,payload_json,payload_bytes,created_at)VALUES('answer','later','assistant_response','{}',2,'2026-01-01T00:01:00Z');
                INSERT INTO thread_path(thread_id,node_id,ordinal)VALUES('t','answer',1);
                INSERT INTO node(id,turn_id,kind,payload_json,payload_bytes,created_at)VALUES('empty','u','incomplete_response','{"partial_text":"","partial_reasoning":"","completed_items":{}}',62,'2026-01-01T00:01:01Z');"#)?;
            Ok(())
        }).await.unwrap();
        refresh(
            &control,
            &tx,
            "t",
            7,
            80,
            Scope {
                selection_revision: 0,
                view_revision: None,
            },
        )
        .await
        .unwrap();
        let UiUpdate::IncompleteResponses { nodes, .. } = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].0, "anchor");
        assert_eq!(nodes[0].1.node_id, "n34");
        let (title, latest) = list(&control, "t".into(), 1).await.unwrap();
        assert!(title.contains("36件"));
        let empty = latest.iter().find(|field| field.value == "empty").unwrap();
        assert_eq!(empty.displayed(), "依頼本文なし");
        refresh(
            &control,
            &tx,
            "t",
            7,
            80,
            Scope {
                selection_revision: 99,
                view_revision: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        for malformed in ["-1", "not-a-revision"] {
            owner
                .db
                .call(move |db| {
                    db.execute(
                        "UPDATE app_meta SET value=?1 WHERE key='root_revision'",
                        [malformed],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            assert!(
                refresh(
                    &control,
                    &tx,
                    "t",
                    7,
                    80,
                    Scope {
                        selection_revision: 0,
                        view_revision: None
                    }
                )
                .await
                .is_err()
            );
            assert!(matches!(
                rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
        }
        owner
            .db
            .call(|db| {
                db.execute(
                    "UPDATE app_meta SET value='0' WHERE key='root_revision'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn attention_follows_the_source_turn_and_survives_reopen_without_rewriting_audit() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root.path().join("data");
        let owner = crate::app::DataOwner::start(
            Some(config.clone()),
            Some(data.clone()),
            root.path().into(),
        )
        .await
        .unwrap();
        let payload = json!({"attempt_id":"a","partial_text":"paid answer","partial_reasoning":"paid reasoning","error":{"kind":"transport","message":"fixture"}}).to_string();
        let saved = payload.clone();
        owner.db.call(move |db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','fixture','fixture','fixture','fixture','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','recovery_wait','now'); INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('anchor','u','user_message','now','{}',2); INSERT INTO thread_path(thread_id,node_id,ordinal)VALUES('t','anchor',0); UPDATE thread SET cursor_node_id='anchor' WHERE id='t';")?;
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('audit','u','incomplete_response','now',?1,length(?1))", [&saved])?;
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('a','u',0,0,'fixture','fixture',0,'incomplete','{}',?1,'now')", [&saved])?;
            Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        for (state, attention) in [
            ("recovery_wait", true),
            ("running", false),
            ("recovery_wait", true),
            ("stopped", false),
            ("completed", false),
        ] {
            let state = state.to_owned();
            owner
                .db
                .call(move |db| {
                    db.execute("UPDATE turn SET state=?1 WHERE id='u'", [state])?;
                    Ok(())
                })
                .await
                .unwrap();
            refresh(
                &control,
                &tx,
                "t",
                1,
                80,
                Scope {
                    selection_revision: 0,
                    view_revision: None,
                },
            )
            .await
            .unwrap();
            let UiUpdate::IncompleteResponses { nodes, .. } = rx.try_recv().unwrap() else {
                panic!()
            };
            assert_eq!(nodes.len(), 1);
            let preview = &nodes[0].1;
            assert_eq!(preview.text, "paid reasoning\npaid answer");
            assert_eq!(preview.node_id, "audit");
            assert_eq!(preview.presentation.requires_attention(), attention);
            assert_eq!(
                preview.presentation.kind,
                if attention {
                    PresentationKind::Interrupted
                } else {
                    PresentationKind::Event
                }
            );
            assert!(
                preview
                    .presentation
                    .fields
                    .iter()
                    .any(|field| field.key == "/source_node_id" && field.value == "audit")
            );
            let window = super::super::history_reader::HistoryReader::new(&control, &tx, 80)
                .node("audit".into(), 5, "t")
                .await
                .unwrap();
            assert_eq!(
                window.presentation.requires_attention(),
                attention,
                "F6/body window must use source Turn state too"
            );
        }
        // The observed response is complete; leave no unrelated live Run for
        // shutdown interruption capture to append to this synthetic path.
        owner
            .db
            .call(|db| {
                db.execute("UPDATE run SET state='completed' WHERE id='r'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        let owner = crate::app::DataOwner::start(Some(config), Some(data), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        refresh(
            &control,
            &tx,
            "t",
            2,
            80,
            Scope {
                selection_revision: 0,
                view_revision: None,
            },
        )
        .await
        .unwrap();
        let UiUpdate::IncompleteResponses { nodes, .. } = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(nodes.len(), 1);
        assert!(!nodes[0].1.presentation.requires_attention());
        let original = owner
            .db
            .call(|db| {
                Ok((
                    db.query_row(
                        "SELECT payload_json FROM node WHERE id='audit'",
                        [],
                        |row| row.get::<_, String>(0),
                    )?,
                    db.query_row(
                        "SELECT error_json FROM model_attempt WHERE id='a'",
                        [],
                        |row| row.get::<_, String>(0),
                    )?,
                    db.query_row(
                        "SELECT count(*) FROM thread_path WHERE thread_id='t'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(original, (payload.clone(), payload, 1));
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
