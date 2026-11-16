//! Post-adoption data safety, independent of best-effort attempt diagnostics.
use super::*;
use crate::app::DataOwner;
use crate::protocol::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

mod record_tests;

async fn wait_for_driver_await_phase(
    phases: &mut tokio::sync::broadcast::Receiver<crate::control::DriverPhaseEvent>,
    thread: &str,
    run: &str,
    expected: crate::control::DriverAwaitPhase,
) {
    loop {
        match phases.recv().await {
            Ok(event) if event.thread == thread && event.run == run && event.phase == expected => {
                return;
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                panic!("test-only driver phase stream closed before {expected:?}")
            }
        }
    }
}

async fn wait_for_driver_attempt_phase(
    phases: &mut tokio::sync::broadcast::Receiver<crate::control::DriverPhaseEvent>,
    thread: &str,
    run: &str,
    expected: crate::control::DriverAwaitPhase,
) -> String {
    loop {
        match phases.recv().await {
            Ok(event) if event.thread == thread && event.run == run && event.phase == expected => {
                if let Some(attempt) = event.attempt {
                    return attempt;
                }
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                panic!("test-only driver phase stream closed before {expected:?}")
            }
        }
    }
}

async fn wait_for_http_future_first_poll(
    phases: &mut tokio::sync::broadcast::Receiver<crate::control::DriverPhaseEvent>,
    thread: &str,
    run: &str,
) -> (String, crate::control::HttpPollResult) {
    loop {
        match phases.recv().await {
            Ok(event) if event.thread == thread && event.run == run => {
                if let crate::control::DriverAwaitPhase::HttpFutureFirstPoll(result) = event.phase {
                    if let Some(attempt) = event.attempt {
                        return (attempt, result);
                    }
                }
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                panic!("test-only driver phase stream closed before the HTTP future's first poll")
            }
        }
    }
}

async fn wait_for_run_b_provider_request(
    control: &Control,
    thread: &str,
    run: &str,
    receive: &mut mpsc::Receiver<Incoming>,
    phases: &mut tokio::sync::broadcast::Receiver<crate::control::DriverPhaseEvent>,
    slot: &DriverSlot,
) -> Incoming {
    let mut events = control.subscribe();
    loop {
        let notified = slot.nudge.notified();
        tokio::pin!(notified);
        tokio::select! {
            request = receive.recv() => {
                return request.unwrap_or_else(|| panic!(
                    "provider body channel closed before Run B request; Thread={thread} Run={run}"
                ));
            }
            event = phases.recv() => match event {
                Ok(event) if event.thread == thread && event.run == run => {
                    eprintln!("Run B typed future progress: attempt={:?} phase={:?}", event.attempt, event.phase);
                    if matches!(event.phase,
                        crate::control::DriverAwaitPhase::HttpResponseFutureCompleted(crate::control::HttpPollResult::ReadyError))
                    {
                        panic!("Run B response future completed with typed error before any provider body: Thread={thread} Run={run} Attempt={:?}", event.attempt);
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!("driver phase stream closed while awaiting Run B provider request"),
            },
            event = events.recv() => match event {
                Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => {
                    let target = thread.to_owned();
                    let run_id = run.to_owned();
                    let run_live = control.store.database.call(move |db| Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND thread_id=?2 AND state='live')",
                        rusqlite::params![run_id, target], |row| row.get::<_, bool>(0),
                    )?)).await.unwrap();
                    if !run_live {
                        panic!("Run B ended before its provider request; Thread={thread} Run={run}");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!("engine event stream closed while awaiting Run B provider request"),
                _ => {}
            },
            _ = &mut notified => {
                let finished = {
                    let job = slot.proactive_job.lock().await;
                    job.as_ref().is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished)
                };
                if finished {
                    panic!("retained proactive job finished before Run B provider body; Thread={thread} Run={run}");
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    Storage,
    Cancellation,
    Remeasurement,
    /// #226: after the checkpoint every attempt-record and automatic app_meta
    /// write is rejected, so only the stage facts and provisional target remain.
    Persistent,
    /// #303: observe the current failed-attempt path and prove legacy W2/W3 (the activity witness is restored for the TUI indicator, #165)
    /// writes are absent rather than injecting faults into removed write sites.
    RetiredWritesAbsent,
    /// #226: reject a terminal attempt write where one remains; for a native
    /// summary, strict FINISH is already inside the adoption transaction.
    RejectFinal,
    /// #226: the stage fact itself is rejected before the stage adopts.
    RejectFact,
}
impl Fault {
    /// Faults whose checkpoint returns `Ok` with an uncancelled token: the
    /// driver legitimately proceeds to the next stage of the same attempt. On
    /// the summary branch an active spill is always followed by the active
    /// summary (compaction.md §2.6, §3 "要約の枝"), so these faults enter it and
    /// the selected request is answered after the checkpoint is released.
    fn continues_after_stage(self) -> bool {
        matches!(self, Fault::RejectFinal | Fault::Remeasurement)
    }
    /// Whether a final record fault allows normal attempt finalization to advance
    /// the reasoning epoch after the selected stage.
    fn continues(self) -> bool {
        matches!(self, Fault::RejectFinal | Fault::RejectFact)
    }
}

/// A fault is attached to one policy-reachable stage, not an independent pair
/// of booleans. All scenarios are triggered by latest provider input 200 at
/// threshold 200; only the separate first-known A comparison selects spill-only
/// versus spill-then-summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservationPhase {
    AtCommittedCheckpoint,
    AfterRejectedStageTransaction,
    AfterShutdownAndRestart,
}

fn expected_outcome(fault: Fault, scenario: Scenario, phase: ObservationPhase) -> &'static str {
    match phase {
        // Spill stages retain the shared driver's provisional record until its
        // terminal writer. Native GC summary stages include strict FINISH in the
        // same adoption transaction, so their committed checkpoint is already
        // terminally adopted before any later shutdown fault is injected.
        ObservationPhase::AtCommittedCheckpoint if scenario.summary() => "adopted",
        ObservationPhase::AtCommittedCheckpoint => "unknown",
        ObservationPhase::AfterRejectedStageTransaction => "failed",
        ObservationPhase::AfterShutdownAndRestart => match fault {
            // Native summary strict FINISH is part of the same transaction as
            // stage/path adoption. Post-stage faults cannot rewrite that final
            // fact to unknown/failed; RejectFact remains a precommit rollback.
            Fault::Persistent
            | Fault::RejectFinal
            | Fault::Storage
            | Fault::RetiredWritesAbsent
            | Fault::Remeasurement
                if scenario.summary() =>
            {
                "adopted"
            }
            // Remeasurement runs after a successful following summary FINISH
            // when this fault is armed at a spill checkpoint.
            Fault::Remeasurement if scenario == Scenario::ActiveSpill => "adopted",
            Fault::RejectFact => "failed",
            // Cancellation after active spill is observed by the closed-summary
            // entry. Pre-cancel returns before planning, so there is no active
            // fallback stage; the committed spill remains and outcome is cancelled.
            Fault::Cancellation if scenario == Scenario::ActiveSpill => "cancelled",
            // Other summary boundaries already completed atomic FINISH before
            // the post-stage cancellation hook runs.
            Fault::Cancellation => "adopted",
            Fault::Persistent | Fault::RejectFinal => "unknown",
            Fault::Storage | Fault::RetiredWritesAbsent | Fault::Remeasurement => "failed",
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    ClosedSpill,
    ClosedSummary,
    ActiveSpill,
    ActiveSummary,
}
impl Scenario {
    const ALL: [Self; 4] = [
        Self::ClosedSpill,
        Self::ClosedSummary,
        Self::ActiveSpill,
        Self::ActiveSummary,
    ];
    const THRESHOLD: u64 = 200;
    const FIRST_KNOWN_A: u64 = 100;
    const LATEST_PROVIDER_INPUT: u64 = 200;

    fn active(self) -> bool {
        matches!(self, Self::ActiveSpill | Self::ActiveSummary)
    }
    fn summary(self) -> bool {
        matches!(self, Self::ClosedSummary | Self::ActiveSummary)
    }
    fn target_percent(self) -> u64 {
        match self {
            Self::ClosedSpill => 50,
            Self::ClosedSummary | Self::ActiveSpill | Self::ActiveSummary => 20,
        }
    }
    fn target_tokens(self) -> u64 {
        Self::THRESHOLD * self.target_percent() / 100
    }
    fn stage(self) -> &'static str {
        match self {
            Self::ClosedSpill => "closed_spill",
            Self::ClosedSummary => "closed_summary",
            Self::ActiveSpill => "active_spill",
            Self::ActiveSummary => "active_summary",
        }
    }
    /// Whether the driver enters the active summary after this scenario's
    /// active-spill checkpoint is released (§3 summary branch).
    fn continues_to_active_summary(self, fault: Fault) -> bool {
        self == Self::ActiveSpill && fault.continues_after_stage()
    }
}

fn assert_trigger_evidence(trigger: &Value, scenario: Scenario) {
    let threshold = Scenario::THRESHOLD;
    let a = Scenario::FIRST_KNOWN_A;
    let target = scenario.target_tokens();
    assert_eq!(trigger["input_tokens"], Scenario::LATEST_PROVIDER_INPUT);
    assert_eq!(trigger["auto_at_tokens"], threshold);
    assert_eq!(trigger["target_percent"], scenario.target_percent());
    assert_eq!(trigger["target_tokens"], target);
    assert_eq!(trigger["a_input_tokens"], a);
    assert!(
        Scenario::LATEST_PROVIDER_INPUT >= threshold,
        "latest provider report must independently pass the trigger threshold"
    );
    match scenario {
        Scenario::ClosedSpill => assert!(a <= target, "A<=target selects closed spill only"),
        Scenario::ClosedSummary | Scenario::ActiveSpill | Scenario::ActiveSummary => {
            assert!(a > target, "A>target enters spill-then-summary policy")
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_limit_waits_for_the_same_proactive_job_without_a_second_summary_post() {
    crate::test_watchdog::watchdog("context-limit joins the active proactive job", async {
        let Fx {
            owner,
            control,
            target,
            mut receive,
            server,
            mut held_phase,
            ..
        } = prepare_held_summary(Scenario::ClosedSummary).await;
        let HeldSummaryPhase { summary } = held_phase
            .take()
            .expect("proactive summary reached its provider boundary");
        let normal = incoming(&control, &mut receive).await;
        assert_ne!(normal.body, summary.body);
        normal
            .reply
            .send((
                400,
                json!({"error":{"message":"context limit","code":"context_length_exceeded"}}),
            ))
            .unwrap();
        summary
            .reply
            .send((200, answer("background summary completed")))
            .unwrap();
        crate::test_watchdog::watchdog("same proactive job terminal", async {
            while !terminal_record_written(&control).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let thread = target.clone();
        let (path, summaries) = control
            .store
            .database
            .call(move |db| {
                let path = db
                    .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
                    .query_map([&thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let summaries = db.query_row(
                    "SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='summary'",
                    [&thread],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok((path, summaries))
            })
            .await
            .unwrap();
        assert!(summaries > 0, "the already-running summary was adopted: {path:?}");
        assert!(
            receive.try_recv().is_err(),
            "context-limit recovery did not launch another summary or replay the POST"
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_background_summary_survives_next_turn_and_adopts_at_current_boundary() {
    let mut trace_keepalive = None;
    let mut trace_thread = None;
    let result = crate::test_watchdog::watchdog("proactive summary request barrier", async {
        let mut fx = prepare_held_summary(Scenario::ClosedSummary).await;
        trace_thread = Some(fx.target.clone());
        trace_keepalive = Some(crate::control::subscribe_driver_await_phases());
        let Fx {
            root,
            url,
            limits,
            owner,
            control,
            target,
            receive,
            server,
            held_phase,
            ..
        } = &mut fx;
        let HeldSummaryPhase { summary } = held_phase
            .take()
            .expect("proactive summary reached its provider boundary");
        assert_eq!(summary.body["tools"], json!([]));
        let normal = incoming(control, receive).await;
        let before: Vec<String> = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    db.prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                    )?
                    .query_map([thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
                }
            })
            .await
            .unwrap();

        // This request reached the provider while the summary response was held.
        assert_ne!(
            normal.body, summary.body,
            "next request is not a duplicate summary POST"
        );
        let during_request: Vec<String> = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    db.prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                    )?
                    .query_map([thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
                }
            })
            .await
            .unwrap();
        assert_eq!(
            during_request, before,
            "no summary adoption during the in-flight normal request"
        );
        normal
            .reply
            .send((200, answer_with_usage("normal request completed", 100)))
            .unwrap();
        settled(control, target).await;
        let after_normal: Vec<String> = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    db.prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                    )?
                    .query_map([thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
                }
            })
            .await
            .unwrap();
        assert!(after_normal.len() >= before.len(), "ordinary request history remains on the selected path");
        let selected_summaries: i64 = control.store.database.call({
            let thread = target.clone();
            move |db| Ok(db.query_row(
                "SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='summary'",
                [thread], |row| row.get(0),
            )?)
        }).await.unwrap();
        assert_eq!(selected_summaries, 0, "held summary is not adopted before Run B");


        // Let Run A finish naturally while its Thread-owned R job is still
        // held, then admit a new Run on the same Thread.
        settled(control, target).await;
        let run_a_slot = {
            let drivers = control.live.test_drivers().lock().await;
            drivers
                .get(target)
                .cloned()
                .expect("Run A slot remains while its proactive job is retained")
        };
        let _run_a_retired = wait_for_driver_retired(control, target, &run_a_slot.id).await;
        let mut driver_phases = crate::control::subscribe_driver_await_phases();
        let accepted: MessageAccepted = serde_json::from_value(
            send(control, target, "new Run while previous R job is held").await,
        )
        .unwrap();
        assert_eq!(accepted.assignment, Assignment::NewRun, "Run B assignment: {accepted:?}");
        assert_eq!(accepted.delivery_status, DeliveryStatus::Accepted);
        let run_id = accepted.run_id.clone().expect("NewRun returns its Run ID");
        let turn_id = accepted.turn_id.clone().expect("NewRun returns its Turn ID");
        let message_id = accepted.message_id.clone();
        let thread_id = target.clone();
        let query_thread = thread_id.clone();
        let query_run = run_id.clone();
        let query_turn = turn_id.clone();
        let event_thread = thread_id.clone();
        let event_run = run_id.clone();
        let event_turn = turn_id.clone();
        let (run_state, turn_state, receipt_state, message_turn, turn_started_events) = control
            .store
            .database
            .call(move |db| {
                let run_state: String = db.query_row(
                    "SELECT state FROM run WHERE id=?1 AND thread_id=?2",
                    rusqlite::params![query_run, query_thread],
                    |row| row.get(0),
                )?;
                let turn_state: String = db.query_row(
                    "SELECT state FROM turn WHERE id=?1 AND run_id=?2",
                    rusqlite::params![query_turn, query_run],
                    |row| row.get(0),
                )?;
                let (receipt_state, message_turn): (String, Option<String>) = db.query_row(
                    "SELECT r.state,n.turn_id FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.message_node_id=?1",
                    [&message_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let turn_started_events: i64 = db.query_row(
                    "SELECT count(*) FROM event WHERE target_thread_id=?1 AND json_extract(payload_ref_json,'$.kind')='turn_started' AND json_extract(payload_ref_json,'$.change.run_id')=?2 AND json_extract(payload_ref_json,'$.change.turn_id')=?3",
                    rusqlite::params![event_thread, event_run, event_turn],
                    |row| row.get(0),
                )?;
                Ok((run_state, turn_state, receipt_state, message_turn, turn_started_events))
            })
            .await
            .unwrap();
        let slot = {
            let drivers = control.live.test_drivers().lock().await;
            drivers.get(target).cloned()
        };
        let slot_facts = if let Some(slot) = slot.as_ref() {
            let job = slot.proactive_job.lock().await;
            Some((
                slot.run_id.clone(),
                slot.driver_active.load(Ordering::Acquire),
                job.is_some(),
                job.as_ref().is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished),
            ))
        } else {
            None
        };
        eprintln!("U2 after Deliver::Next acceptance={accepted:?} db=(run:{run_state},turn:{turn_state},receipt:{receipt_state},message_turn:{message_turn:?},turn_started_events:{turn_started_events}) slot={slot_facts:?}");
        assert_eq!(run_state, "live", "accepted NewRun must persist live Run state");
        assert!(matches!(turn_state.as_str(), "preparing" | "running"), "NewRun Turn state={turn_state}");
        assert_eq!(receipt_state, "accepted");
        assert_eq!(message_turn.as_deref(), Some(accepted.turn_id.as_deref().unwrap()));
        assert_eq!(turn_started_events, 1, "Run B has a persisted turn_started event");
        let slot = slot.expect("accepted Run B has a registered Thread driver slot");
        wait_for_driver_await_phase(
            &mut driver_phases,
            target,
            &run_id,
            crate::control::DriverAwaitPhase::RequestFutureAwait,
        )
        .await;
        let live_attempt = wait_for_driver_attempt_phase(
            &mut driver_phases,
            target,
            &run_id,
            crate::control::DriverAwaitPhase::LiveAdmissionEntered,
        )
        .await;
        let granted_attempt = wait_for_driver_attempt_phase(
            &mut driver_phases,
            target,
            &run_id,
            crate::control::DriverAwaitPhase::LiveAdmissionGranted,
        )
        .await;
        let (first_polled_attempt, first_poll_outcome) =
            wait_for_http_future_first_poll(&mut driver_phases, target, &run_id).await;
        assert_eq!(live_attempt, granted_attempt, "live admission events refer to the same persisted attempt");
        assert_eq!(granted_attempt, first_polled_attempt, "HTTP first poll belongs to the live-admitted attempt");
        eprintln!("Run B HTTP response future first-poll result: {first_poll_outcome:?}");
        let overflow_request = wait_for_run_b_provider_request(
            control,
            target,
            &run_id,
            receive,
            &mut driver_phases,
            &slot,
        )
        .await;
        assert_ne!(overflow_request.body, summary.body, "Run B proceeds without cancelling R");
        overflow_request
            .reply
            .send((400, json!({"error":{"message":"context limit","code":"context_length_exceeded"}})))
            .unwrap();
        let during_overflow: Vec<String> = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    db.prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                    )?
                    .query_map([thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
                }
            })
            .await
            .unwrap();
        assert!(
            during_overflow.starts_with(&before),
            "same R remains pending while Run B's current suffix may append during overflow wait"
        );
        let summaries_during_overflow: i64 = control.store.database.call({
            let thread = target.clone();
            move |db| Ok(db.query_row(
                "SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='summary'",
                [thread], |row| row.get(0),
            )?)
        }).await.unwrap();
        assert_eq!(summaries_during_overflow, 0, "the held job has not adopted before its response");
        assert!(receive.try_recv().is_err(), "Run B does not start a second summary while joining Run A's job");

        let before_release: Vec<String> = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    db.prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                    )?
                    .query_map([thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
                }
            })
            .await
            .unwrap();
        assert!(before_release.len() >= before.len());
        summary
            .reply
            .send((200, answer("background summary completed")))
            .unwrap();
        settled(control, target).await;
        assert!(
            receive.try_recv().is_err(),
            "an exhausted context-limit ends in the established recovery path without a duplicate POST"
        );
        crate::test_watchdog::watchdog("background summary FINISH", async {
            while !terminal_record_written(control).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let after: Vec<String> = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    db.prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                    )?
                    .query_map([thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
                }
            })
            .await
            .unwrap();
        assert!(
            after.len() < before_release.len(),
            "closed R is adopted while later path remains"
        );
        assert_eq!(
            after.last(),
            before_release.last(),
            "later context-overflow request suffix is preserved"
        );
        control.shutdown().await.unwrap();
        server.abort();
        let _ = (root, url, limits, owner);
    })
    .await;
    if result.is_err()
        && let Some(thread) = trace_thread.as_deref()
    {
        crate::control::record_test_watchdog_expired(thread);
    }
    drop(trace_keepalive);
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_naturally_idle_thread_cancels_and_joins_its_retained_summary() {
    crate::test_watchdog::watchdog("ThreadDelete drains the retained proactive job", async {
        let Fx {
            owner,
            control,
            target,
            server,
            mut held_phase,
            mut receive,
            ..
        } = prepare_held_summary(Scenario::ClosedSummary).await;
        let HeldSummaryPhase { summary } = held_phase
            .take()
            .expect("proactive summary reached its provider boundary");
        assert_eq!(summary.body["tools"], json!([]));
        let normal = incoming(&control, &mut receive).await;
        normal
            .reply
            .send((200, answer_with_usage("finish Run A", 100)))
            .unwrap();
        settled(&control, &target).await;
        let slot_id = {
            let drivers = control.live.test_drivers().lock().await;
            drivers
                .get(&target)
                .map(|slot| slot.id.clone())
                .expect("natural Run slot remains until retire_driver processes it")
        };
        let slot = wait_for_driver_retired(&control, &target, &slot_id).await;
        assert!(!slot.driver_active.load(Ordering::Acquire));
        let job_arc = slot.proactive_job.clone();
        let cancel = {
            let job = job_arc.lock().await;
            job.as_ref()
                .expect("the retained summary has one job handle")
                .cancel
                .clone()
        };
        let before: i64 = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    Ok(db.query_row(
                        "SELECT count(*) FROM thread_replacement WHERE thread_id=?1",
                        [thread],
                        |row| row.get(0),
                    )?)
                }
            })
            .await
            .unwrap();
        let operation = format!("{}:delete-held-proactive", control.store.instance_id);
        let make_request = || {
            Request::new(
                operation.clone(),
                Command::ThreadDelete(ThreadDeleteArgs {
                    thread_id: target.clone(),
                    descendants: false,
                    preview_marker: None,
                }),
            )
        };
        let deleted = control.request(make_request()).await;
        assert!(deleted.ok, "{:?}", deleted.error);
        let replay = control.request(make_request()).await;
        assert!(replay.ok, "{:?}", replay.error);
        assert_eq!(
            replay.result, deleted.result,
            "receipt replay returns saved delete result"
        );
        assert!(
            cancel.is_cancelled(),
            "committed delete cancels the Thread-owned provider job"
        );
        assert!(
            job_arc.lock().await.is_none(),
            "delete effect joins and takes the job exactly once"
        );
        assert!(
            control
                .live
                .test_drivers()
                .lock()
                .await
                .get(&target)
                .is_none(),
            "deleted idle Thread slot is pruned after its owned job joins"
        );
        let after: i64 = control
            .store
            .database
            .call({
                let thread = target.clone();
                move |db| {
                    Ok(db.query_row(
                        "SELECT count(*) FROM thread_replacement WHERE thread_id=?1",
                        [thread],
                        |row| row.get(0),
                    )?)
                }
            })
            .await
            .unwrap();
        assert_eq!(
            after, before,
            "no held result adopts after committed Thread deletion"
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_thread_delete_cancels_and_joins_idle_thread_job_once() {
    crate::test_watchdog::watchdog("delete effect joins the retained Thread job", async {
        let root = tempfile::tempdir().unwrap();
        let owner = proactive_owner(root.path(), "http://127.0.0.1:9/v1", "").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        let owner_stop = tokio_util::sync::CancellationToken::new();
        let slot = control
            .live
            .register_or_nudge_driver(&target, "naturally-ended-run".into(), &owner_stop, false)
            .await
            .unwrap();
        slot.driver_active.store(false, Ordering::Release);
        let cancel = tokio_util::sync::CancellationToken::new();
        let task_cancel = cancel.clone();
        let joined = Arc::new(AtomicBool::new(false));
        let joined_by_task = joined.clone();
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            joined_by_task.store(true, Ordering::Release);
            Ok(crate::control::driver::proactive::ProactiveOutcome::NotStarted)
        });
        let job_arc = slot.proactive_job.clone();
        *job_arc.lock().await = Some(
            crate::control::driver::proactive::BackgroundJob::test_started(cancel.clone(), task),
        );
        assert!(
            !control
                .live
                .remove_driver_if_current(&target, &slot.id)
                .await
        );

        let operation = format!("{}:delete-idle-job", control.store.instance_id);
        let make_request = || {
            Request::new(
                operation.clone(),
                Command::ThreadDelete(ThreadDeleteArgs {
                    thread_id: target.clone(),
                    descendants: false,
                    preview_marker: None,
                }),
            )
        };
        let first = control.request(make_request()).await;
        assert!(first.ok, "{:?}", first.error);
        let replay = control.request(make_request()).await;
        assert!(replay.ok, "{:?}", replay.error);
        assert_eq!(replay.result, first.result);
        assert!(cancel.is_cancelled(), "committed deletion cancels the job");
        assert!(
            joined.load(Ordering::Acquire),
            "delete effect joins the job before returning"
        );
        assert!(
            job_arc.lock().await.is_none(),
            "job handle is taken exactly once"
        );
        assert!(
            control
                .live
                .test_drivers()
                .lock()
                .await
                .get(&target)
                .is_none()
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spill_off_skips_automatic_spill_stages_and_adopts_summary() {
    for scenario in [Scenario::ClosedSummary, Scenario::ActiveSummary] {
        let fx = prepare_with_options(Fault::Cancellation, scenario, true, true).await;
        let Fx {
            control,
            target,
            mut receive,
            server,
            ..
        } = fx;
        assert!(receive.try_recv().is_err());
        let path: Vec<String> = control
            .store
            .database
            .call({
                let target = target.clone();
                move |db| {
                    db.prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                    )?
                    .query_map([&target], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
                }
            })
            .await
            .unwrap();
        assert!(!path.is_empty(), "summary adoption retains a real path");
        control.shutdown().await.unwrap();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_spill_uses_effective_thread_target_and_captures_real_stage() {
    crate::test_watchdog::watchdog("closed spill trigger tuple", async {
        boundary(Fault::Cancellation, Scenario::ClosedSpill).await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_summary_cancellation_captures_committed_boundary() {
    crate::test_watchdog::watchdog("active summary committed boundary", async {
        boundary(Fault::Cancellation, Scenario::ActiveSummary).await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_storage_boundaries() {
    boundaries(Fault::Storage).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_cancellation_boundaries() {
    boundaries(Fault::Cancellation).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_remeasurement_boundaries() {
    boundaries(Fault::Remeasurement).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_postcommit_retired_writes_are_absent() {
    boundaries(Fault::RetiredWritesAbsent).await;
}

async fn boundaries(fault: Fault) {
    crate::test_watchdog::watchdog("automatic postcommit data safety", async {
        for scenario in Scenario::ALL {
            boundary(fault, scenario).await;
        }
    })
    .await
    .unwrap();
}

async fn prepare(fault: Fault, scenario: Scenario) -> Fx {
    prepare_with_thread_override(fault, scenario, true).await
}

async fn prepare_with_thread_override(
    fault: Fault,
    scenario: Scenario,
    set_thread_percent: bool,
) -> Fx {
    prepare_with_options(fault, scenario, set_thread_percent, false).await
}

async fn prepare_with_options(
    fault: Fault,
    scenario: Scenario,
    set_thread_percent: bool,
    spill_off: bool,
) -> Fx {
    prepare_with_mode(fault, scenario, set_thread_percent, spill_off, false).await
}

async fn prepare_held_summary(scenario: Scenario) -> Fx {
    prepare_with_mode(Fault::Cancellation, scenario, true, true, true).await
}

async fn prepare_with_mode(
    fault: Fault,
    scenario: Scenario,
    set_thread_percent: bool,
    spill_off: bool,
    hold_summary: bool,
) -> Fx {
    eprintln!(
        "automatic boundary {fault:?} scenario={scenario:?} T=200 P={} A=100 input=200 target={}",
        scenario.target_percent(),
        scenario.target_tokens()
    );
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, mut heads, server) = proxy().await;
    let limits = format!(
        "auto_compact_target_percent={}\n[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n",
        scenario.target_percent()
    );
    let owner = proactive_owner(root.path(), &url, &limits).await;
    let control = Control::new(&owner, root.path().into());
    let target = thread(&control).await;
    if set_thread_percent {
        request(
            &control,
            Command::ThreadSet(Box::new(ThreadSetArgs::summary_settings(
                target.clone(),
                Some(scenario.target_percent().to_string()),
                None,
            ))),
        )
        .await;
    }
    if spill_off {
        request(
            &control,
            Command::ThreadSet(Box::new(ThreadSetArgs {
                thread_id: Some(target.clone()),
                spill: Some("off".into()),
                ..Default::default()
            })),
        )
        .await;
    }
    let stored_percent: Option<i64> = control
        .store
        .database
        .call({
            let target = target.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT auto_compact_target_percent FROM thread WHERE id=?1",
                    [target],
                    |row| row.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(
        stored_percent,
        set_thread_percent.then_some(scenario.target_percent() as i64),
        "per-Thread policy override is explicit; inherited case remains NULL"
    );
    if !scenario.active() {
        for text in ["hi", "yo"] {
            send(&control, &target, text).await;
            incoming(&control, &mut receive)
                .await
                .reply
                .send((200, answer("ok")))
                .unwrap();
            settled(&control, &target).await;
        }
    }
    send(&control, &target, &"protected current ".repeat(15_000)).await;
    let command = if cfg!(windows) {
        "'0' * 50000"
    } else {
        "printf '%050000d' 0"
    };
    // The first completed usage=100 tool-call report establishes immutable A.
    // Both seeded whole groups are committed before the separate input=200
    // report below can enter maybe_compact.
    for index in 0..2 {
        incoming(&control, &mut receive).await.reply.send((200, json!({"id":format!("boundary-{index}"),"status":"completed","usage":{"input_tokens":100,"output_tokens":1},"output":[{"type":"function_call","call_id":format!("boundary-{index}"),"name":"shell","arguments":json!({"command":command}).to_string()}]}))).unwrap();
    }
    let request_boundary = incoming(&control, &mut receive).await;
    // Closed scenarios complete this trigger report as plain text, then open a
    // new Turn with an eligible closed prefix. Active scenarios keep this Turn
    // live with the ordinary tool continuation; the summary POST, if selected,
    // is separately required to carry tools=[].
    let reached = Arc::new(AtomicBool::new(false));
    let checkpoint_reached = Arc::new(AtomicBool::new(false));
    let release_checkpoint = Arc::new(tokio::sync::Notify::new());
    let stage = scenario.stage();
    if !spill_off {
        *control.automatic_fault.lock().unwrap() = Some(crate::control::AutomaticFault {
            stage,
            kind: match fault {
                Fault::Cancellation => 0,
                Fault::Storage => 1,
                Fault::Remeasurement => 2,
                Fault::Persistent => 4,
                Fault::RejectFinal => 6,
                Fault::RejectFact => 8,
                Fault::RetiredWritesAbsent => 9,
            },
            reached: reached.clone(),
            checkpoint_reached: checkpoint_reached.clone(),
            release_checkpoint: release_checkpoint.clone(),
        });
    }
    let kind = if scenario.summary() {
        "summary"
    } else {
        "spill"
    };
    let scope = if scenario.active() && scenario.summary() {
        " AND json_extract(n.payload_json,'$.summary_scope') IN ('active_turn_middle','active_turn_prefix')"
    } else {
        ""
    };
    // Capture immutable witnesses in the adoption transaction. Fault injection
    // itself runs after COMMIT, before the next driver stage.
    // Initial and committed epoch/path witnesses are captured by the same
    // adoption transaction: OLD is the pre-stage epoch, NEW is the commit.
    control.store.database.call(move |db| {
        db.execute_batch(&format!(r#"
            CREATE TABLE fault_boundary(path TEXT, revision INTEGER, epoch INTEGER);
            CREATE TABLE fault_initial(epoch INTEGER);
            CREATE TABLE fault_begin(epoch INTEGER);
            CREATE TABLE fault_originals AS SELECT * FROM node;
            CREATE TABLE fault_nodes AS SELECT * FROM node WHERE 0;
            CREATE TABLE fault_replacements AS SELECT * FROM replacement WHERE 0;
            CREATE TABLE fault_sources AS SELECT * FROM replacement_source WHERE 0;
            CREATE TRIGGER fault_adopt AFTER UPDATE OF selection_revision ON thread
            WHEN NOT EXISTS(SELECT 1 FROM fault_boundary)
            AND EXISTS(SELECT 1 FROM model_attempt a JOIN turn u ON u.id=a.turn_id
                       WHERE u.thread_id=NEW.id
                       AND json_type(a.error_json,'$.automatic_trim_attempt')='object'
                       AND json_extract(a.error_json,'$.automatic_trim_attempt.provisional')=1)
            AND EXISTS(SELECT 1 FROM thread_path p JOIN node n ON n.id=p.node_id
                       WHERE p.thread_id=NEW.id AND n.kind='{kind}'{scope})
            BEGIN
              INSERT INTO fault_initial SELECT OLD.reasoning_epoch
                WHERE NOT EXISTS(SELECT 1 FROM fault_initial);
              INSERT INTO fault_boundary SELECT
                (SELECT json_group_array(node_id) FROM (SELECT node_id FROM thread_path WHERE thread_id=NEW.id ORDER BY ordinal)),
                NEW.selection_revision, NEW.reasoning_epoch;
              INSERT INTO fault_nodes SELECT * FROM node;
              INSERT INTO fault_replacements SELECT * FROM replacement;
              INSERT INTO fault_sources SELECT * FROM replacement_source;
            END;
            -- Thread epoch when the attempt's provisional record is begun, the
            -- reference when no stage of the attempt ever commits.
            CREATE TRIGGER fault_begin AFTER UPDATE OF error_json ON model_attempt
            WHEN NOT EXISTS(SELECT 1 FROM fault_begin)
            AND json_extract(NEW.error_json,'$.automatic_trim_attempt.provisional')=1
            AND json_type(OLD.error_json,'$.automatic_trim_attempt') IS NULL
            BEGIN INSERT INTO fault_begin SELECT t.reasoning_epoch FROM thread t JOIN turn u ON u.thread_id=t.id WHERE u.id=NEW.turn_id; END;
            CREATE TRIGGER fault_epoch AFTER UPDATE OF reasoning_epoch ON thread
            WHEN EXISTS(SELECT 1 FROM fault_boundary)
            BEGIN UPDATE fault_boundary SET epoch=NEW.reasoning_epoch; END;
            -- A later stage of the same provisional attempt (active summary after
            -- the selected active spill) re-captures the boundary inside its own
            -- adoption transaction, where the stage fact follows path and epoch.
            CREATE TRIGGER fault_restage AFTER UPDATE OF error_json ON model_attempt
            WHEN EXISTS(SELECT 1 FROM fault_boundary)
            AND json_extract(NEW.error_json,'$.automatic_trim_attempt.provisional')=1
            AND (SELECT count(*) FROM json_each(NEW.error_json,'$.automatic_trim_attempt.committed_stages'))
              > (SELECT count(*) FROM json_each(OLD.error_json,'$.automatic_trim_attempt.committed_stages'))
            BEGIN
              UPDATE fault_boundary SET
                path=(SELECT json_group_array(node_id) FROM (SELECT p.node_id FROM thread_path p JOIN turn u ON u.thread_id=p.thread_id WHERE u.id=NEW.turn_id ORDER BY p.ordinal)),
                revision=(SELECT t.selection_revision FROM thread t JOIN turn u ON u.thread_id=t.id WHERE u.id=NEW.turn_id),
                epoch=(SELECT t.reasoning_epoch FROM thread t JOIN turn u ON u.thread_id=t.id WHERE u.id=NEW.turn_id);
            END;
        "#))?;
        Ok(())
    }).await.unwrap();
    // #226: faults that must precede the stage are installed before it runs.
    let (hit_flag, stage_name) = (reached.clone(), stage);
    if fault == Fault::RejectFact {
        control.store.database.call(move |db| {
            db.create_scalar_function("fault_fact_hit", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
                hit_flag.store(true, Ordering::SeqCst);
                Ok(1i64)
            })?;
            db.execute_batch(&format!("CREATE TEMP TRIGGER fault_fact BEFORE UPDATE OF error_json ON model_attempt
                WHEN json_type(NEW.error_json,'$.automatic_trim_attempt.committed_stages.{stage_name}')='object'
                BEGIN SELECT fault_fact_hit(); SELECT RAISE(ABORT,'injected stage fact rejection'); END;"))?;
            Ok(())
        }).await.unwrap();
    }
    let guard_before = guard_rows(&control).await;
    request_boundary.reply.send((200, json!({"id":"boundary-small","status":"completed","usage":{"input_tokens":if scenario.active() || hold_summary {Scenario::LATEST_PROVIDER_INPUT} else {Scenario::FIRST_KNOWN_A},"output_tokens":1},"output":[{"type":"function_call","call_id":"boundary-small","name":"shell","arguments":json!({"command":"echo last"}).to_string()}]}))).unwrap();
    if !scenario.active() && !hold_summary {
        incoming(&control, &mut receive)
            .await
            .reply
            .send((
                200,
                answer_with_usage("done", Scenario::LATEST_PROVIDER_INPUT),
            ))
            .unwrap();
        settled(&control, &target).await;
        let facts = admission_boundary_facts(&control, &target).await;
        assert!(
            facts.latest_attempt_id.is_some(),
            "closed trigger report must have an attempt witness: {facts:?}"
        );
        if hold_summary {
            assert_eq!(facts.input_tokens, Some(Scenario::LATEST_PROVIDER_INPUT));
            assert_eq!(facts.next_request_pending, Some(false));
            assert!(
                facts.slot_exists && facts.job_exists && !facts.job_finished,
                "Run A must retain the eligible summary job before it naturally retires: {facts:?}"
            );
        }
        eprintln!("U2 closed trigger before continuation send: {facts:?}");
        let accepted: MessageAccepted =
            serde_json::from_value(send(&control, &target, "next turn").await).unwrap();
        eprintln!(
            "U2 trigger continuation receipt: assignment={:?} run_id={:?} turn_id={:?} delivery={:?}",
            accepted.assignment, accepted.run_id, accepted.turn_id, accepted.delivery_status
        );
    }
    let mut held_summary = None;
    let held_slot = if hold_summary {
        let drivers = control.live.test_drivers().lock().await;
        Some(
            drivers
                .get(&target)
                .cloned()
                .expect("Run A owns its Thread slot"),
        )
    } else {
        None
    };
    if scenario.summary() {
        let mut first_normal_facts_recorded = false;
        let mut observed_heads = Vec::new();
        loop {
            let event = if let Some(slot) = held_slot.as_ref() {
                next_held_fixture_event(&control, &target, &mut receive, &mut heads, slot).await
            } else {
                HeldFixtureEvent::Request(incoming(&control, &mut receive).await)
            };
            let request = match event {
                HeldFixtureEvent::Request(request) => request,
                HeldFixtureEvent::Header(ProxyRequestEvent::Accepted { request_id, peer }) => {
                    eprintln!("U2 mock listener accepted connection: id={request_id} peer={peer}");
                    continue;
                }
                HeldFixtureEvent::Header(ProxyRequestEvent::Headers(head)) => {
                    assert!(
                        head.request_line.starts_with("POST /v1/responses "),
                        "unexpected accepted request line: {head:?}"
                    );
                    assert!(
                        head.content_length.is_some(),
                        "fixture expects Content-Length framing: {head:?}"
                    );
                    assert!(
                        head.transfer_encoding.is_none(),
                        "fixture does not decode chunked request bodies: {head:?}"
                    );
                    assert!(
                        head.content_encoding.is_none(),
                        "fixture does not decode compressed request bodies: {head:?}"
                    );
                    eprintln!("U2 accepted HTTP request headers: {head:?}");
                    observed_heads.push(head);
                    continue;
                }
                HeldFixtureEvent::JobFinished(facts) => panic!(
                    "proactive job terminated before both held provider requests were captured: {facts:?}"
                ),
            };
            if hold_summary
                && !observed_heads
                    .iter()
                    .any(|head| head.request_id == request.request_id)
            {
                loop {
                    match heads
                        .recv()
                        .await
                        .expect("completed body has accepted connection/header events")
                    {
                        ProxyRequestEvent::Accepted { request_id, peer } => {
                            eprintln!(
                                "U2 mock listener accepted connection: id={request_id} peer={peer}"
                            );
                        }
                        ProxyRequestEvent::Headers(head) => {
                            assert!(head.request_line.starts_with("POST /v1/responses "));
                            assert!(head.content_length.is_some());
                            assert!(head.transfer_encoding.is_none());
                            assert!(head.content_encoding.is_none());
                            eprintln!("U2 accepted HTTP request headers: {head:?}");
                            let matched = head.request_id == request.request_id;
                            observed_heads.push(head);
                            if matched {
                                break;
                            }
                        }
                    }
                }
            }
            if request.body["tools"] == json!([]) {
                if hold_summary {
                    held_summary = Some(request);
                    break;
                } else {
                    let summary_request = request;
                    let diagnostic = control
                        .store
                        .database
                        .call({
                            let target = target.clone();
                            move |db| {
                                let attempt: Option<(String, String)> = db
                                    .query_row(
                                        "SELECT a.id,COALESCE(a.error_json,'{}') FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 ORDER BY a.rowid DESC LIMIT 1",
                                        [&target],
                                        |row| Ok((row.get(0)?, row.get(1)?)),
                                    )
                                    .optional()?;
                                let path: Vec<String> = db
                                    .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
                                    .query_map([&target], |row| row.get(0))?
                                    .collect::<rusqlite::Result<_>>()?;
                                let (usage, _) = crate::compaction::context_usage::prepare_context_usage(
                                    crate::compaction::context_usage::read_context_usage_tx(db, &target, None)?,
                                );
                                let report_attempt = usage["last_successful_attempt"]["attempt_id"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_owned();
                                let outputs = db
                                    .prepare("SELECT o.node_id,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?2 AND p.node_id=o.node_id) FROM output_item o WHERE o.attempt_id=?1 ORDER BY o.node_id")?
                                    .query_map(rusqlite::params![report_attempt, target], |row| {
                                        Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                                    })?
                                    .collect::<rusqlite::Result<Vec<_>>>()?;
                                let replacements = db
                                    .prepare("SELECT r.id,r.created_at,r.state,r.format_version,r.prompt_version,r.output_node_id,n.kind,json_extract(n.payload_json,'$.summary_scope'),COALESCE((SELECT json_group_array(source_node_id) FROM (SELECT source_node_id FROM replacement_source WHERE replacement_id=r.id ORDER BY ordinal)),'[]') FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND r.created_at>(SELECT started_at FROM model_attempt WHERE id=?2) ORDER BY r.created_at,r.id")?
                                    .query_map(rusqlite::params![target, report_attempt], |row| Ok((
                                        row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                                        row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                                        row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                                        row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?,
                                        row.get::<_, String>(8)?,
                                    )))?
                                    .collect::<rusqlite::Result<Vec<_>>>()?;
                                let report_facts: Option<(String, Option<String>, String)> = db.query_row(
                                    "SELECT started_at,ended_at,COALESCE(error_json,'{}') FROM model_attempt WHERE id=?1",
                                    [&report_attempt], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
                                ).optional()?;
                                let a = crate::compaction::a_state::read(db, &target)?;
                                let a = match crate::compaction::a_state::reading(a.as_ref()) {
                                    crate::compaction::a_state::Reading::Known(tokens) => Some(tokens),
                                    _ => None,
                                };
                                Ok((attempt, path, usage, outputs, replacements, report_facts, a))
                            }
                        })
                        .await
                        .unwrap();
                    if scenario.active() && !spill_off {
                        assert_eq!(
                            diagnostic.2["last_successful_attempt"]["next_request_pending"],
                            true,
                            "the active spill committed after the trigger attempt; witnesses: latest row={:?}; usage/report attempt={:?}; output nodes/path={:?}; replacements after report attempt={:?}",
                            diagnostic.0,
                            diagnostic.2["last_successful_attempt"],
                            diagnostic.3,
                            diagnostic.4,
                        );
                        assert!(
                            !diagnostic.3.is_empty()
                                && diagnostic.3.iter().all(|(_, selected)| *selected),
                            "the trigger attempt's output items remain selected on the path: {:?}",
                            diagnostic.3
                        );
                        assert!(
                            !diagnostic.4.is_empty(),
                            "active spill must leave typed replacement facts: {:?}",
                            diagnostic.4
                        );
                    }
                    assert_eq!(
                        summary_request.body["tools"],
                        json!([]),
                        "expected tool-free GC summary POST; unexpected provider request body={:?}, latest attempt/stage={:?}, selected path={:?}, trigger usage={:?}, report outputs/path={:?}, typed replacement facts={:?}, report attempt facts={:?}, current A={:?}",
                        summary_request.body,
                        diagnostic.0,
                        diagnostic.1,
                        diagnostic.2,
                        diagnostic.3,
                        diagnostic.4,
                        diagnostic.5,
                        diagnostic.6,
                    );
                    summary_request
                        .reply
                        .send((200, answer("Short adopted summary.")))
                        .unwrap();
                }
                if !hold_summary {
                    break;
                }
            } else {
                if hold_summary && !first_normal_facts_recorded {
                    let facts = admission_boundary_facts(&control, &target).await;
                    assert!(
                        facts.latest_attempt_id.is_some(),
                        "ordinary request must have report witness: {facts:?}"
                    );
                    assert!(
                        facts.slot_exists && facts.job_exists,
                        "preflight must publish the eligible job before normal dispatch: {facts:?}"
                    );
                    let thread_for_progress = target.clone();
                    let (automatic_attempts, active_witness): (Vec<(String, String, String)>, Option<String>) = control
                        .store
                        .database
                        .call(move |db| {
                            let attempts = db.prepare(
                                "SELECT a.id,a.state,COALESCE(json_extract(a.error_json,'$.automatic_trim_attempt'),'null') FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND json_type(a.error_json,'$.automatic_trim_attempt')='object' ORDER BY a.rowid DESC LIMIT 4",
                            )?.query_map([&thread_for_progress], |row| {
                                Ok((row.get(0)?,row.get(1)?,row.get(2)?))
                            })?.collect::<rusqlite::Result<Vec<_>>>()?;
                            let key = crate::compaction::CompactionKeys::new(&thread_for_progress).automatic_active();
                            let active = db.query_row("SELECT value FROM app_meta WHERE key=?1",[key],|row|row.get(0)).optional()?;
                            Ok((attempts,active))
                        })
                        .await
                        .unwrap();
                    let input = request.body["input"]
                        .as_array()
                        .expect("Responses request carries an input array");
                    let call_ids: Vec<&str> = input
                        .iter()
                        .filter(|item| item["type"] == "function_call")
                        .filter_map(|item| item["call_id"].as_str())
                        .collect();
                    let result_ids: Vec<&str> = input
                        .iter()
                        .filter(|item| item["type"] == "function_call_output")
                        .filter_map(|item| item["call_id"].as_str())
                        .collect();
                    assert!(
                        call_ids.contains(&"boundary-small"),
                        "the actual normal request must carry Run A's open tool call: {call_ids:?}"
                    );
                    assert!(
                        result_ids.contains(&"boundary-small"),
                        "the actual normal request must carry the matching saved tool result: {result_ids:?}"
                    );
                    let paired_head = observed_heads
                        .iter()
                        .find(|head| head.request_id == request.request_id)
                        .expect("every completed request body has its accepted HTTP head");
                    assert!(paired_head.request_line.starts_with("POST /v1/responses "));
                    assert_eq!(paired_head.content_length, Some(request.body_bytes));
                    assert!(paired_head.transfer_encoding.is_none());
                    assert!(paired_head.content_encoding.is_none());
                    eprintln!(
                        "U2 first ordinary request HTTP head/body: head={paired_head:?} body_request_id={} body_bytes={} facts={facts:?} call_ids={call_ids:?} result_ids={result_ids:?} automatic_attempts={automatic_attempts:?} active_witness={active_witness:?}",
                        request.request_id, request.body_bytes
                    );
                    first_normal_facts_recorded = true;
                }
                request
                    .reply
                    .send((200, answer_with_usage("normal continuation", 100)))
                    .unwrap();
            }
        }
    }
    if spill_off && !hold_summary {
        crate::test_watchdog::watchdog("spill-off summary attempt finishes", async {
            while !terminal_record_written(&control).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    // RejectFact has no checkpoint: the trigger fires inside the stage's
    // adoption transaction, before the driver's terminal writer runs. The
    // rejected-stage observation point is after that terminal write.
    while !spill_off
        && !hold_summary
        && !if fault == Fault::RejectFact {
            reached.load(Ordering::SeqCst) && terminal_record_written(&control).await
        } else {
            checkpoint_reached.load(Ordering::SeqCst)
        }
    {
        if let Ok(normal) = receive.try_recv() {
            assert_ne!(
                normal.body["tools"],
                json!([]),
                "summary request already completed above"
            );
            normal
                .reply
                .send((200, answer_with_usage("background continuation", 100)))
                .unwrap();
        }
        tokio::task::yield_now().await;
    }
    let held_phase = if hold_summary {
        Some(HeldSummaryPhase {
            summary: held_summary.expect("held-summary phase captured its summary request"),
        })
    } else {
        None
    };
    if hold_summary {
        return Fx {
            root,
            url,
            limits,
            owner,
            control,
            target,
            receive,
            server,
            guard_before,
            reached,
            checkpoint_reached,
            release_checkpoint,
            held_phase,
        };
    }
    let saved = control.store.database.call(|db| Ok(db.query_row(
        "SELECT json_extract(error_json,'$.automatic_trim_attempt') FROM model_attempt WHERE json_type(error_json,'$.automatic_trim_attempt')='object' ORDER BY rowid DESC LIMIT 1",
        [], |row| row.get::<_, String>(0))?)).await.unwrap();
    let saved: Value = serde_json::from_str(&saved).unwrap();
    if spill_off {
        assert!(saved["trigger"]["a_input_tokens"].is_null());
        assert_eq!(saved["outcome"], "adopted");
        assert!(saved["committed_stages"]["closed_spill"].is_null());
        assert!(saved["committed_stages"]["active_spill"].is_null());
        assert!(saved["committed_stages"][scenario.stage()].is_object());
        assert!(
            receive.try_recv().is_err(),
            "one summary POST only; no automatic spill continuation"
        );
    } else {
        assert_trigger_evidence(&saved["trigger"], scenario);
    }
    if !spill_off {
        // This read occurs at the committed stage checkpoint. Legacy spill
        // stages remain provisional; native GC summary stages finish atomically
        // with adoption. A rejected stage fact is read after its terminal write.
        let phase = if fault == Fault::RejectFact {
            ObservationPhase::AfterRejectedStageTransaction
        } else {
            ObservationPhase::AtCommittedCheckpoint
        };
        let expected_outcome = expected_outcome(fault, scenario, phase);
        assert_eq!(
            saved["outcome"], expected_outcome,
            "actual terminal outcome for {fault:?} {scenario:?}: {saved}"
        );
        if fault == Fault::RejectFact {
            assert!(
                saved["committed_stages"][scenario.stage()].is_null(),
                "rejected stage leaves no stage fact: {saved}"
            );
        } else {
            assert!(
                saved["committed_stages"][scenario.stage()].is_object(),
                "selected stage fact is persisted: {saved}"
            );
        }
        assert!(
            receive.try_recv().is_err(),
            "no provider continuation before the selected checkpoint"
        );
    }
    Fx {
        root,
        url,
        limits,
        owner,
        control,
        target,
        receive,
        server,
        guard_before,
        reached,
        checkpoint_reached,
        release_checkpoint,
        held_phase,
    }
}

struct HeldSummaryPhase {
    summary: Incoming,
}

struct Fx {
    root: tempfile::TempDir,
    url: String,
    limits: String,
    owner: DataOwner,
    control: Arc<Control>,
    target: String,
    receive: mpsc::Receiver<Incoming>,
    server: tokio::task::JoinHandle<()>,
    guard_before: Vec<(String, String)>,
    reached: Arc<AtomicBool>,
    checkpoint_reached: Arc<AtomicBool>,
    release_checkpoint: Arc<tokio::sync::Notify>,
    held_phase: Option<HeldSummaryPhase>,
}

/// Whether the latest automatic attempt record has left the provisional state,
/// i.e. the driver's terminal writer for the attempt has committed.
async fn terminal_record_written(control: &Control) -> bool {
    control.store.database.call(|db| Ok(db.query_row(
        "SELECT json_extract(error_json,'$.automatic_trim_attempt.provisional') FROM model_attempt WHERE json_type(error_json,'$.automatic_trim_attempt')='object' ORDER BY rowid DESC LIMIT 1",
        [], |row| row.get::<_, Option<i64>>(0)).optional()?.flatten() == Some(0))).await.unwrap()
}

/// After the checkpoint is released: answer the active summary request the
/// driver legitimately sends after a continuing active-spill fault (it must be
/// a summary request, tools=[]), then wait for the continued attempt to reach
/// the fault's own operation before shutdown can cancel it: the armed
/// remeasurement is consumed and its failure is recorded, or the rejected
/// final write is hit.
async fn after_release(
    control: &Control,
    receive: &mut mpsc::Receiver<Incoming>,
    fault: Fault,
    scenario: Scenario,
    reached: &AtomicBool,
) {
    if scenario.continues_to_active_summary(fault) {
        let summary_request = incoming(control, receive).await;
        assert_eq!(
            summary_request.body["tools"],
            json!([]),
            "only the active summary request follows the released active spill"
        );
        summary_request
            .reply
            .send((200, answer("Short adopted summary.")))
            .unwrap();
    }
    if fault == Fault::Remeasurement {
        crate::test_watchdog::watchdog("remeasurement failure consumed", async {
            while control.compactor.fail_remeasurement.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            // The failed remeasurement is classified by the driver's own
            // terminal write; shutdown must not race that classification.
            while !terminal_record_written(control).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    // Strict summary FINISH already completed inside adoption before this
    // postcommit hook is installed; no second W4 final write follows it.
    if fault == Fault::RejectFinal && !scenario.summary() {
        crate::test_watchdog::watchdog("rejected final write reached", async {
            while !reached.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}

/// Persisted #180 guard and status rows, byte-exact.
async fn guard_rows(control: &Control) -> Vec<(String, String)> {
    control.store.database.call(|db| {
        Ok(db.prepare("SELECT key,value FROM app_meta WHERE key GLOB 'compact.automatic_guard.*' OR key GLOB 'compact.automatic_status.*' ORDER BY key")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap()
}

async fn boundary(fault: Fault, scenario: Scenario) {
    boundary_with_thread_override(fault, scenario, true).await;
}

async fn boundary_with_thread_override(fault: Fault, scenario: Scenario, set_thread_percent: bool) {
    let Fx {
        root,
        url,
        limits,
        owner,
        control,
        target,
        mut receive,
        server,
        guard_before: _,
        reached,
        checkpoint_reached,
        release_checkpoint,
        ..
    } = prepare_with_thread_override(fault, scenario, set_thread_percent).await;
    if fault != Fault::RejectFact {
        assert!(
            checkpoint_reached.load(Ordering::SeqCst),
            "postcommit barrier reached"
        );
        assert_boundary(&control, &target, scenario, fault, "checkpoint").await;
        if fault == Fault::Remeasurement {
            assert!(
                control.compactor.fail_remeasurement.load(Ordering::SeqCst),
                "remeasurement failure is armed before checkpoint release"
            );
        }
        release_checkpoint.notify_one();
        after_release(&control, &mut receive, fault, scenario, &reached).await;
    }
    let shutdown = control.shutdown().await;
    if matches!(
        fault,
        Fault::Persistent
            | Fault::RejectFinal
            | Fault::Remeasurement
            | Fault::RejectFact
            | Fault::Storage
            | Fault::RetiredWritesAbsent
    ) {
        assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
    } else {
        shutdown.unwrap();
    }
    if fault != Fault::RejectFact {
        assert!(
            reached.load(Ordering::SeqCst),
            "fault hook reached its configured checkpoint"
        );
    }
    assert_boundary(&control, &target, scenario, fault, "shutdown").await;
    control
        .store
        .database
        .call(|db| {
            db.execute_batch(
                "DROP TRIGGER fault_adopt; DROP TRIGGER fault_epoch; DROP TRIGGER fault_restage; DROP TRIGGER fault_begin;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // Reopen through normal startup repair, not by patching run/turn states.
    drop(control);
    owner.shutdown().await.unwrap();
    let owner = proactive_owner(root.path(), &url, &limits).await;
    let control = Control::new(&owner, root.path().into());
    assert_boundary(&control, &target, scenario, fault, "reopen").await;
    let id = target.clone();
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key='compact.automatic_active.'||?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    // The synthetic protected user exceeds the ordinary 64K segment baseline.
    // Use the public manual whole-group summary operation on one replacement,
    // without altering the saved user, widening limits, or bypassing validation.
    let id = target.clone();
    let node = control.store.database.call(move |db| Ok(db.query_row(
        "SELECT p.node_id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind IN ('spill','summary') ORDER BY p.ordinal LIMIT 1",
        [id], |r| r.get::<_, String>(0))?)).await.unwrap();
    let edit = ContextEditArgs {
        thread_id: Some(target.clone()),
        action: ContextEditAction::Summary,
        from_node_id: Some(node.clone()),
        to_node_id: Some(node),
        max_chars: None,
    };
    // Normal Control admission, queue and Thread boundary, not a direct edit.
    let accepted = request(&control, Command::ContextEdit(edit)).await;
    let operation = accepted["operation_id"].as_str().unwrap().to_owned();
    let mut requests = 0;
    let result = loop {
        tokio::select! {
            status = request(&control, Command::OperationGet(OperationTarget { operation_id: operation.clone() })) => {
                assert_ne!(status["downstream"]["state"], "failed", "{status}");
                if status["downstream"]["state"] == "completed" {
                    break status["downstream"]["outcome"]["result"].clone();
                }
                tokio::task::yield_now().await;
            }
            request = receive.recv() => {
                let request = request.expect("loopback remains available");
                assert_eq!(request.body["tools"], json!([]));
                requests += 1;
                request.reply.send((200, answer("ok"))).unwrap();
            }
        }
    };
    assert!(
        requests > 0,
        "exercise the next summary, including map/reduce if needed"
    );
    assert!(
        result["compact"]["no_target"] == false,
        "next compaction must actually adopt, not silently skip"
    );
    assert!(result["compact"]["summary_node_id"].is_string(), "{result}");
    assert!(result["compact"]["source_nodes"].as_u64().unwrap() > 0);
    assert!(
        result["after_characters"].as_u64().unwrap()
            < result["before_characters"].as_u64().unwrap()
    );
    assert_immutable(&control).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

async fn assert_immutable(control: &Control) {
    control.store.database.call(|db| {
        // Exact row comparison protects originals, replacement records and ordered
        // provenance, including hashes/revisions, even after another compaction.
        for (saved, current) in [("fault_originals", "node"), ("fault_nodes", "node"), ("fault_replacements", "replacement"), ("fault_sources", "replacement_source")] {
            let missing: i64 = db.query_row(&format!("SELECT count(*) FROM (SELECT * FROM {saved} EXCEPT SELECT * FROM {current})"), [], |r| r.get(0))?;
            assert_eq!(missing, 0, "immutable rows changed: {current}");
        }
        let replacements: Vec<(String, String, String)> = db.prepare("SELECT r.id,n.kind,n.payload_json FROM replacement r JOIN node n ON n.id=r.output_node_id")?.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (replacement, kind, body) in replacements {
            let payload: Value = serde_json::from_str(&body).unwrap();
            let sources: Vec<String> = db.prepare("SELECT source_node_id FROM replacement_source WHERE replacement_id=?1 ORDER BY ordinal")?.query_map([replacement], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            assert_eq!(payload["source_node_ids"], json!(sources), "payload and ordered provenance describe the same complete replacement");
            if kind == "spill" { assert_eq!(sources.len(), 2, "one whole call/result group"); }
        }
        assert_eq!(db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0))?, 0);
        assert_eq!(db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?, "ok");
        Ok(())
    }).await.unwrap();
}

async fn assert_boundary(
    control: &Control,
    target: &str,
    scenario: Scenario,
    fault: Fault,
    phase: &'static str,
) {
    assert_immutable(control).await;
    let target = target.to_owned();
    control.store.database.call(move |db| {
        let (path, revision, epoch): (String, i64, i64) = db.query_row("SELECT path,revision,epoch FROM fault_boundary", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let (current_revision, current_epoch, cursor): (i64, i64, String) = db.query_row("SELECT selection_revision,reasoning_epoch,cursor_node_id FROM thread WHERE id=?1", [&target], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let current_path: Vec<String> = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?.query_map([&target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let committed_path: Vec<String> = serde_json::from_str(&path).unwrap();
        assert!(current_path.len() >= committed_path.len());
        assert_eq!(&current_path[..committed_path.len()], committed_path, "committed selection is preserved exactly");
        let appended = &current_path[committed_path.len()..];
        assert_interruption_audits(db, appended, phase, committed_path.len())?;
        assert_eq!(current_path.last(), Some(&cursor));
        assert_eq!(current_revision, revision + appended.len() as i64, "only validated audit append commits advance selection_revision");
        assert_eq!(current_epoch, epoch, "committed reasoning epoch survives failure/restart");
        let initial_epoch: i64 = db.query_row("SELECT epoch FROM fault_initial", [], |r| r.get(0))?;
        let record_text: String = db.query_row("SELECT json_extract(error_json,'$.automatic_trim_attempt') FROM model_attempt WHERE json_type(error_json,'$.automatic_trim_attempt')='object'", [], |r| r.get(0))?;
        let record: Value = serde_json::from_str(&record_text).unwrap();
        let continued = phase != "checkpoint" && scenario.continues_to_active_summary(fault);
        // The boundary is the last committed stage: the selected one, or the
        // active summary the driver entered after a released active spill.
        let last = if continued { "active_summary" } else { scenario.stage() };
        assert!(record["committed_stages"][last].is_object(), "the continued stage committed: {record}");
        let last_fact = &record["committed_stages"][last];
        assert_eq!(revision, last_fact["selection_revision"].as_i64().unwrap(), "boundary is the last committed stage: {record}");
        let committed_epoch = last_fact["reasoning_epoch"]
            .as_i64().expect("selected committed stage contains its exact reasoning epoch");
        assert_eq!(epoch, committed_epoch, "Thread epoch equals the exact adopted stage fact; initial={initial_epoch}, record={record}");
        assert!(committed_epoch >= initial_epoch, "committed stage does not reverse reasoning epoch");
        assert_trigger_evidence(&record["trigger"], scenario);
        assert!(record["committed_stages"][scenario.stage()].is_object(), "fault reached the intended stage: {record}");
        let kinds: Vec<String> = db.prepare("SELECT n.kind FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind IN ('spill','summary') ORDER BY p.ordinal")?.query_map([&target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let expected: Vec<&str> = match scenario {
            Scenario::ClosedSpill | Scenario::ActiveSpill => vec!["spill", "spill"],
            Scenario::ClosedSummary => vec!["summary", "spill", "spill"],
            Scenario::ActiveSummary => vec!["summary"],
        };
        // The entered active summary replaces the active range from its first
        // work group, the committed spills included (§4.1 active).
        let expected = if continued { vec!["summary"] } else { expected };
        assert_eq!(kinds, expected, "all intended replacements, not a partial group");
        let incomplete: i64 = db.query_row("SELECT count(*) FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id WHERE tr.thread_id=?1 AND (r.state!='completed' OR NOT EXISTS(SELECT 1 FROM replacement_source s WHERE s.replacement_id=r.id) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=r.output_node_id AND n.payload_json IS NOT NULL))", [&target], |r| r.get(0))?;
        assert_eq!(incomplete, 0);
        let missing: i64 = db.query_row("SELECT count(*) FROM replacement_source s LEFT JOIN node n ON n.id=s.source_node_id WHERE n.id IS NULL OR n.payload_json IS NULL OR n.payload_revision!=s.source_revision", [], |r| r.get(0))?;
        assert_eq!(missing, 0);
        Ok(())
    }).await.unwrap();
}

// Per-Control, single-use synchronization point: the adoption future has returned
// (including its epoch write and COMMIT), and no subsequent stage has begun.
pub(super) async fn checkpoint(
    control: &Arc<Control>,
    slot: &DriverSlot,
    stage: &str,
) -> Result<(), ApiError> {
    let injection = {
        let mut pending = control.automatic_fault.lock().unwrap();
        if pending.as_ref().is_some_and(|p| p.stage == stage) {
            pending.take()
        } else {
            None
        }
    };
    let Some(injection) = injection else {
        return Ok(());
    };
    assert!(
        !slot.stop.is_cancelled(),
        "faults must be independent of prior cancellation"
    );
    let result = match injection.kind {
        0 => {
            slot.stop.cancel();
            assert!(slot.stop.is_cancelled(), "token is cancelled before the next stage");
            Ok(())
        }
        1 => control.store.database.call(|db| {
            db.execute_batch("CREATE TEMP TABLE rejected_write(value INTEGER);
                CREATE TEMP TRIGGER reject_write BEFORE INSERT ON rejected_write
                BEGIN SELECT RAISE(ABORT,'injected independent postcommit storage write failure'); END;")?;
            db.execute("INSERT INTO rejected_write VALUES(1)", [])?;
            Ok(())
        }).await.map_err(storage_error),
        2 => {
            control.compactor.fail_remeasurement.store(true, Ordering::SeqCst);
            Ok(())
        }
        4 => control.store.database.call(|db| {
            db.execute_batch("CREATE TEMP TRIGGER persistent_attempt BEFORE UPDATE OF error_json ON model_attempt
                WHEN json_type(NEW.error_json,'$.automatic_trim_attempt')='object'
                BEGIN SELECT RAISE(ABORT,'injected persistent attempt write failure'); END;
                CREATE TEMP TRIGGER persistent_insert BEFORE INSERT ON app_meta WHEN NEW.key GLOB 'compact.automatic_*'
                BEGIN SELECT RAISE(ABORT,'injected persistent meta write failure'); END;
                CREATE TEMP TRIGGER persistent_update BEFORE UPDATE ON app_meta WHEN OLD.key GLOB 'compact.automatic_*'
                BEGIN SELECT RAISE(ABORT,'injected persistent meta write failure'); END;
                CREATE TEMP TRIGGER persistent_delete BEFORE DELETE ON app_meta WHEN OLD.key GLOB 'compact.automatic_*'
                BEGIN SELECT RAISE(ABORT,'injected persistent meta write failure'); END;
                CREATE TEMP TABLE rejected_write(value INTEGER);
                CREATE TEMP TRIGGER reject_write BEFORE INSERT ON rejected_write
                BEGIN SELECT RAISE(ABORT,'injected independent postcommit storage write failure'); END;")?;
            db.execute("INSERT INTO rejected_write VALUES(1)", [])?;
            Ok(())
        }).await.map_err(storage_error),
        9 => {
            let hit = injection.reached.clone();
            control.store.database.call(move |db| {
                db.execute_batch("CREATE TABLE retired_write_hits(site TEXT NOT NULL);")?;
                db.create_scalar_function("retired_write_hit", 1, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
                    hit.store(true, Ordering::SeqCst);
                    Ok(1i64)
                })?;
                db.execute_batch("CREATE TEMP TRIGGER retired_exception_write BEFORE UPDATE OF error_json ON model_attempt
                    WHEN json_extract(NEW.error_json,'$.automatic_trim_attempt.outcome')='exception'
                    BEGIN INSERT INTO retired_write_hits VALUES('W2_exception'); SELECT retired_write_hit(1); END;
                    CREATE TEMP TRIGGER retired_guard_insert BEFORE INSERT ON app_meta WHEN NEW.key GLOB 'compact.automatic_guard.*' OR NEW.key GLOB 'compact.automatic_status.*'
                    BEGIN INSERT INTO retired_write_hits VALUES('W3_insert'); SELECT retired_write_hit(1); END;
                    CREATE TEMP TRIGGER retired_guard_update BEFORE UPDATE ON app_meta WHEN OLD.key GLOB 'compact.automatic_guard.*' OR OLD.key GLOB 'compact.automatic_status.*'
                    BEGIN INSERT INTO retired_write_hits VALUES('W3_update'); SELECT retired_write_hit(1); END;
                    CREATE TEMP TRIGGER retired_guard_delete BEFORE DELETE ON app_meta WHEN OLD.key GLOB 'compact.automatic_guard.*' OR OLD.key GLOB 'compact.automatic_status.*'
                    BEGIN INSERT INTO retired_write_hits VALUES('W3_delete'); SELECT retired_write_hit(1); END;")?;
                Ok(())
            }).await.map_err(storage_error)?;
            control.store.database.call(|db| {
                db.execute_batch("CREATE TEMP TABLE rejected_write(value INTEGER);
                    CREATE TEMP TRIGGER reject_write BEFORE INSERT ON rejected_write
                    BEGIN SELECT RAISE(ABORT,'injected independent postcommit storage write failure'); END;")?;
                db.execute("INSERT INTO rejected_write VALUES(1)", [])?;
                Ok(())
            }).await.map_err(storage_error)
        }
        6 => {
            let hit = injection.reached.clone();
            control.store.database.call(move |db| {
                db.create_scalar_function("fault_final_hit", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
                    hit.store(true, Ordering::SeqCst);
                    Ok(1i64)
                })?;
                db.execute_batch("CREATE TEMP TRIGGER reject_final_attempt BEFORE UPDATE OF error_json ON model_attempt
                    WHEN json_extract(NEW.error_json,'$.automatic_trim_attempt.outcome') NOT IN ('unknown','exception')
                    BEGIN SELECT fault_final_hit(); SELECT RAISE(ABORT,'injected final attempt record rejection'); END;")?;
                Ok(())
            }).await.map_err(storage_error)
        }
        8 => Ok(()),
        _ => unreachable!(),
    };
    if injection.kind != 0 {
        assert!(
            !slot.stop.is_cancelled(),
            "storage faults do not depend on cancellation"
        );
    }
    if matches!(injection.kind, 1 | 4 | 9) {
        assert_eq!(result.as_ref().unwrap_err().code, ErrorCode::SaveFailed);
    }
    if !matches!(injection.kind, 6..=8) {
        injection.reached.store(true, Ordering::SeqCst);
    }
    if injection.kind != 8 {
        injection.checkpoint_reached.store(true, Ordering::SeqCst);
        injection.release_checkpoint.notified().await;
    }
    result
}

async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
fn answer(text: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]})
}
// Fixture guards detect deadlocks, not product latency. Capture only bounded
// saved lifecycle facts on failure; request bodies and error messages stay out.
async fn progress(control: &Control) -> Result<Value, crate::storage::StorageError> {
    control.store.database.call(|db| {
        let mut state = serde_json::Map::new();
        for (name, query) in [
            ("runs", "SELECT json_object('id',substr(id,1,128),'thread_id',substr(thread_id,1,128),'state',substr(state,1,64),'stop_reason',substr(stop_reason,1,128)) FROM run ORDER BY rowid DESC LIMIT 8"),
            ("turns", "SELECT json_object('id',substr(t.id,1,128),'thread_id',substr(t.thread_id,1,128),'run_id',substr(t.run_id,1,128),'state',substr(t.state,1,64),'generation',t.generation,'instruction_snapshot_present',t.instruction_snapshot_id IS NOT NULL,'run_state',substr(r.state,1,64),'recovery_code',substr(json_extract(t.recovery_reason_json,'$.code'),1,128),'attempt_state',substr(a.state,1,64),'error_kind',substr(json_extract(a.error_json,'$.error.kind'),1,128),'http_phase',substr(json_extract(a.error_json,'$.error.http_observation.phase'),1,128)) FROM turn t JOIN run r ON r.id=t.run_id LEFT JOIN model_attempt a ON a.id=(SELECT latest.id FROM model_attempt latest WHERE latest.turn_id=t.id ORDER BY latest.rowid DESC LIMIT 1) ORDER BY t.rowid DESC LIMIT 8"),
        ] {
            let rows = db.prepare(query)?.query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            state.insert(name.into(), json!(rows));
        }
        Ok(Value::Object(state))
    }).await
}
#[derive(Debug)]
struct AdmissionBoundaryFacts {
    latest_attempt_id: Option<String>,
    input_tokens: Option<u64>,
    next_request_pending: Option<bool>,
    slot_exists: bool,
    job_exists: bool,
    job_finished: bool,
}

async fn admission_boundary_facts(control: &Control, thread: &str) -> AdmissionBoundaryFacts {
    let thread_for_usage = thread.to_owned();
    let (latest_attempt_id, input_tokens, next_request_pending) = control
        .store
        .database
        .call(move |db| {
            let (usage, _) = crate::compaction::context_usage::prepare_context_usage(
                crate::compaction::context_usage::read_context_usage_tx(
                    db,
                    &thread_for_usage,
                    None,
                )?,
            );
            let report = &usage["last_successful_attempt"];
            Ok((
                report["attempt_id"].as_str().map(str::to_owned),
                report["input_tokens"].as_u64(),
                report["next_request_pending"].as_bool(),
            ))
        })
        .await
        .unwrap();
    let slot = {
        let drivers = control.live.test_drivers().lock().await;
        drivers.get(thread).cloned()
    };
    let (job_exists, job_finished) = if let Some(slot) = slot.as_ref() {
        let job = slot.proactive_job.lock().await;
        (
            job.is_some(),
            job.as_ref()
                .is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished),
        )
    } else {
        (false, false)
    };
    AdmissionBoundaryFacts {
        latest_attempt_id,
        input_tokens,
        next_request_pending,
        slot_exists: slot.is_some(),
        job_exists,
        job_finished,
    }
}

enum HeldFixtureEvent {
    Request(Incoming),
    Header(ProxyRequestEvent),
    JobFinished(AdmissionBoundaryFacts),
}

async fn next_held_fixture_event(
    control: &Control,
    thread: &str,
    receive: &mut mpsc::Receiver<Incoming>,
    heads: &mut mpsc::UnboundedReceiver<ProxyRequestEvent>,
    slot: &DriverSlot,
) -> HeldFixtureEvent {
    loop {
        let notified = slot.nudge.notified();
        tokio::pin!(notified);
        let finished = {
            let job = slot.proactive_job.lock().await;
            job.as_ref()
                .is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished)
        };
        if finished {
            return HeldFixtureEvent::JobFinished(admission_boundary_facts(control, thread).await);
        }
        tokio::select! {
            biased;
            event = heads.recv() => return HeldFixtureEvent::Header(event.expect("mock head channel remains open")),
            request = receive.recv() => return HeldFixtureEvent::Request(request.expect("mock body channel remains open")),
            _ = &mut notified => {
                let finished = {
                    let job = slot.proactive_job.lock().await;
                    job.as_ref().is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished)
                };
                if finished {
                    return HeldFixtureEvent::JobFinished(admission_boundary_facts(control, thread).await);
                }
            }
        }
    }
}

#[track_caller]
fn incoming<'a>(
    control: &'a Control,
    receive: &'a mut mpsc::Receiver<Incoming>,
) -> impl std::future::Future<Output = Incoming> + 'a {
    let caller = std::panic::Location::caller();
    async move {
        match crate::test_watchdog::watchdog("tests/runtime_features.rs: incoming", receive.recv())
            .await
        {
            Ok(Some(value)) => value,
            result => {
                let diagnostic = crate::test_watchdog::watchdog(
                    "tests/runtime_features.rs: incoming",
                    progress(control),
                )
                .await;
                panic!(
                    "loopback request watchdog at {}:{}; receive={}; saved lifecycle facts={diagnostic:?}",
                    caller.file(),
                    caller.line(),
                    if result.is_err() {
                        "timeout"
                    } else {
                        "sender closed"
                    }
                );
            }
        }
    }
}
async fn wait_for_driver_retired(
    control: &Control,
    thread: &str,
    expected_slot: &str,
) -> DriverSlot {
    let mut events = control.subscribe();
    loop {
        let slot = control
            .live
            .test_drivers()
            .lock()
            .await
            .get(thread)
            .cloned()
            .expect("retained proactive Thread slot remains registered through driver retirement");
        assert_eq!(
            slot.id, expected_slot,
            "the observed driver slot was not replaced"
        );
        if !slot.driver_active.load(Ordering::Acquire) {
            return slot;
        }
        match events.recv().await {
            Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                panic!("event stream closed before driver retirement for Thread {thread}")
            }
            _ => {}
        }
    }
}

#[track_caller]
fn settled<'a>(
    control: &'a Control,
    thread: &'a str,
) -> impl std::future::Future<Output = ()> + 'a {
    let caller = std::panic::Location::caller();
    let mut events = control.subscribe();
    async move {
        let result = crate::test_watchdog::watchdog("tests/runtime_features.rs: settled", async {
            loop {
                let target = thread.to_owned();
                if control.store.database.call(move |db| Ok(db.query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting'))",
                    [target], |row| row.get::<_, bool>(0),
                )?)).await.unwrap() {
                    break;
                }
                loop {
                    match events.recv().await {
                        Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!(
                            "settlement event stream closed at {}:{} for Thread {thread}",
                            caller.file(), caller.line()
                        ),
                        _ => {}
                    }
                }
            }
        }).await;
        if result.is_err() {
            let diagnostic = crate::test_watchdog::watchdog(
                "tests/runtime_features.rs: settled",
                progress(control),
            )
            .await;
            panic!(
                "Thread {thread} did not settle within the fixture watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                caller.file(),
                caller.line()
            );
        }
    }
}
async fn thread(control: &Arc<Control>) -> String {
    request(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .into()
}
async fn send(control: &Arc<Control>, thread: &str, text: &str) -> Value {
    request(
        control,
        Command::MessageSend(MessageSendArgs {
            resume_recovery: false,
            human: false,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.into()),
            text: text.into(),
        }),
    )
    .await
}

async fn proactive_owner(root: &std::path::Path, url: &str, extra: &str) -> DataOwner {
    proactive_owner_retries(root, url, extra, 0, false).await
}
async fn proactive_owner_retries(
    root: &std::path::Path,
    url: &str,
    extra: &str,
    retries: u32,
    chat: bool,
) -> DataOwner {
    let config = root.join("config.toml");
    let api = if chat {
        "api=\"chat_completions\"\n"
    } else {
        ""
    };
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\n{api}base_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms=1\n{extra}")).unwrap();
    DataOwner::start(Some(config), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
fn answer_with_usage(text: &str, input_tokens: u64) -> Value {
    let mut body = answer(text);
    body["usage"] =
        json!({"input_tokens":input_tokens,"output_tokens":1,"total_tokens":input_tokens+1});
    body
}
fn chat_answer_with_usage(text: &str, input_tokens: u64) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":input_tokens,"completion_tokens":1,"total_tokens":input_tokens+1}})
}

struct Incoming {
    request_id: u64,
    body_bytes: usize,
    body: Value,
    reply: oneshot::Sender<(u16, Value)>,
    release: Option<oneshot::Sender<()>>,
}
#[derive(Clone, Debug)]
enum ProxyRequestEvent {
    Accepted {
        request_id: u64,
        peer: std::net::SocketAddr,
    },
    Headers(ProxyHead),
}

#[derive(Clone, Debug)]
struct ProxyHead {
    request_id: u64,
    request_line: String,
    content_length: Option<usize>,
    transfer_encoding: Option<String>,
    content_encoding: Option<String>,
}

async fn proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    mpsc::UnboundedReceiver<ProxyRequestEvent>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel(8);
    let (head_send, head_receive) = mpsc::unbounded_channel::<ProxyRequestEvent>();
    let request_ids = Arc::new(AtomicU64::new(0));
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, peer) = listener.accept().await.unwrap();
            let send = send.clone();
            let head_send = head_send.clone();
            let request_id = request_ids.fetch_add(1, Ordering::Relaxed);
            let _ = head_send.send(ProxyRequestEvent::Accepted { request_id, peer });
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                let mut head_sent = false;
                let (body, body_bytes) = loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(at) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let header_text = String::from_utf8_lossy(&bytes[..at]);
                        let mut lines = header_text.lines();
                        let request_line = lines.next().unwrap_or_default().to_owned();
                        let headers = lines.collect::<Vec<_>>();
                        let header_value = |name: &str| {
                            headers.iter().find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case(name)
                                    .then(|| value.trim().to_owned())
                            })
                        };
                        let content_length = header_value("content-length")
                            .and_then(|value| value.parse::<usize>().ok());
                        let transfer_encoding = header_value("transfer-encoding");
                        let content_encoding = header_value("content-encoding");
                        if !head_sent {
                            let _ = head_send.send(ProxyRequestEvent::Headers(ProxyHead {
                                request_id,
                                request_line,
                                content_length,
                                transfer_encoding,
                                content_encoding,
                            }));
                            head_sent = true;
                        }
                        let size = content_length.unwrap();
                        if bytes.len() >= at + 4 + size {
                            break (
                                serde_json::from_slice(&bytes[at + 4..at + 4 + size]).unwrap(),
                                size,
                            );
                        }
                    }
                };
                let (reply, receive) = oneshot::channel();
                let (release, wait_release) = oneshot::channel();
                if send
                    .send(Incoming {
                        request_id,
                        body_bytes,
                        body,
                        reply,
                        release: Some(release),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                if let Ok((status, body)) = receive.await {
                    if let Some(raw) = body.get("_test_sse_body").and_then(Value::as_str) {
                        let hold = body["_test_hold_sse_open"] == true;
                        let connection = if hold { "keep-alive" } else { "close" };
                        let _ = stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nConnection: {connection}\r\n\r\n{raw}").as_bytes()).await;
                        if hold {
                            let _ = wait_release.await;
                        }
                    } else if body.get("_test_retry_after").is_some() {
                        let body = body.to_string();
                        let _ = stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nRetry-After: 3600\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
                    } else if body.get("_test_incomplete").is_some() {
                        let truncate = body["_test_incomplete"] == "bounded";
                        let body = if truncate {
                            format!(
                                "{{\"error\":{{\"message\":\"SECRET_PROVIDER_MESSAGE\",\"code\":\"invalid_request_error\"}}}}{}",
                                "x".repeat(70 * 1024)
                            )
                        } else {
                            r#"{"error":{"message":"SECRET_PROVIDER_MESSAGE","code":"invalid_request_error"}}"#.to_owned()
                        };
                        let declared = if truncate {
                            body.len()
                        } else {
                            body.len() + 100
                        };
                        let _ = stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await;
                    } else {
                        let body = body.to_string();
                        let _ = stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
                    }
                }
            });
        }
    });
    (
        format!("http://{address}/v1"),
        receive,
        head_receive,
        server,
    )
}

fn assert_interruption_audits(
    db: &rusqlite::Connection,
    appended: &[String],
    phase: &'static str,
    captured_prefix_len: usize,
) -> Result<(), crate::storage::StorageError> {
    let sections = [
        "runs",
        "turns",
        "shells",
        "tool_calls",
        "unreflected_messages",
        "pending_operations_with_unknown_targets",
    ];
    let mut details = std::collections::BTreeMap::new();
    let mut reasons = std::collections::BTreeSet::new();
    for (offset, id) in appended.iter().enumerate() {
        let (kind, turn, at, body): (String, Option<String>, String, String) = db.query_row(
            "SELECT kind,turn_id,created_at,payload_json FROM node WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if kind == "event" {
            let turn_id = turn
                .as_deref()
                .expect("only the notification event belongs to the interrupted turn");
            let (thread, run, state, generation): (String, String, String, i64) = db.query_row(
                "SELECT thread_id,run_id,state,generation FROM turn WHERE id=?1",
                [turn_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            assert_eq!(
                state, "stopped",
                "notification targets the durably stopped Turn"
            );
            let (event_kind, source, target_thread, target_run): (String,String,String,Option<String>) = db.query_row(
                "SELECT e.kind,e.source_json,e.target_thread_id,e.target_run_id FROM event e WHERE e.id=json_extract(?1,'$.event_id')",
                [&body], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            )?;
            assert_eq!(event_kind, "context_changed");
            assert_eq!(source, r#"{"kind":"shellboy","source":"shutdown"}"#);
            assert_eq!(target_thread, thread);
            assert_eq!(
                target_run, None,
                "shutdown context is saved after the run stops and has no replay target"
            );
            assert!(generation > 0);
            let delivery: String = db.query_row(
                "SELECT state FROM delivery WHERE event_id=json_extract(?1,'$.event_id') AND recipient_id=?2",
                rusqlite::params![body,run], |r| r.get(0),
            )?;
            assert_eq!(
                delivery, "pending",
                "shutdown notice remains queued for the next legitimate admission"
            );
        } else {
            assert!(turn.is_none(), "postcommit audit suffix metadata: {}", {
                // Failure-only closed metadata: never print node/Turn IDs, payload,
                // paths, or provider fields. Unknown kinds/states are not forwarded.
                // Node `kind` is a bounded internal enum; expose it verbatim in the
                // isolated test diagnostic so a real lifecycle mismatch is classified.
                let node_kind = kind.as_str();
                let metadata=db.query_row(
                "SELECT r.state,t.generation FROM turn t JOIN run r ON r.id=t.run_id WHERE t.id=?1",
                [turn.as_deref()],|r|Ok((r.get(0)?,r.get(1)?)),
            ).optional();
                let query_failed = metadata.is_err();
                let (run_state, generation): (Option<String>, Option<i64>) =
                    metadata.unwrap_or(None).unwrap_or((None, None));
                let run_state = match run_state.as_deref() {
                    Some("live" | "stopping" | "completed" | "cancelled" | "failed") => {
                        run_state.as_deref().unwrap()
                    }
                    _ if query_failed => "query_failed",
                    _ => "other_or_missing",
                };
                json!({"node_kind":node_kind,"turn_present":turn.is_some(),"ordinal":captured_prefix_len+offset,"phase":phase,"run_state":run_state,"turn_generation":generation,"captured_prefix_len":captured_prefix_len})
            });
        }
        let payload: Value = serde_json::from_str(&body).unwrap();
        let audit: Value = if kind == "event" {
            Value::Null // event kind/id/recipient were validated against the event ledger above
        } else {
            serde_json::from_str(payload["text"].as_str().unwrap()).unwrap()
        };
        match kind.as_str() {
            "event" => {
                assert!(
                    payload["event_id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty())
                );
            }
            "interruption_details" => {
                assert!(sections.contains(&audit["section"].as_str().unwrap()));
                assert!(!audit["items"].as_array().unwrap().is_empty());
                details.insert(
                    id.clone(),
                    (audit["section"].clone(), at, audit["items"].clone()),
                );
            }
            "interruption_context" => {
                assert_eq!(audit["snapshot_node_id"], *id);
                assert_eq!(audit["observed_at"], at);
                let reason = audit["kind"].as_str().unwrap();
                let source = match reason {
                    "app_shutdown" => "shutdown",
                    "previous_instance_interrupted" => "startup",
                    _ => panic!("unexpected audit reason {reason}"),
                };
                assert!(reasons.insert(reason.to_owned()));
                assert_eq!(audit["actor"], json!({"kind":"shellboy","source":source}));
                assert_eq!(audit["change"]["reason"], reason);
                assert_eq!(audit["change"]["automatic_replay"], false);
                assert_eq!(audit["change"]["result_uncertain"], true);
                let mut referenced = std::collections::BTreeSet::new();
                for section in sections {
                    for detail in audit["change"][section]["detail_node_ids"]
                        .as_array()
                        .unwrap()
                    {
                        let detail = detail.as_str().unwrap();
                        assert!(referenced.insert(detail.to_owned()));
                        let (actual_section, actual_at, items) = details
                            .get(detail)
                            .expect("only preceding audit details allowed");
                        assert_eq!(actual_section, section);
                        assert_eq!(actual_at, &at);
                        assert_eq!(items, &audit["change"][section]["items"]);
                    }
                }
                assert_eq!(
                    referenced,
                    details.keys().cloned().collect(),
                    "no unreferenced suffix nodes"
                );
                details.clear();
            }
            _ => panic!("unexpected selected suffix kind {kind}"),
        }
    }
    assert!(details.is_empty(), "no incomplete audit batch");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_trigger_outside_context_changes_do_not_veto_current_r_adoption() {
    for change in ["profile_changed", "run_stopped", "turn_changed"] {
        completed_trigger_outside_context_change(change).await;
    }
}

async fn completed_trigger_outside_context_change(change: &'static str) {
    crate::test_watchdog::watchdog("completed trigger outside-R positive", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, _heads, server) = proxy().await;
        let owner = proactive_owner(
            root.path(),
            &url,
            "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n",
        )
        .await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        send(&control, &target, "Original synthetic request PRIVATE_SENTINEL").await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("completed first answer", 100)))
            .unwrap();
        settled(&control, &target).await;
        send(&control, &target, "next synthetic request").await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer_with_usage("completed second answer", 200)))
            .unwrap();
        settled(&control, &target).await;
        send(&control, &target, "trigger automatic compaction").await;
        let summary = incoming(&control, &mut receive).await;
        assert_eq!(summary.body["tools"], json!([]));
        let scope = target.clone();
        let attempt = control
            .store
            .database
            .call(move |db| {
                let attempt: String = db.query_row(
                    "SELECT a.id FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND a.state='completed' AND json_extract(a.error_json,'$.automatic_trim_attempt.token') IS NOT NULL ORDER BY a.rowid DESC LIMIT 1",
                    [&scope],
                    |row| row.get(0),
                )?;
                match change {
                    "profile_changed" => {
                        db.execute("UPDATE thread SET profile_revision=profile_revision+1 WHERE id=?1", [&scope])?;
                    }
                    "run_stopped" => {
                        db.execute("UPDATE run SET state='stopping' WHERE thread_id=?1 AND state='live'", [&scope])?;
                    }
                    "turn_changed" => {
                        db.execute("UPDATE turn SET generation=generation+1 WHERE thread_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')", [&scope])?;
                    }
                    _ => unreachable!(),
                }
                Ok(attempt)
            })
            .await
            .unwrap();
        summary
            .reply
            .send((200, answer("short synthetic summary")))
            .unwrap();
        crate::test_watchdog::watchdog("outside-R current adoption terminal", async {
            while !terminal_record_written(&control).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let attempt_id = attempt.clone();
        control
            .store
            .database
            .call(move |db| {
                let record: String = db.query_row(
                    "SELECT error_json FROM model_attempt WHERE id=?1",
                    [&attempt_id],
                    |row| row.get(0),
                )?;
                let record: Value = serde_json::from_str(&record).unwrap();
                let automatic = &record["automatic_trim_attempt"];
                assert_eq!(automatic["outcome"], "adopted", "{change}: {automatic}");
                assert_eq!(automatic["provisional"], false);
                assert!(
                    automatic["committed_stages"]
                        .as_object()
                        .is_some_and(|stages| stages.values().any(Value::is_object)),
                    "the R still committed after the outside-R change: {automatic}"
                );
                assert!(automatic["compaction_outcome"].is_null());
                let path = db
                    .prepare("SELECT p.node_id FROM thread_path p WHERE p.thread_id=?1 ORDER BY p.ordinal")?
                    .query_map([&target], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert!(path.iter().any(|id| {
                    db.query_row("SELECT kind='summary' FROM node WHERE id=?1", [id], |row| row.get::<_, bool>(0)).unwrap_or(false)
                }), "summary node adopted for {change}: {path:?}");
                assert!(!record.to_string().contains("PRIVATE_SENTINEL"));
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

mod s1_tests;
