//! Owner-side staging for a single bounded native image transfer.
use super::{AttachmentService, ManagedImage};
use crate::protocol::{
    ApiError, DRAFT_IMAGE_CHUNK_BYTES, DraftImageAction, DraftImageArgs, DraftImageResult,
    ErrorCode,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;

const IDLE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct DraftImages {
    transfer: Option<Transfer>,
    pins: HashMap<String, (String, ManagedImage)>,
}
impl DraftImages {
    pub(crate) fn hand_off(&mut self, draft: &crate::ui::DraftSnapshot) {
        // The committed draft/attachment reference now protects the file from
        // retention. No second runtime pin or client release is needed.
        for attachment in &draft.attachments {
            if self
                .pins
                .get(&attachment.id)
                .is_some_and(|(owner, _)| owner == &draft.draft_id)
            {
                self.pins.remove(&attachment.id);
            }
        }
    }
}

async fn expire_unbound(state: Weak<Mutex<DraftImages>>, id: String, deadline: Instant) {
    tokio::time::sleep_until(deadline).await;
    if let Some(state) = state.upgrade() {
        state.lock().await.pins.remove(&id);
    }
}

struct Transfer {
    id: String,
    draft: String,
    expected: usize,
    bytes: Vec<u8>,
    expires: Instant,
    _timer: tokio_util::sync::DropGuard,
}
async fn expire_transfer(
    state: Weak<Mutex<DraftImages>>,
    target: String,
    timer: CancellationToken,
    mut deadline: Instant,
) {
    loop {
        tokio::select! {
            _ = timer.cancelled() => return,
            _ = tokio::time::sleep_until(deadline) => {}
        }
        let Some(state) = state.upgrade() else { return };
        let mut state = state.lock().await;
        let Some(transfer) = state
            .transfer
            .as_ref()
            .filter(|transfer| transfer.id == target)
        else {
            return;
        };
        deadline = transfer.expires;
        if deadline <= Instant::now() {
            state.transfer = None;
            return;
        }
    }
}

fn missing() -> ApiError {
    ApiError::new(
        ErrorCode::TargetUnavailable,
        "Draft image upload is unavailable",
    )
}
impl AttachmentService {
    pub(crate) async fn draft_image(
        &self,
        args: DraftImageArgs,
        stop: CancellationToken,
    ) -> Result<DraftImageResult, ApiError> {
        if args.instance_id != self.instance_id {
            return Err(ApiError::new(
                ErrorCode::ProtocolMismatch,
                "Image upload targets a different owner instance",
            ));
        }
        let mut state = self.draft_images.lock().await;
        if state
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.expires <= Instant::now())
        {
            state.transfer = None;
        }
        match args.action {
            DraftImageAction::Begin { encoded_bytes } => {
                let cap = self
                    .config
                    .limits
                    .attachment_file_bytes
                    .min(super::clipboard::MAX_CLIPBOARD_RGBA_BYTES as u64);
                if encoded_bytes == 0 || encoded_bytes > cap {
                    return Err(ApiError::new(
                        ErrorCode::InputLimit,
                        "Encoded image exceeds the upload limit",
                    ));
                }
                if state
                    .transfer
                    .as_ref()
                    .is_some_and(|transfer| transfer.draft != args.draft_id)
                {
                    return Err(ApiError::new(
                        ErrorCode::InvalidState,
                        "An image upload is already in progress",
                    )
                    .hint("Wait for the other draft's upload to finish or expire, then start a new upload"));
                }
                let upload_id = super::id();
                let timer = stop.child_token();
                let deadline = Instant::now() + IDLE;
                state.transfer = Some(Transfer {
                    id: upload_id.clone(),
                    draft: args.draft_id,
                    expected: encoded_bytes as usize,
                    bytes: Vec::new(),
                    expires: deadline,
                    _timer: timer.clone().drop_guard(),
                });
                tokio::spawn(expire_transfer(
                    Arc::downgrade(&self.draft_images),
                    upload_id.clone(),
                    timer,
                    deadline,
                ));
                Ok(DraftImageResult::Begun { upload_id })
            }
            DraftImageAction::Append {
                upload_id,
                offset,
                base64,
            } => {
                let transfer = state
                    .transfer
                    .as_mut()
                    .filter(|transfer| transfer.id == upload_id && transfer.draft == args.draft_id)
                    .ok_or_else(missing)?;
                let bytes = STANDARD.decode(base64).map_err(|_| {
                    ApiError::new(
                        ErrorCode::InvalidArgument,
                        "Image chunk is not valid base64",
                    )
                })?;
                if bytes.is_empty()
                    || bytes.len() > DRAFT_IMAGE_CHUNK_BYTES
                    || bytes.len() > transfer.expected - transfer.bytes.len()
                {
                    return Err(ApiError::new(
                        ErrorCode::InputLimit,
                        "Image chunk exceeds the upload window",
                    ));
                }
                if offset != transfer.bytes.len() as u64 {
                    return Err(ApiError::new(
                        ErrorCode::OperationConflict,
                        "Image chunk offset does not match the upload",
                    ));
                }
                transfer.bytes.extend(bytes);
                transfer.expires = Instant::now() + IDLE;
                Ok(DraftImageResult::Appended {
                    received_bytes: transfer.bytes.len() as u64,
                })
            }
            DraftImageAction::Commit { upload_id } => {
                let transfer = state
                    .transfer
                    .as_ref()
                    .filter(|transfer| transfer.id == upload_id && transfer.draft == args.draft_id)
                    .ok_or_else(missing)?;
                if transfer.bytes.len() != transfer.expected {
                    return Err(ApiError::new(
                        ErrorCode::InvalidState,
                        "Image upload is incomplete",
                    ));
                }
                let mut transfer = state.transfer.take().expect("matched transfer");
                let bytes = std::mem::take(&mut transfer.bytes);
                drop(transfer);
                let image = self.save_uploaded_clipboard_png(bytes, stop).await?;
                let attachment = crate::protocol::DraftImageAttachment {
                    id: image.id.clone(),
                    path: image.path.clone(),
                };
                state.pins.insert(image.id.clone(), (args.draft_id, image));
                // Native IPC opens one connection per command, so closing Commit
                // is not evidence that its caller abandoned the composer.
                tokio::spawn(expire_unbound(
                    Arc::downgrade(&self.draft_images),
                    attachment.id.clone(),
                    Instant::now() + IDLE,
                ));
                Ok(DraftImageResult::Committed { attachment })
            }
            DraftImageAction::Release { attachment_id } => {
                if state
                    .pins
                    .get(&attachment_id)
                    .is_some_and(|(owner, _)| owner != &args.draft_id)
                {
                    return Err(ApiError::new(
                        ErrorCode::TargetUnavailable,
                        "Only an unbound image pinned to this draft can be released",
                    ));
                }
                state.pins.remove(&attachment_id);
                Ok(DraftImageResult::Released)
            }
        }
    }

    pub(crate) async fn lock_draft_images(&self) -> tokio::sync::MutexGuard<'_, DraftImages> {
        self.draft_images.lock().await
    }

    pub(crate) async fn clear_draft_images(&self) {
        *self.draft_images.lock().await = DraftImages::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Command;

    #[tokio::test(start_paused = true)]
    async fn native_draft_image_disposes_abandoned_upload_at_idle_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(1);
        let timer = CancellationToken::new();
        let id = "upload".to_owned();
        let state = Arc::new(Mutex::new(DraftImages {
            transfer: Some(Transfer {
                id: id.clone(),
                draft: "draft".into(),
                expected: 4,
                bytes: vec![1, 2],
                expires: deadline,
                _timer: timer.clone().drop_guard(),
            }),
            pins: HashMap::new(),
        }));
        crate::test_watchdog::watchdog(
            "abandoned upload expiration",
            expire_transfer(Arc::downgrade(&state), id, timer, deadline),
        )
        .await
        .unwrap();
        let expired_at = Instant::now();
        assert!(expired_at >= deadline);
        assert!(state.lock().await.transfer.is_none());
    }

    #[tokio::test]
    async fn native_draft_image_commit_replays_one_pinned_image() {
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        // The fixture, not the maintenance worker, consumes the wake permit.
        control.keep_maintenance_signals_for_test();
        let data = root.path().join("data");
        let instance = control.store.instance_id.clone();
        let stale_owner = crate::ipc::call(
            &data,
            Command::DraftImage(DraftImageArgs {
                instance_id: "old".into(),
                draft_id: "draft".into(),
                action: DraftImageAction::Begin { encoded_bytes: 4 },
            }),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(stale_owner.error.unwrap().code, ErrorCode::ProtocolMismatch);
        let call = |draft: &str, action, operation| {
            let command = Command::DraftImage(DraftImageArgs {
                instance_id: instance.clone(),
                draft_id: draft.into(),
                action,
            });
            crate::ipc::call(&data, command, operation, None)
        };
        let png = super::super::clipboard::ClipboardPixels {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        }
        .encode_png()
        .unwrap();
        let begin = || DraftImageAction::Begin {
            encoded_bytes: png.len() as u64,
        };
        let DraftImageResult::Begun {
            upload_id: abandoned,
        } = serde_json::from_value(call("draft", begin(), None).await.unwrap().result.unwrap())
            .unwrap()
        else {
            panic!("expected upload ID")
        };
        let refused = call("other", begin(), None).await.unwrap().error.unwrap();
        assert_eq!(refused.code, ErrorCode::InvalidState);
        assert_eq!(
            refused.hint.as_deref(),
            Some("Wait for the other draft's upload to finish or expire, then start a new upload")
        );
        let DraftImageResult::Begun { upload_id } =
            serde_json::from_value(call("draft", begin(), None).await.unwrap().result.unwrap())
                .unwrap()
        else {
            panic!("expected replacement upload ID")
        };
        assert_ne!(upload_id, abandoned);
        assert_eq!(
            call(
                "draft",
                DraftImageAction::Append {
                    upload_id: abandoned,
                    offset: 0,
                    base64: STANDARD.encode(&png),
                },
                None
            )
            .await
            .unwrap()
            .error
            .unwrap()
            .code,
            ErrorCode::TargetUnavailable
        );
        let append = |offset| DraftImageAction::Append {
            upload_id: upload_id.clone(),
            offset,
            base64: STANDARD.encode(&png),
        };
        assert_eq!(
            call("other", append(0), None)
                .await
                .unwrap()
                .error
                .unwrap()
                .code,
            ErrorCode::TargetUnavailable
        );
        assert_eq!(
            call("draft", append(1), None)
                .await
                .unwrap()
                .error
                .unwrap()
                .code,
            ErrorCode::OperationConflict
        );
        assert!(call("draft", append(0), None).await.unwrap().ok);
        let op = format!("{}:upload", control.store.operation_epoch.current());
        let commit = || DraftImageAction::Commit {
            upload_id: upload_id.clone(),
        };
        let accepted = call("draft", commit(), Some(op.clone()))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(
            call("draft", commit(), Some(op)).await.unwrap().result,
            Some(accepted.clone())
        );
        let DraftImageResult::Committed { attachment } = serde_json::from_value(accepted).unwrap()
        else {
            panic!("expected committed image")
        };
        assert_eq!(std::fs::read(&attachment.path).unwrap(), png);
        let pinned = control.attachments.draft_images.lock().await;
        assert_eq!(pinned.pins.len(), 1);
        assert!(pinned.pins.contains_key(&attachment.id));
        drop(pinned);
        assert_eq!(
            call(
                "other",
                DraftImageAction::Release {
                    attachment_id: attachment.id.clone(),
                },
                None
            )
            .await
            .unwrap()
            .error
            .unwrap()
            .code,
            ErrorCode::TargetUnavailable
        );
        assert!(
            control
                .attachments
                .draft_images
                .lock()
                .await
                .pins
                .contains_key(&attachment.id)
        );
        use crate::protocol::{DraftSaveArgs, ThreadCreateArgs};
        let native = |command| crate::ipc::call(&data, command, None, None);
        let created = native(Command::ThreadCreate(ThreadCreateArgs {
            connection: None,
            cwd: None,
            agents: None,
            model: None,
            effort: None,
            parent_thread_id: None,
        }))
        .await
        .unwrap()
        .result
        .unwrap();
        let thread_id = created["thread_id"].as_str().unwrap().to_owned();
        let mut save = DraftSaveArgs {
            expected_source_message: None,
            expected_thread_revision: None,
            thread_id,
            draft: Box::new(crate::ui::DraftSnapshot {
                draft_id: "draft".into(),
                revision: 2,
                attachments: vec![crate::ui::UiAttachment {
                    id: attachment.id.clone(),
                    path: attachment.path,
                    kind: "image".into(),
                    state: "ready".into(),
                    position: None,
                }],
                ..Default::default()
            }),
            anchor: Default::default(),
            follow_tail: true,
        };
        let signals = control.store.database.signals();
        signals.take_seen();
        assert!(native(Command::DraftSave(save.clone())).await.unwrap().ok);
        crate::test_watchdog::watchdog(
            "upload runtime pin lease ends after durable adoption",
            async {
                while !signals.take_seen().contains(crate::maintenance::Work::Pins) {
                    signals.changed().await;
                }
            },
        )
        .await
        .unwrap();
        // The pass that mark wakes releases the pin row.
        let mut pins = crate::maintenance::WorkSet::default();
        pins.insert(crate::maintenance::Work::Pins);
        control
            .gc
            .collect_kinds(pins, chrono::Utc::now())
            .await
            .unwrap();
        assert!(
            control
                .attachments
                .draft_images
                .lock()
                .await
                .pins
                .is_empty()
        );
        let protected = || {
            let id = attachment.id.clone();
            control.store.database.call(move |db| {
                let mut stmt = db.prepare(super::super::MANAGED_PAGE)?;
                let rows = stmt.query_map(rusqlite::params!["ready", "", "", 100], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, bool>(5)?))
                })?;
                for row in rows {
                    let (candidate, protected) = row?;
                    if candidate == id {
                        return Ok(protected);
                    }
                }
                panic!("uploaded image must be in the managed retention page");
            })
        };
        save.draft.attachments.clear();
        save.draft.revision = 1;
        assert_eq!(
            native(Command::DraftSave(save.clone()))
                .await
                .unwrap()
                .error
                .unwrap()
                .code,
            ErrorCode::OperationConflict
        );
        assert!(protected().await.unwrap());
        save.draft.revision = 3;
        assert!(native(Command::DraftSave(save)).await.unwrap().ok);
        assert!(!protected().await.unwrap());
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
