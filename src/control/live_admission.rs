//! Durable admission and terminal observation serialize on the existing DB worker.
use super::*;
const NONTERMINAL: &str = "SELECT EXISTS(SELECT 1 FROM model_attempt a JOIN turn u ON u.id=a.turn_id JOIN thread t ON t.id=u.thread_id WHERE a.id=?1 AND t.id=?2 AND a.state IN ('preparing','inflight') AND t.deleted_at IS NULL)";
const TERMINAL: &str = "SELECT EXISTS(SELECT 1 FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE a.id=?1 AND u.thread_id=?2 AND a.state IN ('completed','failed','incomplete','cancelled'))";
impl Control {
    pub(super) async fn admit_live_stream<'a>(
        &'a self,
        thread: &'a str,
        attempt: &'a str,
    ) -> change_clock::LiveTextGuard<'a> {
        let guard = self
            .change_clock
            .reserve_live(thread, attempt, &self.text_changed);
        let (token, alive) = guard.reservation();
        let clock = self.change_clock.clone();
        let t = thread.to_owned();
        let a = attempt.to_owned();
        let _ = self
            .store
            .database
            .call(move |db| {
                let allowed: bool =
                    db.query_row(NONTERMINAL, rusqlite::params![a, t], |r| r.get(0))?;
                if allowed {
                    clock.admit_live(&t, &a, &token, &alive);
                }
                Ok(())
            })
            .await;
        self.text_changed.notify_waiters();
        guard
    }
    pub(super) fn stream_admitted_delta(
        &self,
        guard: &change_clock::LiveTextGuard<'_>,
        thread: &str,
        attempt: &str,
        text: &str,
        reasoning: bool,
        offset: usize,
    ) {
        if !guard.record(offset as u64, text, reasoning) {
            return;
        }
        self.publish_event(EngineEvent::Stream {
            thread_id: thread.into(),
            node_id: attempt.into(),
            text: text.into(),
            reasoning,
            stream_char_offset: offset,
        });
        self.text_changed.notify_waiters();
    }
    pub(super) async fn fence_terminal_status(&self, result: &Value) -> Result<(), ApiError> {
        let attempts = result
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                if item.get("kind")?.as_str()? != "attempt"
                    || !matches!(
                        item.get("state")?.as_str()?,
                        "completed" | "failed" | "incomplete" | "cancelled"
                    )
                {
                    return None;
                }
                Some((
                    item.get("thread_id")?.as_str()?.to_owned(),
                    item.get("id")?.as_str()?.to_owned(),
                ))
            })
            .collect::<Vec<_>>();
        if attempts.is_empty() {
            return Ok(());
        }
        let clock = self.change_clock.clone();
        self.store
            .database
            .call(move |db| {
                for (thread, attempt) in attempts {
                    let terminal: bool =
                        db.query_row(TERMINAL, rusqlite::params![attempt, thread], |r| r.get(0))?;
                    if !terminal {
                        return Err(crate::storage::StorageError::from(ApiError::new(
                            ErrorCode::InvalidState,
                            "Terminal status changed before live fence",
                        )));
                    }
                    clock.revoke_live(&thread, &attempt, None);
                }
                Ok(())
            })
            .await
            .map_err(crate::storage::StorageError::into_api_error)?;
        self.text_changed.notify_waiters();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn abandoned_queued_admission_cannot_register_after_cleanup() {
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
        let guard = control
            .change_clock
            .reserve_live("t", "a", &control.text_changed);
        let (token, alive) = guard.reservation();
        let clock = control.change_clock.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let db = control.store.database.clone();
        let task = tokio::spawn(async move {
            db.call(move |_| {
                let _ = ready_tx.send(());
                release_rx.recv().unwrap();
                clock.admit_live("t", "a", &token, &alive);
                Ok(())
            })
            .await
        });
        ready_rx.await.unwrap();
        // Simulates cancellation/lost return while the already queued worker is paused.
        drop(guard);
        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert!(control.change_clock.snapshot(&[], true, true).1.is_empty());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn terminal_status_fences_all_four_states_and_denies_late_admission() {
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
        control.store.database.call(|db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','2026-01-01T00:00:00Z'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','live','2026-01-01T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','running','2026-01-01T00:00:00Z'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('a','u',0,0,'p','m',0,'inflight','{}','2026-01-01T00:00:00Z');")?;Ok(())
        }).await.unwrap();
        for state in ["completed", "failed", "incomplete", "cancelled"] {
            // Recreate a synthetic inflight fixture between independently fenced cases.
            control
                .store
                .database
                .call(|db| {
                    db.execute("UPDATE model_attempt SET state='inflight' WHERE id='a'", [])?;
                    Ok(())
                })
                .await
                .unwrap();
            let guard = control.admit_live_stream("t", "a").await;
            assert!(guard.record(0, "text", false));
            assert!(guard.record(0, "reasoning", true));
            control
                .store
                .database
                .call(move |db| {
                    db.execute("UPDATE model_attempt SET state=?1 WHERE id='a'", [state])?;
                    Ok(())
                })
                .await
                .unwrap();
            let request:Request=serde_json::from_value(json!({"protocol_version":PROTOCOL_VERSION,"operation_id":format!("status-{state}"),"command":"status","args":{"thread_id":"t","all":true}})).unwrap();
            let response = control.evaluate(request, CancellationToken::new()).await;
            assert!(
                response.0.result.as_ref().unwrap()["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["id"] == "a" && i["state"] == state)
            );
            assert!(!guard.record(0, "late", false));
            assert!(!guard.record(0, "late", true));
            let late = control.admit_live_stream("t", "a").await;
            assert!(!late.record(0, "re-admission", false));
            let baseline = control.change_clock.snapshot(&[], true, true);
            assert!(baseline.1.is_empty() && baseline.2.is_empty());
        }
        // A dishonest/stale terminal projection must fail, never return unfenced success.
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE model_attempt SET state='inflight' WHERE id='a'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(control.fence_terminal_status(&json!({"items":[{"kind":"attempt","id":"a","thread_id":"t","state":"completed"}]})).await.is_err());
        assert!(control.fence_terminal_status(&json!({"items":[{"kind":"attempt","id":"a","thread_id":"other","state":"completed"}]})).await.is_err());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
