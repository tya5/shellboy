//! Bounded factual #A state, transactionally updated from provider request tokens.
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
    scope_identity: ScopeIdentity,
    last_reset_origin: Option<String>,
    first: Option<Report>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub(crate) struct ScopeIdentity(String);

impl ScopeIdentity {
    pub(crate) fn new(connection: &str, model: &str) -> Result<Self, ApiError> {
        if connection.is_empty() || model.is_empty() {
            return Err(bad());
        }

        let framed =
            serde_json::to_vec(&("shellboy.compaction.a-state.scope.v2", connection, model))
                .expect("a tuple of strings serializes to JSON");
        Ok(Self(crate::digest::sha256_hex(framed)))
    }
    fn valid(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    attempt: String,
    input_tokens: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Token {
    scope: u64,
    boundary: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reading {
    LegacyAbsent,
    AwaitingFirst,
    Known(u64),
}

fn bad() -> ApiError {
    invalid("Invalid bounded automatic compaction fact state")
}
fn id_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID_BYTES
}
fn next(value: u64) -> Result<u64, ApiError> {
    value.checked_add(1).ok_or_else(bad)
}
impl State {
    pub(crate) fn new(connection: &str, model: &str) -> Result<Self, ApiError> {
        let state = Self {
            version: 2,
            scope_generation: 0,
            boundary_generation: 0,
            scope_identity: ScopeIdentity::new(connection, model)?,
            last_reset_origin: None,
            first: None,
        };
        state.validate()?;
        Ok(state)
    }
    fn validate(&self) -> Result<(), ApiError> {
        if self.version != 2
            || !self.scope_identity.valid()
            || self.last_reset_origin.as_deref().is_some_and(|v| !id_ok(v))
            || self.first.as_ref().is_some_and(|v| !id_ok(&v.attempt))
        {
            return Err(bad());
        }
        Ok(())
    }
    pub(crate) fn token(&self) -> Token {
        Token {
            scope: self.scope_generation,
            boundary: self.boundary_generation,
        }
    }
    pub(crate) fn reading(&self) -> Reading {
        match &self.first {
            None => Reading::AwaitingFirst,
            Some(Report {
                input_tokens: Some(n),
                ..
            }) => Reading::Known(*n),
            // Missing usage is not a known report and never consumes #A.
            Some(Report {
                input_tokens: None, ..
            }) => Reading::AwaitingFirst,
        }
    }
    pub(crate) fn change_scope(&mut self, connection: &str, model: &str) -> Result<bool, ApiError> {
        let identity = ScopeIdentity::new(connection, model)?;
        if self.scope_identity == identity {
            return Ok(false);
        }
        let generation = next(self.scope_generation)?;
        let boundary = next(self.boundary_generation)?;
        self.scope_generation = generation;
        self.boundary_generation = boundary;
        self.scope_identity = identity;
        self.first = None;
        self.last_reset_origin = None;
        Ok(true)
    }
    /// Call only after owning the terminal row CAS in this same transaction.
    pub(crate) fn finish_boundary(
        &mut self,
        expected: Token,
        origin: &str,
    ) -> Result<bool, ApiError> {
        if !id_ok(origin) {
            return Err(bad());
        }
        if expected.scope != self.scope_generation
            || expected.boundary != self.boundary_generation
            || self.last_reset_origin.as_deref() == Some(origin)
        {
            return Ok(false);
        }
        let generation = next(self.boundary_generation)?;
        self.boundary_generation = generation;
        self.last_reset_origin = Some(origin.into());
        self.first = None;
        Ok(true)
    }
    /// Completion order is transaction order. Both the captured scope and boundary
    /// token must still be current, even after connection/model ABA.
    pub(crate) fn completed(
        &mut self,
        started: Token,
        connection: &str,
        model: &str,
        attempt: &str,
        input_tokens: Option<u64>,
    ) -> Result<bool, ApiError> {
        if !id_ok(attempt) {
            return Err(bad());
        }
        if started.scope != self.scope_generation
            || started.boundary != self.boundary_generation
            || ScopeIdentity::new(connection, model)? != self.scope_identity
            // No usage is not evidence and leaves the first-known slot available.
            || input_tokens.is_none()
            || self.first.as_ref().is_some_and(|first| first.input_tokens.is_some())
        {
            return Ok(false);
        }
        self.first = Some(Report {
            attempt: attempt.into(),
            input_tokens,
        });
        Ok(true)
    }
}
fn key(thread: &str) -> Result<String, ApiError> {
    if !id_ok(thread) {
        return Err(bad());
    }
    Ok(format!("compact.a_state.{thread}"))
}
pub(crate) fn read(db: &rusqlite::Connection, thread: &str) -> Result<Option<State>, StorageError> {
    // length check occurs in SQLite, so oversized JSON is not copied into Rust.
    let row:Option<Option<String>>=db.query_row(
        "SELECT CASE WHEN length(CAST(value AS BLOB))<=?2 THEN value END FROM app_meta WHERE key=?1",
        params![key(thread)?,MAX_BYTES as i64],|r|r.get(0)).optional()?;
    match row {
        None => Ok(None),
        Some(None) => Err(bad().into()),
        Some(Some(text)) => {
            let state: State =
                serde_json::from_str(&text).map_err(|_| StorageError::from(bad()))?;
            state.validate()?;
            Ok(Some(state))
        }
    }
}
pub(crate) fn save(
    tx: &rusqlite::Connection,
    thread: &str,
    state: &State,
) -> Result<(), StorageError> {
    state.validate()?;
    let text = serde_json::to_string(state).map_err(|_| StorageError::from(bad()))?;
    if text.len() > MAX_BYTES {
        return Err(bad().into());
    }
    tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key(thread)?,text])?;
    Ok(())
}
pub(crate) fn reading(state: Option<&State>) -> Reading {
    state.map(State::reading).unwrap_or(Reading::LegacyAbsent)
}

pub(crate) fn scope_tx(
    db: &rusqlite::Connection,
    thread: &str,
    connection: &str,
    model: &str,
) -> Result<State, StorageError> {
    let mut state = read(db, thread)?.unwrap_or(State::new(connection, model)?);
    state.change_scope(connection, model)?;
    save(db, thread, &state)?;
    Ok(state)
}
pub(crate) fn begin_report_tx(
    db: &rusqlite::Connection,
    thread: &str,
    connection: &str,
    model: &str,
) -> Result<Token, StorageError> {
    Ok(scope_tx(db, thread, connection, model)?.token())
}
pub(crate) fn complete_report_tx(
    db: &rusqlite::Connection,
    thread: &str,
    connection: &str,
    model: &str,
    attempt: &str,
    started: Option<Token>,
    input: Option<u64>,
) -> Result<(), StorageError> {
    let Some(started) = started else {
        return Ok(());
    }; // Old inflight attempts have no causal token.
    let Some(mut state) = read(db, thread)? else {
        return Ok(());
    };
    if state.completed(started, connection, model, attempt, input)? {
        save(db, thread, &state)?;
    }
    Ok(())
}
pub(crate) fn reset_tx(
    db: &rusqlite::Connection,
    thread: &str,
    expected: Token,
    origin: &str,
) -> Result<(), StorageError> {
    if let Some(mut state) = read(db, thread)?
        && state.finish_boundary(expected, origin)?
    {
        save(db, thread, &state)?;
    }
    Ok(())
}
pub(crate) fn cleanup_tx(db: &rusqlite::Connection, thread: &str) -> Result<(), StorageError> {
    for key in [
        key(thread)?,
        format!("compact.deterministic_range_failure.closed.{thread}"),
        format!("compact.deterministic_range_failure.active.{thread}"),
    ] {
        db.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_model_scope_identity_supports_long_valid_names() {
        for model in [
            "m".repeat(256),
            "m".repeat(257),
            "m".repeat(8192),
            "\u{e9}".repeat(257),
        ] {
            let state = State::new("connection", &model)
                .expect("public model names must not inherit the GC fact identifier bound");
            assert!(serde_json::to_vec(&state).unwrap().len() <= MAX_BYTES);
        }
    }

    #[test]
    fn scope_identity_distinguishes_common_prefix_and_tuple_boundaries() {
        let prefix = "m".repeat(8192);
        let model_a = format!("{prefix}a");
        let model_b = format!("{prefix}b");
        let mut state = State::new("connection", &model_a).unwrap();
        let first = state.token();
        assert!(!state.change_scope("connection", &model_a).unwrap());
        assert!(state.change_scope("connection", &model_b).unwrap());
        assert_ne!(state.token(), first);
        assert!(state.change_scope("connection", &model_a).unwrap());
        assert_ne!(
            state.token(),
            first,
            "scope ABA must not revive old request authority"
        );

        let mut tuple = State::new("ab", "c").unwrap();
        assert!(tuple.change_scope("a", "bc").unwrap());
        assert!(!tuple.change_scope("a", "bc").unwrap());
    }

    #[test]
    fn first_completion_after_boundary_scope_aba_and_unknown_are_facts() {
        let mut s = State::new("A", "model").unwrap();
        let started = s.token();
        assert!(s.finish_boundary(started, "trial1").unwrap());
        assert!(
            !s.completed(started, "A", "model", "pre-boundary", None)
                .unwrap()
        );
        assert_eq!(s.reading(), Reading::AwaitingFirst);
        let after_boundary = s.token();
        assert!(
            !s.completed(after_boundary, "A", "model", "missing", None)
                .unwrap()
        );
        assert_eq!(s.reading(), Reading::AwaitingFirst);
        assert!(
            s.completed(after_boundary, "A", "model", "first-known", Some(123))
                .unwrap()
        );
        assert_eq!(s.reading(), Reading::Known(123));
        assert!(
            !s.completed(after_boundary, "A", "model", "later", Some(999))
                .unwrap()
        );
        assert!(s.change_scope("B", "model").unwrap());
        assert!(s.change_scope("A", "model").unwrap());
        assert!(
            !s.completed(started, "A", "model", "stale", Some(888))
                .unwrap()
        );
        let fresh = s.token();
        assert!(
            s.completed(fresh, "A", "model", "fresh", Some(123))
                .unwrap()
        );
        assert_eq!(s.reading(), Reading::Known(123));
    }
    #[test]
    fn missing_input_does_not_consume_first_known_report_slot() {
        let mut s = State::new("c", "m").unwrap();
        let token = s.token();
        assert!(!s.completed(token, "c", "m", "none-1", None).unwrap());
        assert!(!s.completed(token, "c", "m", "none-2", None).unwrap());
        assert_eq!(s.reading(), Reading::AwaitingFirst);
        assert!(s.completed(token, "c", "m", "known", Some(123)).unwrap());
        assert_eq!(s.reading(), Reading::Known(123));
        assert!(!s.completed(token, "c", "m", "later", Some(456)).unwrap());
        assert_eq!(s.reading(), Reading::Known(123));
    }
    #[test]
    fn known_zero_is_a_real_immutable_first_report() {
        let mut s = State::new("c", "m").unwrap();
        let token = s.token();
        assert!(s.completed(token, "c", "m", "zero", Some(0)).unwrap());
        assert_eq!(s.reading(), Reading::Known(0));
        assert!(!s.completed(token, "c", "m", "later", Some(123)).unwrap());
        assert_eq!(s.reading(), Reading::Known(0));
    }
    #[test]
    fn reset_idempotence_and_stale_origin_never_erase_new_first() {
        let mut s = State::new("c", "m").unwrap();
        let old = s.token();
        assert!(s.finish_boundary(old, "manual1").unwrap());
        let t = s.token();
        assert!(s.completed(t, "c", "m", "a", Some(10)).unwrap());
        assert!(!s.finish_boundary(old, "manual1").unwrap());
        assert!(!s.finish_boundary(t, "manual1").unwrap());
        assert_eq!(s.reading(), Reading::Known(10));
        assert!(s.finish_boundary(t, "trial2").unwrap());
        let now = s.token();
        assert!(s.completed(now, "c", "m", "b", Some(20)).unwrap());
        assert!(!s.finish_boundary(old, "manual1").unwrap());
        assert_eq!(s.reading(), Reading::Known(20));
    }
    #[test]
    fn report_token_round_trips_full_counter_domain_and_rejects_malformed_values() {
        let token = Token {
            scope: u64::MAX,
            boundary: u64::MAX,
        };
        let encoded = serde_json::to_string(&token).unwrap();
        assert_eq!(serde_json::from_str::<Token>(&encoded).unwrap(), token);
        for body in [
            r#"{"scope":-1,"boundary":0}"#,
            r#"{"scope":0,"boundary":"1"}"#,
            r#"{"scope":0,"boundary":0,"extra":1}"#,
        ] {
            assert!(serde_json::from_str::<Token>(body).is_err(), "{body}");
        }
    }
    #[test]
    fn transaction_rollback_bounded_read_and_legacy_absence() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);")
            .unwrap();
        assert_eq!(
            reading(read(&db, "t").unwrap().as_ref()),
            Reading::LegacyAbsent
        );
        {
            let tx = db.transaction().unwrap();
            save(&tx, "t", &State::new("c", "m").unwrap()).unwrap();
        }
        assert!(read(&db, "t").unwrap().is_none());
        {
            let tx = db.transaction().unwrap();
            save(&tx, "t", &State::new("c", "m").unwrap()).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            reading(read(&db, "t").unwrap().as_ref()),
            Reading::AwaitingFirst
        );
        db.execute("UPDATE app_meta SET value=?1", ["x".repeat(MAX_BYTES + 1)])
            .unwrap();
        assert!(read(&db, "t").is_err());
        db.execute("UPDATE app_meta SET value='{}'", []).unwrap();
        assert!(read(&db, "t").is_err());
        let mut invalid_identity = State::new("c", "m").unwrap();
        invalid_identity.scope_identity.0 = "X".repeat(64);
        assert!(save(&db, "t", &invalid_identity).is_err());
        let mut s = State::new("c", "m").unwrap();
        s.scope_generation = u64::MAX;
        let before = s.clone();
        assert!(s.change_scope("d", "m").is_err());
        assert_eq!(s, before);
        s.boundary_generation = u64::MAX;
        assert!(s.finish_boundary(s.token(), "x").is_err());
    }
    #[test]
    fn populated_old_boundary_is_cleared_and_duplicate_does_not_erase_new_first() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);")
            .unwrap();
        let start = {
            let tx = db.transaction().unwrap();
            let t = begin_report_tx(&tx, "t", "c", "m").unwrap();
            tx.commit().unwrap();
            t
        };
        {
            let tx = db.transaction().unwrap();
            complete_report_tx(&tx, "t", "c", "m", "report1", Some(start), Some(111)).unwrap();
            tx.commit().unwrap();
        }
        let state = read(&db, "t").unwrap().unwrap();
        assert_eq!(state.reading(), Reading::Known(111));
        {
            let tx = db.transaction().unwrap();
            assert!(reset_tx(&tx, "t", start, "trial2").is_ok());
            tx.commit().unwrap();
        }
        assert_eq!(
            read(&db, "t").unwrap().unwrap().first,
            None,
            "reset clears old input attribution"
        );
        {
            let tx = db.transaction().unwrap();
            complete_report_tx(&tx, "t", "c", "m", "stale", Some(start), Some(222)).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            read(&db, "t").unwrap().unwrap().reading(),
            Reading::AwaitingFirst,
            "request started before boundary cannot become #A"
        );
        let current = {
            let tx = db.transaction().unwrap();
            let t = begin_report_tx(&tx, "t", "c", "m").unwrap();
            tx.commit().unwrap();
            t
        };
        {
            let tx = db.transaction().unwrap();
            complete_report_tx(&tx, "t", "c", "m", "report2", Some(current), Some(333)).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            read(&db, "t").unwrap().unwrap().reading(),
            Reading::Known(333)
        );
        {
            let tx = db.transaction().unwrap();
            reset_tx(&tx, "t", start, "trial2").unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            read(&db, "t").unwrap().unwrap().reading(),
            Reading::Known(333),
            "duplicate terminal completion cannot erase a newer first report"
        );
    }
}

#[cfg(test)]
mod transactional_reset_tests {
    use super::*;
    #[test]
    fn provider_report_and_terminal_cas_rollback_share_atomic_state() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE terminal(id TEXT PRIMARY KEY,done INTEGER NOT NULL);INSERT INTO terminal VALUES('trial',0);").unwrap();
        let (initial, boundary) = {
            let tx = db.transaction().unwrap();
            let state = scope_tx(&tx, "t", "c", "m").unwrap();
            let token = state.token();
            save(&tx, "t", &state).unwrap();
            tx.commit().unwrap();
            (state, token)
        };
        {
            let tx = db.transaction().unwrap();
            assert_eq!(
                tx.execute("UPDATE terminal SET done=1 WHERE id='trial' AND done=0", [])
                    .unwrap(),
                1
            );
            reset_tx(&tx, "t", boundary, "trial").unwrap();
            // Explicit transaction drop rolls back terminal ownership AND #A.
        }
        assert_eq!(read(&db, "t").unwrap().unwrap(), initial);
        let tx = db.transaction().unwrap();
        assert_eq!(
            tx.execute("UPDATE terminal SET done=1 WHERE id='trial' AND done=0", [])
                .unwrap(),
            1
        );
        reset_tx(&tx, "t", boundary, "trial").unwrap();
        let post_boundary = read(&tx, "t").unwrap().unwrap().token();
        complete_report_tx(&tx, "t", "c", "m", "first", Some(post_boundary), None).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            read(&db, "t").unwrap().unwrap().reading(),
            Reading::AwaitingFirst
        );
        let tx = db.transaction().unwrap();
        assert_eq!(
            tx.execute("UPDATE terminal SET done=1 WHERE id='trial' AND done=0", [])
                .unwrap(),
            0
        );
        assert_eq!(
            read(&tx, "t").unwrap().unwrap().reading(),
            Reading::AwaitingFirst
        );
        tx.commit().unwrap();
        let current = {
            let tx = db.transaction().unwrap();
            let token = begin_report_tx(&tx, "t", "c", "m").unwrap();
            tx.commit().unwrap();
            token
        };
        let tx = db.transaction().unwrap();
        complete_report_tx(&tx, "t", "c", "m", "later-request", Some(current), Some(9)).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            read(&db, "t").unwrap().unwrap().reading(),
            Reading::Known(9)
        );
    }
    #[test]
    fn pre_boundary_request_completion_cannot_become_post_boundary_a() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);")
            .unwrap();
        let old_start = {
            let tx = db.transaction().unwrap();
            let token = begin_report_tx(&tx, "t", "c", "m").unwrap();
            tx.commit().unwrap();
            token
        };
        let committed_boundary = {
            let tx = db.transaction().unwrap();
            let state = read(&tx, "t").unwrap().unwrap();
            assert!(reset_tx(&tx, "t", state.token(), "trial").is_ok());
            let after = read(&tx, "t").unwrap().unwrap().token();
            tx.commit().unwrap();
            after
        };
        assert_ne!(old_start.boundary, committed_boundary.boundary);
        {
            let tx = db.transaction().unwrap();
            complete_report_tx(
                &tx,
                "t",
                "c",
                "m",
                "pre-boundary",
                Some(old_start),
                Some(111),
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            read(&db, "t").unwrap().unwrap().reading(),
            Reading::AwaitingFirst
        );
        let new_start = {
            let tx = db.transaction().unwrap();
            let token = begin_report_tx(&tx, "t", "c", "m").unwrap();
            tx.commit().unwrap();
            token
        };
        assert_eq!(new_start, committed_boundary);
        {
            let tx = db.transaction().unwrap();
            complete_report_tx(
                &tx,
                "t",
                "c",
                "m",
                "post-boundary",
                Some(new_start),
                Some(222),
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let state = read(&db, "t").unwrap().unwrap();
        assert_eq!(
            state.first,
            Some(Report {
                attempt: "post-boundary".into(),
                input_tokens: Some(222)
            })
        );
        assert_eq!(state.reading(), Reading::Known(222));
    }
}

#[cfg(test)]
mod scope_identity_tests {
    use super::*;
    #[test]
    fn scope_identity_encoding_matches_domain_framed_vectors() {
        for (connection, model, expected) in [
            (
                "connection",
                "m".repeat(257),
                "adda2836860392c146dfa8e806ce52178ee504eb3bc38a29fb734ed1110a831b",
            ),
            (
                "ab",
                "c".into(),
                "7abb0019aca29653202236174a4eda539f3e05e23844cd966829410089a1659f",
            ),
            (
                "a",
                "bc".into(),
                "702f4e6af84033454c32e35a549492851fe5631aba646e246ce468948b4c4a80",
            ),
            (
                "connection",
                "\u{e9}".repeat(257),
                "d8109cd9aed37a27cb7898cf89039f1b58bde4631dfdddcf2bba14e073b4ff98",
            ),
        ] {
            assert_eq!(ScopeIdentity::new(connection, &model).unwrap().0, expected);
        }
        assert_ne!(
            ScopeIdentity::new("c", "\u{e9}").unwrap(),
            ScopeIdentity::new("c", "e\u{301}").unwrap()
        );
    }
    #[test]
    fn runtime_rejects_v1_instead_of_adding_a_compatibility_path() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        db.execute("INSERT INTO app_meta VALUES('compact.a_state.t',?1)",[r#"{"version":1,"scope_generation":0,"boundary_generation":0,"connection":"c","model":"m","last_reset_origin":null,"first":null}"#]).unwrap();
        assert!(read(&db, "t").is_err());
    }
}
