//! Thread tags (#222 stage 3): free labels for placement and cross-cutting views.
//!
//! Only `thread_tag` rows and `thread.revision` change. History, nodes, GC roots,
//! `updated_at` and running work are untouched, so list order does not move. Tags
//! are never inherited and end with their Thread (`thread.delete`).
use super::{MutationResult, error, sql_error, target};
use crate::protocol::validation::{MAX_THREAD_TAGS, normalize_tag};
use crate::protocol::{
    ApiError, ErrorCode, Origin, Page, PageCursor, ThreadCwdsArgs, ThreadTagArgs, ThreadTagsArgs,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;
use std::collections::{HashMap, HashSet};

const TAGS_SCOPE: &str = "thread.tags";
const CWDS_SCOPE: &str = "thread.cwds";
/// The next distinct saved cwd after the bound one: one seek of `threads_cwd_updated`.
pub(super) const NEXT_CWD_SQL: &str = "SELECT cwd FROM thread INDEXED BY threads_cwd_updated
     WHERE deleted_at IS NULL AND cwd>?1 ORDER BY cwd LIMIT 1";
/// Every Thread of one list page in one statement (`thread_id` is the key prefix).
pub(super) const FOR_THREADS_SQL: &str = "SELECT thread_id,tag FROM thread_tag
             WHERE thread_id IN (SELECT value FROM json_each(?1)) ORDER BY thread_id,tag";
/// The next distinct name after the bound one: one index seek.
pub(super) const NEXT_NAME_SQL: &str =
    "SELECT tag FROM thread_tag WHERE tag>?1 ORDER BY tag LIMIT 1";

/// Trimmed, validated and de-duplicated, keeping the caller's order.
pub(crate) fn normalize_all(raw: &[String]) -> Result<Vec<String>, ApiError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for tag in raw {
        let tag = normalize_tag(tag)?;
        if seen.insert(tag.clone()) {
            out.push(tag);
        }
    }
    Ok(out)
}

pub(super) fn run(
    tx: &Transaction<'_>,
    args: &ThreadTagArgs,
    caller: Option<&Origin>,
) -> super::Result<MutationResult> {
    let requested: Vec<String> = if args.thread_ids.is_empty() {
        vec![target(None, caller)?]
    } else {
        let mut seen = HashSet::new();
        args.thread_ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .cloned()
            .collect()
    };
    let add = normalize_all(&args.add)?;
    let remove = normalize_all(&args.remove)?;
    let encoded = serde_json::to_string(&requested).expect("Thread IDs serialize");
    // At most 200 validated IDs: no cap needed.
    let live: HashSet<String> = tx
        .prepare(
            "SELECT id FROM thread WHERE id IN (SELECT value FROM json_each(?1)) AND deleted_at IS NULL",
        )
        .and_then(|mut statement| {
            statement
                .query_map([&encoded], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()
        })
        .map_err(sql_error)?;
    if live.is_empty() {
        return Err(error(ErrorCode::ThreadNotFound, "Thread not found"));
    }
    let not_found: Vec<&String> = requested.iter().filter(|id| !live.contains(*id)).collect();
    let (mut changed, mut unchanged) = (Vec::new(), Vec::new());
    for id in requested.iter().filter(|id| live.contains(*id)) {
        let mut touched = 0;
        for tag in &add {
            touched += tx
                .execute(
                    "INSERT OR IGNORE INTO thread_tag(thread_id,tag) VALUES(?1,?2)",
                    params![id, tag],
                )
                .map_err(sql_error)?;
        }
        for tag in &remove {
            touched += tx
                .execute(
                    "DELETE FROM thread_tag WHERE thread_id=?1 AND tag=?2",
                    params![id, tag],
                )
                .map_err(sql_error)?;
        }
        if touched == 0 {
            unchanged.push(id.clone());
            continue;
        }
        let total: i64 = tx
            .query_row(
                "SELECT count(*) FROM thread_tag WHERE thread_id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if total > MAX_THREAD_TAGS as i64 {
            // The error rolls the whole call back: nothing is partially tagged.
            return Err(error(
                ErrorCode::InputLimit,
                format!("Thread {id} would carry {total} tags; the limit is {MAX_THREAD_TAGS}"),
            ));
        }
        tx.execute("UPDATE thread SET revision=revision+1 WHERE id=?1", [id])
            .map_err(sql_error)?;
        changed.push(id.clone());
    }
    let mut result = MutationResult::value(json!({
        "changed": changed,
        "unchanged": unchanged,
        "not_found": not_found,
        "add": add,
        "remove": remove,
    }))?;
    result.changed_threads = changed;
    Ok(result)
}

/// Tags of up to one page of Threads in one statement; absent Threads have none.
pub(crate) fn for_threads(
    connection: &Connection,
    ids: &[String],
) -> super::Result<HashMap<String, Vec<String>>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let encoded = serde_json::to_string(ids).expect("Thread IDs serialize");
    let mut statement = connection.prepare(FOR_THREADS_SQL).map_err(sql_error)?;
    let rows = statement
        .query_map([encoded], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_error)?;
    for row in rows {
        let (id, tag) = row.map_err(sql_error)?;
        out.entry(id).or_default().push(tag);
    }
    Ok(out)
}

pub(crate) fn for_thread(connection: &Connection, id: &str) -> super::Result<Vec<String>> {
    connection
        .prepare("SELECT tag FROM thread_tag WHERE thread_id=?1 ORDER BY tag")
        .and_then(|mut statement| {
            statement
                .query_map([id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(sql_error)
}

/// Distinct names in byte order. Each name costs one index seek, never a scan of
/// its members, so a page is at most `limit + 1` seeks however large a tag is.
pub(crate) fn names(connection: &Connection, args: &ThreadTagsArgs) -> super::Result<Page<String>> {
    distinct_page(
        connection,
        TAGS_SCOPE,
        NEXT_NAME_SQL,
        args.limit,
        args.cursor.as_deref(),
    )
}

/// Distinct saved cwds of non-deleted Threads, archived ones included: the index
/// `threads_cwd_updated` has no archive column, so excluding archived Threads would
/// walk every archived entry of a directory. Same one-seek-per-name paging.
pub(crate) fn cwds(connection: &Connection, args: &ThreadCwdsArgs) -> super::Result<Page<String>> {
    distinct_page(
        connection,
        CWDS_SCOPE,
        NEXT_CWD_SQL,
        args.limit,
        args.cursor.as_deref(),
    )
}

fn distinct_page(
    connection: &Connection,
    scope: &str,
    next_sql: &str,
    limit: u32,
    cursor: Option<&str>,
) -> super::Result<Page<String>> {
    let mut after = match cursor {
        Some(token) => {
            let cursor = PageCursor::decode(token, scope, "", 0, None)?;
            Some(
                serde_json::from_str::<String>(&cursor.last_sort_key)
                    .map_err(|_| error(ErrorCode::CursorExpired, "invalid list position"))?,
            )
        }
        None => None,
    };
    let mut statement = connection.prepare(next_sql).map_err(sql_error)?;
    let mut items = Vec::new();
    while items.len() <= limit as usize {
        let next: Option<String> = statement
            .query_row([after.as_deref().unwrap_or("")], |row| row.get(0))
            .optional()
            .map_err(sql_error)?;
        let Some(value) = next else { break };
        after = Some(value.clone());
        items.push(value);
    }
    let next_cursor = if items.len() > limit as usize {
        items.pop();
        let last = items.last().expect("limit is at least one");
        Some(
            PageCursor {
                version: 1,
                scope: scope.into(),
                filter: String::new(),
                last_sort_key: serde_json::to_string(last).expect("name serializes"),
                watermark: 0,
                revision: 0,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}
