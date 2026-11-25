//! One current-view snapshot on the existing finite read/progress admission.
use super::*;
use crate::storage::StorageError;
use history_reader::{HistoryInfo, RawHistory};
use rusqlite::Connection;

pub(super) struct Query {
    pub data_dir: std::path::PathBuf,
    pub thread: String,
    pub ids: Vec<String>,
    pub generation: Option<u64>,
    pub visible: bool,
    pub epoch: u64,
    pub render_width: u16,
    pub language: crate::config::UiLanguage,
    pub tool_scope: Option<ToolObservation>,
    pub views: view_read::Tickets,
}

struct RawVisible {
    details: Arc<header::RawHeader>,
    history: RawHistory,
    before_cursor: Result<Option<String>, ApiError>,
    sources: message_metadata::SourceFacts,
    tools: tools::ToolFacts,
    recovery: Vec<recovery::Evidence>,
    usage: Vec<usage::Observation>,
    received: Option<received::RawSnapshot>,
    activity: activity::RawProjection,
    index_pause: Option<crate::index::PauseReason>,
    context_usage: crate::ui::ContextUsage,
    tool_scope: Option<ToolObservation>,
    observed_at: std::time::Instant,
}
struct ProjectedVisible {
    details: ThreadDetails,
    nodes: Vec<UiNode>,
    info: HistoryInfo,
    before_cursor: Option<String>,
    received: Option<received::Prepared>,
    activity: activity::Projection,
    context_usage: crate::ui::ContextUsage,
    tool_scope: Option<ToolObservation>,
}

#[cfg(test)]
fn read_visible_tx(
    db: &Connection,
    thread: &str,
    tool_scope: Option<ToolObservation>,
    waits: Option<&[(String, String)]>,
    sequence: u64,
) -> Result<RawVisible, StorageError> {
    read_visible_tx_limited(
        db,
        thread,
        tool_scope,
        waits,
        sequence,
        3,
        header::read_tx(db, thread).map(Arc::new),
    )
}

fn read_visible_tx_limited(
    db: &Connection,
    thread: &str,
    tool_scope: Option<ToolObservation>,
    waits: Option<&[(String, String)]>,
    sequence: u64,
    limit: usize,
    details: Result<Arc<header::RawHeader>, StorageError>,
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
    let details = details?;
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
    let activity = activity::read_tx_with_limit(db, thread, waits, limit)?;
    // Existing bounded display read; a limit error is an unknown value, not a failed view.
    let context_usage =
        match crate::compaction::context_usage::read_context_usage_tx(db, thread, Some(512)) {
            Ok(raw) => raw.reported_usage().into(),
            Err(StorageError::Operation(_)) => Default::default(),
            Err(error) => return Err(error),
        };
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
        context_usage,
        tool_scope,
        index_pause: None,
        observed_at: std::time::Instant::now(),
    })
}

fn project(
    raw: RawVisible,
    width: u16,
    language: crate::config::UiLanguage,
) -> Result<ProjectedVisible, ApiError> {
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
        context_usage,
        mut tool_scope,
        index_pause,
        observed_at,
    } = raw;
    let (mut nodes, info, _) = history.project_page(width, None, language);
    message_metadata::apply(&mut nodes, &sources);
    tools::apply(&mut nodes, &tools);
    recovery::apply(&mut nodes, recovery, language);
    usage::apply(&mut nodes, usage);
    if let Some(scope) = &mut tool_scope {
        message_metadata::apply(&mut scope.nodes, &sources);
        tools::apply(&mut scope.nodes, &tools);
        for node in &mut scope.nodes {
            node.presentation.read_revision = info.read_revision;
        }
    }
    let mut received = received.map(|snapshot| received::prepare(snapshot, width, language));
    if let Some(prepared) = &mut received {
        for (_, node) in &mut prepared.nodes {
            node.presentation.read_revision = info.read_revision;
        }
    }
    let mut activity = activity::prepare(activity);
    activity.snapshot.index_pause = index_pause;
    activity.observed_at = observed_at;
    Ok(ProjectedVisible {
        details: details.prepare_for_ui(language),
        nodes,
        info,
        before_cursor: before_cursor?,
        received,
        activity,
        context_usage,
        tool_scope,
    })
}

type RawCurrentAndViews = (
    Vec<(String, ReceiptStatus)>,
    Result<RawVisible, StorageError>,
    Option<Result<view_read::RawBatch, StorageError>>,
);

struct VisibleReadScope<'a> {
    tool_scope: Option<ToolObservation>,
    waits: Option<&'a [(String, String)]>,
    limit: usize,
}

// Production coupled collector: all consumers use this caller's connection and
// read sequence; the same-Thread inspector borrows the already-read header Arc.
#[cfg(test)]
fn read_current_and_views_tx(
    db: &Connection,
    thread: &str,
    ids: &[String],
    tool_scope: Option<ToolObservation>,
    waits: Option<&[(String, String)]>,
    sequence: u64,
    views: &view_read::Tickets,
) -> Result<RawCurrentAndViews, StorageError> {
    read_current_and_views_tx_limited(
        db,
        thread,
        ids,
        VisibleReadScope {
            tool_scope,
            waits,
            limit: 3,
        },
        sequence,
        views,
        header::read_tx(db, thread).map(Arc::new),
    )
}

fn read_current_and_views_tx_limited(
    db: &Connection,
    thread: &str,
    ids: &[String],
    scope: VisibleReadScope<'_>,
    sequence: u64,
    views: &view_read::Tickets,
    details: Result<Arc<header::RawHeader>, StorageError>,
) -> Result<RawCurrentAndViews, StorageError> {
    let VisibleReadScope {
        tool_scope,
        waits,
        limit,
    } = scope;
    let receipts = receipt_states_tx(db, ids)?;
    let visible = read_visible_tx_limited(db, thread, tool_scope, waits, sequence, limit, details);
    let facts = if views.is_empty() {
        None
    } else {
        Some(view_read::read_shared_tx(
            db,
            views,
            visible.as_ref().ok().map(|raw| &raw.details),
        ))
    };
    Ok((receipts, visible, facts))
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
        data_dir,
        thread,
        ids,
        generation,
        visible,
        epoch,
        render_width,
        language,
        tool_scope,
        views,
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
            if !views.is_empty() {let _=sender.send(BackgroundResult::ViewFacts {tickets:views.clone(),result:Err(error.clone())}).await;}
            let _=sender.send(BackgroundResult::Refreshed {thread_id:thread,receipts,progress:Some(Err(error)),epoch,generation:Some(generation)}).await;
            return;
        };
        let target=thread.clone();
        let waits=control.pending_wait_sources();
        let limit=control.store.config.ui.live_work_rows;
        let view_scopes=super::view_read::prepare_requests(&views).await;
        let index=control.index.clone();
        let holds=control.message_holds();
        let details = super::native_header(&data_dir, &thread).await.map(Arc::new).map_err(StorageError::from);
        let read=snapshot::read_sequenced(
            control.store.database.path().to_path_buf(),stop.clone(),move |db,sequence| {
            // Keep typed per-surface failures; do not reinterpret WAL contention.
            let (receipts, visible, views) = read_current_and_views_tx_limited(db,&target,&ids,VisibleReadScope { tool_scope, waits: waits.as_deref(), limit },sequence,&view_scopes,details)?;
            let visible = visible.and_then(|mut raw| {
                raw.history.project_message_holds(db, &holds)?;
                raw.index_pause = index.read_display_pause(db)?;
                Ok(raw)
            });
            Ok((receipts, visible, views))
        }).await;
        drop(_progress_permit);
        let (receipts,visible,browser_result,sequence)=match checked_storage(&control,read).await {
            Ok(((receipts,visible,browser),sequence))=>(Ok(receipts),visible.map_err(snapshot::storage_error),browser.map(|result|result.map_err(snapshot::storage_error)),sequence),
            Err(error)=>(Err(error.clone()),Err(error.clone()),Some(Err(error)),0),
        };
        let result=async {
            let raw=checked_storage(&control,visible).await?;
            // Rendering, sender/model-error JSON, decoding and timestamp work
            // begin only after the readonly snapshot released its transaction.
            let mut frame=presentation_work(move || project(raw,render_width,language)).await??;
            let page_len=frame.nodes.len();
            if let Some(scope)=&mut frame.tool_scope {frame.nodes.append(&mut scope.nodes);}
            // Raw previews are owned by visible UI items, not every Changed snapshot.
            streams::seal_nodes(&mut frame.nodes).await?;
            if let Some(scope)=&mut frame.tool_scope {scope.nodes=frame.nodes.split_off(page_len);}
            let received=frame.received.map(|received|Box::new(UiUpdate::IncompleteResponses {
                thread_id:thread.clone(),generation,selection_revision:received.selection_revision,
                view_revision:None,root_revision:received.root_revision,nodes:received.nodes,
            }));
            let info=frame.info;
            update(&updates,UiUpdate::CurrentView {
                generation,details:frame.details,context_usage:frame.context_usage,
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
        // Sidebar wire reads have their own tickets. They cannot gate delivery
        // of the selected conversation or release of its refresh slot.
        if !views.is_empty() {
            let result=match browser_result {
                Some(Ok(raw))=>{
                    let language=control.store.config.ui.language;
                    super::view_read::prepare_native(&control,raw,sequence,&views,language).await.map(|prepared|(prepared,sequence))
                },
                Some(Err(error))=>Err(error),
                None=>Err(ApiError::new(ErrorCode::InvalidState,"Visible observations unavailable")),
            };
            let result=checked_storage(&control,result).await;
            let _=sender.send(BackgroundResult::ViewFacts {tickets:views,result:result.map(Box::new)}).await;
        }
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
        let old = crate::frontend::read_window_facts(&tx, &window.items).unwrap();
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
        let fresh = crate::frontend::read_window_facts(&read, &later.items).unwrap();
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
        drop(read);
        owner.shutdown().await.unwrap();
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
        let (raw, receipts, admitted_sequence) = owner
            .db
            .call(move |db| {
                let tx = db.transaction()?;
                let receipts = receipt_states_tx(&tx, &["queued".into()])?;
                let admitted_sequence = message_queue::read_revision();
                let raw = read_visible_tx(&tx, "t", Some(scope), Some(&[]), admitted_sequence)?;
                tx.commit()?;
                Ok((raw, receipts, admitted_sequence))
            })
            .await
            .unwrap();
        assert_eq!(receipts, [("queued".into(), ReceiptStatus::Held)]);
        owner.db.call(|db| {
            db.execute_batch("UPDATE thread SET title='later',model_id='later-model' WHERE id='t'; UPDATE receipt SET state='cancelled' WHERE message_node_id='queued';")?;Ok(())
        }).await.unwrap();
        let frame = presentation_work(move || project(raw, 80, crate::config::UiLanguage::Ja))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.details.title, "before");
        assert_eq!(frame.details.model, "m");
        assert_eq!(
            frame.context_usage,
            crate::ui::ContextUsage {
                input_tokens: Some(140),
                cached_tokens: Some(120),
                output_tokens: Some(20)
            }
        );
        assert_eq!(frame.info.pending.total, 1);
        assert_eq!(frame.info.pending.items[0].message_id, "queued");
        assert!(admitted_sequence > 0);
        assert_eq!(frame.info.read_revision, admitted_sequence);
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
                INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'p','m',0,'completed','{}','{\"input_tokens_details\":{\"cached_tokens\":17}}','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z');
                INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('resume','context_changed','{}','t','{\"kind\":\"recovery_resumed\",\"change\":{\"run_id\":\"r\",\"turn_id\":\"u\"}}','2026-01-01T00:00:02Z');")?;
            let failure=json!({"error":crate::model::ModelError::new("provider","saved failure")}).to_string();
            tx.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'failed','u',0,0,'p','m',0,'failed','{}',?1,'2026-01-01T00:00:00Z','2026-01-01T00:00:01Z')",[failure])?;
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
        let frame = presentation_work(move || project(raw, 80, crate::config::UiLanguage::Ja))
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
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = activity::read_cancellable(&control, "t", cancelled)
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
                activity::read_cancellable(&control, "t", CancellationToken::new()),
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
                        thread: "t".into(),
                        ids: vec![],
                        generation: Some(1),
                        visible: true,
                        epoch: 0,
                        render_width: 80,
                        language: crate::config::UiLanguage::Ja,
                        tool_scope: None,
                        views: view_read::Tickets::default(),
                    },
                    control.clone(),
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
    async fn actual_combined_view_and_browser_keep_one_sequence_without_writer_admission() {
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
        let mut browser = browser::Browser::default();
        browser
            .observe(7, "".into(), Some("t".into()), true)
            .unwrap();
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let mut descendants = lineage_observer::Observer::default();
        descendants.observe(9, vec!["t".into()]).unwrap();
        let descendant_ticket = descendants.take(tokio::time::Instant::now()).unwrap();
        let (updates, received) = std::sync::mpsc::sync_channel(16);
        let (sender, mut results) = mpsc::channel(8);
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let observed = crate::test_watchdog::watchdog(
            "E readonly observation",
            run(
                Query {
                    data_dir: owner.loaded.data_dir.clone(),
                    thread: "t".into(),
                    ids: vec!["held-paired".into()],
                    generation: Some(3),
                    visible: true,
                    epoch: 0,
                    render_width: 80,
                    language: crate::config::UiLanguage::Ja,
                    tool_scope: None,
                    views: view_read::Tickets {
                        browser: Some(ticket),
                        descendants: Some(descendant_ticket),
                        ..Default::default()
                    },
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
                BackgroundResult::ViewFacts { result, .. } => {
                    assert!(
                        receipts_seen,
                        "selected refresh must finish before sidebar readback"
                    );
                    let (prepared, sequence) = *result.unwrap();
                    let patch = prepared.browser.unwrap().unwrap().patch.unwrap();
                    assert_eq!(patch.read_sequence, sequence);
                    let observed = prepared.descendants.unwrap().unwrap();
                    assert_eq!(observed.queried_roots, 1);
                    assert_eq!(observed.snapshots.len(), 1);
                    assert_eq!(observed.snapshots[0].root, "t");
                    assert_eq!(observed.snapshots[0].threads, ["t-child"]);
                    assert_eq!(
                        (observed.snapshots[0].active, observed.snapshots[0].recovery),
                        (1, 0)
                    );
                    browser_sequence = Some(sequence);
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
        assert_eq!(
            observation.pending.items[0].held_by,
            Some(crate::protocol::MessageHeldBy::Unknown)
        );
        assert_eq!(observation.activity.run_id.as_deref(), Some("r"));
        assert_eq!(observation.activity.turn_id.as_deref(), Some("u"));
        assert!(observation.committed_attempts.is_empty());

        server.shutdown().await.unwrap();
        dispatcher.await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn pending_automatic_title_does_not_claim_selected_path_admission() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','','/','','p','m','now');
                INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','now','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','now','now');")?;
            shellboy_fixture_input(&tx,"saved pending title",false)?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let path = owner.db.path().to_path_buf();
        let (before, before_sequence) =
            snapshot::read_sequenced(path.clone(), CancellationToken::new(), |db, sequence| {
                read_visible_tx(db, "t", None, Some(&[]), sequence)
            })
            .await
            .unwrap();
        owner.db.call(|db| {
            let tx=db.transaction()?;
            assert_eq!(crate::history::append_existing_nodes(&tx,"t",0,&["saved pending title".into()],"now")?,1);
            tx.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id='saved pending title'",[])?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let (after, after_sequence) =
            snapshot::read_sequenced(path, CancellationToken::new(), |db, sequence| {
                read_visible_tx(db, "t", None, Some(&[]), sequence)
            })
            .await
            .unwrap();
        let before = project(before, 80, crate::config::UiLanguage::Ja).unwrap();
        assert_eq!(before.details.title, "saved pending title");
        assert!(before.nodes.is_empty());
        assert_eq!(
            before.info.pending.items[0].message_id,
            "saved pending title"
        );
        let after = project(after, 80, crate::config::UiLanguage::Ja).unwrap();
        assert_eq!(after.details.title, "saved pending title");
        assert!(
            after
                .nodes
                .iter()
                .any(|node| node.role == "user" && node.text.contains("saved pending title"))
        );
        assert_eq!(after.info.pending.total, 0);
        assert!(after_sequence > before_sequence);
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn p3_coupled_snapshot_reuses_header_sequence_and_survives_cancelled_consumers() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        owner.db.call(|db| {
            db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','coupled title','/','','p','m','now');
                INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('child','t','child title','/','','p','m','now');
                INSERT INTO run(id,thread_id,state,created_at)VALUES('child-run','child','live','now');")?;
            Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let mut browser = browser::Browser::default();
        browser
            .observe(7, String::new(), Some("t".into()), true)
            .unwrap();
        let mut descendants = lineage_observer::Observer::default();
        descendants.observe(9, vec!["t".into()]).unwrap();
        let mut inspection = inspection_state::Observation::default();
        let target = crate::ui::InspectionTarget::Thread("t".into());
        let context = crate::ui::ManagementContext {
            epoch: 11,
            owner_draft: "synthetic".into(),
            owner_thread: Some("t".into()),
        };
        inspection.install(11, context.clone(), target.clone(), None);
        inspection.observe(&context, &target, true);
        let at = tokio::time::Instant::now();
        let tickets = view_read::Tickets {
            browser: Some(browser.take(at).unwrap()),
            descendants: Some(descendants.take(at).unwrap()),
            inspection: Some(inspection.take_metadata(at).unwrap()),
            inspection_services: Some(inspection_read::Services::new(&control)),
            ..Default::default()
        };
        let collect = tickets.clone();
        let ((visible, facts), sequence) = snapshot::read_sequenced(
            owner.db.path().to_path_buf(),
            CancellationToken::new(),
            move |db, sequence| {
                assert!(!db.is_autocommit());
                assert_eq!(
                    db.pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))?,
                    1
                );
                let (_, visible, facts) =
                    read_current_and_views_tx(db, "t", &[], None, Some(&[]), sequence, &collect)?;
                Ok((
                    visible?,
                    facts.expect("visible tickets").expect("shared collection"),
                ))
            },
        )
        .await
        .unwrap();
        let frame = project(visible, 80, crate::config::UiLanguage::Ja).unwrap();
        assert_eq!(frame.details.title, "coupled title");
        assert_eq!(frame.info.read_revision, sequence);
        let prepared = view_read::prepare(facts, sequence, &tickets, crate::config::UiLanguage::Ja);
        let (updates, received) = std::sync::mpsc::sync_channel(16);
        view_read::complete(
            control.diagnostics(),
            &mut browser,
            &mut descendants,
            &mut inspection,
            &updates,
            tickets.clone(),
            Ok((prepared, sequence)),
        )
        .await;
        let presentation = inspection.take_presentation().unwrap();
        assert_eq!(presentation.metadata.sequence, sequence);
        assert_eq!(presentation.metadata.value["title"], "coupled title");
        assert!(received.try_iter().any(|update|matches!(update,UiUpdate::BrowserPatch{patch,..} if patch.read_sequence==sequence)));
        let descendant = descendants.snapshots().next().unwrap();
        assert_eq!(descendant.root, "t");
        assert_eq!(descendant.threads, ["child"]);
        assert_eq!(descendant.active, 1);
        tickets.browser.as_ref().unwrap().stop.cancel();
        tickets.descendants.as_ref().unwrap().stop.cancel();
        tickets.inspection.as_ref().unwrap().stop.cancel();
        let (_, later_sequence) = snapshot::read_sequenced(
            owner.db.path().to_path_buf(),
            CancellationToken::new(),
            move |db, sequence| {
                let (_, visible, facts) =
                    read_current_and_views_tx(db, "t", &[], None, Some(&[]), sequence, &tickets)?;
                assert!(visible.is_ok() && facts.is_some());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert!(later_sequence > sequence);
        drop(control);
        owner.shutdown().await.unwrap();
    }
}
