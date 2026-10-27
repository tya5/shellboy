//! A real user message invalidates an already captured context-edit snapshot.
use super::*;
use crate::{app::DataOwner, control::Control, protocol::*};

#[tokio::test]
async fn accepted_user_message_advances_selection_and_rejects_a_late_context_edit() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:9/v1\"\nresponse_max_retries=0\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let compactor = Compactor::new(control.store.database.clone(), control.store.config.clone());
    let request = Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        Command::ThreadCreate(ThreadCreateArgs {
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    );
    let created = control.request(request).await;
    assert!(created.ok, "{:?}", created.error);
    let thread = created.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let target = thread.clone();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            for id in ["old", "middle", "last"] {
                history::append_node(
                    &tx,
                    &target,
                    id,
                    None,
                    "message",
                    &json!({"role":"user","content":id}),
                    "now",
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = compactor.snapshot(&thread, true).await.unwrap();
    let captured_revision = snapshot.revision;
    let request = Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        Command::MessageSend(MessageSendArgs {
            thread_id: Some(thread.clone()),
            text: "new accepted user instruction".into(),
        }),
    );
    let sent = control.request(request).await;
    assert!(sent.ok, "{:?}", sent.error);
    let target = thread.clone();
    control
        .store
        .database
        .call(move |db| {
            let revision: i64 = db.query_row(
                "SELECT selection_revision FROM thread WHERE id=?1",
                [target],
                |row| row.get(0),
            )?;
            assert!(
                revision as u64 > captured_revision,
                "user receipt must invalidate the captured path immediately"
            );
            Ok(())
        })
        .await
        .unwrap();
    let error = compactor
        .omit_range(snapshot, 0..1, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("snapshot changed"));
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM replacement", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM thread_path WHERE node_id IN ('old','middle','last')",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                3
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
