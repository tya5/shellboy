//! Historical failure evidence for a saved resume observation, never a live target.
use crate::{
    presentation::{BlockKind, PresentationField},
    ui::UiNode,
};

#[cfg(test)]
use crate::history::window_facts::recovery::*;
#[cfg(test)]
use crate::protocol::ErrorCode;

/// Saved failure reason as shown: a sanitized 256-character excerpt plus the
/// F6 value. The window observation carries only the bounded excerpt.
struct Reason {
    excerpt: String,
    truncated: bool,
    detail: String,
}

struct Saved {
    count: u64,
    attempt: String,
    reason: Option<Reason>,
    source: Option<String>,
}

/// Apply the selected window's recovery facts to the nodes they name.
pub(super) fn apply_window(
    nodes: &mut [UiNode],
    recovery: &[crate::protocol::HistoryWindowRecovery],
    language: crate::config::UiLanguage,
) {
    for saved in recovery {
        let Some(node) = nodes.iter_mut().find(|node| node.node_id == saved.node_id) else {
            continue;
        };
        let reason = saved.reason.as_ref().map(|reason| Reason {
            excerpt: reason.excerpt.clone(),
            truncated: reason.truncated,
            detail: format!(
                "{}: {}{}",
                reason.kind,
                reason.excerpt,
                if reason.truncated { "…" } else { "" }
            ),
        });
        present(
            &mut node.presentation,
            Saved {
                count: saved.failure_count,
                attempt: saved.latest_attempt_id.clone(),
                reason,
                source: saved.source_node_id.clone(),
            },
            language,
        );
    }
}

fn present(
    presentation: &mut crate::presentation::NodePresentation,
    saved: Saved,
    language: crate::config::UiLanguage,
) {
    let note = match language {
        crate::config::UiLanguage::Ja => {
            format!("再開前にこの応答処理で記録したモデル失敗{}件", saved.count)
        }
        crate::config::UiLanguage::En => {
            format!("{} model failures recorded before resume", saved.count)
        }
    };
    presentation.title = match language {
        crate::config::UiLanguage::Ja => format!("復旧再開 · {note}"),
        crate::config::UiLanguage::En => format!("Resume · {note}"),
    };
    if let Some(block) = presentation
        .blocks
        .iter_mut()
        .find(|block| block.kind == BlockKind::Event)
    {
        block.text = saved
            .reason
            .as_ref()
            .map(|reason| {
                let excerpt = &reason.excerpt;
                // Point only at what F6 can open: its source node holds the
                // full saved reason; without one, the excerpt is all we show.
                let suffix = match (reason.truncated, saved.source.is_some()) {
                    (false, _) => "",
                    (true, false) => "…",
                    (true, true) => match language {
                        crate::config::UiLanguage::Ja => "… · 全文はF6 詳細の直近の失敗原文",
                        crate::config::UiLanguage::En => {
                            "… · Full text: F6 Details, source node for latest failure"
                        }
                    },
                };
                match language {
                    crate::config::UiLanguage::Ja => {
                        format!("直近の保存理由: {excerpt}{suffix}")
                    }
                    crate::config::UiLanguage::En => {
                        format!("Most recent saved reason: {excerpt}{suffix}")
                    }
                }
            })
            .unwrap_or_else(|| match language {
                crate::config::UiLanguage::Ja => "直近の理由は保存記録で確認してください".into(),
                crate::config::UiLanguage::En => {
                    "See the saved record for the most recent reason".into()
                }
            });
        block.source_exact = false;
    }
    // These explicitly named projection fields are DB references, not JSON
    // pointers into the immutable event's raw body. Copy preserves full values.
    let mut field = |key: &str, label: &str, value: String| {
        presentation.fields.push(PresentationField {
            value_omitted: false,
            omission: None,
            key: format!("/saved_model_failures/{key}"),
            label: label.into(),
            value,
            display: None,
        })
    };
    let (count_label, attempt_label, reason_label, source_label) = match language {
        crate::config::UiLanguage::Ja => (
            "再開前の保存済みモデル失敗",
            "直近の失敗のAttempt ID",
            "直近の保存理由",
            "直近の失敗原文",
        ),
        crate::config::UiLanguage::En => (
            "Saved model failures before resume",
            "Latest failed attempt ID",
            "Latest saved reason",
            "Source node for latest failure",
        ),
    };
    field("count", count_label, saved.count.to_string());
    field("attempt_id", attempt_label, saved.attempt);
    if let Some(reason) = saved.reason {
        field("error/message", reason_label, reason.detail);
    }
    if let Some(source) = saved.source {
        field("source_node_id", source_label, source);
    }
    presentation.bound_metadata();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::DataOwner, control::Control, presentation};
    use serde_json::json;

    fn requests(nodes: &[UiNode]) -> Vec<(usize, String)> {
        nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.presentation.kind == crate::presentation::PresentationKind::Event
            })
            .map(|(index, node)| (index, node.node_id.clone()))
            .collect()
    }

    /// Saved evidence as the window observation carries it, then applied.
    fn apply(nodes: &mut [UiNode], evidence: Vec<Evidence>, language: crate::config::UiLanguage) {
        let items = nodes
            .iter()
            .map(|node| (node.node_id.clone(), "event".to_owned()))
            .collect::<Vec<_>>();
        let window = evidence
            .into_iter()
            .map(|fact| fact.into_window(&items))
            .collect::<Vec<_>>();
        apply_window(nodes, &window, language);
    }

    async fn enrich_language(
        control: &Control,
        nodes: &mut [UiNode],
        language: crate::config::UiLanguage,
    ) -> Result<(), crate::protocol::ApiError> {
        crate::frontend::window_facts::apply_saved(control, nodes, language).await
    }

    async fn enrich(
        control: &Control,
        nodes: &mut [UiNode],
    ) -> Result<(), crate::protocol::ApiError> {
        enrich_language(control, nodes, control.store.config.ui.language).await
    }

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
                db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?2),?1,?2,0,0,'p','m',0,'incomplete','{}',?3,'2026-10-25T00:00:00Z',?4)",rusqlite::params![id,turn,latest,format!("2026-10-25T00:00:{at}Z")])?;
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
        let requested = requests(&nodes);
        let evidence = control
            .store
            .database
            .call(move |db| read_tx(db, &requested))
            .await
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].count, 2);
        assert!(
            evidence[0]
                .error_json
                .as_ref()
                .unwrap()
                .contains("保存済み二回目🦀")
        );
        let mut english_nodes = nodes.clone();
        apply(
            &mut english_nodes,
            evidence.clone(),
            crate::config::UiLanguage::En,
        );
        let english = &english_nodes[0].presentation;
        assert_eq!(
            english.title,
            "Resume · 2 model failures recorded before resume"
        );
        assert_eq!(
            english.blocks[0].text, "Most recent saved reason: 保存済み二回目🦀",
            "provider-authored reason remains source text"
        );
        assert!(english.fields.iter().any(|field| {
            field.key.ends_with("attempt_id") && field.label == "Latest failed attempt ID"
        }));
        let mut long_evidence = evidence.clone();
        long_evidence[0].error_json = Some(
            serde_json::json!({"error": crate::model::ModelError::new("provider", "x".repeat(300))})
                .to_string(),
        );
        assert!(long_evidence[0].source.is_some());
        apply(
            &mut english_nodes,
            long_evidence.clone(),
            crate::config::UiLanguage::En,
        );
        assert!(
            english_nodes[0].presentation.blocks[0]
                .text
                .ends_with("… · Full text: F6 Details, source node for latest failure")
        );
        // Without a source node the screen claims nothing beyond the excerpt.
        long_evidence[0].source = None;
        let mut unsourced = nodes.clone();
        apply(&mut unsourced, long_evidence, crate::config::UiLanguage::En);
        let shown = &unsourced[0].presentation.blocks[0].text;
        assert!(shown.ends_with('…') && !shown.contains("F6"), "{shown}");
        apply(&mut nodes, evidence, crate::config::UiLanguage::Ja);
        assert_eq!(nodes[0].text, text);
        let p = &nodes[0].presentation;
        assert_eq!(
            p.title,
            "復旧再開 · 再開前にこの応答処理で記録したモデル失敗2件"
        );
        assert_eq!(p.blocks[0].text, "直近の保存理由: 保存済み二回目🦀");
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
        let original = control
            .store
            .database
            .call(|db| {
                Ok((
                    db.query_row(
                        "SELECT error_json FROM model_attempt WHERE id='second'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?,
                    db.query_row(
                        "SELECT payload_json FROM node WHERE id='failure-source'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?,
                    db.query_row(
                        "SELECT payload_json FROM node WHERE id='saved-resume'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        let expected =
            json!({"error":{"kind":"provider","message":"保存済み二回目🦀","status":500}})
                .to_string();
        assert_eq!(original, (expected.clone(), expected, payload.to_string()));
        // A typed message may itself contain this literal text. Never strip it.
        control.store.database.call(|db| {
            db.execute("UPDATE model_attempt SET error_json=?1 WHERE id='second'", [json!({"error":{"kind":"provider","message":"provider: literal saved reason","status":500}}).to_string()])?;
            Ok(())
        }).await.unwrap();
        nodes[0].presentation = presentation::project("event", &payload, &text, 0);
        enrich(&control, &mut nodes).await.unwrap();
        assert_eq!(
            nodes[0].presentation.blocks[0].text,
            "Most recent saved reason: provider: literal saved reason"
        );
        assert!(
            nodes[0]
                .presentation
                .fields
                .iter()
                .any(|field| field.key.ends_with("error/message")
                    && field.value == "provider: provider: literal saved reason")
        );
        let mut english_nodes = nodes.clone();
        enrich_language(&control, &mut english_nodes, crate::config::UiLanguage::En)
            .await
            .unwrap();
        assert_eq!(
            english_nodes[0].presentation.blocks[0].text,
            "Most recent saved reason: provider: literal saved reason"
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

#[cfg(test)]
#[path = "recovery/batch_tests.rs"]
mod batch_tests;
