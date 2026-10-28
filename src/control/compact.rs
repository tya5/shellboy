//! The existing per-Thread compaction queue also serializes explicit context edits.
use super::*;
use crate::{compaction::CompactionKeys, storage::StorageError};
use rusqlite::{OptionalExtension, Transaction, params};

struct CompactBatch {
    operations: Vec<String>,
    edit: Option<ContextEditArgs>,
}

fn saved_edit_args(
    tx: &Transaction<'_>,
    operation: &str,
) -> Result<Option<ContextEditArgs>, StorageError> {
    // Pending/inflight IDs already protect their immutable receipt and its
    // bounded args against receipt collection.
    let saved: String = tx.query_row(
        "SELECT result_ref_json FROM operation_receipt WHERE operation_id=?1",
        [operation],
        |row| row.get(0),
    )?;
    let saved: Value = serde_json::from_str(&saved)
        .map_err(|_| StorageError::Schema("Compaction receipt is malformed".into()))?;
    saved
        .pointer("/result/context_edit")
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|_| {
                StorageError::Schema("Context edit receipt arguments are malformed".into())
            })
        })
        .transpose()
}

fn take_batch(tx: &Transaction<'_>, thread: &str) -> Result<Option<CompactBatch>, StorageError> {
    let key = CompactionKeys::new(thread).pending();
    let body: Option<String> = tx
        .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
            row.get(0)
        })
        .optional()?;
    let mut pending: Vec<String> = match body {
        Some(body) => serde_json::from_str(&body)
            .map_err(|_| StorageError::Schema("Pending compaction metadata is malformed".into()))?,
        None => Vec::new(),
    };
    let Some(first) = pending.first() else {
        tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
        return Ok(None);
    };
    let edit = saved_edit_args(tx, first)?;
    let mut count = 1;
    if edit.is_none() {
        // Coalesce automatic compaction requests without reordering an explicit edit or
        // applying one edit's range/action to any other operation.
        for operation in pending.iter().skip(1) {
            if saved_edit_args(tx, operation)?.is_some() {
                break;
            }
            count += 1;
        }
    }
    let operations: Vec<String> = pending.drain(..count).collect();
    if pending.is_empty() {
        tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
    } else {
        tx.execute(
            "UPDATE app_meta SET value=?2 WHERE key=?1",
            params![key, serde_json::to_string(&pending).unwrap()],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO app_meta(key,value) VALUES(?1,?2)",
        params![
            CompactionKeys::new(thread).inflight(),
            serde_json::to_string(&operations).unwrap()
        ],
    )?;
    Ok(Some(CompactBatch { operations, edit }))
}

fn save_batch_outcome(
    tx: &Transaction<'_>,
    thread: &str,
    operations: &[String],
    downstream: &Value,
) -> Result<(), rusqlite::Error> {
    tx.execute(
        "DELETE FROM app_meta WHERE key=?1",
        [CompactionKeys::new(thread).inflight()],
    )?;
    let body = downstream.to_string();
    for operation in operations {
        tx.execute(
            "INSERT OR REPLACE INTO app_meta(key,value) VALUES(?1,?2)",
            params![CompactionKeys::result(operation), body],
        )?;
    }
    Ok(())
}

impl Control {
    pub(super) fn queue_compact(
        self: &Arc<Self>,
        thread: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let control = self.clone();
        Box::pin(async move {
            if control.stop.is_cancelled() {
                return;
            }
            let mut tasks = control.compact_tasks.lock().await;
            if tasks.contains_key(&thread) {
                return;
            }
            let token = control.stop.child_token();
            tasks.insert(thread.clone(), token.clone());
            drop(tasks);
            tokio::spawn(async move {
                control.clone().run_compact_queue(&thread, token).await;
                control.compact_tasks.lock().await.remove(&thread);
                control.changed.notify_waiters();
                if !control.stop.is_cancelled() {
                    let key = CompactionKeys::new(&thread).pending();
                    if matches!(
                        control
                            .store
                            .database
                            .call(move |db| {
                                Ok(db.query_row(
                                    "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                                    [key],
                                    |r| r.get::<_, bool>(0),
                                )?)
                            })
                            .await,
                        Ok(true)
                    ) {
                        control.queue_compact(thread).await;
                    }
                }
            });
        })
    }

    async fn run_compact_queue(self: Arc<Self>, thread: &str, token: CancellationToken) {
        let boundary = self.boundary(thread).await;
        loop {
            let guard = tokio::select! {biased; _=token.cancelled()=>None, guard=boundary.lock()=>Some(guard)};
            let target = thread.to_owned();
            let next = self
                .store
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let batch = take_batch(&tx, &target)?;
                    tx.commit()?;
                    Ok(batch)
                })
                .await;
            let CompactBatch {
                operations,
                edit: args,
            } = match next {
                Ok(Some(next)) => next,
                Ok(None) => break,
                Err(_) => {
                    self.fail_storage().await;
                    break;
                }
            };
            let result = if guard.is_none() {
                Err(ApiError::new(
                    ErrorCode::InvalidState,
                    "Queued compaction cancelled by stop",
                ))
            } else if let Err(error) = self.gc.require_capacity().await {
                Err(error)
            } else {
                self.perform_compact(thread, args.as_ref(), token.clone())
                    .await
            };
            let save_failed = result
                .as_ref()
                .err()
                .is_some_and(ApiError::is_storage_failure);
            let outcome = match &result {
                Ok(value) => json!({"result":value}),
                Err(error) => json!({"error":error}),
            };
            let saved_thread = thread.to_owned();
            let saved = self.store.database.call(move |db| {
                let tx = db.transaction()?;
                let downstream=json!({"kind":"compact","state":if result.is_ok(){"completed"}else{"failed"},"outcome":outcome,"ended_at":chrono::Utc::now().to_rfc3339()});
                save_batch_outcome(&tx, &saved_thread, &operations, &downstream)?;
                tx.commit()?;
                Ok(())
            }).await;
            if saved.is_err() || save_failed {
                self.fail_storage().await;
                break;
            }
            drop(guard);
            self.changed(thread);
            // A cancelled queue still consumes all pending intents as failed.
            // It must not respawn them under a fresh token after a Run stop.
        }
    }

    async fn perform_compact(
        &self,
        thread: &str,
        edit: Option<&ContextEditArgs>,
        cancel: CancellationToken,
    ) -> Result<Value, ApiError> {
        if let Some(edit) = edit {
            return encode_result(
                self.compactor.edit(thread, edit, cancel).await?,
                ErrorCode::InvalidState,
                "Cannot encode context edit result",
            );
        }
        let spill = self
            .compactor
            .spill_cancellable(thread, false, cancel.clone())
            .await?;
        let mut summary = self.compactor.summarize(thread, false, cancel).await?;
        summary.spilled_groups += spill.spilled_groups;
        summary.source_nodes += spill.source_nodes;
        summary.original_node_ids.extend(spill.original_node_ids);
        summary.no_target = summary.no_target && spill.no_target;
        encode_result(
            summary,
            ErrorCode::InvalidState,
            "Cannot encode compact result",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn edit() -> ContextEditArgs {
        ContextEditArgs {
            thread_id: Some("thread".into()),
            action: ContextEditAction::Spill,
            from_node_id: Some("first".into()),
            to_node_id: Some("last".into()),
        }
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE operation_receipt(operation_id TEXT PRIMARY KEY,result_ref_json TEXT NOT NULL);").unwrap();
        for (operation, explicit) in [
            ("automatic-a", false),
            ("automatic-b", false),
            ("explicit", true),
            ("automatic-c", false),
        ] {
            let mut saved =
                json!({"result":{"thread_id":"thread","status":"queued","operation_id":operation}});
            if explicit {
                saved["result"]["context_edit"] = json!(edit());
            }
            db.execute(
                "INSERT INTO operation_receipt VALUES(?1,?2)",
                params![operation, saved.to_string()],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO app_meta VALUES(?1,?2)",
            params![
                CompactionKeys::new("thread").pending(),
                json!(["automatic-a", "automatic-b", "explicit", "automatic-c"]).to_string()
            ],
        )
        .unwrap();
        db
    }
    fn saved_outcome(db: &Connection, operation: &str) -> String {
        db.query_row(
            "SELECT value FROM app_meta WHERE key=?1",
            [CompactionKeys::result(operation)],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn automatic_prefix_coalesces_once_without_crossing_an_explicit_edit() {
        let mut db = fixture();
        let original = db
            .prepare("SELECT result_ref_json FROM operation_receipt ORDER BY operation_id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut batches = Vec::new();
        for ordinal in 0..3 {
            let tx = db.transaction().unwrap();
            let batch = take_batch(&tx, "thread").unwrap().unwrap();
            let inflight: String = tx
                .query_row(
                    "SELECT value FROM app_meta WHERE key=?1",
                    [CompactionKeys::new("thread").inflight()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Vec<String>>(&inflight).unwrap(),
                batch.operations
            );
            if ordinal == 1 {
                assert_eq!(batch.edit, Some(edit()));
            } else {
                assert!(batch.edit.is_none());
            }
            // Production invokes perform_compact once per returned batch and
            // persists this one immutable outcome to all its operation IDs.
            save_batch_outcome(&tx,"thread",&batch.operations,&json!({"kind":"compact","state":"completed","outcome":{"result":{"ordinal":ordinal}},"ended_at":"same-batch-time"})).unwrap();
            batches.push(batch.operations);
            tx.commit().unwrap();
        }
        assert_eq!(
            batches,
            vec![
                vec!["automatic-a", "automatic-b"],
                vec!["explicit"],
                vec!["automatic-c"]
            ]
        );
        assert_eq!(
            saved_outcome(&db, "automatic-a"),
            saved_outcome(&db, "automatic-b")
        );
        assert_ne!(
            saved_outcome(&db, "automatic-b"),
            saved_outcome(&db, "automatic-c")
        );
        assert_ne!(
            saved_outcome(&db, "automatic-b"),
            saved_outcome(&db, "explicit")
        );
        let tx = db.transaction().unwrap();
        assert!(take_batch(&tx, "thread").unwrap().is_none());
        tx.commit().unwrap();
        let after = db
            .prepare("SELECT result_ref_json FROM operation_receipt ORDER BY operation_id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            after, original,
            "coalescing changes neither original acceptance nor explicit args"
        );
    }

    #[test]
    fn malformed_following_receipt_rolls_back_the_entire_automatic_batch_selection() {
        let mut db = fixture();
        db.execute(
            "UPDATE operation_receipt SET result_ref_json=?1 WHERE operation_id='automatic-b'",
            [json!({"result":{"context_edit":{"action":"unsupported"}}}).to_string()],
        )
        .unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(take_batch(&tx, "thread").is_err());
        }
        let pending: String = db
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [CompactionKeys::new("thread").pending()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&pending).unwrap(),
            ["automatic-a", "automatic-b", "explicit", "automatic-c"]
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE key LIKE 'compact.inflight.%'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn cancelled_worker_drains_automatic_batches_and_individual_edits_without_model_calls() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        owner.db.call(|db| { db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread','/','/AGENTS.md','not-configured','not-configured','now')",[])?; Ok(()) }).await.unwrap();
        let mut operations = Vec::new();
        for explicit in [false, false, true, false] {
            let operation = format!("{}:{}", owner.instance_id, uuid::Uuid::new_v4());
            let command = if explicit {
                Command::ContextEdit(edit())
            } else {
                Command::Compact(ThreadTarget {
                    thread_id: Some("thread".into()),
                })
            };
            control
                .store
                .execute(Request::new(&operation, command), None)
                .await
                .unwrap();
            operations.push(operation);
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        control.clone().run_compact_queue("thread", cancel).await;
        owner.db.call(move |db| {
            let outcomes = operations.iter().map(|id|saved_outcome(db,id)).collect::<Vec<_>>();
            assert_eq!(outcomes[0],outcomes[1]);
            for saved in outcomes {
                let saved:Value=serde_json::from_str(&saved).unwrap();
                assert_eq!(saved["state"],"failed");
                assert!(saved["outcome"]["error"]["message"].as_str().unwrap().contains("cancelled by stop"));
            }
            assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'compact.pending.%' OR key LIKE 'compact.inflight.%'",[],|row|row.get::<_,i64>(0))?,0);
            assert_eq!(db.query_row("SELECT count(*) FROM model_attempt",[],|row|row.get::<_,i64>(0))?,0);
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
