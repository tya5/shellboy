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
async fn repeated_crashes_reuse_one_socket_namespace_for_the_same_data_directory() {
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
            assert_eq!(&published.address, address);
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
        owner.shutdown().await.unwrap();
    }
    let owner = owner(root.path()).await;
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    assert_eq!(endpoint(&owner).address, expected.unwrap());
    let folder = socket_folder(&owner.loaded.data_dir, current_uid()).unwrap();
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
    server.shutdown().await.unwrap();
    assert!(!folder.exists());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_aliases_share_identity_but_distinct_directories_and_uids_do_not() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let alias = root.path().join("alias");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    symlink(&first, &alias).unwrap();
    let identity = socket_folder(&first, current_uid()).unwrap();
    assert_eq!(identity, socket_folder(&alias, current_uid()).unwrap());
    assert_ne!(identity, socket_folder(&second, current_uid()).unwrap());
    assert_ne!(
        namespace::namespace_name(&std::fs::canonicalize(&first).unwrap(), current_uid()),
        namespace::namespace_name(
            &std::fs::canonicalize(&first).unwrap(),
            current_uid().wrapping_add(1)
        )
    );
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
    first_owner.shutdown().await.unwrap();
    let first_owner = owner(first.path()).await;
    let (send, _) = mpsc::channel(1);
    let first_server = LocalServer::bind(&first_owner, send).await.unwrap();
    assert_eq!(endpoint(&first_owner).address, first_address);
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
async fn unsafe_folder_and_socket_paths_fail_without_following_or_deleting_them() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path()).await;
    let foreign = root.path().join("foreign");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&foreign)
        .unwrap();
    std::fs::write(foreign.join("keep"), b"unchanged").unwrap();
    let folder = socket_folder(&owner.loaded.data_dir, current_uid()).unwrap();
    symlink(&foreign, &folder).unwrap();
    assert_eq!(bind_error(&owner).await.code, ErrorCode::IoFailed);
    assert_eq!(std::fs::read(foreign.join("keep")).unwrap(), b"unchanged");
    assert!(folder.is_symlink());
    std::fs::remove_file(&folder).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let socket = folder.join("s");
    symlink(foreign.join("keep"), &socket).unwrap();
    assert_eq!(bind_error(&owner).await.code, ErrorCode::IoFailed);
    assert!(socket.is_symlink());
    assert_eq!(std::fs::read(foreign.join("keep")).unwrap(), b"unchanged");
    std::fs::remove_file(&socket).unwrap();
    std::fs::write(&socket, b"not a socket").unwrap();
    assert_eq!(bind_error(&owner).await.code, ErrorCode::IoFailed);
    assert_eq!(std::fs::read(&socket).unwrap(), b"not a socket");
    std::fs::remove_file(&socket).unwrap();
    std::fs::write(folder.join("keep"), b"unexpected").unwrap();
    assert_eq!(bind_error(&owner).await.code, ErrorCode::IoFailed);
    assert_eq!(std::fs::read(folder.join("keep")).unwrap(), b"unexpected");
    std::fs::remove_file(folder.join("keep")).unwrap();
    std::fs::remove_dir(&folder).unwrap();
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
        !socket_folder(&owner.loaded.data_dir, current_uid())
            .unwrap()
            .exists()
    );
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
async fn a_stale_endpoint_at_the_reused_address_cannot_dispatch_to_the_new_instance() {
    use crate::protocol::{Command, MessageSendArgs};
    let root = tempfile::tempdir().unwrap();
    let first = owner(root.path()).await;
    let (send, _) = mpsc::channel(1);
    let server = LocalServer::bind(&first, send).await.unwrap();
    let stale = endpoint(&first);
    server.task.abort();
    assert!(server.task.await.unwrap_err().is_cancelled());
    first.shutdown().await.unwrap();
    let next = owner(root.path()).await;
    let (send, mut requests) = mpsc::channel(1);
    let server = LocalServer::bind(&next, send).await.unwrap();
    assert_eq!(stale.address, endpoint(&next).address);
    let stream = tokio::net::UnixStream::connect(&stale.address)
        .await
        .unwrap();
    let operation = format!("{}:stale", stale.instance_id);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::ipc::call_stream(
            stream,
            stale,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some("t".into()),
                text: "must not be sent".into(),
            }),
            Some(operation),
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ProtocolMismatch);
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
