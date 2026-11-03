//! Connection-scoped SQLite statement counters. SQL text and bindings stay private.
use rusqlite::{Connection, ffi};
use std::{ffi::c_void, ptr};

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Counters {
    pub fullscan_steps: u64,
    pub sorts: u64,
    pub autoindexes: u64,
    pub vm_steps: u64,
    pub elapsed_ns: u64,
}

pub(super) struct StatementObservation<'a> {
    connection: &'a Connection,
    counters: Box<Counters>,
}

unsafe extern "C" fn profile(
    event: u32,
    context: *mut c_void,
    statement: *mut c_void,
    elapsed: *mut c_void,
) -> i32 {
    if event != ffi::SQLITE_TRACE_PROFILE
        || context.is_null()
        || statement.is_null()
        || elapsed.is_null()
    {
        return 0;
    }
    // SQLite invokes this callback synchronously while the registration and
    // statement are alive; counters are owned by the registration guard.
    unsafe {
        let c = &mut *(context as *mut Counters);
        let stmt = statement.cast::<ffi::sqlite3_stmt>();
        c.fullscan_steps +=
            ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP, 1).max(0) as u64;
        c.sorts += ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_SORT, 1).max(0) as u64;
        c.autoindexes +=
            ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_AUTOINDEX, 1).max(0) as u64;
        c.vm_steps +=
            ffi::sqlite3_stmt_status(stmt, ffi::SQLITE_STMTSTATUS_VM_STEP, 1).max(0) as u64;
        c.elapsed_ns = c.elapsed_ns.saturating_add(*(elapsed as *const u64));
    }
    0
}

impl<'a> StatementObservation<'a> {
    pub fn install(connection: &'a Connection) -> rusqlite::Result<Self> {
        let mut counters = Box::<Counters>::default();
        // The guard removes the callback before dropping its context.
        let result = unsafe {
            ffi::sqlite3_trace_v2(
                connection.handle(),
                ffi::SQLITE_TRACE_PROFILE,
                Some(profile),
                (&mut *counters as *mut Counters).cast(),
            )
        };
        if result != ffi::SQLITE_OK {
            return Err(rusqlite::Error::SqliteFailure(
                ffi::Error::new(result),
                None,
            ));
        }
        Ok(Self {
            connection,
            counters,
        })
    }
    pub fn counters(&self) -> Counters {
        *self.counters
    }
}

impl Drop for StatementObservation<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::sqlite3_trace_v2(self.connection.handle(), 0, None, ptr::null_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captures_counters_without_sql_or_values() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3)")
            .unwrap();
        let trace = StatementObservation::install(&db).unwrap();
        let n: i64 = db
            .query_row("SELECT count(*) FROM t WHERE x > ?1", [1], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        let counters = trace.counters();
        assert!(counters.fullscan_steps > 0);
        assert!(counters.vm_steps > 0);
    }
}
