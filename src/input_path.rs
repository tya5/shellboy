//! Local input paths only: no filesystem access, canonicalization, or wire policy.
use std::path::{Path, PathBuf};

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
