//! Owner-side staging for a single bounded native image transfer.
use super::{AttachmentService, ManagedImage};
use crate::protocol::{
    ApiError, DRAFT_IMAGE_CHUNK_BYTES, DraftImageAction, DraftImageArgs, DraftImageResult,
    ErrorCode,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const IDLE: Duration = Duration::from_secs(60);
const MAX_PINNED: usize = 256;

#[derive(Default)]
pub(super) struct DraftImages {
    transfer: Option<Transfer>,
    pins: HashMap<String, (String, ManagedImage)>,
}
struct Transfer {
    id: String,
    draft: String,
    expected: usize,
    bytes: Vec<u8>,
    expires: Instant,
    _timer: tokio_util::sync::DropGuard,
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
                if state.transfer.is_some() {
                    return Err(ApiError::new(
                        ErrorCode::InvalidState,
                        "An image upload is already in progress",
                    ));
                }
                let upload_id = super::id();
                let timer = stop.child_token();
                state.transfer = Some(Transfer {
                    id: upload_id.clone(),
                    draft: args.draft_id,
                    expected: encoded_bytes as usize,
                    bytes: Vec::new(),
                    expires: Instant::now() + IDLE,
                    _timer: timer.clone().drop_guard(),
                });
                let weak = Arc::downgrade(&self.draft_images);
                let target = upload_id.clone();
                tokio::spawn(async move {
                    let mut deadline = Instant::now() + IDLE;
                    loop {
                        tokio::select! {
                            _ = timer.cancelled() => return,
                            _ = tokio::time::sleep_until(deadline) => { }
                        }
                        let Some(state) = weak.upgrade() else { return };
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
                });
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
                if state.pins.len() >= MAX_PINNED {
                    return Err(ApiError::new(
                        ErrorCode::InputLimit,
                        "Too many draft images are pending adoption",
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
                Ok(DraftImageResult::Committed { attachment })
            }
            DraftImageAction::Release { attachment_id } => {
                if state
                    .pins
                    .get(&attachment_id)
                    .is_some_and(|(draft, _)| *draft == args.draft_id)
                {
                    state.pins.remove(&attachment_id);
                }
                Ok(DraftImageResult::Released)
            }
        }
    }

    pub(crate) async fn clear_draft_images(&self) {
        *self.draft_images.lock().await = DraftImages::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Command;

    #[tokio::test]
    async fn native_draft_image_commit_replays_one_pinned_image() {
        let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let data = root.path().join("data");
        let instance = control.store.instance_id.clone();
        let call = |draft: &str, action, operation| {
            let command = Command::DraftImage(DraftImageArgs {
                instance_id: instance.clone(), draft_id: draft.into(), action,
            });
            crate::ipc::call(&data, command, operation, None)
        };
        let png = super::super::clipboard::ClipboardPixels {
            width: 1, height: 1, rgba: vec![1, 2, 3, 255],
        }.encode_png().unwrap();
        let begun = call("draft", DraftImageAction::Begin { encoded_bytes: png.len() as u64 }, None).await.unwrap();
        let DraftImageResult::Begun { upload_id } = serde_json::from_value(begun.result.unwrap()).unwrap() else { panic!("expected upload ID") };
        let append = |offset| DraftImageAction::Append { upload_id: upload_id.clone(), offset, base64: STANDARD.encode(&png) };
        assert_eq!(call("other", append(0), None).await.unwrap().error.unwrap().code, ErrorCode::TargetUnavailable);
        assert_eq!(call("draft", append(1), None).await.unwrap().error.unwrap().code, ErrorCode::OperationConflict);
        assert!(call("draft", append(0), None).await.unwrap().ok);
        let op = format!("{instance}:upload");
        let commit = || DraftImageAction::Commit { upload_id: upload_id.clone() };
        let accepted = call("draft", commit(), Some(op.clone())).await.unwrap().result.unwrap();
        assert_eq!(call("draft", commit(), Some(op)).await.unwrap().result, Some(accepted.clone()));
        let DraftImageResult::Committed { attachment } = serde_json::from_value(accepted).unwrap() else { panic!("expected committed image") };
        assert_eq!(std::fs::read(&attachment.path).unwrap(), png);
        let pinned = control.attachments.draft_images.lock().await;
        assert_eq!(pinned.pins.len(), 1);
        assert!(pinned.pins.contains_key(&attachment.id));
        drop(pinned);
        assert!(call("draft", DraftImageAction::Release { attachment_id: attachment.id }, None).await.unwrap().ok);
        assert!(control.attachments.draft_images.lock().await.pins.is_empty());
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
