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
    Ok(HistoryWindowObservation {
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
            COALESCE(json_extract(result_json,'$.deliver')='complete'
                AND (hold_reason='run_completion' OR json_extract(result_json,'$.delivery_context.reason')='run_completion'),0)
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

#[cfg(test)]
mod queue_tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE receipt(message_node_id TEXT,client_operation_id TEXT,sender_json TEXT,created_at TEXT,state TEXT,hold_reason TEXT,path_admission TEXT,result_json TEXT,target_thread_id TEXT,run_id TEXT,turn_id TEXT); CREATE TABLE node(id TEXT,payload_json TEXT); CREATE TABLE thread_path(thread_id TEXT,node_id TEXT); CREATE TABLE attachment_ref(source_node_id TEXT); CREATE TABLE draft(id TEXT,thread_id TEXT,state TEXT,updated_at TEXT,body_json TEXT); CREATE INDEX receipt_target ON receipt(target_thread_id,state,created_at);").unwrap();
        db
    }
    fn input(
        db: &Connection,
        id: &str,
        state: &str,
        admission: &str,
        text: &str,
        sender: Value,
        now: bool,
    ) {
        let payload =
            serde_json::json!({"role":"user","content":[{"type":"input_text","text":text}]})
                .to_string();
        db.execute("INSERT INTO node VALUES(?1,?2)", params![id, payload])
            .unwrap();
        db.execute(
            "INSERT INTO receipt VALUES(?1,?2,?3,'2026-10-30T00:00:00Z',?4,?5,?6,?7,'thread',NULL,NULL)",
            params![
                id,
                format!("fixture.0:{id}"),
                sender.to_string(),
                state,
                (state == "held").then_some("terminal_before_path_admission"),
                admission,
                serde_json::json!({"deliver": if now { "now" } else { "next" }}).to_string()
            ],
        )
        .unwrap();
    }
    #[test]
    fn held_region_projects_saved_sender_and_completion_facts_without_null_guessing() {
        use crate::protocol::SenderKind;
        let db = fixture();
        for (id, sender, expected) in [
            (
                "human",
                serde_json::json!({"kind":"human"}),
                SenderKind::Human,
            ),
            (
                "external",
                serde_json::json!({"kind":"external_cli"}),
                SenderKind::ExternalCli,
            ),
            ("legacy-null", Value::Null, SenderKind::Unknown),
            (
                "incomplete-model",
                serde_json::json!({"kind":"model", "thread_id":"other"}),
                SenderKind::Unknown,
            ),
            (
                "model",
                serde_json::json!({"kind":"model", "thread_id":"other", "instance_id":"i", "run_id":"r", "turn_id":"t", "execution_id":"e"}),
                SenderKind::Model,
            ),
        ] {
            input(
                &db,
                id,
                "held",
                "unadmitted",
                "synthetic excerpt",
                sender,
                false,
            );
            let pending = pending(&db, "thread", &MessageHolds::fixture()).unwrap();
            let item = pending
                .items
                .iter()
                .find(|item| item.message_id == id)
                .unwrap();
            assert_eq!(item.sender.kind, expected);
            assert!(!item.completion_scheduled);
        }
        db.execute("UPDATE receipt SET hold_reason='run_completion',result_json='{\"deliver\":\"complete\"}' WHERE message_node_id='external'", []).unwrap();
        let completion = |db: &Connection| {
            pending(db, "thread", &MessageHolds::fixture())
                .unwrap()
                .items
                .into_iter()
                .find(|item| item.message_id == "external")
                .unwrap()
                .completion_scheduled
        };
        assert!(completion(&db));
        db.execute(
            "UPDATE receipt SET hold_reason='recovery_wait' WHERE message_node_id='external'",
            [],
        )
        .unwrap();
        assert!(!completion(&db));
        db.execute("UPDATE receipt SET hold_reason=NULL,state='accepted',result_json='{\"deliver\":\"complete\",\"delivery_context\":{\"reason\":\"run_completion\"}}' WHERE message_node_id='external'", []).unwrap();
        assert!(completion(&db));
        db.execute("UPDATE receipt SET result_json='{\"deliver\":\"next\",\"delivery_context\":{\"reason\":\"run_completion\"}}' WHERE message_node_id='external'", []).unwrap();
        assert!(!completion(&db));
    }

    #[test]
    fn queue_uses_saved_sender_admission_membership_and_original_receipt_order() {
        let db = fixture();
        input(
            &db,
            "z-first",
            "accepted",
            "unadmitted",
            "first",
            Value::Null,
            false,
        );
        input(
            &db,
            "a-second",
            "held",
            "unknown",
            "second",
            serde_json::json!({"kind":"model","instance_id":"i","thread_id":"sender-thread","run_id":"r","turn_id":"t","execution_id":"e"}),
            true,
        );
        input(
            &db,
            "removed-but-admitted",
            "accepted",
            "admitted",
            "removed",
            Value::Null,
            false,
        );
        input(
            &db,
            "selected",
            "accepted",
            "unadmitted",
            "selected",
            Value::Null,
            false,
        );
        input(
            &db,
            "cancelled",
            "cancelled",
            "unadmitted",
            "cancelled",
            Value::Null,
            false,
        );
        db.execute("INSERT INTO thread_path VALUES('thread','selected')", [])
            .unwrap();
        let before = pending(&db, "thread", &MessageHolds::fixture()).unwrap();
        assert_eq!(
            before
                .items
                .iter()
                .map(|item| item.message_id.as_str())
                .collect::<Vec<_>>(),
            ["z-first", "a-second"]
        );
        assert_eq!(before.total, 2);
        assert!(before.items[0].sender.thread_id.is_none());
        assert_eq!(
            before.items[1].sender.thread_id.as_deref(),
            Some("sender-thread")
        );
        assert!(before.items[1].delivery_status == DeliveryStatus::Held && before.items[1].admission_uncertain);
        let tx = db.unchecked_transaction().unwrap();
        tx.execute("INSERT INTO thread_path VALUES('thread','z-first')", [])
            .unwrap();
        let after = pending(&tx, "thread", &MessageHolds::fixture()).unwrap();
        let selected: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread_path WHERE node_id='z-first')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(selected && after.items.iter().all(|item| item.message_id != "z-first"));
        tx.commit().unwrap();
    }
    #[test]
    fn queue_bounds_rows_and_unicode_excerpts_without_hiding_the_total() {
        let db = fixture();
        for index in 0..130 {
            input(
                &db,
                &format!("message-{index}"),
                "accepted",
                "unadmitted",
                &"日".repeat(700),
                Value::Null,
                false,
            );
        }
        let queue = pending(&db, "thread", &MessageHolds::fixture()).unwrap();
        assert!(!queue.total_exact);
        assert!(queue.total >= queue.items.len() as u64 && queue.total <= 130);
        assert_eq!(queue.items.len(), 128);
        assert!(
            queue
                .items
                .iter()
                .all(|item| item.excerpt.chars().count() == 512 && item.excerpt_truncated)
        );
        assert_eq!(queue.items[127].message_id, "message-127");
    }
}
