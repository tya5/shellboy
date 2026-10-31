use shellboy::{
    app::DataOwner,
    control::Control,
    frontend,
    ipc::LocalServer,
    ui::{RecalledPrompt, UiAction, UiUpdate},
};
use std::{sync::mpsc::Receiver, time::Duration};

async fn recalled(
    incoming: &Receiver<UiUpdate>,
    expected_id: u64,
) -> Result<Option<RecalledPrompt>, String> {
    crate::test_watchdog::watchdog("tests/input_history_backend.rs: recalled", async {
        loop {
            match incoming.try_recv() {
                Ok(UiUpdate::RecalledInput {
                    thread_id,
                    generation,
                    request_id,
                    result,
                }) if request_id == expected_id => {
                    assert_eq!(thread_id, "saved");
                    assert_eq!(generation, 7);
                    return result;
                }
                Ok(UiUpdate::LoadFailed { message, .. }) => {
                    panic!("input recall must not fail the history viewport: {message}");
                }
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .expect("recall reply")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_recall_uses_db_worker_and_errors_do_not_fail_viewport_loading() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "[limits]\ndraft_bytes=16\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    owner.db.call(|db| {
        let tx=db.transaction()?;
        tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('saved','/','/AGENTS.md','fixture','fixture','now')",[])?;
        shellboy::history::append_node(&tx,"saved","old",None,"user",&serde_json::json!({"role":"user","content":[{"type":"input_text","text":"日本語\nlast\n"}]}),"now")?;
        shellboy::history::append_node(&tx,"saved","answer",None,"assistant_response",&serde_json::json!({"text":"not recalled"}),"now")?;
        shellboy::history::append_node(&tx,"saved","large",None,"user",&serde_json::json!({"text":"oversized input must be retained without truncation"}),"now")?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let db = owner.db.clone();
    let control = Control::new(&owner, root.path().into());
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    let request =
        |request_id, before_ordinal, before_node_id: Option<&str>| UiAction::RecallInput {
            thread_id: "saved".into(),
            generation: 7,
            request_id,
            before_ordinal,
            older: true,
            before_node_id: before_node_id.map(str::to_owned),
        };
    send.send(request(1, Some(2), Some("large"))).await.unwrap();
    assert_eq!(
        recalled(&incoming, 1).await.unwrap().map(|prompt| (
            prompt.ordinal,
            prompt.node_id,
            prompt.text
        )),
        Some((0, "old".into(), "日本語\nlast\n".into()))
    );
    send.send(request(2, None, None)).await.unwrap();
    assert!(
        recalled(&incoming, 2)
            .await
            .unwrap_err()
            .contains("no text was truncated")
    );
    send.send(request(3, Some(2), Some("displaced")))
        .await
        .unwrap();
    assert!(
        recalled(&incoming, 3)
            .await
            .unwrap_err()
            .contains("path changed")
    );
    send.send(request(4, Some(0), Some("old"))).await.unwrap();
    assert!(recalled(&incoming, 4).await.unwrap().is_none());
    // Hold the DB worker so all eight read slots stay occupied. Admission
    // failure must finish the recall request, without changing viewport state.
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocked = tokio::spawn(async move {
        db.call(move |_| {
            let _ = entered_tx.send(());
            crate::test_watchdog::blocking_recv("release DB barrier", &release_rx).unwrap();
            Ok(())
        })
        .await
    });
    entered_rx.await.unwrap();
    for id in 5..=13 {
        send.send(request(id, Some(2), Some("large")))
            .await
            .unwrap();
    }
    assert!(
        recalled(&incoming, 13)
            .await
            .unwrap_err()
            .contains("キューが満杯")
    );
    release_tx.send(()).unwrap();
    blocked.await.unwrap().unwrap();
    send.send(UiAction::Quit).await.unwrap();
    crate::test_watchdog::watchdog("tests/input_history_backend.rs: persisted_recall_uses_db_worker_and_errors_do_not_fail_viewport_loading", bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
