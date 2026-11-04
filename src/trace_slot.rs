//! SQLite trace-slot owner. Only instrumentation and the fixed test fixture use it.
use rusqlite::ffi;
use std::cell::Cell;
use std::ffi::{CStr, c_void};

thread_local! {
    static SLOW_COMMIT_FIXTURE: Cell<bool> = const { Cell::new(false) };
    static FIXTURE_HITS: Cell<usize> = const { Cell::new(0) };
}

/// Enables the fixed slow-COMMIT fixture on the current DB worker thread.
#[doc(hidden)]
pub fn set_slow_commit_fixture(enabled: bool) {
    SLOW_COMMIT_FIXTURE.with(|v| v.set(enabled));
    if enabled {
        FIXTURE_HITS.with(|v| v.set(0));
    }
}

#[doc(hidden)]
pub fn slow_commit_fixture_hits() -> usize {
    FIXTURE_HITS.with(Cell::get)
}

fn fixture_event(event: u32, statement: *mut c_void) {
    if event != ffi::SQLITE_TRACE_PROFILE
        || statement.is_null()
        || !SLOW_COMMIT_FIXTURE.with(Cell::get)
    {
        return;
    }
    let sql = unsafe { ffi::sqlite3_sql(statement.cast()) };
    if !sql.is_null()
        && unsafe { CStr::from_ptr(sql) }
            .to_bytes()
            .eq_ignore_ascii_case(b"COMMIT")
    {
        FIXTURE_HITS.with(|hits| hits.set(hits.get().saturating_add(1)));
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

pub(crate) unsafe extern "C" fn observation_callback(
    event: u32,
    context: *mut c_void,
    statement: *mut c_void,
    elapsed: *mut c_void,
) -> i32 {
    fixture_event(event, statement);
    unsafe { crate::storage::statement_observation::profile(event, context, statement, elapsed) }
}

pub(crate) unsafe fn register(
    db: *mut ffi::sqlite3,
    mask: u32,
    callback: Option<unsafe extern "C" fn(u32, *mut c_void, *mut c_void, *mut c_void) -> i32>,
    context: *mut c_void,
) -> i32 {
    unsafe { ffi::sqlite3_trace_v2(db, mask, callback, context) }
}
