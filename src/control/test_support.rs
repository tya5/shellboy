//! Shared relational fixtures for terminal attempt transition tests.

use rusqlite::Connection;

/// Build a real thread → run → turn → model-attempt scope with the fields read
/// by both terminal CAS logic and bounded automatic-compaction accounting.
pub(crate) fn terminal_attempt_fixture(attempt_id: &str, state: &str) -> Connection {
    let db = Connection::open_in_memory().expect("open terminal-attempt fixture");
    db.execute_batch(
        "CREATE TABLE thread(id TEXT PRIMARY KEY);
         CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE run(id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,state TEXT NOT NULL);
         CREATE TABLE turn(id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,run_id TEXT NOT NULL,state TEXT NOT NULL,generation INTEGER NOT NULL);
         CREATE TABLE model_attempt(
             id TEXT PRIMARY KEY,turn_id TEXT NOT NULL,state TEXT NOT NULL,generation INTEGER NOT NULL,
             error_json TEXT,response_id TEXT,usage_json TEXT,ended_at TEXT,
             input_manifest_json TEXT NOT NULL DEFAULT '{}',profile_id TEXT NOT NULL,model_id TEXT NOT NULL
         );
         INSERT INTO thread VALUES('t');
         INSERT INTO run VALUES('run','t','live');
         INSERT INTO turn VALUES('turn','t','run','running',1);",
    )
    .expect("create relational terminal-attempt fixture");
    db.execute(
        "INSERT INTO model_attempt(id,turn_id,state,generation,error_json,response_id,usage_json,ended_at,input_manifest_json,profile_id,model_id)
         VALUES(?1,'turn',?2,1,'{\"original\":true}','original','{}','original','{}','c','m')",
        rusqlite::params![attempt_id, state],
    )
    .expect("insert scoped model-attempt fixture");
    db
}
