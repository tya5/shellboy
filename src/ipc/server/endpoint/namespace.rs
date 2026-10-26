//! Short, random Unix endpoint directories independent of the data directory.
use super::{ApiError, Path, PathBuf, validate_folder};
use std::os::unix::fs::MetadataExt;

const MAX_SOCKET_PATH: usize = 103;

pub(super) fn runtime_bases(uid: u32) -> Result<Vec<PathBuf>, ApiError> {
    runtime_bases_from(uid, std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
}

fn runtime_bases_from(uid: u32, candidate: Option<PathBuf>) -> Result<Vec<PathBuf>, ApiError> {
    let mut bases = Vec::with_capacity(2);
    if let Some(candidate) = candidate {
        let candidate: PathBuf = candidate.components().collect();
        if candidate.is_absolute()
            && validate_folder(&candidate, uid).is_ok()
            && let Ok(base) = std::fs::canonicalize(&candidate)
            && protected_ancestors(&base, uid)
            && usable_socket_path(&base.join(namespace_name(uid)))
        {
            bases.push(base);
        }
    }
    match system_tmp(uid) {
        Ok(fallback) => {
            if !bases.contains(&fallback) {
                bases.push(fallback);
            }
        }
        Err(error) if bases.is_empty() => return Err(error),
        Err(_) => {} // A usable XDG runtime base does not depend on /tmp.
    }
    Ok(bases)
}

pub(super) fn runtime_error(reason: &str) -> ApiError {
    ApiError::new(
        crate::protocol::ErrorCode::IoFailed,
        "一時通信領域を作成できません",
    )
    .hint("OSの一時領域の空き容量と書き込み権限を確認して、起動し直してください")
    .details(serde_json::json!({"component":"local_ipc", "reason":reason}))
}

fn system_tmp(uid: u32) -> Result<PathBuf, ApiError> {
    // /tmp is a symlink on macOS; resolve the system-selected base, then
    // require an owned private or root-owned sticky directory and safe parents.
    let fallback = std::fs::canonicalize("/tmp")
        .map_err(|_| runtime_error("system_temporary_directory_unavailable"))?;
    let metadata = std::fs::symlink_metadata(&fallback)
        .map_err(|_| runtime_error("system_temporary_directory_metadata_unavailable"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !((metadata.uid() == 0 && metadata.mode() & 0o1000 != 0)
            || (metadata.uid() == uid && metadata.mode() & 0o077 == 0))
        || !protected_ancestors(&fallback, uid)
    {
        return Err(runtime_error("system_temporary_directory_unsafe"));
    }
    Ok(fallback)
}

pub(super) fn folder_candidates(uid: u32) -> Result<Vec<PathBuf>, ApiError> {
    let mut folders = Vec::with_capacity(16);
    for base in runtime_bases(uid)? {
        for _ in 0..8 {
            let folder = base.join(namespace_name(uid));
            if usable_socket_path(&folder) {
                folders.push(folder);
            }
        }
    }
    Ok(folders)
}

#[cfg(test)]
pub(super) fn create_folder(uid: u32) -> Result<PathBuf, ApiError> {
    use std::os::unix::fs::DirBuilderExt;
    for folder in folder_candidates(uid)? {
        match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
            Ok(()) => {
                validate_folder(&folder, uid)?;
                return Ok(folder);
            }
            Err(_) => continue,
        }
    }
    Err(runtime_error("private_runtime_directory_unavailable"))
}

pub(super) fn recognized_folder(folder: &Path, uid: u32) -> Result<bool, ApiError> {
    let Some(name) = folder.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    let prefix = format!("sb-{uid}-");
    let Some(random) = name.strip_prefix(&prefix) else {
        return Ok(false);
    };
    if random.len() != 32
        || !random
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(false);
    }
    let Some(parent) = folder.parent() else {
        return Ok(false);
    };
    // Exact canonical paths only: locator aliases and symlink components never
    // grant cleanup authority. Do not canonicalize the untrusted locator.
    Ok(runtime_bases(uid)?.iter().any(|base| parent == base))
}

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
fn namespace_name(uid: u32) -> String {
    format!("sb-{uid}-{}", uuid::Uuid::new_v4().simple())
}
fn usable_socket_path(folder: &Path) -> bool {
    let socket = folder.join("s");
    socket.to_str().is_some() && socket.as_os_str().len() <= MAX_SOCKET_PATH
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn no_xdg_uses_system_tmp_and_names_are_random() {
        let uid = super::super::current_uid();
        assert_eq!(
            runtime_bases_from(uid, None).unwrap(),
            vec![std::fs::canonicalize("/tmp").unwrap()]
        );
        assert_ne!(namespace_name(uid), namespace_name(uid));
    }
    #[test]
    fn unsafe_and_long_xdg_are_skipped_without_modification() {
        let root = tempfile::tempdir().unwrap();
        let exposed = root.path().join("exposed");
        std::fs::create_dir(&exposed).unwrap();
        std::fs::set_permissions(&exposed, std::fs::Permissions::from_mode(0o755)).unwrap();
        let alias = root.path().join("alias");
        symlink(&exposed, &alias).unwrap();
        let uid = super::super::current_uid();
        let fallback = std::fs::canonicalize("/tmp").unwrap();
        for candidate in [
            exposed.clone(),
            alias.clone(),
            PathBuf::from("relative"),
            PathBuf::from(format!("/{}", "x".repeat(200))),
        ] {
            assert_eq!(
                runtime_bases_from(uid, Some(candidate)).unwrap(),
                vec![fallback.clone()]
            );
        }
        assert!(alias.is_symlink());
        assert_eq!(std::fs::metadata(exposed).unwrap().mode() & 0o777, 0o755);
    }
    #[test]
    fn path_bound_counts_utf8_bytes_and_rejects_lossy_paths() {
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
}
