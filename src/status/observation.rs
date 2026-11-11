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
    fn canonical(self) -> super::WorkKind {
        match self {
            Self::Run => super::WorkKind::Run,
            Self::Turn => super::WorkKind::Turn,
            Self::Shell => super::WorkKind::Shell,
            Self::Message => super::WorkKind::Message,
            Self::Attempt => super::WorkKind::Attempt,
        }
    }
    pub(crate) fn name(self) -> &'static str {
        self.canonical().as_str()
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

/// Native PK batches only; all correlated metadata uses indexed owner/key ranges.
pub(crate) fn point_sql(kind: Kind) -> String {
    let (table, id, fields, joins, thread, owner, answer, operation, connection, model) = match kind
    {
        Kind::Run => (
            "run r",
            "r.id",
            "r.state,r.thread_id,r.id,t.id,r.created_at,NULL,r.ended_at,NULL,substr(r.stop_reason,1,512)",
            "LEFT JOIN turn t ON t.id=(SELECT id FROM (SELECT id,created_at FROM turn INDEXED BY turns_by_run WHERE run_id=r.id AND created_at=(SELECT max(created_at) FROM turn INDEXED BY turns_by_run WHERE run_id=r.id) LIMIT 201) ORDER BY id DESC LIMIT 1)",
            "r.thread_id",
            "NULL",
            "t.answer_node_id",
            "NULL",
            "NULL",
            "NULL",
        ),
        Kind::Turn => (
            "turn r",
            "r.id",
            "r.state,r.thread_id,r.run_id,r.id,r.created_at,NULL,r.ended_at,NULL,CASE WHEN json_valid(r.recovery_reason_json) THEN substr(COALESCE(json_extract(r.recovery_reason_json,'$.kind'),json_extract(r.recovery_reason_json,'$.code')),1,512) END",
            "JOIN run t ON t.id=r.run_id",
            "r.thread_id",
            "NULL",
            "r.answer_node_id",
            "NULL",
            "NULL",
            "NULL",
        ),
        Kind::Shell => (
            "shell_execution r",
            "r.id",
            "r.state,t.thread_id,r.owner_run_id,r.turn_id,NULL,r.started_at,r.ended_at,NULL,substr(r.end_reason,1,512)",
            "JOIN run t ON t.id=r.owner_run_id",
            "t.thread_id",
            "r.owner_run_id",
            "NULL",
            "NULL",
            "NULL",
            "NULL",
        ),
        Kind::Message => (
            "receipt r INDEXED BY receipt_message_node",
            "r.message_node_id",
            "r.state,r.target_thread_id,r.run_id,r.turn_id,r.created_at,NULL,NULL,NULLIF(r.updated_at,''),substr(r.hold_reason,1,512)",
            "LEFT JOIN run t ON t.id=r.run_id LEFT JOIN turn u ON u.id=r.turn_id",
            "r.target_thread_id",
            "NULL",
            "NULL",
            "CASE WHEN EXISTS(SELECT 1 FROM operation_receipt o WHERE o.operation_id=r.client_operation_id) THEN r.client_operation_id END",
            "NULL",
            "NULL",
        ),
        Kind::Attempt => (
            "model_attempt r",
            "r.id",
            "r.state,t.thread_id,t.run_id,r.turn_id,NULL,r.started_at,r.ended_at,NULL,CASE WHEN json_valid(r.error_json) THEN substr(COALESCE(json_extract(r.error_json,'$.kind'),json_extract(r.error_json,'$.code')),1,512) END",
            "JOIN turn t ON t.id=r.turn_id JOIN run u ON u.id=t.run_id",
            "t.thread_id",
            "NULL",
            "t.answer_node_id",
            "NULL",
            "substr(r.profile_id,1,128)",
            "substr(r.model_id,1,256)",
        ),
    };
    // No reservation text/output payload crosses the raw boundary unchecked.
    // A saturated latest-Turn prefix cannot claim canonical lexical tie ordering.
    let (revisit, guard) = if kind == Kind::Run {
        let (pending, delivered) = crate::control::revisit::summary_sql("r.thread_id", "r.id");
        (
            format!("{pending}||':'||{delivered}"),
            "CASE WHEN EXISTS(SELECT 1 FROM turn INDEXED BY turns_by_run WHERE run_id=r.id AND created_at=(SELECT max(created_at) FROM turn INDEXED BY turns_by_run WHERE run_id=r.id) LIMIT 1 OFFSET 200) THEN 'latest_turn_prefix_limit' END",
        )
    } else if kind == Kind::Message {
        ("json_object('expect_reply',json_extract(r.result_json,'$.expect_reply'),'reply',json_extract(r.result_json,'$.reply'),'reply_to',json_extract(r.result_json,'$.reply_to'))".into(), "NULL")
    } else {
        ("NULL".into(), "NULL")
    };
    let (elapsed, output) = if kind == Kind::Shell {
        (
            "CAST(CASE WHEN json_valid(r.shell_json) AND json_type(r.shell_json,'$.completion.elapsed_ms')='integer' AND json_extract(r.shell_json,'$.completion.elapsed_ms') BETWEEN 0 AND 9223372036854775807 THEN json_extract(r.shell_json,'$.completion.elapsed_ms') END AS TEXT)",
            "(SELECT received_at FROM output_chunk WHERE execution_id=r.id ORDER BY sequence DESC LIMIT 1)",
        )
    } else {
        ("NULL", "NULL")
    };
    let stdin = if kind == Kind::Shell {
        "CASE WHEN r.state IN ('exited','failed','stopped') THEN 'false' WHEN json_type(r.shell_json,'$.stdin_open')='true' THEN 'true' WHEN json_type(r.shell_json,'$.stdin_open')='false' THEN 'false' END,CASE WHEN json_type(r.shell_json,'$.stdin_last_write_at')='text' THEN json_extract(r.shell_json,'$.stdin_last_write_at') END"
    } else {
        "NULL,NULL"
    };
    let discard = if kind == Kind::Attempt {
        crate::compaction::discard::status_sql("r")
    } else {
        "NULL".into()
    };
    format!(
        "SELECT {id},{fields},{owner},{operation},{answer},{connection},{model},substr(q.title,1,80),q.parent_thread_id,substr(q.profile_id,1,128),substr(q.model_id,1,256),{revisit},{elapsed},{output},{stdin},{discard},{guard} FROM json_each(?1) j CROSS JOIN {table} {joins} LEFT JOIN thread q ON q.id={thread} WHERE {id}=j.value LIMIT 201"
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
            if row.get_ref(25)?.as_str_or_null().ok().flatten().is_some() {
                result.limited.push(Limited {
                    id: id.into(),
                    reason: "canonical_metadata_unconfirmed",
                });
                continue;
            }
            let borrowed = (1..25)
                .map(|column| {
                    row.get_ref(column)
                        .map(|v| v.as_str_or_null().ok().flatten())
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if borrowed.iter().enumerate().any(|(column, value)| {
                value.is_some_and(|s| {
                    s.len()
                        > if column == 18 {
                            16384
                        } else if column == 8 {
                            2048
                        } else {
                            1024
                        }
                })
            }) || !budget
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
/// Canonical native-state exclusions, without re-evaluating/moving a page cursor.
/// None is explicit uncertainty, not a filter mismatch or deleted row.
pub(crate) fn matches_filters(
    kind: Kind,
    fields: &Value,
    args: &crate::protocol::StatusArgs,
) -> Option<bool> {
    let state = fields.get("state")?.as_str()?;
    if !args.state.is_empty() {
        if !args.state.iter().any(|wanted| wanted == state) {
            return Some(false);
        }
    } else if !args.all && kind.canonical().terminal(state) {
        return Some(false);
    }
    for (filter, name) in [(&args.thread_id, "thread_id"), (&args.run_id, "run_id")] {
        if let Some(expected) = filter
            && fields.get(name)?.as_str()? != expected
        {
            return Some(false);
        }
    }
    if let Some(since) = args.since.as_deref() {
        let since = super::stamp(since).ok()?;
        let mut activity = None;
        for name in ["created_at", "started_at", "ended_at", "updated_at"] {
            if let Some(text) = fields.get(name).and_then(Value::as_str) {
                let at = super::stamp(text).ok()?;
                activity = Some(activity.map_or(at, |previous| std::cmp::max(previous, at)));
            }
        }
        if activity? < since {
            return Some(false);
        }
    }
    Some(true)
}
fn canonical_item(
    fact: &RawFact,
    observed: chrono::DateTime<chrono::Utc>,
) -> Result<super::WorkItem, ApiError> {
    let get = |column: usize| fact.fields.get(column).cloned().flatten();
    let required = |column| {
        get(column).ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Canonical Status metadata unavailable",
            )
        })
    };
    let kind = fact.key.kind.canonical();
    let id = &fact.key.id;
    let state = required(0)?;
    let created_at = get(4);
    let started_at = get(5);
    let ended_at = get(6);
    let updated_at = get(7);
    let activity = [&created_at, &started_at, &ended_at, &updated_at]
        .into_iter()
        .filter_map(|v| v.as_deref())
        .map(|v| super::stamp(v).map(|at| (at, v)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by_key(|(at, _)| *at)
        .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "Work has no recorded timestamp"))?;
    let elapsed_ms = if fact.key.kind == Kind::Shell {
        if matches!(state.as_str(), "starting" | "running" | "stopping") {
            started_at
                .as_deref()
                .and_then(|s| super::stamp(s).ok())
                .map(|at| observed.signed_duration_since(at).num_milliseconds().max(0) as u64)
        } else {
            get(19).and_then(|s| s.parse().ok())
        }
    } else {
        None
    };
    let mut item = super::WorkItem {
        coverage_v1: None,
        kind: kind.as_str().into(),
        id: id.clone(),
        state,
        thread_id: required(1)?,
        thread_title: required(14)?,
        parent_thread_id: get(15),
        current_connection: required(16)?,
        current_model: required(17)?,
        run_id: get(2),
        owner_run_id: get(9),
        turn_id: get(3),
        execution_id: (fact.key.kind == Kind::Shell).then(|| id.clone()),
        message_id: (fact.key.kind == Kind::Message).then(|| id.clone()),
        attempt_id: (fact.key.kind == Kind::Attempt).then(|| id.clone()),
        operation_id: get(10),
        end_message_node_id: get(11),
        response_nodes: Vec::new(),
        response_nodes_truncated: false,
        expect_reply: false,
        reply: None,
        reply_to: None,
        activity_at: activity.1.into(),
        elapsed_ms,
        last_output_at: get(20),
        stdin_open: get(21).and_then(|v| match v.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }),
        stdin_last_write_at: get(22),
        created_at,
        started_at,
        ended_at,
        updated_at,
        attempt_connection: get(12),
        attempt_model: get(13),
        reason: get(8),
        compaction_discard: get(23)
            .as_deref()
            .and_then(crate::compaction::discard::decode),
        revisit: get(18)
            .as_deref()
            .and_then(|facts| facts.split_once(':'))
            .and_then(|(pending, delivered)| {
                crate::control::revisit::summary(pending == "1", delivered.parse().ok()?)
            }),
        targets: Vec::new(),
        additional_properties: Default::default(),
    };

    if kind == super::WorkKind::Message
        && let Some(metadata) = get(18)
    {
        let metadata: serde_json::Value = serde_json::from_str(&metadata)
            .map_err(|_| ApiError::new(ErrorCode::InvalidState, "invalid reply metadata"))?;
        item.expect_reply = metadata["expect_reply"] == true || metadata["expect_reply"] == 1;
        item.reply = metadata
            .get("reply")
            .filter(|value| !value.is_null())
            .and_then(crate::protocol::StatusReply::from_value)
            .map(Some);
        item.reply_to = metadata["reply_to"].as_str().map(str::to_owned);
    }
    item.targets = super::links(kind, &item)?
        .into_iter()
        .filter_map(|command| {
            let value = serde_json::to_value(command).ok()?;
            Some(crate::protocol::StatusTarget {
                command: value.get("command")?.as_str()?.to_owned(),
                args: value.get("args")?.clone(),
                additional_properties: Default::default(),
            })
        })
        .collect();
    Ok(item)
}

/// Pure metadata patch. Caller retains filters/page/cursor and adoption fences.
pub(crate) fn prepare(raw: Raw) -> Value {
    let observed = chrono::Utc::now();
    let mut limited = raw.limited;
    let mut rows = Vec::new();
    for fact in raw.facts {
        match canonical_item(&fact, observed) {
            Ok(current) => rows.push(
                serde_json::json!({"kind":fact.key.kind.name(),"id":fact.key.id,"current":current}),
            ),
            Err(_) => limited.push(Limited {
                id: fact.key.id,
                reason: "canonical_metadata_unavailable",
            }),
        }
    }
    serde_json::json!({"items":rows,"missing_ids":raw.missing.into_iter().map(|k|serde_json::json!({"kind":k.kind.name(),"id":k.id})).collect::<Vec<_>>(),"limited_fields":limited.into_iter().map(|w|serde_json::json!({"id":w.id,"reason":w.reason})).collect::<Vec<_>>(),"sql_statements":raw.statements})
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        db
    }
    #[test]
    fn status_plans_and_vm_steps_do_not_grow_with_foreign_rows() {
        let db = database();
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES('r','scope','live','2026-01-01',NULL,NULL)",
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
        // Keep index range neighbours present on both sides of the comparison.
        // A singleton/end-of-index prefix omits one final IdxLT range check.
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES('left-edge','foreign','completed','2026-01-01',NULL,NULL)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES('right-edge','zulu','completed','2026-01-01',NULL,NULL)",
            [],
        )
        .unwrap();
        let before = measure();
        for n in 0..10000 {
            db.execute(
                "INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES(?1,'foreign','completed','2026-01-01',NULL,NULL)",
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
            "INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES('r','scope','live','2026-01-01',NULL,NULL)",
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
    fn canonical_fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch(r#"
          INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES
            ('parent','Parent','/','','p','m','2026-11-01T00:00:00Z'),
            ('scope','Current title','/','','current-profile','current-model','2026-11-01T00:00:00Z');
          UPDATE thread SET parent_thread_id='parent' WHERE id='scope';
          INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES('owner','parent','completed','2026-11-01T00:00:00Z','2026-11-01T00:00:01Z',NULL),('r','scope','failed','2026-11-01T00:00:00Z','2026-11-01T00:00:02Z','saved-stop');
          INSERT INTO node(id,kind,created_at,payload_json) VALUES('answer','assistant','2026-11-01T00:00:01Z','{}'),('message','user','2026-11-01T00:00:00Z','{}');
          INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at,answer_node_id,recovery_reason_json) VALUES
            ('t','scope','r','failed','2026-11-01T00:00:00Z','2026-11-01T00:00:02Z','answer','{"kind":"saved-recovery"}');
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,end_reason) VALUES
            ('s','r','t','shell-op','exited','/','{"completion":{"elapsed_ms":42}}','UTF-8','UTF-8','closed','2026-11-01T00:00:00Z','2026-11-01T00:00:02Z','saved-shell');
          INSERT INTO output_chunk VALUES('s',1,'stdout',0,X'61','2026-11-01T00:00:01Z');
          INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at) VALUES('message-op','fixture','{}','h','completed','2026-11-01T00:00:00Z');
          INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,hold_reason,result_json) VALUES
            ('receipt','message-op','{}','scope','message','r','t','applied','2026-11-01T00:00:00Z','2026-11-01T00:00:02Z','saved-hold','{"deliver":"complete","delivery_context":{"reason":"run_completion"}}');
          INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at,ended_at,error_json) VALUES
            ('a','t',0,0,'attempt-profile','attempt-model',0,'failed','{}','2026-11-01T00:00:00Z','2026-11-01T00:00:02Z','{"code":"saved-attempt"}');
        "#).unwrap();
        db
    }
    fn retained_keys() -> Vec<Key> {
        [
            (Kind::Run, "r"),
            (Kind::Turn, "t"),
            (Kind::Shell, "s"),
            (Kind::Message, "message"),
            (Kind::Attempt, "a"),
        ]
        .into_iter()
        .map(|(kind, id)| Key {
            kind,
            id: id.into(),
        })
        .collect()
    }
    #[test]
    fn every_native_dto_matches_canonical_status_including_targets_and_mutated_metadata() {
        let db = canonical_fixture();
        for changed in [false, true] {
            if changed {
                db.execute_batch(r#"
                  UPDATE thread SET title='Renamed',profile_id='new-profile',model_id='new-model',parent_thread_id=NULL WHERE id='scope';
                  UPDATE model_attempt SET profile_id='new-attempt-profile',model_id='new-attempt-model' WHERE id='a';
                  UPDATE receipt SET result_json='{"deliver":"complete"}' WHERE id='receipt';
                  UPDATE shell_execution SET shell_json='{"completion":{"elapsed_ms":99}}' WHERE id='s';
                  INSERT INTO output_chunk VALUES('s',2,'stderr',0,X'62','2026-11-01T00:00:00.5Z');
                "#).unwrap();
            }
            let args = crate::protocol::StatusArgs {
                thread_id: Some("scope".into()),
                all: true,
                limit: 200,
                ..Default::default()
            };
            let canonical = super::super::snapshot(&db, &args, "fixture").unwrap();
            assert_eq!(canonical.items.len(), 5);
            let mut budget = TextBudget::default();
            let raw = read_status_tx(&db, &retained_keys(), &mut budget).unwrap();
            assert_eq!(raw.statements, 5);
            assert!(raw.limited.is_empty());
            let prepared = prepare(raw);
            assert_eq!(prepared["items"].as_array().unwrap().len(), 5);
            for expected in canonical.items {
                let actual = prepared["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["kind"] == expected.kind && row["id"] == expected.id)
                    .unwrap();
                assert_eq!(actual["current"], serde_json::to_value(expected).unwrap());
            }
            assert!(budget.used <= TEXT_BYTES);
        }
    }
    #[test]
    fn discard_status_page_and_point_are_identical_and_reject_private_legacy_fields() {
        let db = canonical_fixture();
        let args = crate::protocol::StatusArgs {
            thread_id: Some("scope".into()),
            all: true,
            limit: 200,
            ..Default::default()
        };
        let fact =
            serde_json::json!({"reason":"run_started","phase":"closed_summary","elapsed_ms":45});
        for extra in [false, true] {
            let mut body = serde_json::json!({"kind":"original_failure","private":"PRIVATE_SENTINEL","automatic_trim_attempt":{"discard":fact}});
            if extra {
                body["automatic_trim_attempt"]["discard"]["secret"] =
                    serde_json::json!("PRIVATE_SENTINEL");
            }
            db.execute(
                "UPDATE model_attempt SET error_json=?1 WHERE id='a'",
                [body.to_string()],
            )
            .unwrap();
            let page = super::super::snapshot(&db, &args, "fixture").unwrap();
            let expected = page.items.into_iter().find(|v| v.id == "a").unwrap();
            assert_eq!(expected.compaction_discard.is_some(), !extra);
            let prepared = prepare(
                read_status_tx(
                    &db,
                    &[Key {
                        kind: Kind::Attempt,
                        id: "a".into(),
                    }],
                    &mut TextBudget::default(),
                )
                .unwrap(),
            );
            assert_eq!(
                prepared["items"][0]["current"],
                serde_json::to_value(expected).unwrap()
            );
            assert!(!prepared.to_string().contains("PRIVATE_SENTINEL"));
        }
        for value in [
            serde_json::json!(123),
            serde_json::json!(true),
            serde_json::json!([]),
            serde_json::json!("private"),
            serde_json::Value::Null,
            serde_json::json!({"reason":"future","phase":"closed_summary","elapsed_ms":1}),
        ] {
            let body = serde_json::json!({"automatic_trim_attempt":{"discard":value}});
            db.execute(
                "UPDATE model_attempt SET error_json=?1 WHERE id='a'",
                [body.to_string()],
            )
            .unwrap();
            let page = super::super::snapshot(&db, &args, "fixture").unwrap();
            assert!(
                page.items
                    .iter()
                    .find(|v| v.id == "a")
                    .unwrap()
                    .compaction_discard
                    .is_none()
            );
            let point = prepare(
                read_status_tx(
                    &db,
                    &[Key {
                        kind: Kind::Attempt,
                        id: "a".into(),
                    }],
                    &mut TextBudget::default(),
                )
                .unwrap(),
            );
            assert_eq!(point["items"].as_array().unwrap().len(), 1);
            assert!(
                point["items"][0]["current"]
                    .get("compaction_discard")
                    .is_none()
            );
        }
    }
    #[test]
    fn saturated_latest_turn_is_unknown_not_a_stale_dto() {
        let db = canonical_fixture();
        for n in 0..200 {
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,'scope','r','completed','2026-11-01T00:00:00Z')",[format!("t{n:03}")]).unwrap();
        }
        let raw = read_status_tx(&db, &retained_keys(), &mut TextBudget::default()).unwrap();
        assert!(!raw.facts.iter().any(|fact| fact.key.kind == Kind::Run));
        assert!(raw.limited.iter().any(|limited| limited.id == "r"));
        assert!(raw.missing.is_empty());
    }
}
