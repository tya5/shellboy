//! Same-directory temporary file, synced and renamed on successful publication.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;

pub(crate) fn create_new(path: &Path, private: bool) -> io::Result<File> {
    #[cfg(windows)]
    if private {
        return windows::private_create(path);
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(windows)]
pub(crate) fn privacy_warning(path: &Path) -> Option<&'static str> {
    windows::warning(path)
}

pub(crate) struct AtomicFile {
    temporary: PathBuf,
    file: Option<File>,
}

impl AtomicFile {
    pub(crate) fn new(temporary: PathBuf, private: bool) -> io::Result<Self> {
        Ok(Self {
            file: Some(create_new(&temporary, private)?),
            temporary,
        })
    }

    pub(crate) fn file(&mut self) -> &mut File {
        self.file.as_mut().expect("temporary file is open")
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.temporary
    }

    pub(crate) fn publish(mut self, target: &Path) -> io::Result<()> {
        let file = self.file.take().expect("temporary file is open");
        file.sync_all()?;
        drop(file);
        let renamed = std::fs::rename(&self.temporary, target);
        #[cfg(windows)]
        let renamed = renamed.map_err(|error| windows::diagnose_rename_error(target, error));
        renamed?;
        self.temporary.clear();
        Ok(())
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.temporary.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
}

/// Keep the stable I/O error code and distinguish native sharing conflicts by reason.
pub(crate) fn file_in_use(error: &io::Error) -> Option<crate::protocol::ApiError> {
    #[cfg(windows)]
    {
        use crate::protocol::{ApiError, ErrorCode};
        use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
        if error.raw_os_error().is_some_and(|code| {
            matches!(code as u32, ERROR_LOCK_VIOLATION | ERROR_SHARING_VIOLATION)
        }) {
            return Some(
                ApiError::new(ErrorCode::IoFailed, "File in use")
                    .hint("Close programs using the file, then try again")
                    .details(serde_json::json!({"reason":"file_in_use","written":false})),
            );
        }
    }
    let _ = error;
    None
}
