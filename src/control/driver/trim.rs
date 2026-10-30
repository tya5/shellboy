//! One deterministic maximum reduction before POST or at the first large-input failure. The
//! Compactor owns sizing, safe ranges, provenance and atomic path adoption.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Adopted,
    NoCandidates,
    Rejected,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn try_trim(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    attempt: &str,
    failure: &mut model::http::AttemptFailure,
    trigger_tokens: u64,
    retries: u32,
    limit: u32,
    not_applied: &mut Option<Value>,
) -> Result<Outcome, ApiError> {
    if slot.stop.is_cancelled() {
        return Ok(Outcome::Rejected);
    }
    let result = match control
        .compactor
        .trim_activated(&snapshot.thread, slot.stop.clone())
        .await
    {
        Ok(result) => result,
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(error) => {
            *not_applied = Some(json!({"outcome":"rejected","error":error}));
            failure
                .error
                .message
                .push_str(&format!("; automatic context trim not adopted: {error}"));
            return Ok(Outcome::Rejected);
        }
    };
    if result.compact.no_target {
        *not_applied = Some(
            json!({"outcome":"no_safe_reduction","trigger_estimated_tokens":trigger_tokens,"result":result}),
        );
        failure.error.message.push_str(
            "; automatic context trim not adopted: no safe groups outside protected work",
        );
        return Ok(Outcome::NoCandidates);
    }
    let body = json!({
        "error":failure.error,
        "partial_text":failure.partial_text,
        "partial_reasoning":failure.partial_reasoning,
        "completed_items":failure.completed_items,
        "raw_response":failure.raw_response,
        "attempt_id":attempt,
        "automatic_trim_trigger":{"estimated_input_tokens":trigger_tokens,"retries_used":retries,"retry_limit":limit,
            "retry_after_ms":failure.facts.retry_after.map(|delay| delay.as_millis().min(u64::MAX as u128) as u64)},
        "automatic_trim":result,
    });
    let partial = failure.facts.received == model::http::ResponseReceived::Output;
    let saved_attempt = attempt.to_owned();
    let turn = snapshot.turn.clone();
    let transition = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let transition = super::super::state::fail_attempt(&tx, &saved_attempt, &body, &now())?;
            if transition == super::super::state::AttemptTransition::Applied && partial {
                save_received_failure(&tx, &turn, &body)?;
            }
            tx.commit()?;
            Ok(transition)
        })
        .await
        .map_err(storage_error)?;
    // Obsolete means stop/generation change won. Do not renew its budget.
    if transition != super::super::state::AttemptTransition::Applied {
        return Ok(Outcome::Adopted);
    }
    control.changed(&snapshot.thread);
    Ok(Outcome::Adopted)
}

/// Called only after the already prepared request exceeds an explicit hint.
/// Optional stale/rejected edits leave ordinary request admission in charge.
pub(super) async fn before_post(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    budget: &mut retry::RetryBudget,
) -> Result<bool, ApiError> {
    let result = match control
        .compactor
        .trim_activated(&snapshot.thread, slot.stop.clone())
        .await
    {
        Ok(result) => result,
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(_) => return Ok(false),
    };
    if result.compact.no_target {
        return Ok(false);
    }
    budget.consume_trim();
    control.changed(&snapshot.thread);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_optional_preflight_preserves_the_turn_and_reduction_right() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:9/v1\"\n").unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let response = control
            .request(Request::new(
                format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    model: None,
                    effort: None,
                    cwd: None,
                    parent_id: None,
                }),
            ))
            .await;
        assert!(response.ok, "{:?}", response.error);
        let thread = response.result.unwrap()["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let target = thread.clone();
        control.store.database.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('preflight-run',?1,'live','now')", [&target])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('preflight-turn',?1,'preflight-run','preparing','now')", [&target])?;
            history::append_node(&tx, &target, "old", None, "user", &json!({"role":"user","content":"old context"}), "now")?;
            history::append_node(&tx, &target, "current", Some("preflight-turn"), "user", &json!({"role":"user","content":"current request"}), "now")?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let Next::Ready(mut snapshot) = next(&control, &thread, "preflight-run").await.unwrap()
        else {
            panic!("prepared fixture must be ready");
        };
        snapshot.input.take();
        let slot = DriverSlot {
            id: "fixture-slot".into(),
            run_id: "preflight-run".into(),
            stop: CancellationToken::new(),
            nudge: Arc::new(Notify::new()),
            foreground: Arc::new(ForegroundSlot::default()),
            turn: Arc::new(tokio::sync::Mutex::new(None)),
        };
        slot.stop.cancel();
        let mut budget = retry::RetryBudget::default();
        budget.observe("preflight-turn", 0);
        assert!(
            !before_post(&control, &snapshot, &slot, &mut budget)
                .await
                .unwrap()
        );
        assert!(
            budget.trim_available(true),
            "a rejected edit must not spend the reduction right"
        );
        control
            .store
            .database
            .call(|db| {
                let (state, reason): (String, Option<String>) = db.query_row(
                    "SELECT state,recovery_reason_json FROM turn WHERE id='preflight-turn'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                assert_eq!(state, "preparing");
                assert!(
                    reason.is_none(),
                    "optional cancellation cannot manufacture recovery"
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM replacement", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        drop(root);
    }
}
