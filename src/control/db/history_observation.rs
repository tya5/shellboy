//! Optional selected-window facts read on the existing owner inspection lane.
use super::*;
use crate::control::message_delivery::MessageHolds;
use std::sync::atomic::{AtomicU64, Ordering};

static READ_SEQUENCE: AtomicU64 = AtomicU64::new(1);
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
            > crate::frontend::WINDOW_FACTS_LIMIT
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
                    let sequence = READ_SEQUENCE.fetch_add(1, Ordering::Relaxed);
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
        id: String,
        holds: MessageHolds,
    ) -> Result<MutationResult> {
        let observer = self.database.clone();
        let queued_at = std::time::Instant::now();
        let detail = inspection_work(move || {
            let mut db = open_inspection(observer.path())?;
            observer.observe_inspection(&mut db, "MessageGet", queued_at, None, |db| {
                let tx = db.transaction().map_err(sql_error)?;
                let mut detail = message_details(&tx, &id)?;
                detail.held_by = holds.read(&tx, [&*id]).map_err(sql_error)?.pop().flatten();
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
    let facts = crate::frontend::read_window_facts(db, &items, extra)
        .map_err(StorageError::into_api_error)?;
    // Divergence points among the window's own nodes, under the family bounds.
    let window_ids = items.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
    let branch_points = history::branches::markers(db, &window.thread_id, &window_ids)?;
    Ok(HistoryWindowObservation {
        branch_points,
        read_sequence,
        pending: pending(db, &window.thread_id, holds).map_err(sql_error)?,
        committed_attempts,
        activity: activity(db, &window.thread_id)?,
        tools: facts.tools,
        recovery: facts.recovery,
        usage: facts.usage,
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
    let run_id: Option<String> = db
        .query_row(
            "SELECT id FROM run WHERE thread_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",
            [thread],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(run_id) = run_id else {
        return Ok(HistoryWindowActivity::default());
    };
    let turn_id: Option<String> = db.query_row(
        "SELECT id FROM turn INDEXED BY activity_turn_run WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1", [&run_id], |r| r.get(0),
    ).optional().map_err(sql_error)?;
    let attempt_id = if let Some(turn_id) = &turn_id {
        db.query_row(
            "SELECT id FROM model_attempt INDEXED BY activity_attempt_turn WHERE turn_id=?1 ORDER BY rowid DESC LIMIT 1", [turn_id], |r| r.get(0),
        ).optional().map_err(sql_error)?
    } else {
        None
    };
    let waiting_reason = crate::readback::run_wait_reason(db, thread, &run_id)?;
    Ok(HistoryWindowActivity {
        run_id: Some(run_id),
        turn_id,
        attempt_id,
        waiting_reason,
    })
}
