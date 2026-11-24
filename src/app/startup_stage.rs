//! Local owner-startup observation, not an endpoint or liveness signal.
use super::{StartupProgress, StartupStage};
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const FILE: &str = "startup-stage.json";

pub(crate) struct DbProgress {
    pub directory: PathBuf,
    pub watch: Option<tokio::sync::watch::Sender<StartupProgress>>,
}

// Only the lock holder calls this, including on the DB writer thread. A
// snapshot failure must not change whether opening or migrating SQLite works.
pub(crate) fn publish(directory: &Path, stage: StartupStage) {
    let result = (|| -> io::Result<()> {
        let temporary = directory.join("startup-stage.tmp");
        remove_if_present(&temporary)?;
        let bytes = serde_json::to_vec(&stage)?;
        let mut output = crate::atomic_file::AtomicFile::new(temporary, true)?;
        output.file().write_all(&bytes)?;
        output.publish(&directory.join(FILE))
    })();
    if let Err(error) = result {
        eprintln!("Could not publish owner startup stage {stage:?}: {error}");
    }
}

pub(super) fn clear(directory: &Path) -> io::Result<()> {
    remove_if_present(&directory.join(FILE))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Read the last stage only. Never opens the database or connects to the owner.
pub async fn read(directory: &Path) -> Option<StartupStage> {
    let path = directory.join(FILE);
    tokio::task::spawn_blocking(move || {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(65).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 64 {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    })
    .await
    .ok()?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_stage_migration_crash_child() {
        let Some(directory) = std::env::var_os("SB_STARTUP_STAGE_CRASH_FIXTURE") else {
            return;
        };
        let directory = PathBuf::from(directory);
        let _owner = super::super::OwnerLock::acquire(&directory).unwrap();
        publish(&directory, StartupStage::DatabaseMigration);
        // Abrupt process exit bypasses lock and snapshot cleanup.
        std::process::exit(42);
    }

    #[tokio::test]
    async fn startup_stage_is_readable_after_mid_migration_exit() {
        let directory = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::startup_stage::tests::startup_stage_migration_crash_child",
            ])
            .env("SB_STARTUP_STAGE_CRASH_FIXTURE", directory.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(42), "{output:?}");
        assert_eq!(
            read(directory.path()).await,
            Some(StartupStage::DatabaseMigration)
        );
        assert!(!directory.path().join("endpoint.json").exists());
    }
}
