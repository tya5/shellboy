//! Post-adoption data safety and lifecycle controls for proactive compaction.
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

#[derive(Clone, Copy)]
struct RunTurnWaitIdentity<'a> {
    thread: &'a str,
    run: &'a str,
    turn: &'a str,
}

struct RunBWaitContext<'a> {
    control: &'a Control,
    identity: RunTurnWaitIdentity<'a>,
    receive: &'a mut mpsc::Receiver<Incoming>,
    events: &'a mut tokio::sync::broadcast::Receiver<EngineEvent>,
    slot: &'a DriverSlot,
}

async fn wait_for_run_b_provider_request(wait: &mut RunBWaitContext<'_>) -> Incoming {
    let control = wait.control;
    let identity = wait.identity;
    let thread = identity.thread;
    let run = identity.run;
    let turn = identity.turn;
    let receive = &mut *wait.receive;
    let events = &mut *wait.events;
    let slot = wait.slot;
    loop {
        let notified = slot.nudge.notified();
        tokio::pin!(notified);
        tokio::select! {
            request=receive.recv()=>return request.unwrap_or_else(||panic!(
                "provider body channel closed before Run B request; Thread={thread} Run={run}"
            )),
            event=events.recv()=>match event {
                Ok(EngineEvent::Changed{thread_id,..}) if thread_id==thread=>{
                    let target=thread.to_owned();
                    let run_id=run.to_owned();
                    let turn_id=turn.to_owned();
                    let state=control.store.database.call(move|db|{
                        db.query_row(
                            concat!(
                                "SELECT r.state,t.state,",
                                "(SELECT state FROM model_attempt WHERE turn_id=t.id ORDER BY started_at DESC,rowid DESC LIMIT 1),",
                                "(SELECT json_extract(error_json,'$.kind') FROM model_attempt WHERE turn_id=t.id ORDER BY started_at DESC,rowid DESC LIMIT 1) ",
                                "FROM run r JOIN turn t ON t.run_id=r.id AND t.thread_id=r.thread_id ",
                                "WHERE r.id=?1 AND r.thread_id=?2 AND t.id=?3",
                            ),
                            rusqlite::params![run_id,target,turn_id],
                            |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?)),
                        ).map_err(Into::into)
                    }).await.unwrap();
                    if state.0!="live" || !matches!(state.1.as_str(),"preparing"|"running"|"waiting") {
                        panic!("Run B driver ended before provider body: Thread={thread} Run={run} Turn={turn} run_state={} turn_state={} attempt_state={:?} failure_kind={:?}",state.0,state.1,state.2,state.3);
                    }
                    if matches!(state.2.as_deref(),Some("failed"|"incomplete"|"cancelled")) {
                        panic!("Run B model attempt failed before provider body: Thread={thread} Run={run} Turn={turn} attempt_state={:?} failure_kind={:?}",state.2,state.3);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{},
                Err(tokio::sync::broadcast::error::RecvError::Closed)=>panic!("engine event stream closed while awaiting Run B provider request"),
                _=>{},
            },
            _=&mut notified=>{
                let finished={let job=slot.proactive_job.lock().await;job.as_ref().is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished)};
                if finished { panic!("retained proactive job finished before Run B provider body; Thread={thread} Run={run}"); }
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
        ObservationPhase::AtCommittedCheckpoint => "unknown",
        ObservationPhase::AfterRejectedStageTransaction => "failed",
        ObservationPhase::AfterShutdownAndRestart => match fault {
            Fault::RejectFact => "failed",
            // Cancellation after active spill is observed by the closed-summary
            // entry. Pre-cancel returns before planning, so there is no active
            // fallback stage; the committed spill remains and outcome is cancelled.
            Fault::Cancellation if scenario == Scenario::ActiveSpill => "cancelled",
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
        let DriverLifecycleFixture {
            root: _root,
            owner,
            control,
            target,
            mut receive,
            server,
            mut held_phase,
            terminal_identity,
            ..
        } = prepare_held_summary(Scenario::ClosedSummary).await;
        let HeldSummaryPhase { summary, normal } = held_phase
            .take()
            .expect("proactive summary reached its provider boundary");
        assert_ne!(normal.body, summary.body);
        assert_eq!(thread_get_summarizing(&control, &target).await, true);
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
            while !terminal_record_written(&control,&terminal_identity,"adopted",Some("closed_summary")).await {
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
        settled(&control, &target).await;
        assert_eq!(thread_get_summarizing(&control, &target).await, false);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_background_summary_survives_next_turn_and_adopts_at_current_boundary() {
    let mut retained_fx: Option<DriverLifecycleFixture> = None;
    let mut retained_summary: Option<Incoming> = None;
    let result = crate::test_watchdog::watchdog("proactive summary request barrier", async {
        let mut fx = prepare_held_summary(Scenario::ClosedSummary).await;
        let terminal_identity=fx.terminal_identity.clone();
        let HeldSummaryPhase { summary, normal } = fx.held_phase.take()
            .expect("fixture holds both in-flight requests");
        retained_summary = Some(summary);
        retained_fx = Some(fx);
        let DriverLifecycleFixture {
            root,
            url,
            limits,
            owner,
            control,
            target,
            receive,
            server,
            ..
        } = retained_fx
            .as_mut()
            .expect("fixture owners remain outside the timed future");
        assert_eq!(
            retained_summary.as_ref().unwrap().body["tools"],
            json!([])
        );
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
            &normal.body,
            &retained_summary.as_ref().unwrap().body,
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
        let mut driver_events = control.subscribe();
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
        let overflow_request = {
            let mut wait = RunBWaitContext {
                control,
                identity: RunTurnWaitIdentity {
                    thread: target,
                    run: &run_id,
                    turn: &turn_id,
                },
                receive,
                events: &mut driver_events,
                slot: &slot,
            };
            wait_for_run_b_provider_request(&mut wait).await
        };
        assert_ne!(
            overflow_request.body,
            retained_summary.as_ref().unwrap().body,
            "Run B proceeds without cancelling R"
        );
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
        retained_summary
            .take()
            .expect("held summary sender remains alive until this response boundary")
            .reply
            .send((200, answer("background summary completed")))
            .unwrap();
        settled(control, target).await;
        let run_b=run_id.clone(); let turn_b=turn_id.clone();
        control.store.database.call(move|db|{
            let (turn_state,attempt_state,outcome,reason,no_stage):(String,String,String,String,bool)=db.query_row(
                "SELECT u.state,a.state,c.outcome,c.reason,c.token IS NULL AND c.closed_spill IS NULL AND c.active_spill IS NULL AND c.closed_summary IS NULL AND c.active_summary IS NULL FROM turn u JOIN model_attempt a ON a.turn_id=u.id JOIN automatic_compaction c ON c.attempt_id=a.id WHERE u.id=?1 AND u.run_id=?2 ORDER BY a.rowid DESC LIMIT 1",
                rusqlite::params![turn_b,run_b],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
            )?;
            assert_eq!(turn_state,"recovery_wait"); assert_eq!(attempt_state,"incomplete");
            assert_eq!(outcome,"adopted"); assert_eq!(reason,"same_proactive_job"); assert!(no_stage);
            Ok(())
        }).await.unwrap();
        assert!(
            receive.try_recv().is_err(),
            "an exhausted context-limit ends in the established recovery path without a duplicate POST"
        );
        crate::test_watchdog::watchdog("background summary FINISH", async {
            while !terminal_record_written(control,&terminal_identity,"adopted",Some("closed_summary")).await {
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
    if result.is_err() {
        // Keep fixture owners alive until explicit cleanup finishes.
        drop(retained_summary.take());
        if let Some(fx) = retained_fx.take() {
            let DriverLifecycleFixture {
                root,
                url,
                limits,
                owner,
                control,
                target,
                receive,
                server,
                ..
            } = fx;
            let keepalive = (root, url, limits, target, receive);
            server.abort();
            let _ = server.await;
            let _ = control.shutdown().await;
            let _ = owner.shutdown().await;
            drop(keepalive);
        }
    }
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_naturally_idle_thread_cancels_and_joins_its_retained_summary() {
    crate::test_watchdog::watchdog("ThreadDelete drains the retained proactive job", async {
        let DriverLifecycleFixture {
            owner,
            control,
            target,
            server,
            mut held_phase,
            ..
        } = prepare_held_summary(Scenario::ClosedSummary).await;
        let HeldSummaryPhase { summary, normal } = held_phase
            .take()
            .expect("proactive summary reached its provider boundary");
        assert_eq!(summary.body["tools"], json!([]));
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
        let fx = prepare_driver_scenario(Fault::Cancellation, scenario, true, true).await;
        let DriverLifecycleFixture {
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
        direct_boundary_case(Fault::Cancellation, Scenario::ClosedSpill, Some(50)).await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_summary_cancellation_captures_committed_boundary() {
    crate::test_watchdog::watchdog("direct active summary committed boundary", async {
        direct_boundary_case(Fault::Cancellation, Scenario::ActiveSummary, Some(20)).await;
    })
    .await
    .unwrap();
}

async fn direct_snapshot_for_fixture(
    control: &Arc<Control>,
    fixture: &crate::control::compaction_test_fixture::StageFixture,
) -> Box<crate::control::driver::Snapshot> {
    match crate::control::driver::next_with_clients(
        control,
        fixture.thread_id,
        &fixture.run_id,
        control.store.clients.snapshot(),
    )
    .await
    .unwrap()
    {
        crate::control::driver::Next::Ready(snapshot) => snapshot,
        _ => panic!("canonical preparation expected a runnable current Turn snapshot"),
    }
}

async fn assert_next_control_summary(
    control: &Arc<Control>,
    thread: &str,
    receive: &mut mpsc::Receiver<Incoming>,
) {
    let target = thread.to_owned();
    let node = control.store.database.call(move |db| Ok(db.query_row(
        "SELECT p.node_id FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind IN ('spill','summary') ORDER BY p.ordinal LIMIT 1",
        [target], |row| row.get::<_, String>(0))?)).await.unwrap();
    let accepted = request(
        control,
        Command::ContextEdit(ContextEditArgs {
            thread_id: Some(thread.to_owned()),
            action: ContextEditAction::Summary,
            from_node_id: Some(node.clone()),
            to_node_id: Some(node),
            max_chars: None,
        }),
    )
    .await;
    let operation = accepted["operation_id"].as_str().unwrap().to_owned();
    let mut requests = 0;
    let result = loop {
        tokio::select! {
            status = request(control, Command::OperationGet(OperationTarget { operation_id: operation.clone() })) => {
                assert_ne!(status["downstream"]["state"], "failed", "{status}");
                if status["downstream"]["state"] == "completed" {
                    break status["downstream"]["outcome"]["result"].clone();
                }
                tokio::task::yield_now().await;
            }
            incoming = receive.recv() => {
                let incoming = incoming.expect("provider remains available for the next Control operation");
                assert_eq!(incoming.body["tools"], json!([]));
                requests += 1;
                incoming.reply.send((200, answer("ok"))).unwrap();
            }
        }
    };
    assert!(
        requests > 0,
        "the next normal Control summary request is exercised"
    );
    assert_eq!(
        result["compact"]["no_target"], false,
        "next Control operation adopts a real summary"
    );
    assert!(result["compact"]["summary_node_id"].is_string());
    assert!(result["compact"]["source_nodes"].as_u64().unwrap() > 0);
    assert!(
        result["after_characters"].as_u64().unwrap()
            < result["before_characters"].as_u64().unwrap()
    );
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct StageWitness {
    pub(super) selection_revision: i64,
    pub(super) reasoning_epoch: i64,
    pub(super) cursor: Option<(i64, String)>,
    pub(super) full_path: Value,
    pub(super) selected_path_source_bytes: Value,
    pub(super) audit_path: Value,
    pub(super) audit_projection: Value,
    pub(super) audit_relations: Value,
    pub(super) replacement_provenance_and_source_bytes: Value,
    pub(super) whole_tool_group_facts: Value,
    pub(super) protected_request_receipts: Value,
}

pub(super) struct DirectBoundaryReceipt {
    pub(super) checkpoint: Value,
    pub(super) terminal: Value,
    pub(super) checkpoint_witness: StageWitness,
    pub(super) after_fault_witness: StageWitness,
    pub(super) reopened_witness: StageWitness,
    pub(super) reopen_audit_appends: Value,
}

async fn committed_selection_witness(control: &Control, thread: &str) -> StageWitness {
    let thread = thread.to_owned();
    control.store.database.call(move |db| {
        let tx = db.transaction()?;
        let selection = tx.query_row(
            "SELECT selection_revision,reasoning_epoch FROM thread WHERE id=?1",
            [&thread],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        // Read the complete selected path first. Audit suffix classification must
        // never affect the cursor or hide an unexpected node/order from the witness.
        let full_path = tx.prepare(
            "SELECT p.ordinal,n.id,n.turn_id,n.kind,n.payload_revision,n.payload_json,n.deleted_reason,n.created_at FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal",
        )?.query_map([&thread], |row| Ok((row.get::<_, i64>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,String>(3)?,row.get::<_,i64>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,Option<String>>(6)?,row.get::<_,String>(7)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let is_audit = |kind: &str| matches!(kind,"event"|"interruption_details"|"interruption_context");
        let source_path = full_path.iter().filter(|row| !is_audit(&row.3)).cloned().collect::<Vec<_>>();
        let audit_path = full_path.iter().filter(|row| is_audit(&row.3)).cloned().collect::<Vec<_>>();

        // Reuse the repository's bounded persisted-payload projection for every
        // audit node; its result kind and field paths are part of the witness.
        let mut audit_projection = Vec::with_capacity(audit_path.len());
        for row in &audit_path {
            let kind = row.3.as_str();
            let payload: Value = serde_json::from_str(row.5.as_deref().expect("selected audit node payload"))
                .map_err(|error| crate::storage::StorageError::Schema(error.to_string()))?;
            let text = crate::history::text_of_payload(&payload);
            let projection = crate::presentation::project(kind, &payload, &text, 0);
            let expected_kind = if kind == "event" {
                crate::presentation::PresentationKind::Event
            } else {
                crate::presentation::PresentationKind::Interrupted
            };
            if projection.kind != expected_kind {
                return Err(crate::storage::StorageError::Schema(format!(
                    "audit payload projection kind mismatch for node {}", row.1
                )));
            }
            audit_projection.push(json!({
                "ordinal":row.0,"node_id":row.1,"kind":kind,
                "presentation_kind":format!("{:?}",projection.kind),
                "field_paths":projection.fields.iter().map(|field| field.key.clone()).collect::<Vec<_>>(),
            }));
        }

        let events = tx.prepare(
            "SELECT p.ordinal,n.id,n.notification_event_id,e.id,e.kind,e.source_json,e.target_thread_id,e.target_run_id,e.payload_ref_json,e.created_at,(SELECT json_group_array(json_object('recipient_id',d.recipient_id,'state',d.state,'owner_thread_id',r.thread_id,'owner_run_state',r.state,'included_attempt_id',d.included_attempt_id,'included_attempt_thread_id',it.thread_id,'included_attempt_run_id',it.run_id,'processed_attempt_id',d.processed_attempt_id,'processed_attempt_thread_id',pt.thread_id,'processed_attempt_run_id',pt.run_id)) FROM delivery d LEFT JOIN run r ON r.id=d.recipient_id LEFT JOIN model_attempt ia ON ia.id=d.included_attempt_id LEFT JOIN turn it ON it.id=ia.turn_id LEFT JOIN model_attempt pa ON pa.id=d.processed_attempt_id LEFT JOIN turn pt ON pt.id=pa.turn_id WHERE d.event_id=e.id),e.semantic_json FROM thread_path p JOIN node n ON n.id=p.node_id LEFT JOIN event e ON e.id=COALESCE(n.notification_event_id,json_extract(n.payload_json,'$.event_id')) WHERE p.thread_id=?1 AND n.kind='event' ORDER BY p.ordinal",
        )?.query_map([&thread], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,Option<String>>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,Option<String>>(8)?,row.get::<_,Option<String>>(9)?,row.get::<_,Option<String>>(10)?,row.get::<_,Option<String>>(11)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let provenance = tx.prepare(
            "SELECT tr.replacement_id,r.output_node_id,out.kind,rs.ordinal,rs.source_node_id,rs.source_revision,rs.source_hash,src.payload_json FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node out ON out.id=r.output_node_id LEFT JOIN replacement_source rs ON rs.replacement_id=r.id LEFT JOIN node src ON src.id=rs.source_node_id WHERE tr.thread_id=?1 ORDER BY r.created_at,r.id,rs.ordinal",
        )?.query_map([&thread], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,Option<i64>>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<i64>>(5)?,row.get::<_,Option<String>>(6)?,row.get::<_,Option<String>>(7)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let runs = tx.prepare("SELECT id,thread_id,state FROM run WHERE thread_id=?1 ORDER BY created_at,rowid")?.query_map([&thread],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let turns = tx.prepare("SELECT id,run_id,state FROM turn WHERE thread_id=?1 ORDER BY created_at,rowid")?.query_map([&thread],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let shells = tx.prepare("SELECT s.id,s.owner_run_id,s.turn_id,s.state FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE r.thread_id=?1 ORDER BY s.started_at,s.rowid")?.query_map([&thread],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let groups = tx.prepare(
            "SELECT g.id,g.turn_id,g.attempt_id,g.state,c.call_id,c.ordinal,c.state,r.node_id,r.status FROM tool_group g JOIN turn t ON t.id=g.turn_id LEFT JOIN tool_call c ON c.group_id=g.id LEFT JOIN tool_result r ON r.group_id=c.group_id AND r.call_id=c.call_id WHERE t.thread_id=?1 ORDER BY g.rowid,c.ordinal,r.node_id",
        )?.query_map([&thread], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<i64>>(5)?,row.get::<_,Option<String>>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,Option<String>>(8)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let receipts = tx.prepare("SELECT id,target_thread_id,run_id,turn_id,message_node_id,state FROM receipt WHERE target_thread_id=?1 ORDER BY created_at,id")?.query_map([&thread],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;

        // Validate persisted interruption-detail/context ownership links against
        // their actual selected positions and source rows, not a kind-filtered suffix.
        let mut context_owned_detail_nodes = Vec::new();
        for row in full_path.iter().filter(|row| row.3 == "interruption_context") {
            let payload: Value = serde_json::from_str(row.5.as_deref().expect("interruption context payload"))
                .map_err(|error| crate::storage::StorageError::Schema(error.to_string()))?;
            let text = payload["text"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption context has no text envelope".into()))?;
            let context: Value = serde_json::from_str(text)
                .map_err(|error| crate::storage::StorageError::Schema(error.to_string()))?;
            let kind = context["kind"].as_str().ok_or_else(|| crate::storage::StorageError::Schema("interruption snapshot kind is absent".into()))?;
            let actor = context["actor"].as_object().ok_or_else(|| crate::storage::StorageError::Schema("interruption snapshot actor is not an object".into()))?;
            let source = actor.get("source").and_then(Value::as_str);
            let producer_pair = matches!((kind, source),
                ("app_shutdown", Some("shutdown"))
                    | ("previous_instance_interrupted", Some("startup"))
            );
            if context["snapshot_node_id"] != row.1
                || row.2.is_some()
                || row.6.is_some()
                || !producer_pair
                || actor.get("kind").and_then(Value::as_str) != Some("shellboy")
                || context["observed_at"].as_str().is_none()
                || context["timezone"].as_str().is_none()
                || context["change"].as_object().is_none()
                || context["change"]["reason"] != kind
                || context["change"]["result_uncertain"] != true
                || context["change"]["automatic_replay"] != false
                || context["change"]["os_processes_inspected"] != false
            {
                return Err(crate::storage::StorageError::Schema(
                    "interruption snapshot envelope/actor/source differs from its producer contract".into(),
                ));
            }
            for section in ["runs", "turns", "shells", "tool_calls", "unreflected_messages", "cancelled_waits", "pending_operations_with_unknown_targets"] {
                let saved = &context["change"][section];
                let items = saved["items"].as_array().ok_or_else(|| crate::storage::StorageError::Schema(format!("interruption section {section} has no item array")))?;
                let count = saved["count"].as_u64().ok_or_else(|| crate::storage::StorageError::Schema(format!("interruption section {section} has no item count")))?;
                let detail_count = saved["detail_node_count"].as_u64().ok_or_else(|| crate::storage::StorageError::Schema(format!("interruption section {section} has no detail count")))?;
                let ids = saved["detail_node_ids"].as_array().ok_or_else(|| crate::storage::StorageError::Schema(format!("interruption section {section} has no detail-id array")))?;
                if saved["section"] != section
                    || saved["details_kind"] != "interruption_details"
                    || saved["truncated"] != (count > items.len() as u64)
                    || items.len() as u64 != count.min(64)
                    || ids.len() as u64 != detail_count.min(64)
                {
                    return Err(crate::storage::StorageError::Schema(format!("interruption section {section} shape/count mismatch")));
                }
                let actual_detail_ids = full_path.iter().filter(|candidate| {
                    candidate.3 == "interruption_details" && candidate.7 == row.7
                }).filter_map(|candidate| {
                    let payload=candidate.5.as_deref().and_then(|raw|serde_json::from_str::<Value>(raw).ok())?;
                    let text=payload["text"].as_str()?;
                    let body=serde_json::from_str::<Value>(text).ok()?;
                    (body["section"]==section && body["items"].is_array()).then(||candidate.1.clone())
                }).collect::<Vec<_>>();
                if actual_detail_ids.len() as u64 != detail_count {
                    return Err(crate::storage::StorageError::Schema(format!("interruption section {section} detail count disagrees with same-snapshot selected nodes")));
                }
                if actual_detail_ids.iter().any(|id| {
                    full_path.iter().find(|candidate| candidate.1 == *id)
                        .is_none_or(|detail| detail.0 >= row.0 || detail.2.is_some() || detail.6.is_some())
                }) {
                    return Err(crate::storage::StorageError::Schema(format!("interruption section {section} contains a same-batch detail that is not earlier, owned and undeleted")));
                }
                let expected_references=actual_detail_ids.iter().take(64).cloned().collect::<Vec<_>>();
                let saved_references=ids.iter().map(|id|id.as_str().ok_or_else(||crate::storage::StorageError::Schema(format!("interruption section {section} detail reference is not text")))).collect::<Result<Vec<_>,_>>()?;
                if saved_references.iter().map(|id|id.to_string()).collect::<Vec<_>>()!=expected_references {
                    return Err(crate::storage::StorageError::Schema(format!("interruption section {section} references are not the producer's exact ordered 64-node prefix")));
                }
                context_owned_detail_nodes.extend(actual_detail_ids);
                for id in ids.iter().filter_map(Value::as_str) {
                    let detail = full_path.iter().find(|candidate| candidate.1 == id)
                        .ok_or_else(|| crate::storage::StorageError::Schema(format!("interruption detail {id} is not selected")))?;
                    if detail.0 >= row.0
                        || detail.3 != "interruption_details"
                        || detail.2.is_some()
                        || detail.6.is_some()
                        || detail.7 != row.7
                    {
                        return Err(crate::storage::StorageError::Schema(format!("interruption detail {id} is not an earlier same-snapshot detail node")));
                    }
                    let detail_payload: Value = serde_json::from_str(detail.5.as_deref().expect("detail payload"))
                        .map_err(|error| crate::storage::StorageError::Schema(error.to_string()))?;
                    let detail_text = detail_payload["text"].as_str().ok_or_else(|| crate::storage::StorageError::Schema("interruption detail has no text envelope".into()))?;
                    let detail_body: Value = serde_json::from_str(detail_text)
                        .map_err(|error| crate::storage::StorageError::Schema(error.to_string()))?;
                    if detail_body["section"] != section || !detail_body["items"].is_array() {
                        return Err(crate::storage::StorageError::Schema(format!("interruption detail {id} section/body mismatch")));
                    }
                }
            }
            for item in context["change"]["runs"]["items"].as_array().into_iter().flatten() {
                let id = item["run_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption run has no id".into()))?;
                if !runs.iter().any(|(run,owner,_)|run==id && owner==&thread) {
                    return Err(crate::storage::StorageError::Schema(format!("interruption run {id} is not owned by target Thread")));
                }
            }
            for item in context["change"]["turns"]["items"].as_array().into_iter().flatten() {
                let id = item["turn_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption turn has no id".into()))?;
                let run = item["run_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption turn has no Run owner".into()))?;
                if !turns.iter().any(|(turn,owner,_)|turn==id && owner==run) {
                    return Err(crate::storage::StorageError::Schema(format!("interruption Turn {id} has mismatched owner")));
                }
            }
            for item in context["change"]["shells"]["items"].as_array().into_iter().flatten() {
                let id = item["execution_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption shell has no id".into()))?;
                let run = item["run_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption shell has no Run owner".into()))?;
                let turn = item["turn_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption shell has no Turn owner".into()))?;
                if !shells.iter().any(|(execution,owner,owner_turn,_)|execution==id && owner==run && owner_turn==turn) {
                    return Err(crate::storage::StorageError::Schema(format!("interruption shell {id} has mismatched owner")));
                }
            }
            for item in context["change"]["tool_calls"]["items"].as_array().into_iter().flatten() {
                let group = item["group_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption tool call has no group id".into()))?;
                let call = item["call_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption tool call has no call id".into()))?;
                let run = item["run_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption tool call has no Run owner".into()))?;
                let turn = item["turn_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption tool call has no Turn owner".into()))?;
                if !turns.iter().any(|(id,owner,_)|id==turn && owner==run)
                    || !groups.iter().any(|row|row.0==group && row.1==turn && row.4.as_deref()==Some(call))
                {
                    return Err(crate::storage::StorageError::Schema(format!("interruption tool call {group}/{call} has mismatched group or Turn owner")));
                }
            }
            for item in context["change"]["unreflected_messages"]["items"].as_array().into_iter().flatten() {
                let id = item["receipt_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption receipt has no id".into()))?;
                let node = item["message_node_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interruption receipt has no message node".into()))?;
                let run = item["run_id"].as_str();
                let turn = item["turn_id"].as_str();
                if !receipts.iter().any(|(receipt,owner,receipt_run,receipt_turn,message,_)|receipt==id && owner==&thread && receipt_run.as_deref()==run && receipt_turn.as_deref()==turn && message==node) {
                    return Err(crate::storage::StorageError::Schema(format!("interruption receipt {id} has mismatched Thread/Run/Turn/message ownership")));
                }
            }
            for item in context["change"]["cancelled_waits"]["items"].as_array().into_iter().flatten() {
                let operation = item["operation_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interrupted wait has no operation id".into()))?;
                if let Some(node) = item["node_id"].as_str() {
                    let saved = full_path.iter().find(|candidate|candidate.1==node)
                        .ok_or_else(||crate::storage::StorageError::Schema(format!("interrupted wait {operation} points outside the selected path")))?;
                    if saved.3!="interrupted_wait" {
                        return Err(crate::storage::StorageError::Schema(format!("interrupted wait {operation} points to a non-wait node")));
                    }
                    let saved_payload:Value=serde_json::from_str(saved.5.as_deref().expect("interrupted wait payload"))
                        .map_err(|error|crate::storage::StorageError::Schema(error.to_string()))?;
                    let saved_text=saved_payload["text"].as_str().ok_or_else(||crate::storage::StorageError::Schema("interrupted wait has no text".into()))?;
                    let original:Value=serde_json::from_str(saved_text)
                        .map_err(|error|crate::storage::StorageError::Schema(error.to_string()))?;
                    if original["operation_id"]!=operation {
                        return Err(crate::storage::StorageError::Schema(format!("interrupted wait {operation} identity differs from its original")));
                    }
                }
            }
        }
        for detail in full_path.iter().filter(|row|row.3=="interruption_details") {
            if detail.2.is_some() || detail.6.is_some() || !context_owned_detail_nodes.iter().any(|id|id==&detail.1) {
                return Err(crate::storage::StorageError::Schema(format!("selected interruption detail {} has no earlier same-batch producer context/section owner",detail.1)));
            }
        }
        for event in &events {
            let event_id=event.3.as_deref().ok_or_else(||crate::storage::StorageError::Schema(format!("selected event node {} has no backing event row",event.1)))?;
            {
                if event.2.as_deref().is_some_and(|marker| marker != event_id) {
                    return Err(crate::storage::StorageError::Schema(format!("event node {} notification marker identity differs from its event row",event.1)));
                }
                let source: Value = serde_json::from_str(event.5.as_deref().ok_or_else(||crate::storage::StorageError::Schema("event row source_json is absent".into()))?)
                    .map_err(|error|crate::storage::StorageError::Schema(error.to_string()))?;
                if !source.is_object() {
                    return Err(crate::storage::StorageError::Schema(format!("event {} source_json is not an object",event_id)));
                }
                let payload_ref: Value = serde_json::from_str(event.8.as_deref().ok_or_else(||crate::storage::StorageError::Schema("event payload_ref_json is absent".into()))?)
                    .map_err(|error|crate::storage::StorageError::Schema(error.to_string()))?;
                if let Some(semantic) = event.11.as_deref() {
                    let fact = crate::history::notification::decode(semantic)
                        .ok_or_else(||crate::storage::StorageError::Schema(format!("event {event_id} semantic fact is not accepted by the existing typed notification reader")))?;
                    if event.4.as_deref()==Some("context_changed") && fact["kind"]=="turn_started"
                        && (!source["kind"].as_str().is_some_and(|kind|matches!(kind,"shellboy"|"human"))
                            || payload_ref["actor"]!=source
                            || payload_ref["change"]["run_id"].as_str().is_none()
                            || payload_ref["change"]["turn_id"].as_str().is_none())
                    {
                        return Err(crate::storage::StorageError::Schema(format!("turn-start event {event_id} source/owner differs from its producer envelope")));
                    }
                }
                if event.4.as_deref()==Some("context_changed")
                    && (payload_ref["kind"].as_str().is_none()
                        || payload_ref["actor"] != source
                        || payload_ref["change"].as_object().is_none()
                        || payload_ref["observed_at"].as_str().is_none()
                        || payload_ref["timezone"].as_str().is_none())
                {
                    return Err(crate::storage::StorageError::Schema(format!("context event {} source/payload does not match its producer envelope",event_id)));
                }
                let node = full_path.iter().find(|row| row.1 == event.1)
                    .ok_or_else(||crate::storage::StorageError::Schema(format!("event node {} is absent from the full selected path",event.1)))?;
                let node_payload: Value = serde_json::from_str(node.5.as_deref().ok_or_else(||crate::storage::StorageError::Schema("selected event node payload is absent".into()))?)
                    .map_err(|error|crate::storage::StorageError::Schema(error.to_string()))?;
                if node_payload["event_id"] != event_id {
                    return Err(crate::storage::StorageError::Schema(format!("event node {} payload identity differs from its event row",event.1)));
                }
                let text = node_payload.pointer("/content/0/text").and_then(Value::as_str)
                    .and_then(|text|text.strip_prefix("Shellboy event v1: "))
                    .map(|text|text.split_once('\n').map_or(text, |(envelope, _)| envelope))
                    .ok_or_else(||crate::storage::StorageError::Schema(format!("event node {} has no canonical event-v1 envelope",event.1)))?;
                let envelope: Value = serde_json::from_str(text)
                    .map_err(|error|crate::storage::StorageError::Schema(error.to_string()))?;
                if envelope["event_id"] != event_id || envelope["kind"] != event.4.as_deref().unwrap_or_default() || envelope["payload"] != payload_ref {
                    return Err(crate::storage::StorageError::Schema(format!("event row and selected envelope disagree at node {}",event.1)));
                }
            }
            if let Some(target) = event.6.as_deref()
                && target != thread
            {
                return Err(crate::storage::StorageError::Schema(format!("event {} targets another Thread",event.1)));
            }
            if let Some(run) = event.7.as_deref()
                && !runs.iter().any(|(id,owner,_)|id==run && owner==&thread)
            {
                return Err(crate::storage::StorageError::Schema(format!("event {} targets a Run outside its Thread",event.1)));
            }
            if let Some(serialized) = event.10.as_deref() {
                let deliveries: Value = serde_json::from_str(serialized)
                    .map_err(|error|crate::storage::StorageError::Schema(error.to_string()))?;
                for delivery in deliveries.as_array().into_iter().flatten() {
                    let recipient = delivery["recipient_id"].as_str().ok_or_else(||crate::storage::StorageError::Schema("event delivery has no recipient Run".into()))?;
                    if delivery["owner_thread_id"].as_str() != Some(thread.as_str())
                        || !runs.iter().any(|(run,owner,_)|run==recipient && owner==&thread)
                    {
                        return Err(crate::storage::StorageError::Schema(format!("event {} delivery recipient is not a Run owned by its target Thread",event.1)));
                    }
                    for (attempt_id, attempt_thread, attempt_run) in [
                        (&delivery["included_attempt_id"], &delivery["included_attempt_thread_id"], &delivery["included_attempt_run_id"]),
                        (&delivery["processed_attempt_id"], &delivery["processed_attempt_thread_id"], &delivery["processed_attempt_run_id"]),
                    ] {
                        if !attempt_id.is_null()
                            && (attempt_thread.as_str()!=Some(thread.as_str()) || attempt_run.as_str()!=Some(recipient))
                        {
                            return Err(crate::storage::StorageError::Schema(format!("event {} delivery attempt owner disagrees with recipient Run",event.1)));
                        }
                    }
                }
            }
        }

        let cursor = full_path.last().map(|(ordinal,node,..)| (*ordinal,node.clone()));
        let witness = StageWitness {
            selection_revision: selection.0,
            reasoning_epoch: selection.1,
            cursor,
            full_path: json!(full_path),
            selected_path_source_bytes: json!(source_path),
            audit_path: json!(audit_path),
            audit_projection: json!(audit_projection),
            audit_relations: json!({"events":events,"runs":runs,"turns":turns,"shells":shells,"tool_groups":groups,"receipts":receipts}),
            replacement_provenance_and_source_bytes: json!(provenance),
            whole_tool_group_facts: json!(groups),
            protected_request_receipts: json!(receipts),
        };
        tx.commit()?;
        Ok(witness)
    }).await.unwrap()
}

const SAVED_RECORD: &str = "json_object('token',c.token,'outcome',COALESCE(c.outcome,'unknown'),'reason',c.reason,'unfinished',json(iif(c.outcome IS NULL,'true','false')),'trigger',json(c.trigger_json),'summary',json(c.result_json->'$.metrics'),'error',json(c.result_json->'$.error'),'compaction_outcome',json(c.failure_json),'committed_stages',json((SELECT json_group_object(key,json(value)) FROM json_each(json_object('closed_spill',c.closed_spill,'active_spill',c.active_spill,'closed_summary',c.closed_summary,'active_summary',c.active_summary)) WHERE value IS NOT NULL)))";

async fn automatic_attempt_record(
    control: &Control,
    fixture: &crate::control::compaction_test_fixture::StageFixture,
) -> Value {
    let (thread, attempt) = (fixture.thread_id.to_owned(), fixture.attempt_id.to_owned());
    let record = control.store.database.call(move |db| {
        db.query_row(
            &format!("SELECT {SAVED_RECORD} FROM automatic_compaction c WHERE attempt_id=?1 AND turn_id IN (SELECT id FROM turn WHERE thread_id=?2)"),
            rusqlite::params![attempt, thread],
            |row| row.get::<_, String>(0),
        ).map_err(Into::into)
    }).await.unwrap();
    serde_json::from_str(&record).unwrap()
}

async fn direct_boundary_case(
    fault: Fault,
    scenario: Scenario,
    thread_target_percent: Option<u64>,
) -> DirectBoundaryReceipt {
    assert!(matches!(
        fault,
        Fault::Storage
            | Fault::Cancellation
            | Fault::Remeasurement
            | Fault::RetiredWritesAbsent
            | Fault::Persistent
            | Fault::RejectFinal
            | Fault::RejectFact
    ));
    use crate::control::compaction_test_fixture::{StagePlacement, stage_fixture};

    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, _heads, server) = proxy().await;
    let connection_target = thread_target_percent.unwrap_or(scenario.target_percent());
    let limits = format!(
        "auto_compact_target_percent={connection_target}\n[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n"
    );
    let owner = proactive_owner(root.path(), &url, &limits).await;
    let control = Control::new(&owner, root.path().into());
    let cwd = root.path().to_string_lossy().into_owned();
    let agents = root.path().join("AGENTS.md").to_string_lossy().into_owned();
    control.store.database.call(move |db| {
        db.execute(
            "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,auto_compact_at,auto_compact_target_percent,spill_override,updated_at) VALUES('t',?1,?2,'fixture','fixture',200,?3,?4,'now')",
            rusqlite::params![cwd, agents, thread_target_percent.map(|value| value as i64), Some(true)],
        )?;
        Ok(())
    }).await.unwrap();
    let placement = if scenario.active() {
        StagePlacement::ActiveTurn
    } else {
        StagePlacement::ClosedPrefix
    };
    let target_percent = thread_target_percent.unwrap_or(scenario.target_percent());
    let fixture = stage_fixture(&control.store.database, placement, target_percent, true).await;

    let slot = DriverSlot {
        id: "direct-slot".into(),
        run_id: fixture.run_id.clone(),
        ..DriverSlot::default()
    };
    if scenario.active() {
        slot.turn
            .lock()
            .await
            .replace(crate::control::turn_interrupt::TurnBinding {
                turn: fixture.turn_id.into(),
                cancel: tokio_util::sync::CancellationToken::new(),
                start_gate: Arc::new(tokio::sync::Mutex::new(false)),
            });
    }
    let mut snapshot = direct_snapshot_for_fixture(&control, &fixture).await;
    // maybe_compact_with_run_stop drops an active replay lease before compaction.
    let plan = preflight(&control, &snapshot)
        .await
        .unwrap()
        .expect("saved provider usage makes this threshold attempt eligible");
    assert_eq!(plan.attempt_id, fixture.attempt_id);
    assert_eq!(plan.input_tokens, fixture.input_tokens);
    assert_eq!(
        plan.a_input_tokens,
        fixture.spill_effective.then_some(fixture.a_input_tokens)
    );
    assert_eq!(plan.target_percent, fixture.target_percent);
    assert_eq!(plan.target_tokens, scenario.target_tokens());
    assert_eq!(plan.spill_effective, fixture.spill_effective);
    let expected_attempt = plan.attempt_id.clone();

    let checkpoint_reached = Arc::new(AtomicBool::new(false));
    let release_checkpoint = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::new(AtomicBool::new(false));
    let stage = scenario.stage();
    let kind = match fault {
        Fault::Cancellation => 0,
        Fault::Storage => 1,
        Fault::Remeasurement => 2,
        Fault::Persistent => 4,
        Fault::RejectFinal => 6,
        Fault::RejectFact => 8,
        Fault::RetiredWritesAbsent => 9,
    };
    *control.automatic_fault.lock().unwrap() = Some(crate::control::AutomaticFault {
        stage,
        kind,
        reached: reached.clone(),
        checkpoint_reached: checkpoint_reached.clone(),
        release_checkpoint: release_checkpoint.clone(),
    });
    if fault == Fault::RejectFact {
        let hit = reached.clone();
        let stage_name = stage;
        control.store.database.call(move |db| {
            db.create_scalar_function("direct_fault_fact_hit", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| {
                hit.store(true, Ordering::SeqCst);
                Ok(1i64)
            })?;
            db.execute_batch(&format!("CREATE TEMP TRIGGER direct_fault_fact BEFORE UPDATE OF {stage_name} ON automatic_compaction
                WHEN NEW.{stage_name} IS NOT NULL
                BEGIN SELECT direct_fault_fact_hit(); SELECT RAISE(ABORT,'injected direct stage-fact rejection'); END;"))?;
            Ok(())
        }).await.unwrap();
    }
    // A rejected spill never reaches a provider request, so preserve its
    // pre-target path here. Summary RejectFact keeps the post-spill,
    // pre-response baseline captured at the existing provider cut below.
    let rejected_spill_pre_target = if fault == Fault::RejectFact && !scenario.summary() {
        Some(committed_selection_witness(&control, fixture.thread_id).await)
    } else {
        None
    };
    let boundary_mutex = tokio::sync::Mutex::new(());
    let guard = boundary_mutex.lock().await;
    let mut boundary = crate::compaction::boundary::Boundary::new(&boundary_mutex, guard);
    let run_stop = slot.stop.clone();
    let (result, identity, checkpoint_record, checkpoint_witness) = {
        let direct = maybe_compact_with_run_stop(
            &control,
            &mut snapshot,
            &slot,
            &mut boundary,
            run_stop,
            plan,
        );
        tokio::pin!(direct);
        let mut identity = None;
        let mut rejected_stage_baseline = None;
        if fault == Fault::RejectFact {
            let result = loop {
                tokio::select! {
                    result = &mut direct => break result,
                    request = receive.recv() => {
                        let request = request.expect("direct summary keeps provider available through rejected stage");
                        assert!(request.body["tools"].as_array().is_some_and(Vec::is_empty));
                        if identity.is_none() {
                            identity = Some(capture_terminal_identity(&control, fixture.thread_id, true).await);
                            rejected_stage_baseline = Some(committed_selection_witness(&control, fixture.thread_id).await);
                        }
                        request.reply.send((200, answer("## Goal\nPreserve the active turn.\n## Constraints\nDo not alter protected input.\n## Current state\nThree complete groups remain.\n## Open issues and next steps\nContinue after summary.\n## Key references\nCurrent fixture."))).unwrap();
                    }
                    _ = tokio::task::yield_now() => {}
                }
            };
            let identity = match identity {
                Some(identity) => identity,
                None => capture_terminal_identity(&control, fixture.thread_id, false).await,
            };
            let checkpoint_record = automatic_attempt_record(&control, &fixture).await;
            let checkpoint_witness = match (rejected_stage_baseline, rejected_spill_pre_target) {
                (Some(baseline), _) => baseline,
                (None, Some(baseline)) => baseline,
                (None, None) => committed_selection_witness(&control, fixture.thread_id).await,
            };
            (result, identity, checkpoint_record, checkpoint_witness)
        } else {
            while !checkpoint_reached.load(Ordering::SeqCst) {
                tokio::select! {
                    result = &mut direct => panic!("direct boundary returned before checkpoint: fault={fault:?} scenario={scenario:?} stage={stage} reached={} result={result:?}", reached.load(Ordering::SeqCst)),
                    request = receive.recv() => {
                        let request = request.expect("direct summary pipeline keeps the provider channel open");
                        assert!(request.body["tools"].as_array().is_some_and(Vec::is_empty));
                        if identity.is_none() {
                            identity = Some(capture_terminal_identity(&control, fixture.thread_id, true).await);
                        }
                        request.reply.send((200, answer("## Goal\nPreserve the active turn.\n## Constraints\nDo not alter protected input.\n## Current state\nThree complete groups remain.\n## Open issues and next steps\nContinue after summary.\n## Key references\nCurrent fixture."))).unwrap();
                    }
                    _ = tokio::task::yield_now() => {}
                }
            }
            let identity = match identity {
                Some(identity) => identity,
                None => capture_terminal_identity(&control, fixture.thread_id, true).await,
            };
            assert_eq!(
                identity.attempt, expected_attempt,
                "FINISH targets the original captured receipt"
            );
            let checkpoint_record = automatic_attempt_record(&control, &fixture).await;
            let checkpoint_witness = committed_selection_witness(&control, fixture.thread_id).await;
            if fault == Fault::Remeasurement {
                assert!(
                    control.compactor.fail_remeasurement.load(Ordering::SeqCst),
                    "remeasurement failure is armed at its committed checkpoint"
                );
            }
            release_checkpoint.notify_one();
            let result = loop {
                tokio::select! {
                    result = &mut direct => break result,
                    request = receive.recv() => {
                        let request = request.expect("follow-on direct summary keeps the provider channel open");
                        assert!(request.body["tools"].as_array().is_some_and(Vec::is_empty));
                        request.reply.send((200, answer("## Goal\nPreserve the active turn.\n## Constraints\nDo not alter protected input.\n## Current state\nThree complete groups remain.\n## Open issues and next steps\nContinue after summary.\n## Key references\nCurrent fixture."))).unwrap();
                    }
                }
            };
            (result, identity, checkpoint_record, checkpoint_witness)
        }
    };
    let expects_error = matches!(
        fault,
        Fault::Storage
            | Fault::Remeasurement
            | Fault::Persistent
            | Fault::RejectFact
            | Fault::RetiredWritesAbsent
    ) || fault == Fault::Cancellation && scenario == Scenario::ActiveSpill
        || fault == Fault::RejectFinal;
    assert_eq!(
        result.is_err(),
        expects_error,
        "direct proactive result: {result:?}"
    );
    let reject_final_write_expected = fault == Fault::RejectFinal;
    assert_eq!(
        reached.load(Ordering::SeqCst),
        fault != Fault::RejectFinal || reject_final_write_expected,
        "RejectFinal reaches the sole terminal writer"
    );
    if fault == Fault::Cancellation {
        assert!(
            slot.stop.is_cancelled(),
            "the postcommit cancellation hook ran"
        );
    }
    let after_fault_witness = committed_selection_witness(&control, fixture.thread_id).await;
    let after_fault_record = automatic_attempt_record(&control, &fixture).await;
    let stage_names = [
        "closed_spill",
        "closed_summary",
        "active_spill",
        "active_summary",
    ];
    let checkpoint_stages = &checkpoint_record["committed_stages"];
    let after_stages = &after_fault_record["committed_stages"];
    if let Some(checkpoint_stage) = stage_names
        .iter()
        .rev()
        .find(|stage| checkpoint_stages[stage].is_object())
    {
        assert_eq!(
            checkpoint_stages[checkpoint_stage]["selection_revision"].as_i64(),
            Some(checkpoint_witness.selection_revision),
            "checkpoint's latest committed stage receipt must match its captured path revision"
        );
    }
    let mut receipt_revision = checkpoint_witness.selection_revision;
    let mut newly_committed_path_stages = 0i64;
    for stage in stage_names {
        if after_stages[stage].is_object() && checkpoint_stages[stage] != after_stages[stage] {
            let stage_revision = after_stages[stage]["selection_revision"]
                .as_i64()
                .expect("committed stage receipt records its post-adoption selection revision");
            assert_eq!(
                stage_revision,
                receipt_revision + 1,
                "each changed stage receipt follows exactly one committed path adoption: checkpoint={checkpoint_stages}, after={after_stages}"
            );
            receipt_revision = stage_revision;
            newly_committed_path_stages += 1;
        }
    }
    let checkpoint_audit = checkpoint_witness.audit_path.as_array().unwrap();
    let after_audit = after_fault_witness.audit_path.as_array().unwrap();
    let checkpoint_audit_ids = checkpoint_audit
        .iter()
        .map(|row| row[1].as_str().unwrap())
        .collect::<Vec<_>>();
    let after_audit_ids = after_audit
        .iter()
        .map(|row| row[1].as_str().unwrap())
        .collect::<Vec<_>>();
    let retained_audit_ids = after_audit_ids
        .iter()
        .filter(|id| checkpoint_audit_ids.contains(id))
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        retained_audit_ids, checkpoint_audit_ids,
        "stage path replacement preserves old audit-node order; ordinal shifts are witnessed in the full path"
    );
    let newly_appended_audit_nodes = after_audit_ids
        .iter()
        .filter(|id| !checkpoint_audit_ids.contains(id))
        .count() as i64;
    let path_identity = |path: &Value| {
        path.as_array()
            .unwrap()
            .iter()
            .map(|row| json!([row[0], row[1], row[3]]))
            .collect::<Vec<_>>()
    };
    let replacement_identity = |witness: &StageWitness| {
        witness
            .replacement_provenance_and_source_bytes
            .as_array()
            .unwrap()
            .iter()
            .map(|row| json!([row[0], row[1], row[2], row[3], row[4], row[5], row[6]]))
            .collect::<Vec<_>>()
    };
    let revision_context = json!({
        "attempt_id":fixture.attempt_id,
        "token":checkpoint_record["token"],
        "checkpoint_revision":checkpoint_witness.selection_revision,
        "after_revision":after_fault_witness.selection_revision,
        "checkpoint_stages":checkpoint_stages,
        "after_stages":after_stages,
        "new_stage_count":newly_committed_path_stages,
        "new_audit_count":newly_appended_audit_nodes,
        "checkpoint_path_ids_kinds":path_identity(&checkpoint_witness.full_path),
        "after_path_ids_kinds":path_identity(&after_fault_witness.full_path),
        "checkpoint_replacements":replacement_identity(&checkpoint_witness),
        "after_replacements":replacement_identity(&after_fault_witness),
    });
    assert_eq!(
        after_fault_witness.selection_revision,
        checkpoint_witness.selection_revision
            + newly_committed_path_stages
            + newly_appended_audit_nodes,
        "exact direct revision chronology mismatch: {revision_context}"
    );
    assert_eq!(
        after_fault_witness.cursor,
        after_fault_witness
            .full_path
            .as_array()
            .unwrap()
            .last()
            .map(|row| (
                row[0].as_i64().unwrap(),
                row[1].as_str().unwrap().to_owned()
            )),
        "cursor is the actual full selected-path tail, including audit nodes"
    );
    let follows_active_summary = scenario == Scenario::ActiveSpill && fault == Fault::Remeasurement;
    if follows_active_summary {
        let checkpoint_path = checkpoint_witness
            .selected_path_source_bytes
            .as_array()
            .unwrap();
        let final_path = after_fault_witness
            .selected_path_source_bytes
            .as_array()
            .unwrap();
        assert!(
            checkpoint_path.iter().any(|entry| entry[3] == "spill"),
            "the Remeasurement checkpoint contains the already-adopted spill"
        );
        assert!(
            final_path.iter().any(|entry| entry[3] == "summary"),
            "the follow-on active summary is adopted before the fault future returns"
        );
        for id in ["current-user", "g3-call", "g3-result"] {
            let source_bytes_and_facts = |path: &[Value]| {
                path.iter().find(|entry| entry[1] == id).map(|entry| {
                    json!([entry[1], entry[2], entry[3], entry[4], entry[5], entry[6]])
                })
            };
            assert_eq!(
                source_bytes_and_facts(checkpoint_path),
                source_bytes_and_facts(final_path),
                "protected/latest source {id} remains byte-identical across the real follow-on summary; ordinal shifts are caused by the selected summary replacement"
            );
        }
        let follow_on = automatic_attempt_record(&control, &fixture).await;
        let active_summary = &follow_on["committed_stages"]["active_summary"];
        assert_eq!(
            active_summary["captured_reasoning_epoch"], checkpoint_witness.reasoning_epoch,
            "the follow-on summary captures the spill checkpoint epoch"
        );
        assert_eq!(
            active_summary["reasoning_epoch"], after_fault_witness.reasoning_epoch,
            "the committed follow-on summary records the resulting adopted epoch"
        );
        let summary_id = active_summary["summary_node_id"].as_str().unwrap();
        let summary_path = final_path
            .iter()
            .find(|entry| entry[1] == summary_id && entry[3] == "summary")
            .unwrap();
        let summary_payload: Value =
            serde_json::from_str(summary_path[5].as_str().unwrap()).unwrap();
        assert_eq!(
            summary_payload["reasoning_epoch"],
            active_summary["reasoning_epoch"]
        );
        let source_rows = after_fault_witness
            .replacement_provenance_and_source_bytes
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row[1] == summary_id && row[2] == "summary")
            .collect::<Vec<_>>();
        assert_eq!(
            source_rows.len() as u64,
            active_summary["source_nodes"].as_u64().unwrap()
        );
        let checkpoint_path = checkpoint_witness
            .selected_path_source_bytes
            .as_array()
            .unwrap();
        let expected_r = checkpoint_path
            .iter()
            .filter(|row| row[3] == "spill")
            .map(|row| row[1].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let provenance_r = source_rows
            .iter()
            .map(|row| row[4].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let payload_r = summary_payload["source_node_ids"]
            .as_array()
            .expect("adopted summary stores its ordered source IDs")
            .iter()
            .map(|id| {
                id.as_str()
                    .expect("summary source ID is a string")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert!(
            !expected_r.is_empty(),
            "the checkpoint includes the active spill R"
        );
        assert_eq!(
            provenance_r, expected_r,
            "replacement provenance is exactly the ordered pre-summary active R"
        );
        assert_eq!(
            payload_r, expected_r,
            "persisted summary source_node_ids exactly certify the ordered pre-summary R"
        );
        let checkpoint_sources = checkpoint_witness
            .selected_path_source_bytes
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[1].as_str().unwrap())
            .collect::<Vec<_>>();
        let mut previous_source_position = None;
        for source in &source_rows {
            let source_id = source[4].as_str().unwrap();
            let source_revision = source[5].as_i64().unwrap();
            let source_hash = source[6].as_str().unwrap();
            let source_position = checkpoint_sources
                .iter()
                .position(|id| *id == source_id)
                .expect("replacement source is in the checkpoint selected path");
            assert!(previous_source_position.is_none_or(|previous| source_position > previous));
            previous_source_position = Some(source_position);
            let selected_source = checkpoint_witness
                .selected_path_source_bytes
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row[1] == source_id)
                .expect("every expected R marker is selected in the pre-summary checkpoint");
            let checkpoint_revision = selected_source[4]
                .as_i64()
                .expect("checkpoint source revision is persisted");
            assert_eq!(source_revision, checkpoint_revision);
            let expected_bytes = selected_source[5]
                .as_str()
                .expect("checkpoint source bytes are retained");
            assert_eq!(source[7].as_str().unwrap(), expected_bytes);
            let checkpoint_payload: Value = serde_json::from_str(expected_bytes).unwrap();
            let expected_hash = crate::digest::sha256_hex(
                serde_json::to_vec(&(checkpoint_revision, &checkpoint_payload)).unwrap(),
            );
            assert_eq!(
                source_hash, expected_hash,
                "replacement source hash matches its exact checkpoint revision and payload bytes"
            );
        }
        let checkpoint_full_ids = checkpoint_witness
            .full_path
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[1].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let splice_start = checkpoint_full_ids
            .iter()
            .position(|id| id == &expected_r[0])
            .expect("the first expected R source has its actual full-path position");
        let splice_end = splice_start + expected_r.len();
        assert_eq!(
            checkpoint_full_ids[splice_start..splice_end],
            expected_r,
            "the exact expected R is contiguous at the production splice position"
        );
        let mut expected_final_ids = checkpoint_full_ids;
        expected_final_ids.splice(splice_start..splice_end, [summary_id.to_owned()]);
        let after_full = after_fault_witness.full_path.as_array().unwrap();
        let actual_final_ids = after_full
            .iter()
            .map(|row| row[1].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            actual_final_ids, expected_final_ids,
            "the summary splice preserves the entire checkpoint prefix and suffix, including audit nodes"
        );
        let retained_ids = checkpoint_witness
            .full_path
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| !expected_r.iter().any(|id| row[1] == *id))
            .map(|row| row[1].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        for id in retained_ids {
            let before = checkpoint_witness
                .full_path
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row[1] == id)
                .expect("retained checkpoint row");
            let after = after_full
                .iter()
                .find(|row| row[1] == id)
                .expect("same retained final row");
            assert_eq!(
                (
                    &before[1], &before[2], &before[3], &before[4], &before[5], &before[6],
                    &before[7]
                ),
                (
                    &after[1], &after[2], &after[3], &after[4], &after[5], &after[6], &after[7]
                ),
                "retained non-R row {id} preserves owner, kind, payload revision/bytes, deletion and creation facts; only ordinal may shift"
            );
        }
        assert!(
            after_full
                .iter()
                .enumerate()
                .all(|(ordinal, row)| row[0].as_i64() == Some(ordinal as i64)),
            "final full-path ordinals are exact after the splice"
        );
        let summary_position = final_path
            .iter()
            .position(|row| row[1] == summary_id)
            .unwrap();
        let last_spilled_ordinal = checkpoint_path
            .iter()
            .filter(|spill| spill[3] == "spill")
            .map(|spill| spill[0].as_i64().unwrap())
            .max()
            .expect("committed spill has a selected checkpoint path position");
        let summary_ordinal = final_path
            .iter()
            .find(|row| row[1] == summary_id)
            .and_then(|row| row[0].as_i64())
            .unwrap();
        let summary_source_positions = source_rows
            .iter()
            .filter_map(|row| {
                checkpoint_sources
                    .iter()
                    .position(|id| *id == row[4].as_str().unwrap())
            })
            .collect::<Vec<_>>();
        assert!(
            summary_source_positions
                .windows(2)
                .all(|positions| positions[0] < positions[1])
        );
        let first_replaced_ordinal = source_rows
            .first()
            .and_then(|source| {
                checkpoint_path
                    .iter()
                    .find(|row| row[1] == source[4])
                    .and_then(|row| row[0].as_i64())
            })
            .expect("summary's first ordered source has its checkpoint path ordinal");
        assert_eq!(
            summary_ordinal, first_replaced_ordinal,
            "follow-on summary occupies the first source's exact splice position; prior spill ordinal {last_spilled_ordinal}, ordered source positions {summary_source_positions:?}"
        );
        assert!(
            summary_position < final_path.len(),
            "summary remains on the selected path"
        );
    } else {
        assert_eq!(
            after_fault_witness, checkpoint_witness,
            "fault return preserves the exact checkpoint-selected path, source bytes, provenance, groups, receipts, revision and epoch"
        );
    }
    let terminal_outcome =
        expected_outcome(fault, scenario, ObservationPhase::AfterShutdownAndRestart);
    let terminal_stage = expected_terminal_stage(scenario, fault);
    let final_record_expected = terminal_outcome != "unknown";
    if final_record_expected {
        let stage_check = (fault != Fault::RejectFact).then_some(terminal_stage);
        assert!(
            terminal_record_written(&control, &identity, terminal_outcome, stage_check).await,
            "FINISH targets the exact attempt/token when its terminal write is expected"
        );
    }
    let checkpoint_outcome = if fault == Fault::RejectFact {
        ObservationPhase::AfterRejectedStageTransaction
    } else {
        ObservationPhase::AtCommittedCheckpoint
    };
    assert_eq!(
        checkpoint_record["outcome"],
        expected_outcome(fault, scenario, checkpoint_outcome),
        "the captured observation preserves the fault's DB stage contract"
    );
    let query_thread = fixture.thread_id.to_owned();
    let query_attempt = fixture.attempt_id.to_owned();
    let state = control.store.database.call(move |db| {
        let summaries: i64 = db.query_row(
            "SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='summary'",
            [&query_thread], |row| row.get(0),
        )?;
        let spills: i64 = db.query_row(
            "SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='spill'",
            [&query_thread], |row| row.get(0),
        )?;
        let record: String = db.query_row(&format!("SELECT {SAVED_RECORD} FROM automatic_compaction c WHERE attempt_id=?1"), [&query_attempt], |row| row.get(0))?;
        let protected: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id='current-user')",
            [&query_thread], |row| row.get(0),
        )?;
        let latest_group_nodes: i64 = db.query_row(
            "SELECT count(*) FROM thread_path WHERE thread_id=?1 AND node_id IN ('g3-call','g3-result')",
            [&query_thread], |row| row.get(0),
        )?;
        Ok((summaries, spills, record, protected, latest_group_nodes))
    }).await.unwrap();
    let record: Value = serde_json::from_str(&state.2).unwrap();
    let summary_committed = fault != Fault::RejectFact
        && !(fault == Fault::RejectFinal && scenario == Scenario::ActiveSpill)
        && (scenario.summary()
            || scenario.continues_to_active_summary(fault) && fault == Fault::Remeasurement);
    if scenario == Scenario::ClosedSpill {
        assert_eq!(
            state.1 > 0,
            fault != Fault::RejectFact,
            "closed whole groups spill unless their stage transaction rejects"
        );
    }
    if scenario == Scenario::ClosedSummary {
        assert!(
            state.1 > 0,
            "closed spill precedes the selected summary stage"
        );
    }
    assert_eq!(
        state.0 > 0,
        summary_committed,
        "only a policy-reachable summary stage adopts"
    );
    assert!(state.3, "the protected current request remains selected");
    assert_eq!(
        state.4, 2,
        "the latest whole active tool group remains selected"
    );
    assert_eq!(record["unfinished"], terminal_outcome == "unknown");
    assert_eq!(record["outcome"], terminal_outcome);
    assert_eq!(record["trigger"]["input_tokens"], fixture.input_tokens);
    assert_eq!(record["trigger"]["a_input_tokens"], fixture.a_input_tokens);
    assert_eq!(
        record["committed_stages"][scenario.stage()].is_object(),
        fault != Fault::RejectFact,
        "precommit fact rejection rolls back its stage while other injected cells commit it",
    );
    if summary_committed {
        let final_summary_stage = if scenario == Scenario::ClosedSummary {
            "closed_summary"
        } else {
            "active_summary"
        };
        assert!(record["committed_stages"][final_summary_stage].is_object());
    } else if scenario == Scenario::ActiveSpill {
        assert!(record["committed_stages"]["active_summary"].is_null());
    }
    if fault == Fault::RetiredWritesAbsent {
        let retired_hits: i64 = control
            .store
            .database
            .call(|db| {
                Ok(
                    db.query_row("SELECT count(*) FROM retired_write_hits", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(
            retired_hits, 0,
            "retired attempt/guard/status writes stay absent"
        );
    }
    drop(boundary.into_guard());
    let storage_failure = matches!(&result, Err(error) if error.is_storage_failure());
    if storage_failure {
        control.fail_storage().await;
    }
    let shutdown = control.shutdown().await;
    if storage_failure {
        assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
    } else {
        shutdown.unwrap();
    }
    owner.shutdown().await.unwrap();

    let reopened_owner = proactive_owner(root.path(), &url, &limits).await;
    let reopened = Control::new(&reopened_owner, root.path().into());
    let reopened_record = automatic_attempt_record(&reopened, &fixture).await;
    assert_eq!(reopened_record["token"], identity.token);
    assert_eq!(reopened_record["outcome"], terminal_outcome);
    assert_eq!(
        reopened_record["unfinished"],
        terminal_outcome == "unknown",
        "unknown receipts remain provisional; terminal outcomes are finalized"
    );
    if fault == Fault::RejectFact {
        assert!(
            reopened_record["committed_stages"][scenario.stage()].is_null(),
            "rejected stage fact remains absent after reopen"
        );
    }
    let active_key = crate::compaction::CompactionKeys::new(fixture.thread_id).automatic_active();
    assert_eq!(
        reopened
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key=?1",
                    [active_key],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap(),
        0,
        "finished direct attempt removes its activity witness"
    );
    let reopened_witness = committed_selection_witness(&reopened, fixture.thread_id).await;
    let mut reopened_core = reopened_witness.clone();
    reopened_core.full_path = after_fault_witness.full_path.clone();
    reopened_core.audit_path = after_fault_witness.audit_path.clone();
    reopened_core.audit_projection = after_fault_witness.audit_projection.clone();
    reopened_core.audit_relations = after_fault_witness.audit_relations.clone();
    reopened_core.selection_revision = after_fault_witness.selection_revision;
    reopened_core.cursor = after_fault_witness.cursor.clone();
    assert_eq!(
        reopened_core, after_fault_witness,
        "normal reopen preserves the exact source path/provenance/group/receipt/cursor/epoch witness"
    );
    let before_audit = after_fault_witness.audit_path.as_array().unwrap();
    let after_audit = reopened_witness.audit_path.as_array().unwrap();
    assert!(
        after_audit.starts_with(before_audit),
        "normal reopen only appends audit-path records"
    );
    assert!(
        reopened_witness
            .audit_projection
            .as_array()
            .unwrap()
            .starts_with(after_fault_witness.audit_projection.as_array().unwrap()),
        "reopen preserves old audit projection witnesses before appending new ones"
    );
    let reopen_audit_appends = json!(&after_audit[before_audit.len()..]);
    let before_full = after_fault_witness.full_path.as_array().unwrap();
    let after_full = reopened_witness.full_path.as_array().unwrap();
    assert!(
        after_full.starts_with(before_full),
        "reopen preserves the exact old full-path prefix"
    );
    assert_eq!(
        &after_full[before_full.len()..],
        reopen_audit_appends.as_array().unwrap(),
        "every reopen path append is represented at its actual full-path position"
    );
    assert_eq!(
        reopened_witness.selection_revision,
        after_fault_witness.selection_revision
            + reopen_audit_appends.as_array().unwrap().len() as i64,
        "shared reopen witness accounts for every authorized audit append revision exactly"
    );
    assert!(
        reopen_audit_appends
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| {
                entry[3] == "event"
                    || entry[3] == "interruption_details"
                    || entry[3] == "interruption_context"
            }),
        "reopen appends contain only the explicitly separated audit node kinds"
    );
    if fault != Fault::RejectFact {
        assert_next_control_summary(&reopened, fixture.thread_id, &mut receive).await;
    }
    let terminal_record = automatic_attempt_record(&reopened, &fixture).await;
    reopened.shutdown().await.unwrap();
    reopened_owner.shutdown().await.unwrap();
    server.abort();
    DirectBoundaryReceipt {
        checkpoint: checkpoint_record,
        terminal: terminal_record,
        checkpoint_witness,
        after_fault_witness,
        reopened_witness,
        reopen_audit_appends,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateMutation {
    SourceRevision,
    Protection,
    ToolState,
    ExternalEpoch,
}
impl CandidateMutation {
    const ALL: [Self; 4] = [
        Self::SourceRevision,
        Self::Protection,
        Self::ToolState,
        Self::ExternalEpoch,
    ];
    fn expected_adoption(self) -> bool {
        self == Self::ExternalEpoch
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_active_candidate_mutations_preserve_r_and_source_provenance() {
    for mutation in CandidateMutation::ALL {
        direct_active_candidate_mutation(mutation).await;
    }
}

async fn direct_active_candidate_mutation(mutation: CandidateMutation) {
    use crate::control::compaction_test_fixture::{StagePlacement, stage_fixture};

    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, _heads, server) = proxy().await;
    let owner = proactive_owner(
        root.path(),
        &url,
        "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n",
    )
    .await;
    let control = Control::new(&owner, root.path().into());
    let cwd = root.path().to_string_lossy().into_owned();
    let agents = root.path().join("AGENTS.md").to_string_lossy().into_owned();
    control.store.database.call(move |db| {
        db.execute(
            "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,auto_compact_at,auto_compact_target_percent,spill_override,updated_at) VALUES('t',?1,?2,'fixture','fixture',200,20,0,'now')",
            rusqlite::params![cwd, agents],
        )?;
        Ok(())
    }).await.unwrap();
    let fixture = stage_fixture(
        &control.store.database,
        StagePlacement::ActiveTurn,
        20,
        false,
    )
    .await;
    let slot = DriverSlot {
        id: "direct-candidate-slot".into(),
        run_id: fixture.run_id.clone(),
        ..DriverSlot::default()
    };
    slot.turn
        .lock()
        .await
        .replace(crate::control::turn_interrupt::TurnBinding {
            turn: fixture.turn_id.into(),
            cancel: tokio_util::sync::CancellationToken::new(),
            start_gate: Arc::new(tokio::sync::Mutex::new(false)),
        });
    let mut snapshot = direct_snapshot_for_fixture(&control, &fixture).await;
    let plan = preflight(&control, &snapshot)
        .await
        .unwrap()
        .expect("typed active fixture is eligible");
    assert_eq!(plan.attempt_id, fixture.attempt_id);
    assert_eq!(plan.input_tokens, fixture.input_tokens);
    assert_eq!(plan.a_input_tokens, None, "spill-off has no A measurement");
    assert_eq!(plan.target_tokens, 40);
    assert!(!plan.spill_effective);

    let boundary_mutex = tokio::sync::Mutex::new(());
    let guard = boundary_mutex.lock().await;
    let mut boundary = crate::compaction::boundary::Boundary::new(&boundary_mutex, guard);
    let run_stop = slot.stop.clone();
    let (result, identity, before_mutation, mutated_candidate) = {
        let direct = maybe_compact_with_run_stop(
            &control,
            &mut snapshot,
            &slot,
            &mut boundary,
            run_stop,
            plan,
        );
        tokio::pin!(direct);
        let request = tokio::select! {
            result = &mut direct => panic!("direct active candidate finished before its existing summary request: {result:?}"),
            request = receive.recv() => request.expect("active candidate sends its tool-free summary"),
        };
        assert!(request.body["tools"].as_array().is_some_and(Vec::is_empty));
        let input = request.body["input"].to_string();
        assert!(input.contains("g1-call") && input.contains("g2-call"));
        assert!(
            !input.contains("g3-call"),
            "latest complete group stays outside active R"
        );
        let identity = capture_terminal_identity(&control, fixture.thread_id, true).await;
        assert_eq!(identity.attempt, fixture.attempt_id);
        let before_mutation = committed_selection_witness(&control, fixture.thread_id).await;

        let receipt_id = uuid::Uuid::new_v4().to_string();
        let mutation_for_db = mutation;
        control.store.database.call(move |db| {
            let tx = db.transaction()?;
            match mutation_for_db {
                CandidateMutation::SourceRevision => {
                    tx.execute("UPDATE node SET payload_revision=payload_revision+1 WHERE id='g1-call'", [])?;
                }
                CandidateMutation::Protection => {
                    tx.execute(
                        "INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at) VALUES(?1,?1,'{}','t','g1-call','accepted','direct-candidate-mutation')",
                        [&receipt_id],
                    )?;
                }
                CandidateMutation::ToolState => {
                    tx.execute("UPDATE tool_group SET state='running' WHERE id='g1'", [])?;
                }
                CandidateMutation::ExternalEpoch => {
                    tx.execute("UPDATE thread SET reasoning_epoch=reasoning_epoch+1 WHERE id='t'", [])?;
                }
            }
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let mutated_candidate = committed_selection_witness(&control, fixture.thread_id).await;
        request
            .reply
            .send((200, answer("short direct candidate summary")))
            .unwrap();
        let result = direct.await;
        (result, identity, before_mutation, mutated_candidate)
    };
    drop(boundary.into_guard());
    let expected_adoption = mutation.expected_adoption();
    let expected_outcome = if expected_adoption {
        "adopted"
    } else {
        "failed"
    };
    assert!(
        terminal_record_written(
            &control,
            &identity,
            expected_outcome,
            expected_adoption.then_some("active_summary")
        )
        .await,
        "exact attempt/token terminal FINISH for {mutation:?}"
    );
    let record = automatic_attempt_record(&control, &fixture).await;
    assert_eq!(identity.attempt, fixture.attempt_id);
    assert_eq!(record["token"], identity.token);
    assert_eq!(record["outcome"], expected_outcome, "{record:#?}");
    assert_eq!(
        result.unwrap(),
        ProactiveOutcome::Finished {
            path_changed: expected_adoption
        }
    );

    let after = committed_selection_witness(&control, fixture.thread_id).await;
    if expected_adoption {
        let summary_id = record["committed_stages"]["active_summary"]["summary_node_id"]
            .as_str()
            .unwrap();
        let rows = after
            .replacement_provenance_and_source_bytes
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row[1] == summary_id && row[2] == "summary")
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len() as u64,
            record["committed_stages"]["active_summary"]["source_nodes"]
                .as_u64()
                .unwrap()
        );
        for (ordinal, source) in rows.iter().enumerate() {
            assert_eq!(source[3].as_u64(), Some(ordinal as u64));
            let prior = before_mutation
                .selected_path_source_bytes
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row[1] == source[4]);
            assert!(
                prior.is_some_and(|row| row[5] == source[7] && row[4] == source[5]),
                "epoch-only mutation preserves exact ordered source bytes/revision for {}",
                source[4]
            );
        }
        assert_eq!(
            after.reasoning_epoch,
            mutated_candidate.reasoning_epoch + 1,
            "DropAll adoption advances the post-mutation projected epoch exactly once"
        );
    } else {
        assert!(
            record["committed_stages"]["active_summary"].is_null(),
            "stale candidate is not committed"
        );
        assert_eq!(
            after.selected_path_source_bytes, mutated_candidate.selected_path_source_bytes,
            "rejected candidate leaves the explicit source/protection/tool mutation as the only path effect"
        );
        assert_eq!(
            after.replacement_provenance_and_source_bytes,
            mutated_candidate.replacement_provenance_and_source_bytes,
            "no summary replacement/provenance is added for stale R"
        );
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

/// #303 §6.3: growth changes the active range identity, not the live Turn.
#[tokio::test]
async fn proactive_growing_active_range_permits_one_summary_per_new_report() {
    use crate::control::compaction_test_fixture::{
        StagePlacement, append_completed_tool_group, stage_fixture,
    };

    crate::test_watchdog::watchdog("active range growth at the owned boundary", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, _heads, server) = proxy().await;
        let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200\n").await;
        let control = Control::new(&owner, root.path().into());
        let cwd = root.path().to_string_lossy().into_owned();
        let agents = root.path().join("AGENTS.md").to_string_lossy().into_owned();
        control.store.database.call(move |db| {
            db.execute(
                "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,auto_compact_at,auto_compact_target_percent,spill_override,updated_at) VALUES('t',?1,?2,'fixture','fixture',200,20,0,'now')",
                rusqlite::params![cwd, agents],
            )?;
            Ok(())
        }).await.unwrap();
        let mut fixture = stage_fixture(
            &control.store.database,
            StagePlacement::ActiveTurn,
            20,
            false,
        ).await;
        let slot = DriverSlot {
            id: "direct-growth-slot".into(),
            run_id: fixture.run_id.clone(),
            ..DriverSlot::default()
        };
        slot.turn.lock().await.replace(crate::control::turn_interrupt::TurnBinding {
            turn: fixture.turn_id.into(),
            cancel: tokio_util::sync::CancellationToken::new(),
            start_gate: Arc::new(tokio::sync::Mutex::new(false)),
        });
        let lock = control.boundary(fixture.thread_id).await;
        let mut tokens_and_failures = Vec::new();
        for (attempt, usage, selected, excluded) in [
            ("trigger", 200, &["g1", "g2"][..], &["g3", "g4"][..]),
            ("growth-trigger", 201, &["g1", "g2", "g3"][..], &["g4"][..]),
        ] {
            if attempt != fixture.attempt_id {
                let thread = fixture.thread_id;
                let turn = fixture.turn_id;
                control.store.database.call(move |db| {
                    let tx = db.transaction()?;
                    let token = crate::compaction::a_state::begin_report_tx(
                        &tx, thread, "fixture", "fixture",
                    )?;
                    let manifest = json!({"adoption":{"outcome":"adopted"},"compaction_report_token":token}).to_string();
                    let report_usage = json!({"input_tokens":usage}).to_string();
                    tx.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at) SELECT 'conversation',(SELECT thread_id FROM turn WHERE id=?2), ?1,?2,0,revision,profile_id,model_id,reasoning_epoch,'completed',?3,?4,'growth','growth' FROM thread WHERE id=?5", rusqlite::params![attempt, turn, manifest, report_usage, thread])?;
                    append_completed_tool_group(&tx, thread, attempt, turn, "g4", 4_000, "out-g4")?;
                    tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id) VALUES(?1,0,'function_call','completed',?2,'g4-call')", rusqlite::params![attempt, json!({"type":"function_call","call_id":"g4","name":"shell","arguments":"{}"}).to_string()])?;
                    crate::compaction::a_state::complete_report_tx(
                        &tx, thread, "fixture", "fixture", attempt, Some(token), Some(usage),
                    )?;
                    tx.commit()?;
                    Ok(())
                }).await.unwrap();
                fixture.attempt_id = attempt;
                fixture.input_tokens = usage;
            }
            let mut snapshot = direct_snapshot_for_fixture(&control, &fixture).await;
            assert_eq!(snapshot.run, fixture.run_id);
            assert_eq!(snapshot.turn, fixture.turn_id);
            let plan = preflight(&control, &snapshot).await.unwrap().expect("eligible report");
            assert_eq!(plan.attempt_id, attempt);
            assert_eq!(plan.input_tokens, usage);
            assert!(!plan.spill_effective);
            let mut boundary = crate::compaction::boundary::Boundary::new(&lock, lock.lock().await);
            let identity = {
                let direct = maybe_compact_with_run_stop(
                    &control, &mut snapshot, &slot, &mut boundary, slot.stop.clone(), plan,
                );
                tokio::pin!(direct);
                let post = tokio::select! {
                    result = &mut direct => panic!("finished before expected Summary: {result:?}"),
                    post = receive.recv() => post.expect("expected exactly one Summary for this report"),
                };
                assert_eq!(post.body["tools"], json!([]));
                let input = post.body["input"].to_string();
                for group in selected {
                    for node in ["call", "result"] {
                        assert!(input.contains(&format!("{group}-{node}")), "missing {group}-{node}: {input}");
                    }
                }
                for group in excluded {
                    for node in ["call", "result"] {
                        assert!(!input.contains(&format!("{group}-{node}")), "protected {group}-{node}: {input}");
                    }
                }
                assert!(!input.contains("long task"), "current request stays outside R");
                let identity = capture_terminal_identity(&control, fixture.thread_id, true).await;
                assert_eq!(identity.attempt, attempt);
                let record = automatic_attempt_record(&control, &fixture).await;
                assert_eq!(record["token"], identity.token);
                assert_eq!(record["unfinished"], true);
                assert!(record["committed_stages"]["active_summary"].is_null());
                assert_eq!(record["trigger"]["run_id"], fixture.run_id);
                assert_eq!(record["trigger"]["input_tokens"], usage);
                post.reply.send((200, answer(" "))).unwrap();
                assert_eq!(
                    direct.await.unwrap(),
                    ProactiveOutcome::Finished { path_changed: false },
                );
                identity
            };
            drop(boundary.into_guard());
            assert!(terminal_record_written(&control, &identity, "failed", None).await);
            let record = automatic_attempt_record(&control, &fixture).await;
            let outcome = &record["compaction_outcome"]["outcome"];
            assert_eq!(outcome["reason"], "empty");
            assert_eq!(outcome["origin"], "output_validation");
            assert_eq!(record["error"]["details"]["summary_usage"]["requests"], 1);
            assert_eq!(record["committed_stages"], json!({}));
            assert_eq!(record["compaction_outcome"]["phase"], "active_summary");
            let thread = fixture.thread_id;
            let failure: String = control.store.database.call(move |db| {
                let closed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                    [format!("compact.deterministic_range_failure.closed.{thread}")], |r| r.get(0))?;
                assert!(!closed);
                Ok(db.query_row("SELECT value FROM app_meta WHERE key=?1",
                    [format!("compact.deterministic_range_failure.active.{thread}")], |r| r.get(0))?)
            }).await.unwrap();
            tokens_and_failures.push((identity.token, failure));
            let unchanged = direct_snapshot_for_fixture(&control, &fixture).await;
            assert!(preflight(&control, &unchanged).await.unwrap().is_none());
            assert!(matches!(receive.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        }
        assert_ne!(tokens_and_failures[0].0, tokens_and_failures[1].0);
        assert_ne!(tokens_and_failures[0].1, tokens_and_failures[1].1);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }).await.unwrap();
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
            if fault == Fault::Cancellation && scenario == Scenario::ActiveSummary {
                continue; // The named cancellation test owns this identical contract.
            }
            direct_boundary_case(fault, scenario, Some(scenario.target_percent())).await;
        }
    })
    .await
    .unwrap();
}

async fn prepare_driver_scenario(
    fault: Fault,
    scenario: Scenario,
    set_thread_percent: bool,
    spill_off: bool,
) -> DriverLifecycleFixture {
    prepare_driver_lifecycle(fault, scenario, set_thread_percent, spill_off, false).await
}

async fn prepare_held_summary(scenario: Scenario) -> DriverLifecycleFixture {
    prepare_driver_lifecycle(Fault::Cancellation, scenario, true, true, true).await
}

async fn prepare_driver_lifecycle(
    fault: Fault,
    scenario: Scenario,
    set_thread_percent: bool,
    spill_off: bool,
    hold_summary: bool,
) -> DriverLifecycleFixture {
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
    if hold_summary {
        assert_eq!(thread_get_summarizing(&control, &target).await, false);
    }
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
    let mut driver_pump = DriverToolPump::default();
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
    let mut held_normal = None;
    let mut terminal_identity = None;
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
                HeldFixtureEvent::OriginalFinished => panic!(
                    "original automatic attempt FINISH arrived before both held provider requests were captured"
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
                terminal_identity = Some(capture_terminal_identity(&control, &target, true).await);
                if hold_summary {
                    assert!(
                        held_summary.replace(request).is_none(),
                        "one summary request"
                    );
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
            } else if scenario.active() {
                driver_pump.continue_turn(request);
            } else {
                if hold_summary {
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
                                &format!("SELECT a.id,a.state,{SAVED_RECORD} FROM model_attempt a JOIN automatic_compaction c ON c.attempt_id=a.id JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 ORDER BY a.rowid DESC LIMIT 4"),
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
                    assert!(
                        held_normal.replace(request).is_none(),
                        "one normal continuation"
                    );
                } else {
                    request
                        .reply
                        .send((200, answer_with_usage("normal continuation", 100)))
                        .unwrap();
                }
            }
            // The provider may deliver either body first. The fixture must not
            // answer either POST before handing both response senders to the test.
            if held_summary.is_some() && held_normal.is_some() {
                break;
            }
        }
    }
    if spill_off && !hold_summary {
        let identity = terminal_identity
            .as_ref()
            .expect("summary terminal identity captured");
        crate::test_watchdog::watchdog("spill-off summary attempt finishes", async {
            loop {
                if let Ok(request) = receive.try_recv() {
                    assert_ne!(request.body["tools"], json!([]), "one summary POST only");
                    if scenario.active() {
                        driver_pump.continue_turn(request);
                    } else {
                        request
                            .reply
                            .send((200, answer_with_usage("spill-off continuation", 100)))
                            .unwrap();
                    }
                    continue;
                }
                if terminal_record_written(&control, identity, "adopted", Some(scenario.stage()))
                    .await
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    // RejectFact has no checkpoint: the trigger fires inside the stage's
    // adoption transaction, before the driver's terminal writer runs. The
    // rejected-stage observation point is after that terminal write.
    let rejected_outcome = expected_outcome(
        fault,
        scenario,
        ObservationPhase::AfterRejectedStageTransaction,
    );
    while !spill_off
        && !hold_summary
        && !if fault == Fault::RejectFact {
            if !reached.load(Ordering::SeqCst) {
                false
            } else {
                if terminal_identity.is_none() {
                    terminal_identity =
                        Some(capture_terminal_identity(&control, &target, false).await);
                }
                terminal_record_written(
                    &control,
                    terminal_identity
                        .as_ref()
                        .expect("rejected attempt identity captured"),
                    rejected_outcome,
                    None,
                )
                .await
            }
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
            if scenario.active() {
                driver_pump.continue_turn(normal);
            } else {
                normal
                    .reply
                    .send((200, answer_with_usage("background continuation", 100)))
                    .unwrap();
            }
        }
        tokio::task::yield_now().await;
    }
    let held_phase = if hold_summary {
        Some(HeldSummaryPhase {
            summary: held_summary.expect("held-summary phase captured its summary request"),
            normal: held_normal.expect("held-summary phase captured its ordinary continuation"),
        })
    } else {
        None
    };
    if hold_summary {
        return DriverLifecycleFixture {
            root,
            url,
            limits,
            owner,
            control,
            target,
            receive,
            server,
            held_phase,
            terminal_identity: terminal_identity
                .expect("capture original attempt before held summary return"),
        };
    }
    if terminal_identity.is_none() {
        terminal_identity = Some(capture_terminal_identity(&control, &target, false).await);
    }
    let terminal_identity = terminal_identity.expect("fixture captured original attempt/token");
    let saved = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row(
        &format!("SELECT {SAVED_RECORD} FROM automatic_compaction c ORDER BY c.rowid DESC LIMIT 1"),
        [], |row| row.get::<_, String>(0))?)
        })
        .await
        .unwrap();
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
    DriverLifecycleFixture {
        root,
        url,
        limits,
        owner,
        control,
        target,
        receive,
        server,
        held_phase,
        terminal_identity,
    }
}

#[derive(Clone, Debug)]
struct TerminalAttemptIdentity {
    thread: String,
    attempt: String,
    token: String,
}

struct HeldSummaryPhase {
    summary: Incoming,
    normal: Incoming,
}

/// Minimal normal-driver provider pump retained by lifecycle integration tests.
#[derive(Default)]
struct DriverToolPump {
    next_call: u64,
}
impl DriverToolPump {
    fn continue_turn(&mut self, request: Incoming) {
        let call_id = format!("driver-pump-{}", self.next_call);
        let response_id = format!("driver-pump-response-{}", self.next_call);
        self.next_call = self.next_call.saturating_add(1);
        request.reply.send((200, json!({
            "id":response_id,
            "status":"completed",
            "usage":{"input_tokens":100,"output_tokens":1},
            "output":[{"type":"function_call","call_id":call_id,"name":"shell","arguments":json!({"command":"echo integration"}).to_string()}]
        }))).unwrap();
    }
}

struct DriverLifecycleFixture {
    root: tempfile::TempDir,
    url: String,
    limits: String,
    owner: DataOwner,
    control: Arc<Control>,
    target: String,
    receive: mpsc::Receiver<Incoming>,
    server: tokio::task::JoinHandle<()>,
    held_phase: Option<HeldSummaryPhase>,
    terminal_identity: TerminalAttemptIdentity,
}

/// Capture the fixture's original attempt/token before another Run can add an observation.
async fn capture_terminal_identity(
    control: &Control,
    thread: &str,
    require_provisional: bool,
) -> TerminalAttemptIdentity {
    let thread_id = thread.to_owned();
    let (attempt, token) = control.store.database.call(move |db| {
        Ok(db.query_row(
            "SELECT a.id,c.token FROM model_attempt a JOIN automatic_compaction c ON c.attempt_id=a.id JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 AND c.token IS NOT NULL AND (?2=0 OR (c.outcome IS NULL)=1) ORDER BY a.rowid DESC LIMIT 1",
            rusqlite::params![thread_id, require_provisional],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?)
    }).await.unwrap();
    TerminalAttemptIdentity {
        thread: thread.to_owned(),
        attempt,
        token,
    }
}

struct TerminalRecord {
    provisional: Option<i64>,
    token: Option<String>,
    outcome: Option<String>,
    committed_stage: Option<i64>,
}

/// Observe only terminal FINISH on the exact captured receipt; callers assert its meaning separately.
async fn terminal_attempt_finished(control: &Control, identity: &TerminalAttemptIdentity) -> bool {
    let identity = identity.clone();
    control.store.database.call(move |db| {
        let row: Option<(Option<i64>, Option<String>)> = db.query_row(
            "SELECT (c.outcome IS NULL),c.token FROM model_attempt a JOIN automatic_compaction c ON c.attempt_id=a.id JOIN turn u ON u.id=a.turn_id WHERE a.id=?1 AND u.thread_id=?2",
            rusqlite::params![identity.attempt, identity.thread],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        Ok(row.is_some_and(|(provisional, token)| {
            provisional == Some(0) && token.as_deref() == Some(identity.token.as_str())
        }))
    }).await.unwrap()
}

/// Require FINISH on the captured original receipt, never a sibling Run observation.
async fn terminal_record_written(
    control: &Control,
    identity: &TerminalAttemptIdentity,
    expected_outcome: &str,
    committed_stage: Option<&str>,
) -> bool {
    let identity = identity.clone();
    let expected_outcome = expected_outcome.to_owned();
    let committed_stage = committed_stage.map(str::to_owned);
    control.store.database.call(move |db| {
        let row: Option<TerminalRecord> = db.query_row(
            "SELECT (c.outcome IS NULL),c.token,c.outcome,CASE WHEN ?4 IS NULL THEN 1 ELSE CASE ?4 WHEN 'closed_spill' THEN c.closed_spill WHEN 'active_spill' THEN c.active_spill WHEN 'closed_summary' THEN c.closed_summary WHEN 'active_summary' THEN c.active_summary END IS NOT NULL END FROM model_attempt a JOIN automatic_compaction c ON c.attempt_id=a.id JOIN turn u ON u.id=a.turn_id WHERE a.id=?1 AND u.thread_id=?2 AND c.token=?3",
            rusqlite::params![identity.attempt,identity.thread,identity.token,committed_stage],
            |row| Ok(TerminalRecord {
                provisional: row.get(0)?,
                token: row.get(1)?,
                outcome: row.get(2)?,
                committed_stage: row.get(3)?,
            }),
        ).optional()?;
        Ok(row.is_some_and(|record| {
            record.provisional == Some(0)
                && record.token.as_deref() == Some(identity.token.as_str())
                && record.outcome.as_deref() == Some(expected_outcome.as_str())
                && record.committed_stage == Some(1)
        }))
    }).await.unwrap()
}

fn expected_terminal_stage(scenario: Scenario, fault: Fault) -> &'static str {
    if scenario.continues_to_active_summary(fault) {
        "active_summary"
    } else {
        scenario.stage()
    }
}

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
            db.execute_batch("CREATE TEMP TRIGGER persistent_attempt BEFORE UPDATE ON automatic_compaction
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
                db.execute_batch("CREATE TEMP TRIGGER retired_exception_write BEFORE UPDATE OF outcome ON automatic_compaction
                    WHEN NEW.outcome='exception'
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
                db.execute_batch("CREATE TEMP TRIGGER reject_final_attempt BEFORE UPDATE OF outcome ON automatic_compaction
                    WHEN NEW.outcome IS NOT NULL
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

async fn thread_get_summarizing(control: &Arc<Control>, thread: &str) -> Value {
    let details = request(
        control,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.into()),
        }),
    )
    .await;
    details["summarizing"].clone()
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
            ("turns", "SELECT json_object('id',substr(t.id,1,128),'thread_id',substr(t.thread_id,1,128),'run_id',substr(t.run_id,1,128),'state',substr(t.state,1,64),'generation',t.generation,'instruction_snapshot_present',t.instruction_snapshot_id IS NOT NULL,'run_state',substr(r.state,1,64),'recovery_code',substr(json_extract(t.recovery_reason_json,'$.code'),1,128),'attempt_state',substr(a.state,1,64),'error_kind',substr(json_extract(a.error_json,'$.error.kind'),1,128),'http_phase',substr(json_extract(a.input_manifest_json,'$.http_observation.phase'),1,128)) FROM turn t JOIN run r ON r.id=t.run_id LEFT JOIN model_attempt a ON a.id=(SELECT latest.id FROM model_attempt latest WHERE latest.turn_id=t.id ORDER BY latest.rowid DESC LIMIT 1) ORDER BY t.rowid DESC LIMIT 8"),
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
    OriginalFinished,
}

async fn next_held_fixture_event(
    control: &Control,
    thread: &str,
    receive: &mut mpsc::Receiver<Incoming>,
    heads: &mut mpsc::UnboundedReceiver<ProxyRequestEvent>,
    slot: &DriverSlot,
) -> HeldFixtureEvent {
    next_held_fixture_event_with_completion(control, thread, receive, heads, slot, None).await
}

async fn next_held_fixture_event_for_attempt(
    control: &Control,
    thread: &str,
    receive: &mut mpsc::Receiver<Incoming>,
    heads: &mut mpsc::UnboundedReceiver<ProxyRequestEvent>,
    slot: &DriverSlot,
    identity: &TerminalAttemptIdentity,
) -> HeldFixtureEvent {
    next_held_fixture_event_with_completion(control, thread, receive, heads, slot, Some(identity))
        .await
}

async fn next_held_fixture_event_with_completion(
    control: &Control,
    thread: &str,
    receive: &mut mpsc::Receiver<Incoming>,
    heads: &mut mpsc::UnboundedReceiver<ProxyRequestEvent>,
    slot: &DriverSlot,
    identity: Option<&TerminalAttemptIdentity>,
) -> HeldFixtureEvent {
    // Subscribe before reading FINISH so a terminal write cannot race past the waiter.
    let mut events = control.subscribe();
    loop {
        if let Some(identity) = identity
            && terminal_attempt_finished(control, identity).await
        {
            return HeldFixtureEvent::OriginalFinished;
        }
        let notified = slot.nudge.notified();
        tokio::pin!(notified);
        let finished = {
            let job = slot.proactive_job.lock().await;
            job.as_ref()
                .is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished)
        };
        if finished {
            if let Some(identity) = identity {
                if terminal_attempt_finished(control, identity).await {
                    return HeldFixtureEvent::OriginalFinished;
                }
            } else {
                return HeldFixtureEvent::JobFinished(
                    admission_boundary_facts(control, thread).await,
                );
            }
        }
        tokio::select! {
            biased;
            event = heads.recv() => return HeldFixtureEvent::Header(event.expect("mock head channel remains open")),
            request = receive.recv() => return HeldFixtureEvent::Request(request.expect("mock body channel remains open")),
            event = events.recv(), if identity.is_some() => match event {
                Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => {
                    if let Some(identity) = identity
                        && terminal_attempt_finished(control, identity).await
                    {
                        return HeldFixtureEvent::OriginalFinished;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!("engine event stream closed while awaiting original automatic-attempt FINISH"),
                _ => {},
            },
            _ = &mut notified => {
                let finished = {
                    let job = slot.proactive_job.lock().await;
                    job.as_ref().is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished)
                };
                if finished {
                    if let Some(identity) = identity {
                        if terminal_attempt_finished(control, identity).await {
                            return HeldFixtureEvent::OriginalFinished;
                        }
                    } else {
                        return HeldFixtureEvent::JobFinished(admission_boundary_facts(control, thread).await);
                    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_trigger_outside_context_changes_do_not_veto_current_r_adoption() {
    for change in [
        "profile_changed",
        "retired_run_annotation_changed",
        "turn_changed",
    ] {
        completed_trigger_outside_context_change(change).await;
    }
}

async fn completed_trigger_outside_context_change(change: &'static str) {
    crate::test_watchdog::watchdog("completed trigger outside-R positive", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, mut heads, server) = proxy().await;
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
        let slot = control
            .live
            .test_drivers()
            .lock()
            .await
            .get(&target)
            .cloned()
            .expect("trigger Run owns the retained proactive job slot");
        let slot_id = slot.id.clone();
        let mut held_summary = None;
        let mut captured_origin: Option<(TerminalAttemptIdentity, String, String, i64)> = None;
        let mut ordinary_completed = false;
        while held_summary.is_none() || !ordinary_completed {
            match next_held_fixture_event(&control, &target, &mut receive, &mut heads, &slot).await {
                HeldFixtureEvent::Request(request) if request.body["tools"] == json!([]) => {
                    assert!(held_summary.is_none(), "one automatic summary request for this trigger");
                    let identity = capture_terminal_identity(&control, &target, true).await;
                    let attempt = identity.attempt.clone();
                    let token = identity.token.clone();
                    let thread = identity.thread.clone();
                    let (run_id, turn_id, generation): (String, String, i64) = control.store.database.call(move |db| {
                        db.query_row(
                            "SELECT u.run_id,u.id,u.generation FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE a.id=?1 AND u.thread_id=?2 AND (SELECT token FROM automatic_compaction WHERE attempt_id=a.id)=?3",
                            rusqlite::params![attempt,thread,token],
                            |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
                        ).map_err(Into::into)
                    }).await.unwrap();
                    captured_origin = Some((identity,run_id,turn_id,generation));
                    held_summary = Some(request);
                }
                HeldFixtureEvent::Request(request) => {
                    assert!(!ordinary_completed, "one ordinary trigger request is serviced");
                    request
                        .reply
                        .send((200, answer_with_usage("completed ordinary trigger request", 100)))
                        .unwrap();
                    settled(&control, &target).await;
                    let _retired = wait_for_driver_retired(&control, &target, &slot_id).await;
                    ordinary_completed = true;
                }
                HeldFixtureEvent::Header(_) => {}
                HeldFixtureEvent::JobFinished(facts) => panic!(
                    "proactive job finished before its summary was captured: {facts:?}"
                ),
                HeldFixtureEvent::OriginalFinished => panic!(
                    "original automatic attempt FINISH arrived before its summary was captured"
                ),
            }
        }
        let summary = held_summary.expect("summary request held until ordinary Run retires");
        assert_eq!(summary.body["tools"], json!([]));
        let scope = target.clone();
        let (terminal_identity, run_id, turn_id, generation) = captured_origin
            .expect("capture original Thread/attempt/token/Run/Turn at the summary boundary");
        let attempt = terminal_identity.attempt.clone();
        let token = terminal_identity.token.clone();
        let query_attempt = attempt.clone();
        let query_token = token.clone();
        let query_thread = scope.clone();
        let run_id_for_mutation = run_id.clone();
        let turn_id_for_mutation = turn_id.clone();
        let generation_for_mutation = generation;
        control.store.database.call(move |db| {
            let (actual_run, actual_turn, actual_generation, run_state, turn_state): (
                String,
                String,
                i64,
                String,
                String,
            ) = db.query_row(
                "SELECT u.run_id,u.id,u.generation,r.state,u.state FROM model_attempt a JOIN turn u ON u.id=a.turn_id JOIN run r ON r.id=u.run_id WHERE a.id=?1 AND u.thread_id=?2 AND (SELECT token FROM automatic_compaction WHERE attempt_id=a.id)=?3",
                rusqlite::params![query_attempt, query_thread, query_token],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )?;
            assert_eq!(actual_run, run_id_for_mutation, "captured original Run ID");
            assert_eq!(actual_turn, turn_id_for_mutation, "captured original Turn ID");
            assert_eq!(actual_generation, generation_for_mutation, "captured original generation before mutation");
            assert_eq!(run_state, "completed", "exact original Run retired before outside-R mutation");
            assert_eq!(turn_state, "completed", "exact original Turn retired before outside-R mutation");
                match change {
                    "profile_changed" => {
                        let before:i64=db.query_row("SELECT profile_revision FROM thread WHERE id=?1",[&scope],|row|row.get(0))?;
                        let changed=db.execute("UPDATE thread SET profile_revision=profile_revision+1 WHERE id=?1",[&scope])?;
                        assert_eq!(changed,1,"captured target Thread profile mutation is nonvacuous");
                        let after:i64=db.query_row("SELECT profile_revision FROM thread WHERE id=?1",[&scope],|row|row.get(0))?;
                        assert_eq!(after,before+1);
                    }
                    "retired_run_annotation_changed" => {
                        // The trigger Run is already completed. Change only its outside-R stop annotation;
                        // never fabricate a completed->stopping state transition or invoke cancellation.
                        let before: Option<String> = db.query_row(
                            "SELECT stop_reason FROM run WHERE id=?1 AND thread_id=?2 AND state='completed'",
                            rusqlite::params![run_id, scope],
                            |row| row.get(0),
                        )?;
                        assert_ne!(before.as_deref(), Some("retired_trigger_context_change"));
                        let changed = db.execute(
                            "UPDATE run SET stop_reason='retired_trigger_context_change' WHERE id=?1 AND thread_id=?2 AND state='completed' AND stop_reason IS ?3",
                            rusqlite::params![run_id, scope, before],
                        )?;
                        assert_eq!(changed,1,"exact captured completed Run annotation mutation is nonvacuous");
                        let after:String=db.query_row("SELECT stop_reason FROM run WHERE id=?1 AND thread_id=?2 AND state='completed'",rusqlite::params![run_id,scope],|row|row.get(0))?;
                        assert_eq!(after,"retired_trigger_context_change");
                    }
                    "turn_changed" => {
                        let changed=db.execute("UPDATE turn SET generation=generation+1 WHERE id=?1 AND thread_id=?2 AND run_id=?3 AND state='completed' AND generation=?4",rusqlite::params![turn_id,scope,run_id,generation])?;
                        assert_eq!(changed,1,"exact captured completed trigger Turn generation mutation is nonvacuous");
                        let after:i64=db.query_row("SELECT generation FROM turn WHERE id=?1 AND thread_id=?2 AND run_id=?3",rusqlite::params![turn_id,scope,run_id],|row|row.get(0))?;
                        assert_eq!(after,generation+1);
                    }
                    _ => unreachable!(),
                }
            let (same_thread, same_run_state, same_turn_state): (String, String, String) = db.query_row(
                "SELECT r.thread_id,r.state,u.state FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id=?1 AND u.id=?2",
                rusqlite::params![run_id, turn_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(same_thread, scope, "mutation remains scoped to captured original Thread");
            assert_eq!(same_run_state, "completed", "context mutation never rewrites Run lifecycle state");
            assert_eq!(same_turn_state, "completed", "context mutation never rewrites Turn lifecycle state");
                Ok(())
            })
            .await
            .unwrap();
        summary
            .reply
            .send((200, answer("short synthetic summary")))
            .unwrap();
        crate::test_watchdog::watchdog("outside-R current adoption terminal", async {
            while !terminal_record_written(&control,&terminal_identity,"adopted",Some("closed_summary")).await {
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
                let (stage, outcome, provider_error): (String, String, Option<String>) = db.query_row(
                    "SELECT c.closed_summary,c.outcome,a.error_json FROM automatic_compaction c JOIN model_attempt a ON a.id=c.attempt_id WHERE c.attempt_id=?1",
                    [&attempt_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
                )?;
                assert_eq!(outcome,"adopted");
                assert!(provider_error.is_none());
                let stage: Value = serde_json::from_str(&stage).unwrap();
                let args = crate::protocol::CompactListArgs {thread_id:Some(target.clone()), since:None, until:None,limit:20};
                let exact = crate::readback::compactions(db,&target,&args)?;
                let exact = exact["items"].as_array().unwrap().iter().find(|item|item["attempt_id"]==attempt_id).expect("automatic row in compact.list");
                let display = crate::readback::last_compaction(db,&target)?;
                let candidate = &display["bounded_candidate"];
                for item in [exact,candidate] {
                    assert_eq!(item["attempt_id"],attempt_id);
                    assert_eq!(item["state"],"adopted");
                    assert_eq!(item["metrics"]["summary_node_id"],stage["summary_node_id"]);
                    assert_eq!(item["metrics"]["source_nodes"],stage["source_nodes"]);
                }
                let path = db
                    .prepare("SELECT p.node_id FROM thread_path p WHERE p.thread_id=?1 ORDER BY p.ordinal")?
                    .query_map([&target], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert!(path.iter().any(|id| {
                    db.query_row("SELECT kind='summary' FROM node WHERE id=?1", [id], |row| row.get::<_, bool>(0)).unwrap_or(false)
                }), "summary node adopted for {change}: {path:?}");
                assert!(!stage.to_string().contains("PRIVATE_SENTINEL"));
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
