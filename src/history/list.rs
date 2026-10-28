//! Selected-path list filters, cursor binding and row projection.
use super::*;
use sha2::{Digest, Sha256};

pub fn list_nodes(
    connection: &Connection,
    args: &HistoryListArgs,
) -> Result<Page<HistorySnippet>, ApiError> {
    let invalid = |message| ApiError::new(ErrorCode::InvalidArgument, message);
    if args.limit == 0 || args.limit > MAX_LIST_LIMIT {
        return Err(invalid("history limit must be in 1..=200"));
    }
    if args.run_id.as_deref().is_some_and(str::is_empty) {
        return Err(invalid("run_id must not be empty"));
    }
    args.validate_excerpt()?;
    args.validate_state()?;
    let since = args.parsed_since()?;
    let (revision, current_cwd): (u64, String) = connection
        .query_row(
            "SELECT selection_revision,cwd FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [&args.thread_id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"))?;
    let count: u64 = connection
        .query_row(
            "SELECT count(*) FROM thread_path WHERE thread_id=?1",
            [&args.thread_id],
            |row| row.get::<_, i64>(0).map(|n| n as u64),
        )
        .map_err(sql_error)?;
    let filter = list_filter(connection, args, since.as_ref())?;
    let (start, watermark) = list_position(args, &filter, revision, count)?;
    let relation = EXECUTION_RELATION.replace("?1", "n.id");
    let sql = format!(
        "SELECT n.id,n.kind,CASE WHEN ?6=0 THEN NULL ELSE n.payload_json END,n.deleted_reason,p.ordinal,n.created_at,length(n.payload_json) FROM thread_path p JOIN node n ON n.id=p.node_id LEFT JOIN turn u ON u.id=n.turn_id WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<?3 AND (?4 IS NULL OR u.run_id=?4) AND (?5 IS NULL OR EXISTS(SELECT 1 {relation} AND s.state=?5)) ORDER BY p.ordinal"
    );
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            args.thread_id,
            start as i64,
            watermark as i64,
            args.run_id,
            args.state,
            args.excerpt_chars
        ])
        .map_err(sql_error)?;
    let mut items = Vec::new();
    let mut next = start;
    let mut more = false;
    while let Some(row) = rows.next().map_err(sql_error)? {
        if let Some(since) = &since {
            let created: String = row.get(5).map_err(sql_error)?;
            let created = crate::protocol::rfc3339(&created, || {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "Stored node creation time is not RFC3339",
                )
            })?;
            if created < *since {
                continue;
            }
        }
        if items.len() == args.limit as usize {
            more = true;
            break;
        }
        let (item, ordinal) = history_snippet(
            connection,
            row,
            &args.thread_id,
            &current_cwd,
            args.excerpt_chars,
        )?;
        items.push(item);
        next = ordinal + 1;
    }
    let next_cursor = if more {
        Some(
            PageCursor {
                version: 1,
                scope: "history.list".into(),
                filter,
                last_sort_key: next.to_string(),
                watermark,
                revision,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

fn list_filter(
    connection: &Connection,
    args: &HistoryListArgs,
    since: Option<&chrono::DateTime<chrono::FixedOffset>>,
) -> Result<String, ApiError> {
    let invalid = |message| ApiError::new(ErrorCode::InvalidArgument, message);
    // Shell states can change without a selected-path revision. Bind filtered
    // cursors to those saved states, never to a stale receipt's old projection.
    let state_fingerprint = if args.state.is_some() {
        let mut digest = Sha256::new();
        let mut statement=connection.prepare("SELECT s.id,s.state FROM shell_execution s JOIN tool_call c ON c.operation_id=s.id JOIN tool_group g ON g.id=c.group_id JOIN turn u ON u.id=g.turn_id AND u.run_id=s.owner_run_id AND u.id=s.turn_id WHERE EXISTS(SELECT 1 FROM output_item o JOIN thread_path p ON p.node_id=o.node_id WHERE o.attempt_id=g.attempt_id AND p.thread_id=?1) OR EXISTS(SELECT 1 FROM tool_result tr JOIN thread_path p ON p.node_id=tr.node_id WHERE tr.group_id=c.group_id AND tr.call_id=c.call_id AND p.thread_id=?1) ORDER BY s.id").map_err(sql_error)?;
        let mut rows = statement.query([&args.thread_id]).map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            digest.update(row.get::<_, String>(0).map_err(sql_error)?.as_bytes());
            digest.update([0]);
            digest.update(row.get::<_, String>(1).map_err(sql_error)?.as_bytes());
            digest.update([0]);
        }
        Some(
            digest
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        )
    } else {
        None
    };
    let filter = if args.run_id.is_none() && args.since.is_none() && args.state.is_none() {
        args.thread_id.clone()
    } else {
        serde_json::to_string(&(
            args.thread_id.as_str(),
            &args.run_id,
            since.map(|t| t.to_rfc3339()),
            &args.state,
            state_fingerprint,
        ))
        .map_err(|_| invalid("Cannot encode history filters"))?
    };
    if args.excerpt_chars == crate::protocol::DEFAULT_HISTORY_EXCERPT_CHARS {
        Ok(filter)
    } else {
        serde_json::to_string(&(filter, "excerpt_chars", args.excerpt_chars))
            .map_err(|_| invalid("Cannot encode history excerpt limit"))
    }
}
fn list_position(
    args: &HistoryListArgs,
    filter: &str,
    revision: u64,
    count: u64,
) -> Result<(u64, u64), ApiError> {
    let (start, watermark) = if let Some(token) = &args.cursor {
        let cursor = PageCursor::decode(token, "history.list", filter, revision, None)?;
        (
            cursor
                .last_sort_key
                .parse::<u64>()
                .map_err(|_| ApiError::new(ErrorCode::CursorExpired, "invalid history position"))?,
            cursor.watermark,
        )
    } else {
        (0, count)
    };
    if start > watermark || watermark > count {
        return Err(ApiError::new(
            ErrorCode::CursorExpired,
            "invalid history watermark",
        ));
    }
    Ok((start, watermark))
}
fn history_snippet(
    connection: &Connection,
    row: &rusqlite::Row<'_>,
    thread_id: &str,
    current_cwd: &str,
    excerpt_chars: u32,
) -> Result<(HistorySnippet, u64), ApiError> {
    let node_id: String = row.get(0).map_err(sql_error)?;
    let body: Option<String> = row.get(2).map_err(sql_error)?;
    let excerpt = if excerpt_chars == 0 {
        String::new()
    } else if let Some(body) = body {
        let value: Value = serde_json::from_str(&body).map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "stored history payload is malformed",
            )
        })?;
        text_of_payload(&value)
            .chars()
            .take(excerpt_chars as usize)
            .collect()
    } else {
        format!(
            "[{}]",
            row.get::<_, Option<String>>(3)
                .map_err(sql_error)?
                .unwrap_or_else(|| "body unavailable".into())
        )
        .chars()
        .take(excerpt_chars as usize)
        .collect()
    };
    let ordinal = row.get::<_, i64>(4).map_err(sql_error)? as u64;
    let (executions, execution_count) = execution_links(connection, &node_id)?;
    let item = HistorySnippet {
        payload_chars: row
            .get::<_, Option<i64>>(6)
            .map_err(sql_error)?
            .map(|n| n as u64),
        provenance: Some(provenance(connection, &node_id)?),
        context: Some(crate::context::for_node(connection, &node_id)?),
        selection: Some(HistorySelection {
            thread_id: thread_id.into(),
            ordinal,
            current_cwd: current_cwd.into(),
        }),
        executions_truncated: execution_count > executions.len() as u64,
        execution_count,
        executions,
        node_id,
        kind: row.get(1).map_err(sql_error)?,
        excerpt,
    };
    Ok((item, ordinal))
}
