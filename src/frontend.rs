//! Async TUI adapter. Files, clipboard, DB, and preparation never run on the UI.
pub mod activity;
mod admission;
use admission::{spawn_read, spawn_work};
mod draft_images;
mod draft_saver;
mod history_reader;
mod input_history;
mod message_metadata;
mod message_queue;
mod receipt_delivery;
mod received;
mod thread_deletion;
mod update_delivery;
use history_reader::{HistoryReader, Reply, WindowReply, WindowRequest};
use update_delivery::{BufferedUpdates, UpdateTarget};
mod management;
mod management_state;
mod recovery;
mod refresh;
mod selection;
pub mod startup;
#[cfg(test)]
pub(crate) mod stream_metrics;
mod streaming;
mod streams;
#[cfg(test)]
use streaming::StreamBody;
mod tools;
mod usage;

use crate::{
    app::DataOwner,
    attachments::{
        AttachmentInput, ManagedImage,
        clipboard::{ClipboardAdapter, ClipboardPixels, CopyOutcome},
    },
    config::CopyBackend,
    control::{Control, EngineEvent},
    diagnostics::{DiagnosticIds, Diagnostics, MessageSendStage, MessageSendTrace},
    ipc::LocalServer,
    protocol::*,
    ui::*,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, OnceLock, mpsc::SyncSender},
};
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
    time::{Duration, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

pub type ClipboardFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;
/// The native adapter is injectable so tests never modify the host clipboard.
pub trait ClipboardBackend: Send + Sync {
    fn capture(
        &self,
        timeout_ms: u64,
        cancel: CancellationToken,
    ) -> ClipboardFuture<'_, ClipboardPixels>;
    fn copy(
        &self,
        text: String,
        backend: CopyBackend,
        max_bytes: usize,
        timeout_ms: u64,
        cancel: CancellationToken,
    ) -> ClipboardFuture<'_, CopyOutcome>;
}
impl ClipboardBackend for ClipboardAdapter {
    fn capture(
        &self,
        timeout_ms: u64,
        cancel: CancellationToken,
    ) -> ClipboardFuture<'_, ClipboardPixels> {
        Box::pin(ClipboardAdapter::capture(self, timeout_ms, cancel))
    }
    fn copy(
        &self,
        text: String,
        backend: CopyBackend,
        max_bytes: usize,
        timeout_ms: u64,
        cancel: CancellationToken,
    ) -> ClipboardFuture<'_, CopyOutcome> {
        Box::pin(self.copy_text(text, backend, max_bytes, timeout_ms, cancel))
    }
}
async fn update(sender: &(impl UpdateTarget + ?Sized), mut value: UiUpdate) {
    loop {
        let Some(next) = sender.defer(value) else {
            return;
        };
        value = next;
        sender.flush_pending();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn operation(control: &Control, command: Command) -> Request {
    Request::new(
        format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
        command,
    )
}
async fn request(control: &Arc<Control>, command: Command) -> Result<Value, ApiError> {
    let response = control.request(operation(control, command)).await;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(response.error.unwrap_or_else(|| {
            ApiError::new(ErrorCode::IoFailed, "Operation failed without details")
        }))
    }
}
async fn management_request(
    control: &Arc<Control>,
    command: Command,
    cancel: CancellationToken,
) -> Result<Value, ApiError> {
    management_request_trigger(
        control,
        command,
        cancel,
        crate::usage_totals::UsageTrigger::Open,
    )
    .await
}
async fn management_request_trigger(
    control: &Arc<Control>,
    command: Command,
    cancel: CancellationToken,
    trigger: crate::usage_totals::UsageTrigger,
) -> Result<Value, ApiError> {
    let is_read_projection = !command.requires_receipt();
    let index_args = if let Command::IndexStatus(args) = &command {
        Some(args.clone())
    } else {
        None
    };
    let response = crate::usage_totals::with_trigger(
        trigger,
        control.request_cancellable(operation(control, command), cancel),
    )
    .await;
    let operation_id = response.operation_id;
    let mut result = if response.ok {
        response.result.unwrap_or(Value::Null)
    } else {
        if is_read_projection {
            return Ok(
                serde_json::json!({"error":response.error.unwrap_or_else(||ApiError::new(ErrorCode::IoFailed,"Operation failed without details"))}),
            );
        }
        return Ok(
            serde_json::json!({"request_operation_id":operation_id,"error":response.error.unwrap_or_else(|| ApiError::new(ErrorCode::IoFailed,"Operation failed without details"))}),
        );
    };
    if !result.is_object() {
        result = serde_json::json!({"items":result});
    }
    if !is_read_projection && let Value::Object(object) = &mut result {
        object.insert("request_operation_id".into(), Value::String(operation_id));
    }
    if let Some(args) = index_args {
        result["management"] = control.index.management_status(args).await?;
    }
    Ok(result)
}
async fn management_update(
    sender: &(impl UpdateTarget + ?Sized),
    context: Option<ManagementContext>,
    value: UiUpdate,
) {
    update(
        sender,
        match context {
            Some(context) => UiUpdate::ScopedManagement {
                context,
                update: Box::new(value),
            },
            None => value,
        },
    )
    .await;
}
async fn management_status(
    sender: &(impl UpdateTarget + ?Sized),
    context: Option<ManagementContext>,
    text: impl ToString,
) {
    management_update(
        sender,
        context,
        UiUpdate::Status {
            thread_id: None,
            text: text.to_string(),
        },
    )
    .await;
}
async fn status(
    sender: &(impl UpdateTarget + ?Sized),
    thread_id: Option<String>,
    text: impl std::fmt::Display,
) {
    update(
        sender,
        UiUpdate::Status {
            thread_id,
            text: text.to_string(),
        },
    )
    .await;
}
fn tool_group_notice(
    release: Option<(&str, crate::control::ForegroundRelease)>,
    cancelled_calls: usize,
) -> String {
    let remaining = if cancelled_calls == 0 {
        "後続の未実行呼出しはありません".to_owned()
    } else {
        format!("未実行のツール呼出しを{cancelled_calls}件取り消しました")
    };
    match release {
        Some((execution, reason)) => {
            let cause = match reason {
                crate::control::ForegroundRelease::Background => "人間の操作",
                crate::control::ForegroundRelease::AdditionalInput => "追加指示",
            };
            format!("{cause}でshell {execution}の待機を解除し背景へ移しました。{remaining}")
        }
        None => remaining,
    }
}

fn trace_send(
    diagnostics: &Diagnostics,
    trace: &MessageSendTrace,
    stage: MessageSendStage,
    thread_id: Option<&str>,
    value: Option<&Value>,
) {
    diagnostics.record_message_send(
        DiagnosticIds {
            thread_id: value
                .and_then(|value| value.get("thread_id"))
                .and_then(Value::as_str)
                .or(thread_id)
                .map(str::to_owned),
            ..Default::default()
        },
        MessageSendTrace {
            stage,
            message_id: value
                .and_then(|value| value.get("message_id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..trace.clone()
        },
    );
}
enum BackgroundResult {
    ThreadDeletion {
        request_id: u64,
        args: ThreadDeleteArgs,
        execute: bool,
        known_thread_ids: Vec<String>,
        result: Result<thread_deletion::Outcome, ApiError>,
    },
    StreamFrame {
        ticket: streaming::Ticket,
        result: Result<streaming::Outcome, ApiError>,
    },
    DraftSaved {
        thread_id: String,
        result: Result<(), ApiError>,
    },
    Refreshed {
        thread_id: String,
        receipts: Result<Vec<(String, ReceiptStatus)>, ApiError>,
        progress: Option<Result<activity::Projection, ApiError>>,
        epoch: u64,
        generation: Option<u64>,
    },
    Progress {
        thread_id: String,
        epoch: u64,
        generation: u64,
        result: Result<activity::Projection, ApiError>,
    },
    Sent {
        generation: u64,
        projection: Option<Box<UiUpdate>>,
        draft_id: String,
        revision: u64,
        attachments: HashSet<String>,
        result: Result<Value, ApiError>,
    },
    Image {
        ticket: u64,
        draft_id: String,
        revision: u64,
        position: usize,
        error_reported: bool,
        result: Result<ManagedImage, ApiError>,
    },
    Copied {
        scope: CopyScope,
        result: Result<CopyOutcome, ApiError>,
    },
    Command {
        work_status: bool,
        context: Option<ManagementContext>,
        generation: u64,
        title: String,
        result: Result<management::Report, ApiError>,
    },
    UsageRefresh {
        context: ManagementContext,
        generation: u64,
        title: String,
        result: Result<management::Report, ApiError>,
    },
    IndexPreview {
        context: Option<ManagementContext>,
        args: IndexArgs,
        result: Result<String, ApiError>,
    },
    EditRequest {
        thread_id: String,
        node_id: String,
        generation: u64,
        draft_revision: u64,
        result: Result<EditableRequest, ApiError>,
    },
}
struct EditableRequest {
    text: String,
    attachments: Vec<UiAttachment>,
    run_id: Option<String>,
}
fn command_title(command: &Command) -> String {
    let name = match command {
        Command::Status(_) => "作業状態",
        Command::ModelList(_) => "モデル一覧",
        Command::ThreadList(_) => "会話一覧",
        Command::ThreadGet(_) => "会話の詳細",
        Command::UsageGet(_) => "使用量の合計",
        Command::ThreadCreate(_) => "会話の作成",
        Command::ThreadFork(_) => "会話の分岐",
        Command::ThreadDelete(_) => "会話の削除",
        Command::ThreadDeletePreview(_) => "会話の削除前確認",
        Command::ThreadSet(_) => "会話の設定",
        Command::MessageSend(_) => "依頼の送信",
        Command::MessageCancel(_) => "反映待ちの取消",
        Command::MessageGet(_) => "依頼と回答の確認",
        Command::OperationGet(_) => "操作の受付結果",
        Command::RunGet(_) => "作業の詳細",
        Command::RunRetry(_) => "同じ内容で再試行",
        Command::RunStop(_) => "作業を停止",
        Command::RevisitSet(_) => "Set Revisit",
        Command::RevisitGet(_) => "Inspect Revisit",
        Command::RevisitCancel(_) => "Cancel Revisit",
        Command::TurnGet(_) => "応答処理の詳細",
        Command::Wait(_) => "条件の成立を待機",
        Command::HistoryList(_) => "履歴一覧",
        Command::HistoryGet(_) => "履歴の原文",
        Command::HistorySearch(_) => "履歴の検索",
        Command::HistoryRelations(_) => "履歴のつながり",
        Command::ShellGet(_) => "コマンドの詳細",
        Command::ShellRead(_) => "コマンド出力の読取り",
        Command::ShellWrite(_) => "コマンドへ入力",
        Command::ShellEof(_) => "コマンドの入力を終了",
        Command::ShellStop(_) => "コマンドを停止",
        Command::AttachmentAdd(_) => "添付の追加",
        Command::AttachmentList(_) => "添付一覧",
        Command::AttachmentRemove(_) => "添付の除外",
        Command::Compact(_) => "会話の要約",
        Command::ContextEdit(_) => "会話の入力範囲を編集",
        Command::IndexStatus(_) => "索引の状態",
        Command::IndexPreview(_) => "索引の費用対象の確認",
        Command::IndexBackfill(_) => "未索引資料の追加",
        Command::IndexRetry(_) => "索引の再試行",
        Command::IndexProbe(_) => "埋め込み接続の確認",
        Command::IndexRebuild(_) => "索引の再構築",
        Command::StorageStatus(_) => "保存容量",
        Command::StorageCollect(_) => "保存データの回収",
    };
    name.into()
}
// Report operation acceptance separately from presentation: a report-rendering
// failure must not invite retrying an already-applied mutation.
async fn command_outcome<T>(
    updates: &(impl UpdateTarget + ?Sized),
    submission: Option<CommandSubmission>,
    result: &Result<T, ApiError>,
) {
    if let Some(submission) = submission {
        update(
            updates,
            UiUpdate::CommandOutcome {
                submission,
                result: result.as_ref().map(|_| ()).map_err(ToString::to_string),
            },
        )
        .await;
    }
}
fn management_outcome(result: &Result<Value, ApiError>) -> Result<(), ApiError> {
    match result {
        Err(error) => Err(error.clone()),
        Ok(value) => match value.get("error") {
            Some(error) => Err(
                serde_json::from_value::<ApiError>(error.clone()).unwrap_or_else(|_| {
                    ApiError::new(
                        ErrorCode::InvalidState,
                        "Operation returned an invalid error",
                    )
                }),
            ),
            None => Ok(()),
        },
    }
}
async fn prepare_management(
    result: Result<Value, ApiError>,
    budget: u64,
) -> Result<management::Report, ApiError> {
    let value = match result {
        Ok(value) => value,
        Err(error) => serde_json::json!({"error":error}),
    };
    presentation_work(move || {
        management::report(value, budget.min(MAX_FRAME_BYTES as u64) as usize)
    })
    .await
}

/// Decode chips once for both send paths. Callers retain their cwd validation order.
fn draft_input(
    text: &str,
    attachments: Vec<UiAttachment>,
) -> Result<(crate::attachments::DraftContent, Vec<AttachmentInput>), ApiError> {
    let (content, attachments) = crate::ui::draft::decode(text, attachments)?;
    let files = attachments
        .into_iter()
        .map(|attachment| {
            let kind = AttachmentKind::parse(&attachment.kind).ok_or_else(|| {
                ApiError::new(ErrorCode::InvalidArgument, "Unknown draft attachment kind")
            })?;
            Ok(AttachmentInput {
                path: attachment.path,
                kind,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok((content, files))
}
async fn load_edit_request(
    control: &Control,
    thread_id: &str,
    node_id: &str,
) -> Result<EditableRequest, ApiError> {
    let result = async {
    let (thread, node) = (thread_id.to_owned(), node_id.to_owned());
    let limit = control.store.config.limits.draft_bytes;
    let value=control.store.database.call(move|db|{
        use rusqlite::OptionalExtension;
        let tx=db.transaction()?;
        let record:Option<(String,Option<String>)>=tx.query_row("SELECT n.kind,n.payload_json FROM node n JOIN thread_path p ON p.node_id=n.id JOIN thread t ON t.id=p.thread_id WHERE p.thread_id=?1 AND n.id=?2 AND t.deleted_at IS NULL",rusqlite::params![thread,node],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((kind,body))=record else{return Ok(Err(ApiError::new(ErrorCode::TargetUnavailable,"Select a saved user request on this Thread path")));};
        if !matches!(kind.as_str(),"user"|"user_message"|"message"){return Ok(Err(ApiError::new(ErrorCode::InvalidArgument,"Only a saved user request can be edited into a new branch")));}
        let Some(body)=body else{return Ok(Err(ApiError::new(ErrorCode::TargetUnavailable,"Original request payload was collected")));};
        let payload:Value=serde_json::from_str(&body).map_err(|_|crate::storage::StorageError::Schema("Stored user request is malformed".into()))?;
        if payload.get("role").and_then(Value::as_str)!=Some("user"){return Ok(Err(ApiError::new(ErrorCode::InvalidArgument,"Only a saved user request can be edited")));}
        let attachments=tx.prepare("SELECT a.id,a.absolute_path,a.input_kind,s.state FROM attachment_ref a LEFT JOIN attachment_selection s ON s.attachment_id=a.id AND s.thread_id=?1 WHERE a.source_node_id=?2 ORDER BY s.selected_order,a.id")?.query_map(rusqlite::params![thread,node],|r|Ok(UiAttachment{id:r.get(0)?,path:PathBuf::from(r.get::<_,String>(1)?),kind:r.get(2)?,state:r.get::<_,Option<String>>(3)?.unwrap_or_else(||"excluded".into()),position:None}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let (text,attachments)=match crate::ui::draft::restore(&payload,attachments,true){Ok(value)=>value,Err(error)=>return Ok(Err(error))};
        if text.len()as u64>limit{return Ok(Err(ApiError::new(ErrorCode::InputLimit,"Original request exceeds the draft byte limit; increase limits.draft_bytes before editing")));}
        let run_id=tx.query_row("SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping')",[thread],|r|r.get::<_,String>(0)).optional()?;
        tx.commit()?;Ok(Ok(EditableRequest{text,attachments,run_id}))
    }).await.map_err(storage_error)??;
    Ok(value)

    }.await;
    checked_storage(control, result).await
}
/// Source DB failures stop the owner even when observed through a UI read.
fn storage_error(error: crate::storage::StorageError) -> ApiError {
    match error {
        crate::storage::StorageError::QueueFull => ApiError::new(
            ErrorCode::InvalidState,
            "Database read queue is busy; retry after pending work completes",
        ),
        error => crate::app::storage_error(error),
    }
}
async fn checked_storage<T>(control: &Control, result: Result<T, ApiError>) -> Result<T, ApiError> {
    if result
        .as_ref()
        .err()
        .is_some_and(|error| error.is_storage_failure())
    {
        control.fail_storage().await;
    }
    result
}
async fn load_failed(
    sender: &(impl UpdateTarget + ?Sized),
    thread_id: String,
    generation: u64,
    error: ApiError,
) {
    load_failed_at(sender, thread_id, generation, None, error).await;
}
async fn load_failed_at(
    sender: &(impl UpdateTarget + ?Sized),
    thread_id: String,
    generation: u64,
    view_revision: Option<u64>,
    error: ApiError,
) {
    if error.code == ErrorCode::CursorExpired {
        update(
            sender,
            UiUpdate::HistoryInvalidated {
                thread_id,
                generation,
                view_revision,
            },
        )
        .await;
        return;
    }
    update(
        sender,
        UiUpdate::LoadFailed {
            thread_id,
            generation,
            view_revision,
            message: error.to_string(),
        },
    )
    .await;
}

async fn stream_commits(
    updates: &(impl UpdateTarget + ?Sized),
    thread: &str,
    generation: u64,
    mappings: Vec<(String, String)>,
) {
    for (attempt_id, node_id) in mappings {
        update(
            updates,
            UiUpdate::StreamCommitted {
                thread_id: thread.into(),
                generation,
                attempt_id,
                node_id,
            },
        )
        .await;
    }
}
async fn receipt_states(
    control: &Control,
    ids: Vec<String>,
) -> Result<Vec<(String, ReceiptStatus)>, ApiError> {
    let result = async {
    control.store.database.call(move|db|{use rusqlite::OptionalExtension;let mut result=Vec::new();for id in ids{
        let state:Option<(String,Option<String>)>=db.query_row("SELECT r.state,u.state FROM receipt r LEFT JOIN turn u ON u.id=r.turn_id WHERE r.message_node_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((receipt,turn))=state
            && let Some(status)=receipt_delivery::project_state(&receipt,turn.as_deref()) { result.push((id,status)); }
    }Ok(result)}).await.map_err(storage_error)

    }.await;
    checked_storage(control, result).await
}
async fn restore_snapshot(
    control: &Control,
    node_id: String,
) -> Result<(String, Vec<UiAttachment>), ApiError> {
    let result = async {
    let limit = control.store.config.limits.draft_bytes;
    control.store.database.call(move|db|{use rusqlite::OptionalExtension;let body:Option<Option<String>>=db.query_row("SELECT payload_json FROM node WHERE id=?1",[&node_id],|r|r.get(0)).optional()?;
        let Some(Some(body))=body else{return Ok(Err(ApiError::new(ErrorCode::OutputExpired,"Saved request source expired; its pending snapshot cannot be reconstructed")));};
        let payload:Value=serde_json::from_str(&body).map_err(|_|crate::storage::StorageError::Schema("Saved request is malformed".into()))?;
        let attachments=db.prepare("SELECT id,absolute_path,input_kind FROM attachment_ref WHERE source_node_id=?1 ORDER BY id")?.query_map([node_id],|r|Ok(UiAttachment{id:r.get(0)?,path:PathBuf::from(r.get::<_,String>(1)?),kind:r.get(2)?,state:"saved source; revalidated on send".into(),position:None}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let (text,attachments)=match crate::ui::draft::restore(&payload,attachments,false){Ok(value)=>value,Err(error)=>return Ok(Err(error))};
        if text.len()as u64>limit{return Ok(Err(ApiError::new(ErrorCode::InputLimit,"Saved request exceeds current draft limit; no text was truncated")));}
        Ok(Ok((text,attachments)))
    }).await.map_err(storage_error)?

    }.await;
    checked_storage(control, result).await
}
pub async fn run(
    control: Arc<Control>,
    actions: impl Into<crate::ui::actions::ActionReceiver>,
    updates: SyncSender<UiUpdate>,
    cwd: PathBuf,
    owner: DataOwner,
    server: LocalServer,
) -> Result<(), ApiError> {
    run_with_clipboard(
        control,
        actions,
        updates,
        cwd,
        owner,
        server,
        Arc::new(ClipboardAdapter::new()?),
    )
    .await
}
#[allow(clippy::too_many_arguments)]
pub async fn run_with_clipboard(
    control: Arc<Control>,
    actions: impl Into<crate::ui::actions::ActionReceiver>,
    updates: SyncSender<UiUpdate>,
    cwd: PathBuf,
    owner: DataOwner,
    server: LocalServer,
    clipboard: Arc<dyn ClipboardBackend>,
) -> Result<(), ApiError> {
    run_with_clipboard_slots(
        control,
        actions.into(),
        updates,
        cwd,
        owner,
        server,
        clipboard,
        Arc::new(Semaphore::new(32)),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_with_clipboard_slots(
    control: Arc<Control>,
    actions: crate::ui::actions::ActionReceiver,
    updates: SyncSender<UiUpdate>,
    cwd: PathBuf,
    owner: DataOwner,
    server: LocalServer,
    clipboard: Arc<dyn ClipboardBackend>,
    command_slots: Arc<Semaphore>,
) -> Result<(), ApiError> {
    run_with_delivery(
        control,
        actions,
        BufferedUpdates::new(updates),
        cwd,
        owner,
        server,
        clipboard,
        command_slots,
        #[cfg(test)]
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_with_delivery(
    control: Arc<Control>,
    mut actions: crate::ui::actions::ActionReceiver,
    updates: BufferedUpdates,
    cwd: PathBuf,
    owner: DataOwner,
    server: LocalServer,
    clipboard: Arc<dyn ClipboardBackend>,
    command_slots: Arc<Semaphore>,
    #[cfg(test)] startup: Option<receipt_delivery::Startup>,
) -> Result<(), ApiError> {
    actions.close_tool_starts_on_quit(&control);
    let mut selection = selection::Selection::default();
    let mut ipc_failure = server.failures();
    let mut watch_ipc_failure = true;
    let mut control_tasks = JoinSet::new();
    let mut streams = streaming::Display::default();
    let mut stream_inflight = false;
    let mut stream_retry: Option<Box<streaming::Prepared>> = None;
    let mut stream_next = tokio::time::Instant::now();
    let mut delivery_next = tokio::time::Instant::now();
    let stream_period =
        Duration::from_secs_f64(1.0 / f64::from(control.store.config.ui.max_fps.clamp(1, 30)));
    #[cfg(not(test))]
    let (mut pending, mut receipt_batch) = (
        HashMap::<String, receipt_delivery::PendingReceipt>::new(),
        receipt_delivery::Batch::default(),
    );
    #[cfg(test)]
    let receipt_delivery::Startup {
        mut pending,
        batch: mut receipt_batch,
    } = startup.unwrap_or_default();
    let mut images = draft_images::DraftImages::default();
    let (background_tx, mut background_rx) = mpsc::channel(32);
    let mut workers = JoinSet::new();
    let mut send_workers = JoinSet::new();
    let mut read_workers = JoinSet::new();
    let read_slots = Arc::new(Semaphore::new(8));
    let read_cancel = CancellationToken::new();
    let progress_slots = Arc::new(Semaphore::new(1));
    let mut dirty_threads: HashSet<String> = HashSet::new();
    let mut refreshing: HashSet<String> = HashSet::new();
    let mut draft_saver = draft_saver::DraftSaver::new(control.clone(), background_tx.clone());
    let clipboard_slots = Arc::new(Semaphore::new(2));
    let send_slots = Arc::new(Semaphore::new(32));
    let mut render_width = 80_u16;
    let mut management = management_state::Management::default();
    let cancel = CancellationToken::new();
    let mut ui_subscription = control.subscribe_ui();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut initialize = Some(UiUpdate::Initialize { cwd });
    let mut draft_failure = None;
    loop {
        if let Some(value) = initialize.take() {
            initialize = updates.defer(value);
        }
        if initialize.is_none() {
            receipt_batch.drain(&mut pending, &updates);
        }
        draft_saver.pump_delivery(&mut draft_failure, &updates);
        if let Some(watch) = management.usage_ready()
            && let Ok(permit) = command_slots.clone().try_acquire_owned()
        {
            management.usage_begin_refresh(watch.generation);
            let control = control.clone();
            let sender = background_tx.clone();
            spawn_work(&mut workers, permit, async move {
                let title = command_title(&watch.command);
                let response = management_request_trigger(
                    &control,
                    watch.command,
                    CancellationToken::new(),
                    crate::usage_totals::UsageTrigger::Changed,
                )
                .await;
                let result =
                    prepare_management(response, control.store.config.cache.body_bytes).await;
                let _ = sender
                    .send(BackgroundResult::UsageRefresh {
                        context: watch.context,
                        generation: watch.generation,
                        title,
                        result,
                    })
                    .await;
            });
        }

        while !updates.backpressured() {
            let Some(thread) = dirty_threads
                .iter()
                .find(|thread| !refreshing.contains(*thread))
                .cloned()
            else {
                break;
            };
            let Ok(permit) = read_slots.clone().try_acquire_owned() else {
                break;
            };
            dirty_threads.remove(&thread);
            let ids = receipt_delivery::next_ids(&mut pending, &thread);
            let generation = selection.generations.get(&thread).copied();
            let visible = selection.active_thread.as_deref() == Some(&thread);
            if ids.is_empty() && !visible {
                continue;
            }
            refreshing.insert(thread.clone());
            #[cfg(test)]
            stream_metrics::FRONTEND_RELOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            read_workers.spawn(refresh::run(
                refresh::Query {
                    thread,
                    ids,
                    generation,
                    visible,
                    epoch: selection.progress_epoch,
                    render_width,
                },
                control.clone(),
                updates.worker(),
                background_tx.clone(),
                read_cancel.clone(),
                progress_slots.clone(),
                permit,
            ));
        }
        if updates.closed() {
            break;
        }
        ui_subscription.select(selection.active_thread.as_deref());
        actions.enable_cancel(control_tasks.is_empty());
        tokio::select! {
            _=control.gc.availability_changed()=> {
                if let Some(thread)=selection.active_thread.as_ref()
                    && let Some(generation)=selection.generations.get(thread).copied()
                {
                    update(&updates,UiUpdate::DetailSourcesChanged {thread_id:thread.clone(),generation}).await;
                }
            },
            _=tokio::task::yield_now(),if receipt_batch.active() && updates.main_slot()=> {},
            changed=ipc_failure.changed(),if watch_ipc_failure=> {
                watch_ipc_failure = false;
                let error = if changed.is_ok() {
                    ipc_failure.borrow_and_update().clone()
                } else {
                    Some(ApiError::new(
                        ErrorCode::IoFailed,
                        "IPC task stopped unexpectedly",
                    ))
                };
                if let Some(error) = error {
                    status(
                        &updates,
                        None,
                        format!("CLI受付が停止しました。本体を再起動して復旧してください: {error}"),
                    )
                    .await;
                }
            },
            completed=control_tasks.join_next(),if !control_tasks.is_empty()=> {
                actions.complete_cancel();
                match completed {
                    Some(Ok((thread_id, Err(error)))) => status(&updates, Some(thread_id), error).await,
                    Some(Err(_)) => {
                        status(
                            &updates,
                            None,
                            "中断処理の観測が失敗しました。対象状態を確認してください",
                        )
                        .await
                    }
                    _ => {}
                }
            },
            _=updates.notified()=> {
                updates.flush();delivery_next=tokio::time::Instant::now()+Duration::from_millis(5);
            },
            _=tokio::time::sleep_until(delivery_next),if updates.pending()=> {
                updates.flush();delivery_next=tokio::time::Instant::now()+Duration::from_millis(5);
            },
            action=actions.recv_with_admission(!updates.backpressured())=> {
                let Some(action) = action else {
                    break;
                };
                let mut action = action;
                let mut submission = None;
                let mut context = None;
                let action = loop {
                    match action {
                        UiAction::SubmittedCommand {
                            submission: input,
                            action: inner,
                        } => {
                            submission = Some(input);
                            action = *inner;
                        }
                        UiAction::ManagementIntent {
                            context: owner,
                            action: inner,
                        } => {
                            context = Some(owner);
                            action = *inner;
                        }
                        action => break action,
                    }
                };
                let has_submission = submission.is_some();
                #[cfg(test)] stream_metrics::FRONTEND_ACTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match action {
                    UiAction::SubmittedCommand { .. } | UiAction::ManagementIntent { .. } => {
                        unreachable!("only one UI submission wrapper is emitted")
                    }
                    UiAction::Quit => {
                        // Also covers ordinary Tokio action receivers used by
                        // embedders, which do not have the UI control mailbox.
                        control.close_tool_start_admission();
                        break;
                    },
                    UiAction::RenderWidth { width } => {
                        render_width = width.max(1);
                        if let Some(thread) = &selection.active_thread {
                            streams.invalidate(thread);
                        }
                    }
                    UiAction::LoadReceived{thread_id,page}=>{
                        let control=control.clone();let sender=updates.worker();
                        let reply_context=context.clone();
                        if !spawn_read(&mut read_workers,&read_slots,&read_cancel,async move{
                            match received::list(&control,thread_id,page).await{
                                Ok((title,fields))=>management_update(&sender,reply_context,UiUpdate::ReceivedList{title,fields}).await,
                                Err(error)=>management_status(&sender,reply_context,error).await,
                            }
                        }){management_status(&updates,context,"読取り枠が満杯です。再操作してください").await;}
                    }
                    UiAction::LoadModels {
                        connection,
                        request_id,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let token = read_cancel.child_token();
                        if !spawn_read(&mut read_workers, &read_slots, &read_cancel, async move {
                            let response = control
                                .request_cancellable(
                                    operation(
                                        &control,
                                        Command::ModelList(crate::protocol::ModelListArgs {
                                            connection: Some(connection),
                                            thread_id: None,
                                        }),
                                    ),
                                    token,
                                )
                                .await;
                            let result = if response.ok {
                                serde_json::from_value::<crate::protocol::ModelListResult>(
                                    response.result.unwrap_or(Value::Null),
                                )
                                .map(|result| result.models)
                                .map_err(|error| error.to_string())
                            } else {
                                Err(response
                                    .error
                                    .map(|error| error.to_string())
                                    .unwrap_or_else(|| "一覧を取得できませんでした".into()))
                            };
                            update(&sender, UiUpdate::Models { request_id, result }).await;
                        }) {
                            update(
                                &updates,
                                UiUpdate::Models {
                                    request_id,
                                    result: Err(
                                        "取得キューが満杯です。Ctrl+Rで再取得してください".into()
                                    ),
                                },
                            )
                            .await;
                        }
                    }
                    UiAction::InterruptTurn {thread_id,turn_id} => {
                        if !control_tasks.is_empty() {status(&updates,Some(thread_id),"中断操作を処理中です。状態を確認して再操作してください").await;continue;}
                        let control=control.clone();
                        control_tasks.spawn(async move {let result=control.interrupt_turn(thread_id.clone(),turn_id).await.map(|_|());(thread_id,result)});
                    }
                    UiAction::CancelMessages {message_ids,target} => {
                        let Ok(permit)=command_slots.clone().try_acquire_owned() else {status(&updates,target.thread_id.clone(),"取消キューが満杯です。未受付です").await;continue;};
                        let control=control.clone();let sender=updates.worker();
                        spawn_work(&mut workers,permit,async move {message_queue::cancel(&control,&sender,target,message_ids).await;});
                    }
                    UiAction::RestoreArchive {archive_id,target} => {
                        let control=control.clone();let sender=updates.worker();let rejected=target.thread_id.clone();
                        if !spawn_read(&mut read_workers,&read_slots,&read_cancel,async move {message_queue::restore(&control,&sender,target,archive_id).await;}) {status(&updates,rejected,"復元キューが満杯です。再操作してください").await;}
                    }
                    UiAction::ListRestore {thread_id,generation,request_id} => {
                        let control=control.clone();let sender=updates.worker();let rejected=thread_id.clone();
                        if !spawn_read(&mut read_workers,&read_slots,&read_cancel,async move {
                            match message_queue::archives(&control,thread_id.clone()).await {
                                Ok(entries)=>update(&sender,UiUpdate::RestoreArchives {thread_id,generation,request_id,entries,error:None}).await,
                                Err(error)=>update(&sender,UiUpdate::RestoreArchives {thread_id,generation,request_id,entries:vec![],error:Some(error.to_string())}).await,
                            }
                        }) {status(&updates,rejected,"下書き一覧の取得キューが満杯です").await;}
                    }
                    UiAction::Send {
                        generation, now, snapshot,
                        draft_id,
                        revision,
                        thread_id,
                        cwd,
                        text,
                        attachments,
                        connection,
                        model,
                        effort,
                    } => {
                        let diagnostics = owner.diagnostics.clone();
                        let trace = MessageSendTrace {
                            stage: MessageSendStage::Queued,
                            draft_id: draft_id.clone(),
                            revision,
                            operation_id: format!(
                                "{}:{}",
                                control.store.instance_id,
                                uuid::Uuid::new_v4()
                            ),
                            message_id: None,
                        };
                        trace_send(&diagnostics, &trace, MessageSendStage::Queued, thread_id.as_deref(), None);
                        selection.active_draft = Some(draft_id.clone());
                        selection.active_thread = thread_id.clone();
                        let accepting=UiUpdate::SendOperation {draft_id:draft_id.clone(),revision,operation_id:trace.operation_id.clone()};
                        if !actions.is_closing() {
                            update(&updates, accepting).await;
                        }
                        let permit = if actions.is_closing() {
                            None
                        } else {
                            match send_slots.clone().try_acquire_owned() {
                                Ok(permit) => Some(permit),
                                Err(_) => {
                                    trace_send(&diagnostics, &trace, MessageSendStage::QueueFull, thread_id.as_deref(), None);
                                    update(
                                        &updates,
                                        UiUpdate::Receipt {
                                            draft_id,
                                            revision,
                                            thread_id,
                                            status: ReceiptStatus::Failed,
                                            message: Some(
                                                "送信準備キューが満杯です。下書きを保持しました".into(),
                                            ),
                                        },
                                    )
                                    .await;
                                    continue;
                                }
                            }
                        };
                        let send_slots = send_slots.clone();
                        let image_leases = images.leases(&draft_id, &attachments);
                        let control = control.clone();
                        let sender = background_tx.clone();
                        send_workers.spawn(async move {
                            let _image_leases = image_leases;
                            let _permit = match permit {
                                Some(permit) => permit,
                                None => send_slots
                                    .acquire_owned()
                                    .await
                                    .expect("frontend send slots remain open"),
                            };
                            trace_send(&diagnostics, &trace, MessageSendStage::Processing, thread_id.as_deref(), None);
                            let trace_thread = thread_id.as_deref().map(str::to_owned);
                            let ids = attachments
                                .iter()
                                .map(|attachment| attachment.id.clone())
                                .collect();
                            let result = async {
                                let cwd = cwd.into_os_string().into_string().map_err(|_| {
                                    ApiError::new(
                                        ErrorCode::InvalidArgument,
                                        "Thread cwd must be valid Unicode",
                                    )
                                })?;
                                let snapshot=message_queue::submission_snapshot(snapshot,&draft_id,revision,&text,&attachments,None,control.store.config.limits.draft_bytes)?;
                                let (content, files) = draft_input(&snapshot.text,snapshot.attachments.clone())?;
                                let submission=crate::control::db::DraftSubmission {draft_id:crate::control::db::DraftSubmission::id_for_operation(&trace.operation_id),source_draft_id:draft_id.clone(),revision,snapshot:serde_json::to_value(&snapshot).map_err(|_|ApiError::new(ErrorCode::InvalidState,"Cannot preserve draft"))?};
                                control.store.prepare_draft_submission(&trace.operation_id,thread_id.as_deref(),&submission).await?;
                                let options=crate::control::db::DraftSendOptions {now,submission:Some(submission),..Default::default()};
                                control
                                    .send_ordered_draft(
                                        trace.operation_id.clone(),
                                        ThreadCreateArgs { agents: None,
                                            connection,
                                            cwd: Some(cwd),
                                            model,
                                            effort,
                                            parent_id: None,
                                        },
                                        thread_id,
                                        content,
                                        files,
                                        options,
                                    )
                                    .await
                            }
                            .await;
                            trace_send(
                                &diagnostics,
                                &trace,
                                if result.is_ok() { MessageSendStage::Saved } else { MessageSendStage::Failed },
                                trace_thread.as_deref(),
                                result.as_ref().ok(),
                            );
                            let projection=match &result {
                                Ok(value)=>HistoryReader::saved(&control,render_width,draft_id.clone(),revision,generation,value).await.ok(),
                                Err(_)=>None,
                            };
                            let _ = sender
                                .send(BackgroundResult::Sent {
                                    generation, projection: projection.map(Box::new),
                                    draft_id,
                                    revision,
                                    attachments: ids,
                                    result,
                                })
                                .await;
                        });
                    }
                    UiAction::SelectAgents { request_id, draft_id, generation, args } => {
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            let error=ApiError::new(ErrorCode::InvalidState,"設定の変更キューが満杯です");
                            command_outcome::<()>(&updates,submission,&Err(error.clone())).await;
                            update(&updates,UiUpdate::AgentsSelected { request_id,draft_id,generation,result:Err(error) }).await;
                            continue;
                        };
                        let control=control.clone();
                        let sender=updates.worker();
                        spawn_work(&mut workers,permit,async move {
                            let result=request(&control,Command::ThreadCreate(args)).await.and_then(|v|serde_json::from_value::<ThreadDetails>(v).map_err(|_|ApiError::new(ErrorCode::InvalidState,"Invalid Thread creation response")));
                            command_outcome(&sender,submission,&result).await;
                            update(&sender,UiUpdate::AgentsSelected {request_id,draft_id,generation,result}).await;
                        });
                    }
                    UiAction::Command(Command::ThreadSet(args)) => {
                        let target = args
                            .thread_id
                            .clone()
                            .or_else(|| selection.active_thread.clone());
                        let generation = target
                            .as_ref()
                            .and_then(|thread| selection.generations.get(thread))
                            .copied()
                            .unwrap_or(0);
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            command_outcome::<()>(
                                &updates,
                                submission,
                                &Err(ApiError::new(
                                    ErrorCode::InvalidState,
                                    "設定の変更キューが満杯です。完了後に再操作してください",
                                )),
                            )
                            .await;
                            if !has_submission {
                                status(
                                    &updates,
                                    target,
                                    "設定の変更キューが満杯です。完了後に再操作してください",
                                )
                                .await;
                            }
                            continue;
                        };
                        status(&updates, target.clone(), "設定を反映中…").await;
                        let control = control.clone();
                        let sender = updates.worker();
                        spawn_work(&mut workers, permit, async move {
                            let result =
                                request(&control, Command::ThreadSet(args))
                                    .await
                                    .and_then(|value| {
                                        serde_json::from_value::<ThreadDetails>(value).map_err(|_| {
                                            ApiError::new(
                                                ErrorCode::InvalidState,
                                                "Invalid settings response",
                                            )
                                        })
                                    });
                            command_outcome(&sender, submission, &result).await;
                            match result {
                                Ok(details) => {
                                    let thread = details.thread_id.clone();
                                    let message = format!(
                                        "設定を変更しました · {} / {}",
                                        details.connection, details.model
                                    );
                                    update(
                                        &sender,
                                        UiUpdate::Thread {
                                            generation,
                                            details,
                                            draft: None,
                                        },
                                    )
                                    .await;
                                    status(&sender, Some(thread), message).await;
                                }
                                Err(error) => {
                                    if !has_submission {
                                        status(&sender, target, error).await;
                                    }
                                }
                            }
                        });
                    }
                    // Human /retry is an action on the current conversation. F8
                    // Manage(RunRetry) remains an explicit inspectable report.
                    UiAction::Command(Command::RunRetry(args)) => {
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            command_outcome::<()>(
                                &updates,
                                submission,
                                &Err(ApiError::new(
                                    ErrorCode::InvalidState,
                                    "再試行の準備キューが満杯です。完了後に再操作してください",
                                )),
                            )
                            .await;
                            if !has_submission {
                                status(
                                    &updates,
                                    selection.active_thread.clone(),
                                    "再試行の準備キューが満杯です。完了後に再操作してください",
                                )
                                .await;
                            }
                            continue;
                        };
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = selection.active_thread.clone();
                        spawn_work(&mut workers, permit, async move {
                            let result = request(&control, Command::RunRetry(args)).await;
                            command_outcome(&sender, submission, &result).await;
                            match result {
                                Ok(_) => {
                                    status(&sender, target, "同じ内容で再試行を受け付けました").await
                                }
                                Err(error) => {
                                    if !has_submission {
                                        status(&sender, target, error).await;
                                    }
                                }
                            }
                        });
                    }
                    UiAction::ThreadDeletion { request_id, args, execute, mut known_thread_ids } => {
                        known_thread_ids = thread_deletion::candidates(known_thread_ids, &selection,
                            draft_saver.threads().chain(images.threads()).cloned());
                        if known_thread_ids.len() > thread_deletion::MAX_KNOWN_THREADS {
                            update(&updates, UiUpdate::ThreadDeletion { request_id, args, execute, result: Err(ApiError::new(ErrorCode::InvalidArgument, "削除確認の会話件数が上限を超えました。Shellboy を再起動して再操作してください")) }).await;
                            continue;
                        }
                        let control = control.clone();
                        let results = background_tx.clone();
                        let action_args = args.clone();
                        let refusal_args = args.clone();
                        let work = async move {
                            let result = thread_deletion::guard(thread_deletion::run(&control, action_args, execute)).await;
                            let _ = results.send(BackgroundResult::ThreadDeletion { request_id, args, execute, known_thread_ids, result }).await;
                        };
                        if execute {
                            let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                                // The work future owns its arguments but has not polled or mutated.
                                drop(work);
                                update(&updates, UiUpdate::ThreadDeletion { request_id, args: refusal_args, execute, result: Err(ApiError::new(ErrorCode::InvalidState, "操作キューが満杯です。削除は未受付です")) }).await;
                                continue;
                            };
                            spawn_work(&mut workers, permit, work);
                        } else if !spawn_read(&mut read_workers, &read_slots, &read_cancel, work) {
                            update(&updates, UiUpdate::ThreadDeletion { request_id, args: refusal_args, execute, result: Err(ApiError::new(ErrorCode::InvalidState, "読取りキューが満杯です。削除確認を再操作してください")) }).await;
                        }
                    }
                    UiAction::Command(command) | UiAction::Manage(command) => {
                        let title = command_title(&command);
                        let mutation = !command.is_read_only();
                        let notice_owner = management::NoticeOwner::capture(
                            context.as_ref(),
                            submission.as_ref(),
                            selection.active_thread.as_deref(),
                            selection.active_draft.as_deref(),
                        );
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            let error = ApiError::new(
                                ErrorCode::InvalidState,
                                "操作キューが満杯です。完了後に再操作してください",
                            );
                            command_outcome::<()>(&updates, submission, &Err(error.clone())).await;
                            if mutation {
                                if let Some(owner) = notice_owner {
                                    update(
                                        &updates,
                                        owner.update(format!("未受付 · {title} · {}", error.message)),
                                    )
                                    .await;
                                }
                            } else if !has_submission {
                                management_status(&updates, context.clone(), error.message).await;
                            }
                            continue;
                        };
                        let generation = management.next_generation();
                        management.watch_usage(&command,context.as_ref(),generation);
                        let stop = matches!(command, Command::RunStop(_));
                        let work_status = management::work_status(&command);
                        let control = control.clone();
                        let sender = background_tx.clone();
                        let outcomes = updates.worker();
                        let observe = management.observe(&command);
                        spawn_work(&mut workers, permit, async move {
                            let response = management_request(&control, command, observe).await;
                            command_outcome(&outcomes, submission, &management_outcome(&response))
                                .await;
                            if mutation && let Some(owner) = notice_owner {
                                update(
                                    &outcomes,
                                    owner.update(management::completion_text(&title, stop, &response)),
                                )
                                .await;
                            }
                            let result =
                                prepare_management(response, control.store.config.cache.body_bytes)
                                    .await;
                            let _ = sender
                                .send(BackgroundResult::Command {
                                    work_status,
                                    context,
                                    generation,
                                    title,
                                    result,
                                })
                                .await;
                        });
                    }
                    UiAction::ReloadAttachments { thread_id } => {
                        let notice_owner = management::NoticeOwner::capture(
                            context.as_ref(),
                            submission.as_ref(),
                            selection.active_thread.as_deref(),
                            selection.active_draft.as_deref(),
                        );
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            let error = ApiError::new(
                                ErrorCode::InvalidState,
                                "添付操作を準備中です。完了後に再操作してください",
                            );
                            command_outcome::<()>(&updates, submission, &Err(error.clone())).await;
                            if let Some(owner) = notice_owner {
                                update(
                                    &updates,
                                    owner.update(format!("未受付 · 添付再読込 · {}", error.message)),
                                )
                                .await;
                            }
                            continue;
                        };
                        let generation = management.next_generation();
                        let control = control.clone();
                        let sender = background_tx.clone();
                        let outcomes = updates.worker();
                        spawn_work(&mut workers, permit, async move {
                            let title = format!("添付を明示再読込み: {thread_id}（API送信なし）");
                            let response = control.reload_attachments(thread_id).await;
                            command_outcome(&outcomes, submission, &response).await;
                            if let Some(owner) = notice_owner {
                                update(
                                    &outcomes,
                                    owner.update(management::completion_text(&title, false, &response)),
                                )
                                .await;
                            }
                            let result =
                                prepare_management(response, control.store.config.cache.body_bytes)
                                    .await;
                            let _ = sender
                                .send(BackgroundResult::Command {
                                    work_status: false,
                                    context,
                                    generation,
                                    title,
                                    result,
                                })
                                .await;
                        });
                    }
                    UiAction::IndexPreview(args) => {
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            management_update(
                                &updates,
                                context.clone(),
                                UiUpdate::IndexPreviewFailed { args, error: "索引操作を準備中です。完了後に再操作してください".into() },
                            )
                            .await;
                            continue;
                        };
                        management_update(&updates, context.clone(), UiUpdate::IndexPreviewStarted { args: args.clone() }).await;
                        let control = control.clone();
                        let sender = background_tx.clone();
                        spawn_work(&mut workers, permit, async move {
                            let result =
                                control
                                    .index
                                    .preview(args.clone())
                                    .await
                                    .and_then(|preview| {
                                        serde_json::to_value(preview).map_err(|_| {
                                            ApiError::new(
                                                ErrorCode::InvalidState,
                                                "Cannot encode index preview",
                                            )
                                        })
                                    });
                            let result = match result {
                                Ok(value) => {
                                    presentation_work(move || management::index_preview_text(&value)).await
                                }
                                Err(error) => Err(error),
                            };
                            let _ = sender
                                .send(BackgroundResult::IndexPreview {
                                    context,
                                    args,
                                    result,
                                })
                                .await;
                        });
                    }
                    UiAction::LoadEditRequest {
                        thread_id,
                        node_id,
                        generation,
                        draft_revision,
                    } => {
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            command_outcome::<()>(
                                &updates,
                                submission,
                                &Err(ApiError::new(
                                    ErrorCode::InvalidState,
                                    "履歴操作を準備中です。完了後に再操作してください",
                                )),
                            )
                            .await;
                            if !has_submission {
                                status(
                                    &updates,
                                    None,
                                    "履歴操作を準備中です。完了後に再操作してください",
                                )
                                .await;
                            }
                            continue;
                        };
                        let control = control.clone();
                        let sender = background_tx.clone();
                        let outcomes = updates.worker();
                        spawn_work(&mut workers, permit, async move {
                            let result = load_edit_request(&control, &thread_id, &node_id).await;
                            command_outcome(&outcomes, submission, &result).await;
                            if has_submission && result.is_err() {
                                return;
                            }
                            let _ = sender
                                .send(BackgroundResult::EditRequest {
                                    thread_id,
                                    node_id,
                                    generation,
                                    draft_revision,
                                    result,
                                })
                                .await;
                        });
                    }
                    UiAction::SendEdited {
                        generation, now, snapshot,
                        draft_id,
                        revision,
                        source_thread_id,
                        source_node_id,
                        confirmed_run_id,
                        text,
                        attachments,
                        cwd,
                        connection,
                        model,
                        effort,
                    } => {
                        let diagnostics = owner.diagnostics.clone();
                        let trace = MessageSendTrace {
                            stage: MessageSendStage::Queued,
                            draft_id: draft_id.clone(),
                            revision,
                            operation_id: format!(
                                "{}:{}",
                                control.store.instance_id,
                                uuid::Uuid::new_v4()
                            ),
                            message_id: None,
                        };
                        trace_send(&diagnostics, &trace, MessageSendStage::Queued, Some(source_thread_id.as_str()), None);
                        selection.active_draft = Some(draft_id.clone());
                        selection.active_thread = None;
                        let accepting=UiUpdate::SendOperation {draft_id:draft_id.clone(),revision,operation_id:trace.operation_id.clone()};
                        if !actions.is_closing() {
                            update(&updates, accepting).await;
                        }
                        let permit = if actions.is_closing() {
                            None
                        } else {
                            match send_slots.clone().try_acquire_owned() {
                                Ok(permit) => Some(permit),
                                Err(_) => {
                                    trace_send(&diagnostics, &trace, MessageSendStage::QueueFull, Some(source_thread_id.as_str()), None);
                                    update(
                                        &updates,
                                        UiUpdate::Receipt {
                                            draft_id,
                                            revision,
                                            thread_id: None,
                                            status: ReceiptStatus::Failed,
                                            message: Some(
                                                "送信準備キューが満杯です。下書きを保持しました".into(),
                                            ),
                                        },
                                    )
                                    .await;
                                    continue;
                                }
                            }
                        };
                        let send_slots = send_slots.clone();
                        let image_leases = images.leases(&draft_id, &attachments);
                        let control = control.clone();
                        let sender = background_tx.clone();
                        send_workers.spawn(async move {
                            let _image_leases = image_leases;
                            let _permit = match permit {
                                Some(permit) => permit,
                                None => send_slots
                                    .acquire_owned()
                                    .await
                                    .expect("frontend send slots remain open"),
                            };
                            trace_send(&diagnostics, &trace, MessageSendStage::Processing, Some(source_thread_id.as_str()), None);
                            let trace_thread = Some(source_thread_id.clone());
                            let ids = attachments.iter().map(|a| a.id.clone()).collect();
                            let result = async {
                                let snapshot=message_queue::submission_snapshot(snapshot,&draft_id,revision,&text,&attachments,Some((&source_thread_id,&source_node_id)),control.store.config.limits.draft_bytes)?;
                                let (content, files) = draft_input(&snapshot.text,snapshot.attachments.clone())?;
                                let cwd = cwd.into_os_string().into_string().map_err(|_| {
                                    ApiError::new(
                                        ErrorCode::InvalidArgument,
                                        "Thread cwd must be valid Unicode",
                                    )
                                })?;
                                let submission=crate::control::db::DraftSubmission {draft_id:crate::control::db::DraftSubmission::id_for_operation(&trace.operation_id),source_draft_id:draft_id.clone(),revision,snapshot:serde_json::to_value(&snapshot).map_err(|_|ApiError::new(ErrorCode::InvalidState,"Cannot preserve draft"))?};
                                control.store.prepare_draft_submission(&trace.operation_id,Some(&source_thread_id),&submission).await?;
                                let options=crate::control::db::DraftSendOptions {now,submission:Some(submission),..Default::default()};
                                control
                                    .edit_ordered_request(
                                        trace.operation_id.clone(),
                                        source_thread_id,
                                        source_node_id,
                                        confirmed_run_id,
                                        content,
                                        files,
                                        ThreadCreateArgs { agents: None,
                                            cwd: Some(cwd),
                                            connection,
                                            model,
                                            effort,
                                            parent_id: None,
                                        },
                                        options,
                                    )
                                    .await
                            }
                            .await;
                            trace_send(
                                &diagnostics,
                                &trace,
                                if result.is_ok() { MessageSendStage::Saved } else { MessageSendStage::Failed },
                                trace_thread.as_deref(),
                                result.as_ref().ok(),
                            );
                            let projection=match &result {
                                Ok(value)=>HistoryReader::saved(&control,render_width,draft_id.clone(),revision,generation,value).await.ok(),
                                Err(_)=>None,
                            };
                            let _ = sender
                                .send(BackgroundResult::Sent {
                                    generation, projection: projection.map(Box::new),
                                    draft_id,
                                    revision,
                                    attachments: ids,
                                    result,
                                })
                                .await;
                        });
                    }
                    UiAction::WaitForShutdown | UiAction::ForceQuit => {
                        status(
                            &updates,
                            None,
                            "終了操作は /quit または Ctrl+Q の後に選択してください",
                        )
                        .await
                    }
                    UiAction::RestoreSnapshot {
                        draft_id,
                        revision,
                        node_id,
                        expected_revision,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let rejected = submission.clone();
                        if !spawn_read(&mut read_workers, &read_slots, &read_cancel, async move {
                            let result = restore_snapshot(&control, node_id).await;
                            command_outcome(&sender, submission, &result).await;
                            match result {
                                Ok((text, attachments)) => {
                                    update(
                                        &sender,
                                        UiUpdate::RestoredSnapshot {
                                            draft_id,
                                            revision,
                                            expected_revision,
                                            text,
                                            attachments,
                                        },
                                    )
                                    .await
                                }
                                Err(error) => {
                                    if !has_submission {
                                        status(&sender, None, error).await;
                                    }
                                }
                            }
                        }) {
                            command_outcome::<()>(
                                &updates,
                                rejected,
                                &Err(ApiError::new(
                                    ErrorCode::InvalidState,
                                    "原文読込キューが満杯です。完了後に再操作してください",
                                )),
                            )
                            .await;
                            if !has_submission {
                                status(
                                    &updates,
                                    None,
                                    "原文読込キューが満杯です。完了後に再操作してください",
                                )
                                .await;
                            }
                        }
                    }
                    UiAction::LoadThreads {
                        cursor,
                        filter,
                        generation,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        if !spawn_read(&mut read_workers, &read_slots, &read_cancel, async move {
                            let requested_cursor = cursor.clone();
                            match request(
                                &control,
                                Command::ThreadList(ThreadListArgs {
                                    limit: DEFAULT_LIST_LIMIT,
                                    cursor,
                                    query: if filter.is_empty() {
                                        None
                                    } else {
                                        Some(filter)
                                    },
                                }),
                            )
                            .await
                            .and_then(|value| {
                                serde_json::from_value::<Page<ThreadSummary>>(value).map_err(|_| {
                                    ApiError::new(ErrorCode::IoFailed, "Invalid Thread list reply")
                                })
                            }) {
                                Ok(page) => match presentation_work(move || {
                                    let times = thread_display_times(&page.items);
                                    (page, times)
                                })
                                .await
                                {
                                    Ok((page, display_times)) => {
                                        update(
                                            &sender,
                                            UiUpdate::Threads {
                                                generation,
                                                requested_cursor,
                                                threads: page.items,
                                                display_times,
                                                next_cursor: page.next_cursor,
                                            },
                                        )
                                        .await
                                    }
                                    Err(error) => {
                                        update(
                                            &sender,
                                            UiUpdate::ThreadListFailed {
                                                generation,
                                                error: error.to_string(),
                                            },
                                        )
                                        .await
                                    }
                                },
                                Err(error) => {
                                    update(
                                        &sender,
                                        UiUpdate::ThreadListFailed {
                                            generation,
                                            error: error.to_string(),
                                        },
                                    )
                                    .await
                                }
                            }
                        }) {
                            update(
                                &updates,
                                UiUpdate::ThreadListFailed {
                                    generation,
                                    error: "スレッド読込キューが満杯です。Ctrl+Rで再取得してください"
                                        .into(),
                                },
                            )
                            .await;
                        }
                    }
                    UiAction::ActivateDraft { draft_id } => {
                        management.close_usage();
                        selection.activate_draft(draft_id);
                        streams.clear();
                        stream_retry = None;
                    }
                    UiAction::SelectThread {
                        thread_id,
                        generation,
                        view_revision,
                        known_view,
                        preview_sources,
                    } => {
                        management.close_usage();
                        selection.select_thread(thread_id.clone(), generation);
                        ui_subscription.select(Some(&thread_id));
                        streams.clear();
                        streams.resume(thread_id.clone(), preview_sources);
                        stream_retry = None;
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        let results = background_tx.clone();
                        let progress_slots = progress_slots.clone();
                        let epoch = selection.progress_epoch;
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                match select_thread(
                                    &control,
                                    &sender,
                                    &target,
                                    generation,
                                    view_revision,
                                    known_view,
                                    render_width,
                                )
                                .await
                                {
                                    Err(error) => load_failed(&sender, target, generation, error).await,
                                    Ok(()) => {
                                        if let Ok(_permit) = progress_slots.acquire_owned().await {
                                            let result = activity::read(&control, &target).await;
                                            let _ = results
                                                .send(BackgroundResult::Progress {
                                                    thread_id: target,
                                                    epoch,
                                                    generation,
                                                    result,
                                                })
                                                .await;
                                        }
                                    }
                                }
                            },
                        ) {
                            load_failed(
                                &updates,
                                thread_id,
                                generation,
                                ApiError::new(
                                    ErrorCode::InvalidState,
                                    "History read queue is busy; reselect to retry",
                                ),
                            )
                            .await;
                        }
                    }
                    UiAction::LoadHistory {
                        thread_id,
                        generation,
                        before_cursor,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                let reply = Reply::new(target, generation);
                                if let Err(error) = HistoryReader::new(&control, &sender, render_width)
                                    .page(&reply, before_cursor, None)
                                    .await
                                {
                                    reply.failed(&sender, error).await;
                                }
                            },
                        ) {
                            load_failed(
                                &updates,
                                thread_id,
                                generation,
                                ApiError::new(ErrorCode::InvalidState, "History read queue is busy"),
                            )
                            .await;
                        }
                    }
                    UiAction::RefreshTools {
                        thread_id,
                        generation,
                        revision,
                        mut nodes,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                let result = checked_storage(
                                    &control,
                                    async {
                                        tools::enrich(&control, &mut nodes).await?;
                                        streams::enrich_previews(&control, &mut nodes, render_width).await
                                    }.await,
                                )
                                .await
                                .map(|_| nodes)
                                .map_err(|error| error.to_string());
                                update(
                                    &sender,
                                    UiUpdate::ToolFacts {
                                        thread_id: target,
                                        generation,
                                        revision,
                                        result,
                                    },
                                )
                                .await;
                            },
                        ) {
                            update(
                                &updates,
                                UiUpdate::ToolFacts {
                                    thread_id,
                                    generation,
                                    revision,
                                    result: Err("tool state read queue is full".into()),
                                },
                            )
                            .await;
                        }
                    }
                    UiAction::RebaseStream(request) => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = request.thread_id.clone();
                        let rejected = target.clone();
                        let generation = request.generation;
                        let revision = request.view_revision;
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                if let Err(error) =
                                    streaming::rebase(&control, &sender, *request, render_width).await
                                {
                                    load_failed_at(&sender, target, generation, Some(revision), error)
                                        .await;
                                }
                            },
                        ) {
                            load_failed_at(
                                &updates,
                                rejected,
                                generation,
                                Some(revision),
                                ApiError::new(
                                    ErrorCode::InvalidState,
                                    "History read queue is busy; scroll to retry",
                                ),
                            )
                            .await;
                        }
                    }
                    UiAction::RebaseHistory {
                        thread_id,
                        generation,
                        view_revision,
                        anchor,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                let reply = WindowReply::new(target, generation, view_revision);
                                if let Err(error) = HistoryReader::new(&control, &sender, render_width)
                                    .rebase(&reply, anchor)
                                    .await
                                {
                                    reply.failed(&sender, error).await;
                                }
                            },
                        ) {
                            load_failed_at(
                                &updates,
                                thread_id,
                                generation,
                                Some(view_revision),
                                ApiError::new(
                                    ErrorCode::InvalidState,
                                    "History read queue is busy; scroll to retry",
                                ),
                            )
                            .await;
                        }
                    }
                    UiAction::LoadHistoryWindow {
                        thread_id,
                        generation,
                        view_revision,
                        ordinal,
                        char_offset,
                        from_end,
                        selection_revision,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                let reply = WindowReply::new(target, generation, view_revision);
                                let query = WindowRequest::new(
                                    ordinal,
                                    char_offset,
                                    from_end,
                                    selection_revision,
                                );
                                if let Err(error) = HistoryReader::new(&control, &sender, render_width)
                                    .window(&reply, query)
                                    .await
                                {
                                    reply.failed(&sender, error).await;
                                }
                            },
                        ) {
                            load_failed_at(
                                &updates,
                                thread_id,
                                generation,
                                Some(view_revision),
                                ApiError::new(ErrorCode::InvalidState, "History read queue is busy"),
                            )
                            .await;
                        }
                    }
                    UiAction::LoadDetailLinks { thread_id, generation, request_id, node_ids } => {
                        let control=control.clone();
                        let sender=updates.worker();
                        let target=thread_id.clone();
                        if !spawn_read(&mut read_workers,&read_slots,&selection.cancel,async move {
                            let result=HistoryReader::new(&control,&sender,render_width).links(node_ids).await.map_err(|error|error.to_string());
                            update(&sender,UiUpdate::DetailLinks {thread_id:target,generation,request_id,result}).await;
                        }) {
                            update(&updates,UiUpdate::DetailLinks {thread_id,generation,request_id,result:Err("リンクの読込キューが満杯です。完了後に再操作してください".into())}).await;
                        }
                    }
                    UiAction::LoadToolPreview {
                        thread_id,
                        generation,
                        key,
                        request_revision,
                        source_node_id,
                        group_id,
                        call_id,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        let requested_key = key.clone();
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                let identity_matches = match &requested_key {
                                    ItemKey::Tool { group_id: expected_group, call_id: expected_call } => {
                                        group_id.as_deref() == Some(expected_group.as_str()) && call_id == *expected_call
                                    }
                                    ItemKey::LocalTool { node_id, .. } => node_id == &source_node_id,
                                    ItemKey::Node(_) => false,
                                };
                                let result = if !identity_matches {
                                    Err("出力の表示対象が変わりました。項目を開き直してください".into())
                                } else {
                                    let limit = control.store.config.limits.tool_excerpt_bytes.min(16 * 1024) as usize;
                                    let deadline = Duration::from_millis(control.store.config.io.file_prepare_timeout_ms.min(1000));
                                    match tokio::time::timeout(deadline, streams::preview(&control, &source_node_id, group_id.as_deref(), &call_id, limit)).await {
                                        Ok(Ok(page)) => Ok(tool_preview(page)),
                                        Ok(Err(error)) => Err(error.to_string()),
                                        Err(_) => Err("出力の読込が時間内に終わりませんでした。もう一度開いてください".into()),
                                    }
                                };
                                update(&sender, UiUpdate::ToolPreview {
                                    thread_id: target, generation, key: requested_key, request_revision, result,
                                }).await;
                            },
                        ) {
                            update(&updates, UiUpdate::ToolPreview {
                                thread_id, generation, key, request_revision,
                                result: Err("出力の読込キューが満杯です。完了後に再操作してください".into()),
                            }).await;
                        }
                    }
                    UiAction::LoadNodeWindow {
                        thread_id,
                        generation,
                        node_id,
                        char_offset,
                        view_revision,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                let reply = WindowReply::new(target.clone(), generation, view_revision);
                                match HistoryReader::new(&control, &sender, render_width)
                                    .node(node_id, char_offset, &target)
                                    .await
                                {
                                    Ok(node) => reply.deliver_node(&sender, node).await,
                                    Err(error) => reply.failed(&sender, error).await,
                                }
                            },
                        ) {
                            load_failed_at(
                                &updates,
                                thread_id,
                                generation,
                                Some(view_revision),
                                ApiError::new(ErrorCode::InvalidState, "History read queue is busy"),
                            )
                            .await;
                        }
                    }
                    UiAction::RecallInput {
                        thread_id,
                        generation,
                        request_id,
                        before_ordinal,
                        before_node_id,
                        older,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        let target = thread_id.clone();
                        if !spawn_read(
                            &mut read_workers,
                            &read_slots,
                            &selection.cancel,
                            async move {
                                let result = input_history::recall(
                                    &control,
                                    target.clone(),
                                    before_ordinal,
                                    before_node_id,
                                    older,
                                )
                                .await
                                .map_err(|error| error.to_string());
                                update(
                                    &sender,
                                    UiUpdate::RecalledInput {
                                        thread_id: target,
                                        generation,
                                        request_id,
                                        result,
                                    },
                                )
                                .await;
                            },
                        ) {
                            update(
                                &updates,
                                UiUpdate::RecalledInput {
                                    thread_id,
                                    generation,
                                    request_id,
                                    result: Err(
                                        "入力履歴の読込キューが満杯です。完了後に再操作してください"
                                            .into(),
                                    ),
                                },
                            )
                            .await;
                        }
                    }
                    UiAction::SaveDraft {
                        thread_id,
                        draft,
                        anchor,
                        follow,
                    } => {
                        images.associate(&draft.draft_id, &thread_id);
                        images.release(
                            &draft.draft_id,
                            draft.revision,
                            &draft
                                .attachments
                                .iter()
                                .map(|attachment| attachment.id.clone())
                                .collect(),
                        );
                        if draft_saver.enqueue(
                            thread_id.clone(),
                            draft,
                            anchor,
                            follow,
                            actions.is_closing(),
                        ) {
                        } else {
                            update(
                                &updates,
                                UiUpdate::DraftSaveFailed {
                                    thread_id,
                                    message: draft_saver.rejected_message().into(),
                                },
                            )
                            .await;
                        }
                    }
                    UiAction::DraftAttachments {
                        draft_id,
                        revision,
                        attachment_ids,
                    } => {
                        images.release(&draft_id, revision, &attachment_ids.into_iter().collect());
                    }
                    UiAction::ClipboardDecision {
                        draft_id,
                        attachment_id,
                        accepted,
                    } => {
                        images.decision(draft_id, attachment_id, accepted);
                    }
                    UiAction::PasteImage {
                        draft_id,
                        revision,
                        position,
                        thread_id,
                    } => {
                        let Ok(permit) = clipboard_slots.clone().try_acquire_owned() else {
                            command_outcome::<()>(
                                &updates,
                                submission,
                                &Err(ApiError::new(
                                    ErrorCode::InvalidState,
                                    "画像の準備中です。完了後に再操作してください",
                                )),
                            )
                            .await;
                            if !has_submission {
                                update(
                                    &updates,
                                    UiUpdate::DraftStatus {
                                        draft_id,
                                        text: "画像の準備中です。完了後に再操作してください".into(),
                                    },
                                )
                                .await;
                            }
                            continue;
                        };
                        update(
                            &updates,
                            UiUpdate::DraftStatus {
                                draft_id: draft_id.clone(),
                                text: "クリップボード画像を保存中…".into(),
                            },
                        )
                        .await;
                        let ticket = images.begin_image(&draft_id, thread_id.as_deref());
                        let clipboard = clipboard.clone();
                        let attachments = control.attachments.clone();
                        let timeout = control.store.config.io.file_prepare_timeout_ms;
                        let cancel = cancel.child_token();
                        let sender = background_tx.clone();
                        let outcomes = updates.worker();
                        spawn_work(&mut workers, permit, async move {
                            let result = async {
                                let pixels = clipboard.capture(timeout, cancel.clone()).await?;
                                attachments.save_clipboard_pixels(pixels, cancel).await
                            }
                            .await;
                            command_outcome(&outcomes, submission, &result).await;
                            {
                                let _ = sender
                                    .send(BackgroundResult::Image {
                                        ticket,
                                        draft_id,
                                        revision,
                                        position,
                                        error_reported: has_submission,
                                        result,
                                    })
                                    .await;
                            }
                        });
                    }
                    UiAction::CloseUsageDetails { epoch } => management.close_usage_at(epoch),
                    UiAction::CancelWait => {
                        management.cancel_wait();
                        status(
                            &updates,
                            None,
                            "待機の観測を取消しました。対象処理は継続します",
                        )
                        .await;
                    }
                    UiAction::LoadOriginal {
                        node_id,
                        cursor,
                        request_id,
                    } => {
                        let control = control.clone();
                        let sender = updates.worker();
                        if !spawn_read(&mut read_workers, &read_slots, &read_cancel, async move {
                            match request(
                                &control,
                                Command::HistoryGet(HistoryGetArgs { final_answer_only: false,
                                    node_id,
                                    cursor,
                                    limit_bytes: MAX_BODY_BYTES,
                                    encoding: DEFAULT_ENCODING.into(),
                                }),
                            )
                            .await
                            .and_then(|value| {
                                serde_json::from_value(value).map_err(|_| {
                                    ApiError::new(ErrorCode::IoFailed, "Invalid original page")
                                })
                            }) {
                                Ok(body) => match presentation_work(move || {
                                    UiUpdate::original_page(request_id, body)
                                })
                                .await
                                {
                                    Ok(page) => update(&sender, page).await,
                                    Err(error) => status(&sender, None, error).await,
                                },
                                Err(error) => status(&sender, None, error).await,
                            }
                        }) {
                            status(&updates, None, "原文読込キューが満杯です").await;
                        }
                    }
                    UiAction::FollowManagement {
                        request_id,
                        pointer,
                        mode,
                    } => {
                        if request_id != management.request_id {
                            management_status(
                                &updates,
                                context.clone(),
                                "結果が更新されました。現在の一覧から選択してください",
                            )
                            .await;
                            continue;
                        }
                        let Some(value) = management.detail.clone() else {
                            management_status(
                                &updates,
                                context.clone(),
                                "詳細が未保持です。範囲を絞って再照会してください",
                            )
                            .await;
                            continue;
                        };
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            management_status(&updates, context.clone(), "操作キューが満杯です").await;
                            continue;
                        };
                        let generation = management.next_generation();
                        let control = control.clone();
                        let sender = background_tx.clone();
                        let follow_updates = updates.worker();
                        spawn_work(&mut workers, permit, async move {
                            let result =
                                presentation_work(move || management::follow(&value, &pointer, mode))
                                    .await;
                            let command = match result {
                                Ok(Ok(command)) => command,
                                Ok(Err(error)) | Err(error) => {
                                    management_status(&follow_updates, context.clone(), error).await;
                                    return;
                                }
                            };
                            let title = command_title(&command);
                            let work_status = management::work_status(&command);
                            let observed =
                                match management_request(&control, command, CancellationToken::new())
                                    .await
                                {
                                    Ok(value) => value,
                                    Err(error) => {
                                        management_status(&follow_updates, context.clone(), error)
                                            .await;
                                        return;
                                    }
                                };
                            if let Some(error) = observed.get("error") {
                                management_status(
                                    &follow_updates,
                                    context.clone(),
                                    error
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("参照を取得できません。Rで一覧を更新してください"),
                                )
                                .await;
                                return;
                            }
                            let result =
                                prepare_management(Ok(observed), control.store.config.cache.body_bytes)
                                    .await;
                            let _ = sender
                                .send(BackgroundResult::Command {
                                    work_status,
                                    context,
                                    generation,
                                    title,
                                    result,
                                })
                                .await;
                        });
                    }
                    UiAction::LoadManagementDetail {
                        request_id,
                        pointer,
                        cursor,
                    } => {
                        if request_id != management.request_id {
                            management_status(
                                &updates,
                                context.clone(),
                                "管理結果が更新されました。現在の結果から選択してください",
                            )
                            .await;
                            continue;
                        }
                        let Some(value) = management.detail.clone() else {
                            management_status(&updates,context.clone(),"詳細は本文cache予算を超えたため未保持。対象IDから原文pageを取得するか範囲を絞って再照会してください").await;
                            continue;
                        };
                        let sender = updates.worker();
                        let worker_context = context.clone();
                        if !spawn_read(&mut read_workers, &read_slots, &read_cancel, async move {
                            let selected = pointer.clone();
                            match presentation_work(move || {
                                management::detail_page(&value, &selected, cursor.as_deref())
                            })
                            .await
                            {
                                Ok(Ok((text, next_cursor))) => {
                                    management_update(
                                        &sender,
                                        worker_context.clone(),
                                        UiUpdate::ManagementDetail {
                                            request_id,
                                            pointer,
                                            text,
                                            next_cursor,
                                        },
                                    )
                                    .await
                                }
                                Ok(Err(error)) | Err(error) => {
                                    management_status(&sender, worker_context.clone(), error).await
                                }
                            }
                        }) {
                            management_status(&updates, context.clone(), "詳細読込キューが満杯です")
                                .await;
                        }
                    }
                    UiAction::CopyText { text, scope } => {
                        let Ok(permit) = clipboard_slots.clone().try_acquire_owned() else {
                            management_status(&updates, context.clone(), "クリップボードの準備中です")
                                .await;
                            continue;
                        };
                        let control = control.clone();
                        let clipboard = clipboard.clone();
                        let sender = background_tx.clone();
                        let cancel = cancel.child_token();
                        spawn_work(&mut workers, permit, async move {
                            let result = clipboard
                                .copy(
                                    text,
                                    control.store.config.ui.copy_backend,
                                    MAX_BODY_BYTES as usize,
                                    control.store.config.io.file_prepare_timeout_ms,
                                    cancel,
                                )
                                .await;
                            let _ = sender
                                .send(BackgroundResult::Copied { scope, result })
                                .await;
                        });
                    }
                    UiAction::Copy {
                        node_id,
                        code_block,
                    } => {
                        let scope = if code_block.is_some() {
                            CopyScope::CodeBlock
                        } else {
                            CopyScope::Body
                        };
                        let Ok(permit) = clipboard_slots.clone().try_acquire_owned() else {
                            command_outcome::<()>(
                                &updates,
                                submission,
                                &Err(ApiError::new(
                                    ErrorCode::InvalidState,
                                    "クリップボードの準備中です。完了後に再操作してください",
                                )),
                            )
                            .await;
                            if !has_submission {
                                management_status(
                                    &updates,
                                    context.clone(),
                                    "クリップボードの準備中です。完了後に再操作してください",
                                )
                                .await;
                            }
                            continue;
                        };
                        let control = control.clone();
                        let clipboard = clipboard.clone();
                        let sender = background_tx.clone();
                        let outcomes = updates.worker();
                        let cancel = cancel.child_token();
                        spawn_work(&mut workers, permit, async move {
                            let result = async {
                                let text = copy_source(&control, node_id, code_block).await?;
                                clipboard
                                    .copy(
                                        text,
                                        control.store.config.ui.copy_backend,
                                        MAX_BODY_BYTES as usize,
                                        control.store.config.io.file_prepare_timeout_ms,
                                        cancel,
                                    )
                                    .await
                            }
                            .await;
                            command_outcome(&outcomes, submission, &result).await;
                            if !has_submission || result.is_ok() {
                                let _ = sender
                                    .send(BackgroundResult::Copied { scope, result })
                                    .await;
                            }
                        });
                    }
                    UiAction::Cancel { thread_id } => {
                        if !control_tasks.is_empty() {
                            status(
                                &updates,
                                Some(thread_id),
                                "中断操作を処理中です。対象状態を確認して再操作してください",
                            )
                            .await;
                            continue;
                        };
                        let control = control.clone();
                        control_tasks.spawn(async move {
                            let result = control.cancel_thread(thread_id.clone()).await;
                            (thread_id, result)
                        });
                    }
                    UiAction::Background { thread_id } => {
                        let Ok(permit) = command_slots.clone().try_acquire_owned() else {
                            command_outcome::<()>(
                                &updates,
                                submission,
                                &Err(ApiError::new(
                                    ErrorCode::InvalidState,
                                    "操作キューが満杯です。背景化は未受付です",
                                )),
                            )
                            .await;
                            if !has_submission {
                                status(
                                    &updates,
                                    Some(thread_id),
                                    "操作キューが満杯です。背景化は未受付です",
                                )
                                .await;
                            }
                            continue;
                        };
                        let control = control.clone();
                        let updates = updates.worker();
                        spawn_work(&mut workers, permit, async move {
                            let result = control.background(&thread_id).await;
                            command_outcome(&updates, submission, &result).await;
                            if let Err(error) = result
                                && !has_submission
                            {
                                status(&updates, Some(thread_id), error).await;
                            }
                        });
                    }
                }
            },
            result=background_rx.recv(),if !receipt_batch.active() && updates.main_ready()=> match result {
                Some(BackgroundResult::ThreadDeletion { request_id, args, execute, mut known_thread_ids, result }) => {
                    let result = match result {
                        Ok(thread_deletion::Outcome::Preview(value)) => Ok(ThreadDeletionReply::Preview(value)),
                        Ok(thread_deletion::Outcome::Applied) => {
                            known_thread_ids.push(args.thread_id.clone());
                            known_thread_ids = thread_deletion::candidates(known_thread_ids, &selection,
                                draft_saver.threads().chain(images.threads()).cloned());
                            let (deleted, drafts, warning) = thread_deletion::finish_cleanup(&control, &args.thread_id, known_thread_ids).await;
                            let ids: HashSet<_> = deleted.iter().cloned().collect();
                            draft_saver.forget_threads(&ids);
                            images.forget_threads(&ids, &drafts);
                            selection.generations.retain(|thread, _| !ids.contains(thread));
                            dirty_threads.retain(|thread| !ids.contains(thread));
                            refreshing.retain(|thread| !ids.contains(thread));
                            pending.retain(|_, value| !ids.contains(&value.thread));
                            if selection.active_thread.as_ref().is_some_and(|thread| ids.contains(thread)) {
                                ui_subscription.select(None);
                                selection.activate_draft(uuid::Uuid::new_v4().to_string());
                                streams.clear();
                                stream_retry = None;
                            }
                            Ok(match warning {
                                Some(warning) => ThreadDeletionReply::DeletedWithWarning { ids: deleted, warning },
                                None => ThreadDeletionReply::Deleted(deleted),
                            })
                        }
                        Err(error) => Err(error),
                    };
                    update(&updates, UiUpdate::ThreadDeletion { request_id, args, execute, result }).await;
                }
                Some(BackgroundResult::StreamFrame { ticket, result }) => {
                    stream_inflight = false;
                    match result {
                        Ok(streaming::Outcome::Queued { ended: true }) => {
                            streams.remove(&ticket);
                            dirty_threads.insert(ticket.thread);
                        }
                        Ok(
                            streaming::Outcome::Queued { ended: false }
                            | streaming::Outcome::Cancelled
                            | streaming::Outcome::Unchanged,
                        ) => {}
                        Ok(streaming::Outcome::Full(frame)) => {
                            if selection.active_thread.as_deref() == Some(&ticket.thread)
                                && selection.generations.get(&ticket.thread) == Some(&ticket.generation)
                                && (frame.ended() || streams.current(&ticket, render_width))
                            {
                                stream_retry = Some(frame);
                            }
                        }
                        Ok(streaming::Outcome::Closed) => break,
                        Err(error) => status(&updates, Some(ticket.thread), error).await,
                    }
                }
                Some(BackgroundResult::DraftSaved {
                    thread_id,
                    result: Err(error),
                }) => {
                    if error.is_storage_failure() {
                        update(&updates,UiUpdate::StorageFailed{thread_id,message:format!("正本DBの保存に失敗しました。新規実行を停止し資源を回収しています。下書き本文は画面に保持しています: {error}")}).await;
                    } else {
                        update(
                            &updates,
                            UiUpdate::DraftSaveFailed {
                                thread_id,
                                message: error.to_string(),
                            },
                        )
                        .await;
                    }
                }
                Some(BackgroundResult::DraftSaved { result: Ok(()), .. }) => {}
                Some(BackgroundResult::Refreshed {
                    thread_id,
                    receipts,
                    progress,
                    epoch,
                    generation,
                }) => {
                    refreshing.remove(&thread_id);
                    if let (Some(result), Some(generation)) = (progress, generation) {
                        selection
                            .publish_activity(&updates, thread_id.clone(), generation, epoch, result)
                            .await;
                    }
                    match receipts {
                        Ok(receipts) => {
                            receipt_batch = receipt_delivery::Batch::new(receipts);
                            if receipt_delivery::continue_scan(&mut pending, &thread_id) {
                                dirty_threads.insert(thread_id);
                            }
                        }
                        Err(error) => status(&updates, Some(thread_id), error).await,
                    }
                }
                Some(BackgroundResult::Progress {
                    thread_id,
                    result,
                    epoch,
                    generation,
                }) => {
                    selection
                        .publish_activity(&updates, thread_id, generation, epoch, result)
                        .await
                }
                Some(BackgroundResult::Sent {
                    generation, projection,
                    draft_id,
                    revision,
                    attachments,
                    result,
                }) => match result {
                    Ok(value) => {
                        images.sent(&draft_id, &attachments);
                        let thread = value
                            .get("thread_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        if let Some(projection)=projection {
                            update(&updates,*projection).await;
                        } else {
                            if let Some(thread_id)=thread.clone() {update(&updates,UiUpdate::SavedMessagePending {draft_id:draft_id.clone(),revision,generation,thread_id}).await;}
                        }
                        if let Some(node_id) = value.get("message_id").and_then(Value::as_str) {
                            update(
                                &updates,
                                UiUpdate::ReceiptSource {
                                    draft_id: draft_id.clone(),
                                    revision,
                                    node_id: node_id.into(),
                                },
                            )
                            .await;
                        }
                        if let Some(thread) = thread {
                            images.associate(&draft_id, &thread);
                            selection.sent(&thread, &draft_id);
                            dirty_threads.insert(thread);
                        }
                    }
                    Err(error) => {
                        if error
                            .details
                            .as_ref()
                            .and_then(|v| v.get("confirmation_required"))
                            .and_then(Value::as_bool)
                            == Some(true)
                            && let Some(run_id) = error
                                .details
                                .as_ref()
                                .and_then(|v| v.get("run_id"))
                                .and_then(Value::as_str)
                        {
                            update(
                                &updates,
                                UiUpdate::EditConfirmationRequired {
                                    draft_id: draft_id.clone(),
                                    revision,
                                    run_id: run_id.into(),
                                },
                            )
                            .await;
                        }
                        update(
                            &updates,
                            UiUpdate::Receipt {
                                draft_id,
                                revision,
                                thread_id: None,
                                status: ReceiptStatus::Failed,
                                message: Some(crate::config_setup::tui_error(&error)),
                            },
                        )
                        .await;
                    }
                },
                Some(BackgroundResult::Image {
                    ticket,
                    draft_id,
                    revision,
                    position,
                    error_reported,
                    result,
                }) => {
                    if !images.accept_image(ticket) { continue; }
                    match result {
                    Ok(image) => {
                        let attachment = UiAttachment {
                            id: image.id.clone(),
                            path: image.path.clone(),
                            kind: "image".into(),
                            state: "ready".into(),
                            position: Some(position),
                        };
                        images.insert(draft_id.clone(), revision, image);
                        update(
                            &updates,
                            UiUpdate::ClipboardAttachment {
                                draft_id,
                                revision,
                                attachment,
                            },
                        )
                        .await;
                    }
                    Err(error) if !error_reported => {
                        update(
                            &updates,
                            UiUpdate::DraftStatus {
                                draft_id,
                                text: error.to_string(),
                            },
                        )
                        .await
                    }
                    Err(_) => {}
                    }
                    images.prune_owners();
                },
                Some(BackgroundResult::Copied {
                    scope,
                    result: Ok(CopyOutcome::OsCopied),
                }) => status(&updates, None, format!("{}をコピーしました", scope.label())).await,
                Some(BackgroundResult::Copied {
                    scope,
                    result: Ok(CopyOutcome::TerminalRequest(bytes)),
                }) => {
                    update(&updates, UiUpdate::TerminalRequest { bytes }).await;
                    status(
                        &updates,
                        None,
                        format!(
                            "OSC52: {}のコピー要求を端末へ送りました（完了未確認）",
                            scope.label()
                        ),
                    )
                    .await;
                }
                Some(BackgroundResult::Copied {
                    result: Err(error), ..
                }) => status(&updates, None, error).await,
                Some(BackgroundResult::UsageRefresh{context,generation,title,result})=>{
                    if !management.accepts(generation) || !management.usage_accepts(generation){continue;}
                    match result {
                        Ok(report)=>{
                            management.usage_finished(generation,report.detail.as_ref());
                            let request_id=management.begin_result();
                            management.detail=report.detail.map(Arc::new);
                            management_update(&updates,Some(context),UiUpdate::StructuredManagement{work_status:false,request_id,title,fields:report.fields,omitted:report.omitted}).await;
                        },
                        Err(error)=>{
                            management.usage_finished(generation,None);
                            management_status(&updates,Some(context),error).await;
                        },
                    }
                },
                Some(BackgroundResult::Command {
                    work_status,
                    context,
                    generation,
                    title,
                    result,
                }) => {
                    if !management.accepts(generation) {
                        continue;
                    }
                    management.usage_finished(generation,None);
                    let management_id = management.begin_result();
                    match result {
                        Ok(report) => {
                            management.usage_finished(generation,report.detail.as_ref());
                            management.detail = report.detail.map(Arc::new);
                            management_update(
                                &updates,
                                context,
                                UiUpdate::StructuredManagement {
                                    work_status,
                                    request_id: management_id,
                                    title,
                                    fields: report.fields,
                                    omitted: report.omitted,
                                },
                            )
                            .await;
                        }
                        Err(error) => {
                            management_update(
                                &updates,
                                context,
                                UiUpdate::Management {
                                    title,
                                    text: format!(
                                        "{}\ncode: {:?}\nhint: {}",
                                        error.message,
                                        error.code,
                                        error.hint.unwrap_or_default()
                                    ),
                                },
                            )
                            .await
                        }
                    }
                }
                Some(BackgroundResult::IndexPreview {
                    context,
                    args,
                    result,
                }) => match result {
                    Ok(text) => {
                        management_update(
                            &updates,
                            context,
                            UiUpdate::IndexPreviewReady { args, text },
                        )
                        .await
                    }
                    Err(error) => {
                        management_update(
                            &updates,
                            context,
                            UiUpdate::IndexPreviewFailed { args, error: error.to_string() },
                        )
                        .await
                    }
                },
                Some(BackgroundResult::EditRequest {
                    thread_id,
                    node_id,
                    generation,
                    draft_revision,
                    result,
                }) => match result {
                    Ok(edit) => {
                        update(
                            &updates,
                            UiUpdate::EditRequestLoaded {
                                thread_id,
                                node_id,
                                generation,
                                draft_revision,
                                text: edit.text,
                                attachments: edit.attachments,
                                run_id: edit.run_id,
                            },
                        )
                        .await
                    }
                    Err(error) => status(&updates, Some(thread_id), error).await,
                },
                None => {}
            },
            event=ui_subscription.receiver.recv()=> match event {
                Ok(EngineEvent::Changed { thread_id, recovery }) => {
                    if recovery && control.store.config.ui.bell {
                        update(&updates, UiUpdate::RecoveryBell).await;
                    }
                    management.usage_changed(&thread_id);
                    streams.invalidate(&thread_id);
                    selection.changed(&thread_id);
                    receipt_delivery::changed(&mut pending, &thread_id);
                    dirty_threads.insert(thread_id);
                }
                Ok(EngineEvent::Stream {
                    thread_id,
                    node_id,
                    text,
                    reasoning,
                    stream_char_offset,
                }) => {
                    if selection.active_thread.as_deref() == Some(&thread_id)
                        && selection.generations.contains_key(&thread_id)
                    {
                        streams.observe(thread_id, node_id, stream_char_offset, &text, reasoning);
                    }
                }
                Ok(EngineEvent::ModelReceive { thread_id, attempt_id, observed_at }) => {
                    if selection.active_thread.as_deref() == Some(&thread_id)
                        && let Some(generation) = selection.generations.get(&thread_id).copied()
                    {
                        update(&updates, UiUpdate::ModelReceive {
                            thread_id, generation, attempt_id, observed_at,
                        }).await;
                    }
                }
                Ok(EngineEvent::Error { thread_id, error }) => {
                    status(&updates, Some(thread_id), error).await
                }
                Ok(EngineEvent::Notice { thread_id, foreground_release, cancelled_calls }) => {
                    let release = foreground_release.as_ref().map(|(execution, reason)| (execution.as_str(), *reason));
                    status(&updates, Some(thread_id), tool_group_notice(release, cancelled_calls)).await
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    management.usage_lagged();
                    #[cfg(test)] stream_metrics::FRONTEND_LAGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let Some(thread) = &selection.active_thread {
                        streams.invalidate(thread);
                    }
                    for receipt in pending.values_mut() { receipt.repeat = true; }
                    dirty_threads.extend(pending.values().map(|value| value.thread.clone()));
                    if let Some(thread) = &selection.active_thread {
                        dirty_threads.insert(thread.clone());
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _=tokio::time::sleep_until(stream_next),if !stream_inflight && (stream_retry.is_some() || streams.pending())=> {
                let retry = stream_retry
                    .take()
                    .filter(|frame| frame.ended() || streams.current(&frame.ticket, render_width));
                let frame = retry.map(|frame| *frame).or_else(|| {
                    selection
                        .active_thread
                        .as_ref()
                        .and_then(|thread| selection.generations.get(thread))
                        .and_then(|generation| streams.take(*generation, render_width))
                });
                if let Some(frame) = frame {
                    stream_inflight = true;
                    stream_next = tokio::time::Instant::now() + stream_period;
                    let control = control.clone();
                    let sender = updates.worker();
                    let results = background_tx.clone();
                    let stop = selection.cancel.child_token();
                    let ticket = frame.ticket.clone();
                    read_workers.spawn(async move {
                        let result = frame.deliver(&control, sender, stop).await;
                        let _ = results
                            .send(BackgroundResult::StreamFrame { ticket, result })
                            .await;
                    });
                }
            },
            _=tick.tick(),if selection.progress_running || management.usage_waiting()=> {
                if selection.progress_running && let Some(thread) = selection.active_thread.clone()
                    && let Some(generation) = selection.generations.get(&thread).copied()
                    && let Ok(permit) = progress_slots.clone().try_acquire_owned()
                {
                    let control = control.clone();
                    let sender = background_tx.clone();
                    let stop = selection.cancel.clone();
                    let epoch = selection.progress_epoch;
                    read_workers.spawn(async move {
                                let _permit = permit;
                                tokio::select! {
                                    biased;
                                    _ = stop.cancelled() => {},
                                    result = activity::read(&control, &thread) => {
                                        let _ = sender.send(BackgroundResult::Progress {
                                            thread_id: thread, result, epoch, generation,
                                        }).await;
                                    },
                                }
                            });
                }
            },
            _=read_workers.join_next(),if !read_workers.is_empty()=> {},
            _=workers.join_next(),if !workers.is_empty()=> {},
            _=send_workers.join_next(),if !send_workers.is_empty()=> {},
        }
    }
    drop(ui_subscription);
    read_cancel.cancel();
    selection.cancel.cancel();
    cancel.cancel();
    drop(background_rx);
    let grace = Duration::from_millis(control.store.config.io.shutdown_grace_ms);
    let shutdown_updates = &updates;
    let shutdown_diagnostics = owner.diagnostics.clone();
    let shutdown = async move {
        use crate::diagnostics::{ShutdownBoundary as Edge, ShutdownScope, ShutdownStage as Stage};
        let started = std::time::Instant::now();
        let trace = |stage, boundary| {
            shutdown_diagnostics.record_shutdown(
                ShutdownScope::Frontend,
                stage,
                boundary,
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            )
        };
        // Durable work precedes presentation-only waits; the outer select still
        // flushes the bounded delivery and accepts explicit shutdown choices.
        trace(Stage::Listener, Edge::Begin);
        let server_result = server.shutdown().await;
        trace(Stage::Listener, Edge::End);
        // Coalesced autosaves retain the newest snapshot per Thread; FIFO
        // database admission within each Thread is preserved on shutdown too.
        trace(Stage::DraftEnqueue, Edge::Begin);
        let save_worker = draft_saver.finish().await;
        trace(Stage::DraftEnqueue, Edge::End);
        // Retain accepted-send workers until their durable commit/result closes.
        trace(Stage::AcceptedSends, Edge::Begin);
        while send_workers.join_next().await.is_some() {}
        trace(Stage::AcceptedSends, Edge::End);
        trace(Stage::DraftSave, Edge::Begin);
        let save_result = save_worker
            .await
            .map_err(|_| ApiError::new(ErrorCode::SaveFailed, "Draft save worker failed"))
            .and_then(|result| result);
        trace(Stage::DraftSave, Edge::End);
        // Preserve registered waits/owned work before cancellation erases them.
        // Failure is returned by Control::shutdown after drain still completes.
        trace(Stage::InterruptCapture, Edge::Begin);
        let _ = control.prepare_shutdown().await;
        trace(Stage::InterruptCapture, Edge::End);
        trace(Stage::ControlTasks, Edge::Begin);
        while control_tasks.join_next().await.is_some() {}
        trace(Stage::ControlTasks, Edge::End);
        trace(Stage::ReadWorkers, Edge::Begin);
        while read_workers.join_next().await.is_some() {}
        trace(Stage::ReadWorkers, Edge::End);
        trace(Stage::Images, Edge::Begin);
        drop(images);
        trace(Stage::Images, Edge::End);
        trace(Stage::Control, Edge::Begin);
        let control_result = control.shutdown().await;
        shutdown_diagnostics.record_shutdown_result(
            ShutdownScope::Frontend,
            Stage::Control,
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            control_result.as_ref().err().map(|error| error.code),
        );
        trace(Stage::Workers, Edge::Begin);
        while workers.join_next().await.is_some() {}
        trace(Stage::Workers, Edge::End);
        // Owner closes the diagnostic writer last; its inner stages explain
        // this final await, which cannot be logged after that close.
        trace(Stage::Owner, Edge::Begin);
        let owner_result = owner.shutdown().await;
        if let Some(value) = initialize {
            update(shutdown_updates, value).await;
        }
        receipt_batch.finish(&mut pending, shutdown_updates).await;
        save_result
            .and(server_result)
            .and(control_result)
            .and(owner_result)
    };
    wait_for_shutdown_delivery(&mut actions, &updates, grace, shutdown).await
}

// Drive delivery while the shutdown future is pending, not just the active loop.
async fn wait_for_shutdown_delivery<F: Future<Output = Result<(), ApiError>>>(
    actions: &mut crate::ui::actions::ActionReceiver,
    updates: &BufferedUpdates,
    grace: Duration,
    shutdown: F,
) -> Result<(), ApiError> {
    let result = {
        let waiting = wait_for_shutdown_updates(actions, updates, grace, shutdown);
        tokio::pin!(waiting);
        loop {
            tokio::select! {
                result = &mut waiting => break result,
                _ = updates.notified() => updates.flush(),
                _ = tokio::time::sleep(Duration::from_millis(5)), if updates.pending() => updates.flush(),
            }
        }
    };
    updates.finish(actions, result).await
}

/// Keep the terminal responsive while owned shutdown proceeds. A force request
/// becomes available only after the configured grace and is always explicit.
pub async fn wait_for_shutdown<F: Future<Output = Result<(), ApiError>>>(
    actions: &mut impl crate::ui::actions::ActionStream,
    updates: &SyncSender<UiUpdate>,
    grace: Duration,
    shutdown: F,
) -> Result<(), ApiError> {
    wait_for_shutdown_updates(actions, updates, grace, shutdown).await
}
async fn wait_for_shutdown_updates<F: Future<Output = Result<(), ApiError>>>(
    actions: &mut impl crate::ui::actions::ActionStream,
    updates: &(impl UpdateTarget + ?Sized),
    grace: Duration,
    shutdown: F,
) -> Result<(), ApiError> {
    let started = tokio::time::Instant::now();
    let mut next_choice = started + grace;
    let mut force_available = false;
    let mut actions_open = true;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {biased;
            result=&mut shutdown=>{
                return finish_shutdown_updates(actions, updates, result).await;
            },
            action=actions.next_action(),if actions_open=>match action {
                Some(UiAction::ForceQuit) if force_available=>{
                    // Force is explicit: reporting it must never wait for a stalled UI.
                    let _ = updates.defer(UiUpdate::ForceShutdown);
                    return Err(ApiError::new(ErrorCode::InvalidState,"Forced shutdown selected; saved data and owned process collection may be incomplete").details(serde_json::json!({"forced_shutdown":true})));
                },
                Some(UiAction::WaitForShutdown)=>{
                    force_available=false;next_choice=tokio::time::Instant::now()+grace;
                    update(updates,UiUpdate::ShutdownProgress{elapsed_ms:started.elapsed().as_millis().min(u64::MAX as u128)as u64,detail:"停止・保存を継続しています".into(),force_available:false}).await;
                },
                None=>actions_open=false,
                _=>{},
            },
            _=tick.tick()=>{
                if tokio::time::Instant::now()>=next_choice{force_available=true;}
                update(updates,UiUpdate::ShutdownProgress{elapsed_ms:started.elapsed().as_millis().min(u64::MAX as u128)as u64,detail:if force_available{"停止・保存が猶予時間を超えました。待機継続または強制終了を選択してください。強制終了では未保存・未回収が残る可能性があります".into()}else{"新規受付を停止し、所有処理・下書き・出力を保存して解放しています".into()},force_available}).await;
            },
        }
    }
}

// Owned work has finished. Retain at most its error status and terminal marker;
// waiting for presentation capacity must still honor one explicit ForceQuit.
async fn finish_shutdown_updates(
    actions: &mut impl crate::ui::actions::ActionStream,
    updates: &(impl UpdateTarget + ?Sized),
    result: Result<(), ApiError>,
) -> Result<(), ApiError> {
    let terminal = result
        .as_ref()
        .err()
        .map(|error| UiUpdate::Status {
            thread_id: None,
            text: error.to_string(),
        })
        .into_iter()
        .chain(std::iter::once(UiUpdate::Shutdown));
    for mut value in terminal {
        while let Some(retained) = updates.defer(value) {
            value = retained;
            updates.flush_pending();
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(5)) => {},
                action = actions.next_action() => {
                    if matches!(action, Some(UiAction::ForceQuit) | None) {
                        return Err(ApiError::new(ErrorCode::InvalidState,
                            "Forced shutdown while terminal UI delivery was pending")
                            .details(serde_json::json!({"forced_shutdown": true})));
                    }
                }
            }
        }
    }
    result
}

async fn save_draft(
    control: &Control,
    thread: String,
    draft: DraftSnapshot,
    anchor: ViewAnchor,
    follow: bool,
) -> Result<(), ApiError> {
    let submission_fence = crate::control::submission_retention::fence("?1", "?3", "?2");
    let result = async {
    control.store.database.call(move|db|{let tx=db.transaction()?;let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",[&thread],|row|row.get(0))?;if !live{return Ok(());}let body=serde_json::to_string(&draft).map_err(|error|crate::storage::StorageError::Configuration(error.to_string()))?;tx.execute(&format!("INSERT INTO draft(id,thread_id,revision,body_json,state,updated_at) SELECT ?1,?2,?3,?4,'editing',?5 WHERE NOT {submission_fence} ON CONFLICT(id)DO UPDATE SET revision=excluded.revision,body_json=excluded.body_json,updated_at=excluded.updated_at WHERE excluded.revision>=draft.revision"),rusqlite::params![draft.draft_id,thread,draft.revision as i64,body,chrono::Utc::now().to_rfc3339()])?;tx.execute("INSERT INTO view_state(thread_id,anchor_node_id,anchor_offset,follow_tail,input_state_json)VALUES(?1,?2,?3,?4,'{}')ON CONFLICT(thread_id)DO UPDATE SET anchor_node_id=excluded.anchor_node_id,anchor_offset=excluded.anchor_offset,follow_tail=excluded.follow_tail",rusqlite::params![thread,anchor.node_id,anchor.char_offset as i64,follow])?;tx.commit()?;Ok(())}).await.map_err(storage_error)

    }.await;
    checked_storage(control, result).await
}
async fn select_thread(
    control: &Arc<Control>,
    updates: &dyn UpdateTarget,
    thread: &str,
    generation: u64,
    view_revision: u64,
    known_view: Option<(ViewAnchor, bool)>,
    render_width: u16,
) -> Result<(), ApiError> {
    let result = async {
    let metadata_target=thread.to_owned();
    let details=control.store.database.call(move|db|crate::control::db::thread_details(db,&metadata_target).map_err(crate::storage::StorageError::from)).await.map_err(storage_error)?;
    let target = thread.to_owned();
    let submission_fence = crate::control::submission_retention::fence("current.id", "current.revision", "?1");
    let (draft,view)=control.store.database.call(move|db|{use rusqlite::OptionalExtension;let body:Option<String>=db.query_row(&format!("SELECT body_json FROM draft current WHERE thread_id=?1 AND state='editing' AND NOT {submission_fence} ORDER BY updated_at DESC LIMIT 1"),[&target],|r|r.get(0)).optional()?;let view=db.query_row("SELECT anchor_node_id,anchor_offset,follow_tail FROM view_state WHERE thread_id=?1",[target],|r|Ok((ViewAnchor{node_id:r.get(0)?,char_offset:r.get::<_,i64>(1)? as usize},r.get::<_,bool>(2)?))).optional()?;Ok((body.and_then(|v|serde_json::from_str::<DraftSnapshot>(&v).ok()),view))}).await.map_err(storage_error)?;
    update(
        updates,
        UiUpdate::Thread {
            generation,
            details,
            draft,
        },
    )
    .await;
    let restore = known_view.is_none();
    if let Some((anchor, follow)) = known_view.or(view) {
        if restore {
            update(
                updates,
                UiUpdate::ViewState {
                    thread_id: thread.into(),
                    generation,
                    view_revision,
                    anchor: anchor.clone(),
                    follow,
                },
            )
            .await;
        }
        HistoryReader::new(control, updates, render_width).page(
            &Reply::new(thread,generation), None, if follow {None} else {Some(anchor)}
        )
        .await?;
    } else {
        HistoryReader::new(control, updates, render_width).page(&Reply::new(thread, generation), None, None).await?;
    }
    Ok(())

    }.await;
    checked_storage(control, result).await
}
pub async fn progress(
    control: &Control,
    updates: &SyncSender<UiUpdate>,
    thread: &str,
) -> Result<bool, ApiError> {
    let projection = activity::read(control, thread).await?;
    update(updates, projection.progress_update(thread)).await;
    Ok(projection.poll_running)
}

fn window(text: &str, offset: Option<usize>) -> (String, usize, usize) {
    let total = text.chars().count();
    let start = if let Some(offset) = offset {
        text.char_indices()
            .nth(offset.min(total).saturating_sub(512))
            .map_or(text.len(), |(index, _)| index)
    } else {
        text.ceil_char_boundary(text.len().saturating_sub(MAX_BODY_BYTES as usize))
    };
    let end = text.floor_char_boundary((start + MAX_BODY_BYTES as usize).min(text.len()));
    (
        text[start..end].into(),
        text[..start].chars().count(),
        total,
    )
}
/// Canonical history text plus visible reasoning ranges; opaque continuations
/// are excluded by the same known-field rule as the history reader.
pub fn display_payload(payload: &Value) -> (String, Vec<(usize, usize)>) {
    let canonical = crate::history::text_of_payload(payload);
    let mut rendered = String::new();
    let mut seen = false;
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    crate::history::payload::visit(payload, |leaf| {
        if seen {
            rendered.push('\n');
        }
        seen = true;
        rendered.push_str(leaf.text);
        if leaf.context.reasoning && leaf.end > leaf.start {
            if let Some(last) = ranges.last_mut()
                && last.1 + 1 == leaf.start
            {
                last.1 = leaf.end;
            } else {
                ranges.push((leaf.start, leaf.end));
            }
        }
    });
    // A future history extractor change must never corrupt canonical anchors.
    if rendered != canonical {
        ranges.clear();
    }
    (canonical, ranges)
}
/// Read payload bytes on the DB worker; decode/project only on the bounded pool.
struct RawNode {
    id: String,
    received_recovery_wait: bool,
    kind: String,
    payload: Option<String>,
    reason: Option<String>,
}
impl RawNode {
    fn decode(&self) -> (Value, String, Vec<(usize, usize)>) {
        match self
            .payload
            .as_ref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
        {
            Some(payload) => {
                let (text, ranges) = if matches!(
                    self.kind.as_str(),
                    "incomplete_response" | "model_stream_observation"
                ) {
                    received::body(&payload)
                } else {
                    display_payload(&payload)
                };
                (payload, text, ranges)
            }
            None => {
                let reason = self
                    .reason
                    .as_deref()
                    .unwrap_or("stored payload unavailable or malformed");
                (
                    serde_json::json!({"tombstone_reason": reason}),
                    format!("[original unavailable: {reason}]"),
                    vec![],
                )
            }
        }
    }
}
fn expired_history_cursor() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "History selection changed; reload from the latest page",
    )
}

// Called only inside the existing bounded presentation worker. Chrono Local
// may consult OS timezone files, including on a cache refresh.
fn thread_display_times(threads: &[ThreadSummary]) -> Vec<crate::ui::ThreadDisplayTime> {
    threads
        .iter()
        .map(|thread| {
            let (date_time, time) = match chrono::DateTime::parse_from_rfc3339(&thread.updated_at) {
                Ok(time) => {
                    let local = time.with_timezone(&chrono::Local);
                    (
                        local.format("%m-%d %H:%M").to_string(),
                        local.format("%H:%M").to_string(),
                    )
                }
                Err(_) => (thread.updated_at.clone(), thread.updated_at.clone()),
            };
            crate::ui::ThreadDisplayTime {
                thread_id: thread.thread_id.clone(),
                raw_updated_at: thread.updated_at.clone(),
                date_time,
                time,
            }
        })
        .collect()
}

fn tool_preview(page: crate::control::output::preview::PreviewPage) -> ToolPreview {
    tool_preview_at(page, chrono::Utc::now())
}

fn tool_preview_at(
    page: crate::control::output::preview::PreviewPage,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> ToolPreview {
    use crate::presentation::{BlockKind, OutputStream, PresentationBlock};
    let mut notices = Vec::new();
    if page.skipped {
        notices.push("前方を省略した末尾抜粋です。全文は原文から取得できます");
    }
    if page.decode_uncertain {
        notices.push("末尾からの文字変換は不確実です。原文を参照してください");
    }
    if page.decode_errors {
        notices.push("文字変換エラーがあります");
    }
    if !page.output_complete {
        notices.push(if page.terminal {
            "未回収の出力があります"
        } else {
            "実行中の出力スナップショットです"
        });
    }
    let blocks = [
        (page.stdout, OutputStream::Stdout),
        (page.stderr, OutputStream::Stderr),
    ]
    .into_iter()
    .filter(|(text, _)| !text.is_empty())
    .map(|(text, stream)| PresentationBlock {
        kind: BlockKind::ToolResult,
        stream: Some(stream),
        text,
        // Display tails cannot claim canonical node character offsets.
        source_exact: false,
        ..Default::default()
    })
    .collect();
    ToolPreview {
        blocks,
        refresh_error: None,
        // Store the observation label once in the existing budgeted payload.
        // Paint/cache hits must not advance a snapshot's apparent read time.
        notice: Some(format!(
            "取得 {} UTC · 閉じて開くと再取得{}",
            observed_at.format("%Y-%m-%d %H:%M:%S"),
            if notices.is_empty() {
                String::new()
            } else {
                format!(" · {}", notices.join("。"))
            },
        )),
    }
}

async fn presentation_work<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(2)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Presentation workers closed"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Presentation worker failed"))
}
#[derive(Clone, Copy)]
enum NodePosition {
    At(usize),
    FromEnd(usize),
}
impl NodePosition {
    fn offset(self, total: usize) -> usize {
        match self {
            Self::At(offset) => offset.min(total),
            Self::FromEnd(offset) => total.saturating_sub(offset),
        }
    }
}
impl RawNode {
    fn into_ui_node(self, position: Option<NodePosition>, render_width: u16) -> UiNode {
        let (payload, text, mut reasoning_ranges) = self.decode();
        let offset = position.map(|position| match position {
            NodePosition::At(offset) => offset,
            NodePosition::FromEnd(_) => position.offset(text.chars().count()),
        });
        let node_id = self.id;
        let kind = self.kind;
        if kind == "reasoning" && !text.is_empty() {
            reasoning_ranges = vec![(0, text.chars().count())];
        }
        let (text, base_char_offset, total_chars) = window(&text, offset);
        let window_end = base_char_offset + text.chars().count();
        reasoning_ranges.retain(|range| range.1 > base_char_offset && range.0 < window_end);
        if matches!(
            kind.as_str(),
            "incomplete_response" | "model_stream_observation"
        ) {
            return received::project(
                &kind,
                UiNode {
                    node_id,
                    text,
                    base_char_offset,
                    total_chars,
                    reasoning_ranges,
                    ..Default::default()
                },
                render_width,
                false,
                self.received_recovery_wait,
            );
        }
        let mut presentation =
            crate::presentation::project(&kind, &payload, &text, base_char_offset);
        crate::presentation::render_markdown(&mut presentation, render_width.max(1));
        UiNode {
            presentation,
            node_id,
            role: match kind.as_str() {
                "user" | "user_message" | "message" => "user",
                "tool_result" => "tool",
                "event" => "shellboy",
                "summary" | "compact_summary" => "summary",
                "spill" => "spill",
                _ => "assistant",
            }
            .into(),
            text,
            base_char_offset,
            total_chars,
            reasoning_ranges,
        }
    }
}
pub async fn copy_source(
    control: &Control,
    node_id: String,
    code_block: Option<usize>,
) -> Result<String, ApiError> {
    let result = async {
    let body = crate::history::History::new(control.store.database.clone())
        .get(HistoryGetArgs { final_answer_only: false,
            node_id,
            limit_bytes: MAX_BODY_BYTES,
            cursor: None,
            encoding: DEFAULT_ENCODING.into(),
        })
        .await?;
    if let Some(reason) = body.tombstone_reason {
        return Err(ApiError::new(
            ErrorCode::OutputExpired,
            format!("Original body unavailable: {reason}"),
        ));
    }
    if let Some(index) = code_block {
        return source_code_block(&body.text,index,body.next_cursor.is_none()).ok_or_else(||ApiError::new(ErrorCode::InvalidArgument,"Requested complete code block is outside the available body; retrieve the original by history get"));
    }
    if body.next_cursor.is_some() {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Body exceeds the 64KiB copy limit; retrieve/save the original with history get",
        ));
    }
    Ok(body.text)

    }.await;
    checked_storage(control, result).await
}
pub fn fenced_code_block(text: &str, index: usize) -> Option<String> {
    source_code_block(text, index, true)
}

fn source_code_block(text: &str, index: usize, source_complete: bool) -> Option<String> {
    crate::presentation::markdown_code_blocks(text)
        .nth(index)
        .filter(|block| block.is_complete(text.len(), source_complete))
        .map(|block| block.text)
}

#[cfg(test)]
mod code_copy_tests {
    use super::source_code_block;

    #[test]
    fn incomplete_pages_never_copy_a_block_that_can_continue() {
        for text in [
            "```",
            "```rust\n",
            "```rust\nx",
            "```rust\nx\n``",
            "    code\n",
        ] {
            assert!(source_code_block(text, 0, false).is_none(), "{text:?}");
            assert!(
                source_code_block(text, 0, true).is_some(),
                "complete source {text:?}"
            );
        }
    }

    #[test]
    fn explicit_closing_and_container_ends_remain_copyable_on_partial_pages() {
        for (text, expected) in [
            ("```\n```", ""),
            ("```rust\nx\n```", "x\n"),
            ("```\r\nx\r\n```", "x\n"),
            ("- item\n\n    ```\n    nested\n    ```", "nested\n"),
            ("> ```\n> quoted\n> ```", "quoted\n"),
            ("> ```\n> quoted\n\nprose", "quoted\n"),
            ("    indented\n\nprose", "indented\n"),
        ] {
            assert_eq!(
                source_code_block(text, 0, false).as_deref(),
                Some(expected),
                "{text:?}"
            );
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn thread_time_projection_runs_on_bounded_worker_and_preserves_raw_identity() {
        let ui_thread = std::thread::current().id();
        let rows = vec![
            ThreadSummary {
                activity: crate::activity::ActivityPhase::Idle,
                latest_run_stop_reason: None,
                revisit: None,
                thread_id: "valid".into(),
                title: String::new(),
                cwd: String::new(),
                parent_id: None,
                state: "idle".into(),
                updated_at: "2026-10-25T09:30:00Z".into(),
                excerpt: String::new(),
            },
            ThreadSummary {
                activity: crate::activity::ActivityPhase::Idle,
                latest_run_stop_reason: None,
                revisit: None,
                thread_id: "unknown".into(),
                title: String::new(),
                cwd: String::new(),
                parent_id: None,
                state: "idle".into(),
                updated_at: "unknown timezone/date".into(),
                excerpt: String::new(),
            },
        ];
        let (worker_thread, times) =
            presentation_work(move || (std::thread::current().id(), thread_display_times(&rows)))
                .await
                .unwrap();
        assert_ne!(worker_thread, ui_thread);
        assert_eq!(times[0].thread_id, "valid");
        assert_eq!(times[0].raw_updated_at, "2026-10-25T09:30:00Z");
        assert_eq!(times[0].date_time.len(), 11);
        assert_eq!(times[0].time.len(), 5);
        assert_eq!(times[1].thread_id, "unknown");
        assert_eq!(times[1].date_time, "unknown timezone/date");
        assert_eq!(times[1].time, times[1].raw_updated_at);
    }

    #[test]
    fn oversized_delta_releases_unused_capacity_and_preserves_unicode_source_offsets() {
        let mut body = StreamBody::default();
        let incoming = "界".repeat(MAX_BODY_BYTES as usize);
        body.append(&incoming, true);
        assert!(body.text.len() <= MAX_BODY_BYTES as usize);
        assert!(body.text.capacity() <= MAX_BODY_BYTES as usize * 2);
        assert_eq!(body.base + body.text.chars().count(), body.total);
        assert_eq!(body.reasoning_ranges, vec![(0, MAX_BODY_BYTES as usize)]);
        body.append("answer", false);
        assert!(body.text.ends_with("answer"));
        assert_eq!(body.base + body.text.chars().count(), body.total);
        assert_eq!(body.reasoning_ranges, vec![(0, MAX_BODY_BYTES as usize)]);
    }
}

#[cfg(test)]
pub(crate) mod control_tests;

#[cfg(test)]
mod notice_tests {
    use super::tool_group_notice;
    use crate::control::ForegroundRelease;

    #[test]
    fn tool_group_facts_are_formatted_at_the_frontend_boundary() {
        let background = tool_group_notice(Some(("execution", ForegroundRelease::Background)), 0);
        assert!(background.contains("人間の操作でshell execution"));
        assert!(background.contains("後続の未実行呼出しはありません"));
        assert!(!background.contains("取り消"));
        let input = tool_group_notice(Some(("other", ForegroundRelease::AdditionalInput)), 2);
        assert!(input.contains("追加指示でshell other"));
        assert!(input.contains("2件取り消しました"));
        assert_eq!(
            tool_group_notice(None, 3),
            "未実行のツール呼出しを3件取り消しました"
        );
    }
}

#[cfg(test)]
mod delivery_performance;

#[cfg(test)]
mod tool_preview_tests {
    use super::*;
    use crate::control::output::preview::PreviewPage;
    use crate::presentation::OutputStream;

    #[test]
    fn complete_preview_still_identifies_observation_time_and_refresh() {
        let preview = tool_preview_at(
            PreviewPage {
                stdout: "complete".into(),
                stderr: String::new(),
                raw_bytes: 8,
                inspected_rows: 1,
                skipped: false,
                decode_uncertain: false,
                decode_errors: false,
                terminal: true,
                output_complete: true,
            },
            chrono::DateTime::parse_from_rfc3339("2026-10-30T04:05:06Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert_eq!(
            preview.notice.as_deref(),
            Some("取得 2026-10-30 04:05:06 UTC · 閉じて開くと再取得")
        );
    }

    #[test]
    fn inline_tail_keeps_streams_distinct_and_discloses_only_known_limits() {
        let page = PreviewPage {
            stdout: "last stdout".into(),
            stderr: "last stderr".into(),
            raw_bytes: 2048,
            inspected_rows: 3,
            skipped: true,
            decode_uncertain: true,
            decode_errors: false,
            terminal: true,
            output_complete: false,
        };
        let observed_at = chrono::DateTime::parse_from_rfc3339("2026-10-30T04:05:06Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let preview = tool_preview_at(page, observed_at);
        assert_eq!(preview.blocks.len(), 2);
        assert_eq!(preview.blocks[0].stream, Some(OutputStream::Stdout));
        assert_eq!(preview.blocks[1].stream, Some(OutputStream::Stderr));
        assert_eq!(preview.blocks[0].text, "last stdout");
        assert!(
            preview
                .blocks
                .iter()
                .all(|block| !block.source_exact && block.markdown.is_empty())
        );
        let notice = preview.notice.unwrap();
        assert!(notice.contains("取得 2026-10-30 04:05:06 UTC"));
        assert!(notice.contains("閉じて開くと再取得"));
        assert!(
            notice.contains("末尾抜粋") && notice.contains("不確実") && notice.contains("未回収")
        );
        assert!(!notice.contains("行") && !notice.contains("文字変換エラー"));
    }
}
