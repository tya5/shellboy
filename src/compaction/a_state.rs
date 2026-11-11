//! Bounded factual #A state; not yet wired to terminal/provider/Thread writers.
//! All writes require the caller's existing SQLite transaction.
use super::*;
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 4096;
const MAX_ID_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    version: u8,
    scope_generation: u64,
    boundary_generation: u64,
    connection: String,
    model: String,
    last_reset_origin: Option<String>,
    first: Option<Report>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Report { attempt: String, input_tokens: Option<u64> }
#[derive(Clone, Copy)]
pub(super) struct Token { scope: u64, boundary: u64 }
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reading { LegacyAbsent, AwaitingFirst, Known(u64), Unknown }

fn bad() -> ApiError { invalid("Invalid bounded automatic compaction fact state") }
fn id_ok(value: &str) -> bool { !value.is_empty() && value.len() <= MAX_ID_BYTES }
fn next(value: u64) -> Result<u64,ApiError> { value.checked_add(1).ok_or_else(bad) }
impl State {
    pub(super) fn new(connection: &str, model: &str) -> Result<Self,ApiError> {
        let state=Self { version:1,scope_generation:0,boundary_generation:0,connection:connection.into(),model:model.into(),last_reset_origin:None,first:None };
        state.validate()?;Ok(state)
    }
    fn validate(&self) -> Result<(),ApiError> {
        if self.version!=1 || !id_ok(&self.connection) || !id_ok(&self.model)
            || self.last_reset_origin.as_deref().is_some_and(|v|!id_ok(v))
            || self.first.as_ref().is_some_and(|v|!id_ok(&v.attempt)) { return Err(bad()); }
        Ok(())
    }
    pub(super) fn token(&self) -> Token { Token {scope:self.scope_generation,boundary:self.boundary_generation} }
    pub(super) fn reading(&self) -> Reading {
        match &self.first { None=>Reading::AwaitingFirst,Some(Report{input_tokens:Some(n),..})=>Reading::Known(*n),Some(_)=>Reading::Unknown }
    }
    pub(super) fn change_scope(&mut self, connection:&str, model:&str) -> Result<bool,ApiError> {
        if !id_ok(connection)||!id_ok(model) { return Err(bad()); }
        if self.connection==connection && self.model==model {return Ok(false);}
        let generation=next(self.scope_generation)?;
        let boundary=next(self.boundary_generation)?;
        self.scope_generation=generation;self.boundary_generation=boundary;
        self.connection=connection.into();self.model=model.into();self.first=None;self.last_reset_origin=None;
        Ok(true)
    }
    /// Call only after owning the terminal row CAS in this same transaction.
    pub(super) fn finish_boundary(&mut self, expected:Token, origin:&str) -> Result<bool,ApiError> {
        if !id_ok(origin) {return Err(bad());}
        if expected.scope!=self.scope_generation || expected.boundary!=self.boundary_generation
            || self.last_reset_origin.as_deref()==Some(origin) {return Ok(false);}
        let generation=next(self.boundary_generation)?;
        self.boundary_generation=generation;self.last_reset_origin=Some(origin.into());self.first=None;
        Ok(true)
    }
    /// Completion order is transaction order. An older boundary token is valid;
    /// an older SCOPE token is not, even after connection/model ABA.
    pub(super) fn completed(&mut self, started:Token, connection:&str, model:&str, attempt:&str, input_tokens:Option<u64>) -> Result<bool,ApiError> {
        if !id_ok(attempt) {return Err(bad());}
        if started.scope!=self.scope_generation || connection!=self.connection || model!=self.model || self.first.is_some() {return Ok(false);}
        self.first=Some(Report{attempt:attempt.into(),input_tokens});Ok(true)
    }
}
fn key(thread:&str) -> Result<String,ApiError> {
    if !id_ok(thread) {return Err(bad());}Ok(format!("compact.a_state.{thread}"))
}
pub(super) fn read(db:&rusqlite::Connection, thread:&str) -> Result<Option<State>,StorageError> {
    // length check occurs in SQLite, so oversized JSON is not copied into Rust.
    let row:Option<Option<String>>=db.query_row(
        "SELECT CASE WHEN length(CAST(value AS BLOB))<=?2 THEN value END FROM app_meta WHERE key=?1",
        params![key(thread)?,MAX_BYTES as i64],|r|r.get(0)).optional()?;
    match row {
        None=>Ok(None),
        Some(None)=>Err(bad().into()),
        Some(Some(text))=>{
            let state:State=serde_json::from_str(&text).map_err(|_|StorageError::from(bad()))?;
            state.validate()?;Ok(Some(state))
        }
    }
}
pub(super) fn save(tx:&rusqlite::Transaction<'_>, thread:&str, state:&State) -> Result<(),StorageError> {
    state.validate()?;
    let text=serde_json::to_string(state).map_err(|_|StorageError::from(bad()))?;
    if text.len()>MAX_BYTES {return Err(bad().into());}
    tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key(thread)?,text])?;
    Ok(())
}
pub(super) fn reading(state:Option<&State>) -> Reading { state.map(State::reading).unwrap_or(Reading::LegacyAbsent) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_completion_after_boundary_scope_aba_and_unknown_are_facts() {
        let mut s=State::new("A","model").unwrap();let started=s.token();
        assert!(s.finish_boundary(started,"trial1").unwrap());
        assert!(s.completed(started,"A","model","first",None).unwrap());
        assert_eq!(s.reading(),Reading::Unknown);
        assert!(!s.completed(started,"A","model","later",Some(999)).unwrap());
        assert!(s.change_scope("B","model").unwrap());assert!(s.change_scope("A","model").unwrap());
        assert!(!s.completed(started,"A","model","stale",Some(888)).unwrap());
        let fresh=s.token();assert!(s.completed(fresh,"A","model","fresh",Some(123)).unwrap());
        assert_eq!(s.reading(),Reading::Known(123));
    }
    #[test]
    fn reset_idempotence_and_stale_origin_never_erase_new_first() {
        let mut s=State::new("c","m").unwrap();let old=s.token();
        assert!(s.finish_boundary(old,"manual1").unwrap());
        let t=s.token();assert!(s.completed(t,"c","m","a",Some(10)).unwrap());
        assert!(!s.finish_boundary(old,"manual1").unwrap());
        assert!(!s.finish_boundary(t,"manual1").unwrap());
        assert_eq!(s.reading(),Reading::Known(10));
        assert!(s.finish_boundary(t,"trial2").unwrap());
        let now=s.token();assert!(s.completed(now,"c","m","b",Some(20)).unwrap());
        assert!(!s.finish_boundary(old,"manual1").unwrap());assert_eq!(s.reading(),Reading::Known(20));
    }
    #[test]
    fn transaction_rollback_bounded_read_and_legacy_absence() {
        let mut db=rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);").unwrap();
        assert_eq!(reading(read(&db,"t").unwrap().as_ref()),Reading::LegacyAbsent);
        {let tx=db.transaction().unwrap();save(&tx,"t",&State::new("c","m").unwrap()).unwrap();}
        assert!(read(&db,"t").unwrap().is_none());
        {let tx=db.transaction().unwrap();save(&tx,"t",&State::new("c","m").unwrap()).unwrap();tx.commit().unwrap();}
        assert_eq!(reading(read(&db,"t").unwrap().as_ref()),Reading::AwaitingFirst);
        db.execute("UPDATE app_meta SET value=?1",["x".repeat(MAX_BYTES+1)]).unwrap();assert!(read(&db,"t").is_err());
        db.execute("UPDATE app_meta SET value='{}'",[]).unwrap();assert!(read(&db,"t").is_err());
        assert!(State::new(&"x".repeat(MAX_ID_BYTES+1),"m").is_err());
        let mut s=State::new("c","m").unwrap();s.scope_generation=u64::MAX;
        let before=s.clone();assert!(s.change_scope("d","m").is_err());assert_eq!(s,before);
        s.boundary_generation=u64::MAX;assert!(s.finish_boundary(s.token(),"x").is_err());
    }
}
