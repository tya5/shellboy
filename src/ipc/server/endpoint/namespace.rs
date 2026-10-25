//! Stable Unix endpoint paths beneath validated user-private parents.
use super::{ApiError, Path, PathBuf, server_error, validate_folder};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};

// macOS sockaddr_un has 104 bytes including the terminating NUL. Keep the
// same conservative bound on other Unix targets and in published UTF-8 JSON.
const MAX_SOCKET_PATH: usize = 103;

pub(super) fn socket_folder(data_dir: &Path, uid: u32) -> Result<PathBuf, ApiError> {
    choose_folder(data_dir, uid, private_candidates(uid))
}

fn private_candidates(uid: u32) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(3);
    if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR") {
        candidates.push(path.into());
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = darwin_user_temp() {
        candidates.push(path);
    }
    #[cfg(target_os = "linux")]
    candidates.push(PathBuf::from(format!("/run/user/{uid}")));
    #[cfg(not(target_os = "linux"))]
    let _ = uid;
    candidates
}

#[cfg(target_os = "macos")]
fn darwin_user_temp() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buffer = [0u8; libc::PATH_MAX as usize];
    // SAFETY: confstr writes at most buffer.len() bytes to this valid buffer.
    // Its returned required length includes NUL; reject truncation/failure.
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if length == 0 || length > buffer.len() || buffer[length - 1] != 0 {
        return None;
    }
    Some(std::ffi::OsString::from_vec(buffer[..length - 1].to_vec()).into())
}

fn choose_folder(
    data_dir: &Path,
    uid: u32,
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Result<PathBuf, ApiError> {
    let canonical = std::fs::canonicalize(data_dir)
        .map_err(|error| server_error("data directory identity failed", error))?;
    let name = namespace_name(&canonical, uid);
    for candidate in candidates {
        // Components discard trailing separators, so symlink_metadata checks
        // the actual leaf rather than following a symlink with a trailing '/'.
        let candidate: PathBuf = candidate.components().collect();
        if !candidate.is_absolute()
            || candidate.as_os_str().len() > MAX_SOCKET_PATH
            || validate_folder(&candidate, uid).is_err()
        {
            continue;
        }
        let Ok(base) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if validate_folder(&base, uid).is_err() || !protected_ancestors(&base, uid) {
            continue;
        }
        let folder = base.join(&name);
        if usable_socket_path(&folder) {
            return Ok(folder);
        }
    }
    // No shared-/tmp fallback: its predictable name can be squatted by a
    // different uid. The existing data parent is safe only if others cannot
    // create/replace the private child. This also supports headless Linux.
    let parent = std::fs::symlink_metadata(&canonical)
        .map_err(|error| server_error("data directory metadata failed", error))?;
    let folder = canonical.join(".ipc");
    if parent.is_dir()
        && parent.uid() == uid
        && parent.mode() & 0o022 == 0
        && usable_socket_path(&folder)
        && protected_ancestors(&canonical, uid)
    {
        return Ok(folder);
    }
    Err(server_error(
        "private socket namespace unavailable",
        "no owned private base has a UTF-8 socket path within 103 bytes",
    )
    .hint("Set XDG_RUNTIME_DIR to a short existing absolute directory owned by this user with mode 0700; shared /tmp is not a private runtime directory"))
}

/// A private leaf under an attacker-writable, non-sticky ancestor can still be
/// renamed/replaced. Resolve aliases first, then trust only root/our uid and
/// non-writable parents (or sticky parents such as the system /tmp).
fn protected_ancestors(path: &Path, uid: u32) -> bool {
    path.ancestors().all(|parent| {
        std::fs::symlink_metadata(parent).is_ok_and(|metadata| {
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && (metadata.uid() == 0 || metadata.uid() == uid)
                && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0)
        })
    })
}

pub(super) fn namespace_name(canonical: &Path, uid: u32) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(uid.to_le_bytes());
    hash.update(canonical.as_os_str().as_bytes());
    let hash = hash.finalize();
    let identity: String = hash[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("r-{identity}")
}

fn usable_socket_path(folder: &Path) -> bool {
    let socket = folder.join("s");
    socket.to_str().is_some() && socket.as_os_str().len() <= MAX_SOCKET_PATH
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("shellboy-ns-")
            .tempdir_in("/tmp")
            .unwrap()
    }
    fn private(path: &Path) {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[test]
    fn short_private_base_handles_long_data_paths_and_preserves_full_identity() {
        let root = root();
        let base = root.path().join("base");
        private(&base);
        let data = root.path().join("long".repeat(40));
        private(&data);
        let uid = super::super::current_uid();
        let folder = choose_folder(&data, uid, [base.clone()]).unwrap();
        assert_eq!(
            folder.parent().unwrap(),
            std::fs::canonicalize(&base).unwrap()
        );
        assert!(usable_socket_path(&folder));
        assert_eq!(folder.file_name().unwrap().len(), 34);
        let alias = root.path().join("alias");
        symlink(&data, &alias).unwrap();
        assert_eq!(folder, choose_folder(&alias, uid, [base]).unwrap());
        assert!(
            !folder.exists(),
            "selection alone does not mutate the filesystem"
        );
    }
    #[test]
    fn untrusted_environment_candidates_are_skipped_without_chmod_or_deletion() {
        let root = root();
        let data = root.path().join("data");
        private(&data);
        let exposed = root.path().join("exposed");
        private(&exposed);
        std::fs::set_permissions(&exposed, std::fs::Permissions::from_mode(0o755)).unwrap();
        let private_base = root.path().join("private");
        private(&private_base);
        let alias = root.path().join("alias");
        symlink(&private_base, &alias).unwrap();
        let with_separator = PathBuf::from(format!("{}/", alias.display()));
        let uid = super::super::current_uid();
        let result = choose_folder(
            &data,
            uid,
            [
                PathBuf::from("relative"),
                exposed.clone(),
                with_separator,
                PathBuf::from(format!("/{}", "x".repeat(200))),
                private_base.clone(),
            ],
        )
        .unwrap();
        assert_eq!(
            result.parent().unwrap(),
            std::fs::canonicalize(&private_base).unwrap()
        );
        assert!(alias.is_symlink());
        assert_eq!(std::fs::metadata(exposed).unwrap().mode() & 0o777, 0o755);
        assert!(
            choose_folder(&data, uid.wrapping_add(1), [private_base]).is_err(),
            "another uid cannot trust these parents"
        );
    }
    #[test]
    fn headless_fallback_requires_an_owned_non_writable_data_parent() {
        let root = root();
        let data = root.path().join("data");
        private(&data);
        let uid = super::super::current_uid();
        assert_eq!(
            choose_folder(&data, uid, []).unwrap(),
            std::fs::canonicalize(&data).unwrap().join(".ipc")
        );
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(choose_folder(&data, uid, []).is_ok());
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = choose_folder(&data, uid, []).unwrap_err();
        assert!(error.hint.unwrap().contains("XDG_RUNTIME_DIR"));
        assert!(!data.join(".ipc").exists());
        assert_eq!(std::fs::metadata(data).unwrap().mode() & 0o777, 0o777);
    }
    #[test]
    fn private_leaf_below_replaceable_ancestor_is_not_a_trusted_base() {
        let root = root();
        let data = root.path().join("data");
        private(&data);
        let exposed = root.path().join("writable");
        private(&exposed);
        std::fs::set_permissions(&exposed, std::fs::Permissions::from_mode(0o777)).unwrap();
        let leaf = exposed.join("private");
        private(&leaf);
        let uid = super::super::current_uid();
        assert_eq!(
            choose_folder(&data, uid, [leaf]).unwrap(),
            std::fs::canonicalize(&data).unwrap().join(".ipc")
        );
        assert!(!protected_ancestors(&exposed, uid));
    }

    #[test]
    fn path_bound_counts_utf8_bytes_and_never_publishes_lossy_paths() {
        assert!(usable_socket_path(&PathBuf::from(format!(
            "/{}",
            "x".repeat(100)
        ))));
        assert!(!usable_socket_path(&PathBuf::from(format!(
            "/{}",
            "x".repeat(101)
        ))));
        assert!(!usable_socket_path(&PathBuf::from(format!(
            "/{}",
            "界".repeat(34)
        ))));
        use std::os::unix::ffi::OsStringExt;
        assert!(!usable_socket_path(&PathBuf::from(
            std::ffi::OsString::from_vec(vec![b'/', 0xff])
        )));
    }
    #[test]
    fn overlong_data_without_a_private_base_fails_with_recovery_guidance() {
        let root = root();
        let data = root.path().join("long".repeat(40));
        private(&data);
        let error = choose_folder(&data, super::super::current_uid(), []).unwrap_err();
        assert!(error.hint.unwrap().contains("XDG_RUNTIME_DIR"));
        assert!(!data.join(".ipc").exists());
    }
}
