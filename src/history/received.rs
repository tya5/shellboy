//! Bounded keyset scan over one Thread's Turns and each Turn's node IDs.
use crate::{
    protocol::{
        ApiError, ErrorCode, HistoryReceivedArgs, HistoryReceivedPage, HistoryReceivedRow,
        PageCursor,
    },
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const TURNS: usize = 32;
const NODES: usize = 200;
const ROWS: usize = 32;
const SOURCE_BYTES: i64 = 64 * 1024;
const SCOPE: &str = "history.received";

#[derive(Clone, Serialize, Deserialize)]
struct Position {
    turn_at: String,
    turn_rowid: i64,
    turn_id: String,
    node_id: Option<String>,
}

fn expired() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "Invalid received history position",
    )
}

fn encode(thread: &str, position: &Position) -> Result<String, ApiError> {
    PageCursor {
        version: 1,
        scope: SCOPE.into(),
        filter: thread.into(),
        last_sort_key: serde_json::to_string(position).map_err(|_| expired())?,
        watermark: 0,
        revision: 0,
        encoding: None,
    }
    .encode()
}

struct Turn {
    id: String,
    position: Position,
    state: String,
}

fn turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    let id: String = row.get(0)?;
    Ok(Turn {
        id: id.clone(),
        position: Position {
            turn_at: row.get(1)?,
            turn_rowid: row.get(2)?,
            turn_id: id,
            node_id: None,
        },
        state: row.get(3)?,
    })
}

pub(crate) fn read(
    db: &Connection,
    args: &HistoryReceivedArgs,
) -> Result<HistoryReceivedPage, StorageError> {
    let thread = &args.thread_id;
    let live: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [thread],
        |r| r.get(0),
    )?;
    if !live {
        return Err(
            ApiError::new(ErrorCode::TargetUnavailable, "Saved Thread is unavailable").into(),
        );
    }
    let position = args
        .cursor
        .as_ref()
        .map(|token| {
            let cursor = PageCursor::decode(token, SCOPE, thread, 0, None)?;
            let position: Position =
                serde_json::from_str(&cursor.last_sort_key).map_err(|_| expired())?;
            if position.turn_at.len() > 128
                || position.turn_id.len() > 512
                || position.node_id.as_ref().is_some_and(|id| id.len() > 512)
            {
                return Err(expired());
            }
            Ok::<_, ApiError>(position)
        })
        .transpose()?;
    // Split the timestamp tie and older ranges so both are indexed seeks,
    // rather than asking SQLite to filter a disjunction over the Thread.
    const COLUMNS: &str = "SELECT CASE WHEN octet_length(id)<=512 THEN id END,CASE WHEN octet_length(created_at)<=128 THEN created_at END,rowid,state FROM turn INDEXED BY turns_by_thread WHERE thread_id=?1";
    let mut turns = Vec::new();
    if let Some(p) = &position {
        let ties = format!("{COLUMNS} AND created_at=?2 AND rowid<?3 ORDER BY rowid DESC LIMIT ?4");
        // A partial Turn is resumed only when its exact row still belongs to this Thread.
        if p.node_id.is_some() {
            let sql = format!("{COLUMNS} AND created_at=?2 AND rowid=?3");
            let current = db
                .query_row(&sql, params![thread, p.turn_at, p.turn_rowid], turn)
                .optional()?
                .ok_or_else(expired)?;
            if current.id != p.turn_id {
                return Err(expired().into());
            }
            turns.push(current);
        }
        let budget = TURNS + 1 - turns.len();
        turns.extend(
            db.prepare(&ties)?
                .query_map(
                    params![thread, p.turn_at, p.turn_rowid, budget as i64],
                    turn,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        if turns.len() <= TURNS {
            let older =
                format!("{COLUMNS} AND created_at<?2 ORDER BY created_at DESC,rowid DESC LIMIT ?3");
            let budget = TURNS + 1 - turns.len();
            turns.extend(
                db.prepare(&older)?
                    .query_map(params![thread, p.turn_at, budget as i64], turn)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
    } else {
        let sql = format!("{COLUMNS} ORDER BY created_at DESC,rowid DESC LIMIT ?2");
        turns = db
            .prepare(&sql)?
            .query_map(params![thread, (TURNS + 1) as i64], turn)?
            .collect::<rusqlite::Result<_>>()?;
    }
    let mut result = HistoryReceivedPage {
        items: Vec::new(),
        next_cursor: None,
    };
    let mut nodes_left = NODES;
    let mut last = None;
    'turns: for (index, mut turn) in turns.into_iter().enumerate() {
        if index == TURNS || nodes_left == 0 || result.items.len() == ROWS {
            result.next_cursor = last.as_ref().map(|p| encode(thread, p)).transpose()?;
            break;
        }
        let lower = position
            .as_ref()
            .filter(|p| p.turn_rowid == turn.position.turn_rowid)
            .and_then(|p| p.node_id.as_deref());
        const NODE: &str = "SELECT CASE WHEN octet_length(id)<=512 THEN id END,kind IN ('incomplete_response','model_stream_observation') AND deleted_reason IS NULL FROM node INDEXED BY node_turn WHERE turn_id=?1";
        let candidates = if let Some(lower) = lower {
            db.prepare(&format!("{NODE} AND id>?2 ORDER BY id LIMIT ?3"))?
                .query_map(params![turn.id, lower, (nodes_left + 1) as i64], candidate)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            db.prepare(&format!("{NODE} ORDER BY id LIMIT ?2"))?
                .query_map(params![turn.id, (nodes_left + 1) as i64], candidate)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, is_received) in candidates {
            if nodes_left == 0 || result.items.len() == ROWS {
                result.next_cursor = last.as_ref().map(|p| encode(thread, p)).transpose()?;
                break 'turns;
            }
            nodes_left -= 1;
            if is_received {
                let (kind, at, excerpt, excerpt_limited): (String, String, Option<String>, bool) = db.query_row(
                    "SELECT kind,CASE WHEN octet_length(created_at)<=128 THEN created_at ELSE '' END,CASE WHEN octet_length(payload_json)<=?2 THEN substr(json_extract(payload_json,'$.partial_text'),1,80) END,COALESCE(octet_length(payload_json)>?2,0) FROM node WHERE id=?1", params![id,SOURCE_BYTES],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                let (follows_node_id, differs_from_final) =
                    placement(db, thread, &turn.id, &id, &kind)?;
                result.items.push(HistoryReceivedRow {
                    node_id: id.clone(),
                    turn_id: turn.id.clone(),
                    kind,
                    at,
                    state: turn.state.clone(),
                    request: None,
                    request_limited: false,
                    excerpt,
                    excerpt_limited,
                    follows_node_id,
                    differs_from_final,
                });
            }
            turn.position.node_id = Some(id);
            last = Some(turn.position.clone());
        }
        turn.position.node_id = None;
        last = Some(turn.position);
    }
    populate_requests(db, thread, &mut result.items)?;
    Ok(result)
}

/// Saved payloads compared with their final response are read up to this size;
/// a larger stream difference is not compared and reports no difference.
const COMPARED_BYTES: i64 = 1024 * 1024;

/// Where a received response sits and, for a stream difference, whether it
/// differs from the final response saved with it.
fn placement(
    db: &Connection,
    thread: &str,
    turn: &str,
    node: &str,
    kind: &str,
) -> rusqlite::Result<(Option<String>, Option<bool>)> {
    // The Turn's own nodes are the bounded side; the path is joined by node.
    let follows = db
        .query_row(
            "SELECT CASE WHEN octet_length(p.node_id)<=512 THEN p.node_id END
               FROM node prior INDEXED BY node_turn
               JOIN thread_path p ON p.thread_id=?1 AND p.node_id=prior.id
              WHERE prior.turn_id=?2 AND prior.rowid<(SELECT rowid FROM node WHERE id=?3)
              ORDER BY p.ordinal DESC LIMIT 1",
            params![thread, turn, node],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let differs = if kind == "model_stream_observation" {
        let payload: Option<String> = db
            .query_row(
                "SELECT CASE WHEN octet_length(payload_json)<=?2 THEN payload_json END FROM node WHERE id=?1",
                params![node, COMPARED_BYTES],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Some(
            payload
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .is_some_and(|payload| differs_from_final(&payload)),
        )
    } else {
        None
    };
    Ok((follows, differs))
}

/// Mismatch classifications describe JSON structure as well as prose. A
/// metadata/content/call-id difference alone must not duplicate a paid answer.
fn differs_from_final(payload: &serde_json::Value) -> bool {
    use super::display_source::received_body as body;
    let Some(final_response) = payload.get("raw_response") else {
        return false;
    };
    if !final_response
        .get("output")
        .is_some_and(serde_json::Value::is_array)
    {
        return false;
    }
    body(payload) != body(&serde_json::json!({"raw_response": final_response}))
}

fn candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, bool)> {
    Ok((row.get(0)?, row.get(1)?))
}

fn populate_requests(
    db: &Connection,
    thread: &str,
    items: &mut [HistoryReceivedRow],
) -> rusqlite::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    // Merge the four ordered ranges of the existing covering index. Fetch
    // receipt rows only for the global chronological prefix, not its sentinel.
    let keys = db.prepare(r#"
        SELECT created_at, rowid AS receipt_row FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='accepted'
        UNION ALL SELECT created_at, rowid FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='held'
        UNION ALL SELECT created_at, rowid FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='applied'
        UNION ALL SELECT created_at, rowid FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='cancelled'
        ORDER BY created_at, receipt_row LIMIT 129
    "#)?
        .query_map([thread], |r| r.get::<_, i64>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = keys.len() > 128;
    let mut receipt = db.prepare("SELECT CASE WHEN octet_length(turn_id)<=512 THEN turn_id END, CASE WHEN octet_length(message_node_id)<=512 THEN message_node_id END FROM receipt WHERE rowid=?1")?;
    let mut excerpts = items
        .iter()
        .map(|row| (row.turn_id.clone(), None))
        .collect::<BTreeMap<_, _>>();
    for key in keys.into_iter().take(128) {
        let (turn, message): (Option<String>, Option<String>) =
            receipt.query_row([key], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if let Some(excerpt @ None) = turn.and_then(|turn| excerpts.get_mut(&turn)) {
            // A known tombstone is skipped. If the earlier identity cannot be
            // read, do not substitute a later request for this Turn.
            *excerpt = match message {
                Some(message) => request_excerpt(db, &message)?,
                None => Some((None, true)),
            };
        }
    }
    for row in items {
        if let Some(Some((excerpt, limited))) = excerpts.get(&row.turn_id) {
            row.request.clone_from(excerpt);
            row.request_limited = *limited;
        } else {
            row.request_limited = has_more;
        }
    }
    Ok(())
}

fn request_excerpt(
    db: &Connection,
    node: &str,
) -> rusqlite::Result<Option<(Option<String>, bool)>> {
    db.query_row(r#"SELECT CASE WHEN octet_length(payload_json)<=?2 THEN
      substr(coalesce((SELECT json_extract(value,'$.text') FROM json_each(payload_json,'$.content')
        WHERE CASE WHEN type='object' THEN json_extract(value,'$.type') IN ('input_text','text')
        AND json_type(value,'$.text')='text' END LIMIT 1),
        CASE WHEN json_type(payload_json,'$.text')='text' THEN json_extract(payload_json,'$.text') END),1,80) END,
      COALESCE(octet_length(payload_json)>?2,0)
      FROM node WHERE id=?1 AND deleted_reason IS NULL"#,
        params![node, SOURCE_BYTES], |r| Ok((r.get(0)?, r.get(1)?))).optional()
}

#[cfg(test)]
mod tests {
    use super::differs_from_final;
    use crate::history::display_source::received_body as body;
    use serde_json::json;

    #[test]
    fn paid_channels_choose_one_source_and_never_show_tool_or_encrypted_metadata() {
        let payload = json!({"partial_reasoning":"考え","partial_text":"回答","raw_response":{"output":[{"type":"message","content":[{"type":"output_text","text":"回答"}]},{"type":"function_call","arguments":"PRIVATE_TOOL"},{"type":"reasoning","encrypted_content":"PRIVATE_OPAQUE","summary":[{"type":"summary_text","text":"考え"}]}]}});
        assert_eq!(body(&payload), ("考え\n回答".into(), vec![(0, 2)]));
        let mut fallback = payload;
        fallback
            .as_object_mut()
            .unwrap()
            .remove("partial_reasoning");
        fallback.as_object_mut().unwrap().remove("partial_text");
        assert_eq!(body(&fallback), ("考え\n回答".into(), vec![(0, 2)]));
        let mut differing = json!({"raw_response":{"output":[{"type":"message","content":[{"type":"output_text","text":"最終"}]}]},"completed_items":{"0":{"type":"message","content":[{"type":"output_text","text":"途中"}]}}});
        assert_eq!(body(&differing).0, "最終");
        differing["classification"] = json!("stream_difference");
        assert_eq!(body(&differing).0, "途中");
        // Without a comparable final response nothing differs.
        assert!(!differs_from_final(
            &json!({"differences":[{"field":"metadata"}]})
        ));
        assert!(differs_from_final(
            &json!({"partial_text":"paid differing text","raw_response":{"output":[]}})
        ));
        assert!(differs_from_final(
            &json!({"partial_text":"paid delta","raw_response":{"output":[]},"differences":[{"field":"metadata"}],"delta_differences":["text"]})
        ));
        assert_eq!(body(&json!({"completed_items":{"10":{"type":"message","content":[{"type":"refusal","refusal":"後"}]},"2":{"type":"message","content":[{"type":"output_text","text":"先"}]}}})).0,"先\n後");
    }

    #[test]
    fn structural_differences_alone_do_not_duplicate_the_final_answer() {
        let message =
            json!({"type":"message","content":[{"type":"output_text","text":"same answer"}]});
        let mut payload = json!({"partial_text":"same answer","completed_items":{"0":message},"raw_response":{"output":[message]},"differences":[{"field":"content"},{"field":"call_id"},{"field":"arguments"}],"delta_differences":[]});
        assert!(!differs_from_final(&payload));
        payload["partial_text"] = json!("different paid answer");
        assert!(differs_from_final(&payload));
        let reason = json!({"partial_reasoning":"paid reasoning","partial_text":"same answer","raw_response":{"output":[message,{"type":"reasoning","summary":[{"type":"summary_text","text":"final reasoning"}]}]}});
        assert!(differs_from_final(&reason));
    }

    #[test]
    fn done_only_prose_and_reasoning_are_compared_to_final_channels() {
        let message =
            |text| json!({"type":"message","content":[{"type":"output_text","text":text}]});
        let reasoning =
            |text| json!({"type":"reasoning","summary":[{"type":"summary_text","text":text}]});
        let mut payload = json!({"classification":"stream_difference","partial_text":"","partial_reasoning":"","completed_items":{"0":message("done answer"),"1":reasoning("done reasoning")},"raw_response":{"output":[message("final answer"),reasoning("final reasoning")]}});
        assert_eq!(body(&payload).0, "done reasoning\ndone answer");
        assert!(differs_from_final(&payload));
        payload["raw_response"]["output"] =
            json!([message("done answer"), reasoning("final reasoning")]);
        assert!(
            differs_from_final(&payload),
            "reasoning-only difference remains visible"
        );
        payload["raw_response"]["output"] =
            json!([message("done answer"), reasoning("done reasoning")]);
        assert!(!differs_from_final(&payload));
    }
}
