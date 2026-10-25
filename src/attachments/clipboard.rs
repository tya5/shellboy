//! Explicit clipboard operations on one bounded worker. The OS clipboard object
//! stays alive (important for X11 ownership); UI never calls this API directly.
use crate::{
    config::CopyBackend,
    protocol::{ApiError, ErrorCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::sync::{
    Arc,
    mpsc::{SyncSender, sync_channel},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub const MAX_CLIPBOARD_RGBA_BYTES: usize = 128 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct ClipboardPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
impl ClipboardPixels {
    pub fn validate(&self) -> Result<(), ApiError> {
        let size = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|v| v.checked_mul(4));
        if self.width == 0
            || self.height == 0
            || size != Some(self.rgba.len())
            || self.rgba.len() > MAX_CLIPBOARD_RGBA_BYTES
        {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Clipboard image dimensions or decoded RGBA size are invalid",
            ));
        }
        Ok(())
    }
}
#[derive(Debug)]
pub enum CopyOutcome {
    OsCopied,
    TerminalRequest(Vec<u8>),
}
enum Operation {
    Capture(oneshot::Sender<Result<ClipboardPixels, ApiError>>),
    Copy(String, oneshot::Sender<Result<(), ApiError>>),
}
struct Inner {
    sender: SyncSender<Operation>,
}
#[derive(Clone)]
pub struct ClipboardAdapter {
    inner: Arc<Inner>,
}
impl ClipboardAdapter {
    pub fn new() -> Result<Self, ApiError> {
        let (sender, receiver) = sync_channel::<Operation>(8);
        std::thread::Builder::new().name("shellboy-clipboard".into()).spawn(move||{
            let mut clipboard=None;
            while let Ok(operation)=receiver.recv(){
                if clipboard.is_none(){match arboard::Clipboard::new(){Ok(value)=>clipboard=Some(value),Err(_)=>{let error=ApiError::new(ErrorCode::IoFailed,"OS clipboard is unavailable; use terminal selection or configure OSC52 copying");match operation{Operation::Capture(reply)=>{let _=reply.send(Err(error));},Operation::Copy(_,reply)=>{let _=reply.send(Err(error));}}continue;}}}
                let clipboard=clipboard.as_mut().expect("initialized clipboard");
                match operation{
                    Operation::Capture(reply)=>{let result=clipboard.get_image().map_err(|_|ApiError::new(ErrorCode::IoFailed,"Clipboard has no readable image; text paste remains terminal input")).and_then(|image|{let width=u32::try_from(image.width).map_err(|_|ApiError::new(ErrorCode::InputLimit,"Clipboard image width is too large"))?;let height=u32::try_from(image.height).map_err(|_|ApiError::new(ErrorCode::InputLimit,"Clipboard image height is too large"))?;let pixels=ClipboardPixels{width,height,rgba:image.bytes.into_owned()};pixels.validate()?;Ok(pixels)});let _=reply.send(result);},
                    Operation::Copy(text,reply)=>{let result=clipboard.set_text(text).map_err(|_|ApiError::new(ErrorCode::IoFailed,"OS clipboard rejected text; use terminal selection or OSC52"));let _=reply.send(result);}
                }
            }
        }).map_err(|_|ApiError::new(ErrorCode::IoFailed,"Cannot start clipboard worker"))?;
        Ok(Self {
            inner: Arc::new(Inner { sender }),
        })
    }
    pub async fn capture(
        &self,
        timeout_ms: u64,
        cancel: CancellationToken,
    ) -> Result<ClipboardPixels, ApiError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let (reply, result) = oneshot::channel();
        self.inner
            .sender
            .try_send(Operation::Capture(reply))
            .map_err(|_| busy())?;
        tokio::select! {biased;_=cancel.cancelled()=>Err(cancelled()),result=tokio::time::timeout(std::time::Duration::from_millis(timeout_ms),result)=>result.map_err(|_|timeout())?.map_err(|_|busy())?}
    }
    pub async fn copy_text(
        &self,
        text: String,
        backend: CopyBackend,
        max_bytes: usize,
        timeout_ms: u64,
        cancel: CancellationToken,
    ) -> Result<CopyOutcome, ApiError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        if text.len() > max_bytes {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Copy exceeds the configured body limit; retrieve/save the original or use terminal selection",
            ));
        }
        let backend = if backend == CopyBackend::Auto {
            if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
            {
                CopyBackend::Osc52
            } else {
                CopyBackend::Os
            }
        } else {
            backend
        };
        if backend == CopyBackend::Osc52 {
            return Ok(CopyOutcome::TerminalRequest(osc52(&text, max_bytes)?));
        }
        let (reply, result) = oneshot::channel();
        self.inner
            .sender
            .try_send(Operation::Copy(text, reply))
            .map_err(|_| busy())?;
        tokio::select! {biased;_=cancel.cancelled()=>Err(cancelled()),result=tokio::time::timeout(std::time::Duration::from_millis(timeout_ms),result)=>{result.map_err(|_|timeout())?.map_err(|_|busy())??;Ok(CopyOutcome::OsCopied)}}
    }
}
pub fn osc52(text: &str, max_bytes: usize) -> Result<Vec<u8>, ApiError> {
    if text.len() > max_bytes {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "OSC52 copy is too large; it will not be split into overwriting fragments",
        ));
    }
    Ok(format!("\x1b]52;c;{}\x07", STANDARD.encode(text.as_bytes())).into_bytes())
}
fn cancelled() -> ApiError {
    ApiError::new(ErrorCode::InvalidState, "Clipboard operation cancelled")
}
fn busy() -> ApiError {
    ApiError::new(
        ErrorCode::IoFailed,
        "Clipboard worker is busy or unavailable; retry explicitly",
    )
}
fn timeout() -> ApiError {
    ApiError::new(
        ErrorCode::IoFailed,
        "Clipboard operation timed out; an already-started OS operation may still finish",
    )
}
