//! Test-only actual SQLite statement observations; no production facility.
use rusqlite::Connection;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Observe real top-level SQLite statement executions, without a new dependency
/// or production counter. The hook and its pointee live only for this fixture.
pub(crate) struct Statements<'a> {
    db: &'a Connection,
    count: Box<AtomicUsize>,
}
impl<'a> Statements<'a> {
    pub(crate) fn new(db: &'a Connection) -> Self {
        let count = Box::new(AtomicUsize::new(0));
        extern "C" fn trace(
            _: u32,
            context: *mut std::ffi::c_void,
            _: *mut std::ffi::c_void,
            _: *mut std::ffi::c_void,
        ) -> i32 {
            // SAFETY: context is the boxed counter owned by Statements; Drop
            // removes the hook before that box is released. No concurrent DB use.
            let count = unsafe { &*context.cast::<AtomicUsize>() };
            count.fetch_add(1, Ordering::Relaxed);
            0
        }
        // SAFETY: this fixture owns the connection; callback context remains at
        // a stable boxed address until the hook is explicitly removed in Drop.
        let result = unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                db.handle(),
                rusqlite::ffi::SQLITE_TRACE_STMT,
                Some(trace),
                std::ptr::from_ref(count.as_ref()).cast_mut().cast(),
            )
        };
        assert_eq!(result, rusqlite::ffi::SQLITE_OK);
        Self { db, count }
    }
    pub(crate) fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }
}
impl Drop for Statements<'_> {
    fn drop(&mut self) {
        // SAFETY: connection outlives this guard; unregister before count drops.
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(self.db.handle(), 0, None, std::ptr::null_mut());
        }
    }
}
