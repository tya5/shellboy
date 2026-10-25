use crate::{
    frontend,
    ui::{UiAction, UiState},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use unicode_width::UnicodeWidthStr;

fn screen(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let mut text = String::new();
            let mut x = 0;
            while x < buffer.area.width {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_canonical_output_flows_through_frontend_into_human_and_internal_details() {
    let (root, owner, control, server) = super::fixture().await;
    // This is the actual durable assistant_response shape produced by the
    // runtime, rather than the legacy `items` envelope in the first unit fixture.
    let body = serde_json::json!({"group_id":null,"reasoning_epoch":0,"output":[{
        "type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":"実際の保存形式の本文 e\u{301} 🦀"}]}]});
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            crate::history::append_node(
                &tx,
                "thread",
                "persisted-answer",
                None,
                "assistant_response",
                &body,
                "2026-10-25T00:00:00Z",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let mut ui = UiState::new(&control.store.config, root.path().into());
    ui.select_thread("thread".into());
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    frontend::HistoryReader::new(&control, &updates, 100)
        .page(
            &frontend::Reply::new("thread", ui.view().generation),
            None,
            None,
        )
        .await
        .unwrap();
    while let Ok(update) = incoming.try_recv() {
        ui.apply(update);
    }
    assert!(
        ui.view().nodes()[0]
            .presentation
            .fields
            .iter()
            .any(|field| field.key == "/output")
    );
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    ui.event(key(KeyCode::F(6)));
    ui.event(key(KeyCode::Enter));
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut ui))
        .unwrap();
    let human = screen(&terminal);
    assert!(human.contains("本文: 実際の保存形式"), "{human}");
    for redundant in [
        "呼出し群ID: null",
        "reasoning_epoch",
        "返却結果（件数）",
        "content（件数）",
        "項目 1 › 項目 1",
    ] {
        assert!(!human.contains(redundant), "{redundant}: {human}");
    }
    ui.event(key(KeyCode::Char('v')));
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut ui))
        .unwrap();
    let internal = screen(&terminal);
    assert!(internal.contains("呼出し群ID: null"));
    assert!(internal.contains("reasoning_epoch: 0"));
    let mut copied_zero = false;
    for _ in 0..20 {
        copied_zero |= ui
            .event(key(KeyCode::Char('c')))
            .iter()
            .any(|action| matches!(action,UiAction::CopyText{text,..} if text=="0"));
        ui.event(key(KeyCode::Down));
    }
    assert!(
        copied_zero,
        "internal values remain copyable without a display approximation"
    );
    assert!(ui.event(key(KeyCode::Char('o'))).iter().any(
        |action| matches!(action,UiAction::LoadOriginal{node_id,..} if node_id=="persisted-answer")
    ));
    assert!(
        ui.view().draft.text.is_empty(),
        "detail keys cannot leak into the composer"
    );
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
