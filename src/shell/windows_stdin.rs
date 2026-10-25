//! Overlapped stdin keeps transfer counts tied to completed Win32 I/O, rather
//! than bytes accepted into Mio's write staging buffer.
use super::*;
use std::{
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, HANDLE,
        INVALID_HANDLE_VALUE,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        OPEN_EXISTING, PIPE_ACCESS_OUTBOUND, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_WAIT,
        },
        Threading::CreateEventW,
    },
};

fn io_error(action: &str) -> ApiError {
    ApiError::new(
        ErrorCode::IoFailed,
        format!("{action}: {}", std::io::Error::last_os_error()),
    )
}
fn own(handle: HANDLE, action: &str) -> Result<OwnedHandle, ApiError> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io_error(action))
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}
pub(super) struct NativeStdin {
    handle: Arc<OwnedHandle>,
}
struct PendingIo {
    handle: Arc<OwnedHandle>,
    overlapped: Box<OVERLAPPED>,
    buffer: Vec<u8>,
    _event: OwnedHandle,
}
// OVERLAPPED is uniquely owned and used for one operation. Its pointers refer
// only to this stable heap allocation and its owned event; no thread-local data.
unsafe impl Send for PendingIo {}
struct NativeOperation {
    inner: Option<Box<PendingIo>>,
    pending: bool,
}
impl NativeOperation {
    fn new(handle: Arc<OwnedHandle>, buffer: Vec<u8>) -> Result<Self, ApiError> {
        let event = own(
            unsafe { CreateEventW(null(), 1, 0, null()) },
            "create stdin completion event",
        )?;
        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { zeroed() });
        overlapped.hEvent = event.as_raw_handle();
        Ok(Self {
            inner: Some(Box::new(PendingIo {
                handle,
                overlapped,
                buffer,
                _event: event,
            })),
            pending: false,
        })
    }
    fn begin_write(&mut self) -> Result<(), ApiError> {
        let inner = self.inner.as_mut().expect("operation storage");
        let result = unsafe {
            WriteFile(
                inner.handle.as_raw_handle(),
                inner.buffer.as_ptr(),
                inner.buffer.len() as u32,
                null_mut(),
                inner.overlapped.as_mut(),
            )
        };
        if result == 0
            && std::io::Error::last_os_error().raw_os_error() != Some(ERROR_IO_PENDING as i32)
        {
            return Err(io_error("write shell stdin"));
        }
        self.pending = true;
        Ok(())
    }
    fn begin_connect(&mut self) -> Result<(), ApiError> {
        let inner = self.inner.as_mut().expect("operation storage");
        let result =
            unsafe { ConnectNamedPipe(inner.handle.as_raw_handle(), inner.overlapped.as_mut()) };
        if result != 0 {
            self.pending = false;
            return Ok(());
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(code) if code == ERROR_PIPE_CONNECTED as i32 => {
                self.pending = false;
                Ok(())
            }
            Some(code) if code == ERROR_IO_PENDING as i32 => {
                self.pending = true;
                Ok(())
            }
            _ => Err(io_error("connect shell stdin pipe")),
        }
    }
    fn cancel(&self) {
        if self.pending {
            let inner = self.inner.as_ref().expect("operation storage");
            unsafe { CancelIoEx(inner.handle.as_raw_handle(), inner.overlapped.as_ref()) };
        }
    }
    /// None is still pending; Some reports the actual completed transfer count
    /// even when the operation was cancelled or ended with a pipe error.
    fn completion(&mut self) -> Option<(u32, Option<String>)> {
        if !self.pending {
            return Some((0, None));
        }
        let inner = self.inner.as_ref().expect("operation storage");
        let mut transferred = 0;
        if unsafe {
            GetOverlappedResult(
                inner.handle.as_raw_handle(),
                inner.overlapped.as_ref(),
                &mut transferred,
                0,
            )
        } != 0
        {
            self.pending = false;
            return Some((transferred, None));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
            None
        } else {
            self.pending = false;
            Some((transferred, Some(format!("stdin I/O failed: {error}"))))
        }
    }
}
impl Drop for NativeOperation {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        self.cancel();
        // Cancellation is asynchronous. A dropped RPC/task must not free its
        // OVERLAPPED or data while the kernel can still access either. Leak into
        // a cleanup waiter first; if runtime shutdown rejects a queued waiter,
        // memory remains valid until process exit instead of becoming dangling.
        let pending = Box::leak(self.inner.take().expect("pending operation storage"));
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || {
                let mut transferred = 0;
                unsafe {
                    GetOverlappedResult(
                        pending.handle.as_raw_handle(),
                        pending.overlapped.as_ref(),
                        &mut transferred,
                        1,
                    )
                };
                // The owned handle/event were retained and the operation has ended.
                unsafe { drop(Box::from_raw(pending as *mut PendingIo)) };
            });
        }
    }
}
impl NativeStdin {
    pub(super) async fn write(
        &mut self,
        bytes: &[u8],
        deadline: Instant,
        stop: &CancellationToken,
        finished: &CancellationToken,
    ) -> StdinWriteResult {
        let mut written = 0;
        while written < bytes.len() {
            if stop.is_cancelled() {
                return write_result(written as u64, StdinWriteStatus::Stopped, false, None);
            }
            if finished.is_cancelled() {
                return write_result(written as u64, StdinWriteStatus::Closed, false, None);
            }
            if Instant::now() >= deadline {
                return write_result(
                    written as u64,
                    StdinWriteStatus::Timeout,
                    true,
                    Some("stdin write deadline exceeded; do not automatically resend".into()),
                );
            }
            let end = (written + 65536).min(bytes.len());
            let mut operation =
                match NativeOperation::new(self.handle.clone(), bytes[written..end].to_vec()) {
                    Ok(v) => v,
                    Err(error) => {
                        return write_result(
                            written as u64,
                            StdinWriteStatus::Failed,
                            false,
                            Some(error.message),
                        );
                    }
                };
            if let Err(error) = operation.begin_write() {
                return write_result(
                    written as u64,
                    StdinWriteStatus::Failed,
                    false,
                    Some(error.message),
                );
            }
            let mut interrupted = None;
            let mut cancellation_deadline = None;
            loop {
                if let Some((count, error)) = operation.completion() {
                    written += count as usize;
                    if let Some(status) = interrupted {
                        return write_result(
                            written as u64,
                            status,
                            status == StdinWriteStatus::Timeout,
                            Some(
                                "stdin I/O cancellation completed; do not automatically resend"
                                    .into(),
                            ),
                        );
                    }
                    if let Some(error) = error {
                        return write_result(
                            written as u64,
                            StdinWriteStatus::Failed,
                            false,
                            Some(error),
                        );
                    }
                    if count == 0 {
                        return write_result(written as u64, StdinWriteStatus::Closed, false, None);
                    }
                    break;
                }
                if interrupted.is_none() {
                    let status = tokio::select! {
                        biased;
                        _=stop.cancelled()=>Some(StdinWriteStatus::Stopped),
                        _=finished.cancelled()=>Some(StdinWriteStatus::Closed),
                        _=tokio::time::sleep_until(deadline)=>Some(StdinWriteStatus::Timeout),
                        _=tokio::time::sleep(Duration::from_millis(2))=>None,
                    };
                    if let Some(status) = status {
                        interrupted = Some(status);
                        operation.cancel();
                        cancellation_deadline = Some(Instant::now() + Duration::from_secs(1));
                    }
                } else {
                    if cancellation_deadline.is_some_and(|until| Instant::now() >= until) {
                        return write_result(written as u64,StdinWriteStatus::Failed,false,Some("stdin cancellation completion unconfirmed; reported bytes are confirmed only, the current <=64KiB transfer is uncertain; do not resend".into()));
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }
        }
        write_result(written as u64, StdinWriteStatus::Complete, true, None)
    }
}

pub(super) async fn input_pipe() -> Result<(NativeStdin, OwnedHandle), ApiError> {
    let name: Vec<u16> = format!(r"\\.\pipe\shellboy-shell-{}", uuid::Uuid::new_v4())
        .encode_utf16()
        .chain([0])
        .collect();
    let parent = Arc::new(own(
        unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                null(),
            )
        },
        "create overlapped shell stdin pipe",
    )?);
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let child = own(
        unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ,
                0,
                &security,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        },
        "open child stdin pipe",
    )?;
    let mut operation = NativeOperation::new(parent.clone(), Vec::new())?;
    operation.begin_connect()?;
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some((_, error)) = operation.completion() {
                return error.map_or(Ok(()), |message| {
                    Err(ApiError::new(ErrorCode::IoFailed, message))
                });
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .map_err(|_| {
        ApiError::new(
            ErrorCode::IoFailed,
            "stdin pipe connection deadline exceeded",
        )
    })??;
    Ok((NativeStdin { handle: parent }, child))
}
