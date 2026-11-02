//! Short, random Unix endpoint directories, preferring temporary bases.
#[cfg(any(test, target_os = "linux"))]
use super::validate_folder;
use super::{ApiError, Path, PathBuf};
use std::os::unix::fs::MetadataExt;

// Only Linux's cooperating tmpfiles ageing has a documented lock exclusion.
// A lease does not prevent logout cleanup, unlink, or arbitrary administrators.
#[cfg(target_os = "linux")]
pub(super) struct Lease {
    directory: std::fs::File,
}
#[cfg(target_os = "linux")]
impl Lease {
    pub(super) fn acquire(folder: &Path, identity: (u64, u64)) -> Result<Self, ApiError> {
        use fs2::FileExt;
        use std::os::unix::fs::OpenOptionsExt;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(folder)
            .map_err(|error| super::server_error("namespace lease open failed", error))?;
        let metadata = directory
            .metadata()
            .map_err(|error| super::server_error("namespace lease metadata failed", error))?;
        validate_folder(folder, super::current_uid())?;
        if !metadata.is_dir()
            || metadata.uid() != super::current_uid()
            || metadata.mode() & 0o7777 != 0o700
            || (metadata.dev(), metadata.ino()) != identity
            || super::folder_identity(folder)? != identity
        {
            return Err(super::server_error(
                "namespace lease refused",
                "directory identity changed; preserved",
            ));
        }
        directory
            .try_lock_exclusive()
            .map_err(|error| super::server_error("namespace lease lock failed", error))?;
        Ok(Self { directory })
    }
}
#[cfg(target_os = "linux")]
impl Drop for Lease {
    fn drop(&mut self) {
        // Match the owner lock's final-owner semantics in the presence of dup
        // or a short-lived fork before exec. The FD itself is close-on-exec.
        let _ = fs2::FileExt::unlock(&self.directory);
    }
}

const MAX_SOCKET_PATH: usize = 103;

pub(super) fn runtime_bases(uid: u32) -> Result<Vec<PathBuf>, ApiError> {
    runtime_bases_from(uid, std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
}

fn runtime_bases_from(uid: u32, candidate: Option<PathBuf>) -> Result<Vec<PathBuf>, ApiError> {
    runtime_bases_with(uid, candidate, os_user_tmp())
}

fn runtime_bases_with(
    uid: u32,
    candidate: Option<PathBuf>,
    user_tmp: Option<PathBuf>,
) -> Result<Vec<PathBuf>, ApiError> {
    let mut bases = Vec::with_capacity(3);
    for candidate in [candidate, user_tmp].into_iter().flatten() {
        if let Some(base) = private_runtime_base(uid, candidate)
            && !bases.contains(&base)
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
        Err(_) => {} // A usable private base does not depend on /tmp.
    }
    Ok(bases)
}

fn private_runtime_base(uid: u32, candidate: PathBuf) -> Option<PathBuf> {
    checked_private_base(uid, candidate).ok()
}

#[derive(Clone, Copy, Debug)]
struct Rejection {
    reason: &'static str,
    errno: Option<i32>,
    error_kind: Option<&'static str>,
}
impl Rejection {
    fn plain(reason: &'static str) -> Self {
        Self {
            reason,
            errno: None,
            error_kind: None,
        }
    }
    fn io(reason: &'static str, error: &std::io::Error) -> Self {
        let kind = match error.kind() {
            std::io::ErrorKind::PermissionDenied => "permission_denied",
            std::io::ErrorKind::NotFound => "not_found",
            std::io::ErrorKind::AlreadyExists => "already_exists",
            std::io::ErrorKind::OutOfMemory => "out_of_memory",
            std::io::ErrorKind::Interrupted => "interrupted",
            _ => "other",
        };
        Self {
            reason,
            errno: error.raw_os_error(),
            error_kind: Some(kind),
        }
    }
}

fn checked_private_base(uid: u32, candidate: PathBuf) -> Result<PathBuf, Rejection> {
    let candidate: PathBuf = candidate.components().collect();
    if !candidate.is_absolute() {
        return Err(Rejection::plain("relative_path"));
    }
    let metadata = std::fs::symlink_metadata(&candidate)
        .map_err(|error| Rejection::io("metadata_failed", &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Rejection::plain("not_real_directory"));
    }
    if metadata.uid() != uid {
        return Err(Rejection::plain("wrong_owner"));
    }
    if metadata.mode() & 0o7777 != 0o700 {
        return Err(Rejection::plain("unsafe_mode"));
    }
    let base = std::fs::canonicalize(candidate)
        .map_err(|error| Rejection::io("canonicalize_failed", &error))?;
    if !protected_ancestors(&base, uid) {
        return Err(Rejection::plain("unsafe_ancestor"));
    }
    check_socket_path(&base.join(namespace_name(uid)))?;
    Ok(base)
}

fn check_socket_path(folder: &Path) -> Result<(), Rejection> {
    let socket = folder.join("s");
    if socket.to_str().is_none() {
        return Err(Rejection::plain("non_utf8"));
    }
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(Rejection::plain("path_too_long"));
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Diagnostics {
    entries: Vec<Diagnostic>,
    omitted: u32,
}
#[derive(serde::Serialize)]
struct Diagnostic {
    candidate: &'static str,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    errno: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_kind: Option<&'static str>,
    count: u32,
}
impl Diagnostics {
    fn record(&mut self, candidate: &'static str, rejection: Rejection) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.candidate == candidate
                && entry.reason == rejection.reason
                && entry.errno == rejection.errno
                && entry.error_kind == rejection.error_kind
        }) {
            entry.count = entry.count.saturating_add(1);
        } else if self.entries.len() < 40 {
            self.entries.push(Diagnostic {
                candidate,
                reason: rejection.reason,
                errno: rejection.errno,
                error_kind: rejection.error_kind,
                count: 1,
            });
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }
    pub(super) fn mkdir_failed(&mut self, candidate: &'static str, error: &std::io::Error) {
        self.record(candidate, Rejection::io("mkdir_failed", error));
    }
    pub(super) fn into_error(self) -> ApiError {
        let denied = self.entries.iter().any(|entry| {
            matches!(entry.errno, Some(libc::EACCES | libc::EPERM))
                || entry.error_kind == Some("permission_denied")
        });
        let long = self.entries.iter().any(|entry| {
            entry.reason == "path_too_long" || entry.errno == Some(libc::ENAMETOOLONG)
        });
        let hint = match (denied, long) {
            (true, true) => {
                "Check whether a sandbox or security software blocks directory creation and use a short private XDG_RUNTIME_DIR; the data directory or another candidate is also too long."
            }
            (true, false) => {
                "Check whether a sandbox or security software blocks directory creation, and set XDG_RUNTIME_DIR to a short private directory you own."
            }
            (false, true) => {
                "Use a shorter data directory or a short private XDG_RUNTIME_DIR you own, then restart Shellboy."
            }
            (false, false) => {
                "Check the candidate reasons, permissions and free space, or set XDG_RUNTIME_DIR to a short private directory you own, then restart Shellboy."
            }
        };
        ApiError::new(crate::protocol::ErrorCode::IoFailed, "Cannot create the temporary IPC directory")
            .hint(hint)
            .details(serde_json::json!({"component":"local_ipc", "reason":"private_runtime_directory_unavailable", "candidates":self.entries, "omitted":self.omitted}))
    }
}

pub(super) struct Plan {
    pub(super) folders: Vec<(&'static str, PathBuf)>,
    pub(super) diagnostics: Diagnostics,
}

pub(super) fn plan(uid: u32, data_dir: &Path) -> Plan {
    plan_with(
        uid,
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        os_user_tmp(),
        PathBuf::from("/tmp"),
        data_dir.to_path_buf(),
    )
}

pub(super) fn plan_with(
    uid: u32,
    xdg: Option<PathBuf>,
    user: Option<PathBuf>,
    system: PathBuf,
    data: PathBuf,
) -> Plan {
    let mut diagnostics = Diagnostics::default();
    let mut bases = Vec::new();
    for (kind, candidate) in [
        ("xdg", xdg),
        ("os", user),
        ("system", Some(system)),
        ("data", Some(data)),
    ] {
        let Some(candidate) = candidate else {
            diagnostics.record(kind, Rejection::plain("unavailable"));
            continue;
        };
        let result = if kind == "system" {
            checked_system_base(uid, &candidate)
        } else {
            checked_private_base(uid, candidate)
        };
        match result {
            Ok(base) if !bases.iter().any(|(_, prior)| prior == &base) => bases.push((kind, base)),
            Ok(_) => diagnostics.record(kind, Rejection::plain("duplicate")),
            Err(rejection) => diagnostics.record(kind, rejection),
        }
    }
    let mut folders = Vec::with_capacity(32);
    for (kind, base) in bases {
        for _ in 0..8 {
            folders.push((kind, base.join(namespace_name(uid))));
        }
    }
    Plan {
        folders,
        diagnostics,
    }
}

#[cfg(target_os = "macos")]
fn os_user_tmp() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    // Query the OS rather than trusting inherited TMPDIR. confstr includes its
    // terminating NUL in the required size; keep even this discovery bounded.
    // SAFETY: a null buffer with zero length requests only the required size.
    let length = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
    if length == 0 || length > 4096 {
        return None;
    }
    let mut bytes = vec![0u8; length];
    // SAFETY: the buffer is writable for exactly the supplied byte length.
    let copied = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if copied != length || bytes.pop() != Some(0) || bytes.contains(&0) {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(target_os = "macos"))]
fn os_user_tmp() -> Option<PathBuf> {
    None
}

pub(super) fn runtime_error(reason: &str) -> ApiError {
    ApiError::new(
        crate::protocol::ErrorCode::IoFailed,
        "Cannot create the temporary IPC directory",
    )
    .hint("Check free space and write permissions in the OS temporary directory, then restart Shellboy")
    .details(serde_json::json!({"component":"local_ipc", "reason":reason}))
}

fn system_tmp(uid: u32) -> Result<PathBuf, ApiError> {
    checked_system_base(uid, Path::new("/tmp"))
        .map_err(|_| runtime_error("system_temporary_directory_unavailable"))
}

fn checked_system_base(uid: u32, path: &Path) -> Result<PathBuf, Rejection> {
    let fallback = std::fs::canonicalize(path)
        .map_err(|error| Rejection::io("canonicalize_failed", &error))?;
    let metadata = std::fs::symlink_metadata(&fallback)
        .map_err(|error| Rejection::io("metadata_failed", &error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Rejection::plain("not_real_directory"));
    }
    if !((metadata.uid() == 0 && metadata.mode() & 0o1000 != 0)
        || (metadata.uid() == uid && metadata.mode() & 0o077 == 0))
    {
        return Err(Rejection::plain("unsafe_owner_or_mode"));
    }
    if !protected_ancestors(&fallback, uid) {
        return Err(Rejection::plain("unsafe_ancestor"));
    }
    check_socket_path(&fallback.join(namespace_name(uid)))?;
    Ok(fallback)
}

#[cfg(test)]
pub(super) fn folder_candidates(uid: u32) -> Result<Vec<PathBuf>, ApiError> {
    let mut folders = Vec::with_capacity(24);
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

pub(super) fn recognized_folder(
    folder: &Path,
    uid: u32,
    data_dir: Option<&Path>,
) -> Result<bool, ApiError> {
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
    if data_dir
        .and_then(|data| private_runtime_base(uid, data.to_path_buf()))
        .as_deref()
        == Some(parent)
    {
        return Ok(true);
    }
    Ok(runtime_bases(uid).is_ok_and(|bases| bases.iter().any(|base| parent == base)))
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
#[cfg(test)]
fn usable_socket_path(folder: &Path) -> bool {
    check_socket_path(folder).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn candidate_reasons_are_bounded_private_and_actionable() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::Builder::new()
            .prefix("i")
            .tempdir_in("/tmp")
            .unwrap();
        let uid = super::super::current_uid();
        let missing = root.path().join("private-missing-name");
        let long = root.path().join("x".repeat(90));
        std::fs::create_dir(&long).unwrap();
        std::fs::set_permissions(&long, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut plan = plan_with(
            uid,
            Some(PathBuf::from("relative-private-name")),
            Some(missing),
            root.path().join("missing-system"),
            long,
        );
        for _ in 0..8 {
            plan.diagnostics
                .mkdir_failed("xdg", &std::io::Error::from_raw_os_error(libc::EPERM));
        }
        let error = plan.diagnostics.into_error();
        let details = error.details.unwrap();
        let entries = details["candidates"].as_array().unwrap();
        assert!(
            entries
                .iter()
                .any(|v| v["candidate"] == "xdg" && v["reason"] == "relative_path")
        );
        assert!(entries.iter().any(|v| v["candidate"] == "os"
            && v["reason"] == "metadata_failed"
            && v["errno"] == libc::ENOENT));
        assert!(
            entries
                .iter()
                .any(|v| v["candidate"] == "data" && v["reason"] == "path_too_long")
        );
        assert!(entries.iter().any(|v| v["candidate"] == "xdg"
            && v["reason"] == "mkdir_failed"
            && v["errno"] == libc::EPERM
            && v["count"] == 8));
        assert!(error.hint.unwrap().contains("sandbox"));
        assert!(!details.to_string().contains("private-missing-name"));
        assert!(!details.to_string().contains("relative-private-name"));
        let mut diagnostics = Diagnostics::default();
        for errno in 1..=200 {
            diagnostics.mkdir_failed("data", &std::io::Error::from_raw_os_error(errno));
        }
        let bounded = diagnostics.into_error().details.unwrap();
        assert_eq!(bounded["candidates"].as_array().unwrap().len(), 40);
        assert_eq!(bounded["omitted"], 160);
        let mut non_utf8 = root.path().as_os_str().as_encoded_bytes().to_vec();
        non_utf8.extend_from_slice(b"/\xff");
        let path = PathBuf::from(std::ffi::OsString::from_vec(non_utf8));
        assert_eq!(check_socket_path(&path).unwrap_err().reason, "non_utf8");
    }

    #[test]
    fn data_base_rejects_unsafe_mode_symlink_and_wrong_owner() {
        let root = tempfile::Builder::new()
            .prefix("i")
            .tempdir_in("/tmp")
            .unwrap();
        let uid = super::super::current_uid();
        let data = root.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            checked_private_base(uid, data.clone()).unwrap_err().reason,
            "unsafe_mode"
        );
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            checked_private_base(uid.wrapping_add(1), data.clone())
                .unwrap_err()
                .reason,
            "wrong_owner"
        );
        let link = root.path().join("link");
        symlink(&data, &link).unwrap();
        assert_eq!(
            checked_private_base(uid, link).unwrap_err().reason,
            "not_real_directory"
        );
    }

    #[test]
    fn runtime_error_uses_english_without_changing_code_or_source_details() {
        let reason = "fixture reason: \u{4fdd}\u{5b58}\u{6e08}\u{307f} \u{25c9}";
        let error = runtime_error(reason);
        assert_eq!(error.code, crate::protocol::ErrorCode::IoFailed);
        assert_eq!(error.message, "Cannot create the temporary IPC directory");
        assert_eq!(
            error.hint.as_deref(),
            Some(
                "Check free space and write permissions in the OS temporary directory, then restart Shellboy"
            )
        );
        assert_eq!(
            error.details,
            Some(serde_json::json!({
                "component": "local_ipc", "reason": reason
            }))
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn lease_rejects_busy_replaced_and_symlinked_folders_and_unlocks_its_final_owner() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("private");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let identity = super::super::folder_identity(&folder).unwrap();
        let lease = Lease::acquire(&folder, identity).unwrap();
        let inherited = lease.directory.try_clone().unwrap();
        assert!(Lease::acquire(&folder, identity).is_err());
        assert!(Lease::acquire(&folder, (identity.0, identity.1.wrapping_add(1))).is_err());
        let alias = root.path().join("alias");
        symlink(&folder, &alias).unwrap();
        assert!(Lease::acquire(&alias, identity).is_err());
        drop(lease);
        let next = Lease::acquire(&folder, identity).expect("final owner unlocks despite dup");
        drop(inherited);
        assert!(
            Lease::acquire(&folder, identity).is_err(),
            "old dup close cannot unlock new owner"
        );
        drop(next);
        assert!(Lease::acquire(&folder, identity).is_ok());
    }

    #[test]
    fn no_xdg_uses_safe_os_temporary_bases_and_names_are_random() {
        let uid = super::super::current_uid();
        let bases = runtime_bases_from(uid, None).unwrap();
        assert_eq!(bases.last(), Some(&std::fs::canonicalize("/tmp").unwrap()));
        let expected_user = os_user_tmp().and_then(|path| private_runtime_base(uid, path));
        assert_eq!(
            bases.first(),
            expected_user.as_ref().or_else(|| bases.last())
        );
        for base in &bases {
            let folder = base.join(namespace_name(uid));
            assert!(usable_socket_path(&folder));
            assert!(
                recognized_folder(&folder, uid, None).unwrap(),
                "restart cleanup recognizes each base"
            );
        }
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
        let fallback = runtime_bases_from(uid, None).unwrap();
        for candidate in [
            exposed.clone(),
            alias.clone(),
            PathBuf::from("relative"),
            PathBuf::from(format!("/{}", "x".repeat(200))),
        ] {
            assert_eq!(runtime_bases_from(uid, Some(candidate)).unwrap(), fallback);
        }
        assert!(alias.is_symlink());
        assert_eq!(std::fs::metadata(exposed).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn private_bases_preserve_xdg_priority_then_user_tmp_then_system_tmp() {
        let fallback = std::fs::canonicalize("/tmp").unwrap();
        // Keep this fixture's socket paths short even on macOS, where the
        // default tempfile root itself includes a user-specific long prefix.
        let root = tempfile::tempdir_in(&fallback).unwrap();
        let uid = super::super::current_uid();
        let xdg = root.path().join("xdg");
        let user = root.path().join("user");
        for path in [&xdg, &user] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(
            runtime_bases_with(uid, Some(xdg.clone()), Some(user.clone())).unwrap(),
            vec![xdg.clone(), user.clone(), fallback.clone()]
        );
        assert_eq!(
            runtime_bases_with(uid, Some(user.clone()), Some(user.clone())).unwrap(),
            vec![user.clone(), fallback.clone()],
            "duplicate bases do not multiply candidate attempts"
        );
        let alias = root.path().join("alias");
        symlink(&user, &alias).unwrap();
        let long = root.path().join("x".repeat(110));
        std::fs::create_dir(&long).unwrap();
        std::fs::set_permissions(&long, std::fs::Permissions::from_mode(0o700)).unwrap();
        let exposed = root.path().join("exposed");
        let protected_child = exposed.join("private");
        std::fs::create_dir(&exposed).unwrap();
        std::fs::create_dir(&protected_child).unwrap();
        std::fs::set_permissions(&exposed, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::set_permissions(&protected_child, std::fs::Permissions::from_mode(0o700)).unwrap();
        for unsafe_user in [
            alias.clone(),
            long,
            protected_child,
            PathBuf::from("relative"),
        ] {
            assert_eq!(
                runtime_bases_with(uid, Some(xdg.clone()), Some(unsafe_user)).unwrap(),
                vec![xdg.clone(), fallback.clone()],
                "unsafe or overlong user tmp must preserve the usable XDG and fallback"
            );
        }
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            runtime_bases_with(uid, None, Some(user.clone())).unwrap(),
            vec![fallback]
        );
        assert!(alias.is_symlink());
        assert_eq!(std::fs::metadata(user).unwrap().mode() & 0o777, 0o755);
        assert_eq!(std::fs::metadata(exposed).unwrap().mode() & 0o777, 0o777);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_os_user_tmp_precedes_system_tmp_when_safe_and_short_enough() {
        let uid = super::super::current_uid();
        let user = os_user_tmp().expect("Darwin supplies its per-user temporary directory");
        if let Some(canonical) = private_runtime_base(uid, user) {
            assert_eq!(runtime_bases_from(uid, None).unwrap()[0], canonical);
            assert!(recognized_folder(&canonical.join(namespace_name(uid)), uid, None).unwrap());
        } else {
            assert_eq!(
                runtime_bases_from(uid, None).unwrap(),
                vec![std::fs::canonicalize("/tmp").unwrap()],
                "unusable OS user temporary directory falls back without relaxing checks"
            );
        }
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
