//! Historical failure evidence for a saved resume observation, never a live target.
use crate::{
    control::Control,
    presentation::{BlockKind, PresentationField},
    protocol::ApiError,
    ui::UiNode,
};
use rusqlite::OptionalExtension;

// A resume observation can omit turn_id in older data. Resolve its saved Run's
// latest Turn at that observation time, not whichever Turn is currently active.
const RESUME: &str = r"
SELECT (SELECT t.id FROM turn t
        WHERE t.run_id=json_extract(e.payload_ref_json,'$.change.run_id')
          AND t.thread_id=e.target_thread_id
          AND julianday(t.created_at)<=julianday(e.created_at)
          AND (json_extract(e.payload_ref_json,'$.change.turn_id') IS NULL
               OR t.id=json_extract(e.payload_ref_json,'$.change.turn_id'))
        ORDER BY julianday(t.created_at) DESC,t.rowid DESC LIMIT 1),e.created_at
FROM node n JOIN event e ON e.id=json_extract(n.payload_json,'$.event_id')
WHERE n.id=?1 AND json_valid(n.payload_json) AND json_valid(e.payload_ref_json)
  AND e.kind='context_changed'
  AND json_extract(e.payload_ref_json,'$.kind')='recovery_resumed'";
const FAILURES: &str = r"
SELECT count(*) FROM model_attempt
WHERE turn_id=?1 AND julianday(ended_at)<=julianday(?2)
  AND json_valid(error_json) AND json_type(error_json,'$.error')='object'";
const LATEST: &str = r"
SELECT a.id,CASE WHEN length(CAST(a.error_json AS BLOB))<=131072 THEN a.error_json END,
       (SELECT n.id FROM node n WHERE n.turn_id=a.turn_id AND n.kind='incomplete_response'
          AND n.payload_json=a.error_json ORDER BY n.rowid DESC LIMIT 1)
FROM model_attempt a
WHERE a.turn_id=?1 AND julianday(a.ended_at)<=julianday(?2)
  AND json_valid(a.error_json) AND json_type(a.error_json,'$.error')='object'
ORDER BY julianday(a.ended_at) DESC,a.rowid DESC LIMIT 1";
struct Evidence {
    index: usize,
    count: i64,
    attempt: String,
    message: Option<String>,
    source: Option<String>,
}

pub(super) async fn enrich(control: &Control, nodes: &mut [UiNode]) -> Result<(), ApiError> {
    let requests = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.presentation.kind == crate::presentation::PresentationKind::Event)
        .map(|(index, node)| (index, node.node_id.clone()))
        .collect::<Vec<_>>();
    if requests.is_empty() {
        return Ok(());
    }
    let evidence = control
        .store
        .database
        .call(move |db| {
            let mut resume = db.prepare(RESUME)?;
            let mut count = db.prepare(FAILURES)?;
            let mut latest = db.prepare(LATEST)?;
            let mut evidence = Vec::new();
            for (index, node) in requests {
                let saved: Option<(Option<String>, String)> = resume
                    .query_row([node], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                let Some((Some(turn), at)) = saved else {
                    continue;
                };
                let count: i64 = count.query_row(rusqlite::params![turn, at], |r| r.get(0))?;
                if count == 0 {
                    continue;
                }
                let (attempt, body, source): (String, Option<String>, Option<String>) = latest
                    .query_row(rusqlite::params![turn, at], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?;
                let message = body
                    .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                    .and_then(|body| {
                        serde_json::from_value::<crate::model::ModelError>(body["error"].clone())
                            .ok()
                    })
                    .map(|error| error.to_string());
                evidence.push(Evidence {
                    index,
                    count,
                    attempt,
                    message,
                    source,
                });
            }
            Ok(evidence)
        })
        .await
        .map_err(|error| error.into_api_error())?;
    for saved in evidence {
        let presentation = &mut nodes[saved.index].presentation;
        let note = format!("再開前にこのTurnで記録したモデル失敗{}件", saved.count);
        presentation.title = format!("復旧再開 · {note}");
        if let Some(block) = presentation
            .blocks
            .iter_mut()
            .find(|block| block.kind == BlockKind::Event)
        {
            block.text = saved
                .message
                .as_ref()
                .map(|message| {
                    format!("直近の保存理由: {}", {
                        let excerpt = message.chars().take(256).collect::<String>();
                        let suffix = if message.chars().count() > 256 {
                            "… · 完全な理由はF6 詳細"
                        } else {
                            ""
                        };
                        format!(
                            "{}{suffix}",
                            crate::presentation::sanitize_plain(&excerpt, 0).0
                        )
                    })
                })
                .unwrap_or_else(|| "直近の理由は保存記録で確認してください".into());
            block.source_exact = false;
        }
        // These explicitly named projection fields are DB references, not JSON
        // pointers into the immutable event's raw body. Copy preserves full values.
        let mut field = |key: &str, label: &str, value: String| {
            presentation.fields.push(PresentationField {
                value_omitted: false,
                key: format!("/saved_model_failures/{key}"),
                label: label.into(),
                value,
                display: None,
            })
        };
        field(
            "count",
            "再開前の保存済みモデル失敗",
            saved.count.to_string(),
        );
        field("attempt_id", "直近の失敗のAttempt ID", saved.attempt);
        if let Some(message) = saved.message {
            field("error/message", "直近の保存理由", message);
        }
        if let Some(source) = saved.source {
            field("source_node_id", "直近の失敗原文", source);
        }
        presentation.bound_metadata();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::DataOwner, presentation};
    use serde_json::json;

    #[tokio::test]
    async fn saved_resume_history_excludes_later_other_turn_and_current_recovery_targets() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        let envelope = json!({"event_id":"resume","kind":"context_changed","payload":{"kind":"recovery_resumed","change":{"run_id":"r","turn_id":"u"}}});
        let payload = json!({"event_id":"resume","role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {envelope}")}]});
        let saved = payload.to_string();
        let latest = json!({"error":{"kind":"provider","message":"保存済み二回目🦀","status":500}})
            .to_string();
        let latest_source = latest.clone();
        control.store.database.call(move|db|{
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','2026-10-25T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2026-10-25T00:00:00Z'),('other','t','r','recovery_wait','2026-10-25T00:00:10Z');")?;
            for (id,turn,at) in [("first","u","01"),("second","u","02"),("later","u","04"),("unrelated","other","01")] {
                db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES(?1,?2,0,0,'p','m',0,'incomplete','{}',?3,'2026-10-25T00:00:00Z',?4)",rusqlite::params![id,turn,latest,format!("2026-10-25T00:00:{at}Z")])?;
            }
            db.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('resume','context_changed','{}','t',?1,'2026-10-25T00:00:03Z')",[json!({"kind":"recovery_resumed","change":{"run_id":"r","turn_id":"u"}}).to_string()])?;
            db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('saved-resume','event','now',?1,length(?1))",[saved])?;
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('failure-source','u','incomplete_response','now',?1,length(?1))",[latest_source])?;
            Ok(())
        }).await.unwrap();
        let text = crate::history::text_of_payload(&payload);
        let mut nodes = vec![UiNode {
            node_id: "saved-resume".into(),
            role: "shellboy".into(),
            text: text.clone(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: presentation::project("event", &payload, &text, 0),
        }];
        enrich(&control, &mut nodes).await.unwrap();
        assert_eq!(nodes[0].text, text);
        let p = &nodes[0].presentation;
        assert_eq!(
            p.title,
            "復旧再開 · 再開前にこのTurnで記録したモデル失敗2件"
        );
        assert!(p.blocks[0].text.contains("保存済み二回目🦀"));
        assert!(
            p.fields
                .iter()
                .any(|f| f.key.ends_with("attempt_id") && f.value == "second")
        );
        assert!(
            p.fields
                .iter()
                .any(|f| f.key.ends_with("source_node_id") && f.value == "failure-source")
        );
        assert!(
            p.fields.iter().any(
                |f| f.key.ends_with("error/message") && f.value == "provider: 保存済み二回目🦀"
            )
        );
        assert!(
            p.requires_attention(),
            "past failures remain visible outside routine event grouping"
        );
        // Missing saved facts do not fabricate a reason or a recovery count.
        control
            .store
            .database
            .call(|db| {
                db.execute("DELETE FROM model_attempt", [])?;
                Ok(())
            })
            .await
            .unwrap();
        nodes[0].presentation = presentation::project("event", &payload, &text, 0);
        enrich(&control, &mut nodes).await.unwrap();
        assert_eq!(nodes[0].presentation.title, "復旧再開");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
