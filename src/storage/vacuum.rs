//! Explicit `shellboy storage vacuum`: shrink the conversation DB file (#238).
//!
//! Standalone and owner-stopped only: it takes the data-directory owner lock
//! like `storage check` and refuses when the lock is unavailable. Nothing calls
//! it automatically. Only `shellboy.db` is rewritten; the standalone RAG store
//! (`rag.db`-style stores) is a separate file with its own lock and is not
//! touched.

use super::{Connection, OpenFlags, StorageError, register_vec};
use crate::protocol::{ApiError, ErrorCode};
use serde_json::{Value, json};
use std::path::Path;

/// SQLite VACUUM builds a copy of all live pages in a temporary file (at most
/// `live_bytes`; placed in `<data-dir>/tmp`) and writes every live page back
/// through the WAL: a 32-byte WAL header plus one frame per page, each frame a
/// 24-byte header followed by the page. The WAL index file (`-shm`) is counted at
/// its measured current size. Required free space is the sum of the three.
const BASIS: &str = "temp copy (live_bytes) + WAL (32 + live_pages * (page_size + 24)) + current -shm size; live_bytes = (page_count - freelist_count) * page_size";

fn sqlite(error: rusqlite::Error) -> ApiError {
    crate::app::storage_error(StorageError::Sqlite(error))
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Result row of `wal_checkpoint(TRUNCATE)`: (busy, log frames, checkpointed frames).
fn checkpoint_truncate(connection: &Connection) -> Result<(i64, i64, i64), ApiError> {
    connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(sqlite)
}

fn same_device(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev(),
            _ => true,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        true
    }
}

fn pragma(connection: &Connection, name: &str) -> Result<u64, ApiError> {
    let value: i64 = connection
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .map_err(sqlite)?;
    Ok(value.max(0) as u64)
}

pub fn vacuum(
    config: Option<&Path>,
    data_dir: Option<&Path>,
    cwd: &Path,
    dry_run: bool,
) -> Result<Value, ApiError> {
    let loaded = crate::config::Config::load_with_overrides(
        config,
        data_dir,
        cwd,
        &crate::config::PathEnvironment::current(),
    )
    .map_err(|error| ApiError::new(ErrorCode::ConfigInvalid, error.to_string()))?;
    vacuum_directory(&loaded.data_dir, dry_run, &|dir| fs2::available_space(dir))
}

/// `available` is injectable so tests can simulate a full filesystem.
pub(super) fn vacuum_directory(
    data_dir: &Path,
    dry_run: bool,
    available: &dyn Fn(&Path) -> std::io::Result<u64>,
) -> Result<Value, ApiError> {
    let _lock = crate::app::OwnerLock::acquire(data_dir)?;
    register_vec().map_err(crate::app::storage_error)?;
    let db_path = data_dir.join("shellboy.db");
    let wal_path = data_dir.join("shellboy.db-wal");
    let connection = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite)?;
    // Fail fast instead of waiting on a reader (request inspection and RAG
    // history reads open the DB without the owner lock).
    connection
        .busy_timeout(std::time::Duration::ZERO)
        .map_err(sqlite)?;
    if !dry_run {
        // Fold the WAL first so the "before" size is the real file and the
        // WAL does not carry stale frames into the rewrite.
        let (busy, log, done) = checkpoint_truncate(&connection)?;
        if busy != 0 || log != done {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Another process holds a read transaction on the database, so the WAL cannot be truncated",
            )
            .hint("Stop other shellboy commands that read this data directory, then run again")
            .details(json!({"wal_checkpoint":{"busy":busy,"log_frames":log,"checkpointed_frames":done}})));
        }
    } else {
        connection
            .execute_batch("PRAGMA query_only=ON")
            .map_err(sqlite)?;
    }
    let page_size = pragma(&connection, "page_size")?;
    let page_count = pragma(&connection, "page_count")?;
    let freelist = pragma(&connection, "freelist_count")?;
    let live_bytes = page_count.saturating_sub(freelist) * page_size;
    let live_pages = page_count.saturating_sub(freelist);
    let wal_bound = 32 + live_pages.saturating_mul(page_size + 24);
    let shm_bytes = file_len(&data_dir.join("shellboy.db-shm"));
    let required = live_bytes
        .saturating_add(wal_bound)
        .saturating_add(shm_bytes);
    let free = available(data_dir)
        .map_err(|error| ApiError::new(ErrorCode::IoFailed, error.to_string()))?;
    let before = file_len(&db_path);
    let mut result = json!({
        "dry_run": dry_run,
        "page_size": page_size,
        "page_count": page_count,
        "freelist_pages": freelist,
        "reclaimable_bytes": freelist * page_size,
        "db_file_bytes": before,
        "wal_bytes": file_len(&wal_path),
        "live_bytes": live_bytes,
        "required_temp_bytes": live_bytes,
        "required_wal_bytes": wal_bound,
        "shm_bytes": shm_bytes,
        "required_free_bytes": required,
        "required_free_basis": BASIS,
        "filesystem_free_bytes": free,
        "sufficient_space": free >= required,
    });
    // The temp copy lives in <data-dir>/tmp; check it separately only when that
    // directory is on another device than the data directory itself.
    let temp_dir = data_dir.join("tmp");
    let mut sufficient = free >= required;
    if temp_dir.is_dir() && !same_device(data_dir, &temp_dir) {
        let temp_free = available(&temp_dir)
            .map_err(|error| ApiError::new(ErrorCode::IoFailed, error.to_string()))?;
        result["tmp_filesystem_free_bytes"] = json!(temp_free);
        sufficient = free >= required.saturating_sub(live_bytes) && temp_free >= live_bytes;
    }
    result["sufficient_space"] = json!(sufficient);
    if !sufficient {
        if dry_run {
            // A preview still reports the shortfall; only the real run refuses.
            return Ok(result);
        }
        return Err(ApiError::new(
            ErrorCode::StorageLimit,
            "Not enough free space in the data directory filesystem to vacuum",
        )
        .details(result)
        .hint("Free the required space on the data directory filesystem, then run again"));
    }
    if dry_run {
        return Ok(result);
    }
    // Temp files go next to the DB so the space check above covers them.
    let temp = data_dir.join("tmp");
    std::fs::create_dir_all(&temp)
        .map_err(|error| ApiError::new(ErrorCode::IoFailed, error.to_string()))?;
    connection
        .pragma_update(
            None,
            "temp_store_directory",
            temp.to_string_lossy().as_ref(),
        )
        .map_err(sqlite)?;
    // temp_store_directory is process-global in SQLite: restore it afterwards so
    // a deleted directory cannot break later connections in the same process.
    struct ResetTemp<'a>(&'a Connection);
    impl Drop for ResetTemp<'_> {
        fn drop(&mut self) {
            let _ = self.0.pragma_update(None, "temp_store_directory", "");
        }
    }
    let reset = ResetTemp(&connection);
    // VACUUM is atomic: a failure or crash leaves the previous database intact.
    connection.execute_batch("VACUUM").map_err(sqlite)?;
    drop(reset);
    let (busy, log, done) = checkpoint_truncate(&connection)?;
    if busy != 0 || log != done {
        // VACUUM committed (the DB is consistent) but a reader keeps the WAL
        // from being folded back, so the file has not shrunk yet.
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "VACUUM completed but the WAL could not be truncated because another process holds a read transaction; the file has not shrunk yet",
        )
        .hint("Stop other shellboy commands that read this data directory; the WAL is folded in when the last connection closes")
        .details(json!({"wal_checkpoint":{"busy":busy,"log_frames":log,"checkpointed_frames":done},"db_file_bytes_before":before,"db_file_bytes":file_len(&db_path),"wal_bytes":file_len(&wal_path)})));
    }
    let quick: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(sqlite)?;
    if quick != "ok" {
        return Err(crate::app::storage_error(StorageError::Schema(format!(
            "quick_check after vacuum: {quick}"
        ))));
    }
    let after = file_len(&db_path);
    let map = result.as_object_mut().expect("object");
    map.insert("db_file_bytes_before".into(), json!(before));
    map.insert("db_file_bytes_after".into(), json!(after));
    map.insert("wal_bytes_after".into(), json!(file_len(&wal_path)));
    map.insert("bytes_freed".into(), json!(before.saturating_sub(after)));
    map.insert(
        "freelist_pages_after".into(),
        json!(pragma(&connection, "freelist_count")?),
    );
    map.insert("quick_check".into(), json!("ok"));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) -> Connection {
        let c = Connection::open(dir.join("shellboy.db")).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t(id INTEGER PRIMARY KEY, body BLOB);",
        )
        .unwrap();
        for _ in 0..200 {
            c.execute("INSERT INTO t(body) VALUES(randomblob(8000))", [])
                .unwrap();
        }
        c.execute("DELETE FROM t WHERE id % 4 != 0", []).unwrap();
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        c
    }
    fn real(dir: &Path) -> std::io::Result<u64> {
        fs2::available_space(dir)
    }

    #[test]
    fn dry_run_reports_pragma_facts_and_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let c = fixture(root.path());
        let (ps, pc, fl): (i64, i64, i64) = (
            c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap(),
            c.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap(),
            c.query_row("PRAGMA freelist_count", [], |r| r.get(0))
                .unwrap(),
        );
        drop(c);
        let (ps, pc, fl) = (ps as u64, pc as u64, fl as u64);
        let size = file_len(&root.path().join("shellboy.db"));
        let out = vacuum_directory(root.path(), true, &real).unwrap();
        assert!(fl > 0);
        assert_eq!(out["freelist_pages"], fl);
        assert_eq!(out["reclaimable_bytes"], fl * ps);
        assert_eq!(out["live_bytes"], (pc - fl) * ps);
        assert_eq!(
            out["required_free_bytes"],
            (pc - fl) * ps + 32 + (pc - fl) * (ps + 24) + out["shm_bytes"].as_u64().unwrap()
        );
        assert_eq!(out["db_file_bytes"], size);
        assert_eq!(out["dry_run"], true);
        assert!(out.get("db_file_bytes_after").is_none());
        assert_eq!(file_len(&root.path().join("shellboy.db")), size);
    }

    #[test]
    fn vacuum_shrinks_by_freelist_pages_and_keeps_integrity() {
        let root = tempfile::tempdir().unwrap();
        drop(fixture(root.path()));
        let path = root.path().join("shellboy.db");
        let before = vacuum_directory(root.path(), true, &real).unwrap();
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap());
        let out = vacuum_directory(root.path(), false, &real).unwrap();
        let reclaim = before["reclaimable_bytes"].as_u64().unwrap();
        assert_eq!(out["db_file_bytes_before"], before["db_file_bytes"]);
        assert_eq!(out["bytes_freed"].as_u64().unwrap(), reclaim);
        assert_eq!(out["freelist_pages_after"], 0);
        assert_eq!(out["wal_bytes_after"], 0);
        assert_eq!(
            file_len(&path),
            out["db_file_bytes_after"].as_u64().unwrap()
        );
        // In-place VACUUM keeps the file identity the RAG history store binds.
        #[cfg(unix)]
        assert_eq!(
            ino,
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap())
        );
        let c = Connection::open(&path).unwrap();
        let ok: String = c
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        let rows: i64 = c
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 50);
    }

    #[test]
    fn refuses_while_owner_lock_is_held() {
        let root = tempfile::tempdir().unwrap();
        drop(fixture(root.path()));
        let _owner = crate::app::OwnerLock::acquire(root.path()).unwrap();
        for dry in [true, false] {
            let error = vacuum_directory(root.path(), dry, &real).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidState);
        }
    }

    #[test]
    fn refuses_while_a_reader_holds_the_wal_and_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let writer = fixture(root.path());
        writer
            .execute("INSERT INTO t(body) VALUES(randomblob(100))", [])
            .unwrap();
        let path = root.path().join("shellboy.db");
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        let before = file_len(&path);
        let error = vacuum_directory(root.path(), false, &real).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(
            error.message.contains("read transaction"),
            "{}",
            error.message
        );
        assert_eq!(file_len(&path), before);
        reader.execute_batch("COMMIT").unwrap();
        drop(reader);
        drop(writer);
        let out = vacuum_directory(root.path(), false, &real).unwrap();
        assert!(out["bytes_freed"].as_u64().unwrap() > 0);
    }

    #[test]
    fn refuses_on_insufficient_space_and_leaves_db_untouched() {
        let root = tempfile::tempdir().unwrap();
        drop(fixture(root.path()));
        let path = root.path().join("shellboy.db");
        let before = std::fs::read(&path).unwrap();
        let error = vacuum_directory(root.path(), false, &|_| Ok(1)).unwrap_err();
        assert_eq!(error.code, ErrorCode::StorageLimit);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // The preview still reports the shortfall without failing.
        let out = vacuum_directory(root.path(), true, &|_| Ok(1)).unwrap();
        assert_eq!(out["sufficient_space"], false);
        assert_eq!(out["filesystem_free_bytes"], 1);
    }

    #[test]
    fn missing_database_is_not_created() {
        let root = tempfile::tempdir().unwrap();
        assert!(vacuum_directory(root.path(), false, &real).is_err());
        assert!(!root.path().join("shellboy.db").exists());
    }
}
