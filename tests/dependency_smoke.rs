//! Development probes, not a production DB worker or migration.
use rusqlite::{Connection, ffi::sqlite3_auto_extension};

#[test]
fn bundled_sqlite_vec_search_and_rollback() -> rusqlite::Result<()> {
    // SAFETY: sqlite-vec exports the SQLite extension entry point with the ABI
    // required by sqlite3_auto_extension, as in its pinned upstream Rust example.
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
    let db = Connection::open_in_memory()?;
    let version: String = db.query_row("SELECT vec_version()", [], |row| row.get(0))?;
    assert_eq!(version, "v0.1.9");
    db.execute_batch("CREATE VIRTUAL TABLE vectors USING vec0(embedding float[3]);")?;
    db.execute(
        "INSERT INTO vectors(rowid, embedding) VALUES (1, '[1,0,0]')",
        [],
    )?;
    db.execute_batch(
        "BEGIN; INSERT INTO vectors(rowid, embedding) VALUES (2, '[0,1,0]'); ROLLBACK;",
    )?;
    let count: i64 = db.query_row("SELECT count(*) FROM vectors", [], |row| row.get(0))?;
    assert_eq!(count, 1);
    db.execute_batch("BEGIN; DELETE FROM vectors WHERE rowid = 1; ROLLBACK;")?;
    let nearest: i64 = db.query_row(
        "SELECT rowid FROM vectors WHERE embedding MATCH '[1,0,0]' AND k = 1 ORDER BY distance",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(nearest, 1);
    db.execute("DELETE FROM vectors WHERE rowid = 1", [])?;
    let count: i64 = db.query_row("SELECT count(*) FROM vectors", [], |row| row.get(0))?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn shipped_schema_builds_with_bundled_sqlite() -> rusqlite::Result<()> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(include_str!("../migrations/0001_initial.sql"))?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 35);
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    assert_eq!(integrity, "ok");
    assert!(!db.prepare("PRAGMA foreign_key_check")?.exists([])?);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_runtime_and_tui_backend_compile() {
    let value = tokio::spawn(async { 42 }).await.expect("task completes");
    assert_eq!(value, 42);
    let backend = ratatui::backend::TestBackend::new(40, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            frame.render_widget(ratatui::widgets::Paragraph::new("Shellboy"), frame.area())
        })
        .expect("render");
    assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "S");
}
