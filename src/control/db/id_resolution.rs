//! Bounded CLI-only identifier resolution against existing identifier lookup indexes.
use super::*;
use crate::protocol::{IdKind, IdResolveArgs, IdResolveResult};
use rusqlite::{Connection, params};
use serde_json::json;

const ID_MATCH_LIMIT: i64 = 9;
const MAX_ID_CANDIDATES: usize = 8;

pub(super) fn resolve_id(
    db: &Connection,
    args: &IdResolveArgs,
    caller: Option<&crate::protocol::Origin>,
) -> Result<IdResolveResult> {
    let Some(upper) = prefix_upper_bound(&args.prefix) else {
        return Err(not_found(
            args.kind,
            db,
            args.include_pending_duties,
            caller,
        )?);
    };
    let sql = match args.kind {
        IdKind::Thread => {
            "SELECT id, deleted_at IS NULL FROM thread WHERE id>=?1 AND id<?2 ORDER BY id LIMIT ?3"
        }
        IdKind::Message => {
            "SELECT message_node_id, 1 FROM receipt INDEXED BY receipt_message_node WHERE message_node_id>=?1 AND message_node_id<?2 ORDER BY message_node_id LIMIT ?3"
        }
        IdKind::Run => "SELECT id, 1 FROM run WHERE id>=?1 AND id<?2 ORDER BY id LIMIT ?3",
        IdKind::ShellExecution => {
            "SELECT id, 1 FROM shell_execution WHERE id>=?1 AND id<?2 ORDER BY id LIMIT ?3"
        }
    };
    let mut statement = db.prepare(sql).map_err(super::sql_error)?;
    // Count raw index rows before visibility filtering. A full ninth row means
    // this bounded sample cannot establish uniqueness, even if it is a tombstone.
    let rows = statement
        .query_map(params![args.prefix, upper, ID_MATCH_LIMIT], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })
        .map_err(super::sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(super::sql_error)?;
    let truncated = rows.len() == ID_MATCH_LIMIT as usize;
    let mut ids = Vec::new();
    for (id, visible) in rows {
        if visible && !ids.contains(&id) {
            ids.push(id);
        }
    }
    if truncated {
        return Err(super::error(
            ErrorCode::InvalidArgument,
            "ID prefix is too short to resolve within the bounded scan",
        )
        .hint("Pass more characters to narrow the prefix")
        .details(json!({
            "reason":"prefix_too_short",
            "kind":args.kind,
            "candidates":ids.iter().take(MAX_ID_CANDIDATES).collect::<Vec<_>>(),
            "candidates_truncated":ids.len() > MAX_ID_CANDIDATES,
            "rows_scanned":ID_MATCH_LIMIT,
        })));
    }
    match ids.as_slice() {
        [id] => Ok(IdResolveResult { id: id.clone() }),
        [] => Err(not_found(
            args.kind,
            db,
            args.include_pending_duties,
            caller,
        )?),
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

fn not_found(
    kind: IdKind,
    db: &Connection,
    include_pending_duties: bool,
    caller: Option<&crate::protocol::Origin>,
) -> Result<ApiError> {
    let mut details = json!({"kind":kind,"reason":"not_found"});
    if include_pending_duties && kind == IdKind::Message {
        let pending_duty_ids = caller
            .map(|origin| {
                crate::control::reply::awaiting_replies_tx(db, &origin.thread_id, None)
                    .map(|items| {
                        items
                            .into_iter()
                            .take(crate::protocol::MAX_PENDING_REPLY_DUTIES as usize)
                            .filter_map(|item| item["message_id"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>()
                    })
                    .map_err(super::sql_error)
            })
            .transpose()?
            .unwrap_or_default();
        details["pending_duty_ids"] = json!(pending_duty_ids);
    }
    Ok(super::error(ErrorCode::NotFound, "No ID matches this prefix").details(details))
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
            include_pending_duties: false,
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
            assert_eq!(
                resolve_id(&db, &args(kind, prefix), None).unwrap().id,
                expected
            );
        }
        assert_eq!(
            resolve_id(&db, &args(IdKind::Thread, "22222222"), None)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn missing_and_ambiguous_results_are_typed_and_candidates_are_bounded() {
        let db = database();
        for index in 0..2 {
            let id = format!("abcdef12-{index:02}");
            db.execute("INSERT INTO run VALUES(?1)", [&id]).unwrap();
        }
        let ambiguous = resolve_id(&db, &args(IdKind::Run, "abcdef12"), None).unwrap_err();
        assert_eq!(ambiguous.code, ErrorCode::InvalidArgument);
        assert_eq!(ambiguous.details.as_ref().unwrap()["reason"], "ambiguous");
        assert_eq!(
            ambiguous.details.as_ref().unwrap()["candidates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ambiguous.details.as_ref().unwrap()["candidates_truncated"],
            false
        );

        let missing = resolve_id(&db, &args(IdKind::Message, "deadbeef"), None).unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
        assert_eq!(missing.details.as_ref().unwrap()["reason"], "not_found");

        let db = database();
        for index in 0..9 {
            let id = format!("facefeed-{index:02}");
            db.execute("INSERT INTO run VALUES(?1)", [&id]).unwrap();
        }
        let truncated = resolve_id(&db, &args(IdKind::Run, "facefeed"), None).unwrap_err();
        assert_eq!(
            truncated.details.as_ref().unwrap()["reason"],
            "prefix_too_short"
        );
        assert_eq!(
            truncated.details.as_ref().unwrap()["candidates"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(
            truncated.details.as_ref().unwrap()["candidates_truncated"],
            true
        );
    }

    #[test]
    fn ninth_raw_row_is_prefix_too_short_even_when_it_is_a_tombstone() {
        let db = database();
        db.execute(
            "INSERT INTO thread VALUES('abcd1234-visible',NULL),('abcd1234-dead-1','gone'),('abcd1234-dead-2','gone'),('abcd1234-dead-3','gone'),('abcd1234-dead-4','gone'),('abcd1234-dead-5','gone'),('abcd1234-dead-6','gone'),('abcd1234-dead-7','gone'),('abcd1234-dead-8','gone')",
            [],
        )
        .unwrap();
        let error = resolve_id(&db, &args(IdKind::Thread, "abcd1234"), None).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(
            error.details.as_ref().unwrap()["reason"],
            "prefix_too_short"
        );
        assert_eq!(
            error.details.as_ref().unwrap()["candidates"],
            json!(["abcd1234-visible"])
        );
        assert_eq!(error.details.as_ref().unwrap()["rows_scanned"], 9);
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
            resolve_id(&db, &args(IdKind::Message, "abcd1234"), None)
                .unwrap()
                .id,
            "abcd1234-receipted"
        );
    }
}
