//! Turn-scoped AGENTS.md snapshots using bounded control-file workers.
use crate::{
    config::Config,
    protocol::{ApiError, ErrorCode},
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
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
    /// Compose all explicitly configured files in the existing bounded file lane.
    pub(crate) async fn prepare_with_extra(
        &self,
        cwd: PathBuf,
        source_path: PathBuf,
        extra: Vec<PathBuf>,
    ) -> Result<Option<InstructionSnapshot>, ApiError> {
        let local = self.prepare(cwd, source_path.clone()).await?;
        if extra.is_empty() {
            return Ok(local);
        }
        self.file_work(move || {
            let mut files = Vec::new();
            for path in extra {
                let meta = std::fs::metadata(&path).map_err(|_| failed(&format!("Extra instruction file cannot be inspected: {}",path.display())))?;
                if !meta.is_file() { return Err(failed(&format!("Extra instruction path is not a regular file: {}",path.display()))); }
                let mut bytes=Vec::new();
                File::open(&path).and_then(|f|f.take(8193).read_to_end(&mut bytes)).map_err(|_|failed(&format!("Extra instruction file cannot be read: {}",path.display())))?;
                if bytes.len()>8192 { return Err(ApiError::new(ErrorCode::InputLimit,format!("Extra instruction file exceeds 8192 bytes: {}",path.display()))); }
                let body=String::from_utf8(bytes).map_err(|_|failed(&format!("Extra instruction file is not UTF-8: {}",path.display())))?;
                files.push(serde_json::json!({"path":path,"hash":crate::digest::sha256_hex(&body),"bytes":body.len(),"body":body}));
            }
            let body=serde_json::json!({"shellboy_instruction_bundle_v1":true,"extra":files,"local":local.as_ref().map(|s|s.body.as_ref())}).to_string();
            Ok(Some(InstructionSnapshot { source_path, hash:crate::digest::sha256_hex(&body),size:body.len() as u64,body:Arc::from(body),modified:None }))
        }).await
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

/// Saved structured body, not live filesystem data; same-Turn replay never re-reads files.
pub(crate) fn bundle(body: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    (value.get("shellboy_instruction_bundle_v1") == Some(&serde_json::Value::Bool(true)))
        .then_some(value)
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    #[tokio::test]
    async fn extra_instructions_are_explicit_ordered_stable_and_refreshed_only_on_prepare() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("AGENTS.md");
        let one = root.path().join("studio.md");
        let two = root.path().join("user.md");
        std::fs::write(&local, "thread role").unwrap();
        std::fs::write(&one, "studio guide").unwrap();
        std::fs::write(&two, "user guide").unwrap();
        let loader = InstructionLoader::new(&Config::default());
        let no_extra = loader
            .prepare_with_extra(root.path().into(), local.clone(), vec![])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&*no_extra.body, "thread role");
        let first = loader
            .prepare_with_extra(
                root.path().into(),
                local.clone(),
                vec![one.clone(), two.clone()],
            )
            .await
            .unwrap()
            .unwrap();
        let same = loader
            .prepare_with_extra(
                root.path().into(),
                local.clone(),
                vec![one.clone(), two.clone()],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.body, same.body);
        assert_eq!(first.hash, same.hash);
        let captured = bundle(&first.body).unwrap();
        assert_eq!(captured["extra"][0]["body"], "studio guide");
        assert_eq!(captured["extra"][1]["body"], "user guide");
        assert_eq!(captured["local"], "thread role");
        std::fs::write(&one, "changed").unwrap();
        let next = loader
            .prepare_with_extra(root.path().into(), local.clone(), vec![one.clone(), two])
            .await
            .unwrap()
            .unwrap();
        assert_ne!(next.hash, first.hash);
        assert_eq!(bundle(&first.body).unwrap(), captured);
        std::fs::remove_file(&local).unwrap();
        let missing_local = loader
            .prepare_with_extra(root.path().into(), local, vec![one])
            .await
            .unwrap()
            .unwrap();
        assert!(bundle(&missing_local.body).unwrap()["local"].is_null());
    }

    #[tokio::test]
    async fn extra_instructions_fail_closed_for_missing_directory_oversize_and_invalid_utf8() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("extra.md");
        let loader = InstructionLoader::new(&Config::default());
        for body in [None, Some(vec![b'a'; 8193]), Some(vec![255])] {
            if let Some(body) = body {
                std::fs::write(&path, body).unwrap();
            }
            let error = loader
                .prepare_with_extra(
                    root.path().into(),
                    root.path().join("absent"),
                    vec![path.clone()],
                )
                .await
                .err()
                .unwrap();
            assert!(error.message.contains("Extra instruction"));
        }
        std::fs::write(&path, vec![b'a'; 8192]).unwrap();
        assert!(
            loader
                .prepare_with_extra(
                    root.path().into(),
                    root.path().join("absent"),
                    vec![path.clone()]
                )
                .await
                .is_ok()
        );
        assert!(
            loader
                .prepare_with_extra(
                    root.path().into(),
                    root.path().join("absent"),
                    vec![root.path().into()]
                )
                .await
                .is_err()
        );
    }

    #[test]
    fn extra_instructions_config_requires_bounded_absolute_paths() {
        assert!(Config::parse("extra_instructions=['relative.md']").is_err());
        let config = Config { extra_instructions: vec![std::env::temp_dir().join("guide.md"); 9], ..Config::default() };
        assert!(config.validate().is_err());
    }
}
