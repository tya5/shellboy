//! Consecutive conversation retries; the HTTP client reports facts only.
use super::*;
#[cfg(test)]
use model::http::ResponseFacts;
#[cfg(test)]
use model::http::{AttemptFailure, ResponseReceived};
#[cfg(test)]
use model::request_path::AttemptKind;
pub(super) use model::request_path::RetryBudget;
#[cfg(test)]
use model::request_path::classify_attempt_retry;
#[cfg(test)]
use model::request_path::eligible_trim_failure;
#[cfg(test)]
use std::time::Duration;

pub(super) use crate::model::retry_class::RetryPolicy;
#[cfg(test)]
use crate::model::retry_class::classify_retry;

#[cfg(test)]
fn classify_conversation_retry(failure: &AttemptFailure) -> RetryPolicy {
    classify_attempt_retry(failure, AttemptKind::Conversation)
}

/// #182 rule 4: the provider rejected a replayed signed thinking block as bound
/// to a different prefix. Save the failure with the retry fact, start a new
/// reasoning epoch (no reasoning saved before it is sent again) in the same
/// transaction, and resend once. Returns false when the attempt was obsolete.
async fn thinking_binding_retry(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    attempt: &str,
    partial_attempts: &[String],
) -> Result<bool, ApiError> {
    let (saved_attempt, thread) = (attempt.to_owned(), snapshot.thread.clone());
    let current = snapshot.clone();
    let partial_attempts = partial_attempts.to_vec();
    let applied = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            if !live_outcome_generation(&tx, &current)? {
                return Ok(false);
            }
            let applied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM model_attempt WHERE id=?1 AND turn_id=?2 AND generation=?3 AND state='failed' AND json_extract(error_json,'$.automatic_retry.reason')='thinking_binding')",params![saved_attempt,current.turn,current.generation],|r|r.get(0))?;
            if applied {
                save_partial_attempts(&tx, &current, &partial_attempts)?;
                tx.execute("UPDATE model_attempt SET error_json=json_set(error_json,'$.thinking_binding_retry',json('{\"reasoning_dropped\":true}')) WHERE id=?1",[&saved_attempt])?;
                tx.execute(
                    "UPDATE thread SET reasoning_epoch=reasoning_epoch+1 WHERE id=?1",
                    [&thread],
                )?;
            }
            tx.commit()?;
            Ok(applied)
        })
        .await
        .map_err(storage_error)?;
    if applied {
        control.diagnostics.record(
            crate::diagnostics::DiagnosticEvent::ThinkingBindingRetry,
            crate::diagnostics::DiagnosticIds {
                thread_id: Some(snapshot.thread.clone()),
                run_id: Some(snapshot.run.clone()),
                turn_id: Some(snapshot.turn.clone()),
                ..Default::default()
            },
        );
        control.changed(&snapshot.thread);
    }
    Ok(applied)
}

/// #182 overflow fallback while the tool loop is open: the active-Turn summary
/// (③ summarized, ④ as text), else the ordinary summary of earlier Turns.
async fn open_loop_reduce(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
) -> Result<Value, ApiError> {
    let compactor = &control.compactor;
    let mut scope = "active_turn";
    let mut result = compactor
        .summarize_active_guarded(
            &snapshot.thread,
            slot.stop.clone(),
            Some(&mut *boundary),
            None,
            Some(snapshot.run.clone()),
        )
        .await;
    if matches!(&result, Ok(r) if r.no_target) {
        scope = "prefix";
        result = compactor
            .summarize_prefix_guarded(
                &snapshot.thread,
                slot.stop.clone(),
                Some(&mut *boundary),
                None,
                Some(snapshot.run.clone()),
            )
            .await;
    }
    Ok(match result {
        Ok(result) => {
            json!({"outcome":if result.no_target {"no_safe_reduction"} else {"adopted"},"summary":result,"midturn":{"scope":scope,"reason":"open_tool_loop"}})
        }
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(error) => {
            json!({"outcome":"rejected","error":error,"midturn":{"scope":scope,"reason":"open_tool_loop"}})
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn retry_or_recover(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    attempt: String,
    mut failure: model::http::AttemptFailure,
    adaptation: bool,
    decision: model::request_path::FailureDecision,
    budget: &mut RetryBudget,
    overflow: &mut OverflowBudget,
    proactive_job: &mut super::proactive::ThreadJob,
    boundary_guard: &mut crate::compaction::boundary::Boundary<'_>,
    partial_attempts: Vec<String>,
) -> Result<(), ApiError> {
    let clients = &snapshot.clients;
    if !clients.connections.contains_key(&snapshot.profile) {
        return recover_with_observation(
            control,
            snapshot,
            ApiError::new(
                ErrorCode::ConfigInvalid,
                "Model connection is not configured",
            ),
            Some(attempt),
            None,
            partial_attempts,
        )
        .await;
    };
    let model::request_path::FailureDecision {
        policy,
        used,
        limit,
        drop_reasoning,
    } = decision;
    if drop_reasoning
        && thinking_binding_retry(control, snapshot, &attempt, &partial_attempts).await?
    {
        return Ok(());
    }
    let mut can_rebuild = adaptation;
    let mut trim_not_applied = None;
    let mut proactive_adopted = false;
    // Join already-admitted compaction even when there is no slot to rebuild.
    if failure.error.is_context_limit()
        && let super::proactive::ThreadJob::Retained(job) = proactive_job
    {
        let waited = boundary_guard.pause(&slot.stop, job.join()).await;
        let joined = match waited {
            Ok(joined) => joined,
            Err(_) if slot.stop.is_cancelled() => return Ok(()),
            Err(_) => return Err(invalid("proactive summary boundary wait cancelled")),
        };
        *proactive_job = super::proactive::ThreadJob::Idle;
        match joined {
            Ok(Ok(super::proactive::ProactiveOutcome::Finished { path_changed: true })) => {
                proactive_adopted = true;
                trim_not_applied = Some(json!({"outcome":"adopted","reason":"same_proactive_job"}));
                control.changed(&snapshot.thread);
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.is_storage_failure() => return Err(error),
            Ok(Err(_)) => {}
            Err(_) => return Err(invalid("proactive summary task panicked")),
        }
    }
    if adaptation && !proactive_adopted {
        let open_loop = control.compactor.tool_loop_open(&snapshot.thread).await?;
        if failure.error.is_context_limit() && open_loop {
            // #182 rule 3: before a closed-loop boundary, selective trims and
            // spills wait; overflow falls back to the active-Turn summary.
            let mut observation = json!({"outcome":"stages_exhausted"});
            if overflow.begin().is_some() {
                observation = open_loop_reduce(control, snapshot, slot, boundary_guard).await?;
                if !boundary_guard.is_held() {
                    return Ok(());
                }
                control.changed(&snapshot.thread);
            }
            if observation["outcome"] != "adopted" {
                can_rebuild = false;
            }
            trim_not_applied = Some(observation);
        } else if failure.error.is_context_limit() {
            let mut observation = json!({"outcome":"stages_exhausted"});
            let stage = overflow.begin();
            if stage == Some(overflow::Reduction::OldGroups) {
                observation = trim::reduce(control, snapshot, slot, &mut failure).await?;
            }
            // No safe maximum target must not cause an identical POST. Try the
            // final existing stage now, while the same retry slot is available.
            if stage == Some(overflow::Reduction::CurrentSuffix)
                || (stage == Some(overflow::Reduction::OldGroups)
                    && observation["outcome"] == "no_safe_reduction")
            {
                if stage == Some(overflow::Reduction::OldGroups) {
                    overflow.begin();
                }
                let maximum_outcome = observation["outcome"].clone();
                observation = match spill_and_summarize(
                    &control.compactor.pinned(snapshot.clients.clone()),
                    &snapshot.thread,
                    slot.stop.clone(),
                    Some(boundary_guard),
                    Some(snapshot.run.clone()),
                )
                .await
                {
                    Ok(result) => {
                        json!({"outcome":if result.no_target {"no_safe_reduction"} else {"adopted"},"summary":result})
                    }
                    Err(error) if error.is_storage_failure() => return Err(error),
                    Err(error) => json!({"outcome":"rejected","error":error}),
                };
                if !boundary_guard.is_held() {
                    return Ok(());
                }
                if stage == Some(overflow::Reduction::OldGroups) {
                    observation["maximum_outcome"] = maximum_outcome;
                }
                control.changed(&snapshot.thread);
            }
            if observation["outcome"] != "adopted" {
                can_rebuild = false;
            }
            trim_not_applied = Some(observation);
        } else {
            trim_not_applied = Some(if open_loop {
                // #182: the large-input trim is a selective edit; it waits.
                json!({"outcome":"deferred","reason":"open_tool_loop"})
            } else {
                trim::reduce(control, snapshot, slot, &mut failure).await?
            });
        }
    }
    if slot.stop.is_cancelled() {
        return Ok(());
    }
    if can_rebuild {
        let current = snapshot.clone();
        control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                if !live_outcome_generation(&tx, &current)? {
                    return Ok(());
                }
                save_partial_attempts(&tx, &current, &partial_attempts)?;
                if let Some(observation) = trim_not_applied {
                    let trusted =
                        super::trim_observation::DriverTrimObservation::from_reduction(observation);
                    trusted.persist_trim(&tx, &attempt)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(storage_error)?;
        control.changed(&snapshot.thread);
        return Ok(());
    }
    budget.abandon_retry();
    if let RetryPolicy::Deferred(delay) = policy {
        failure.error.message.push_str(&format!(
            "; server requested Retry-After {}s; automatic wait limit is 600s",
            delay.as_secs()
        ));
    }
    overflow::retry_or_recover(
        control,
        snapshot,
        slot,
        attempt,
        failure,
        used,
        limit,
        policy,
        trim_not_applied,
        partial_attempts,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_untyped_terminal_authority_is_conversation_only_and_not_restorable() {
        let mut stream = model::ResponseStream::default();
        stream
            .push(b"data: {\"error\":{\"code\":\"599\"}}\n\n")
            .unwrap();
        let mut failure = AttemptFailure::from(stream.finish(false).unwrap_err());
        failure.facts = ResponseFacts {
            status: Some(200),
            terminal_received: true,
            received: ResponseReceived::Metadata,
            ..Default::default()
        };
        assert_eq!(
            classify_conversation_retry(&failure),
            RetryPolicy::Retry {
                partial_output: false,
            }
        );
        assert_eq!(
            classify_retry(&failure.error, &failure.facts),
            RetryPolicy::Ineligible,
            "the shared summary classifier still vetoes the same live terminal"
        );
        let mut saved = serde_json::to_value(&failure.error).unwrap();
        assert_eq!(
            saved["stream_provider_status"],
            json!({"value":599,"source":"stream_error_code"})
        );
        assert!(
            saved
                .get("strict_untyped_stream_error_before_done")
                .is_none()
        );
        saved["strict_untyped_stream_error_before_done"] = json!(true);
        failure.error = serde_json::from_value(saved).unwrap();
        assert!(failure.error.untyped_stream_error_witness().is_none());
        assert_eq!(
            classify_conversation_retry(&failure),
            RetryPolicy::Ineligible
        );
    }

    #[test]
    fn consecutive_budget_resets_only_after_adoption_or_turn_generation_change() {
        let mut budget = RetryBudget::default();
        budget.observe("t", 1);
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            assert_eq!(
                budget.delay(8, Duration::from_secs(60), None),
                Some(Duration::from_secs(seconds))
            );
            budget.used += 1;
            budget.observe("t", 1);
        }
        assert_eq!(budget.delay(8, Duration::from_secs(60), None), None);
        budget.succeeded();
        assert_eq!(
            budget.delay(8, Duration::from_secs(60), None),
            Some(Duration::from_secs(1))
        );
        budget.used = 8;
        budget.observe("t", 2);
        assert_eq!(budget.used, 0);
        assert_eq!(budget.delay(0, Duration::from_secs(60), None), None);
        budget.used = 8;
        budget.observe("other", 2);
        assert_eq!(budget.used, 0);
    }
    #[test]
    fn usage_limit_retry_bypass_is_one_shot_and_clears_on_new_generation() {
        let mut budget = RetryBudget::default();
        budget.observe("turn", 1);
        budget.usage_limit_retry_fact = Some(crate::model::usage_limit::UsageLimit {
            kind: crate::model::usage_limit::LimitKind::UsageLimitReached,
            resets_at: chrono::Utc::now().timestamp() + 1,
            limit_window_minutes: None,
            locally_blocked: false,
        });
        assert!(budget.usage_limit_retry_bypass().is_some());
        assert!(budget.usage_limit_retry_bypass().is_some());
        budget.clear_usage_limit_retry_bypass();
        assert!(budget.usage_limit_retry_bypass().is_none());
        budget.usage_limit_retry_fact = Some(crate::model::usage_limit::UsageLimit {
            kind: crate::model::usage_limit::LimitKind::UsageLimitReached,
            resets_at: chrono::Utc::now().timestamp() + 1,
            limit_window_minutes: None,
            locally_blocked: false,
        });
        budget.observe("turn", 2);
        assert!(budget.usage_limit_retry_bypass().is_none());
    }
    #[test]
    fn reset_comparison_uses_only_the_remaining_full_retry_schedule() {
        let mut budget = RetryBudget::default();
        assert_eq!(
            budget.planned_wait(8, Duration::from_secs(60), None),
            Duration::from_secs(183)
        );
        budget.used = 6;
        assert_eq!(
            budget.planned_wait(8, Duration::from_secs(60), None),
            Duration::from_secs(120)
        );
        assert_eq!(
            budget.planned_wait(2, Duration::from_secs(60), None),
            Duration::ZERO
        );
        assert_eq!(
            budget.planned_wait(8, Duration::from_secs(60), Some(Duration::from_secs(2))),
            Duration::from_secs(62)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn retry_after_delay_uses_virtual_time_not_runner_speed() {
        let delay = RetryBudget::default()
            .delay(8, Duration::from_millis(10), Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(delay, Duration::from_secs(1));
        let wait = tokio::time::sleep(delay);
        tokio::pin!(wait);
        assert!(futures_util::poll!(&mut wait).is_pending());
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(futures_util::poll!(&mut wait).is_pending());
        tokio::time::advance(Duration::from_millis(2)).await;
        wait.await;
    }
    #[test]
    fn server_wait_is_not_shortened_by_exponential_cap() {
        let budget = RetryBudget::default();
        for seconds in [0, 1, 120, 600] {
            assert_eq!(
                budget.delay(
                    8,
                    Duration::from_millis(10),
                    Some(Duration::from_secs(seconds))
                ),
                Some(Duration::from_secs(seconds))
            );
        }
    }
    #[test]
    fn one_policy_classifies_error_status_reception_and_terminal() {
        use ResponseReceived::{Metadata, None as NoResponse, Output};
        for kind in [
            "http",
            "provider",
            "usage_limit",
            "transport",
            "timeout",
            "incomplete",
            "protocol",
            "cancelled",
            "consumer_closed",
        ] {
            for status in [
                None,
                Some(200),
                Some(400),
                Some(401),
                Some(403),
                Some(404),
                Some(429),
                Some(500),
                Some(501),
                Some(502),
                Some(503),
                Some(504),
            ] {
                for received in [NoResponse, Metadata, Output] {
                    for terminal_received in [false, true] {
                        let facts = ResponseFacts {
                            status,
                            received,
                            terminal_received,
                            http_error_complete: false,
                            admission_wait_ms: None,
                            retry_after: None,
                            retry_after_present: false,
                            http_observation: None,
                        };
                        let error = model::ModelError::new(kind, "fixture");
                        let expected = !terminal_received
                            && match kind {
                                "http" | "provider" => {
                                    matches!(status, Some(429 | 500..=599))
                                }
                                "transport" | "timeout" | "incomplete" => {
                                    status.is_none()
                                        || status == Some(200)
                                        || matches!(status, Some(429 | 500..=599))
                                }
                                _ => false,
                            };
                        assert_eq!(
                            classify_retry(&error, &facts),
                            if expected {
                                RetryPolicy::Retry {
                                    partial_output: received == Output,
                                }
                            } else {
                                RetryPolicy::Ineligible
                            },
                            "{kind} {facts:?}"
                        );
                    }
                }
            }
        }
        let mut error = model::ModelError::new("provider", "fixture");
        error.provider_code = Some("context_length_exceeded".into());
        let mut facts = ResponseFacts {
            status: Some(503),
            ..Default::default()
        };
        assert_eq!(
            classify_retry(&error, &facts),
            RetryPolicy::Retry {
                partial_output: false
            }
        );
        error.provider_code = None;
        let mut limit_error = crate::model::usage_limit::UsageLimit {
            kind: crate::model::usage_limit::LimitKind::UsageLimitReached,
            resets_at: chrono::Utc::now().timestamp() + 1,
            limit_window_minutes: None,
            locally_blocked: false,
        }
        .error();
        let mut limit_facts = ResponseFacts {
            status: Some(429),
            ..Default::default()
        };
        assert_eq!(
            classify_retry(&limit_error, &limit_facts),
            RetryPolicy::Retry {
                partial_output: false
            }
        );
        limit_error.usage_limit = None;
        assert_eq!(
            classify_retry(&limit_error, &limit_facts),
            RetryPolicy::Ineligible
        );
        limit_error.usage_limit = Some(Box::new(crate::model::usage_limit::UsageLimit {
            kind: crate::model::usage_limit::LimitKind::UsageLimitReached,
            resets_at: chrono::Utc::now().timestamp() + 1,
            limit_window_minutes: None,
            locally_blocked: false,
        }));
        limit_facts.retry_after = Some(Duration::from_secs(601));
        assert_eq!(
            classify_retry(&limit_error, &limit_facts),
            RetryPolicy::Deferred(Duration::from_secs(601))
        );
        error.provider_code = None;
        for seconds in [0, 600, 601] {
            facts.retry_after = Some(Duration::from_secs(seconds));
            assert_eq!(
                classify_retry(&error, &facts),
                if seconds > 600 {
                    RetryPolicy::Deferred(Duration::from_secs(seconds))
                } else {
                    RetryPolicy::Retry {
                        partial_output: false,
                    }
                }
            );
        }
    }
}

#[cfg(test)]
mod trim_budget_tests {
    use super::*;
    #[test]
    fn context_overflow_uses_the_normal_finite_budget() {
        let mut e = model::ModelError::new("provider", "fixture");
        e.provider_code = Some("context_length_exceeded".into());
        let mut facts = ResponseFacts::default();
        assert_eq!(
            classify_retry(&e, &facts),
            RetryPolicy::Retry {
                partial_output: false
            }
        );
        facts.retry_after = Some(Duration::from_secs(601));
        assert_eq!(
            classify_retry(&e, &facts),
            RetryPolicy::Deferred(Duration::from_secs(601))
        );
        facts.retry_after = None;
        facts.received = ResponseReceived::Output;
        assert_eq!(
            classify_retry(&e, &facts),
            RetryPolicy::Retry {
                partial_output: true,
            }
        );
        let mut b = RetryBudget::default();
        b.observe("t", 0);
        b.used = 8;
        assert_eq!(b.delay(8, Duration::from_secs(1), None), None);
        b.succeeded();
        assert_eq!(b.used, 0);
    }
    #[test]
    fn early_trim_separates_all_server_failures_and_header_timeouts_from_idle_or_upload_failures() {
        use model::http::{ConnectionReuse, HttpObservation, HttpPhase};
        let mut facts = ResponseFacts {
            http_observation: Some(HttpObservation {
                network_started: true,
                phase: HttpPhase::BodyReceive,
                request_body_bytes: 100,
                body_source_bytes_consumed: 100,
                header_elapsed_ms: Some(10),
                pool_generation: 0,
                connection_reuse: ConnectionReuse::FreshPool,
            }),
            ..Default::default()
        };
        for status in [500, 501, 502, 503, 504, 599] {
            facts.status = Some(status);
            assert!(eligible_trim_failure(
                &model::ModelError::new("provider", "fixture"),
                &facts,
            ));
            assert!(eligible_trim_failure(
                &model::ModelError::new("timeout", "error body stalled"),
                &facts,
            ));
        }
        facts.status = Some(429);
        assert!(!eligible_trim_failure(
            &model::ModelError::new("timeout", "rate limit body stalled"),
            &facts,
        ));
        facts.status = Some(200);
        assert!(!eligible_trim_failure(
            &model::ModelError::new("timeout", "body idle"),
            &facts,
        ));
        facts.status = None;
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::AwaitingHeadersDeliveryUnknown;
        assert!(eligible_trim_failure(
            &model::ModelError::new("timeout", "header timeout"),
            &facts,
        ));
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::UploadSourceIncomplete;
        facts
            .http_observation
            .as_mut()
            .unwrap()
            .body_source_bytes_consumed = 50;
        assert!(eligible_trim_failure(
            &model::ModelError::new("timeout", "upload incomplete"),
            &facts,
        ));
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::BeforeBodySource;
        facts
            .http_observation
            .as_mut()
            .unwrap()
            .body_source_bytes_consumed = 0;
        assert!(!eligible_trim_failure(
            &model::ModelError::new("transport", "connect failed"),
            &facts,
        ));
        let setup = ResponseFacts::default();
        let setup_error =
            model::ModelError::new("transport", "client reconstruction failed before sending");
        assert!(!eligible_trim_failure(&setup_error, &setup));
        assert!(
            matches!(
                classify_retry(&setup_error, &setup),
                RetryPolicy::Retry { .. }
            ),
            "failure before transport observation retains ordinary retry without spending trim"
        );
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::BodyReceive;
        facts.status = Some(503);
        facts.retry_after = Some(Duration::from_secs(601));
        assert!(!eligible_trim_failure(
            &model::ModelError::new("provider", "fixture"),
            &facts,
        ));
        facts.retry_after = None;
        facts.received = ResponseReceived::Output;
        assert!(eligible_trim_failure(
            &model::ModelError::new("provider", "fixture"),
            &facts,
        ));
        facts.received = ResponseReceived::None;
        facts.terminal_received = true;
        assert!(!eligible_trim_failure(
            &model::ModelError::new("provider", "fixture"),
            &facts,
        ));
    }
}
