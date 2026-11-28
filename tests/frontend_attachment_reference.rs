//! Actual frontend reference dispatch; saved facts only, no native file/API access.
use crate::test_watchdog::watchdog;
use shellboy::{
    app::DataOwner,
    control::Control,
    frontend,
    ipc::LocalServer,
    presentation::PresentationField,
    protocol::{AttachmentListArgs, Command, OperationTarget},
    ui::{ManagementFollow, UiAction, UiUpdate},
};

fn field<'a>(fields: &'a [PresentationField], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.as_str())
}
async fn report(
    incoming: &mut tokio::sync::mpsc::Receiver<UiUpdate>,
    ready: impl Fn(&[PresentationField]) -> bool,
) -> (u64, Vec<PresentationField>) {
    watchdog("saved attachment reference report", async {
        while let Some(update) = incoming.recv().await {
            if let UiUpdate::StructuredManagement {
                request_id,
                title,
                fields,
                ..
            } = update
            {
                assert!(
                    ready(&fields),
                    "Unexpected saved attachment report {title}: {:?}",
                    fields.iter().map(|f| &f.key).collect::<Vec<_>>()
                );
                return (request_id, fields);
            }
        }
        panic!("frontend closed before saved attachment report")
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_reference_reads_exact_foreign_selection_outside_list_then_original_source() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    owner.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('actual','/','','fixture','synthetic','now'),('viewer','/','','fixture','synthetic','now');
          INSERT INTO node(id,kind,created_at,payload_json)VALUES('source','user','now','{\"role\":\"user\",\"content\":\"SOURCE_ATTACHMENT_SENTINEL\"}');
          INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('late','source','/SHELLBOY_SYNTHETIC_NO_FILE_PROBE','file','external','now');
          INSERT INTO attachment_selection(thread_id,attachment_id,state,reason,selected_at,selected_order)VALUES('actual','late','excluded','explicitly removed','now',99999);
          INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('fixture.0:attachment','fixture','null','hash','completed','{\"result\":{\"thread_id\":\"actual\",\"attachment_id\":\"late\",\"source_node_id\":\"source\"}}','now');")?;
        for i in 0..150 {
            let id = format!("early-{i:04}");
            db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES(?1,'source','/SHELLBOY_SYNTHETIC_NO_FILE_PROBE','file','external','now')", [&id])?;
            db.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('actual',?1,'active','now',0)", [&id])?;
        }
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let list_saved = |attachment_ids| async {
        let response = shellboy::ipc::call(
            &owner.loaded.data_dir,
            Command::AttachmentList(AttachmentListArgs {
                attachment_ids,
                thread_id: Some("actual".into()),
                limit: 2,
                cursor: None,
            }),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(response.ok, "{:?}", response.error);
        response.result.unwrap()
    };
    let first = list_saved(None).await;
    let ids = vec!["late".into(), "early-0000".into()];
    let exact_page = list_saved(Some(ids)).await;
    assert_eq!(exact_page["items"][0], first["items"][0]);
    assert_eq!(exact_page["items"][1]["attachment_id"], "late");
    assert!(exact_page["next_cursor"].is_null());
    let (actions, receiver) = tokio::sync::mpsc::channel(16);
    let (updates, source) = std::sync::mpsc::sync_channel(128);
    // Notification-driven test adapter; both channels are finite and owned by
    // this fixture, and the blocking receiver is joined off the async thread.
    let (notify, mut incoming) = tokio::sync::mpsc::channel(128);
    let forwarder = std::thread::spawn(move || {
        while let Ok(update) = source.recv() {
            if notify.blocking_send(update).is_err() {
                break;
            }
        }
    });
    let bridge = tokio::spawn(frontend::run(
        control,
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    actions
        .send(UiAction::Manage(Command::AttachmentList(
            AttachmentListArgs {
                attachment_ids: None,
                thread_id: Some("actual".into()),
                limit: 2,
                cursor: None,
            },
        )))
        .await
        .unwrap();
    let (_, list) = report(&mut incoming, |fields| {
        field(fields, "/items/0/thread_id") == Some("actual")
    })
    .await;
    assert!(
        field(&list, "/next_cursor").is_some(),
        "first owner page is not the whole list"
    );
    assert!(
        !list
            .iter()
            .any(|f| f.key.ends_with("/attachment_id") && f.value == "late")
    );
    actions
        .send(UiAction::Manage(Command::OperationGet(OperationTarget {
            operation_id: "fixture.0:attachment".into(),
        })))
        .await
        .unwrap();
    let (request, _) = report(&mut incoming, |fields| {
        field(fields, "/operation_id") == Some("fixture.0:attachment")
    })
    .await;
    actions
        .send(UiAction::FollowManagement {
            request_id: request,
            pointer: "/outcome/result/attachment_id".into(),
            mode: ManagementFollow::Inspect,
        })
        .await
        .unwrap();
    let (request, exact) = report(&mut incoming, |fields| {
        field(fields, "/items/0/attachment_id") == Some("late")
    })
    .await;
    assert_eq!(field(&exact, "/items/0/thread_id"), Some("actual"));
    assert_eq!(field(&exact, "/items/0/selected"), Some("false"));
    assert!(field(&exact, "/items/1/attachment_id").is_none());
    actions
        .send(UiAction::FollowManagement {
            request_id: request,
            pointer: "/items/0/attachment_id".into(),
            mode: ManagementFollow::Read,
        })
        .await
        .unwrap();
    let (_, source) = report(&mut incoming, |fields| {
        field(fields, "/text") == Some("SOURCE_ATTACHMENT_SENTINEL")
    })
    .await;
    assert_eq!(field(&source, "/node_id"), Some("source"));
    actions.send(UiAction::Quit).await.unwrap();
    watchdog("saved attachment frontend shutdown", bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(incoming);
    tokio::task::spawn_blocking(move || forwarder.join().unwrap())
        .await
        .unwrap();
}
