//! Connection-scoped SQLite statement counters. SQL text and bindings stay private.
use rusqlite::{Connection, ffi};
use std::{
    collections::HashMap,
    ffi::{CStr, c_void},
    ptr,
    sync::{Arc, Mutex},
};

/// Profile counters cover completed statements; rows also include the active statement.
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
    _context: Box<TraceContext>,
    installed: bool,
}

struct TraceContext {
    counters: Arc<Mutex<Counters>>,
    baselines: HashMap<usize, [u64; 4]>,
}

unsafe fn statement_counts(stmt: *mut ffi::sqlite3_stmt) -> [u64; 4] {
    [
        ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP,
        ffi::SQLITE_STMTSTATUS_SORT,
        ffi::SQLITE_STMTSTATUS_AUTOINDEX,
        ffi::SQLITE_STMTSTATUS_VM_STEP,
    ]
    .map(|op| unsafe { ffi::sqlite3_stmt_status(stmt, op, 0).max(0) as u64 })
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
    let context = unsafe { &mut *context.cast::<TraceContext>() };
    let stmt = statement.cast::<ffi::sqlite3_stmt>();
    if event == ffi::SQLITE_TRACE_STMT {
        if !elapsed.is_null()
            && unsafe { CStr::from_ptr(elapsed.cast()) }
                .to_bytes()
                .starts_with(b"--")
        {
            return 0;
        }
        context
            .baselines
            .insert(stmt as usize, unsafe { statement_counts(stmt) });
        return 0;
    }
    if event == ffi::SQLITE_TRACE_ROW {
        // SQLite reports one callback for each generated result row.
        if let Ok(mut c) = context.counters.lock() {
            c.rows = c.rows.saturating_add(1);
        }
        return 0;
    }
    if event != ffi::SQLITE_TRACE_PROFILE || elapsed.is_null() {
        return 0;
    }
    // SQLite invokes this callback synchronously while the registration and
    // statement are alive; counters are owned by the registration guard.
    unsafe {
        let baseline = context
            .baselines
            .remove(&(stmt as usize))
            .unwrap_or_default();
        let current = statement_counts(stmt);
        let Ok(mut c) = context.counters.lock() else {
            return 0;
        };
        c.fullscan_steps = c
            .fullscan_steps
            .saturating_add(current[0].saturating_sub(baseline[0]));
        c.sorts = c
            .sorts
            .saturating_add(current[1].saturating_sub(baseline[1]));
        c.autoindexes = c
            .autoindexes
            .saturating_add(current[2].saturating_sub(baseline[2]));
        c.vm_steps = c
            .vm_steps
            .saturating_add(current[3].saturating_sub(baseline[3]));
        c.elapsed_ns = c.elapsed_ns.saturating_add(*(elapsed as *const u64));
    }
    0
}

impl<'a> StatementObservation<'a> {
    pub fn install(connection: &'a mut Connection) -> Self {
        Self::install_using(connection, |db, context| unsafe {
            ffi::sqlite3_trace_v2(
                db,
                ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_ROW,
                Some(profile),
                context,
            )
        })
    }
    fn install_using(
        connection: &'a mut Connection,
        register: impl FnOnce(*mut ffi::sqlite3, *mut c_void) -> i32,
    ) -> Self {
        let counters = Arc::new(Mutex::new(Counters::default()));
        let mut context = Box::new(TraceContext {
            counters: counters.clone(),
            baselines: HashMap::new(),
        });
        // The guard removes the callback before dropping its context.
        let result = register(
            unsafe { connection.handle() },
            (&mut *context as *mut TraceContext).cast(),
        );
        Self {
            connection,
            counters,
            _context: context,
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

/// Capture at most once per job, on the thread running SQLite.
#[derive(Clone)]
pub(super) struct SlowBacktrace {
    started: std::time::Instant,
    captured: Arc<Mutex<Option<std::backtrace::Backtrace>>>,
    attempted: Arc<std::sync::atomic::AtomicBool>,
}
impl SlowBacktrace {
    pub fn new(started: std::time::Instant) -> Self {
        Self {
            started,
            captured: Arc::new(Mutex::new(None)),
            attempted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    pub fn observe(&self, now: std::time::Instant) {
        self.observe_using(now, std::backtrace::Backtrace::force_capture);
    }
    fn observe_using(
        &self,
        now: std::time::Instant,
        capture: impl FnOnce() -> std::backtrace::Backtrace + std::panic::UnwindSafe,
    ) {
        if now.saturating_duration_since(self.started) < super::SLOW_JOB_THRESHOLD {
            return;
        }
        if let Ok(mut captured) = self.captured.try_lock()
            && !self
                .attempted
                .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            *captured = std::panic::catch_unwind(capture).ok();
        }
    }
    pub fn symbols(&self) -> Option<Vec<String>> {
        let captured = self.captured.try_lock().ok()?;
        let symbols = safe_symbols(&captured.as_ref()?.to_string());
        (!symbols.is_empty()).then_some(symbols)
    }
}

// Only compiler-produced Shellboy symbol identifiers are safe to emit.
// Backtrace formatting is not stable, so unknown formats are omitted.
fn safe_symbols(rendered: &str) -> Vec<String> {
    rendered
        .lines()
        .filter_map(|line| {
            let (_, symbol) = line.trim().split_once(": ")?;
            let symbol = symbol.trim();
            if symbol.len() <= 200
                && symbol.starts_with("shellboy::")
                && symbol
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':')
            {
                Some(symbol.to_owned())
            } else {
                None
            }
        })
        .take(32)
        .collect()
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
            let backtrace = SlowBacktrace::new(std::time::Instant::now());
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
    fn symbols_omit_paths_lines_and_untrusted_text() {
        let input = "   0: shellboy::storage::run_worker\n             at /private/test.rs:42:5\n   1: user::secret\n   2: shellboy::bad/path\n   3: shellboy::ok_42\n";
        assert_eq!(
            safe_symbols(input),
            vec!["shellboy::storage::run_worker", "shellboy::ok_42"]
        );
    }

    #[test]
    fn failed_backtrace_capture_is_not_retried() {
        let started = std::time::Instant::now();
        let trace = SlowBacktrace::new(started);
        let later = started + super::super::SLOW_JOB_THRESHOLD;
        trace.observe_using(later, || panic!("injected capture failure"));
        assert!(trace.captured.lock().unwrap().is_none());
        trace.observe_using(later, std::backtrace::Backtrace::force_capture);
        assert!(trace.captured.lock().unwrap().is_none());
    }

    #[test]
    fn backtrace_is_captured_once_after_threshold() {
        let started = std::time::Instant::now();
        let trace = SlowBacktrace::new(started);
        trace.observe(started);
        assert!(trace.captured.lock().unwrap().is_none());
        trace.observe(started + super::super::SLOW_JOB_THRESHOLD);
        assert!(trace.captured.lock().unwrap().is_some());
        let first = trace.captured.lock().unwrap().as_ref().unwrap() as *const _;
        trace.observe(started + super::super::SLOW_JOB_THRESHOLD * 2);
        assert_eq!(
            first,
            trace.captured.lock().unwrap().as_ref().unwrap() as *const _
        );
    }

    #[test]
    fn preserves_statement_status_and_counts_reused_statements_once_per_execution() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);")
            .unwrap();
        let mut trace = StatementObservation::install(&mut db);
        let mut statement = trace
            .connection()
            .prepare("SELECT x FROM t WHERE x > ?1")
            .unwrap();
        for _ in 0..2 {
            let rows = statement
                .query_map([1], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows, vec![2, 3]);
        }
        let vm_steps = statement.get_status(rusqlite::StatementStatus::VmStep);
        let fullscan_steps = statement.get_status(rusqlite::StatementStatus::FullscanStep);
        assert!(vm_steps > 0);
        assert!(fullscan_steps > 0);
        drop(statement);
        let observed = trace.counters();
        assert_eq!(observed.vm_steps, vm_steps as u64);
        assert_eq!(observed.fullscan_steps, fullscan_steps as u64);
        assert_eq!(observed.rows, 4);
    }

    #[test]
    fn unavailable_trace_does_not_prevent_database_work() {
        let mut db = Connection::open_in_memory().unwrap();
        let mut observation =
            StatementObservation::install_using(&mut db, |_, _| ffi::SQLITE_ERROR);
        assert!(!observation.available());
        let result: i64 = observation
            .connection()
            .query_row("SELECT 42", [], |row| row.get(0))
            .unwrap();
        assert_eq!(result, 42);
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
