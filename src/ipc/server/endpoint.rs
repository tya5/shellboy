//! Owner-bound socket namespace and cleanup, with filesystem work off the UI.
use super::server_error;
use crate::{app::Endpoint, protocol::ApiError};
use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
pub(super) struct SocketFile {
    pub(super) path: PathBuf,
    identity: (u64, u64, i64, i64),
}
#[cfg(unix)]
impl SocketFile {
    pub(super) fn capture(path: PathBuf) -> Result<Self, ApiError> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| server_error("socket metadata failed", error))?;
        if !metadata.file_type().is_socket() || metadata.uid() != current_uid() {
            return Err(server_error(
                "socket ownership mismatch",
                "preserved unexpected file",
            ));
        }
        Ok(Self {
            path,
            identity: socket_identity(&metadata),
        })
    }
    fn remove(&self) -> Result<(), ApiError> {
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(server_error("socket metadata failed", error)),
        };
        {
            validate_folder(
                self.path.parent().expect("socket has a parent"),
                current_uid(),
            )?;
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            if !metadata.file_type().is_socket()
                || metadata.uid() != current_uid()
                || socket_identity(&metadata) != self.identity
            {
                return Err(server_error(
                    "socket cleanup refused",
                    "socket identity changed; replacement preserved",
                ));
            }
        }
        remove_if_present(&self.path)
    }
}

#[cfg(unix)]
fn socket_identity(metadata: &std::fs::Metadata) -> (u64, u64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    // ctime also detects an inode reused or changed after the original binding.
    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid has no arguments and does not mutate process credentials.
    unsafe { libc::geteuid() }
}
#[cfg(unix)]
mod namespace;
#[cfg(unix)]
use namespace::socket_folder;

#[cfg(unix)]
fn validate_folder(folder: &Path, uid: u32) -> Result<(), ApiError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(folder)
        .map_err(|error| server_error("directory metadata failed", error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(server_error(
            "private directory check failed",
            "expected an owned 0700 directory; existing path preserved",
        ));
    }
    Ok(())
}
#[cfg(unix)]
fn prepare_folder(data_dir: &Path) -> Result<(PathBuf, Option<SocketFile>), ApiError> {
    use std::os::unix::fs::DirBuilderExt;
    let uid = current_uid();
    let folder = socket_folder(data_dir, uid)?;
    match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(server_error("directory creation failed", error)),
    }
    validate_folder(&folder, uid)?;
    for entry in
        std::fs::read_dir(&folder).map_err(|error| server_error("directory read failed", error))?
    {
        if entry
            .map_err(|error| server_error("directory read failed", error))?
            .file_name()
            != "s"
        {
            return Err(server_error(
                "directory contains unexpected files",
                "existing contents preserved",
            ));
        }
    }
    let socket = folder.join("s");
    let previous = match std::fs::symlink_metadata(&socket) {
        Ok(_) => Some(SocketFile::capture(socket)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(server_error("socket metadata failed", error)),
    };
    Ok((folder, previous))
}

#[cfg(unix)]
pub(super) async fn bind_unix(
    data_dir: PathBuf,
    instance: String,
) -> Result<(tokio::net::UnixListener, Endpoint, Option<SocketFile>), ApiError> {
    let (folder, previous) = tokio::task::spawn_blocking(move || prepare_folder(&data_dir))
        .await
        .map_err(|error| server_error("directory worker failed", error))??;
    if let Some(previous) = previous {
        // A live/uncertain endpoint is never removed, even if bind is called
        // twice under the same owner lock. Only an absent/refused peer is stale.
        match tokio::time::timeout(
            std::time::Duration::from_millis(250),
            tokio::net::UnixStream::connect(&previous.path),
        )
        .await
        {
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) => {}
            Ok(Err(error)) => return Err(server_error("existing socket probe failed", error)),
            _ => {
                return Err(server_error(
                    "bind refused",
                    "existing socket is live or its probe timed out; preserved",
                ));
            }
        }
        tokio::task::spawn_blocking(move || previous.remove())
            .await
            .map_err(|error| server_error("socket cleanup worker failed", error))??;
    }
    let path = folder.join("s");
    let listener = match tokio::net::UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            let _ = tokio::task::spawn_blocking(move || remove_folder(&folder)).await;
            return Err(server_error("bind failed", error));
        }
    };
    let address = path.to_string_lossy().into_owned();
    let socket = tokio::task::spawn_blocking(move || SocketFile::capture(path))
        .await
        .map_err(|error| server_error("socket metadata worker failed", error))??;
    Ok((
        listener,
        Endpoint {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            instance_id: instance,
            transport: "unix".into(),
            address,
        },
        Some(socket),
    ))
}

pub(super) struct EndpointFiles {
    pub(super) metadata: PathBuf,
    pub(super) instance: String,
    #[cfg(unix)]
    pub(super) socket: Option<SocketFile>,
}
impl EndpointFiles {
    pub(super) async fn cleanup(self) -> Result<(), ApiError> {
        tokio::task::spawn_blocking(move || {
            let ours = std::fs::read(&self.metadata)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Endpoint>(&bytes).ok())
                .is_some_and(|endpoint| endpoint.instance_id == self.instance);
            if ours {
                remove_if_present(&self.metadata)?;
            }
            // Failed publication may have left our own uniquely named temp file.
            remove_if_present(
                &self
                    .metadata
                    .with_file_name(format!("endpoint-{}.tmp", self.instance)),
            )?;
            #[cfg(unix)]
            if let Some(socket) = self.socket {
                socket.remove()?;
                if let Some(folder) = socket.path.parent() {
                    remove_folder(folder)?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|error| server_error("cleanup worker failed", error))?
    }
}
#[cfg(unix)]
fn remove_folder(folder: &Path) -> Result<(), ApiError> {
    match std::fs::symlink_metadata(folder) {
        Ok(_) => validate_folder(folder, current_uid())?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(server_error("directory metadata failed", error)),
    }
    match std::fs::remove_dir(folder) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(server_error("directory cleanup failed", error)),
    }
}
fn remove_if_present(path: &Path) -> Result<(), ApiError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(server_error("endpoint cleanup failed", error)),
    }
}

#[cfg(all(test, unix))]
mod tests;
