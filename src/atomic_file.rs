//! Same-directory temporary file, synced and renamed on successful publication.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub(crate) struct AtomicFile {
    temporary: PathBuf,
    file: Option<File>,
}

impl AtomicFile {
    pub(crate) fn new(temporary: PathBuf, private: bool) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        Ok(Self {
            file: Some(options.open(&temporary)?),
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
