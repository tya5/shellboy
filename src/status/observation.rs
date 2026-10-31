//! Bounded saved Status facts; caller owns transaction, admission and adoption.
use crate::protocol::{ApiError, ErrorCode};
use crate::storage::StorageError;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) const MAX_IDS: usize = 200;
pub(crate) const PREFIX: usize = 201;
pub(crate) const TEXT_BYTES: usize = 256 * 1024;
pub(crate) const POINT_STATEMENTS: usize = 5;
pub(crate) const MEMBERSHIP_STATEMENTS: usize = 12;
pub(crate) const MEMBERSHIP_CANDIDATES: usize = PREFIX * MEMBERSHIP_STATEMENTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Run,
    Turn,
    Shell,
    Message,
    Attempt,
}
impl Kind {
    pub(crate) const ALL: [Self; 5] = [
        Self::Run,
        Self::Turn,
        Self::Shell,
        Self::Message,
        Self::Attempt,
    ];
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Turn => "turn",
            Self::Shell => "shell",
            Self::Message => "message",
            Self::Attempt => "attempt",
        }
    }
    pub(crate) fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == s)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Key {
    pub kind: Kind,
    pub id: String,
}

#[derive(Default)]
pub(crate) struct TextBudget {
    pub used: usize,
}
impl TextBudget {
    pub(crate) fn take(&mut self, bytes: usize) -> bool {
        match self.used.checked_add(bytes) {
            Some(n) if n <= TEXT_BYTES => {
                self.used = n;
                true
            }
            _ => false,
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Limited {
    pub id: String,
    pub reason: &'static str,
}
#[derive(Debug)]
pub(crate) struct RawFact {
    pub key: Key,
    pub fields: Vec<Option<String>>,
}
#[derive(Default, Debug)]
pub(crate) struct Raw {
    pub facts: Vec<RawFact>,
    pub missing: Vec<Key>,
    pub limited: Vec<Limited>,
    pub statements: usize,
}
pub(crate) fn validate_ids(ids: &[String]) -> Result<(), ApiError> {
    if ids.len() > MAX_IDS
        || ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control))
    {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "List scope exceeds bounded ID input",
        ));
    }
    Ok(())
}
fn invalid(error: ApiError) -> StorageError {
    error.into()
}

/// Projection columns are native identities/state/times, never output or payload.
pub(crate) fn point_sql(kind: Kind) -> String {
    let (table, id, fields, joins) = match kind {
        Kind::Run => (
            "run r",
            "r.id",
            "r.state,r.thread_id,r.id,NULL,r.created_at,NULL,r.ended_at,NULL,r.stop_reason",
            "",
        ),
        Kind::Turn => (
            "turn r",
            "r.id",
            "r.state,r.thread_id,r.run_id,r.id,r.created_at,NULL,r.ended_at,NULL,CASE WHEN json_valid(r.recovery_reason_json) THEN substr(COALESCE(json_extract(r.recovery_reason_json,'$.kind'),json_extract(r.recovery_reason_json,'$.code')),1,512) END",
            "",
        ),
        Kind::Shell => (
            "shell_execution r",
            "r.id",
            "r.state,t.thread_id,r.owner_run_id,r.turn_id,NULL,r.started_at,r.ended_at,NULL,substr(r.end_reason,1,512)",
            "JOIN run t ON t.id=r.owner_run_id",
        ),
        Kind::Message => (
            "receipt r INDEXED BY receipt_message_node",
            "r.message_node_id",
            "r.state,r.target_thread_id,r.run_id,r.turn_id,r.created_at,NULL,NULL,NULLIF(r.updated_at,''),substr(r.hold_reason,1,512)",
            "",
        ),
        Kind::Attempt => (
            "model_attempt r",
            "r.id",
            "r.state,t.thread_id,t.run_id,r.turn_id,NULL,r.started_at,r.ended_at,NULL,CASE WHEN json_valid(r.error_json) THEN substr(COALESCE(json_extract(r.error_json,'$.kind'),json_extract(r.error_json,'$.code')),1,512) END",
            "JOIN turn t ON t.id=r.turn_id",
        ),
    };
    format!(
        "SELECT {id},{fields} FROM json_each(?1) j CROSS JOIN {table} {joins} WHERE {id}=j.value LIMIT 201"
    )
}

/// Five fixed point batches. Missing is distinct from budget/invalid uncertainty.
pub(crate) fn read_status_tx(
    db: &Connection,
    keys: &[Key],
    budget: &mut TextBudget,
) -> Result<Raw, StorageError> {
    if keys.len() > MAX_IDS
        || keys.iter().any(|key| {
            key.id.is_empty() || key.id.len() > 1024 || key.id.chars().any(char::is_control)
        })
    {
        return Err(invalid(ApiError::new(
            ErrorCode::InputLimit,
            "Status retained scope exceeds bounded IDs",
        )));
    }
    let mut result = Raw::default();
    // Reserve identity/unknown witnesses before cloning bounded IDs.
    if !budget.take(keys.iter().map(|key| key.id.len() * 2).sum()) {
        result.limited.push(Limited {
            id: String::new(),
            reason: "metadata_identity_channel_limit",
        });
        return Ok(result);
    }
    for kind in Kind::ALL {
        let ids = keys
            .iter()
            .filter(|k| k.kind == kind)
            .map(|k| k.id.clone())
            .collect::<BTreeSet<_>>();
        if ids.is_empty() {
            continue;
        }
        let encoded = serde_json::to_string(&ids).expect("bounded IDs");
        let mut statement = db.prepare(&point_sql(kind))?;
        let mut rows = statement.query([encoded])?;
        result.statements += 1;
        let mut found = BTreeSet::new();
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            let id = row
                .get_ref(0)?
                .as_str()
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            if !found.insert(id.to_owned()) {
                result.facts.retain(|fact| {
                    fact.key
                        != Key {
                            kind,
                            id: id.into(),
                        }
                });
                if !result.limited.iter().any(|w| w.id == id) {
                    result.limited.push(Limited {
                        id: id.into(),
                        reason: "ambiguous_native_identity",
                    });
                }
                continue;
            }
            let borrowed = (1..10)
                .map(|column| {
                    row.get_ref(column)
                        .map(|v| v.as_str_or_null().ok().flatten())
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if borrowed.iter().flatten().any(|s| s.len() > 1024)
                || !budget
                    .take(id.len() + borrowed.iter().flatten().map(|s| s.len()).sum::<usize>())
            {
                result.limited.push(Limited {
                    id: id.into(),
                    reason: "metadata_text_or_scalar_limit",
                });
                continue;
            }
            result.facts.push(RawFact {
                key: Key {
                    kind,
                    id: id.into(),
                },
                fields: borrowed.into_iter().map(|s| s.map(str::to_owned)).collect(),
            });
        }
        if count == PREFIX {
            result.limited.push(Limited {
                id: String::new(),
                reason: "native_point_prefix_limit",
            });
        }
        for id in ids.difference(&found) {
            if count == PREFIX {
                result.limited.push(Limited {
                    id: id.clone(),
                    reason: "native_point_prefix_unconfirmed",
                });
            } else {
                result.missing.push(Key {
                    kind,
                    id: id.clone(),
                });
            }
        }
    }
    assert!(result.statements <= POINT_STATEMENTS);
    Ok(result)
}

#[derive(Default, Debug)]
pub(crate) struct Membership {
    pub candidates: Vec<Key>,
    pub limited: bool,
    pub unconfirmed: bool,
    pub statements: usize,
    pub prefix_rows: usize,
}
/// Each correlated native prefix has a fixed limit, independent of foreign rows.
pub(crate) fn prefix_sql(kind: Kind, per_thread: usize) -> String {
    assert!((1..=PREFIX).contains(&per_thread));
    let native = match kind {
        Kind::Run => {
            "SELECT id FROM run INDEXED BY wait_run_state WHERE thread_id=t.value AND state=?2 ORDER BY rowid DESC"
        }
        Kind::Turn => {
            "SELECT id FROM turn INDEXED BY turns_by_thread WHERE thread_id=t.value ORDER BY created_at DESC"
        }
        Kind::Shell => {
            "SELECT id FROM shell_execution INDEXED BY shell_active_owner WHERE owner_run_id=(SELECT id FROM run INDEXED BY one_live_run WHERE thread_id=t.value AND state IN ('live','stopping')) AND state IN ('starting','running','stopping')"
        }
        Kind::Message => {
            "SELECT message_node_id id FROM receipt INDEXED BY receipt_target WHERE target_thread_id=t.value AND state=?2 ORDER BY created_at DESC"
        }
        Kind::Attempt => {
            "SELECT id FROM model_attempt INDEXED BY attempts_by_turn WHERE turn_id=(SELECT id FROM turn INDEXED BY one_open_turn WHERE thread_id=t.value AND state IN ('preparing','running','waiting','recovery_wait')) ORDER BY started_at DESC"
        }
    };
    format!(
        "SELECT t.value,p.value FROM json_each(?1) t CROSS JOIN json_each((SELECT json_group_array(CASE WHEN octet_length(id)<=1024 THEN id END) FROM ({native} LIMIT {per_thread}))) p"
    )
}
pub(crate) fn read_membership_tx(
    db: &Connection,
    threads: &[String],
    budget: &mut TextBudget,
) -> Result<Membership, StorageError> {
    validate_ids(threads).map_err(invalid)?;
    let threads = threads.iter().collect::<BTreeSet<_>>();
    let mut result = Membership {
        unconfirmed: true,
        ..Default::default()
    };
    if threads.is_empty() {
        return Ok(result);
    }
    let per_thread = PREFIX / threads.len();
    let encoded = serde_json::to_string(&threads).expect("bounded Threads");
    let mut seen = BTreeSet::new();
    for kind in Kind::ALL {
        let states: &[&str] = match kind {
            Kind::Run => &["live", "stopping", "completed", "stopped", "failed"],
            Kind::Message => &["accepted", "held", "applied", "cancelled"],
            _ => &[""],
        };
        for state in states {
            let sql = prefix_sql(kind, per_thread);
            let mut statement = db.prepare(&sql)?;
            let mut rows = if sql.contains("?2") {
                statement.query(params![encoded, state])?
            } else {
                statement.query([&encoded])?
            };
            result.statements += 1;
            while let Some(row) = rows.next()? {
                result.prefix_rows += 1;
                let Some(id) = row
                    .get_ref(1)?
                    .as_str_or_null()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?
                else {
                    result.limited = true;
                    continue;
                };
                if id.len() > 1024 || result.candidates.len() >= MAX_IDS || !budget.take(id.len()) {
                    result.limited = true;
                    continue;
                }
                let key = Key {
                    kind,
                    id: id.into(),
                };
                if seen.insert(key.clone()) {
                    result.candidates.push(key);
                }
            }
        }
    }
    assert!(result.prefix_rows <= MEMBERSHIP_CANDIDATES);
    Ok(result)
}
/// Pure metadata patch. Caller retains filters/page/cursor and adoption fences.
pub(crate) fn prepare(raw: Raw) -> Value {
    let rows = raw
        .facts
        .into_iter()
        .map(|fact| {
            let names = [
                "state",
                "thread_id",
                "run_id",
                "turn_id",
                "created_at",
                "started_at",
                "ended_at",
                "updated_at",
                "reason",
            ];
            let mut fields = serde_json::Map::new();
            for (name, value) in names.into_iter().zip(fact.fields) {
                fields.insert(name.into(), value.map_or(Value::Null, Value::String));
            }
            serde_json::json!({"kind":fact.key.kind.name(),"id":fact.key.id,"current":fields})
        })
        .collect::<Vec<_>>();
    serde_json::json!({"items":rows,"missing_ids":raw.missing.into_iter().map(|k|serde_json::json!({"kind":k.kind.name(),"id":k.id})).collect::<Vec<_>>(),"limited_fields":raw.limited.into_iter().map(|w|serde_json::json!({"id":w.id,"reason":w.reason})).collect::<Vec<_>>(),"sql_statements":raw.statements})
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE run(id TEXT PRIMARY KEY,thread_id TEXT,state TEXT,created_at TEXT,ended_at TEXT,stop_reason TEXT); CREATE INDEX wait_run_state ON run(thread_id,state); CREATE UNIQUE INDEX one_live_run ON run(thread_id) WHERE state IN ('live','stopping'); CREATE TABLE turn(id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,state TEXT,created_at TEXT,ended_at TEXT,recovery_reason_json TEXT); CREATE INDEX turns_by_thread ON turn(thread_id,created_at); CREATE UNIQUE INDEX one_open_turn ON turn(thread_id) WHERE state IN ('preparing','running','waiting','recovery_wait'); CREATE TABLE receipt(message_node_id TEXT,target_thread_id TEXT,run_id TEXT,turn_id TEXT,state TEXT,created_at TEXT,updated_at TEXT,hold_reason TEXT); CREATE INDEX receipt_message_node ON receipt(message_node_id); CREATE INDEX receipt_target ON receipt(target_thread_id,state,created_at); CREATE TABLE shell_execution(id TEXT PRIMARY KEY,owner_run_id TEXT,turn_id TEXT,state TEXT,started_at TEXT,ended_at TEXT,end_reason TEXT); CREATE INDEX shell_active_owner ON shell_execution(owner_run_id,state) WHERE state IN ('starting','running','stopping'); CREATE TABLE model_attempt(id TEXT PRIMARY KEY,turn_id TEXT,state TEXT,started_at TEXT,ended_at TEXT,error_json TEXT); CREATE INDEX attempts_by_turn ON model_attempt(turn_id,started_at);").unwrap();
        db
    }
    #[test]
    fn status_plans_and_vm_steps_do_not_grow_with_foreign_rows() {
        let db = database();
        db.execute(
            "INSERT INTO run VALUES('r','scope','live','2026-01-01',NULL,NULL)",
            [],
        )
        .unwrap();
        let query = prefix_sql(Kind::Run, 201);
        let measure = || {
            let plan = db
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap()
                .query_map(params!["[\"scope\"]", "live"], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                plan.iter()
                    .any(|s| s.contains("SEARCH run USING INDEX wait_run_state"))
            );
            let mut st = db.prepare(&query).unwrap();
            {
                let mut rows = st.query(params!["[\"scope\"]", "live"]).unwrap();
                while rows.next().unwrap().is_some() {}
            }
            assert_eq!(st.get_status(StatementStatus::FullscanStep), 0);
            st.get_status(StatementStatus::VmStep)
        };
        let before = measure();
        for n in 0..10000 {
            db.execute(
                "INSERT INTO run VALUES(?1,'foreign','completed','2026-01-01',NULL,NULL)",
                [format!("foreign{n}")],
            )
            .unwrap();
        }
        assert_eq!(measure(), before);
        for kind in Kind::ALL {
            let plan = db
                .prepare(&format!("EXPLAIN QUERY PLAN {}", point_sql(kind)))
                .unwrap()
                .query_map(["[\"missing\"]"], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(!plan.iter().any(|s| {
                ["SCAN r", "SCAN t"]
                    .iter()
                    .any(|p| s == p || s.starts_with(&format!("{p} USING")))
            }));
        }
    }
    #[test]
    fn missing_and_metadata_limits_are_distinct_from_an_empty_success() {
        let db = database();
        db.execute(
            "INSERT INTO run VALUES('r','scope','live','2026-01-01',NULL,NULL)",
            [],
        )
        .unwrap();
        let keys = vec![
            Key {
                kind: Kind::Run,
                id: "r".into(),
            },
            Key {
                kind: Kind::Run,
                id: "gone".into(),
            },
        ];
        let raw = read_status_tx(&db, &keys, &mut TextBudget { used: TEXT_BYTES }).unwrap();
        assert!(raw.facts.is_empty());
        assert!(raw.missing.is_empty());
        assert_eq!(raw.limited.len(), 1);
        assert_eq!(raw.statements, 0);
        db.execute("UPDATE run SET thread_id=?1", ["x".repeat(1025)])
            .unwrap();
        let raw = read_status_tx(&db, &keys, &mut TextBudget::default()).unwrap();
        assert!(raw.facts.is_empty());
        assert_eq!(raw.missing.len(), 1);
        assert_eq!(raw.limited.len(), 1);
        assert_eq!(raw.statements, 1);
        let membership = read_membership_tx(&db, &[], &mut TextBudget::default()).unwrap();
        assert!(membership.unconfirmed);
        assert_eq!(membership.statements, 0);
    }
}
