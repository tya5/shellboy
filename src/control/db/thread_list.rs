use super::{
    Result, StorageError, error, normalize, sql_error, tags, thread_display_title, thread_summary,
    title_text,
};
use crate::digest::sha256_hex as hash;
use crate::protocol::{
    ArchivedFilter, ErrorCode, MAX_LIST_LIMIT, PageCursor, RecordPage, ThreadListArgs,
    ThreadListFilters, ThreadListOrder, ThreadListPage, ThreadSummary,
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
) -> Result<ThreadListPage> {
    if let Some(ids) = &args.thread_ids {
        let items = summaries_by_id(connection, ids)?;
        return Ok(RecordPage::new(
            MAX_LIST_LIMIT,
            list_filters(args, false),
            items,
            None,
        ));
    }
    if args.awaiting.is_some() {
        return thread_list_awaiting_human(connection, args);
    }
    let filter = args.query.as_deref().unwrap_or("");
    let has_query = !title_text(filter).is_empty();
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
            format!("{}.state_scan.{state}", list_scope(args.archived))
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
        _ if state.is_some() => 0,
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
    let (mut items, continuation) = match args.order {
        ThreadListOrder::Updated => updated_page(
            connection,
            args,
            (last_time, last_id),
            position.is_some(),
            UpdatedFilter {
                cwd,
                tags: &tags,
                state,
                has_query,
                watermark,
            },
        )?,
        ThreadListOrder::Created => {
            let sql = thread_summary::created_page_sql(
                args.archived,
                position.as_ref().map(|(time, _)| time),
            );
            let mut statement = connection.prepare(&sql).map_err(sql_error)?;
            let mut rows = statement
                .query(params![last_time, last_id, i64::from(args.limit) + 1])
                .map_err(sql_error)?;
            let mut items = Vec::new();
            let mut continuation = None;
            while let Some(row) = rows.next().map_err(sql_error)? {
                if items.len() == args.limit as usize {
                    continuation = items.last().map(|item: &ThreadSummary| {
                        (item.created_at.clone(), item.thread_id.clone())
                    });
                    break;
                }
                let raw = thread_summary::decode(row).map_err(sql_error)?;
                let title = thread_display_title(connection, &raw.thread_id, &raw.stored_title)?;
                items.push(complete_summary(connection, raw, title)?);
            }
            (items, continuation)
        }
    };
    let next_cursor = continuation
        .map(|position| {
            PageCursor {
                version: 1,
                scope,
                filter: cursor_filter,
                last_sort_key: serde_json::to_string(&position).unwrap(),
                watermark: watermark as u64,
                revision: 0,
                encoding: None,
            }
            .encode()
        })
        .transpose()?;
    attach_tags(connection, &mut items)?;
    Ok(RecordPage::new(
        args.limit,
        list_filters(args, has_query),
        items,
        next_cursor,
    ))
}

fn list_filters(args: &ThreadListArgs, has_query: bool) -> ThreadListFilters {
    let discovery = args.thread_ids.is_none();
    ThreadListFilters {
        order: (discovery && args.awaiting.is_none()).then_some(args.order),
        archived: discovery.then_some(args.archived),
        state: args.state,
        thread_ids: args.thread_ids.clone(),
        tags_omitted: !args.tags.is_empty(),
        awaiting_omitted: args.awaiting.is_some(),
        cwd_omitted: args.cwd.is_some(),
        query_omitted: has_query,
    }
}

type ListPosition = (Option<String>, String);

struct UpdatedFilter<'a> {
    cwd: Option<&'a str>,
    tags: &'a [String],
    state: Option<&'a str>,
    has_query: bool,
    watermark: i64,
}

fn updated_page(
    connection: &Connection,
    args: &ThreadListArgs,
    (last_time, last_id): (&str, &str),
    after: bool,
    filter: UpdatedFilter<'_>,
) -> Result<(Vec<ThreadSummary>, Option<ListPosition>)> {
    let sql = thread_summary::updated_candidate_sql(args.archived, after, filter.cwd.is_some());
    let mut statement = connection.prepare_cached(&sql).map_err(sql_error)?;
    let limit = crate::protocol::MAX_LIST_LIMIT;
    let candidate_limit = i64::from(limit) + 1;
    let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&candidate_limit];
    if after {
        binds.extend([&last_time as &dyn rusqlite::ToSql, &last_id]);
    }
    binds.extend(filter.cwd.iter().map(|cwd| cwd as &dyn rusqlite::ToSql));
    let mut rows = statement.query(binds.as_slice()).map_err(sql_error)?;
    let mut tag_probe = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM thread_tag WHERE thread_id=?1 AND tag=?2)")
        .map_err(sql_error)?;
    let mut state_probe = connection
        .prepare_cached(&thread_summary::state_sql())
        .map_err(sql_error)?;
    let mut ids = Vec::new();
    let mut titles = std::collections::HashMap::new();
    let mut last_examined = None;
    let mut continuation = None;
    'candidates: for scanned in 0..=limit {
        let Some(row) = rows.next().map_err(sql_error)? else {
            break;
        };
        if scanned == limit || ids.len() == args.limit as usize {
            continuation = last_examined;
            break;
        }
        let id: String = row.get(0).map_err(sql_error)?;
        let updated_at: String = row.get(1).map_err(sql_error)?;
        last_examined = Some((Some(updated_at), id.clone()));
        let rowid: i64 = row.get(4).map_err(sql_error)?;
        if filter.state.is_none() && rowid > filter.watermark {
            continue;
        }
        for tag in filter.tags {
            if !tag_probe
                .query_row(params![id, tag], |r| r.get::<_, bool>(0))
                .map_err(sql_error)?
            {
                continue 'candidates;
            }
        }
        if let Some(state) = filter.state {
            let actual: String = state_probe
                .query_row([&id], |r| r.get(0))
                .map_err(sql_error)?;
            if actual != state {
                continue;
            }
        }
        if filter.has_query {
            let stored: String = row.get(2).map_err(sql_error)?;
            let cwd: String = row.get(3).map_err(sql_error)?;
            let title = thread_display_title(connection, &id, &stored)?;
            if !thread_summary::matches_filter(
                &id,
                &title,
                &cwd,
                args.query.as_deref().unwrap_or_default(),
            ) {
                continue;
            }
            titles.insert(id.clone(), title);
        }
        ids.push(id);
    }
    // Heavy summary, observations and excerpt reads run only for admitted IDs.
    let mut by_id: std::collections::HashMap<_, _> = thread_summary::read_ids_tx(connection, &ids)
        .map_err(StorageError::into_api_error)?
        .into_iter()
        .map(|raw| (raw.thread_id.clone(), raw))
        .collect();
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        let raw = by_id.remove(&id).ok_or_else(|| {
            error(
                ErrorCode::InvalidState,
                "Thread disappeared during list read",
            )
        })?;
        let title = match titles.remove(&id) {
            Some(title) => title,
            None => thread_display_title(connection, &id, &raw.stored_title)?,
        };
        items.push(complete_summary(connection, raw, title)?);
    }
    Ok((items, continuation))
}

const EXCERPT_NODES: usize = 32;

fn thread_excerpt(connection: &Connection, thread: &str) -> Result<(String, bool)> {
    let mut suffix = connection
        .prepare_cached(
            "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT ?2",
        )
        .map_err(sql_error)?;
    let mut prose = connection.prepare_cached(
        "SELECT payload_json FROM node WHERE id=?1 AND kind IN ('user','user_message','message','assistant','assistant_response')"
    ).map_err(sql_error)?;
    let mut rows = suffix
        .query(params![thread, EXCERPT_NODES as i64 + 1])
        .map_err(sql_error)?;
    for _ in 0..EXCERPT_NODES {
        let Some(row) = rows.next().map_err(sql_error)? else {
            return Ok((String::new(), true));
        };
        let id: String = row.get(0).map_err(sql_error)?;
        let body: Option<String> = prose
            .query_row([id], |r| r.get(0))
            .optional()
            .map_err(sql_error)?
            .flatten();
        let Some(payload) =
            body.and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        else {
            continue;
        };
        let excerpt = crate::history::conversation_excerpt(&payload, 512);
        if !excerpt.trim().is_empty() {
            return Ok((excerpt, true));
        }
    }
    Ok((String::new(), rows.next().map_err(sql_error)?.is_none()))
}

fn complete_summary(
    connection: &Connection,
    raw: thread_summary::RawSummary,
    title: String,
) -> Result<ThreadSummary> {
    let (excerpt, excerpt_complete) = thread_excerpt(connection, &raw.thread_id)?;
    let mut summary = thread_summary::prepare(raw, title, excerpt);
    summary.excerpt_complete = excerpt_complete;
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
    let raw = thread_summary::read_ids_tx(connection, ids).map_err(StorageError::into_api_error)?;
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
) -> Result<ThreadListPage> {
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
    Ok(RecordPage::new(
        args.limit,
        list_filters(args, false),
        items,
        next_cursor,
    ))
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
                    let exact = thread_summary::reply_count_exact(count.max(0) as u64);
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

    #[test]
    fn search_misses_do_not_read_summary_columns_and_continue_to_a_match() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<450)
            INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)
            SELECT printf('t%06d',i),CASE WHEN i=250 THEN 'An exact Needle' ELSE 'Other' END,'/','','p','m','2026-11-24T00:00:00Z' FROM n;").unwrap();
        db.authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read {
                table_name: "receipt" | "node",
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
        let args = ThreadListArgs {
            query: Some("EXACT needle".into()),
            limit: 1,
            ..Default::default()
        };
        let first = thread_list(&db, &args).unwrap();
        assert!(first.items.is_empty());
        let cursor = first
            .next_cursor
            .expect("more may exist after an empty page");
        db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .unwrap();
        let next = thread_list(
            &db,
            &ThreadListArgs {
                cursor: Some(cursor),
                ..args
            },
        )
        .unwrap();
        assert_eq!(next.items[0].thread_id, "t000250");
    }

    #[test]
    fn tag_pages_and_deep_cwd_pages_do_not_walk_unrelated_candidates() {
        let root = tempfile::tempdir().unwrap();
        let fixture_cwd = |name| {
            normalize(&root.path().join(name), root.path())
                .unwrap()
                .into_os_string()
                .into_string()
                .unwrap()
        };
        let work = fixture_cwd("work");
        let archive = fixture_cwd("archive");
        let measure = |growth| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch(&format!("
                WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<=500+{growth})
                INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at,archived_at)
                  SELECT printf('t%06d',i),'Title','','','p','m','2026-11-24T00:00:00Z',NULL FROM n;
                WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<{growth})
                INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at,archived_at)
                  SELECT printf('z%06d',i),'Archive','','','p','m','2026-11-24T00:00:00Z','archived' FROM n;
                INSERT INTO thread_tag(thread_id,tag) SELECT id,'common' FROM thread;
                INSERT INTO thread_tag(thread_id,tag) VALUES('t000200','rare');")).unwrap();
            db.execute("UPDATE thread SET cwd=?1", [&work]).unwrap();
            let tag_args = ThreadListArgs {
                tags: vec!["common".into(), "rare".into()],
                ..Default::default()
            };
            let tag_page = thread_list(&db, &tag_args).unwrap();
            assert!(tag_page.items.is_empty() && tag_page.next_cursor.is_some());
            // Continue at the same timestamp and ID, with thousands of rows above it.
            let args = ThreadListArgs {
                cwd: Some(work.clone()),
                limit: 1,
                ..Default::default()
            };
            let cursor = PageCursor {
                version: 1,
                scope: list_scope(args.archived).into(),
                filter: list_cursor_filter("", &[], Some(&work)),
                last_sort_key: serde_json::to_string(&(Some("2026-11-24T00:00:00Z"), "t000201"))
                    .unwrap(),
                watermark: 100_000,
                revision: 0,
                encoding: None,
            }
            .encode()
            .unwrap();
            let page = thread_list(
                &db,
                &ThreadListArgs {
                    cursor: Some(cursor),
                    ..args
                },
            )
            .unwrap();
            assert_eq!(page.items[0].thread_id, "t000200");
            // No preceding archive scan even when there are only archived candidates in the cwd.
            db.execute(
                "UPDATE thread SET cwd=?1 WHERE archived_at IS NOT NULL",
                [&archive],
            )
            .unwrap();
            let page = thread_list(
                &db,
                &ThreadListArgs {
                    cwd: Some(archive.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(page.items.is_empty() && page.next_cursor.is_none());
        };
        measure(20);
        measure(5000);
    }

    #[test]
    fn excerpt_distinguishes_exhausted_history_from_an_unread_prefix() {
        let measure = |count| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch(&format!("
                CREATE TEMP TABLE fixture AS WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<{count}) SELECT i,printf('n%06d',i) id FROM n;
                INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','Title','/','','p','m','now');
                INSERT INTO node(id,kind,created_at,payload_json) SELECT id,'tool_result','now','{{}}' FROM fixture;
                INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT 't',i,id FROM fixture;")).unwrap();
            let (excerpt, complete) = thread_excerpt(&db, "t").unwrap();
            assert!(excerpt.is_empty());
            assert_eq!(complete, count <= EXCERPT_NODES);
            let page = thread_list(
                &db,
                &ThreadListArgs {
                    thread_ids: Some(vec!["t".into()]),
                    ..Default::default()
                },
            )
            .unwrap();
            let row = serde_json::to_value(&page.items[0]).unwrap();
            if complete {
                assert!(row.get("excerpt_complete").is_none());
            } else {
                assert_eq!(row["excerpt_complete"], false);
            }
        };
        measure(EXCERPT_NODES);
        measure(EXCERPT_NODES + 1);
        measure(5000);
    }

    #[test]
    fn sparse_state_page_continues_without_scanning_all_threads() {
        let measure = |num_threads| {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch(&format!("
                WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<{num_threads})
                INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)
                SELECT printf('t%06d',i),'Title','/','','p','m','2026-11-24T00:00:00Z' FROM n;
                INSERT INTO run(id,thread_id,state,created_at)
                SELECT 'r'||id,id,'failed',updated_at FROM thread WHERE id=printf('t%06d',{num_threads}-200);
            ")).unwrap();
            let args = ThreadListArgs {
                state: Some(crate::protocol::ThreadListState::Failed),
                archived: ArchivedFilter::Include,
                limit: 1,
                ..Default::default()
            };
            let first = thread_list(&db, &args).unwrap();
            assert!(first.items.is_empty());
            let cursor = first.next_cursor.expect("unexamined candidates remain");
            let second = thread_list(
                &db,
                &ThreadListArgs {
                    cursor: Some(cursor),
                    ..args
                },
            )
            .unwrap();
            assert_eq!(
                second.items[0].thread_id,
                format!("t{:06}", num_threads - 200)
            );
            let sql = thread_summary::updated_candidate_sql(ArchivedFilter::Include, true, false);
            let mut statement = db.prepare(&sql).unwrap();
            let rows = statement
                .query_map(
                    params![
                        201,
                        "2026-11-24T00:00:00Z",
                        format!("t{:06}", num_threads - 199)
                    ],
                    |_| Ok(()),
                )
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows.len(), 201);
        };
        measure(450);
        measure(8000);
    }
}
