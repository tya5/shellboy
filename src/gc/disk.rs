//! Free-space guard for the volume that holds the data directory.
//!
//! The gate in `require_capacity_tx` reads the OS value on every call: one
//! `statvfs` / `GetDiskFreeSpaceEx` call, no cache, so a stale value can neither
//! let the disk fill nor hold work after space returned. The volume is the one
//! that contains the database file, resolved by the OS (a symlinked data
//! directory therefore reports its target volume).
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

/// Free space relative to `limits.disk_free_min_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskState {
    /// At least twice the minimum is free.
    Ok,
    /// Below twice the minimum but not yet held.
    Warning,
    /// Below the minimum: new work is held.
    Held,
    /// The filesystem did not report free space; nothing is held.
    Unknown,
}

pub fn disk_state(free: Option<u64>, minimum: u64) -> DiskState {
    match free {
        None => DiskState::Unknown,
        Some(free) if free < minimum => DiskState::Held,
        Some(free) if free < minimum.saturating_mul(2) => DiskState::Warning,
        Some(_) => DiskState::Ok,
    }
}

pub(super) fn volume_free_result(directory: &Path) -> std::io::Result<u64> {
    #[cfg(test)]
    if let Some(forced) = tests::forced(directory) {
        return forced.ok_or_else(|| std::io::Error::other("free space unavailable"));
    }
    fs2::available_space(directory)
}
pub(super) fn volume_free(directory: &Path) -> Option<u64> {
    volume_free_result(directory).ok()
}

/// Free bytes of the volume holding this connection's database file; `None`
/// for an in-memory database or when the filesystem cannot report it.
pub(super) fn database_volume_free(db: &Connection) -> Option<u64> {
    let path = Path::new(db.path().filter(|path| !path.is_empty())?);
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    volume_free(directory)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Mutex, OnceLock},
    };

    type Forced = Mutex<HashMap<PathBuf, Option<u64>>>;
    fn table() -> &'static Forced {
        static TABLE: OnceLock<Forced> = OnceLock::new();
        TABLE.get_or_init(Forced::default)
    }
    pub(super) fn forced(directory: &Path) -> Option<Option<u64>> {
        // SQLite reports the canonical path (/private/var on macOS).
        let key = directory
            .canonicalize()
            .unwrap_or_else(|_| directory.to_path_buf());
        table().lock().unwrap().get(&key).copied()
    }
    /// Deterministic free space for one temporary data directory; other
    /// directories (and parallel tests) are unaffected.
    pub(crate) fn force_free(directory: &Path, free: Option<u64>) {
        let key = directory.canonicalize().unwrap();
        table().lock().unwrap().insert(key, free);
    }

    #[test]
    fn states_follow_the_minimum_and_its_double() {
        assert_eq!(disk_state(None, 10), DiskState::Unknown);
        assert_eq!(disk_state(Some(9), 10), DiskState::Held);
        assert_eq!(disk_state(Some(10), 10), DiskState::Warning);
        assert_eq!(disk_state(Some(19), 10), DiskState::Warning);
        assert_eq!(disk_state(Some(20), 10), DiskState::Ok);
        assert_eq!(disk_state(Some(0), u64::MAX), DiskState::Held);
    }

    #[test]
    fn symlinked_directory_reports_its_target_volume() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("real");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&target, &link).is_err() {
            return;
        }
        let through_link = volume_free(&link).expect("free space through a symlink");
        let direct = volume_free(&target).unwrap();
        // Same volume: the two reads differ only by concurrent activity.
        assert!(through_link.abs_diff(direct) < 1 << 30);
    }

    #[test]
    fn in_memory_database_has_unknown_free_space() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(database_volume_free(&db), None);
    }

    #[test]
    fn database_in_a_symlinked_directory_uses_the_directory_it_was_opened_through() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&real, &link).is_err() {
            return;
        }
        let db = Connection::open(link.join("shellboy.db")).unwrap();
        assert!(database_volume_free(&db).is_some());
    }

    use crate::{
        attachments::AttachmentService,
        config::Config,
        gc::{GcService, require_capacity_with},
        protocol::{ErrorCode, StorageCategory},
        storage::DbWorker,
    };
    use std::sync::Arc;

    fn schema() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db
    }

    #[test]
    fn gate_holds_below_the_minimum_with_a_distinct_reason() {
        let db = schema();
        let mut config = Config::default();
        config.limits.disk_free_min_bytes = 100;
        require_capacity_with(&db, &config, Some(100)).unwrap();
        require_capacity_with(&db, &config, Some(u64::MAX)).unwrap();
        // Unknown free space never holds.
        require_capacity_with(&db, &config, None).unwrap();
        let held = require_capacity_with(&db, &config, Some(99)).unwrap_err();
        assert_eq!(held.code, ErrorCode::StorageLimit);
        let details = held.details.unwrap();
        assert_eq!(details["reason"], "disk_free_low");
        assert_eq!(details["filesystem_free_bytes"], 99);
        assert_eq!(details["disk_free_min_bytes"], 100);
        // The logical limit keeps its own reason and wins when both apply.
        config.limits.data_max_bytes = 0;
        let both = require_capacity_with(&db, &config, Some(1)).unwrap_err();
        assert_eq!(both.details.unwrap()["reason"], "logical_limit");
        let logical = require_capacity_with(&db, &config, Some(1000)).unwrap_err();
        assert_eq!(logical.details.unwrap()["reason"], "logical_limit");
    }

    #[tokio::test]
    async fn held_for_low_disk_keeps_collection_and_observation_and_recovers() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.limits.disk_free_min_bytes = 1000;
        let db = DbWorker::open(temp.path().join("shellboy.db"))
            .await
            .unwrap();
        let attachments = Arc::new(AttachmentService::new(
            db.clone(),
            &config,
            temp.path().to_owned(),
            "fixture".into(),
        ));
        let gc = GcService::new(
            db.clone(),
            Arc::new(config),
            temp.path().to_owned(),
            attachments,
        );
        force_free(temp.path(), Some(2000));
        gc.require_capacity().await.unwrap();
        assert_eq!(
            serde_json::to_value(gc.details().await.unwrap()).unwrap()["disk_free_state"],
            "ok"
        );
        force_free(temp.path(), Some(1500));
        gc.require_capacity().await.unwrap();
        let warning = serde_json::to_value(gc.details().await.unwrap()).unwrap();
        assert_eq!(warning["disk_free_state"], "warning");
        assert_eq!(warning["filesystem_free_bytes"], 1500);
        force_free(temp.path(), Some(999));
        let held = gc.require_capacity().await.unwrap_err();
        assert_eq!(held.code, ErrorCode::StorageLimit);
        assert_eq!(held.details.as_ref().unwrap()["reason"], "disk_free_low");
        // Observation and the space-freeing operations are not gated.
        let details = serde_json::to_value(gc.details().await.unwrap()).unwrap();
        assert_eq!(details["disk_free_state"], "held");
        assert_eq!(details["disk_free_min_bytes"], 1000);
        assert_eq!(details["limit_reached"], false);
        let sample = gc
            .sample_observation_physical(
                tokio::time::Instant::from_std(
                    crate::test_watchdog::Deadline::new("disk guard observation").at(),
                ),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(sample.filesystem_free.bytes, Some(999));
        assert_eq!(sample.disk_guard.unwrap().state, DiskState::Held);
        gc.collect(StorageCategory::Raw, chrono::Utc::now())
            .await
            .unwrap();
        gc.collect(StorageCategory::History, chrono::Utc::now())
            .await
            .unwrap();
        // Space returned: the next call passes with no extra step.
        force_free(temp.path(), Some(1000));
        gc.require_capacity().await.unwrap();
        // Unknown free space is shown and does not hold.
        force_free(temp.path(), None);
        gc.require_capacity().await.unwrap();
        let unknown = serde_json::to_value(gc.details().await.unwrap()).unwrap();
        assert_eq!(unknown["disk_free_state"], "unknown");
        assert!(unknown.get("filesystem_free_bytes").is_none());
        let sample = gc
            .sample_observation_physical(
                tokio::time::Instant::from_std(
                    crate::test_watchdog::Deadline::new("disk guard observation").at(),
                ),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(sample.filesystem_free.bytes, None);
        assert!(sample.filesystem_free.error.is_some());
        assert_eq!(sample.disk_guard.unwrap().state, DiskState::Unknown);
        db.shutdown().await.unwrap();
    }

    /// Cost record, not an assertion: run with `--ignored --nocapture`.
    #[tokio::test]
    #[ignore = "measurement only; temporary store"]
    async fn measure_gate_cost() {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("shellboy.db"))
            .await
            .unwrap();
        let path = temp.path().to_owned();
        let (free_ns, counters_ns, gate_ns) = db
            .call(move |db| {
                let config = Config::default();
                let n = 20_000u32;
                let time = |f: &dyn Fn()| {
                    let start = std::time::Instant::now();
                    for _ in 0..n {
                        f();
                    }
                    start.elapsed().as_nanos() as u64 / u64::from(n)
                };
                let free = time(&|| {
                    std::hint::black_box(fs2::available_space(&path).unwrap());
                });
                let counters = time(&|| {
                    std::hint::black_box(crate::gc::usage::logical_usage(db).unwrap());
                });
                let gate = time(&|| {
                    std::hint::black_box(crate::gc::require_capacity_tx(db, &config).unwrap());
                });
                Ok((free, counters, gate))
            })
            .await
            .unwrap();
        println!(
            "statvfs {free_ns} ns, logical counters {counters_ns} ns, whole gate {gate_ns} ns (mean per call)"
        );
        db.shutdown().await.unwrap();
    }
}
