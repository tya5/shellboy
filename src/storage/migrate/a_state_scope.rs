//! Startup-only v1 fact conversion; ordinary readers recognize only v2.
use crate::{compaction::a_state::ScopeIdentity, storage::StorageError};
use rusqlite::{Connection, OptionalExtension, params, types::ValueRef};
use serde::{Deserialize, Serialize};

const PREFIX: &str = "compact.a_state.";
const MAX_BYTES: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1 {
    version: u8,
    scope_generation: u64,
    boundary_generation: u64,
    connection: String,
    model: String,
    last_reset_origin: Option<String>,
    first: Option<Report>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    attempt: String,
    input_tokens: Option<u64>,
}
#[derive(Serialize)]
struct V2 {
    version: u8,
    scope_generation: u64,
    boundary_generation: u64,
    scope_identity: ScopeIdentity,
    last_reset_origin: Option<String>,
    first: Option<Report>,
}
fn old_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256
}

fn convert(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let old: V1 = serde_json::from_slice(bytes).ok()?;
    if old.version != 1
        || !old_id(&old.connection)
        || !old_id(&old.model)
        || old.last_reset_origin.as_deref().is_some_and(|s| !old_id(s))
        || old.first.as_ref().is_some_and(|r| !old_id(&r.attempt))
    {
        return None;
    }
    // Historical v1 accepted whitespace-only (but nonempty) scope IDs too.
    let identity = ScopeIdentity::new(&old.connection, &old.model).ok()?;
    serde_json::to_string(&V2 {
        version: 2,
        scope_generation: old.scope_generation,
        boundary_generation: old.boundary_generation,
        scope_identity: identity,
        last_reset_origin: old.last_reset_origin,
        first: old.first,
    })
    .ok()
    .filter(|s| s.len() <= MAX_BYTES)
}

/// Caller owns the existing Immediate migration transaction and its checkpoint.
pub(super) fn convert_rows(tx: &Connection) -> Result<(), StorageError> {
    let mut lower_bound = Some(i64::MIN);
    while let Some(cursor) = lower_bound {
        // Rowid advances even when a TEXT key contains invalid UTF-8. Decode only
        // bounded raw bytes; malformed keys must not abort unrelated upgrades.
        let row: Option<(i64, Option<String>)> = tx.query_row(
            "SELECT rowid,CASE WHEN typeof(key)='text' AND length(CAST(key AS BLOB))<=?2 THEN key END FROM app_meta WHERE rowid>=?1 ORDER BY rowid LIMIT 1",
            params![cursor, (PREFIX.len() + 256) as i64],
            |row| {
                let key = match row.get_ref(1)? {
                    ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok()
                        .filter(|key| key.strip_prefix(PREFIX).is_some_and(old_id))
                        .map(str::to_owned),
                    _ => None,
                };
                Ok((row.get(0)?, key))
            },
        ).optional()?;
        let Some((rowid, key)) = row else {
            break;
        };
        lower_bound = rowid.checked_add(1);
        let Some(key) = key else {
            continue;
        };
        let thread = &key[PREFIX.len()..];
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
            [thread],
            |r| r.get(0),
        )?;
        if !live {
            continue;
        }
        let converted = tx.query_row(
            "SELECT CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))<=?2 THEN value END FROM app_meta WHERE key=?1",
            params![key, MAX_BYTES as i64],
            |row| Ok(match row.get_ref(0)? { ValueRef::Text(bytes) => convert(bytes), _ => None }),
        )?;
        if let Some(text) = converted {
            tx.execute(
                "UPDATE app_meta SET value=?2 WHERE key=?1",
                params![key, text],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn v1(first: Value) -> String {
        json!({"version":1,"scope_generation":u64::MAX,"boundary_generation":11,
            "connection":" ","model":"m","last_reset_origin":"not-a-uuid",
            "first":first})
        .to_string()
    }

    #[test]
    fn v1_scope_conversion_preserves_first_report_counters_and_origin() {
        for first in [
            Value::Null,
            json!({"attempt":"attempt","input_tokens":null}),
            json!({"attempt":"attempt","input_tokens":0}),
            json!({"attempt":"attempt","input_tokens":u64::MAX}),
        ] {
            let text = v1(first.clone());
            let converted = convert(text.as_bytes()).unwrap();
            let current: Value = serde_json::from_str(&converted).unwrap();
            assert_eq!(current["version"], 2);
            assert_eq!(current["scope_generation"], u64::MAX);
            assert_eq!(current["boundary_generation"], 11);
            assert_eq!(current["last_reset_origin"], "not-a-uuid");
            assert_eq!(current["first"], first);
            assert_eq!(
                current["scope_identity"],
                serde_json::to_value(ScopeIdentity::new(" ", "m").unwrap()).unwrap()
            );
            assert!(
                convert(converted.as_bytes()).is_none(),
                "current rows are untouched, not rewritten"
            );
        }
        for invalid in [
            v1(json!({"attempt":"","input_tokens":1})),
            v1(Value::Null).replace("\"version\":1", "\"version\":3"),
            v1(Value::Null).replace("\"model\":\"m\"", "\"model\":\"\""),
            v1(Value::Null).replace(
                "\"model\":\"m\"",
                &format!("\"model\":\"{}\"", "x".repeat(257)),
            ),
            "{}".into(),
            "x".repeat(MAX_BYTES + 1),
        ] {
            assert!(convert(invalid.as_bytes()).is_none(), "{invalid}");
        }
    }

    fn raw_rows(db: &Connection) -> Vec<(String, String, Option<Vec<u8>>)> {
        db.prepare("SELECT key,typeof(value),CAST(value AS BLOB) FROM app_meta ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn scope_conversion_preserves_invalid_nontext_bytes_and_is_idempotent() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value); CREATE TABLE thread(id TEXT PRIMARY KEY,deleted_at TEXT);").unwrap();
        for name in [
            "valid",
            "blob",
            "integer",
            "real",
            "null",
            "invalid-utf8",
            "oversize",
            "unknown",
            "extra",
            "dangling",
            "deleted",
        ] {
            if name != "dangling" {
                db.execute("INSERT INTO thread(id,deleted_at) VALUES(?1,CASE WHEN ?1='deleted' THEN 'now' END)",[name]).unwrap();
            }
        }
        for name in [
            "valid",
            "blob",
            "integer",
            "real",
            "null",
            "invalid-utf8",
            "oversize",
            "unknown",
            "extra",
            "dangling",
            "deleted",
        ] {
            db.execute(
                "INSERT INTO app_meta VALUES(?1,?2)",
                params![format!("{PREFIX}{name}"), v1(Value::Null)],
            )
            .unwrap();
        }
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key=?2",
            params![v1(Value::Null).into_bytes(), format!("{PREFIX}blob")],
        )
        .unwrap();
        db.execute_batch("UPDATE app_meta SET value=42 WHERE key='compact.a_state.integer'; UPDATE app_meta SET value=1.25 WHERE key='compact.a_state.real'; UPDATE app_meta SET value=NULL WHERE key='compact.a_state.null'; UPDATE app_meta SET value=CAST(x'80ff' AS TEXT) WHERE key='compact.a_state.invalid-utf8';").unwrap();
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key=?2",
            params!["x".repeat(MAX_BYTES + 1), format!("{PREFIX}oversize")],
        )
        .unwrap();
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key=?2",
            params![
                v1(Value::Null).replace("\"version\":1", "\"version\":9"),
                format!("{PREFIX}unknown")
            ],
        )
        .unwrap();
        let mut extra: Value = serde_json::from_str(&v1(Value::Null)).unwrap();
        extra["unexpected"] = json!(1);
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key=?2",
            params![extra.to_string(), format!("{PREFIX}extra")],
        )
        .unwrap();
        for (rowid, name) in [(i64::MIN, "edge-min"), (i64::MAX, "edge-max")] {
            db.execute("INSERT INTO thread(id,deleted_at) VALUES(?1,NULL)", [name])
                .unwrap();
            db.execute(
                "INSERT INTO app_meta(rowid,key,value) VALUES(?1,?2,?3)",
                params![rowid, format!("{PREFIX}{name}"), v1(Value::Null)],
            )
            .unwrap();
        }
        let before = raw_rows(&db);
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        convert_rows(&tx).unwrap();
        tx.commit().unwrap();
        let after = raw_rows(&db);
        for (old, new) in before.iter().zip(&after) {
            if ["valid", "edge-min", "edge-max"]
                .iter()
                .any(|name| old.0 == format!("{PREFIX}{name}"))
            {
                assert_ne!(old.2, new.2);
            } else {
                assert_eq!(old, new, "byte AND SQLite type must survive");
            }
        }
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        convert_rows(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(raw_rows(&db), after);
    }

    fn schema45() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("DELETE FROM schema_migration WHERE version=46; PRAGMA user_version=45;")
            .unwrap();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now')",[]).unwrap();
        db.execute(
            "INSERT INTO app_meta VALUES('compact.a_state.t',?1)",
            [v1(json!({"attempt":"original","input_tokens":0}))],
        )
        .unwrap();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('u','/','','p','m','now')",[]).unwrap();
        db.execute(
            "INSERT INTO app_meta VALUES('compact.a_state.u',?1)",
            [v1(Value::Null)],
        )
        .unwrap();
        db
    }

    #[test]
    fn forward46_ledger_conversion_and_sql_fault_share_immediate_rollback() {
        let mut db = schema45();
        let before = raw_rows(&db);
        db.execute_batch("CREATE TRIGGER reject_scope_conversion BEFORE UPDATE ON app_meta WHEN OLD.key='compact.a_state.u' BEGIN SELECT RAISE(ABORT,'injected scope conversion failure'); END;").unwrap();
        let error = super::super::run(&mut db, 45, &|_, _| {}, &|| {}).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected scope conversion failure"),
            "{error}"
        );
        assert_eq!(raw_rows(&db), before);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            45
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM schema_migration WHERE version=46",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        db.execute_batch("DROP TRIGGER reject_scope_conversion")
            .unwrap();
        super::super::run(&mut db, 45, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            46
        );
        assert_eq!(db.query_row("SELECT count(*) FROM schema_migration WHERE version=46 AND name='0046_scope_identity_v2'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        let state = crate::compaction::a_state::read(&db, "t").unwrap().unwrap();
        assert_eq!(
            state.reading(),
            crate::compaction::a_state::Reading::Known(0)
        );
        assert_eq!(
            serde_json::to_value(state.token()).unwrap(),
            json!({"scope":u64::MAX,"boundary":11})
        );
        let original = state.clone();
        assert_eq!(
            crate::compaction::a_state::begin_report_tx(&db, "t", " ", "m").unwrap(),
            state.token()
        );
        let stale = serde_json::from_value(json!({"scope":u64::MAX,"boundary":10})).unwrap();
        crate::compaction::a_state::complete_report_tx(
            &db,
            "t",
            " ",
            "m",
            "late",
            Some(stale),
            Some(999),
        )
        .unwrap();
        assert_eq!(
            crate::compaction::a_state::read(&db, "t").unwrap().unwrap(),
            original
        );
        let after = raw_rows(&db);
        super::super::run(&mut db, 46, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(raw_rows(&db), after);
    }
    #[test]
    fn forward46_invalid_utf8_text_key_does_not_block_later_valid_rows() {
        let mut db = schema45();
        db.execute("DELETE FROM app_meta WHERE key='compact.a_state.u'", [])
            .unwrap();
        let mut invalid_key = PREFIX.as_bytes().to_vec();
        invalid_key.extend_from_slice(&[0xff, 0x80]);
        db.execute(
            "INSERT INTO app_meta(key,value) VALUES(CAST(?1 AS TEXT),?2)",
            params![invalid_key, v1(Value::Null)],
        )
        .unwrap();
        let bad_rowid = db.last_insert_rowid();
        db.execute(
            "INSERT INTO app_meta VALUES('compact.a_state.u',?1)",
            [v1(Value::Null)],
        )
        .unwrap();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('unknown','/','','p','m','now')",[]).unwrap();
        let mut unknown_json: Value = serde_json::from_str(&v1(Value::Null)).unwrap();
        unknown_json["version"] = json!(9);
        let unknown = unknown_json.to_string();
        db.execute(
            "INSERT INTO app_meta VALUES('compact.a_state.unknown',?1)",
            [&unknown],
        )
        .unwrap();
        let raw_key_row = |db: &Connection| -> (String, Vec<u8>, String, Vec<u8>) {
            db.query_row("SELECT typeof(key),CAST(key AS BLOB),typeof(value),CAST(value AS BLOB) FROM app_meta WHERE rowid=?1",[bad_rowid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
        };
        let original = raw_key_row(&db);
        assert_eq!(original.0, "text");
        assert!(std::str::from_utf8(&original.1).is_err());
        let before = db
            .query_row(
                "SELECT rowid FROM app_meta WHERE key='compact.a_state.t'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        let after = db
            .query_row(
                "SELECT rowid FROM app_meta WHERE key='compact.a_state.u'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert!(before < bad_rowid && bad_rowid < after);
        super::super::run(&mut db, 45, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(
            raw_key_row(&db),
            original,
            "malformed raw key and value retain exact type and bytes"
        );
        for thread in ["t", "u"] {
            assert!(
                crate::compaction::a_state::read(&db, thread)
                    .unwrap()
                    .is_some(),
                "both valid rows convert across invalid key"
            );
        }
        assert_eq!(
            db.query_row(
                "SELECT value FROM app_meta WHERE key='compact.a_state.unknown'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            unknown
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            46
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM schema_migration WHERE version=46",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let t = crate::compaction::a_state::read(&db, "t").unwrap();
        let u = crate::compaction::a_state::read(&db, "u").unwrap();
        super::super::run(&mut db, 46, &|_, _| {}, &|| {}).unwrap();
        assert_eq!(raw_key_row(&db), original);
        assert_eq!(crate::compaction::a_state::read(&db, "t").unwrap(), t);
        assert_eq!(crate::compaction::a_state::read(&db, "u").unwrap(), u);
    }
}
