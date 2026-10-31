//! One current-view snapshot on the existing finite read/progress admission.
use super::*;
use crate::storage::StorageError;
use history_reader::{HistoryInfo, RawHistory};
use rusqlite::Connection;

pub(super) struct Query {
    pub thread: String,
    pub ids: Vec<String>,
    pub generation: Option<u64>,
    pub visible: bool,
    pub epoch: u64,
    pub render_width: u16,
    pub tool_scope: Option<ToolObservation>,
    pub browser: Option<browser::Ticket>,
}

struct RawVisible {
    details: header::RawHeader,
    history: RawHistory,
    before_cursor: Result<Option<String>, ApiError>,
    sources: message_metadata::SourceFacts,
    tools: tools::ToolFacts,
    recovery: Vec<recovery::Evidence>,
    usage: Vec<usage::Observation>,
    received: Option<received::RawSnapshot>,
    activity: activity::RawProjection,
    tool_scope: Option<ToolObservation>,
    observed_at: std::time::Instant,
}
struct ProjectedVisible {
    details: ThreadDetails,
    nodes: Vec<UiNode>,
    info: HistoryInfo,
    before_cursor: Option<String>,
    tools: tools::ToolFacts,
    received: Option<received::Prepared>,
    activity: activity::Projection,
    tool_scope: Option<ToolObservation>,
}

fn read_visible_tx(
    db: &Connection,
    thread: &str,
    tool_scope: Option<ToolObservation>,
    waits: Option<&[(String, String)]>,
    sequence: u64,
) -> Result<RawVisible, StorageError> {
    if tool_scope
        .as_ref()
        .is_some_and(|scope| scope.nodes.len() > 128)
    {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Visible tool source scope exceeds its node bound",
        )
        .into());
    }
    let details = header::read_tx(db, thread)?;
    let (mut history, before_cursor) = history_reader::query_current_tx(db, thread)?;
    let page_ids = history.source_ids().map(str::to_owned).collect::<Vec<_>>();
    let mut sources = message_metadata::read_tx(db, Some(thread), &page_ids)?;
    let mut tools = tools::read_nodes_tx(db, &page_ids)?;
    if let Some(scope) = &tool_scope {
        let ids = scope
            .nodes
            .iter()
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>();
        sources.append(message_metadata::read_tx(db, Some(thread), &ids)?)?;
        tools.append(tools::read_nodes_tx(db, &ids)?);
    }
    let recovery = recovery::read_tx(db, &history.event_requests())?;
    let usage = usage::read_tx(db, &history.assistant_requests())?;
    let received = received::query_tx(
        db,
        thread,
        received::Scope {
            selection_revision: history.info().revision,
            view_revision: None,
        },
    )?;
    let activity = activity::read_tx_with_waits(db, thread, waits)?;
    // Admission order belongs to the whole paired observation, not read completion.
    history.set_read_revision(sequence);
    Ok(RawVisible {
        details,
        history,
        before_cursor,
        sources,
        tools,
        recovery,
        usage,
        received,
        activity,
        tool_scope,
        observed_at: std::time::Instant::now(),
    })
}

fn project(raw: RawVisible, width: u16) -> Result<ProjectedVisible, ApiError> {
    let RawVisible {
        details,
        history,
        before_cursor,
        sources,
        tools,
        recovery,
        usage,
        received,
        activity,
        mut tool_scope,
        observed_at,
    } = raw;
    let (mut nodes, info, _) = history.project_page(width, None);
    message_metadata::apply(&mut nodes, &sources);
    tools::apply(&mut nodes, &tools);
    recovery::apply(&mut nodes, recovery);
    usage::apply(&mut nodes, usage);
    if let Some(scope) = &mut tool_scope {
        message_metadata::apply(&mut scope.nodes, &sources);
        tools::apply(&mut scope.nodes, &tools);
        for node in &mut scope.nodes {
            node.presentation.read_revision = info.read_revision;
        }
    }
    let mut received = received.map(|snapshot| received::prepare(snapshot, width));
    if let Some(prepared) = &mut received {
        for (_, node) in &mut prepared.nodes {
            node.presentation.read_revision = info.read_revision;
        }
    }
    let mut activity = activity::prepare(activity);
    activity.observed_at = observed_at;
    Ok(ProjectedVisible {
        details: details.prepare(),
        nodes,
        info,
        before_cursor: before_cursor?,
        tools,
        received,
        activity,
        tool_scope,
    })
}

pub(super) async fn run(
    query: Query,
    control: Arc<Control>,
    updates: impl UpdateTarget + Send + 'static,
    sender: mpsc::Sender<BackgroundResult>,
    stop: CancellationToken,
    progress_slots: Arc<Semaphore>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let Query {
        thread,
        ids,
        generation,
        visible,
        epoch,
        render_width,
        tool_scope,
        mut browser,
    } = query;
    let _permit = permit;
    tokio::select! {biased;_=stop.cancelled()=>{},_=async {
        if !visible || generation.is_none() {
            let receipts=receipt_states(&control,ids).await;
            let _=sender.send(BackgroundResult::Refreshed {thread_id:thread,receipts,progress:None,epoch,generation}).await;
            return;
        }
        let generation=generation.expect("visible generation checked above");
        let tool_revision=tool_scope.as_ref().map(|scope|scope.revision);
        let Ok(_progress_permit)=progress_slots.acquire_owned().await else {
            let error=ApiError::new(ErrorCode::InvalidState,"Current-view progress admission is closed");
            let receipts=receipt_states(&control,ids).await;
            if let Some(revision)=tool_revision {
                update(&updates,UiUpdate::ToolFacts {thread_id:thread.clone(),generation,revision,result:Err(error.to_string())}).await;
            }
            if let Some(ticket)=browser.take() {
                let _=sender.send(BackgroundResult::BrowserPatch {ticket,result:Err(error.clone())}).await;
            }
            let _=sender.send(BackgroundResult::Refreshed {thread_id:thread,receipts,progress:Some(Err(error)),epoch,generation:Some(generation)}).await;
            return;
        };
        let target=thread.clone();
        let waits=control.pending_wait_sources();
        let browser_scope=browser.as_ref().map(|ticket|(ticket.ids.clone(),ticket.selected.clone(),ticket.stop.clone()));
        let read=snapshot::read_sequenced(
            control.store.database.path().to_path_buf(),stop.clone(),move |db,sequence| {
            let receipts=receipt_states_tx(db,&ids)?;
            // Keep typed per-surface errors until the display mapper can distinguish
            // retryable WAL contention from a true storage failure.
            let visible=read_visible_tx(db,&target,tool_scope,waits.as_deref(),sequence);
            let browser=browser_scope.map(|(ids,selected,stop)|{
                if stop.is_cancelled() {return Err(StorageError::from(ApiError::new(ErrorCode::InvalidState,"Browser observation closed")));}
                super::browser_read::read_tx(db,&ids,selected.as_deref())
            });
            Ok((receipts,visible,browser))
        }).await;
        drop(_progress_permit);
        let (receipts,visible,browser_result,sequence)=match checked_storage(&control,read).await {
            Ok(((receipts,visible,browser),sequence))=>(Ok(receipts),visible.map_err(snapshot::storage_error),browser.map(|result|result.map_err(snapshot::storage_error)),sequence),
            Err(error)=>(Err(error.clone()),Err(error.clone()),Some(Err(error)),0),
        };
        if let Some(ticket)=browser.take() {
            let result=match browser_result {
                Some(Ok(raw))=>super::browser::project(raw,sequence,ticket.generation).await,
                Some(Err(error))=>Err(error),
                None=>Err(ApiError::new(ErrorCode::InvalidState,"Browser observation unavailable")),
            };
            let result=checked_storage(&control,result).await;
            let _=sender.send(BackgroundResult::BrowserPatch {ticket,result}).await;
        }
        let result=async {
            let raw=checked_storage(&control,visible).await?;
            // Rendering, sender/model-error JSON, decoding and timestamp work
            // begin only after the readonly snapshot released its transaction.
            let mut frame=presentation_work(move || project(raw,render_width)).await??;
            let page_len=frame.nodes.len();
            if let Some(scope)=&mut frame.tool_scope {frame.nodes.append(&mut scope.nodes);}
            // Page and paused observations share one finite preview deadline.
            streams::enrich_previews_observed(&control,&mut frame.nodes,render_width,&frame.tools).await?;
            if let Some(scope)=&mut frame.tool_scope {scope.nodes=frame.nodes.split_off(page_len);}
            let received=frame.received.map(|received|Box::new(UiUpdate::IncompleteResponses {
                thread_id:thread.clone(),generation,selection_revision:received.selection_revision,
                view_revision:None,root_revision:received.root_revision,nodes:received.nodes,
            }));
            let info=frame.info;
            update(&updates,UiUpdate::CurrentView {
                generation,details:frame.details,
                history:Box::new(UiUpdate::History {
                    thread_id:thread.clone(),generation,nodes:frame.nodes,pending:info.pending,
                    read_revision:info.read_revision,before_cursor:frame.before_cursor,append:false,
                    first_ordinal:info.first,selection_revision:info.revision,total_nodes:info.total,
                }),
                received,tool_facts:frame.tool_scope,committed:info.committed,
            }).await;
            Ok(frame.activity)
        }.await;
        let result=checked_storage(&control,result).await;
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
    use crate::{
        app::DataOwner,
        presentation::{BlockKind, InputSource, NodePresentation, ToolPresentation},
    };
    use serde_json::json;

    #[test]
    fn oversized_paused_scope_is_rejected_before_any_unbounded_source_query() {
        let db = Connection::open_in_memory().unwrap();
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
        let error = read_visible_tx(
            &db,
            "not-read",
            Some(scope),
            Some(&[]),
            message_queue::read_revision(),
        )
        .err()
        .unwrap()
        .into_api_error();
        assert_eq!(error.code, ErrorCode::InputLimit);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn current_page_queue_paused_facts_and_activity_are_owned_one_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','before','/','','p','m','now');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','now','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','now','now');
                INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','now');
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
        let (raw, receipts) = owner
            .db
            .call(move |db| {
                let tx = db.transaction()?;
                let receipts = receipt_states_tx(&tx, &["queued".into()])?;
                let raw = read_visible_tx(
                    &tx,
                    "t",
                    Some(scope),
                    Some(&[]),
                    message_queue::read_revision(),
                )?;
                tx.commit()?;
                Ok((raw, receipts))
            })
            .await
            .unwrap();
        assert_eq!(receipts, [("queued".into(), ReceiptStatus::Held)]);
        owner.db.call(|db| {
            db.execute_batch("UPDATE thread SET title='later',model_id='later-model' WHERE id='t'; UPDATE receipt SET state='cancelled' WHERE message_node_id='queued';")?;Ok(())
        }).await.unwrap();
        let frame = presentation_work(move || project(raw, 80))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.details.title, "before");
        assert_eq!(frame.details.model, "m");
        assert_eq!(frame.info.pending.total, 1);
        assert_eq!(frame.info.pending.items[0].message_id, "queued");
        assert!(frame.info.read_revision > 0);
        assert!(
            frame
                .nodes
                .iter()
                .all(|node| node.presentation.read_revision == frame.info.read_revision)
        );
        let input = frame
            .nodes
            .iter()
            .find(|node| node.node_id == "input")
            .unwrap();
        assert_eq!(
            input.presentation.input_source,
            InputSource::Unknown,
            "explicit CLI label does not confer human-band authority"
        );
        let scope = frame.tool_scope.unwrap();
        assert_eq!((scope.revision, scope.view_revision), (7, 3));
        assert_eq!(
            scope.nodes[0].presentation.read_revision,
            frame.info.read_revision
        );
        assert_eq!(
            scope.nodes[0].presentation.blocks[0]
                .tool
                .as_ref()
                .unwrap()
                .group_id
                .as_deref(),
            Some("g")
        );
        assert!(frame.received.unwrap().nodes.is_empty());
        assert_eq!(
            frame.activity.snapshot.phase,
            crate::activity::ActivityPhase::Completed
        );
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
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let raw=owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','saved','/','','p','m','2026-01-01T00:00:00Z');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:03Z');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:03Z');
                INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','{\"input_tokens_details\":{\"cached_tokens\":17}}','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z');
                INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('resume','context_changed','{}','t','{\"kind\":\"recovery_resumed\",\"change\":{\"run_id\":\"r\",\"turn_id\":\"u\"}}','2026-01-01T00:00:02Z');")?;
            let failure=json!({"error":crate::model::ModelError::new("provider","saved failure")}).to_string();
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES('failed','u',0,0,'p','m',0,'failed','{}',?1,'2026-01-01T00:00:00Z','2026-01-01T00:00:01Z')",[failure])?;
            for (ordinal,id,kind,payload) in [
                (0,"actual-event","event",json!({"event_id":"resume"})),
                (1,"spoof-assistant","assistant_response",json!({"event_id":"resume","text":"paid assistant response"})),
                (2,"plain-assistant","assistant",json!({"text":"existing assistant kind"})),
            ] {
                crate::history::append_node(&tx,"t",id,Some("u"),kind,&payload,"2026-01-01T00:00:02Z")?;
                if kind!="event" {
                    tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('a',?1,'message','completed','{}',?2)",rusqlite::params![ordinal,id])?;
                }
            }
            let raw=read_visible_tx(&tx,"t",None,Some(&[]),message_queue::read_revision())?;
            assert_eq!(raw.recovery.len(),1,"a non-event carrying event_id is not a recovery observation");
            tx.commit()?;Ok(raw)
        }).await.unwrap();
        let frame = presentation_work(move || project(raw, 80))
            .await
            .unwrap()
            .unwrap();
        let event = frame
            .nodes
            .iter()
            .find(|node| node.node_id == "actual-event")
            .unwrap();
        assert!(event.presentation.title.starts_with("復旧再開"));
        for id in ["spoof-assistant", "plain-assistant"] {
            let node = frame.nodes.iter().find(|node| node.node_id == id).unwrap();
            assert!(!node.presentation.title.starts_with("復旧再開"));
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
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_combined_view_and_browser_keep_one_sequence_without_writer_admission() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','paired saved name','/','','p','m','2026-01-01T00:00:00Z')",[])?;
            tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z');")?;
            shellboy_fixture_input(&tx,"held-paired",false)?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
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
        tokio::time::timeout(Duration::from_secs(60), ready)
            .await
            .unwrap()
            .unwrap();
        let mut browser = browser::Browser::default();
        browser
            .observe(7, "".into(), Some("t".into()), true)
            .unwrap();
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let (updates, received) = std::sync::mpsc::sync_channel(16);
        let (sender, mut results) = mpsc::channel(8);
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let observed = tokio::time::timeout(
            Duration::from_secs(60),
            run(
                Query {
                    thread: "t".into(),
                    ids: vec!["held-paired".into()],
                    generation: Some(3),
                    visible: true,
                    epoch: 0,
                    render_width: 80,
                    tool_scope: None,
                    browser: Some(ticket),
                },
                control.clone(),
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
        let mut browser_sequence = None;
        let mut receipts_seen = false;
        while let Ok(result) = results.try_recv() {
            match result {
                BackgroundResult::BrowserPatch { result, .. } => {
                    browser_sequence = Some(result.unwrap().read_sequence)
                }
                BackgroundResult::Refreshed {
                    receipts, progress, ..
                } => {
                    assert_eq!(
                        receipts.unwrap(),
                        [("held-paired".into(), ReceiptStatus::Held)]
                    );
                    assert!(progress.unwrap().is_ok());
                    receipts_seen = true;
                }
                _ => {}
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
        let UiUpdate::History {
            pending,
            read_revision,
            ..
        } = *view.1
        else {
            panic!("paired history")
        };
        assert_eq!(Some(read_revision), browser_sequence);
        assert_eq!(pending.items[0].message_id, "held-paired");
        assert_eq!(slots.available_permits(), 1);
        drop(control);
        owner.shutdown().await.unwrap();
    }
}
