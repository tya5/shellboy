//! Connection-scoped SQLite statement counters. SQL text and bindings stay private.
#[path = "sql_normalize.rs"]
mod sql_normalize;

use rusqlite::{Connection, ffi};
use std::{
    collections::HashMap,
    ffi::{CStr, c_void},
    ptr,
    sync::{Arc, Mutex},
};

/// Profile counters cover completed statements; rows also include the active statement.
#[derive(Debug, Default, Clone)]
pub(crate) struct Counters {
    pub fullscan_steps: u64,
    pub sorts: u64,
    pub autoindexes: u64,
    pub vm_steps: u64,
    pub elapsed_ns: u64,
    pub rows: Option<u64>,
    row_count: Arc<std::sync::atomic::AtomicU64>,
    pub row_scope: Option<&'static str>,
    pub counter_scope: Option<&'static str>,
    pub fingerprint: Option<&'static str>,
    pub plan: Option<[Option<&'static str>; 8]>,
    current_fingerprint: Option<&'static str>,
    fingerprint_frozen: bool,
    pub sql: Option<Arc<str>>,
    current_sql: Option<Arc<str>>,
    current_statement: usize,
    captured_statement: usize,
    captured_profile_recorded: bool,
}

impl Counters {
    pub(super) fn snapshot(&self) -> Self {
        let mut result = self.clone();
        if result.rows.is_some() {
            result.rows = Some(self.row_count.load(std::sync::atomic::Ordering::Relaxed));
        }
        result
    }
    fn clear_current_if(&mut self, statement: usize) {
        if self.current_statement == statement {
            self.current_statement = 0;
            self.current_sql = None;
            self.current_fingerprint = None;
        }
    }
}

impl serde::Serialize for Counters {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        if let Some(scope) = self.counter_scope {
            map.serialize_entry("counter_scope", scope)?;
            map.serialize_entry("fullscan_steps", &self.fullscan_steps)?;
            map.serialize_entry("sorts", &self.sorts)?;
            map.serialize_entry("autoindexes", &self.autoindexes)?;
            map.serialize_entry("vm_steps", &self.vm_steps)?;
            map.serialize_entry("elapsed_ns", &self.elapsed_ns)?;
        }
        if let Some(rows) = self.rows {
            map.serialize_entry("rows", &rows)?;
        }
        if let Some(scope) = self.row_scope {
            map.serialize_entry("row_scope", scope)?;
        }
        if let Some(fingerprint) = self.fingerprint {
            map.serialize_entry("fingerprint", fingerprint)?;
        }
        if let Some(sql) = &self.sql {
            map.serialize_entry("sql", sql.as_ref())?;
            map.serialize_entry(
                "sql_scope",
                if self.fingerprint_frozen {
                    "overrun_statement"
                } else {
                    "last_sampled_statement"
                },
            )?;
        }
        if let Some(plan) = self.plan {
            map.serialize_entry("plan", &plan)?;
        }
        map.end()
    }
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
    exhaustive: bool,
    sample_every_profile: bool,
    row_count: Arc<std::sync::atomic::AtomicU64>,
    last_sql: Vec<u8>,
    last_shape: Option<Arc<str>>,
}

impl TraceContext {
    fn shape(&mut self, sql: &[u8]) -> Arc<str> {
        if let Some(shape) = &self.last_shape
            && self.last_sql == sql
        {
            return shape.clone();
        }
        let shape: Arc<str> = sql_normalize::normalize(sql).into();
        if sql.len() <= 4096 {
            self.last_sql.clear();
            self.last_sql.extend_from_slice(sql);
            self.last_shape = Some(shape.clone());
        } else {
            self.last_sql.clear();
            self.last_shape = None;
        }
        shape
    }
}

// Only exact, reviewed static statements are eligible for diagnostic output.
fn safe_fingerprint(sql: &[u8]) -> Option<&'static str> {
    match sql {
        b"SELECT deleted_at FROM thread WHERE id=?1" => Some("thread.deleted_at_by_id"),
        b"SELECT parent_thread_id FROM thread WHERE id=?1" => Some("thread.parent_by_id"),
        b"SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)" => {
            Some("thread.live_by_id")
        }
        _ => None,
    }
}

/// Explain only reviewed statements on a separate read-only connection. Plan
/// text is reduced to fixed categories so schema names and values never escape.
pub(crate) fn add_readonly_plan(path: &std::path::Path, counters: &mut Counters) {
    let sql = match counters.fingerprint {
        Some("thread.deleted_at_by_id") => "SELECT deleted_at FROM thread WHERE id=?1",
        Some("thread.parent_by_id") => "SELECT parent_thread_id FROM thread WHERE id=?1",
        Some("thread.live_by_id") => {
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)"
        }
        _ => return,
    };
    let result = std::panic::catch_unwind(|| -> rusqlite::Result<[Option<&'static str>; 8]> {
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut statement = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let mut rows = statement.query([rusqlite::types::Null])?;
        let mut plan = [None; 8];
        for item in &mut plan {
            let Some(row) = rows.next()? else { break };
            let detail: String = row.get(3)?;
            *item = Some(if detail.starts_with("SEARCH ") {
                "search"
            } else if detail.starts_with("SCAN ") {
                "scan"
            } else if detail.starts_with("USE TEMP B-TREE") {
                "temp_btree"
            } else {
                "other"
            });
        }
        Ok(plan)
    });
    counters.plan = result.ok().and_then(Result::ok);
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

fn sample_profile(duration_ns: u64) -> bool {
    duration_ns >= 1_000_000
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
    if !context.exhaustive {
        if event == ffi::SQLITE_TRACE_STMT {
            let sql = unsafe { ffi::sqlite3_sql(stmt) };
            let (shape, fingerprint) = if sql.is_null() {
                (None, None)
            } else {
                let sql = unsafe { CStr::from_ptr(sql) }.to_bytes();
                (Some(context.shape(sql)), safe_fingerprint(sql))
            };
            if let Ok(mut counters) = context.counters.lock() {
                counters.current_statement = stmt as usize;
                counters.current_sql = shape;
                counters.current_fingerprint = fingerprint;
            }
            return 0;
        }
        if event == ffi::SQLITE_TRACE_ROW {
            let _ = context.row_count.try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |rows| Some(rows.saturating_add(1)),
            );
            return 0;
        }
        if event != ffi::SQLITE_TRACE_PROFILE || elapsed.is_null() {
            return 0;
        }
        let duration = unsafe { *(elapsed as *const u64) };
        if !context.sample_every_profile && !sample_profile(duration) {
            if let Ok(mut counters) = context.counters.lock() {
                if counters.fingerprint_frozen && counters.captured_statement == stmt as usize {
                    counters.captured_profile_recorded = true;
                }
                counters.clear_current_if(stmt as usize);
            }
            return 0;
        }
        let values = unsafe { statement_counts(stmt) };
        if let Ok(mut counters) = context.counters.lock() {
            if counters.fingerprint_frozen {
                if counters.captured_statement != stmt as usize
                    || counters.captured_profile_recorded
                {
                    counters.clear_current_if(stmt as usize);
                    return 0;
                }
                counters.captured_profile_recorded = true;
            } else {
                counters.sql = counters.current_sql.clone();
            }
            counters.fullscan_steps = values[0];
            counters.sorts = values[1];
            counters.autoindexes = values[2];
            counters.vm_steps = values[3];
            counters.elapsed_ns = duration;
            counters.counter_scope = Some("statement_lifetime_cumulative");
            let sql = unsafe { ffi::sqlite3_sql(stmt) };
            if !counters.fingerprint_frozen {
                counters.fingerprint = if sql.is_null() {
                    None
                } else {
                    safe_fingerprint(unsafe { CStr::from_ptr(sql) }.to_bytes())
                };
            }
            counters.clear_current_if(stmt as usize);
        }
        return 0;
    }
    if event == ffi::SQLITE_TRACE_STMT {
        if !elapsed.is_null()
            && unsafe { CStr::from_ptr(elapsed.cast()) }
                .to_bytes()
                .starts_with(b"--")
        {
            return 0;
        }
        let sql = unsafe { ffi::sqlite3_sql(stmt) };
        if let Ok(mut counters) = context.counters.lock() {
            counters.current_fingerprint = if sql.is_null() {
                None
            } else {
                safe_fingerprint(unsafe { CStr::from_ptr(sql) }.to_bytes())
            };
        }
        context
            .baselines
            .insert(stmt as usize, unsafe { statement_counts(stmt) });
        return 0;
    }
    if event == ffi::SQLITE_TRACE_ROW {
        // SQLite reports one callback for each generated result row.
        let _ = context.row_count.try_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |rows| Some(rows.saturating_add(1)),
        );
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
        c.counter_scope = Some("job_cumulative");
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
        let mut observation = Self::install_production(connection);
        observation._context.sample_every_profile = cfg!(test);
        observation
    }
    fn install_production(connection: &'a mut Connection) -> Self {
        Self::install_using(connection, |db, context| unsafe {
            ffi::sqlite3_trace_v2(
                db,
                ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_ROW,
                Some(profile),
                context,
            )
        })
    }
    #[cfg(test)]
    pub(crate) fn install_exhaustive_for_tests(connection: &'a mut Connection) -> Self {
        let mut observation = Self::install_using(connection, |db, context| unsafe {
            ffi::sqlite3_trace_v2(
                db,
                ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE | ffi::SQLITE_TRACE_ROW,
                Some(profile),
                context,
            )
        });
        observation._context.exhaustive = true;
        observation
    }
    fn install_using(
        connection: &'a mut Connection,
        register: impl FnOnce(*mut ffi::sqlite3, *mut c_void) -> i32,
    ) -> Self {
        let counters = Arc::new(Mutex::new(Counters {
            rows: Some(0),
            row_scope: Some("job_cumulative"),
            ..Counters::default()
        }));
        let mut context = Box::new(TraceContext {
            counters: counters.clone(),
            baselines: HashMap::new(),
            exhaustive: false,
            sample_every_profile: false,
            row_count: counters.lock().unwrap().row_count.clone(),
            last_sql: Vec::new(),
            last_shape: None,
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
        self.counters
            .lock()
            .map(|c| c.snapshot())
            .unwrap_or_default()
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
    counters: Option<Arc<Mutex<Counters>>>,
}
impl SlowBacktrace {
    pub fn new(started: std::time::Instant, counters: Option<Arc<Mutex<Counters>>>) -> Self {
        Self {
            started,
            captured: Arc::new(Mutex::new(None)),
            attempted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            counters,
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
            if let Some(counters) = &self.counters
                && let Ok(mut counters) = counters.lock()
            {
                counters.fingerprint = counters.current_fingerprint;
                counters.sql = counters.current_sql.clone();
                counters.captured_statement = counters.current_statement;
                counters.counter_scope = None;
                counters.fingerprint_frozen = true;
            }
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
    fn overrun_captures_executing_statement_not_later_sql() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE thread(id TEXT PRIMARY KEY,deleted_at TEXT); INSERT INTO thread VALUES ('private-id',NULL)").unwrap();
        let mut observation = StatementObservation::install(&mut db);
        let start = std::time::Instant::now();
        let shared = observation.shared_counters();
        let trace = SlowBacktrace::new(start, Some(shared.clone()));
        let active = Arc::new(Mutex::new(None));
        let snapshot = active.clone();
        observation
            .connection()
            .progress_handler(
                1,
                Some(move || {
                    trace.observe(start + super::super::SLOW_JOB_THRESHOLD);
                    let mut snapshot = snapshot.lock().unwrap();
                    if snapshot.is_none() {
                        *snapshot = Some(shared.lock().unwrap().snapshot());
                    }
                    false
                }),
            )
            .unwrap();
        let _: Option<String> = observation
            .connection()
            .query_row(
                "SELECT deleted_at FROM thread WHERE id=?1",
                ["private-id"],
                |row| row.get(0),
            )
            .unwrap();
        observation
            .connection()
            .progress_handler(0, None::<fn() -> bool>)
            .unwrap();
        observation
            .connection()
            .create_scalar_function(
                "later_delay",
                0,
                rusqlite::functions::FunctionFlags::empty(),
                |_| {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    Ok(1)
                },
            )
            .unwrap();
        observation
            .connection()
            .execute_batch("SELECT later_delay()")
            .unwrap();
        let active = active.lock().unwrap().clone().unwrap();
        assert_eq!(active.sql, observation.counters().sql);
        assert!(active.sql.as_deref().unwrap().contains("WHERE"));
        let json = serde_json::to_string(&observation.counters()).unwrap();
        assert!(json.contains("thread.deleted_at_by_id"));
        assert!(!json.contains("private-id"));
    }

    #[test]
    fn fingerprint_is_exact_and_plan_is_read_only_and_bounded() {
        assert_eq!(
            safe_fingerprint(b"SELECT deleted_at FROM thread WHERE id=?1"),
            Some("thread.deleted_at_by_id")
        );
        assert_eq!(
            safe_fingerprint(b"SELECT deleted_at FROM thread WHERE id='secret'"),
            None
        );
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE thread(id TEXT PRIMARY KEY, deleted_at TEXT)")
            .unwrap();
        let mut counters = Counters {
            fingerprint: Some("thread.deleted_at_by_id"),
            ..Default::default()
        };
        add_readonly_plan(&path, &mut counters);
        assert_eq!(counters.plan.unwrap()[0], Some("search"));
        assert!(
            serde_json::to_string(&counters)
                .unwrap()
                .contains("thread.deleted_at_by_id")
        );
        let missing = root.path().join("missing");
        counters.plan = None;
        add_readonly_plan(&missing, &mut counters);
        assert!(counters.plan.is_none());
        assert!(!missing.exists());
    }

    #[test]
    #[ignore = "manual component overhead measurement"]
    fn measure_hook_components() {
        unsafe extern "C" fn callback(
            event: u32,
            ctx: *mut c_void,
            stmt: *mut c_void,
            _: *mut c_void,
        ) -> i32 {
            if event == ffi::SQLITE_TRACE_PROFILE && !ctx.is_null() {
                std::hint::black_box(unsafe { statement_counts(stmt.cast()) });
            }
            0
        }
        fn workload(db: &Connection) {
            let mut q = db
                .prepare_cached("SELECT value FROM bench WHERE id = ?1")
                .unwrap();
            for id in 0..50_000_i64 {
                std::hint::black_box(q.query_row([id % 10_000], |r| r.get::<_, i64>(0)).unwrap());
            }
        }
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE bench(id INTEGER PRIMARY KEY,value INTEGER); WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<9999) INSERT INTO bench SELECT x,x FROM n;").unwrap();
        workload(&db);
        for pass in 0..7 {
            for mode in ["baseline", "profile", "profile_status", "progress"] {
                let raw = unsafe { db.handle() };
                if mode.starts_with("profile") {
                    let ctx = if mode == "profile_status" {
                        std::ptr::dangling_mut::<u8>().cast()
                    } else {
                        ptr::null_mut()
                    };
                    assert_eq!(
                        unsafe {
                            ffi::sqlite3_trace_v2(
                                raw,
                                ffi::SQLITE_TRACE_PROFILE,
                                Some(callback),
                                ctx,
                            )
                        },
                        ffi::SQLITE_OK
                    );
                }
                if mode == "progress" {
                    db.progress_handler(
                        1000,
                        Some(|| {
                            std::hint::black_box(std::time::Instant::now());
                            false
                        }),
                    )
                    .unwrap();
                }
                let start = std::time::Instant::now();
                workload(&db);
                eprintln!("pass={pass} mode={mode} ns={}", start.elapsed().as_nanos());
                if mode.starts_with("profile") {
                    unsafe {
                        ffi::sqlite3_trace_v2(raw, 0, None, ptr::null_mut());
                    }
                }
                if mode == "progress" {
                    db.progress_handler(0, None::<fn() -> bool>).unwrap();
                }
            }
        }
    }

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
            let backtrace = SlowBacktrace::new(std::time::Instant::now(), None);
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
    #[ignore = "manual comparison of production row and statement hooks"]
    fn measure_final_hook_variants() {
        fn workload(db: &Connection, scan: bool) {
            if scan {
                let mut statement = db
                    .prepare_cached("SELECT value FROM bench WHERE id < 1000")
                    .unwrap();
                for _ in 0..100 {
                    let mut rows = statement.query([]).unwrap();
                    while let Some(row) = rows.next().unwrap() {
                        std::hint::black_box(row.get::<_, i64>(0).unwrap());
                    }
                }
            } else {
                let mut statement = db
                    .prepare_cached("SELECT value FROM bench WHERE id = ?1")
                    .unwrap();
                for id in 0..50000_i64 {
                    std::hint::black_box(
                        statement
                            .query_row([id % 10000], |r| r.get::<_, i64>(0))
                            .unwrap(),
                    );
                }
            }
        }
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE bench(id INTEGER PRIMARY KEY,value INTEGER); WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<9999) INSERT INTO bench SELECT x,x FROM n;").unwrap();
        for scan in [false, true] {
            workload(&db, scan);
            for pass in 0..7 {
                let start = std::time::Instant::now();
                workload(&db, scan);
                let baseline = start.elapsed();
                let mut profile_only =
                    StatementObservation::install_using(&mut db, |db, context| unsafe {
                        ffi::sqlite3_trace_v2(db, ffi::SQLITE_TRACE_PROFILE, Some(profile), context)
                    });
                let start = std::time::Instant::now();
                workload(profile_only.connection(), scan);
                let profile_time = start.elapsed();
                drop(profile_only);
                let mut all = StatementObservation::install_production(&mut db);
                let capture = SlowBacktrace::new(std::time::Instant::now(), None);
                all.connection()
                    .progress_handler(
                        1000,
                        Some(move || {
                            capture.observe(std::time::Instant::now());
                            false
                        }),
                    )
                    .unwrap();
                let start = std::time::Instant::now();
                workload(all.connection(), scan);
                let full = start.elapsed();
                all.connection()
                    .progress_handler(0, None::<fn() -> bool>)
                    .unwrap();
                drop(all);
                eprintln!(
                    "scan={scan} pass={pass} baseline_ns={} profile_ns={} production_ns={}",
                    baseline.as_nanos(),
                    profile_time.as_nanos(),
                    full.as_nanos()
                );
            }
        }
    }

    #[test]
    fn normal_test_install_samples_fast_profiles() {
        let mut db = Connection::open_in_memory().unwrap();
        let mut observation = StatementObservation::install(&mut db);
        let _: i64 = observation
            .connection()
            .query_row("SELECT 1", [], |row| row.get(0))
            .unwrap();
        let counters = observation.counters();
        assert_eq!(
            counters.counter_scope,
            Some("statement_lifetime_cumulative")
        );
        assert!(counters.vm_steps > 0);
    }

    #[test]
    fn production_sampling_ignores_fast_statements_and_labels_slow_cumulative_counts() {
        let mut db = Connection::open_in_memory().unwrap();
        db.create_scalar_function(
            "pause",
            0,
            rusqlite::functions::FunctionFlags::empty(),
            |_| {
                std::thread::sleep(std::time::Duration::from_millis(4));
                Ok(1_i64)
            },
        )
        .unwrap();
        let mut observation = StatementObservation::install_production(&mut db);
        let _: i64 = observation
            .connection()
            .query_row("SELECT 1", [], |r| r.get(0))
            .unwrap();
        assert!(!sample_profile(999_999));
        assert!(sample_profile(1_000_000));
        let fast = observation.counters();
        assert_eq!(fast.rows, Some(1));
        if fast.counter_scope.is_some() {
            assert!(fast.elapsed_ns >= 1_000_000);
        }
        let _: i64 = observation
            .connection()
            .query_row("SELECT pause()", [], |r| r.get(0))
            .unwrap();
        let counters = observation.counters();
        assert_eq!(
            counters.counter_scope,
            Some("statement_lifetime_cumulative")
        );
        assert!(counters.vm_steps > 0);
        assert!(counters.elapsed_ns >= 1_000_000);
        assert_eq!(counters.rows, Some(2));
        assert_eq!(counters.row_scope, Some("job_cumulative"));
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
        let trace = SlowBacktrace::new(started, None);
        let later = started + super::super::SLOW_JOB_THRESHOLD;
        trace.observe_using(later, || panic!("injected capture failure"));
        assert!(trace.captured.lock().unwrap().is_none());
        trace.observe_using(later, std::backtrace::Backtrace::force_capture);
        assert!(trace.captured.lock().unwrap().is_none());
    }

    #[test]
    fn backtrace_is_captured_once_after_threshold() {
        let started = std::time::Instant::now();
        let trace = SlowBacktrace::new(started, None);
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
        let mut trace = StatementObservation::install_exhaustive_for_tests(&mut db);
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
        assert_eq!(observed.rows, Some(4));
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
        let mut trace = StatementObservation::install_exhaustive_for_tests(&mut db);
        let n: i64 = trace
            .connection()
            .query_row("SELECT count(*) FROM t WHERE x > ?1", [1], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        let counters = trace.counters();
        assert!(counters.fullscan_steps > 0);
        assert!(counters.vm_steps > 0);
        assert_eq!(counters.rows, Some(1));
        let mut stmt = trace
            .connection()
            .prepare("SELECT x FROM t ORDER BY x")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0)).unwrap();
        assert_eq!(rows.collect::<Result<Vec<_>, _>>().unwrap(), vec![1, 2, 3]);
        drop(stmt);
        assert_eq!(trace.counters().rows, Some(4));
    }
}
