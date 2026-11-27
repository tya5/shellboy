//! Optional selected-window facts read on the existing owner inspection lane.
use super::*;
use crate::control::message_delivery::MessageHolds;
use std::sync::atomic::{AtomicU64, Ordering};

static READ_SEQUENCE: AtomicU64 = AtomicU64::new(1);
pub(super) fn next_read_sequence() -> u64 {
    READ_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}
const PENDING_LIMIT: usize = 128;

impl Store {
    pub(in crate::control) async fn observe_history_window(
        &self,
        args: HistoryWindowArgs,
        holds: MessageHolds,
        operation: &str,
        shell: bool,
    ) -> Result<MutationResult> {
        if args.before.saturating_add(args.after).saturating_add(1) as usize
            > crate::history::window_facts::WINDOW_FACTS_LIMIT
        {
            return Err(error(
                ErrorCode::InputLimit,
                "Selected observation exceeds its 128-node window bound",
            ));
        }
        let operation = operation.to_owned();
        let observer = self.database.clone();
        let queued_at = std::time::Instant::now();
        inspection_work(move || {
            let mut db = open_inspection(observer.path())?;
            let window = observer.observe_inspection(
                &mut db,
                "HistoryWindow",
                queued_at,
                Some(if shell { "shell" } else { "external" }),
                |db| {
                    let tx = db.transaction().map_err(sql_error)?;
                    let sequence = next_read_sequence();
                    let mut window = history::window(&tx, &args)?;
                    window.observation = Some(observe(
                        &tx,
                        &window,
                        &args.observe_node_ids,
                        &holds,
                        sequence,
                    )?);
                    tx.commit().map_err(sql_error)?;
                    Ok::<_, ApiError>(window)
                },
            )?;
            let value = serde_json::to_value(window).map_err(|_| {
                error(
                    ErrorCode::InvalidState,
                    "History observation serialization failed",
                )
            })?;
            let value = history::rpc::finish("history.window", &operation, value)?;
            if !history::fits_response_frame(&operation, &value).map_err(|_| {
                error(
                    ErrorCode::InvalidState,
                    "History observation serialization failed",
                )
            })? {
                return Err(error(
                    ErrorCode::InputLimit,
                    "Selected observation exceeds the frame limit; request a smaller window",
                ));
            }
            MutationResult::value(value)
        })
        .await?
    }
}

impl Store {
    /// `message.get` with its live hold, read on the inspection lane.
    pub(in crate::control) async fn observe_message(
        &self,
        args: crate::protocol::MessageTarget,
        holds: MessageHolds,
    ) -> Result<MutationResult> {
        let observer = self.database.clone();
        let queued_at = std::time::Instant::now();
        let limit = self.config.limits.draft_bytes;
        let id = args.message_id;
        let detail = inspection_work(move || {
            let mut db = open_inspection(observer.path())?;
            observer.observe_inspection(&mut db, "MessageGet", queued_at, None, |db| {
                let tx = db.transaction().map_err(sql_error)?;
                use crate::protocol::MessageGetResult;
                let detail = if let Some(thread) = args.source_thread_id.as_deref() {
                    MessageGetResult::Source(message_source(&tx, thread, &id, limit)?)
                } else {
                    let mut detail = message_details(&tx, &id)?;
                    detail.held_by = holds.read(&tx, [&*id]).map_err(sql_error)?.pop().flatten();
                    MessageGetResult::Details(Box::new(detail))
                };
                tx.commit().map_err(sql_error)?;
                Ok::<_, ApiError>(detail)
            })
        })
        .await??;
        MutationResult::value(detail)
    }
}

fn observe(
    db: &Connection,
    window: &HistoryWindow,
    extra: &[String],
    holds: &MessageHolds,
    read_sequence: u64,
) -> Result<HistoryWindowObservation> {
    let committed_attempts = history::committed_attempt_bindings(
        db,
        window
            .items
            .iter()
            .filter(|item| item.kind == "assistant_response")
            .map(|item| item.node_id.as_str()),
    )
    .map_err(sql_error)?
    .into_iter()
    .map(|(attempt_id, node_id)| HistoryWindowCommittedAttempt {
        attempt_id,
        node_id,
    })
    .collect();
    let items = window
        .items
        .iter()
        .map(|item| (item.node_id.clone(), item.kind.clone()))
        .collect::<Vec<_>>();
    let facts = crate::history::window_facts::read_window_facts(db, &items, extra)
        .map_err(StorageError::into_api_error)?;
    let received_root_revision = db
        .query_row(
            "SELECT value FROM app_meta WHERE key='root_revision'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_error)?
        .parse::<i64>()
        .ok()
        .and_then(|revision| u64::try_from(revision).ok())
        .ok_or_else(|| {
            StorageError::Schema("root revision metadata is malformed".into()).into_api_error()
        })?;
    let rows = db.prepare_cached(
        "SELECT n.id,n.kind,(SELECT p.node_id FROM thread_path p JOIN node prior ON prior.id=p.node_id
           WHERE p.thread_id=u.thread_id AND prior.turn_id=n.turn_id AND prior.rowid<n.rowid
           ORDER BY p.ordinal DESC LIMIT 1),u.state='recovery_wait',n.payload_revision,
           CASE WHEN octet_length(n.payload_json)<=1048576 THEN n.payload_json END
         FROM node n JOIN turn u ON u.id=n.turn_id
         WHERE u.thread_id=?1 AND n.kind IN ('incomplete_response','model_stream_observation')
           AND n.deleted_reason IS NULL AND json_valid(n.payload_json)
         ORDER BY n.rowid DESC LIMIT 2",
    ).map_err(sql_error)?.query_map([&window.thread_id], |row| {
        Ok((crate::protocol::HistoryWindowReceived {
            node_id: row.get(0)?, kind: row.get(1)?, after_node_id: row.get(2)?, recovery_wait: row.get(3)?, source_revision: row.get(4)?,
        }, row.get::<_, Option<String>>(5)?))
    }).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    let received = rows
        .into_iter()
        .filter_map(|(notice, payload)| {
            if let Some(payload) = payload {
                let value: Value = serde_json::from_str(&payload).ok()?;
                let (text, _) = crate::history::display_source::received_body(&value);
                if !crate::history::display_source::received_preview_visible(
                    &notice.kind,
                    &value,
                    &text,
                ) {
                    return None;
                }
            } else if notice.kind == "model_stream_observation" {
                return None;
            }
            Some(notice)
        })
        .collect();
    let source_ids = extra
        .iter()
        .chain(window.items.iter().map(|item| &item.node_id))
        .cloned()
        .collect::<Vec<_>>();
    let compactions =
        crate::history::window_facts::compactions::facts(db, &window.thread_id, &items)
            .map_err(StorageError::into_api_error)?;
    let ids = items.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
    let branches = crate::history::branches::markers(db, &window.thread_id, &ids)?
        .into_iter()
        .map(|marker| HistoryWindowBranch {
            anchor_node_id: marker.anchor_node_id,
            current: marker.current,
            alternatives: marker
                .alternatives
                .into_iter()
                .map(|alternative| HistoryWindowBranchAlternative {
                    thread_id: alternative.thread_id,
                    excerpt: alternative.excerpt,
                })
                .collect(),
        })
        .collect();
    Ok(HistoryWindowObservation {
        read_sequence,
        observed_at: chrono::Utc::now().to_rfc3339(),
        pending: pending(db, &window.thread_id, holds).map_err(sql_error)?,
        committed_attempts,
        activity: activity(db, &window.thread_id)?,
        tools: facts.tools,
        recovery: facts.recovery,
        usage: facts.usage,
        received,
        received_root_revision,
        branches,
        compactions,
        pinned_sources: crate::history::window_facts::message_sources::pinned(
            db,
            &window.thread_id,
            &source_ids,
        )
        .map_err(StorageError::into_api_error)?,
    })
}

fn pending(
    db: &Connection,
    thread: &str,
    holds: &MessageHolds,
) -> rusqlite::Result<HistoryWindowPending> {
    // Seek each status separately using the receipt index. Filter after the
    // bounded raw prefix so admitted receipts cannot turn this into a scan.
    let mut rows = db.prepare_cached(
        "WITH candidates AS MATERIALIZED (
             SELECT rowid, message_node_id, client_operation_id, created_at, path_admission
               FROM receipt INDEXED BY receipt_target
              WHERE target_thread_id=?1 AND state=?2 ORDER BY created_at,rowid LIMIT ?3
         )
         SELECT rowid, message_node_id, client_operation_id, created_at,
                path_admission<>'admitted' AND NOT EXISTS(
                    SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=message_node_id
                ) FROM candidates ORDER BY created_at,rowid",
    )?;
    let mut candidates = Vec::with_capacity(PENDING_LIMIT * 2);
    let mut first_unproven: Option<(String, i64)> = None;
    for (state, delivery_status) in [
        ("accepted", DeliveryStatus::Accepted),
        ("held", DeliveryStatus::Held),
    ] {
        let mut result = rows.query(params![thread, state, (PENDING_LIMIT + 1) as i64])?;
        let mut examined = 0;
        while let Some(row) = result.next()? {
            examined += 1;
            let key = (row.get::<_, String>(3)?, row.get::<_, i64>(0)?);
            if examined > PENDING_LIMIT && first_unproven.as_ref().is_none_or(|bound| &key < bound)
            {
                first_unproven = Some(key.clone());
            }
            if row.get::<_, bool>(4)? {
                candidates.push((
                    key,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    delivery_status,
                ));
            }
        }
    }
    candidates.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let total = candidates.len() as u64;
    // Only hydrate the proved prefix, not either raw state scan or its lookahead.
    let mut display = db.prepare_cached(
        "SELECT
        COALESCE((SELECT substr(group_concat(fragment,''),1,513) FROM
            (SELECT substr(json_extract(j.value,'$.text'),1,513) AS fragment
               FROM json_each(n.payload_json,'$.content') j
              WHERE json_extract(j.value,'$.type') IN ('input_text','output_text') LIMIT 128)),''),
        (SELECT count(*) FROM attachment_ref a WHERE a.source_node_id=n.id)
        FROM node n WHERE n.id=?1",
    )?;
    // Saved receipt facts the queue row shows; the hold itself is `held_by`.
    let mut receipt = db.prepare_cached(
        "SELECT substr(sender_json,1,8193),
            COALESCE(json_extract(result_json,'$.reply.state')='pending',0),
            path_admission='unknown',
            COALESCE(json_extract(result_json,'$.deliver')='complete',0)
         FROM receipt WHERE rowid=?1",
    )?;
    let mut items = Vec::with_capacity(PENDING_LIMIT.min(candidates.len()));
    for ((created_at, rowid), message_id, operation_id, delivery_status) in candidates
        .into_iter()
        .take_while(|(key, ..)| first_unproven.as_ref().is_none_or(|bound| key < bound))
        .take(PENDING_LIMIT)
    {
        let (excerpt, attachment_count): (String, i64) =
            display.query_row([&message_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let (sender, expect_reply, admission_uncertain, completion_scheduled) =
            receipt.query_row([rowid], |row| {
                Ok((
                    crate::protocol::MessageSender::from_saved(
                        row.get::<_, Option<String>>(0)?.as_deref(),
                    ),
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            })?;
        items.push(HistoryWindowPendingMessage {
            message_id,
            operation_id,
            delivery_status,
            sender,
            expect_reply,
            admission_uncertain,
            completion_scheduled,
            created_at,
            excerpt_truncated: excerpt.chars().count() > 512,
            excerpt: excerpt.chars().take(512).collect(),
            attachment_count: attachment_count as u64,
            held_by: None,
        });
    }
    let blockers = holds.read(db, items.iter().map(|item| item.message_id.as_str()))?;
    for (item, held_by) in items.iter_mut().zip(blockers) {
        item.held_by = held_by;
    }
    Ok(HistoryWindowPending {
        items,
        total,
        total_exact: first_unproven.is_none(),
    })
}

fn activity(db: &Connection, thread: &str) -> Result<HistoryWindowActivity> {
    let Some(activity) =
        super::thread_activity::read(db, thread).map_err(StorageError::into_api_error)?
    else {
        return Ok(HistoryWindowActivity::default());
    };
    Ok(HistoryWindowActivity {
        turn_id: activity.turn.map(|turn| turn.turn_id),
        attempt_id: activity.current_attempt.map(|attempt| attempt.attempt_id),
        waiting_reason: activity.run.waiting_reason,
        run_id: Some(activity.run.run_id),
    })
}
