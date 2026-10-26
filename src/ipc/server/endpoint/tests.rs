use super::*;
use crate::{app::DataOwner, ipc::LocalServer, protocol::ErrorCode};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use tokio::sync::mpsc;

async fn owner(root: &Path) -> DataOwner {
    let config = root.join("config.toml");
    std::fs::write(&config, "").unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.to_path_buf())
        .await
        .unwrap()
}
async fn crash_owner(owner: DataOwner) {
    let path = owner.loaded.data_dir.join("endpoint.json");
    let metadata = std::fs::read(&path).unwrap();
    // Stop the test's DB/diagnostic workers without killing the test process,
    // then restore the metadata that an abrupt process exit would have left.
    owner.shutdown().await.unwrap();
    std::fs::write(path, metadata).unwrap();
}
fn endpoint(owner: &DataOwner) -> Endpoint {
    serde_json::from_slice(&std::fs::read(owner.loaded.data_dir.join("endpoint.json")).unwrap())
        .unwrap()
}
async fn bind_error(owner: &DataOwner) -> ApiError {
    let (send, _) = mpsc::channel(1);
    match LocalServer::bind(owner, send).await {
        Ok(server) => {
            server.shutdown().await.unwrap();
            panic!("fixture should refuse bind");
        }
        Err(error) => error,
    }
}

#[tokio::test]
async fn cleanup_succeeds_if_the_os_already_removed_the_temporary_namespace() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let socket = PathBuf::from(endpoint(&owner).address);
    std::fs::remove_file(&socket).unwrap();
    std::fs::remove_dir(socket.parent().unwrap()).unwrap();
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn repeated_crashes_remove_the_old_namespace_and_create_a_fresh_one() {
    let root = tempfile::tempdir().unwrap();
    let mut expected = None;
    let mut previous_instance = None;
    for _ in 0..3 {
        let owner = owner(root.path()).await;
        let (send, _) = mpsc::channel(1);
        let server = LocalServer::bind(&owner, send).await.unwrap();
        let published = endpoint(&owner);
        assert!(published.address.len() < 104);
        if let Some(address) = &expected {
            assert_ne!(&published.address, address);
            assert!(!Path::new(address).parent().unwrap().exists());
        }
        if let Some(instance) = &previous_instance {
            assert_ne!(&published.instance_id, instance);
        }
        expected = Some(published.address);
        previous_instance = Some(published.instance_id);
        // Simulate termination before the task can run its cleanup, without
        // killing a process or touching another owner's endpoint.
        server.task.abort();
        assert!(server.task.await.unwrap_err().is_cancelled());
        assert!(Path::new(expected.as_ref().unwrap()).exists());
        crash_owner(owner).await;
    }
    let owner = owner(root.path()).await;
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let published = endpoint(&owner);
    assert_ne!(published.address, expected.unwrap());
    let folder = PathBuf::from(published.address)
        .parent()
        .unwrap()
        .to_path_buf();
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
    server.shutdown().await.unwrap();
    assert!(!folder.exists());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovering_one_data_directory_preserves_another_live_owner() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let first_owner = owner(first.path()).await;
    let second_owner = owner(second.path()).await;
    let (send, _) = mpsc::channel(1);
    let first_server = LocalServer::bind(&first_owner, send).await.unwrap();
    let (send, _) = mpsc::channel(1);
    let second_server = LocalServer::bind(&second_owner, send).await.unwrap();
    let first_address = endpoint(&first_owner).address;
    let second_address = endpoint(&second_owner).address;
    assert_ne!(first_address, second_address);
    first_server.task.abort();
    assert!(first_server.task.await.unwrap_err().is_cancelled());
    crash_owner(first_owner).await;
    let first_owner = owner(first.path()).await;
    let (send, _) = mpsc::channel(1);
    let first_server = LocalServer::bind(&first_owner, send).await.unwrap();
    assert_ne!(endpoint(&first_owner).address, first_address);
    assert!(!Path::new(&first_address).parent().unwrap().exists());
    first_server.shutdown().await.unwrap();
    first_owner.shutdown().await.unwrap();
    assert!(Path::new(&second_address).exists());
    drop(
        tokio::net::UnixStream::connect(&second_address)
            .await
            .unwrap(),
    );
    second_server.shutdown().await.unwrap();
    second_owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_live_socket_is_preserved_when_bind_is_called_twice() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let address = endpoint(&owner).address;
    let error = bind_error(&owner).await;
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(error.message.contains("live"));
    drop(tokio::net::UnixStream::connect(&address).await.unwrap());
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn untrusted_metadata_preserves_arbitrary_directories_and_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let foreign = root.path().join("foreign");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&foreign)
        .unwrap();
    std::fs::write(foreign.join("keep"), b"unchanged").unwrap();
    let alias = root.path().join("alias");
    symlink(&foreign, &alias).unwrap();
    for folder in [&foreign, &alias] {
        let forged = Endpoint {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            instance_id: "untrusted".into(),
            transport: "unix".into(),
            address: folder.join("s").to_str().unwrap().into(),
        };
        std::fs::write(
            owner.loaded.data_dir.join("endpoint.json"),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        let (send, _) = mpsc::channel(1);
        let server = LocalServer::bind(&owner, send).await.unwrap();
        server.shutdown().await.unwrap();
        assert_eq!(std::fs::read(foreign.join("keep")).unwrap(), b"unchanged");
        assert!(alias.is_symlink());
    }
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn recognized_namespace_with_unexpected_contents_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let folder = namespace::create_folder(current_uid()).unwrap();
    let socket = folder.join("s");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    drop(listener);
    let keep = folder.join("keep");
    std::fs::write(&keep, b"unchanged").unwrap();
    let forged = Endpoint {
        protocol_version: crate::protocol::PROTOCOL_VERSION,
        instance_id: "untrusted".into(),
        transport: "unix".into(),
        address: socket.to_str().unwrap().into(),
    };
    std::fs::write(
        owner.loaded.data_dir.join("endpoint.json"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    server.shutdown().await.unwrap();
    assert!(socket.exists());
    assert_eq!(std::fs::read(&keep).unwrap(), b"unchanged");
    std::fs::remove_file(keep).unwrap();
    std::fs::remove_file(socket).unwrap();
    std::fs::remove_dir(folder).unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn group_writable_data_directory_and_ancestors_do_not_control_socket_namespace() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let owner = owner(root.path()).await;
    std::fs::set_permissions(
        &owner.loaded.data_dir,
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let published = endpoint(&owner);
    assert!(!Path::new(&published.address).starts_with(root.path()));
    assert!(published.address.len() <= 103);
    validate_folder(
        Path::new(&published.address).parent().unwrap(),
        current_uid(),
    )
    .unwrap();
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[test]
fn directory_validation_rejects_foreign_uid_and_exposed_permissions() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("private");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    validate_folder(&folder, current_uid()).unwrap();
    assert!(validate_folder(&folder, current_uid().wrapping_add(1)).is_err());
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(validate_folder(&folder, current_uid()).is_err());
    assert!(folder.exists());
}

#[tokio::test]
async fn failed_publication_removes_only_its_owned_socket_and_temp_metadata() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let metadata = owner.loaded.data_dir.join("endpoint.json");
    std::fs::create_dir(&metadata).unwrap();
    let error = bind_error(&owner).await;
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(metadata.is_dir());
    assert!(
        !owner
            .loaded
            .data_dir
            .join(format!("endpoint-{}.tmp", owner.instance_id))
            .exists()
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn cleanup_preserves_a_socket_replaced_after_the_original_binding() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("private");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let path = folder.join("s");
    let original = tokio::net::UnixListener::bind(&path).unwrap();
    let owned = SocketFile::capture(path.clone()).unwrap();
    std::fs::remove_file(&path).unwrap();
    let replacement = tokio::net::UnixListener::bind(&path).unwrap();
    assert!(owned.remove().is_err());
    assert!(path.exists());
    drop(tokio::net::UnixStream::connect(&path).await.unwrap());
    drop(replacement);
    drop(original);
}

#[tokio::test]
async fn a_stale_endpoint_has_no_address_after_a_restart() {
    let root = tempfile::tempdir().unwrap();
    let first = owner(root.path()).await;
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&first, send).await.unwrap();
    let stale = endpoint(&first);
    server.task.abort();
    assert!(server.task.await.unwrap_err().is_cancelled());
    crash_owner(first).await;
    let next = owner(root.path()).await;
    let (send, mut requests) = mpsc::channel(1);
    let server = LocalServer::bind(&next, send).await.unwrap();
    assert_ne!(stale.address, endpoint(&next).address);
    assert!(
        tokio::net::UnixStream::connect(&stale.address)
            .await
            .is_err()
    );
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    server.shutdown().await.unwrap();
    next.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_preplaced_legacy_shared_tmp_name_cannot_block_the_private_namespace() {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let canonical = std::fs::canonicalize(&owner.loaded.data_dir).unwrap();
    let hash: String = Sha256::digest(canonical.as_os_str().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let legacy = PathBuf::from("/tmp").join(format!("shellboy-{}-{hash}", current_uid()));
    let foreign = root.path().join("must-preserve");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("keep"), b"unchanged").unwrap();
    // A foreign-owned entry is not required to exercise the old startup DoS:
    // this preplaced symlink is also rejected and preserved by the old code.
    symlink(&foreign, &legacy).unwrap();
    assert!(
        validate_folder(&legacy, current_uid()).is_err(),
        "the legacy namespace correctly rejects this entry but cannot start there"
    );
    let (send, _) = mpsc::channel(1);
    let result = LocalServer::bind(&owner, send).await;
    let started = result.is_ok();
    if let Ok(server) = result {
        server.shutdown().await.unwrap();
    }
    assert!(legacy.is_symlink());
    assert_eq!(std::fs::read(foreign.join("keep")).unwrap(), b"unchanged");
    std::fs::remove_file(legacy).unwrap();
    owner.shutdown().await.unwrap();
    assert!(
        started,
        "a preplaced predictable shared-/tmp name must not block startup"
    );
}

#[tokio::test]
async fn cleanup_preserves_a_replaced_parent_directory() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("private");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let path = folder.join("s");
    let original = tokio::net::UnixListener::bind(&path).unwrap();
    let owned = SocketFile::capture(path.clone()).unwrap();
    let moved = root.path().join("moved");
    std::fs::rename(&folder, &moved).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let replacement = tokio::net::UnixListener::bind(&path).unwrap();
    assert!(owned.remove().is_err());
    assert!(owned.remove_folder().is_err());
    assert!(path.exists());
    assert!(moved.join("s").exists());
    drop(replacement);
    drop(original);
}

#[tokio::test]
async fn recognized_namespace_socket_impostors_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let target = root.path().join("keep");
    std::fs::write(&target, b"unchanged").unwrap();
    for link in [false, true] {
        let folder = namespace::create_folder(current_uid()).unwrap();
        let socket = folder.join("s");
        if link {
            symlink(&target, &socket).unwrap();
        } else {
            std::fs::write(&socket, b"not a socket").unwrap();
        }
        let forged = Endpoint {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            instance_id: "untrusted".into(),
            transport: "unix".into(),
            address: socket.to_str().unwrap().into(),
        };
        std::fs::write(
            owner.loaded.data_dir.join("endpoint.json"),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        let (send, _) = mpsc::channel(1);
        let server = LocalServer::bind(&owner, send).await.unwrap();
        server.shutdown().await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        assert!(std::fs::symlink_metadata(&socket).is_ok());
        if link {
            assert!(socket.is_symlink());
        } else {
            assert_eq!(std::fs::read(&socket).unwrap(), b"not a socket");
        }
        std::fs::remove_file(socket).unwrap();
        std::fs::remove_dir(folder).unwrap();
    }
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn crash_during_socket_preparation_retains_a_locator_and_reclaims_empty_directories() {
    let root = tempfile::tempdir().unwrap();
    for stage in 0..3 {
        let first = owner(root.path()).await;
        let folder = namespace::folder_candidates(current_uid())
            .unwrap()
            .remove(0);
        let planned = Endpoint {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            instance_id: first.instance_id.clone(),
            transport: "unix".into(),
            address: folder.join("s").to_str().unwrap().into(),
        };
        first.publish_endpoint(planned).await.unwrap();
        assert!(!folder.exists(), "locator is published before mkdir");
        if stage >= 1 {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&folder)
                .unwrap();
        }
        if stage >= 2 {
            drop(tokio::net::UnixListener::bind(folder.join("s")).unwrap());
        }
        crash_owner(first).await;
        let next = owner(root.path()).await;
        let (send, _) = mpsc::channel(1);
        let server = LocalServer::bind(&next, send).await.unwrap();
        assert!(
            !folder.exists(),
            "planned, empty, and socket-bearing old directories are reclaimed"
        );
        server.shutdown().await.unwrap();
        next.shutdown().await.unwrap();
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn live_namespace_holds_directory_flock_until_listener_cleanup() {
    use fs2::FileExt;
    use std::os::unix::fs::OpenOptionsExt;
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let socket = PathBuf::from(endpoint(&owner).address);
    let folder = socket.parent().unwrap();
    let observer = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(folder)
        .unwrap();
    assert_eq!(
        observer.try_lock_exclusive().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    // Stale capture must not acquire this lock and silently skip the live
    // endpoint. The ordinary probe still rejects binding the same owner twice.
    let error = bind_error(&owner).await;
    assert!(error.message.contains("live"));
    server.shutdown().await.unwrap();
    assert!(!folder.exists());
    assert!(!owner.loaded.data_dir.join("endpoint.json").exists());
    observer
        .try_lock_exclusive()
        .expect("listener cleanup must release its directory lease");
    fs2::FileExt::unlock(&observer).unwrap();
    owner.shutdown().await.unwrap();
}
