//! Private bounded storage and compare-and-swap primitives for failure memory.
use super::*;

pub(super) const SLOT_READ_SQL: &str = "SELECT typeof(value),length(CAST(value AS BLOB)),CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))=64 THEN CAST(value AS BLOB) ELSE NULL END FROM app_meta WHERE key=?1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SlotRead {
    Absent,
    Exact(String),
    UnknownInvalid,
}

pub(super) fn read(db: &Connection, key: &str) -> Result<SlotRead, StorageError> {
    let row: Option<(String, Option<i64>, Option<Vec<u8>>)> = db
        .query_row(SLOT_READ_SQL, [key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let Some((storage_type, byte_length, projected)) = row else {
        return Ok(SlotRead::Absent);
    };
    if storage_type != "text" || byte_length != Some(64) {
        return Ok(SlotRead::UnknownInvalid);
    }
    let Some(value) = projected else {
        return Ok(SlotRead::UnknownInvalid);
    };
    if value.len() != 64
        || !value
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Ok(SlotRead::UnknownInvalid);
    }
    let Ok(value) = String::from_utf8(value) else {
        return Ok(SlotRead::UnknownInvalid);
    };
    Ok(SlotRead::Exact(value))
}

pub(super) fn blocks(
    observed: &SlotRead,
    failure_identity: &str,
    provider_key: Option<&str>,
) -> bool {
    matches!(observed, SlotRead::Exact(saved) if saved == failure_identity || Some(saved.as_str()) == provider_key)
}

pub(super) fn write(
    tx: &rusqlite::Transaction<'_>,
    key: &str,
    observed: &SlotRead,
    value: &str,
) -> Result<bool, StorageError> {
    if !valid_digest(value) || matches!(observed, SlotRead::UnknownInvalid) {
        return Ok(false);
    }
    let changed = match observed {
        SlotRead::Absent => tx.execute(
            "INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO NOTHING",
            params![key, value],
        )?,
        SlotRead::Exact(expected) => tx.execute(
            "UPDATE app_meta SET value=?3 WHERE key=?1 AND typeof(value)='text' AND length(CAST(value AS BLOB))=64 AND value COLLATE BINARY=?2",
            params![key, expected, value],
        )?,
        SlotRead::UnknownInvalid => 0,
    };
    Ok(changed == 1)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn provider_turn_key(
    failure_identity: &str,
    run_id: &str,
    turn_id: &str,
    generation: u64,
    owner_instance_id: &str,
) -> String {
    crate::digest::sha256_hex(
        serde_json::to_vec(&json!([
            "compaction-provider-turn-v1",
            failure_identity,
            run_id,
            turn_id,
            generation,
            owner_instance_id
        ]))
        .expect("provider turn identity serializes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value);")
            .unwrap();
        db
    }

    #[test]
    fn bounded_reader_distinguishes_absent_exact_and_unknown_invalid() {
        let db = db();
        db.execute(
            "INSERT INTO app_meta(key,value) VALUES
             ('exact',?1),('malformed',?2),('uppercase',?3),('oversize',?4),
             ('blob',?5),('integer',?6),('null',NULL),('invalid_utf8',CAST(x'ff' || zeroblob(63) AS TEXT))",
            rusqlite::params![
                "a".repeat(64),
                format!("{}g", "a".repeat(63)),
                "A".repeat(64),
                "a".repeat(65_536),
                vec![b'a'; 64],
                64_i64,
            ],
        )
        .unwrap();

        assert_eq!(read(&db, "absent").unwrap(), SlotRead::Absent);
        assert_eq!(read(&db, "exact").unwrap(), SlotRead::Exact("a".repeat(64)));
        for key in [
            "malformed",
            "uppercase",
            "oversize",
            "blob",
            "integer",
            "null",
        ] {
            assert_eq!(read(&db, key).unwrap(), SlotRead::UnknownInvalid, "{key}");
        }
    }

    #[test]
    fn sql_projection_gates_value_by_type_and_exact_byte_length() {
        assert!(SLOT_READ_SQL.contains("typeof(value)='text'"));
        assert!(SLOT_READ_SQL.contains("length(CAST(value AS BLOB))=64"));
        assert!(SLOT_READ_SQL.contains("THEN CAST(value AS BLOB) ELSE NULL END"));
    }

    #[test]
    fn exact_memory_only_blocks_matching_persistent_or_provider_digest() {
        let e = "a".repeat(64);
        let provider = "b".repeat(64);
        assert!(blocks(&SlotRead::Exact(e.clone()), &e, Some(&provider)));
        assert!(blocks(
            &SlotRead::Exact(provider.clone()),
            &e,
            Some(&provider)
        ));
        assert!(!blocks(
            &SlotRead::Exact("c".repeat(64)),
            &e,
            Some(&provider)
        ));
        assert!(!blocks(&SlotRead::UnknownInvalid, &e, Some(&provider)));
        assert!(!blocks(&SlotRead::Absent, &e, Some(&provider)));
    }

    #[test]
    fn cas_insert_and_exact_replacement_misses_preserve_concurrent_values() {
        let mut db = db();
        let tx = db.transaction().unwrap();
        assert!(write(&tx, "slot", &SlotRead::Absent, &"a".repeat(64)).unwrap());
        tx.commit().unwrap();

        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='slot'",
            [&"b".repeat(64)],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!write(&tx, "slot", &SlotRead::Absent, &"c".repeat(64)).unwrap());
        tx.commit().unwrap();
        assert_eq!(read(&db, "slot").unwrap(), SlotRead::Exact("b".repeat(64)));

        let observed = SlotRead::Exact("b".repeat(64));
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='slot'",
            [&"d".repeat(64)],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!write(&tx, "slot", &observed, &"e".repeat(64)).unwrap());
        tx.commit().unwrap();
        assert_eq!(read(&db, "slot").unwrap(), SlotRead::Exact("d".repeat(64)));

        let observed = SlotRead::Exact("d".repeat(64));
        for replacement in [
            rusqlite::types::Value::Blob(vec![b'x'; 64]),
            rusqlite::types::Value::Text("x".repeat(65_536)),
            rusqlite::types::Value::Text(format!("{}g", "x".repeat(63))),
            rusqlite::types::Value::Text("X".repeat(64)),
        ] {
            db.execute(
                "UPDATE app_meta SET value=?1 WHERE key='slot'",
                [&replacement],
            )
            .unwrap();
            let tx = db.transaction().unwrap();
            assert!(!write(&tx, "slot", &observed, &"f".repeat(64)).unwrap());
            tx.commit().unwrap();
            let saved: rusqlite::types::Value = db
                .query_row("SELECT value FROM app_meta WHERE key='slot'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                saved, replacement,
                "replacement must survive stale exact observation"
            );
        }

        db.execute("DELETE FROM app_meta WHERE key='slot'", [])
            .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!write(&tx, "slot", &observed, &"f".repeat(64)).unwrap());
        tx.commit().unwrap();
        assert_eq!(
            read(&db, "slot").unwrap(),
            SlotRead::Absent,
            "stale update cannot resurrect deletion"
        );
    }

    #[test]
    fn absent_insert_cas_loses_to_concurrent_valid_or_invalid_row() {
        let mut db = db();
        for replacement in [
            rusqlite::types::Value::Text("a".repeat(64)),
            rusqlite::types::Value::Text("x".repeat(65_536)),
            rusqlite::types::Value::Blob(vec![b'x'; 64]),
            rusqlite::types::Value::Text(format!("{}g", "x".repeat(63))),
        ] {
            db.execute(
                "INSERT INTO app_meta(key,value) VALUES('slot',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [&replacement],
            ).unwrap();
            let tx = db.transaction().unwrap();
            assert!(!write(&tx, "slot", &SlotRead::Absent, &"b".repeat(64)).unwrap());
            tx.commit().unwrap();
            let saved: rusqlite::types::Value = db
                .query_row("SELECT value FROM app_meta WHERE key='slot'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                saved, replacement,
                "insert-if-absent must preserve any concurrent row"
            );
        }
    }

    #[test]
    fn unknown_invalid_never_repairs_or_overwrites_even_if_slot_changes() {
        let mut db = db();
        db.execute(
            "INSERT INTO app_meta(key,value) VALUES('slot',?1)",
            ["x".repeat(65_536)],
        )
        .unwrap();
        let observed = read(&db, "slot").unwrap();
        assert_eq!(observed, SlotRead::UnknownInvalid);
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='slot'",
            [&"a".repeat(64)],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!write(&tx, "slot", &observed, &"b".repeat(64)).unwrap());
        tx.commit().unwrap();
        assert_eq!(read(&db, "slot").unwrap(), SlotRead::Exact("a".repeat(64)));
    }

    #[test]
    fn read_and_write_sql_errors_are_fatal() {
        let missing = rusqlite::Connection::open_in_memory().unwrap();
        assert!(read(&missing, "slot").is_err());

        let mut db = db();
        db.execute(
            "INSERT INTO app_meta(key,value) VALUES('slot',?1)",
            [&"a".repeat(64)],
        )
        .unwrap();
        db.execute_batch("CREATE TRIGGER fail_slot_update BEFORE UPDATE ON app_meta BEGIN SELECT RAISE(ABORT,'synthetic write failure'); END;").unwrap();
        let observed = read(&db, "slot").unwrap();
        let tx = db.transaction().unwrap();
        assert!(write(&tx, "slot", &observed, &"b".repeat(64)).is_err());
        drop(tx);
        assert_eq!(read(&db, "slot").unwrap(), observed);
    }

    #[test]
    fn provider_turn_key_is_bound_to_real_invocation_scope_and_owner() {
        let base = provider_turn_key("e", "run", "turn", 4, "owner");
        assert_eq!(base.len(), 64);
        assert!(
            base.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_ne!(
            base,
            provider_turn_key("other-e", "run", "turn", 4, "owner")
        );
        assert_ne!(
            base,
            provider_turn_key("e", "other-run", "turn", 4, "owner")
        );
        assert_ne!(
            base,
            provider_turn_key("e", "run", "other-turn", 4, "owner")
        );
        assert_ne!(base, provider_turn_key("e", "run", "turn", 5, "owner"));
        assert_ne!(
            base,
            provider_turn_key("e", "run", "turn", 4, "other-owner")
        );
        assert_eq!(base, provider_turn_key("e", "run", "turn", 4, "owner"));
    }
}
