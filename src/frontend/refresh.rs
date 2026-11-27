//! The selected current view from the native owner: header, one window
//! observation with its display extents and queue, and the bounded pinned
//! tool nodes' facts from the same observation.
use super::*;
use history_reader::{Anchor, Page};
use native_history::Owner;
use native_page::KnownSubmission;

pub(super) struct Query {
    pub data_dir: std::path::PathBuf,
    pub instance_id: String,
    pub thread: String,
    pub ids: Vec<String>,
    pub generation: Option<u64>,
    pub visible: bool,
    pub epoch: u64,
    pub render_width: u16,
    pub tool_scope: Option<ToolObservation>,
    pub known: Vec<KnownSubmission>,
}

/// Pinned tool nodes outside the window share its observation, at most 128.
const TOOL_SCOPE_NODES: usize = 128;

struct Current {
    details: ThreadDetails,
    page: Page,
    before_cursor: Option<String>,
    received: Option<received::Prepared>,
    activity: activity::Projection,
    context_usage: crate::ui::ContextUsage,
    tool_scope: Option<ToolObservation>,
}

/// Provider-reported tokens of `thread.get`'s last successful attempt; all
/// unknown when the owner reported none or could not bound that evidence.
fn reported_context_usage(usage: Option<&Value>) -> crate::ui::ContextUsage {
    let latest = usage.map(|usage| &usage["last_successful_attempt"]);
    let tokens = |key: &str| latest.and_then(|latest| latest[key].as_u64());
    crate::ui::ContextUsage {
        input_tokens: tokens("input_tokens"),
        cached_tokens: tokens("cached_tokens"),
        output_tokens: tokens("output_tokens"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn read_current(
    ui: &crate::config::Ui,
    owner: &Owner<'_>,
    thread: &str,
    render_width: u16,
    known: &[KnownSubmission],
    mut tool_scope: Option<ToolObservation>,
    stop: CancellationToken,
) -> Result<Current, ApiError> {
    let language = ui.language;
    if tool_scope
        .as_ref()
        .is_some_and(|scope| scope.nodes.len() > TOOL_SCOPE_NODES)
    {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Visible tool source scope exceeds its node bound",
        ));
    }
    let details = super::native_header(owner.data_dir, owner.instance, thread).await?;
    let pinned = tool_scope
        .as_ref()
        .map(|scope| {
            scope
                .nodes
                .iter()
                .map(|node| node.node_id.clone())
                .collect()
        })
        .unwrap_or_default();
    let page = history_reader::read_page(
        language,
        owner,
        render_width,
        known,
        history_reader::window_args(thread, Anchor::Tail, 49, 0, pinned),
        None,
        |items| 0..items.len(),
        |_| None,
    )
    .await?;
    let before_cursor = history_reader::before_cursor(thread, page.info.first, page.info.revision)?;
    let read_revision = page.info.read_revision;
    let context_usage = reported_context_usage(details.context_usage());
    if let Some(scope) = &mut tool_scope {
        let tools = page.observation.tools.clone();
        let sources = page.observation.pinned_sources.clone();
        let mut nodes = std::mem::take(&mut scope.nodes);
        nodes = presentation_work(move || {
            message_metadata::apply_pinned(&mut nodes, &sources);
            tools::reset_observations(&mut nodes);
            tools::apply_window(&mut nodes, &tools);
            for node in &mut nodes {
                node.presentation.read_revision = read_revision;
            }
            nodes
        })
        .await?;
        scope.nodes = nodes;
    }
    let received = Some(received::read_native(owner, &page, render_width, language).await?);
    let activity =
        activity::project_native(ui, owner, thread, details.activity().cloned(), stop).await?;
    Ok(Current {
        details: details.prepare_for_ui(language),
        page,
        before_cursor,
        received,
        activity,
        context_usage,
        tool_scope,
    })
}

pub(super) async fn run(
    query: Query,
    ui: crate::config::Ui,
    updates: impl UpdateTarget + Send + 'static,
    sender: mpsc::Sender<BackgroundResult>,
    stop: CancellationToken,
    progress_slots: Arc<Semaphore>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let Query {
        data_dir,
        instance_id,
        thread,
        ids,
        generation,
        visible,
        epoch,
        render_width,
        tool_scope,
        known,
    } = query;
    let _permit = permit;
    let owner = Owner {
        data_dir: &data_dir,
        instance: &instance_id,
    };
    tokio::select! {biased;_=stop.cancelled()=>{},_=async {
        let receipts = native_page::receipts(&owner, &ids).await;
        if !visible || generation.is_none() {
            let _=sender.send(BackgroundResult::Refreshed {thread_id:thread,receipts,progress:None,epoch,generation}).await;
            return;
        }
        let generation=generation.expect("visible generation checked above");
        let tool_revision=tool_scope.as_ref().map(|scope|scope.revision);
        let Ok(_progress_permit)=progress_slots.acquire_owned().await else {
            let error=ApiError::new(ErrorCode::InvalidState,"Current-view progress admission is closed");
            if let Some(revision)=tool_revision {
                update(&updates,UiUpdate::ToolFacts {thread_id:thread.clone(),generation,revision,result:Err(error.to_string())}).await;
            }
            let _=sender.send(BackgroundResult::Refreshed {thread_id:thread,receipts,progress:Some(Err(error)),epoch,generation:Some(generation)}).await;
            return;
        };
        let current = read_current(&ui, &owner, &thread, render_width, &known, tool_scope, stop.clone()).await;
        drop(_progress_permit);
        let result: Result<activity::Projection, ApiError>=async {
            let Current {details, page, before_cursor, received, activity, context_usage, mut tool_scope} = current?;
            let Page {mut nodes, info, ..} = page;
            let page_len=nodes.len();
            if let Some(scope)=&mut tool_scope {nodes.append(&mut scope.nodes);}
            // Raw previews are owned by visible UI items, not every Changed snapshot.
            streams::seal_nodes(&mut nodes).await?;
            if let Some(scope)=&mut tool_scope {scope.nodes=nodes.split_off(page_len);}
            let received=received.map(|received|Box::new(UiUpdate::IncompleteResponses {
                thread_id:thread.clone(),generation,selection_revision:received.selection_revision,
                view_revision:None,root_revision:received.root_revision,nodes:received.nodes,
            }));
            update(&updates,UiUpdate::CurrentView {
                generation,details,context_usage,
                history:Box::new(UiUpdate::History {
                    thread_id:thread.clone(),generation,nodes,pending:info.pending,
                    read_revision:info.read_revision,before_cursor,append:false,
                    first_ordinal:info.first,selection_revision:info.revision,total_nodes:info.total,
                }),
                received,tool_facts:tool_scope,committed:info.committed,
            }).await;
            Ok(activity)
        }.await;
        if let Err(error)=&result {
            if let Some(revision)=tool_revision {
                update(&updates,UiUpdate::ToolFacts {thread_id:thread.clone(),generation,revision,result:Err(error.to_string())}).await;
            }
            load_failed(&updates,thread.clone(),generation,error.clone()).await;
        }
        let _=sender.send(BackgroundResult::Refreshed {thread_id:thread,receipts,progress:Some(result),epoch,generation:Some(generation)}).await;
    }=>{}}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageError;
    use crate::{
        app::DataOwner,
        presentation::{BlockKind, InputSource, NodePresentation, ToolPresentation},
    };
    use rusqlite::Connection;
    use serde_json::json;

    fn items(window: &crate::protocol::HistoryWindow) -> Vec<(String, String)> {
        window
            .items
            .iter()
            .map(|item| (item.node_id.clone(), item.kind.clone()))
            .collect()
    }

    struct Observed {
        receipts: Result<Vec<(String, ReceiptStatus)>, ApiError>,
        progress: Option<Result<activity::Projection, ApiError>>,
        view: Option<(
            ThreadDetails,
            crate::ui::ContextUsage,
            Box<UiUpdate>,
            Option<ToolObservation>,
        )>,
    }

    /// One native current-view refresh, collected from its outputs.
    async fn refresh(
        control: &Arc<Control>,
        data_dir: &std::path::Path,
        ids: Vec<String>,
        tool_scope: Option<ToolObservation>,
    ) -> Observed {
        let (updates, received) = std::sync::mpsc::sync_channel(64);
        let (sender, mut results) = mpsc::channel(8);
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        crate::test_watchdog::watchdog(
            "native current view",
            run(
                Query {
                    data_dir: data_dir.to_path_buf(),
                    instance_id: control.store.instance_id.clone(),
                    thread: "t".into(),
                    ids,
                    generation: Some(1),
                    visible: true,
                    epoch: 0,
                    render_width: 80,
                    tool_scope,
                    known: Vec::new(),
                },
                control.store.config.ui.clone(),
                updates,
                sender,
                CancellationToken::new(),
                Arc::new(Semaphore::new(1)),
                permit,
            ),
        )
        .await
        .unwrap();
        let mut observed = Observed {
            receipts: Ok(Vec::new()),
            progress: None,
            view: None,
        };
        while let Ok(result) = results.try_recv() {
            if let BackgroundResult::Refreshed {
                receipts, progress, ..
            } = result
            {
                observed.receipts = receipts;
                observed.progress = progress;
            }
        }
        observed.view = received.try_iter().find_map(|update| match update {
            UiUpdate::CurrentView {
                details,
                context_usage,
                history,
                tool_facts,
                ..
            } => Some((details, context_usage, history, tool_facts)),
            _ => None,
        });
        observed
    }

    async fn served(
        root: &tempfile::TempDir,
    ) -> (DataOwner, Arc<Control>, crate::ipc::LocalServer) {
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, dispatcher) = mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(dispatcher));
        control.start().await.unwrap();
        (owner, control, server)
    }

    #[tokio::test]
    async fn oversized_paused_scope_is_rejected_before_any_owner_read() {
        let root = tempfile::tempdir().unwrap();
        let (owner, control, server) = served(&root).await;
        let scope = ToolObservation {
            revision: 7,
            view_revision: 3,
            nodes: (0..129)
                .map(|n| UiNode {
                    node_id: format!("source-{n}"),
                    ..Default::default()
                })
                .collect(),
        };
        let observed = refresh(&control, &owner.loaded.data_dir, vec![], Some(scope)).await;
        let error = observed.progress.unwrap().err().unwrap();
        assert_eq!(error.code, ErrorCode::InputLimit);
        assert!(observed.view.is_none());
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_window_facts_keep_the_selected_transaction_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','before','/','','p','m','now');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','now','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','now','now');
                INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'p','m',0,'completed',json_object('adoption',json_object('outcome','adopted')),'{\"input_tokens\":140,\"output_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":120}}','now');
                INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now');
                INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g','call',0,'shell','{\"command\":\"printf saved\"}','completed');")?;
            shellboy_fixture_input(&tx,"input",true)?;
            let call=json!({"group_id":"g","item":{"type":"function_call","call_id":"call","name":"shell","arguments":"{\"command\":\"printf saved\"}"}});
            crate::history::append_node(&tx,"t","call-source",Some("u"),"assistant_response",&call,"now")?;
            let result=json!({"group_id":"g","item":{"type":"function_call_output","call_id":"call","output":json!({"status":"exited","exit_code":0,"output":"saved successful output"}).to_string()}});
            crate::history::append_node(&tx,"t","result-source",Some("u"),"tool_result",&result,"now")?;
            tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','call','result-source','exited')",[])?;
            shellboy_fixture_input(&tx,"queued",false)?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let observed_window = |db: &Connection| {
            crate::history::window(
                db,
                &serde_json::from_value(json!({"thread_id":"t", "include_observation":true}))
                    .unwrap(),
            )
        };
        // Pin the selected path before a separate writer changes a tool result.
        // The display facts must still come from that WAL view.
        let mut read = Connection::open(owner.db.path()).unwrap();
        let tx = read.transaction().unwrap();
        let window = observed_window(&tx).unwrap();
        owner
            .db
            .call(|db| {
                db.execute(
                    "UPDATE tool_result SET status='failed' WHERE node_id='result-source'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let old =
            crate::history::window_facts::read_window_facts(&tx, &items(&window), &[]).unwrap();
        assert!(
            old.tools
                .calls
                .iter()
                .any(|call| call.node_id == "result-source"
                    && call.group_id == "g"
                    && call.call_id == "call"
                    && call.result_state == "exited")
        );
        tx.commit().unwrap();
        let later = observed_window(&read).unwrap();
        assert_eq!(later.revision, window.revision);
        let fresh =
            crate::history::window_facts::read_window_facts(&read, &items(&later), &[]).unwrap();
        assert!(
            fresh
                .tools
                .calls
                .iter()
                .any(|call| call.node_id == "result-source"
                    && call.group_id == "g"
                    && call.call_id == "call"
                    && call.result_state == "failed")
        );
        let block = |group: &str| crate::presentation::PresentationBlock {
            kind: crate::presentation::BlockKind::ToolCall,
            tool: Some(crate::presentation::ToolPresentation {
                call_id: "call".into(),
                group_id: Some(group.into()),
                status: "requested".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let node = UiNode {
            node_id: "call-source".into(),
            presentation: crate::presentation::NodePresentation {
                blocks: vec![block("g"), block("different-group")],
                ..Default::default()
            },
            ..Default::default()
        };
        for (facts, state) in [(&old.tools, "completed"), (&fresh.tools, "failed")] {
            let mut nodes = [node.clone()];
            tools::apply_window(&mut nodes, facts);
            let blocks = &nodes[0].presentation.blocks;
            assert_eq!(blocks[0].tool.as_ref().unwrap().status, state);
            assert_eq!(blocks[1].tool.as_ref().unwrap().status, "requested");
        }
        drop(read);
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn current_page_queue_paused_facts_and_activity_share_one_observation() {
        let root = tempfile::tempdir().unwrap();
        let (owner, control, server) = served(&root).await;
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','before','/','','p','m','now');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','now','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','now','now');
                INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'p','m',0,'completed',json_object('adoption',json_object('outcome','adopted')),'{\"input_tokens\":140,\"output_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":120}}','now');
                INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','a','u','completed','now');
                INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g','call',0,'shell','{\"command\":\"printf saved\"}','completed');")?;
            shellboy_fixture_input(&tx,"input",true)?;
            let call=json!({"group_id":"g","item":{"type":"function_call","call_id":"call","name":"shell","arguments":"{\"command\":\"printf saved\"}"}});
            crate::history::append_node(&tx,"t","call-source",Some("u"),"assistant_response",&call,"now")?;
            let result=json!({"group_id":"g","item":{"type":"function_call_output","call_id":"call","output":json!({"status":"exited","exit_code":0,"output":"saved successful output"}).to_string()}});
            crate::history::append_node(&tx,"t","result-source",Some("u"),"tool_result",&result,"now")?;
            tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','call','result-source','exited')",[])?;
            shellboy_fixture_input(&tx,"queued",false)?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let scope = ToolObservation {
            revision: 7,
            view_revision: 3,
            nodes: vec![UiNode {
                node_id: "call-source".into(),
                presentation: NodePresentation {
                    blocks: vec![crate::presentation::PresentationBlock {
                        kind: BlockKind::ToolCall,
                        tool: Some(ToolPresentation {
                            call_id: "call".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            }],
        };
        let observed = refresh(
            &control,
            &owner.loaded.data_dir,
            vec!["queued".into()],
            Some(scope),
        )
        .await;
        assert_eq!(
            observed.receipts.unwrap(),
            [("queued".into(), ReceiptStatus::Held)]
        );
        let (details, context_usage, history, scope) = observed.view.unwrap();
        assert_eq!(details.title, "before");
        assert_eq!(details.model, "m");
        assert_eq!(
            context_usage,
            crate::ui::ContextUsage {
                input_tokens: Some(140),
                cached_tokens: Some(120),
                output_tokens: Some(20)
            }
        );
        let UiUpdate::History {
            nodes,
            pending,
            read_revision,
            ..
        } = *history
        else {
            panic!("selected history")
        };
        assert_eq!(pending.total, 1);
        assert_eq!(pending.items[0].message_id, "queued");
        assert!(read_revision > 0);
        assert!(
            nodes
                .iter()
                .all(|node| node.presentation.read_revision == read_revision)
        );
        let input = nodes.iter().find(|node| node.node_id == "input").unwrap();
        assert_eq!(
            input.presentation.input_source,
            InputSource::Unknown,
            "explicit CLI label does not confer human-band authority"
        );
        let scope = scope.unwrap();
        assert_eq!((scope.revision, scope.view_revision), (7, 3));
        assert_eq!(scope.nodes[0].presentation.read_revision, read_revision);
        assert_eq!(
            scope.nodes[0].presentation.blocks[0]
                .tool
                .as_ref()
                .unwrap()
                .group_id
                .as_deref(),
            Some("g")
        );
        assert_eq!(
            observed.progress.unwrap().unwrap().snapshot.phase,
            crate::activity::ActivityPhase::Completed
        );
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    fn shellboy_fixture_input(
        tx: &rusqlite::Transaction<'_>,
        id: &str,
        admitted: bool,
    ) -> Result<(), StorageError> {
        let payload = json!({"role":"user","content":[{"type":"input_text","text":id}]});
        if admitted {
            crate::history::append_node(tx, "t", id, Some("u"), "user", &payload, id)?;
        } else {
            tx.execute(
                "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user',?1,?2)",
                rusqlite::params![id, payload.to_string()],
            )?;
        }
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,path_admission)VALUES(?1,?1,'{\"kind\":\"external_cli\",\"label\":\"human\"}','t',?1,'r','u',?2,?1,?3)",rusqlite::params![id,if admitted {"applied"}else{"held"},if admitted {"admitted"}else{"unadmitted"}])?;
        Ok(())
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn source_kind_filters_match_existing_recovery_and_usage_projections() {
        const ADOPTED: &str = "00000000-0000-4000-8000-00000000000a";
        const FAILED: &str = "00000000-0000-4000-8000-0000000000ff";
        let root = tempfile::tempdir().unwrap();
        let (owner, control, server) = served(&root).await;
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','saved','/','','p','m','2026-01-01T00:00:00Z');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:03Z');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:03Z');
                INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('resume','context_changed','{}','t','{\"kind\":\"recovery_resumed\",\"change\":{\"run_id\":\"r\",\"turn_id\":\"u\"}}','2026-01-01T00:00:02Z');")?;
            tx.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('conversation','t',?1,'u',0,0,'p','m',0,'completed','{}','{\"input_tokens_details\":{\"cached_tokens\":17}}','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z')",[ADOPTED])?;
            let failure=json!({"error":crate::model::ModelError::new("provider","saved failure")}).to_string();
            tx.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES('conversation','t',?1,'u',0,0,'p','m',0,'failed','{}',?2,'2026-01-01T00:00:00Z','2026-01-01T00:00:01Z')",rusqlite::params![FAILED,failure])?;
            for (ordinal,id,kind,payload) in [
                (0,"actual-event","event",json!({"event_id":"resume"})),
                (1,"spoof-assistant","assistant_response",json!({"event_id":"resume","text":"paid assistant response"})),
                (2,"plain-assistant","assistant",json!({"text":"existing assistant kind"})),
            ] {
                crate::history::append_node(&tx,"t",id,Some("u"),kind,&payload,"2026-01-01T00:00:02Z")?;
                if kind!="event" {
                    tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,?2,'message','completed','{}',?3)",rusqlite::params![ADOPTED,ordinal,id])?;
                }
            }
            tx.commit()?;Ok(())
        }).await.unwrap();
        let observed = refresh(&control, &owner.loaded.data_dir, vec![], None).await;
        let (_, _, history, _) = observed.view.unwrap();
        let UiUpdate::History { nodes, .. } = *history else {
            panic!("selected history")
        };
        // Page projection follows the owner's configured UI language.
        let resumed = match control.store.config.ui.language {
            crate::config::UiLanguage::Ja => "復旧再開",
            crate::config::UiLanguage::En => "Resume",
        };
        let event = nodes
            .iter()
            .find(|node| node.node_id == "actual-event")
            .unwrap();
        assert!(
            event.presentation.title.starts_with(resumed),
            "{}",
            event.presentation.title
        );
        for id in ["spoof-assistant", "plain-assistant"] {
            let node = nodes.iter().find(|node| node.node_id == id).unwrap();
            assert!(!node.presentation.title.starts_with(resumed));
            assert!(
                !node
                    .presentation
                    .fields
                    .iter()
                    .any(|field| field.key.starts_with("/saved_model_failures/"))
            );
            assert!(
                node.presentation
                    .fields
                    .iter()
                    .any(|field| field.key == "/ui_model_attempt/cached_tokens"
                        && field.value == "17"),
                "both existing assistant projector kinds retain exact adopted usage"
            );
        }
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn selected_snapshot_reflects_persisted_index_pause_and_clear() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n[embedding]\nconnection='fixture'\nmodel='synthetic'\n").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let family = crate::digest::sha256_hex(
            b"fixture\0http://127.0.0.1:9/v1\0synthetic\0unicode-paragraph-v1",
        );
        let key = format!("index.pause.{family}");
        let saved_key = key.clone();
        owner.db.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','','/','','p','m','now')", [])?;
            tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2)", rusqlite::params![saved_key, r#"{"kind":"transport","message":"private provider text"}"#])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, dispatcher) = mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        let dispatcher = tokio::spawn(control.clone().serve(dispatcher));
        control.start().await.unwrap();
        let native = Owner {
            data_dir: &owner.loaded.data_dir,
            instance: &control.store.instance_id,
        };
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = activity::read_cancellable(&control.store.config.ui, &native, "t", cancelled)
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, crate::protocol::ErrorCode::InvalidState);
        for expected in [Some(crate::index::PauseReason::Transport), None] {
            let (entered, ready) = tokio::sync::oneshot::channel();
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            let pending = owner
                .db
                .try_call(move |_| {
                    let _ = entered.send(());
                    let _ = blocked.recv();
                    Ok(())
                })
                .unwrap();
            crate::test_watchdog::watchdog("initial activity writer held", ready)
                .await
                .unwrap()
                .unwrap();
            let initial = crate::test_watchdog::watchdog(
                "initial activity saved pause",
                activity::read_cancellable(
                    &control.store.config.ui,
                    &native,
                    "t",
                    CancellationToken::new(),
                ),
            )
            .await;
            drop(release);
            pending.receive().await.unwrap();
            assert_eq!(initial.unwrap().unwrap().snapshot.index_pause, expected);
            let (updates, _received) = std::sync::mpsc::sync_channel(16);
            let (sender, mut results) = mpsc::channel(8);
            let slots = Arc::new(Semaphore::new(1));
            let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
            crate::test_watchdog::watchdog(
                "selected index pause snapshot",
                run(
                    Query {
                        data_dir: owner.loaded.data_dir.clone(),
                        instance_id: control.store.instance_id.clone(),
                        thread: "t".into(),
                        ids: vec![],
                        generation: Some(1),
                        visible: true,
                        epoch: 0,
                        render_width: 80,
                        tool_scope: None,
                        known: Vec::new(),
                    },
                    control.store.config.ui.clone(),
                    updates,
                    sender,
                    CancellationToken::new(),
                    slots,
                    permit,
                ),
            )
            .await
            .unwrap();
            let mut observed = None;
            while let Ok(result) = results.try_recv() {
                if let BackgroundResult::Refreshed {
                    progress: Some(progress),
                    ..
                } = result
                {
                    observed = Some(progress.unwrap().snapshot.index_pause);
                }
            }
            assert_eq!(observed, Some(expected));
            let key = key.clone();
            owner
                .db
                .call(move |db| {
                    db.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        drop(control);
        server.shutdown().await.unwrap();
        dispatcher.await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn current_view_read_does_not_require_writer_admission() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','paired saved name','/','','p','m','2026-01-01T00:00:00Z')",[])?;
            tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z');")?;
            tx.execute_batch("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t-child','t','unowned descendant','/','','p','m','2026-01-01T00:00:00Z'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r-child','t-child','live','2026-01-01T00:00:00Z');")?;
            shellboy_fixture_input(&tx,"held-paired",false)?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, dispatcher) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        let dispatcher = tokio::spawn(control.clone().serve(dispatcher));
        // The first native request initializes Control via the writer. Hold
        // the writer only after readiness, as the embedded launcher does.
        control.start().await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let pending = owner
            .db
            .try_call(move |_| {
                let _ = entered.send(());
                let _ = blocked.recv();
                Ok(())
            })
            .unwrap();
        crate::test_watchdog::watchdog("E readonly observation", ready)
            .await
            .unwrap()
            .unwrap();
        let (updates, received) = std::sync::mpsc::sync_channel(16);
        let (sender, mut results) = mpsc::channel(8);
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let observed = crate::test_watchdog::watchdog(
            "E readonly observation",
            run(
                Query {
                    data_dir: owner.loaded.data_dir.clone(),
                    instance_id: control.store.instance_id.clone(),
                    thread: "t".into(),
                    ids: vec!["held-paired".into()],
                    generation: Some(3),
                    visible: true,
                    epoch: 0,
                    render_width: 80,
                    tool_scope: None,
                    known: Vec::new(),
                },
                control.store.config.ui.clone(),
                updates,
                sender,
                CancellationToken::new(),
                slots.clone(),
                permit,
            ),
        )
        .await;
        drop(release);
        pending.receive().await.unwrap();
        observed.unwrap();
        let mut receipts_seen = false;
        while let Ok(result) = results.try_recv() {
            if let BackgroundResult::Refreshed {
                receipts, progress, ..
            } = result
            {
                assert_eq!(
                    receipts.unwrap(),
                    [("held-paired".into(), ReceiptStatus::Held)]
                );
                assert!(progress.unwrap().is_ok());
                receipts_seen = true;
            }
        }
        assert!(receipts_seen);
        let view = received
            .try_iter()
            .find_map(|update| match update {
                UiUpdate::CurrentView {
                    details, history, ..
                } => Some((details, history)),
                _ => None,
            })
            .unwrap();
        assert_eq!(view.0.title, "paired saved name");
        let UiUpdate::History { pending, .. } = *view.1 else {
            panic!("paired history")
        };
        assert_eq!(pending.items[0].message_id, "held-paired");
        assert_eq!(slots.available_permits(), 1);
        server.shutdown().await.unwrap();
        dispatcher.await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_window_observation_does_not_require_writer_admission() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','paired saved name','/','','p','m','2026-01-01T00:00:00Z')",[])?;
            tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z');")?;
            tx.execute_batch("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t-child','t','unowned descendant','/','','p','m','2026-01-01T00:00:00Z'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r-child','t-child','live','2026-01-01T00:00:00Z');")?;
            shellboy_fixture_input(&tx,"held-paired",false)?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc, dispatcher) = tokio::sync::mpsc::channel(16);
        let server = crate::ipc::LocalServer::bind(&owner, rpc).await.unwrap();
        let dispatcher = tokio::spawn(control.clone().serve(dispatcher));
        // The first native request initializes Control via the writer. Hold
        // the writer only after readiness, as the embedded launcher does.
        control.start().await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let pending = owner
            .db
            .try_call(move |_| {
                let _ = entered.send(());
                let _ = blocked.recv();
                Ok(())
            })
            .unwrap();
        crate::test_watchdog::watchdog("E readonly observation", ready)
            .await
            .unwrap()
            .unwrap();
        let native_window = crate::test_watchdog::watchdog(
            "native selected observation bypasses the writer",
            crate::ipc::call_selected_operation(
                &owner.loaded.data_dir,
                &control.store.instance_id,
                Command::HistoryWindow(
                    serde_json::from_value(json!({
                        "thread_id": "t", "include_observation": true,
                    }))
                    .unwrap(),
                ),
                None,
            ),
        )
        .await;
        drop(release);
        pending.receive().await.unwrap();
        let response = native_window.unwrap().unwrap();
        assert!(response.ok, "{:?}", response.error);
        let window: crate::protocol::HistoryWindow =
            serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(window.thread_id, "t");
        let observation = window.observation.expect("opted-in observation");
        assert_eq!(observation.pending.total, 1);
        assert!(observation.pending.total_exact);
        assert_eq!(observation.pending.items[0].message_id, "held-paired");
        assert_eq!(observation.pending.items[0].created_at, "held-paired");
        assert_eq!(observation.pending.items[0].excerpt, "held-paired");
        assert!(!observation.pending.items[0].excerpt_truncated);
        assert_eq!(observation.pending.items[0].attachment_count, 0);
        let row = &observation.pending.items[0];
        assert_eq!(row.sender.kind, crate::protocol::SenderKind::ExternalCli);
        assert_eq!(row.sender.label.as_deref(), Some("human"));
        assert!(!row.expect_reply && !row.admission_uncertain && !row.completion_scheduled);
        assert_eq!(
            observation.pending.items[0].held_by,
            Some(crate::protocol::MessageHeldBy::Unknown)
        );
        assert_eq!(observation.activity.run_id.as_deref(), Some("r"));
        assert_eq!(observation.activity.turn_id.as_deref(), Some("u"));
        assert!(observation.committed_attempts.is_empty());
        owner.db.call(|db| {
            db.execute("UPDATE receipt SET path_admission='unknown',result_json='{\"deliver\":\"complete\",\"reply\":{\"state\":\"pending\"}}' WHERE message_node_id='held-paired'", [])?;
            Ok(())
        }).await.unwrap();
        let response = crate::ipc::call_selected_operation(
            &owner.loaded.data_dir,
            &control.store.instance_id,
            Command::HistoryWindow(
                serde_json::from_value(json!({"thread_id": "t", "include_observation": true}))
                    .unwrap(),
            ),
            None,
        )
        .await
        .unwrap();
        let window: crate::protocol::HistoryWindow =
            serde_json::from_value(response.result.unwrap()).unwrap();
        let row = &window.observation.unwrap().pending.items[0];
        assert!(row.expect_reply && row.admission_uncertain && row.completion_scheduled);
        // Saved hold reasons Core verified are typed holds, not `unknown`.
        for (reason, held_by) in [
            (
                "terminal_before_path_admission",
                crate::protocol::MessageHeldBy::TerminalBeforePathAdmission,
            ),
            (
                "previous_instance_interrupted",
                crate::protocol::MessageHeldBy::PreviousInstanceInterrupted,
            ),
            ("stopping", crate::protocol::MessageHeldBy::Stopping),
        ] {
            owner
                .db
                .call(move |db| {
                    db.execute(
                        "UPDATE receipt SET hold_reason=?1 WHERE message_node_id='held-paired'",
                        [reason],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            let response = crate::ipc::call_selected_operation(
                &owner.loaded.data_dir,
                &control.store.instance_id,
                Command::HistoryWindow(
                    serde_json::from_value(json!({"thread_id": "t", "include_observation": true}))
                        .unwrap(),
                ),
                None,
            )
            .await
            .unwrap();
            let window: crate::protocol::HistoryWindow =
                serde_json::from_value(response.result.unwrap()).unwrap();
            assert_eq!(
                window.observation.unwrap().pending.items[0].held_by,
                Some(held_by)
            );
        }

        server.shutdown().await.unwrap();
        dispatcher.await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn pending_automatic_title_does_not_claim_selected_path_admission() {
        let root = tempfile::tempdir().unwrap();
        let (owner, control, server) = served(&root).await;
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','','/','','p','m','now');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','now','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','now','now');")?;
            shellboy_fixture_input(&tx,"saved pending title",false)?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let history = |observed: Observed| {
            let (details, _, history, _) = observed.view.unwrap();
            let UiUpdate::History {
                nodes,
                pending,
                read_revision,
                ..
            } = *history
            else {
                panic!("selected history")
            };
            (details, nodes, pending, read_revision)
        };
        let (before, nodes, pending, before_sequence) =
            history(refresh(&control, &owner.loaded.data_dir, vec![], None).await);
        assert_eq!(before.title, "saved pending title");
        assert!(nodes.is_empty());
        assert_eq!(pending.items[0].message_id, "saved pending title");
        owner.db.call(|db| {
            let tx=db.transaction()?;
            assert_eq!(crate::history::append_existing_nodes(&tx,"t",0,&["saved pending title".into()],"now")?,1);
            tx.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id='saved pending title'",[])?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let (after, nodes, pending, after_sequence) =
            history(refresh(&control, &owner.loaded.data_dir, vec![], None).await);
        assert_eq!(after.title, "saved pending title");
        assert!(
            nodes
                .iter()
                .any(|node| node.role == "user" && node.text.contains("saved pending title"))
        );
        assert_eq!(pending.total, 0);
        assert!(after_sequence > before_sequence);
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
