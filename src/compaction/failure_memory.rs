//! Test-first contract for the private S2b failure-memory storage boundary.
//! The typed reader and CAS writer are implemented in the following source commit.
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
             ('blob',?5),('integer',?6),('null',NULL)",
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
    fn sql_projection_gates_value_materialization_by_storage_type_and_byte_length() {
        // This assertion binds the production reader itself to a CASE projection:
        // only a text value with exactly 64 SQLite bytes may leave the query.
        assert!(SLOT_READ_SQL.contains("typeof(value)='text'"));
        assert!(SLOT_READ_SQL.contains("length(CAST(value AS BLOB))=64"));
        assert!(SLOT_READ_SQL.contains("THEN value ELSE NULL END"));
    }

    #[test]
    fn cas_insert_and_exact_replacement_misses_preserve_concurrent_values() {
        let mut db = db();
        let tx = db.transaction().unwrap();
        assert!(write(&tx, "absent", &SlotRead::Absent, &"a".repeat(64)).unwrap());
        tx.commit().unwrap();

        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='absent'",
            [&"b".repeat(64)],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!write(&tx, "absent", &SlotRead::Absent, &"c".repeat(64)).unwrap());
        tx.commit().unwrap();
        assert_eq!(
            read(&db, "absent").unwrap(),
            SlotRead::Exact("b".repeat(64))
        );

        let observed = SlotRead::Exact("b".repeat(64));
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='absent'",
            [&"d".repeat(64)],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert!(!write(&tx, "absent", &observed, &"e".repeat(64)).unwrap());
        tx.commit().unwrap();
        assert_eq!(
            read(&db, "absent").unwrap(),
            SlotRead::Exact("d".repeat(64))
        );
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
