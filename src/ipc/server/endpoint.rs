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
    parent_identity: (u64, u64),
    // Only newly created live namespaces own a lease. Stale capture stays a
    // metadata/probe operation and must not bypass a live socket on contention.
    #[cfg(target_os = "linux")]
    _namespace_lease: Option<namespace::Lease>,
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
            parent_identity: folder_identity(path.parent().expect("socket has a parent"))?,
            path,
            identity: socket_identity(&metadata),
            #[cfg(target_os = "linux")]
            _namespace_lease: None,
        })
    }
    fn validate_parent(&self) -> Result<(), ApiError> {
        let folder = self.path.parent().expect("socket has a parent");
        validate_folder(folder, current_uid())?;
        if folder_identity(folder)? != self.parent_identity {
            return Err(server_error(
                "socket cleanup refused",
                "parent identity changed; replacement preserved",
            ));
        }
        Ok(())
    }
    fn remove_folder(&self) -> Result<(), ApiError> {
        if let Err(error) =
            std::fs::symlink_metadata(self.path.parent().expect("socket has a parent"))
        {
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(server_error("directory metadata failed", error));
        }
        self.validate_parent()?;
        remove_folder(self.path.parent().expect("socket has a parent"))
    }
    fn remove(&self) -> Result<(), ApiError> {
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(server_error("socket metadata failed", error)),
        };
        {
            self.validate_parent()?;
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
fn folder_identity(folder: &Path) -> Result<(u64, u64), ApiError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(folder)
        .map_err(|error| server_error("directory metadata failed", error))?;
    Ok((metadata.dev(), metadata.ino()))
}

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
struct PreviousNamespace {
    folder: PathBuf,
    identity: (u64, u64),
    socket: Option<SocketFile>,
}
#[cfg(unix)]
impl PreviousNamespace {
    fn remove(self) -> Result<(), ApiError> {
        if let Some(socket) = &self.socket {
            socket.remove()?;
        }
        validate_folder(&self.folder, current_uid())?;
        if folder_identity(&self.folder)? != self.identity {
            return Err(server_error(
                "directory cleanup refused",
                "directory identity changed; replacement preserved",
            ));
        }
        remove_folder(&self.folder)
    }
}

#[cfg(unix)]
fn previous_namespace(data_dir: &Path) -> Result<Option<PreviousNamespace>, ApiError> {
    use std::io::Read;
    let metadata_path = data_dir.join("endpoint.json");
    use std::os::unix::fs::OpenOptionsExt;
    // The data directory is not an IPC trust boundary: another user may have
    // changed the locator. Never block on a FIFO or follow a locator symlink.
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(metadata_path)
    else {
        return Ok(None);
    };
    let Ok(metadata) = file.metadata() else {
        return Ok(None);
    };
    if !metadata.is_file() || metadata.len() > 65536 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    if file.take(65537).read_to_end(&mut bytes).is_err() {
        return Ok(None);
    }
    let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&bytes) else {
        return Ok(None);
    };
    if endpoint.transport != "unix"
        || endpoint.protocol_version != crate::protocol::PROTOCOL_VERSION
    {
        return Ok(None);
    }
    let path = PathBuf::from(endpoint.address);
    let Some(folder) = path.parent() else {
        return Ok(None);
    };
    if path.file_name().is_none_or(|name| name != "s")
        || !namespace::recognized_folder(folder, current_uid())?
        || validate_folder(folder, current_uid()).is_err()
    {
        return Ok(None);
    }
    let identity = folder_identity(folder)?;
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Ok(None);
    };
    let mut has_socket = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return Ok(None);
        };
        if entry.file_name() != "s" {
            return Ok(None);
        }
        has_socket = true;
    }
    let socket = if has_socket {
        let Ok(socket) = SocketFile::capture(path.clone()) else {
            return Ok(None);
        };
        Some(socket)
    } else {
        None
    };
    Ok(Some(PreviousNamespace {
        folder: folder.to_path_buf(),
        identity,
        socket,
    }))
}

#[cfg(unix)]
pub(super) async fn bind_unix(
    owner: &crate::app::DataOwner,
    instance: String,
) -> Result<(tokio::net::UnixListener, Endpoint, Option<SocketFile>), ApiError> {
    let data_dir = owner.loaded.data_dir.clone();
    let previous = tokio::task::spawn_blocking(move || previous_namespace(&data_dir))
        .await
        .map_err(|error| server_error("directory worker failed", error))??;
    if let Some(previous) = previous {
        if let Some(socket) = &previous.socket {
            // Only an absent/refused peer is stale. Live/uncertain peers survive.
            match tokio::time::timeout(
                std::time::Duration::from_millis(250),
                tokio::net::UnixStream::connect(&socket.path),
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
        }
        tokio::task::spawn_blocking(move || previous.remove())
            .await
            .map_err(|error| server_error("socket cleanup worker failed", error))??;
    }
    let candidates = tokio::task::spawn_blocking(|| namespace::folder_candidates(current_uid()))
        .await
        .map_err(|error| server_error("directory worker failed", error))??;
    for folder in candidates {
        let endpoint = Endpoint {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            instance_id: instance.clone(),
            transport: "unix".into(),
            address: folder
                .join("s")
                .to_str()
                .expect("validated UTF-8 socket path")
                .into(),
        };
        let files = EndpointFiles {
            metadata: owner.loaded.data_dir.join("endpoint.json"),
            instance: instance.clone(),
            socket: None,
        };
        // Publish the planned location before creating anything. A process
        // crash during mkdir/bind now leaves a bounded cleanup locator too.
        if let Err(error) = owner.publish_endpoint(endpoint.clone()).await {
            let _ = files.cleanup().await;
            return Err(error);
        }
        let candidate = folder.clone();
        let created = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&candidate)
        })
        .await
        .map_err(|error| server_error("directory worker failed", error))?;
        if created.is_err() {
            // mkdir failure grants no ownership of an existing path.
            let _ = files.cleanup().await;
            continue;
        }
        let candidate = folder.clone();
        let identity = tokio::task::spawn_blocking(move || {
            validate_folder(&candidate, current_uid())?;
            folder_identity(&candidate)
        })
        .await
        .map_err(|error| server_error("directory worker failed", error))??;
        let created = PreviousNamespace {
            folder: folder.clone(),
            identity,
            socket: None,
        };
        #[cfg(target_os = "linux")]
        let lease = {
            let candidate = folder.clone();
            match tokio::task::spawn_blocking(move || {
                namespace::Lease::acquire(&candidate, identity)
            })
            .await
            .map_err(|error| server_error("directory lease worker failed", error))?
            {
                Ok(lease) => lease,
                Err(error) => {
                    // Identity checks preserve any replacement or unexpected
                    // contents. No recursive delete or lockfile is needed.
                    let _ = tokio::task::spawn_blocking(move || created.remove()).await;
                    let _ = files.cleanup().await;
                    return Err(error);
                }
            }
        };
        let path = folder.join("s");
        let listener = match tokio::net::UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = tokio::task::spawn_blocking(move || created.remove()).await;
                let _ = files.cleanup().await;
                return Err(server_error("bind failed", error));
            }
        };
        let socket = tokio::task::spawn_blocking(move || {
            let socket = SocketFile::capture(path)?;
            if socket.parent_identity != identity {
                return Err(server_error(
                    "socket namespace changed",
                    "directory replacement preserved",
                ));
            }
            #[cfg(target_os = "linux")]
            let socket = {
                let mut socket = socket;
                socket._namespace_lease = Some(lease);
                socket
            };
            Ok::<_, ApiError>(socket)
        })
        .await
        .map_err(|error| server_error("socket metadata worker failed", error))??;
        return Ok((listener, endpoint, Some(socket)));
    }
    Err(namespace::runtime_error(
        "private_runtime_directory_unavailable",
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
                socket.remove_folder()?;
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
