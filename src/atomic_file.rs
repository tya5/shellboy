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
        std::fs::rename(&self.temporary, target)?;
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
