//! Connection-scoped SQLite statement counters. SQL text and bindings stay private.
use rusqlite::{Connection, ffi};
use std::{
    ffi::c_void,
    ptr,
    sync::{Arc, Mutex},
};

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub(crate) struct Counters {
    pub fullscan_steps: u64,
    pub sorts: u64,
    pub autoindexes: u64,
    pub vm_steps: u64,
    pub elapsed_ns: u64,
    pub rows: u64,
}

pub(crate) struct StatementObservation<'a> {
    connection: &'a mut Connection,
    counters: Arc<Mutex<Counters>>,
    installed: bool,
}

unsafe extern "C" fn profile(
    event: u32,
    context: *mut c_void,
    statement: *mut c_void,
    elapsed: *mut c_void,
) -> i32 {
    if context.is_null() || statement.is_null() {
        return 0;
    }
    if event == ffi::SQLITE_TRACE_ROW {
        // SQLite reports one callback for each generated result row.
        unsafe {
            let counters = &*(context as *const Mutex<Counters>);
            if let Ok(mut c) = counters.lock() {
                c.rows = c.rows.saturating_add(1);
            }
        }
        return 0;
    }
    if event != ffi::SQLITE_TRACE_PROFILE || elapsed.is_null() {
        return 0;
    }
    // SQLite invokes this callback synchronously while the registration and
    // statement are alive; counters are owned by the registration guard.
    unsafe {
        let counters = &*(context as *const Mutex<Counters>);
        let Ok(mut c) = counters.lock() else {
            return 0;
        };
        let stmt = statement.cast::<ffi::sqlite3_stmt>();
        c.fullscan_steps = c.fullscan_steps.saturating_add(
            ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP, 1).max(0) as u64,
        );
        c.sorts = c.sorts.saturating_add(
            ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_SORT, 1).max(0) as u64,
        );
        c.autoindexes = c.autoindexes.saturating_add(
            ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_AUTOINDEX, 1).max(0) as u64,
        );
        c.vm_steps = c.vm_steps.saturating_add(
            ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_VM_STEP, 1).max(0) as u64,
        );
        c.elapsed_ns = c.elapsed_ns.saturating_add(*(elapsed as *const u64));
    }
    0
}

impl<'a> StatementObservation<'a> {
    pub fn install(connection: &'a mut Connection) -> Self {
        let counters = Arc::new(Mutex::new(Counters::default()));
        // The guard removes the callback before dropping its context.
        let result = unsafe {
            ffi::sqlite3_trace_v2(
                connection.handle(),
                ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_ROW,
                Some(profile),
                Arc::as_ptr(&counters).cast_mut().cast(),
            )
        };
        Self {
            connection,
            counters,
            installed: result == ffi::SQLITE_OK,
        }
    }
    pub fn available(&self) -> bool {
        self.installed
    }
    pub fn connection(&mut self) -> &mut Connection {
        self.connection
    }
    pub fn counters(&self) -> Counters {
        self.counters.lock().map(|c| *c).unwrap_or_default()
    }
    pub(super) fn shared_counters(&self) -> Arc<Mutex<Counters>> {
        self.counters.clone()
    }
}

impl Drop for StatementObservation<'_> {
    fn drop(&mut self) {
        if self.installed {
            unsafe {
                ffi::sqlite3_trace_v2(self.connection.handle(), 0, None, ptr::null_mut());
            }
        }
    }
}

/// Record at most one executing-thread backtrace after the job threshold.
/// The capture is deliberately kept in memory until a privacy-safe symbol
/// representation is available; file paths and source lines must not be logged.
pub(super) struct SlowBacktrace {
    started: std::time::Instant,
    captured: Option<std::backtrace::Backtrace>,
}
impl SlowBacktrace {
    pub fn new(started: std::time::Instant) -> Self {
        Self {
            started,
            captured: None,
        }
    }
    pub fn observe(&mut self, now: std::time::Instant) {
        if self.captured.is_none()
            && now.saturating_duration_since(self.started) >= super::SLOW_JOB_THRESHOLD
        {
            self.captured = Some(std::backtrace::Backtrace::force_capture());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "manual hook overhead measurement"]
    fn measure_hook_overhead() {
        fn workload(db: &Connection) {
            let mut query = db
                .prepare_cached("SELECT value FROM bench WHERE id = ?1")
                .unwrap();
            for id in 0..50_000_i64 {
                let value: i64 = query.query_row([id % 10_000], |row| row.get(0)).unwrap();
                std::hint::black_box(value);
            }
        }
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE bench(id INTEGER PRIMARY KEY, value INTEGER); WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<9999) INSERT INTO bench SELECT x,x FROM n;").unwrap();
        workload(&db);
        for pass in 0..5 {
            let started = std::time::Instant::now();
            workload(&db);
            let baseline = started.elapsed();
            let mut observation = StatementObservation::install(&mut db);
            assert!(observation.available());
            let mut backtrace = SlowBacktrace::new(std::time::Instant::now());
            observation
                .connection()
                .progress_handler(
                    1000,
                    Some(move || {
                        backtrace.observe(std::time::Instant::now());
                        false
                    }),
                )
                .unwrap();
            let started = std::time::Instant::now();
            workload(observation.connection());
            let instrumented = started.elapsed();
            observation
                .connection()
                .progress_handler(0, None::<fn() -> bool>)
                .unwrap();
            drop(observation);
            eprintln!(
                "pass={pass} queries=50000 rows=10000 baseline_ns={} instrumented_ns={} overhead_pct={:.2}",
                baseline.as_nanos(),
                instrumented.as_nanos(),
                (instrumented.as_secs_f64() / baseline.as_secs_f64() - 1.0) * 100.0
            );
        }
    }

    #[test]
    fn backtrace_is_captured_once_after_threshold() {
        let started = std::time::Instant::now();
        let mut trace = SlowBacktrace::new(started);
        trace.observe(started);
        assert!(trace.captured.is_none());
        trace.observe(started + super::super::SLOW_JOB_THRESHOLD);
        let first = trace.captured.as_ref().unwrap() as *const _;
        trace.observe(started + super::super::SLOW_JOB_THRESHOLD * 2);
        assert_eq!(first, trace.captured.as_ref().unwrap() as *const _);
    }

    #[test]
    fn captures_counters_without_sql_or_values() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3)")
            .unwrap();
        let mut trace = StatementObservation::install(&mut db);
        let n: i64 = trace
            .connection()
            .query_row("SELECT count(*) FROM t WHERE x > ?1", [1], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        let counters = trace.counters();
        assert!(counters.fullscan_steps > 0);
        assert!(counters.vm_steps > 0);
        assert_eq!(counters.rows, 1);
        let mut stmt = trace
            .connection()
            .prepare("SELECT x FROM t ORDER BY x")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0)).unwrap();
        assert_eq!(rows.collect::<Result<Vec<_>, _>>().unwrap(), vec![1, 2, 3]);
        drop(stmt);
        assert_eq!(trace.counters().rows, 4);
    }
}
