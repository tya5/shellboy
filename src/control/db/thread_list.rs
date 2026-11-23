use super::{
    Result, error, normalize, sql_error, storage_error, tags, thread_display_title, thread_excerpt,
    thread_summary, title_text,
};
use crate::digest::sha256_hex as hash;
use crate::protocol::{
    ArchivedFilter, ErrorCode, Page, PageCursor, ThreadListArgs, ThreadListOrder, ThreadSummary,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use std::path::Path;

fn list_scope(mode: ArchivedFilter) -> &'static str {
    match mode {
        ArchivedFilter::Exclude => "thread.list",
        ArchivedFilter::Include => "thread.list.include_archived",
        ArchivedFilter::Only => "thread.list.only_archived",
    }
}

pub(crate) fn thread_list(
    connection: &Connection,
    args: &ThreadListArgs,
) -> Result<Page<ThreadSummary>> {
    if let Some(ids) = &args.thread_ids {
        let items = summaries_by_id(connection, ids)?;
        return Ok(Page {
            items,
            next_cursor: None,
        });
    }
    if args.awaiting.is_some() {
        return thread_list_awaiting_human(connection, args);
    }
    let filter = args.query.as_deref().unwrap_or("");
    let needle = title_text(filter).to_lowercase();
    // Tags and cwd narrow in SQL; the cursor is bound to them through a digest.
    let tags = tags::normalize_all(&args.tags)?;
    // Same lexical rule as saved cwds (`normalize`): `/work/proj/` and `/work/./proj` find
    // `/work/proj`; a relative path has no base here and is refused, never silently empty.
    let cwd_normalized = match args.cwd.as_deref() {
        None => None,
        Some(raw) if Path::new(raw).is_absolute() => Some(
            normalize(Path::new(raw), Path::new("/"))?
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| error(ErrorCode::InvalidArgument, "cwd must be valid UTF-8"))?,
        ),
        Some(_) => {
            return Err(error(
                ErrorCode::InvalidArgument,
                "cwd filter must be an absolute path",
            ));
        }
    };
    let cwd = cwd_normalized.as_deref();
    let cursor_filter = list_cursor_filter(filter, &tags, cwd);
    let state = args.state.map(crate::protocol::ThreadListState::as_str);
    let scope = match (args.order, state) {
        (ThreadListOrder::Updated, Some(state)) => {
            format!("{}.state.{state}", list_scope(args.archived))
        }
        (ThreadListOrder::Updated, None) => list_scope(args.archived).to_owned(),
        (ThreadListOrder::Created, _) => format!("thread.list.created.{}", args.archived.as_str()),
    };
    let cursor = args
        .cursor
        .as_deref()
        .map(|token| PageCursor::decode(token, &scope, &cursor_filter, 0, None))
        .transpose()?;
    let watermark: i64 = match (args.order, &cursor) {
        (ThreadListOrder::Created, _) => 0,
        (ThreadListOrder::Updated, Some(cursor)) => i64::try_from(cursor.watermark)
            .map_err(|_| error(ErrorCode::CursorExpired, "invalid thread watermark"))?,
        (ThreadListOrder::Updated, None) => connection
            .query_row("SELECT COALESCE(MAX(rowid),0) FROM thread", [], |row| {
                row.get(0)
            })
            .map_err(sql_error)?,
    };
    let key = cursor.map(|cursor| cursor.last_sort_key);
    let position = key
        .as_deref()
        .map(serde_json::from_str::<(Option<String>, String)>)
        .transpose()
        .map_err(|_| error(ErrorCode::CursorExpired, "invalid thread list position"))?;
    if args.order == ThreadListOrder::Updated
        && position.as_ref().is_some_and(|(time, _)| time.is_none())
    {
        return Err(error(
            ErrorCode::CursorExpired,
            "invalid thread list position",
        ));
    }
    let last_time = position
        .as_ref()
        .and_then(|(time, _)| time.as_deref())
        .unwrap_or("");
    let last_id = position.as_ref().map_or("", |(_, id)| id.as_str());
    // Filter against the same derived labels returned by ThreadGet/List. Keep
    // candidates as a cursor: a filtered miss scans without collecting history.
    let sql = match args.order {
        ThreadListOrder::Updated => {
            thread_summary::page_sql(args.archived, tags.len(), cwd.is_some(), state.is_some())
        }
        ThreadListOrder::Created => {
            thread_summary::created_page_sql(args.archived, position.as_ref().map(|(time, _)| time))
        }
    };
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let page_limit = if needle.is_empty() {
        i64::from(args.limit) + 1
    } else {
        -1
    };
    let mut binds: Vec<&dyn rusqlite::ToSql> = match args.order {
        ThreadListOrder::Updated => vec![&watermark, &last_time, &last_id, &page_limit],
        ThreadListOrder::Created => vec![&last_time, &last_id, &page_limit],
    };
    binds.extend(tags.iter().map(|tag| tag as &dyn rusqlite::ToSql));
    binds.extend(cwd.iter().map(|path| path as &dyn rusqlite::ToSql));
    binds.extend(state.iter().map(|state| state as &dyn rusqlite::ToSql));
    let mut rows = statement.query(binds.as_slice()).map_err(sql_error)?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let raw = thread_summary::decode(row).map_err(sql_error)?;
        let id = &raw.thread_id;
        let title = thread_display_title(connection, id, &raw.stored_title)?;
        if !thread_summary::matches_filter(id, &title, &raw.cwd, filter) {
            continue;
        }
        items.push(complete_summary(connection, raw, title)?);
        if items.len() > args.limit as usize {
            break;
        }
    }
    let next_cursor = if items.len() > args.limit as usize {
        items.pop();
        let last = items.last().unwrap();
        Some(
            PageCursor {
                version: 1,
                scope,
                filter: cursor_filter,
                last_sort_key: serde_json::to_string(&(
                    match args.order {
                        ThreadListOrder::Updated => Some(last.updated_at.as_str()),
                        ThreadListOrder::Created => last.created_at.as_deref(),
                    },
                    &last.thread_id,
                ))
                .unwrap(),
                watermark: watermark as u64,
                revision: 0,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    attach_tags(connection, &mut items)?;
    Ok(Page { items, next_cursor })
}

fn complete_summary(
    connection: &Connection,
    raw: thread_summary::RawSummary,
    title: String,
) -> Result<ThreadSummary> {
    let excerpt = thread_excerpt(connection, &raw.thread_id)?;
    let mut summary = thread_summary::prepare(raw, title, excerpt);
    summary.last_end_message =
        crate::thread_observation::last_end_message(connection, &summary.thread_id)?;
    summary.last_input = crate::thread_observation::last_input(connection, &summary.thread_id)?;
    Ok(summary)
}

fn attach_tags(connection: &Connection, items: &mut [ThreadSummary]) -> Result<()> {
    let ids: Vec<_> = items.iter().map(|item| item.thread_id.clone()).collect();
    let mut tagged = tags::for_threads(connection, &ids)?;
    for item in items {
        item.tags = tagged.remove(&item.thread_id).unwrap_or_default();
    }
    Ok(())
}

fn summaries_by_id(connection: &Connection, ids: &[String]) -> Result<Vec<ThreadSummary>> {
    let raw = thread_summary::read_ids_tx(connection, ids).map_err(storage_error)?;
    let mut items = Vec::with_capacity(raw.len());
    for raw in raw {
        let title = thread_display_title(connection, &raw.thread_id, &raw.stored_title)?;
        items.push(complete_summary(connection, raw, title)?);
    }
    attach_tags(connection, &mut items)?;
    Ok(items)
}

/// Enumerate only target Threads present in the partial pending-reply index.
/// The dedicated partial index excludes Thread-directed replies before paging,
/// so work follows human reply duties rather than the complete Thread table.
pub(super) fn thread_list_awaiting_human(
    connection: &Connection,
    args: &ThreadListArgs,
) -> Result<Page<ThreadSummary>> {
    const SCOPE: &str = "thread.list.awaiting_human.live.v2";
    let name = args.awaiting.as_deref().expect("awaiting filter");
    let archived_filter = format!("{name}:{}", args.archived.as_str());
    let decoded = args
        .cursor
        .as_ref()
        .map(|token| PageCursor::decode_live(token, SCOPE, &archived_filter, 0, None))
        .transpose()?;
    let after = decoded
        .as_ref()
        .map_or("", |cursor| cursor.last_sort_key.as_str());
    let candidates = awaiting_human_candidates(connection, after, args.limit, name)?;
    let has_more = candidates.len() > args.limit as usize;
    let examined = &candidates[..candidates.len().min(args.limit as usize)];
    let candidate_ids: Vec<String> = examined.iter().map(|(id, _)| id.clone()).collect();
    let mut visible_ids = Vec::with_capacity(candidate_ids.len());
    for id in &candidate_ids {
        let row: Option<Option<String>> = connection
            .query_row(
                "SELECT archived_at FROM thread WHERE id=?1 AND deleted_at IS NULL",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        let Some(archived_at) = row else {
            continue;
        };
        if matches!(
            (args.archived, archived_at.is_some()),
            (ArchivedFilter::Exclude, true) | (ArchivedFilter::Only, false)
        ) {
            continue;
        }
        visible_ids.push(id.clone());
    }
    let mut items = summaries_by_id(connection, &visible_ids)?;
    for item in &mut items {
        if let Some((_, counts)) = candidates.iter().find(|(id, _)| id == &item.thread_id) {
            item.awaiting_named_count = counts.count;
            item.awaiting_named_count_exact = counts.exact;
            item.awaiting_named_oldest_at = counts.oldest.clone();
        }
    }
    items.retain(|item| item.awaiting_named_count > 0);
    let next_cursor = if has_more {
        let last = examined.last().expect("continued page has candidates");
        Some(
            PageCursor {
                version: 2,
                scope: SCOPE.into(),
                filter: archived_filter,
                last_sort_key: last.0.clone(),
                watermark: 0,
                revision: 0,
                encoding: None,
            }
            .encode()
            .map_err(|_| error(ErrorCode::InvalidState, "cannot encode thread cursor"))?,
        )
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

#[derive(Debug)]
struct AwaitingHumanCount {
    count: u64,
    exact: bool,
    oldest: Option<String>,
}

/// Read a bounded page of pending Thread keys and exact page-local aggregates.
fn awaiting_human_candidates(
    connection: &Connection,
    after: &str,
    limit: u32,
    name: &str,
) -> Result<Vec<(String, AwaitingHumanCount)>> {
    let mut statement = connection
        .prepare(awaiting_human_candidates_sql())
        .map_err(sql_error)?;
    let rows = statement
        .query_map(
            params![
                after,
                i64::from(limit) + 1,
                (crate::protocol::MAX_HUMAN_REPLY_SUMMARY_DUTIES + 1) as i64,
                name,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(sql_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map(|rows| {
            rows.into_iter()
                .map(|(id, count, oldest)| {
                    let exact =
                        count < (crate::protocol::MAX_HUMAN_REPLY_SUMMARY_DUTIES + 1) as i64;
                    (
                        id,
                        AwaitingHumanCount {
                            count: count.max(0) as u64,
                            exact,
                            oldest: if exact { oldest } else { None },
                        },
                    )
                })
                .collect()
        })
        .map_err(sql_error)
}

/// Each candidate costs at most one index seek; per-Thread aggregates stop at 17.
pub(super) fn awaiting_human_candidates_sql() -> &'static str {
    "WITH RECURSIVE page(target_thread_id) AS (\
       SELECT (SELECT r.target_thread_id FROM receipt r INDEXED BY receipt_named_reply_pending \
         WHERE r.target_thread_id>?1 AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.ask_name')=?4 ORDER BY r.target_thread_id LIMIT 1) \
       UNION ALL SELECT (SELECT r.target_thread_id FROM receipt r INDEXED BY receipt_named_reply_pending \
         WHERE r.target_thread_id>page.target_thread_id AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.ask_name')=?4 ORDER BY r.target_thread_id LIMIT 1) FROM page WHERE page.target_thread_id IS NOT NULL LIMIT ?2) \
     SELECT page.target_thread_id,\
       (SELECT count(*) FROM (SELECT 1 FROM receipt r INDEXED BY receipt_named_reply_pending WHERE r.target_thread_id=page.target_thread_id \
         AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.ask_name')=?4 LIMIT ?3)),\
       CASE WHEN (SELECT count(*) FROM (SELECT 1 FROM receipt r INDEXED BY receipt_named_reply_pending WHERE r.target_thread_id=page.target_thread_id \
         AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.ask_name')=?4 LIMIT ?3))<?3 \
         THEN (SELECT min(r.created_at) FROM (SELECT r.created_at FROM receipt r INDEXED BY receipt_named_reply_pending WHERE r.target_thread_id=page.target_thread_id \
         AND json_extract(r.result_json,'$.reply.state')='pending' AND json_extract(r.result_json,'$.ask_name')=?4 ORDER BY r.created_at LIMIT ?3) r) END \
     FROM page WHERE page.target_thread_id IS NOT NULL ORDER BY page.target_thread_id"
}

/// The cursor's filter: the text query alone when nothing else narrows the list,
/// otherwise the query plus a digest of the sorted tags and the cwd.
fn list_cursor_filter(query: &str, tags: &[String], cwd: Option<&str>) -> String {
    if tags.is_empty() && cwd.is_none() {
        return query.to_owned();
    }
    let mut sorted = tags.to_vec();
    sorted.sort();
    format!(
        "{query}\u{0}{}",
        hash(json!({"tags": sorted, "cwd": cwd}).to_string().as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_thread_list_returns_existing_summaries_without_paging() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at,archived_at,deleted_at) VALUES
            ('wanted','Wanted','/','','p','m','1','2',NULL),
            ('other','Other','/','','p','m','3',NULL,NULL),
            ('deleted','Deleted','/','','p','m','1',NULL,'2');
            INSERT INTO thread_tag(thread_id,tag) VALUES('wanted','saved');").unwrap();
        let ordinary = thread_list(
            &db,
            &ThreadListArgs {
                archived: ArchivedFilter::Include,
                ..Default::default()
            },
        )
        .unwrap();
        let expected = ordinary
            .items
            .iter()
            .find(|item| item.thread_id == "wanted")
            .unwrap();
        let mut ids = vec!["wanted".into(), "deleted".into()];
        ids.extend((2..crate::protocol::MAX_LIST_LIMIT).map(|i| format!("missing-{i}")));
        let exact = thread_list(
            &db,
            &ThreadListArgs {
                thread_ids: Some(ids),
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(exact.next_cursor.is_none());
        assert_eq!(
            serde_json::to_value(exact.items).unwrap(),
            json!([expected])
        );
        let empty = thread_list(
            &db,
            &ThreadListArgs {
                thread_ids: Some(Vec::new()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(empty.items.is_empty() && empty.next_cursor.is_none());
    }
}
