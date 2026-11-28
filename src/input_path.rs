//! Local input paths only: no filesystem access, canonicalization, or wire policy.
use std::path::{Path, PathBuf};

/// Maximum entries observed by a registered-folder walk.
pub(crate) const FOLDER_WALK_MAX_DEPTH: usize = 64;
pub(crate) const FOLDER_WALK_MAX_ENTRIES: usize = 100_000;

pub(crate) fn relative_to(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    }
}
pub(crate) fn utf8(path: &Path) -> Result<&str, &'static str> {
    path.to_str().ok_or("path must be representable as UTF-8")
}
