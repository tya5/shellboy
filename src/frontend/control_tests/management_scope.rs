use crate::{
    frontend,
    ui::{UiAction, UiState, UiUpdate},
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_already_queued_management_result_stays_durable_but_cannot_reopen_a_closed_other_view() {
    let (root, owner, control, server) = super::fixture().await;
    let mut ui = UiState::new(&control.store.config, root.path().into());
    let (sender, receiver) = crate::ui::actions::channel(32);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    for action in ui.select_thread("thread".into()) {
        sender.try_send(action).unwrap();
    }
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    ui.event(key(KeyCode::F(8)));
    ui.event(Event::Paste("thread create".into()));
    ui.event(key(KeyCode::Enter));
    let actions = ui.event(key(KeyCode::Enter));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::ManagementIntent { .. }))
    );
    for action in actions {
        sender.try_send(action).unwrap();
    }
    let queued=tokio::time::timeout(std::time::Duration::from_secs(10),async {
        loop {
            while let Ok(update)=incoming.try_recv(){
                if matches!(&update,UiUpdate::ScopedManagement{update,..} if matches!(update.as_ref(),UiUpdate::StructuredManagement{..})) {return update;}
                ui.apply(update);for action in ui.take_deferred_actions(){sender.try_send(action).unwrap();}
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    let count = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row("SELECT count(*) FROM thread", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(
        count, 3,
        "Thread creation committed before the display result was consumed"
    );
    ui.event(key(KeyCode::Esc));
    ui.select_thread("independent".into());
    let status = ui.view().status.clone();
    ui.apply(queued.clone());
    assert!(ui.management.is_none());
    assert_eq!(
        ui.view().status,
        status,
        "late text cannot enter the newly selected conversation"
    );
    ui.select_thread("thread".into());
    ui.apply(queued);
    assert!(
        ui.management.is_none(),
        "the same original owner in a new epoch also rejects the closed result"
    );
    let count = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row("SELECT count(*) FROM thread", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(
        count, 3,
        "closing the view does not cancel or repeat a committed mutation"
    );
    sender.try_send(UiAction::Quit).unwrap();
    super::finish_bridge(bridge, &incoming).await;
}
