//! Per-execution publication owned by the scheduled shell collector.
use super::{Control, schedule::service_not_found, storage_error};
use crate::protocol::{ApiError, ErrorCode, ServiceEndpoint};
use rusqlite::OptionalExtension;
use std::{
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) const ENV: &str = "SHELLBOY_SERVICE_ENDPOINT_FILE";
const MAX_BYTES: u64 = 8192;

fn path(data: &Path, instance: &str, execution: &str) -> PathBuf {
    data.join(format!("service-endpoint-{instance}-{execution}.json"))
}

pub(super) struct Publication(PathBuf);

impl Publication {
    pub(super) fn create(control: &Control, execution: &str) -> Result<Self, ApiError> {
        let file = path(&control.data_dir, &control.store.instance_id, execution);
        if file.to_str().is_none() {
            return Err(ApiError::new(
                ErrorCode::IoFailed,
                "Service endpoint path is not UTF-8",
            ));
        }
        crate::atomic_file::create_new(&file, true).map_err(|_| {
            ApiError::new(ErrorCode::IoFailed, "Cannot create service endpoint file")
        })?;
        Ok(Self(file))
    }

    pub(super) fn env_value(&self) -> String {
        self.0.to_str().expect("validated endpoint path").to_owned()
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("Could not remove service endpoint runtime file");
        }
    }
}

fn unavailable(reason: &'static str) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, "Service endpoint is unavailable")
        .details(serde_json::json!({"reason":reason}))
}

async fn running(control: &Control, id: &str) -> Result<String, ApiError> {
    let id = id.to_owned();
    let row = control.store.database.call(move |db| {
        Ok(db.query_row(
            "SELECT CASE WHEN q.enabled AND s.state='running' THEN s.id END
             FROM schedule q JOIN thread t ON t.id=q.thread_id AND t.deleted_at IS NULL
             LEFT JOIN shell_execution s ON s.id=(SELECT id FROM shell_execution WHERE schedule_id=q.id ORDER BY rowid DESC LIMIT 1)
             WHERE q.id=?1 AND q.kind='service'", [&id], |row| row.get::<_,Option<String>>(0),
        ).optional()?)
    }).await.map_err(storage_error)?;
    row.ok_or_else(service_not_found)?
        .ok_or_else(|| unavailable("service_not_running"))
}

pub(super) async fn read(control: &Control, id: &str) -> Result<ServiceEndpoint, ApiError> {
    let execution = running(control, id).await?;
    let file = path(&control.data_dir, &control.store.instance_id, &execution);
    let value = tokio::task::spawn_blocking(move || read_file(&file))
        .await
        .map_err(|_| unavailable("service_endpoint_unavailable"))?;
    if running(control, id).await? != execution {
        return Err(unavailable("service_not_running"));
    }
    value
}

fn read_file(path: &Path) -> Result<ServiceEndpoint, ApiError> {
    let invalid = || unavailable("service_endpoint_unavailable");
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file() {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(invalid());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
            || !matches!(crate::atomic_file::has_private_acl(&file), Ok(true))
        {
            return Err(invalid());
        }
    }
    let mut body = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| invalid())?;
    if body.len() as u64 > MAX_BYTES {
        return Err(invalid());
    }
    let endpoint: ServiceEndpoint = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if endpoint.port == 0
        || endpoint.token.is_empty()
        || !endpoint.token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(invalid());
    }
    Ok(endpoint)
}
