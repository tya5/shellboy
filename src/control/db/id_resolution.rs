//! Bounded CLI-only identifier resolution against existing identifier lookup indexes.
use super::*;
use crate::protocol::{IdKind, IdResolveArgs, IdResolveResult};
use rusqlite::{Connection, params};
use serde_json::json;

const ID_MATCH_LIMIT: i64 = 9;
const MAX_ID_CANDIDATES: usize = 8;

pub(super) fn resolve_id(db: &Connection, args: &IdResolveArgs) -> Result<IdResolveResult> {
    let Some(upper) = prefix_upper_bound(&args.prefix) else {
        return Err(not_found(args.kind));
    };
    let sql = match args.kind {
        IdKind::Thread => {
            "SELECT id FROM thread WHERE id>=?1 AND id<?2 AND deleted_at IS NULL ORDER BY id LIMIT ?3"
        }
        IdKind::Message => {
            "SELECT DISTINCT message_node_id FROM receipt INDEXED BY receipt_message_node WHERE message_node_id>=?1 AND message_node_id<?2 ORDER BY message_node_id LIMIT ?3"
        }
        IdKind::Run => "SELECT id FROM run WHERE id>=?1 AND id<?2 ORDER BY id LIMIT ?3",
        IdKind::ShellExecution => {
            "SELECT id FROM shell_execution WHERE id>=?1 AND id<?2 ORDER BY id LIMIT ?3"
        }
    };
    let mut statement = db.prepare(sql).map_err(super::sql_error)?;
    let ids = statement
        .query_map(params![args.prefix, upper, ID_MATCH_LIMIT], |row| {
            row.get::<_, String>(0)
        })
        .map_err(super::sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(super::sql_error)?;
    match ids.as_slice() {
        [id] => Ok(IdResolveResult { id: id.clone() }),
        [] => Err(not_found(args.kind)),
        _ => Err(
            super::error(ErrorCode::InvalidArgument, "ID prefix is ambiguous").details(json!({
                "reason":"ambiguous",
                "kind":args.kind,
                "candidates":ids.iter().take(MAX_ID_CANDIDATES).collect::<Vec<_>>(),
                "candidates_truncated":ids.len() > MAX_ID_CANDIDATES,
            })),
        ),
    }
}

fn not_found(kind: IdKind) -> ApiError {
    super::error(ErrorCode::NotFound, "No ID matches this prefix")
        .details(json!({"kind":kind,"reason":"not_found"}))
}

fn prefix_upper_bound(prefix: &str) -> Option<String> {
    if !prefix.is_ascii() {
        return None;
    }
    let mut bytes = prefix.as_bytes().to_vec();
    for index in (0..bytes.len()).rev() {
        if bytes[index] < 0x7f {
            bytes[index] += 1;
            bytes.truncate(index + 1);
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE thread(id TEXT PRIMARY KEY, deleted_at TEXT); CREATE TABLE node(id TEXT PRIMARY KEY); CREATE TABLE receipt(id TEXT PRIMARY KEY, message_node_id TEXT); CREATE INDEX receipt_message_node ON receipt(message_node_id); CREATE TABLE run(id TEXT PRIMARY KEY); CREATE TABLE shell_execution(id TEXT PRIMARY KEY);")
            .unwrap();
        db
    }

    fn args(kind: IdKind, prefix: &str) -> IdResolveArgs {
        IdResolveArgs {
            kind,
            prefix: prefix.into(),
        }
    }

    #[test]
    fn resolves_unique_primary_key_prefixes_for_each_kind_and_hides_deleted_threads() {
        let db = database();
        db.execute(
            "INSERT INTO thread VALUES('11111111-thread',NULL),('22222222-deleted','gone')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO node VALUES('33333333-message')", [])
            .unwrap();
        db.execute(
            "INSERT INTO receipt VALUES('receipt','33333333-message')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO run VALUES('44444444-run')", [])
            .unwrap();
        db.execute("INSERT INTO shell_execution VALUES('55555555-shell')", [])
            .unwrap();

        for (kind, prefix, expected) in [
            (IdKind::Thread, "11111111", "11111111-thread"),
            (IdKind::Message, "33333333", "33333333-message"),
            (IdKind::Run, "44444444", "44444444-run"),
            (IdKind::ShellExecution, "55555555", "55555555-shell"),
        ] {
            assert_eq!(resolve_id(&db, &args(kind, prefix)).unwrap().id, expected);
        }
        assert_eq!(
            resolve_id(&db, &args(IdKind::Thread, "22222222"))
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn missing_and_ambiguous_results_are_typed_and_candidates_are_bounded() {
        let db = database();
        for index in 0..10 {
            let id = format!("abcdef12-{index:02}");
            db.execute("INSERT INTO run VALUES(?1)", [&id]).unwrap();
        }
        let ambiguous = resolve_id(&db, &args(IdKind::Run, "abcdef12")).unwrap_err();
        assert_eq!(ambiguous.code, ErrorCode::InvalidArgument);
        assert_eq!(ambiguous.details.as_ref().unwrap()["reason"], "ambiguous");
        assert_eq!(
            ambiguous.details.as_ref().unwrap()["candidates"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(
            ambiguous.details.as_ref().unwrap()["candidates_truncated"],
            true
        );

        let missing = resolve_id(&db, &args(IdKind::Message, "deadbeef")).unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
        assert_eq!(missing.details.as_ref().unwrap()["reason"], "not_found");
    }

    #[test]
    fn message_prefix_range_uses_the_existing_receipt_message_index() {
        let db = database();
        let plan = db
            .prepare("EXPLAIN QUERY PLAN SELECT DISTINCT message_node_id FROM receipt INDEXED BY receipt_message_node WHERE message_node_id>='abc' AND message_node_id<'abd' ORDER BY message_node_id LIMIT 9")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" ");
        assert!(plan.contains("receipt_message_node"), "{plan}");
        assert!(
            plan.contains("message_node_id>") && plan.contains("message_node_id<"),
            "{plan}"
        );
    }

    #[test]
    fn message_resolution_requires_a_message_receipt() {
        let db = database();
        db.execute(
            "INSERT INTO node VALUES('abcd1234-unreceipted'),('abcd1234-receipted')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO receipt VALUES('r','abcd1234-receipted'),('r2','abcd1234-receipted')",
            [],
        )
        .unwrap();
        assert_eq!(
            resolve_id(&db, &args(IdKind::Message, "abcd1234"))
                .unwrap()
                .id,
            "abcd1234-receipted"
        );
    }
}
