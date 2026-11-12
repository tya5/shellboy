//! Bounded factual #A state; not yet wired to terminal/provider/Thread writers.
//! All writes require the caller's existing SQLite transaction.
use super::*;
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 4096;
const MAX_ID_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
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
#[derive(Clone, Copy, Deserialize, Serialize)]
pub(crate) struct Token { scope: u64, boundary: u64 }
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reading { LegacyAbsent, AwaitingFirst, Known(u64), Unknown }

fn bad() -> ApiError { invalid("Invalid bounded automatic compaction fact state") }
fn id_ok(value: &str) -> bool { !value.is_empty() && value.len() <= MAX_ID_BYTES }
fn next(value: u64) -> Result<u64,ApiError> { value.checked_add(1).ok_or_else(bad) }
impl State {
    pub(crate) fn new(connection: &str, model: &str) -> Result<Self,ApiError> {
        let state=Self { version:1,scope_generation:0,boundary_generation:0,connection:connection.into(),model:model.into(),last_reset_origin:None,first:None };
        state.validate()?;Ok(state)
    }
    fn validate(&self) -> Result<(),ApiError> {
        if self.version!=1 || !id_ok(&self.connection) || !id_ok(&self.model)
            || self.last_reset_origin.as_deref().is_some_and(|v|!id_ok(v))
            || self.first.as_ref().is_some_and(|v|!id_ok(&v.attempt)) { return Err(bad()); }
        Ok(())
    }
    pub(crate) fn token(&self) -> Token { Token {scope:self.scope_generation,boundary:self.boundary_generation} }
    pub(crate) fn reading(&self) -> Reading {
        match &self.first { None=>Reading::AwaitingFirst,Some(Report{input_tokens:Some(n),..})=>Reading::Known(*n),Some(_)=>Reading::Unknown }
    }
    pub(crate) fn change_scope(&mut self, connection:&str, model:&str) -> Result<bool,ApiError> {
        if !id_ok(connection)||!id_ok(model) { return Err(bad()); }
        if self.connection==connection && self.model==model {return Ok(false);}
        let generation=next(self.scope_generation)?;
        let boundary=next(self.boundary_generation)?;
        self.scope_generation=generation;self.boundary_generation=boundary;
        self.connection=connection.into();self.model=model.into();self.first=None;self.last_reset_origin=None;
        Ok(true)
    }
    /// Call only after owning the terminal row CAS in this same transaction.
    pub(crate) fn finish_boundary(&mut self, expected:Token, origin:&str) -> Result<bool,ApiError> {
        if !id_ok(origin) {return Err(bad());}
        if expected.scope!=self.scope_generation || expected.boundary!=self.boundary_generation
            || self.last_reset_origin.as_deref()==Some(origin) {return Ok(false);}
        let generation=next(self.boundary_generation)?;
        self.boundary_generation=generation;self.last_reset_origin=Some(origin.into());self.first=None;
        Ok(true)
    }
    /// Completion order is transaction order. An older boundary token is valid;
    /// an older SCOPE token is not, even after connection/model ABA.
    pub(crate) fn completed(&mut self, started:Token, connection:&str, model:&str, attempt:&str, input_tokens:Option<u64>) -> Result<bool,ApiError> {
        if !id_ok(attempt) {return Err(bad());}
        if started.scope!=self.scope_generation || connection!=self.connection || model!=self.model || self.first.is_some() {return Ok(false);}
        self.first=Some(Report{attempt:attempt.into(),input_tokens});Ok(true)
    }
}
fn key(thread:&str) -> Result<String,ApiError> {
    if !id_ok(thread) {return Err(bad());}Ok(format!("compact.a_state.{thread}"))
}
pub(crate) fn read(db:&rusqlite::Connection, thread:&str) -> Result<Option<State>,StorageError> {
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
pub(crate) fn save(tx:&rusqlite::Connection, thread:&str, state:&State) -> Result<(),StorageError> {
    state.validate()?;
    let text=serde_json::to_string(state).map_err(|_|StorageError::from(bad()))?;
    if text.len()>MAX_BYTES {return Err(bad().into());}
    tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key(thread)?,text])?;
    Ok(())
}
pub(crate) fn reading(state:Option<&State>) -> Reading { state.map(State::reading).unwrap_or(Reading::LegacyAbsent) }

/// Decimal text preserves the full u64 counter domain across SQLite JSON1.
pub(crate) fn decode_generation(text:Option<&str>)->Result<Option<u64>,StorageError> {
    text.map(|value| {
        if value.is_empty() || value.len()>20 || !value.bytes().all(|b|b.is_ascii_digit()) {return Err(StorageError::from(bad()));}
        value.parse::<u64>().map_err(|_|StorageError::from(bad()))
    }).transpose()
}

pub(crate) fn scope_tx(db:&rusqlite::Connection, thread:&str, connection:&str, model:&str) -> Result<State,StorageError> {
    let mut state=read(db,thread)?.unwrap_or(State::new(connection,model)?);
    state.change_scope(connection,model)?;save(db,thread,&state)?;Ok(state)
}
pub(crate) fn begin_report_tx(db:&rusqlite::Connection, thread:&str, connection:&str, model:&str) -> Result<u64,StorageError> {
    Ok(scope_tx(db,thread,connection,model)?.scope_generation)
}
pub(crate) fn complete_report_tx(db:&rusqlite::Connection, thread:&str, connection:&str, model:&str, attempt:&str, generation:Option<u64>, input:Option<u64>) -> Result<(),StorageError> {
    let Some(generation)=generation else {return Ok(());}; // Legacy inflight attempt has no causal scope token.
    let Some(mut state)=read(db,thread)? else {return Ok(());};
    if state.completed(Token{scope:generation,boundary:0},connection,model,attempt,input)? {save(db,thread,&state)?;}
    Ok(())
}
pub(crate) fn reset_tx(db:&rusqlite::Connection,thread:&str,expected:Token,origin:&str)->Result<(),StorageError> {
    if let Some(mut state)=read(db,thread)? {
        if state.finish_boundary(expected,origin)? {save(db,thread,&state)?;}
    }
    Ok(())
}
pub(crate) fn cleanup_tx(db:&rusqlite::Connection, thread:&str) -> Result<(),StorageError> {
    for key in [key(thread)?,format!("compact.deterministic_range_failure.closed.{thread}"),format!("compact.deterministic_range_failure.active.{thread}")] {
        db.execute("DELETE FROM app_meta WHERE key=?1",[key])?;
    }
    Ok(())
}

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
    #[test]
    fn populated_old_boundary_is_cleared_and_duplicate_does_not_erase_new_first() {
        let mut db=rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);").unwrap();
        let mut state=State::new("c","m").unwrap();let old=state.token();
        assert!(state.completed(old,"c","m","report1",Some(111)).unwrap());
        {let tx=db.transaction().unwrap();save(&tx,"t",&state).unwrap();tx.commit().unwrap();}
        let mut state=read(&db,"t").unwrap().unwrap();
        assert!(state.finish_boundary(old,"trial2").unwrap());
        assert_eq!(state.first,None,"reset clears both identity and tokens atomically");
        let new=state.token();
        assert!(state.completed(old,"c","m","report2",Some(222)).unwrap(),"pre-boundary start may complete after reset");
        assert!(!state.finish_boundary(old,"trial2").unwrap());
        assert!(!state.finish_boundary(new,"trial2").unwrap(),"same origin remains idempotent with fresh token too");
        {let tx=db.transaction().unwrap();save(&tx,"t",&state).unwrap();tx.commit().unwrap();}
        let persisted=read(&db,"t").unwrap().unwrap();
        assert_eq!(persisted.first,Some(Report{attempt:"report2".into(),input_tokens:Some(222)}));
        assert_eq!(persisted.reading(),Reading::Known(222));
    }

}

#[cfg(test)]
mod transactional_reset_tests {
    use super::*;
    #[test]
    fn provider_report_and_terminal_cas_rollback_share_atomic_state() {
        let mut db=rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE terminal(id TEXT PRIMARY KEY,done INTEGER NOT NULL);INSERT INTO terminal VALUES('trial',0);").unwrap();
        let (initial,boundary)={let tx=db.transaction().unwrap();let state=scope_tx(&tx,"t","c","m").unwrap();let token=state.token();save(&tx,"t",&state).unwrap();tx.commit().unwrap();(state,token)};
        {
            let tx=db.transaction().unwrap();
            assert_eq!(tx.execute("UPDATE terminal SET done=1 WHERE id='trial' AND done=0",[]).unwrap(),1);
            reset_tx(&tx,"t",boundary,"trial").unwrap();
            // Explicit transaction drop rolls back terminal ownership AND #A.
        }
        assert_eq!(read(&db,"t").unwrap().unwrap(),initial);
        let tx=db.transaction().unwrap();
        assert_eq!(tx.execute("UPDATE terminal SET done=1 WHERE id='trial' AND done=0",[]).unwrap(),1);
        reset_tx(&tx,"t",boundary,"trial").unwrap();
        complete_report_tx(&tx,"t","c","m","first",Some(boundary.scope),None).unwrap();
        tx.commit().unwrap();
        assert_eq!(read(&db,"t").unwrap().unwrap().reading(),Reading::Unknown);
        let tx=db.transaction().unwrap();
        assert_eq!(tx.execute("UPDATE terminal SET done=1 WHERE id='trial' AND done=0",[]).unwrap(),0);
        complete_report_tx(&tx,"t","c","m","later",Some(boundary.scope),Some(9)).unwrap();
        tx.commit().unwrap();
        assert_eq!(read(&db,"t").unwrap().unwrap().reading(),Reading::Unknown);
    }
    #[test]
    fn generation_decimal_has_no_signed_wrap_or_domain_truncation() {
        assert_eq!(decode_generation(Some(&u64::MAX.to_string())).unwrap(),Some(u64::MAX));
        assert_eq!(decode_generation(None).unwrap(),None);
        for value in ["-1","1.0","1e20","18446744073709551616",""] {assert!(decode_generation(Some(value)).is_err());}
    }

}
