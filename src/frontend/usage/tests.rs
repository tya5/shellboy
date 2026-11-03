use super::*;
use crate::{
    app::DataOwner,
    presentation,
    ui::{UiState, UiUpdate},
};
use serde_json::json;

#[test]
fn cached_usage_never_turns_missing_or_invalid_counts_into_zero() {
    assert_eq!(cached_tokens(None), "未報告");
    for count in [0, 12345, i64::MAX as u64] {
        assert_eq!(cached_tokens(Some(count)), count.to_string());
    }
}

fn node(id: &str) -> UiNode {
    let payload = json!({"output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"synthetic answer"}]}]});
    UiNode {
        node_id: id.into(),
        role: "assistant".into(),
        text: "synthetic answer".into(),
        base_char_offset: 0,
        total_chars: 16,
        reasoning_ranges: vec![],
        presentation: presentation::project("assistant_response", &payload, "synthetic answer", 0),
    }
}
fn field<'a>(node: &'a UiNode, key: &str) -> Option<&'a str> {
    node.presentation
        .fields
        .iter()
        .find(|field| field.key == key)
        .map(|field| field.value.as_str())
}

#[tokio::test]
async fn history_usage_is_bound_to_exact_adopted_attempt_and_keeps_saved_json_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now');
        INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
        INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');")?;
        for (id,usage) in [
            ("old",&*format!(r#"{{"input_tokens":20000,"input_tokens_details":{{"cached_tokens":16000}},"opaque":{{"retain":true}},"attribution":"{}"}}"#,"x".repeat(128*1024))),
            ("new",r#"{"input_tokens_details":{"cached_tokens":0}}"#),
            ("missing",r#"{"input_tokens":123}"#),
            ("invalid",r#"{"input_tokens_details":{"cached_tokens":"9000"}}"#),
            ("oversized",&format!(r#"{{"input_tokens_details":{{"cached_tokens":999}},"extra":"{}"}}"#,"x".repeat(128 * 1024))),
        ] {
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES(?1,'u',0,0,'p','m',0,'completed','{}',?2,'now')",rusqlite::params![id,usage])?;
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'u','assistant_response','now','{}',2)",[id])?;
            db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,0,'message','completed','{}',?1)",[id])?;
        }
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('ambiguous','u','assistant_response','now','{}',2)",[])?;
        for id in ["old","new"] {db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,1,'message','completed','{}','ambiguous')",[id])?;}
        Ok(())
    }).await.unwrap();
    let saved = owner
        .db
        .call(|db| {
            Ok(db
                .prepare("SELECT id,usage_json FROM model_attempt ORDER BY id")?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    let mut nodes = [
        "old",
        "new",
        "missing",
        "invalid",
        "oversized",
        "ambiguous",
        "unrelated",
    ]
    .map(node);
    let requested = requests(&nodes);
    let observations = control
        .store
        .database
        .call(move |db| read_tx(db, &requested))
        .await
        .unwrap();
    assert_eq!(
        observations[0].sources,
        vec![Source {
            attempt: "old".into(),
            input_tokens: Some(20000),
            cached_tokens: Some(16000),
            input_bytes: None,
        }]
    );
    assert_eq!(observations[5].sources.len(), 2);
    assert!(observations[6].sources.is_empty());
    apply(&mut nodes, observations);
    assert_eq!(field(&nodes[0], FIELD), Some("16000"));
    assert_eq!(
        field(&nodes[0], ATTEMPT),
        Some("old"),
        "latest attempt in same Turn must never replace its source"
    );
    assert_eq!(field(&nodes[1], FIELD), Some("0"));
    assert_eq!(field(&nodes[2], FIELD), Some("未報告"));
    assert_eq!(field(&nodes[3], FIELD), Some("未報告"));
    use crate::config::UiLanguage::En;
    assert_eq!(
        crate::presentation::fields::attempt_label(FIELD, En),
        Some("Cached input tokens (provider-reported)")
    );
    assert_eq!(
        crate::presentation::fields::attempt_value(FIELD, field(&nodes[6], FIELD).unwrap(), En,)
            .as_deref(),
        Some("Not reported (no request record)")
    );
    assert_eq!(field(&nodes[4], FIELD), Some("999"));
    assert_eq!(
        field(&nodes[5], FIELD),
        Some("未報告（要求との対応が一意でない）")
    );
    assert_eq!(field(&nodes[5], ATTEMPT), None);
    assert_eq!(field(&nodes[6], FIELD), Some("未報告（要求の記録なし）"));
    enrich(&control, &mut nodes).await.unwrap();
    assert_eq!(
        nodes[0]
            .presentation
            .fields
            .iter()
            .filter(|field| field.key == FIELD)
            .count(),
        1,
        "refresh is idempotent"
    );
    assert!(nodes.iter().all(|node| node.text == "synthetic answer"));
    let after = owner
        .db
        .call(|db| {
            Ok(db
                .prepare("SELECT id,usage_json FROM model_attempt ORDER BY id")?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    assert_eq!(after, saved, "display enrichment never edits usage_json");
    let mut ui = UiState::new(&control.store.config, root.path().into());
    ui.select_thread("t".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "t".into(),
        generation,
        nodes: vec![nodes[0].clone()],
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
    });
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    for key in [KeyCode::F(6), KeyCode::Enter] {
        ui.event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)));
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 35)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut ui))
        .unwrap();
    let screen = (0..35)
        .map(|y| {
            (0..120)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        screen.contains("Cached input tokens (provider-reported): 16,000")
            && screen.contains("Input tokens (provider-reported): 20,000")
            && screen.contains("Input bytes (UTF-8): Not recorded"),
        "{screen}"
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

fn render_details(ui: &mut UiState, nodes: Vec<UiNode>) -> String {
    ui.select_thread("t".into());
    let generation = ui.view().generation;
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "t".into(),
        generation,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 1,
        total_nodes: 1,
    });
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    for key in [KeyCode::F(6), KeyCode::Enter] {
        ui.event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)));
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 35)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, ui)).unwrap();
    (0..35)
        .map(|y| {
            (0..160)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// #186: F6 shows the adopted request's measured bytes by source beside the
/// provider-reported tokens, in both languages; unknown is never zero.
#[tokio::test]
async fn details_show_request_bytes_beside_reported_tokens_in_both_languages() {
    use crate::model::wire_bytes::{Category, Label, measure};
    let wire = json!({"input":[{"role":"user","content":"日本語の質問"},{"type":"function_call_output","output":"result"}],"model":"m"});
    let bytes = measure(
        &wire,
        &[
            ("/input/0".into(), Label::Whole(Category::User)),
            ("/input/1".into(), Label::Whole(Category::ToolResult)),
        ],
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let manifest = json!({"input_bytes":bytes.to_value()}).to_string();
    owner.db.call(move |db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now');
        INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
        INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');")?;
        for (id, manifest, usage) in [
            ("measured", manifest.as_str(), r#"{"input_tokens":4500,"input_tokens_details":{"cached_tokens":3000}}"#),
            ("legacy", "{}", "{}"),
        ] {
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES(?1,'u',0,0,'p','m',0,'completed',?2,?3,'now')",rusqlite::params![id,manifest,usage])?;
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'u','assistant_response','now','{}',2)",[id])?;
            db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,0,'message','completed','{}',?1)",[id])?;
        }
        Ok(())
    }).await.unwrap();
    let mut nodes = ["measured", "legacy"].map(node);
    enrich(&control, &mut nodes).await.unwrap();
    assert_eq!(
        field(&nodes[0], BYTES),
        Some(&*bytes.to_value().to_string())
    );
    assert_eq!(field(&nodes[1], BYTES), Some(NOT_RECORDED));
    assert_eq!(field(&nodes[1], TOKENS), Some(NOT_REPORTED));
    let user = bytes.get(Category::User);
    let tool = bytes.get(Category::ToolResult);
    let other = bytes.get(Category::Other);
    for (language, expected) in [
        (
            crate::config::UiLanguage::Ja,
            vec![
                format!(
                    "入力のバイト数（UTF-8）: 合計 {} / 利用者 {user} / ツール結果 {tool} / その他 {other}",
                    bytes.total
                ),
                "入力トークン（提供元の報告）: 4,500".into(),
                "キャッシュ済み入力トークン（提供元の報告）: 3,000".into(),
            ],
        ),
        (
            crate::config::UiLanguage::En,
            vec![
                format!(
                    "Input bytes (UTF-8): Total {} / User {user} / Tool results {tool} / Other {other}",
                    bytes.total
                ),
                "Input tokens (provider-reported): 4,500".into(),
                "Cached input tokens (provider-reported): 3,000".into(),
            ],
        ),
    ] {
        let mut config = (*control.store.config).clone();
        config.ui.language = language;
        let mut ui = UiState::new(&config, root.path().into());
        let screen = render_details(&mut ui, vec![nodes[0].clone()]);
        // Wide cells leave padding cells; compare without spaces.
        let compact = |text: &str| text.replace(' ', "");
        for line in &expected {
            assert!(
                compact(&screen).contains(&compact(line)),
                "{line}\n{screen}"
            );
        }
        let mut ui = UiState::new(&config, root.path().into());
        let screen = render_details(&mut ui, vec![nodes[1].clone()]);
        let unknown = match language {
            crate::config::UiLanguage::Ja => "入力のバイト数（UTF-8）: 未記録",
            crate::config::UiLanguage::En => "Input bytes (UTF-8): Not recorded",
        };
        assert!(compact(&screen).contains(&compact(unknown)), "{screen}");
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
