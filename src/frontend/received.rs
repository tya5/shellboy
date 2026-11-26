//! Read-only display of saved failed responses; never a model replay source.
use super::*;
mod list;
use crate::{
    config::UiLanguage,
    presentation::{EventClass, PresentationKind},
};

pub(super) const ROLE: &str = "received_response";

fn received_title(language: UiLanguage, observation: bool) -> String {
    match language {
        UiLanguage::Ja => {
            if observation {
                crate::ui::text_width::glyphs::formatted!(
                    "ストリーム差分 · 最終応答は確定済み · /received 一覧",
                    "ストリーム差分 | 最終応答は確定済み | /received 一覧"
                )
            } else {
                crate::ui::text_width::glyphs::formatted!(
                    "途中で切れた応答（保存済み・会話には含めない） · /received 一覧",
                    "途中で切れた応答（保存済み・会話には含めない） | /received 一覧"
                )
            }
        }
        UiLanguage::En => {
            if observation {
                crate::ui::text_width::glyphs::formatted!(
                    "{} · {} · {}",
                    "{} | {} | {}",
                    "Stream difference",
                    "final response confirmed",
                    "/received list"
                )
            } else {
                crate::ui::text_width::glyphs::formatted!(
                    "{} · {}",
                    "{} | {}",
                    "Interrupted response (saved; not part of the conversation)",
                    "/received list"
                )
            }
        }
    }
}

pub(super) fn project(
    kind: &str,
    mut node: UiNode,
    width: u16,
    omitted: bool,
    recovery_wait: bool,
    language: UiLanguage,
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
    p.title = received_title(language, observation);
    p.ui_messages.title = Some(crate::presentation::messages::Message::Text(
        if observation {
            crate::presentation::messages::Text::StreamDifference
        } else {
            crate::presentation::messages::Text::ReceivedResponse
        },
    ));
    p.provisional_stream = false;
    for block in &mut p.blocks {
        block.source_exact = false;
    }
    p.body_extent_unknown = omitted;
    p.fields.push(crate::presentation::PresentationField {
        key: "/source_node_id".into(),
        label: match language {
            UiLanguage::Ja => "保存原文",
            UiLanguage::En => "Saved source",
        }
        .into(),
        value: node.node_id.clone(),
        ..Default::default()
    });
    node.role = ROLE.into();
    node.presentation = p;
    streams::seal_node(&mut node);
    node
}

/// Received responses one snapshot shows, newest first by saved time.
const SNAPSHOT_ROWS: usize = 2;

#[derive(Clone, Copy)]
pub(super) struct Scope {
    pub(super) selection_revision: u64,
    pub(super) view_revision: Option<u64>,
    /// The owner `read_sequence` of the page this snapshot accompanies.
    pub(super) request_sequence: u64,
}

/// The two newest received responses (by saved time) on the first
/// `history.received` page, each after the selected-path node it follows, as
/// positioned owner display extents. A stream difference that matches its
/// final response and an empty body show nothing.
pub(super) async fn snapshot(
    owner: &native_history::Owner<'_>,
    thread: &str,
    width: u16,
    language: UiLanguage,
) -> Result<Vec<(String, UiNode)>, ApiError> {
    use crate::protocol::{
        HistoryDisplayNode, HistoryDisplayPosition, HistoryReceivedArgs, HistoryReceivedPage,
    };
    let page: HistoryReceivedPage = owner
        .call(Command::HistoryReceived(HistoryReceivedArgs {
            thread_id: thread.to_owned(),
            cursor: None,
        }))
        .await?;
    let mut rows = page.items;
    rows.sort_by(|a, b| (&b.at, &b.node_id).cmp(&(&a.at, &a.node_id)));
    rows.truncate(SNAPSHOT_ROWS);
    let mut nodes = Vec::with_capacity(rows.len());
    for row in rows.into_iter().rev() {
        let Some(after) = row.follows_node_id else {
            continue;
        };
        if row.differs_from_final == Some(false) {
            continue;
        }
        let display = match owner
            .display(&row.node_id, None, HistoryDisplayPosition::Tail)
            .await
        {
            Ok(display) => display,
            // Collected after the page was read: nothing to show.
            Err(error) if error.code == ErrorCode::TargetUnavailable => continue,
            Err(error) => return Err(error),
        };
        if display.total_chars == 0 || !matches!(display.outline.node, HistoryDisplayNode::Received)
        {
            continue;
        }
        let node_id = row.node_id;
        let node =
            presentation_work(move || display.into_ui_node(node_id, width, language)).await?;
        nodes.push((after, node));
    }
    Ok(nodes)
}

/// Read and deliver the snapshot for an installed page.
pub(super) async fn refresh(
    owner: &native_history::Owner<'_>,
    updates: &dyn UpdateTarget,
    thread: &str,
    generation: u64,
    width: u16,
    language: UiLanguage,
    scope: Scope,
) -> Result<(), ApiError> {
    let mut nodes = snapshot(owner, thread, width, language).await?;
    for (_, node) in &mut nodes {
        node.presentation.read_revision = scope.request_sequence;
    }
    update(
        updates,
        UiUpdate::IncompleteResponses {
            thread_id: thread.into(),
            generation,
            selection_revision: scope.selection_revision,
            view_revision: scope.view_revision,
            request_sequence: scope.request_sequence,
            nodes,
        },
    )
    .await;
    Ok(())
}

pub(super) async fn list(
    data_dir: &std::path::Path,
    language: UiLanguage,
    thread: String,
    cursor: Option<String>,
) -> Result<(String, Vec<crate::presentation::PresentationField>), ApiError> {
    list::read(data_dir, language, thread, cursor).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn snapshot_of(control: &Control) -> Vec<(String, UiNode)> {
        let owner = native_history::Owner {
            data_dir: compose_data_dir(control),
            instance: &control.store.instance_id,
        };
        snapshot(&owner, "t", 80, UiLanguage::En).await.unwrap()
    }

    #[test]
    fn received_labels_follow_ui_language_without_changing_source_facts() {
        let cases = [
            (
                UiLanguage::Ja,
                "incomplete_response",
                "途中で切れた応答（保存済み・会話には含めない） · /received 一覧",
                "途中で切れた応答（保存済み・会話には含めない） | /received 一覧",
                "保存原文",
                PresentationKind::Interrupted,
                EventClass::Attention,
            ),
            (
                UiLanguage::En,
                "incomplete_response",
                "Interrupted response (saved; not part of the conversation) · /received list",
                "Interrupted response (saved; not part of the conversation) | /received list",
                "Saved source",
                PresentationKind::Interrupted,
                EventClass::Attention,
            ),
            (
                UiLanguage::Ja,
                "model_stream_observation",
                "ストリーム差分 · 最終応答は確定済み · /received 一覧",
                "ストリーム差分 | 最終応答は確定済み | /received 一覧",
                "保存原文",
                PresentationKind::Event,
                EventClass::Change,
            ),
            (
                UiLanguage::En,
                "model_stream_observation",
                "Stream difference · final response confirmed · /received list",
                "Stream difference | final response confirmed | /received list",
                "Saved source",
                PresentationKind::Event,
                EventClass::Change,
            ),
        ];

        for (language, kind, dot_title, bar_title, label, expected_kind, expected_class) in cases {
            let node = project(
                kind,
                UiNode {
                    node_id: "source-node".into(),
                    text: "saved raw response".into(),
                    ..Default::default()
                },
                80,
                false,
                true,
                language,
            );

            assert!(
                node.presentation.title == dot_title || node.presentation.title == bar_title,
                "unexpected title for {language:?}: {}",
                node.presentation.title
            );
            assert_eq!(node.presentation.kind, expected_kind);
            assert_eq!(node.presentation.event_class, expected_class);
            assert_eq!(node.role, ROLE);
            assert_eq!(node.node_id, "source-node");
            assert_eq!(node.text, "saved raw response");
            let source_field = node
                .presentation
                .fields
                .iter()
                .find(|field| field.key == "/source_node_id")
                .expect("received source reference is retained");
            assert_eq!(source_field.label, label);
            assert_eq!(source_field.value, "source-node");
        }
    }
    #[test]
    fn received_response_titles_have_selected_language_copy_without_rewriting_saved_text() {
        for (kind, expected) in [
            (
                "incomplete_response",
                crate::presentation::messages::Text::ReceivedResponse,
            ),
            (
                "model_stream_observation",
                crate::presentation::messages::Text::StreamDifference,
            ),
        ] {
            let node = project(
                kind,
                UiNode {
                    node_id: "saved-response".into(),
                    text: "RAW_RESPONSE".into(),
                    ..Default::default()
                },
                80,
                false,
                true,
                UiLanguage::Ja,
            );
            let message = node.presentation.ui_messages.title.unwrap();
            assert_eq!(
                message,
                crate::presentation::messages::Message::Text(expected)
            );
            let english = message.render(crate::config::UiLanguage::En);
            assert!(english.is_ascii(), "{english}");
            assert!(english.contains("/received list"));
            assert_eq!(node.text, "RAW_RESPONSE");
        }
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
                db.execute("INSERT INTO node(id,turn_id,kind,payload_json,payload_bytes,created_at)VALUES(?1,'u',?2,?3,length(?3),?4)",rusqlite::params![format!("n{i}"),kind,body,format!("2026-01-01T00:00:{i:02}Z")])?;
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
        let (rpc, receive) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
        let list_page = |cursor| list(&owner.loaded.data_dir, UiLanguage::En, "t".into(), cursor);
        let nodes = snapshot_of(&control).await;
        assert_eq!(
            nodes
                .iter()
                .map(|(_, n)| n.node_id.as_str())
                .collect::<Vec<_>>(),
            ["n33", "n34"]
        );
        assert_eq!(nodes[0].1.text, "理由\n有料本文33");
        assert_eq!(nodes[0].1.reasoning_ranges, [(0, 2)]);
        assert!(
            nodes[0]
                .1
                .presentation
                .title
                .contains("Interrupted response")
        );
        assert!(
            nodes[1]
                .1
                .presentation
                .title
                .contains("final response confirmed")
        );
        let (title, first) = list_page(None).await.unwrap();
        assert!(title.contains("32+ records"));
        assert_eq!(
            first
                .iter()
                .filter(|f| f.key.starts_with("/received/source/"))
                .count(),
            32
        );
        let cursor = first
            .iter()
            .find_map(|field| field.key.strip_prefix("/received/cursor/"))
            .unwrap();
        let (_, second) = list_page(Some(cursor.to_owned())).await.unwrap();
        let sources = first
            .iter()
            .chain(&second)
            .filter(|field| field.key.starts_with("/received/source/"))
            .map(|field| field.value.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(sources.len(), 35);
        assert!(sources.contains("n0"));
        assert!(list_page(Some("invalid".into())).await.is_err());
        let raw = crate::history::History::new(owner.db.clone())
            .read(crate::protocol::HistoryReadArgs {
                display: None,
                end_message_only: false,
                parts: false,
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
        let nodes = snapshot_of(&control).await;
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].0, "anchor");
        assert_eq!(nodes[0].1.node_id, "n34");
        let (title, latest) = list_page(None).await.unwrap();
        assert!(title.contains("32+ records"));
        let empty = latest.iter().find(|field| field.value == "empty").unwrap();
        assert_eq!(empty.displayed(), "No request text");
        server.shutdown().await.unwrap();
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
            db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'fixture','fixture',0,'incomplete','{}',?1,'now')", [&saved])?;
            Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, receive) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
        let (tx, _rx) = std::sync::mpsc::sync_channel(8);
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
            let nodes = snapshot_of(&control).await;
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
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        let owner = crate::app::DataOwner::start(Some(config), Some(data), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, receive) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
        let nodes = snapshot_of(&control).await;
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
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
