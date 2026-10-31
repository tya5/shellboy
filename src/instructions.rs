//! Turn-scoped AGENTS.md snapshots using bounded control-file workers.
use crate::{
    config::Config,
    protocol::{ApiError, ErrorCode},
};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::Semaphore;

const MAX_INSTRUCTION_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct InstructionSnapshot {
    pub source_path: PathBuf,
    pub body: Arc<str>,
    pub hash: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
}
#[derive(Clone)]
pub struct InstructionLoader {
    workers: Arc<Semaphore>,
    worker_capacity: usize,
    timeout: Duration,
}
impl InstructionLoader {
    pub fn new(config: &Config) -> Self {
        Self {
            workers: Arc::new(Semaphore::new(config.workers.control_file)),
            worker_capacity: config.workers.control_file,
            timeout: Duration::from_millis(config.io.file_prepare_timeout_ms),
        }
    }
    pub async fn wait_idle(&self) -> Result<(), ApiError> {
        let count = u32::try_from(self.worker_capacity).map_err(|_| {
            ApiError::new(
                ErrorCode::ConfigInvalid,
                "Control-file worker count exceeds supported capacity",
            )
        })?;
        let _all = self
            .workers
            .clone()
            .acquire_many_owned(count)
            .await
            .map_err(|_| failed("Control-file workers closed during shutdown"))?;
        Ok(())
    }
    /// Reuse the existing control-file admission; no DB connection may enter it.
    pub(crate) async fn file_work<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let permit = tokio::time::timeout_at(deadline, self.workers.clone().acquire_owned())
            .await
            .map_err(|_| failed("Control file worker deadline exceeded"))?
            .map_err(|_| failed("Control file worker closed"))?;
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        });
        tokio::time::timeout_at(deadline, task).await
            .map_err(|_| failed("Control file preparation timed out; the underlying file task may still be running"))?
            .map_err(|_| failed("Control file worker failed"))?
    }
    /// The owned permit lives in the actual blocking task even after timeout.
    pub async fn prepare(
        &self,
        cwd: PathBuf,
        source_path: PathBuf,
    ) -> Result<Option<InstructionSnapshot>, ApiError> {
        self.file_work(move || {
            let meta = std::fs::metadata(&cwd).map_err(|_| {
                failed("Thread cwd is missing or unreadable; change cwd then retry")
            })?;
            if !meta.is_dir() {
                return Err(failed("Thread cwd is not a directory"));
            }
            let meta = match std::fs::metadata(&source_path) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(_) => {
                    return Err(failed(
                        "AGENTS.md exists but cannot be inspected; fix access then retry",
                    ));
                }
            };
            if !meta.is_file() {
                return Err(failed("AGENTS.md is not a readable regular file"));
            }
            if meta.len() > MAX_INSTRUCTION_BYTES {
                return Err(ApiError::new(
                    ErrorCode::InputLimit,
                    "AGENTS.md exceeds the 8 MiB instruction limit",
                ));
            }
            read_snapshot(source_path, meta).map(Some)
        })
        .await
    }
}
// Keep the stat/read boundary explicit: file changes between them must not
// let observed attributes substitute for the captured body/hash.
fn read_snapshot(
    source_path: PathBuf,
    meta: std::fs::Metadata,
) -> Result<InstructionSnapshot, ApiError> {
    let modified = meta.modified().ok();
    // The driver persists and reuses a Turn's fixed snapshot. A new
    // Turn reads current bytes; history_bytes belongs to API replay.
    // Limit the read itself too: a file can grow after metadata was read.
    use std::io::Read;
    let file = std::fs::File::open(&source_path)
        .map_err(|_| failed("AGENTS.md cannot be opened; fix access then retry"))?;
    let mut bytes = Vec::new();
    file.take(MAX_INSTRUCTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failed("AGENTS.md read failed; fix access then retry"))?;
    if bytes.len() as u64 > MAX_INSTRUCTION_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "AGENTS.md grew past its byte limit",
        ));
    }
    let body = String::from_utf8(bytes).map_err(|_| failed("AGENTS.md must contain UTF-8 text"))?;
    let hash = Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let captured_size = body.len() as u64;
    let snapshot = InstructionSnapshot {
        source_path: source_path.clone(),
        body: Arc::from(body),
        hash,
        size: captured_size,
        modified,
    };
    Ok(snapshot)
}

fn failed(message: &str) -> ApiError {
    ApiError::new(ErrorCode::IoFailed, message)
        .hint("Correct the path or access permissions and explicitly retry this Run")
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[tokio::test]
    async fn shared_file_work_holds_existing_permit_until_real_exit() {
        crate::test_watchdog::watchdog("control-file observation admission", async {
            let mut config = Config::default();
            config.workers.control_file = 1;
            let loader = InstructionLoader::new(&config);
            let (entered, ready) = tokio::sync::oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let worker = loader.clone();
            let pending = tokio::spawn(async move {
                worker
                    .file_work(move || {
                        entered.send(()).unwrap();
                        gate.recv().unwrap();
                        Ok(7)
                    })
                    .await
            });
            ready.await.unwrap();
            assert_eq!(loader.workers.available_permits(), 0);
            release.send(()).unwrap();
            assert_eq!(pending.await.unwrap().unwrap(), 7);
            loader.wait_idle().await.unwrap();
            assert_eq!(loader.workers.available_permits(), 1);
        })
        .await
        .unwrap();
    }

    #[test]
    fn stat_read_race_size_describes_captured_body_and_growth_stays_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("AGENTS.md");
        std::fs::write(&path, "old").unwrap();
        let observed = std::fs::metadata(&path).unwrap();
        // Inject a change at the exact metadata/read boundary without racing a
        // scheduler or relying on filesystem timestamp resolution.
        std::fs::write(&path, "新しい captured bytes").unwrap();
        let snapshot = read_snapshot(path.clone(), observed).unwrap();
        assert_eq!(&*snapshot.body, "新しい captured bytes");
        assert_eq!(snapshot.size, snapshot.body.len() as u64);
        assert_eq!(
            snapshot.hash,
            "36c18882fd4215c6d604a3984152a629f151b8b006d905b8eb65aaa67621b7b7"
        );
        let observed = std::fs::metadata(&path).unwrap();
        std::fs::write(&path, vec![b'x'; MAX_INSTRUCTION_BYTES as usize + 1]).unwrap();
        assert_eq!(
            read_snapshot(path, observed).unwrap_err().code,
            ErrorCode::InputLimit
        );
    }
}
