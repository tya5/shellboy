//! Single writer-owned summary job slot. The storage worker invokes
//! `reconcile_before_db_reply` after each committed command and before reply.
use super::{gc_capture::CapturedSummaryInput, single_request::Dispatched};
use crate::protocol::ApiError;
use futures_util::FutureExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WriterJobId(u64);
impl WriterJobId {
    #[cfg(test)]
    pub(crate) fn test_fixture() -> Self {
        Self(0xfeed)
    }
    pub(crate) fn is_valid(self) -> bool {
        self.0 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriterJobPhase {
    Computing,
    Ready,
    Closing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionDisposition {
    Accepted,
    Closed,
    Stale,
}

#[derive(Debug)]
pub(crate) enum WriterCompletion {
    Ready(Dispatched),
    Failed(ApiError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriterLifecycle {
    LaunchPanicked,
    ProducerTerminated,
    ChildPanicked,
}

#[derive(Debug)]
enum ChildExit {
    Finished(Option<WriterCompletion>),
    Panicked,
}

#[derive(Clone, Debug)]
struct SourceWitness {
    ordinal: i64,
    id: String,
    turn: Option<String>,
    revision: i64,
}
#[derive(Clone, Debug)]
struct CaptureWitness {
    thread: String,
    profile_revision: i64,
    epoch: i64,
    nonappend_generation: i64,
    profile: String,
    model: String,
    effort: Option<String>,
    sources: Vec<SourceWitness>,
}
impl From<&CapturedSummaryInput> for CaptureWitness {
    fn from(input: &CapturedSummaryInput) -> Self {
        Self {
            thread: input.thread.clone(),
            profile_revision: input.profile_revision,
            epoch: input.epoch,
            nonappend_generation: input.nonappend_generation,
            profile: input.profile.clone(),
            model: input.model.clone(),
            effort: input.effort.clone(),
            sources: input
                .groups
                .iter()
                .flat_map(|group| group.stamps.iter())
                .map(|stamp| SourceWitness {
                    ordinal: stamp.ordinal,
                    id: stamp.id.clone(),
                    turn: stamp.turn.clone(),
                    revision: stamp.revision,
                })
                .collect(),
        }
    }
}

struct OwnedGcJob {
    id: WriterJobId,
    witness: CaptureWitness,
    spill_epoch_advanced: bool,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
    completion_signal: Option<tokio::sync::oneshot::Receiver<ChildExit>>,
    retirement_task: Option<JoinHandle<()>>,
    joined_receiver: Option<tokio::sync::oneshot::Receiver<JoinedFacts>>,
    joined: bool,
    runtime: tokio::runtime::Handle,
    retirement_wake: crate::storage::GcRetirementWake,
    phase: WriterJobPhase,
    terminal: Option<WriterCompletion>,
    lifecycle: Option<WriterLifecycle>,
    adoption_candidate: Option<Dispatched>,
    stage: super::attempt::GcStage,
}

#[derive(Debug)]
struct JoinedFacts {
    producer_panicked: bool,
    producer_joined: bool,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShutdownFacts {
    pub producer_joined: bool,
}

fn start_join_reaper(job: &mut OwnedGcJob) {
    if job.retirement_task.is_some() || job.joined || job.task.is_none() {
        return;
    }
    let producer = job.task.take().expect("producer handle checked");
    let (sender, receiver) = tokio::sync::oneshot::channel();
    job.joined_receiver = Some(receiver);
    let wake = job.retirement_wake.clone();
    let id = job.id;
    let reaper = job.runtime.spawn(async move {
        let producer_panicked = producer.await.is_err();
        JoinedFacts {
            producer_panicked,
            producer_joined: true,
        }
    });
    let retirement_task = job.runtime.spawn(async move {
        let facts = match reaper.await {
            Ok(facts) => facts,
            Err(_) => JoinedFacts {
                producer_panicked: true,
                producer_joined: false,
            },
        };
        let _ = sender.send(facts);
        let _ = wake.notify(id).await;
    });
    job.retirement_task = Some(retirement_task);
}

fn close_for_lifecycle(job: &mut OwnedGcJob) {
    job.phase = WriterJobPhase::Closing;
    job.cancel.cancel();
    job.adoption_candidate = None;
}

fn record_child_exit(job: &mut OwnedGcJob, exit: ChildExit) {
    match exit {
        ChildExit::Finished(Some(terminal)) => {
            job.lifecycle
                .get_or_insert(WriterLifecycle::ProducerTerminated);
            if job.terminal.is_none() {
                job.terminal = Some(terminal);
            }
        }
        ChildExit::Finished(None) => {
            job.lifecycle
                .get_or_insert(WriterLifecycle::ProducerTerminated);
        }
        ChildExit::Panicked => {
            job.lifecycle.get_or_insert(WriterLifecycle::ChildPanicked);
        }
    }
    // The producer is the only code allowed to drive an accepted candidate to
    // adoption. Its exit without strict adoption closes the candidate.
    close_for_lifecycle(job);
}

/// Not Clone, Send-only as required by the storage Job, and never shared behind
/// a mutex. The DB worker constructs and owns its one instance.
#[derive(Default)]
pub(crate) struct WriterGcCoordinator {
    active: Option<OwnedGcJob>,
}

impl WriterGcCoordinator {
    pub(crate) fn can_install_capture(&self) -> bool {
        self.active.is_none()
    }

    /// Called after the capture/provisional-record transaction has committed.
    pub(crate) fn install_after_commit<L, F>(
        &mut self,
        db: &Connection,
        captured: CapturedSummaryInput,
        stage: super::attempt::GcStage,
        runtime: tokio::runtime::Handle,
        retirement_wake: crate::storage::GcRetirementWake,
        launch: L,
    ) -> Result<WriterJobId, &'static str>
    where
        L: FnOnce(WriterJobId, CapturedSummaryInput, CancellationToken) -> F,
        F: Future<Output = Option<WriterCompletion>> + Send + 'static,
    {
        if !db.is_autocommit() {
            return Err("GC job installation requires a committed capture");
        }
        if self.active.is_some() {
            return Err("a writer-owned GC job is already active");
        }
        if stage.thread_id != captured.thread
            || stage.profile_id != captured.profile
            || stage.model_id != captured.model
            || stage.effort != captured.effort
            || (stage.reasoning_epoch != captured.epoch
                && !(captured.spill_epoch_advanced
                    && stage.reasoning_epoch.checked_add(1) == Some(captured.epoch)))
            || !super::attempt::provisional_gc_matches(db, &stage)
                .map_err(|_| "GC provisional receipt verification failed")?
        {
            return Err("GC capture requires the matching committed strict provisional receipt");
        }
        let witness = CaptureWitness::from(&captured);
        let spill_epoch_advanced = captured.spill_epoch_advanced;
        let cancel = CancellationToken::new();
        let id = WriterJobId(NEXT_JOB.fetch_add(1, Ordering::Relaxed));
        let (signal_tx, completion_signal) = tokio::sync::oneshot::channel();
        let launch = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            launch(id, captured, cancel.clone())
        }));
        let (task, phase, lifecycle) = match launch {
            Ok(future) => {
                let task = runtime.spawn(async move {
                    let exit = match std::panic::AssertUnwindSafe(future).catch_unwind().await {
                        Ok(terminal) => ChildExit::Finished(terminal),
                        Err(_) => ChildExit::Panicked,
                    };
                    let _ = signal_tx.send(exit);
                });
                (Some(task), WriterJobPhase::Computing, None)
            }
            Err(_) => {
                cancel.cancel();
                let _ = signal_tx.send(ChildExit::Panicked);
                (
                    None,
                    WriterJobPhase::Closing,
                    Some(WriterLifecycle::LaunchPanicked),
                )
            }
        };
        self.active = Some(OwnedGcJob {
            id,
            witness,
            spill_epoch_advanced,
            cancel,
            task,
            completion_signal: Some(completion_signal),
            retirement_task: None,
            joined_receiver: None,
            joined: false,
            runtime,
            retirement_wake,
            phase,
            terminal: None,
            lifecycle,
            adoption_candidate: None,
            stage,
        });
        Ok(id)
    }

    pub(crate) fn reconcile_joined_retirement(
        &mut self,
        db: &Connection,
        id: WriterJobId,
    ) -> Result<bool, rusqlite::Error> {
        if !self.active.as_ref().is_some_and(|job| job.id == id) {
            return Ok(false);
        }
        self.reconcile_before_db_reply(db)?;
        if self
            .active
            .as_ref()
            .is_some_and(|job| job.id == id && job.joined)
        {
            self.active = None;
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn active_job_id(&self) -> Option<WriterJobId> {
        self.active.as_ref().map(|job| job.id)
    }

    pub(crate) fn cancel_job(&mut self, id: WriterJobId) -> bool {
        let Some(job) = self.active.as_mut().filter(|job| job.id == id) else {
            return false;
        };
        job.phase = WriterJobPhase::Closing;
        job.adoption_candidate = None;
        job.cancel.cancel();
        start_join_reaper(job);
        true
    }
    pub(crate) fn phase(&self, id: WriterJobId) -> Option<WriterJobPhase> {
        self.active
            .as_ref()
            .filter(|job| job.id == id)
            .map(|job| job.phase)
    }

    pub(crate) fn complete_before_db_reply(
        &mut self,
        db: &Connection,
        id: WriterJobId,
        result: Result<Dispatched, ApiError>,
    ) -> Result<CompletionDisposition, rusqlite::Error> {
        let Some(job) = self.active.as_mut().filter(|job| job.id == id) else {
            return Ok(CompletionDisposition::Stale);
        };
        if job.terminal.is_some() {
            return Ok(CompletionDisposition::Closed);
        }
        match result {
            Ok(summary) => {
                job.terminal = Some(WriterCompletion::Ready(summary.clone()));
                if job.phase == WriterJobPhase::Computing && !job.cancel.is_cancelled() {
                    match witness_matches(db, &job.witness) {
                        Ok(true) => {
                            job.adoption_candidate = Some(summary);
                            job.phase = WriterJobPhase::Ready;
                        }
                        Ok(false) => {
                            job.cancel.cancel();
                            job.phase = WriterJobPhase::Closing;
                        }
                        Err(error) => {
                            job.cancel.cancel();
                            job.phase = WriterJobPhase::Closing;
                            return Err(error);
                        }
                    }
                }
            }
            Err(error) => {
                job.terminal = Some(WriterCompletion::Failed(error));
                job.cancel.cancel();
                job.phase = WriterJobPhase::Closing;
            }
        }
        Ok(if job.phase == WriterJobPhase::Ready {
            CompletionDisposition::Accepted
        } else {
            CompletionDisposition::Closed
        })
    }

    /// Run real SQL adoption, strict stage facts, qualified target CAS and
    /// original-receipt FINISH in one writer-owned transaction. FINISH is only
    /// reached after the caller's replacement/path writes have succeeded.
    pub(crate) fn adopt_ready_candidate<T, F>(
        &mut self,
        db: &mut Connection,
        id: WriterJobId,
        apply: F,
    ) -> Result<T, crate::storage::StorageError>
    where
        F: FnOnce(
            &rusqlite::Transaction<'_>,
            &Dispatched,
        ) -> Result<(T, String), crate::storage::StorageError>,
    {
        let Some(job) = self.active.as_mut().filter(|job| job.id == id) else {
            return Err(crate::storage::StorageError::InvalidInput(
                "GC job is stale".into(),
            ));
        };
        if job.phase != WriterJobPhase::Ready {
            return Err(crate::storage::StorageError::InvalidInput(
                "GC job has no adoption-ready candidate".into(),
            ));
        }
        let stage = &job.stage;
        if stage.thread_id != job.witness.thread
            || stage.profile_id != job.witness.profile
            || stage.model_id != job.witness.model
            || stage.effort != job.witness.effort
            || (stage.reasoning_epoch != job.witness.epoch
                && !(job.spill_epoch_advanced
                    && stage.reasoning_epoch.checked_add(1) == Some(job.witness.epoch)))
        {
            return Err(crate::storage::StorageError::InvalidInput(
                "GC stage does not match captured source authority".into(),
            ));
        }
        let current = witness_matches(db, &job.witness);
        match current {
            Ok(true) => {}
            Ok(false) => {
                job.adoption_candidate = None;
                job.phase = WriterJobPhase::Closing;
                job.cancel.cancel();
                return Err(crate::storage::StorageError::InvalidInput(
                    "GC sources changed before strict adoption".into(),
                ));
            }
            Err(error) => {
                job.adoption_candidate = None;
                job.phase = WriterJobPhase::Closing;
                job.cancel.cancel();
                return Err(error.into());
            }
        }
        let Some(candidate) = job.adoption_candidate.as_ref() else {
            return Err(crate::storage::StorageError::InvalidInput(
                "GC adoption candidate is absent".into(),
            ));
        };
        let outcome: Result<T, crate::storage::StorageError> =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let tx = db.transaction()?;
                let (value, fact) = apply(&tx, candidate)?;
                let fact_value: serde_json::Value =
                    serde_json::from_str(&fact).map_err(|error| {
                        crate::storage::StorageError::InvalidInput(format!(
                            "invalid GC stage fact: {error}"
                        ))
                    })?;
                super::attempt::record_gc_stage_tx(&tx, stage, &fact)?;
                super::attempt::finish_gc_tx(&tx, stage)?;
                if fact_value["drop_all_reasoning"] == true {
                    let captured_epoch = fact_value["captured_reasoning_epoch"]
                        .as_i64()
                        .ok_or_else(|| {
                            crate::storage::StorageError::InvalidInput(
                                "active GC FINISH lacks captured epoch witness".into(),
                            )
                        })?;
                    let current: i64 = tx.query_row(
                        "SELECT reasoning_epoch FROM thread WHERE id=?1",
                        [&stage.thread_id],
                        |row| row.get(0),
                    )?;
                    if captured_epoch.checked_add(1) != Some(current) {
                        return Err(crate::storage::StorageError::InvalidInput(
                            "active GC reasoning epoch changed during FINISH".into(),
                        ));
                    }
                    tx.execute(
                        "DELETE FROM app_meta WHERE key=?1",
                        [crate::history::reasoning_keep_key(&stage.thread_id)],
                    )?;
                }
                tx.commit()?;
                Ok(value)
            })) {
                Ok(result) => result,
                Err(_) => Err(crate::storage::StorageError::JobPanicked),
            };
        match outcome {
            Ok(value) => {
                job.adoption_candidate = None;
                job.phase = WriterJobPhase::Closing;
                job.cancel.cancel();
                Ok(value)
            }
            Err(error) => {
                job.adoption_candidate = None;
                job.phase = WriterJobPhase::Closing;
                job.cancel.cancel();
                Err(error)
            }
        }
    }

    pub(crate) fn validate_job(
        &mut self,
        db: &Connection,
        id: WriterJobId,
    ) -> Result<bool, rusqlite::Error> {
        let Some(job) = self.active.as_mut().filter(|job| job.id == id) else {
            return Ok(false);
        };
        if job.cancel.is_cancelled() {
            return Ok(false);
        }
        if job.phase != WriterJobPhase::Computing {
            return Ok(job.phase == WriterJobPhase::Ready);
        }
        match witness_matches(db, &job.witness) {
            Ok(true) => Ok(true),
            Ok(false) => {
                job.cancel.cancel();
                job.phase = WriterJobPhase::Closing;
                Ok(false)
            }
            Err(error) => {
                job.cancel.cancel();
                job.phase = WriterJobPhase::Closing;
                Err(error)
            }
        }
    }

    /// Poll completion and check exact source/profile fences while still in the
    /// writer command, before its DbReply sender runs. No raw payload is loaded.
    pub(crate) fn reconcile_before_db_reply(
        &mut self,
        db: &Connection,
    ) -> Result<bool, rusqlite::Error> {
        let retire = self.active.as_ref().is_some_and(|job| {
            job.phase == WriterJobPhase::Closing
                && ((job.joined
                    && job
                        .retirement_task
                        .as_ref()
                        .is_none_or(JoinHandle::is_finished))
                    || (job.lifecycle == Some(WriterLifecycle::LaunchPanicked)
                        && job.task.is_none()))
        });
        if retire {
            self.active = None;
        }
        let Some(job) = self.active.as_mut() else {
            return Ok(false);
        };
        if let Some(signal) = job.completion_signal.as_mut() {
            match signal.try_recv() {
                Ok(exit) => record_child_exit(job, exit),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    job.lifecycle
                        .get_or_insert(WriterLifecycle::ProducerTerminated);
                    if job.terminal.is_none() {
                        close_for_lifecycle(job);
                    }
                }
            }
        }
        if job.phase == WriterJobPhase::Closing {
            start_join_reaper(job);
            if let Some(receiver) = job.joined_receiver.as_mut() {
                if let Ok(facts) = receiver.try_recv() {
                    if facts.producer_panicked {
                        job.lifecycle.get_or_insert(WriterLifecycle::ChildPanicked);
                    }
                    if facts.producer_joined {
                        job.joined = true;
                    }
                    if facts.producer_panicked {
                        job.lifecycle.get_or_insert(WriterLifecycle::ChildPanicked);
                    }
                    job.joined_receiver = None;
                }
            }
            return Ok(true);
        }
        match witness_matches(db, &job.witness) {
            Ok(true) => Ok(false),
            Ok(false) => {
                job.phase = WriterJobPhase::Closing;
                job.cancel.cancel();
                job.adoption_candidate = None;
                Ok(true)
            }
            Err(error) => {
                job.phase = WriterJobPhase::Closing;
                job.cancel.cancel();
                job.adoption_candidate = None;
                Err(error)
            }
        }
    }

    pub(crate) async fn drain(&mut self, id: WriterJobId) -> bool {
        let Some(job) = self.active.as_mut().filter(|job| job.id == id) else {
            return false;
        };
        job.phase = WriterJobPhase::Closing;
        job.cancel.cancel();
        if let Some(task) = job.task.as_mut() {
            if task.await.is_err() {
                job.lifecycle.get_or_insert(WriterLifecycle::ChildPanicked);
            }
            job.task = None;
        }
        if let Some(signal) = job.completion_signal.as_mut() {
            if let Ok(exit) = signal.try_recv() {
                record_child_exit(job, exit);
            }
        }
        job.joined = true;
        true
    }
    pub(crate) fn retire_joined(&mut self, id: WriterJobId) -> bool {
        if self.active.as_ref().is_some_and(|job| {
            job.id == id
                && job.phase == WriterJobPhase::Closing
                && job.task.is_none()
                && job.joined
                && job
                    .retirement_task
                    .as_ref()
                    .is_none_or(JoinHandle::is_finished)
        }) {
            self.active = None;
            true
        } else {
            false
        }
    }
    #[cfg(test)]
    pub(crate) fn shutdown_facts(&self) -> Option<ShutdownFacts> {
        self.active.as_ref().map(|job| ShutdownFacts {
            producer_joined: job.joined,
        })
    }

    pub(crate) async fn shutdown(&mut self) {
        if let Some(job) = self.active.as_mut() {
            job.phase = WriterJobPhase::Closing;
            job.cancel.cancel();
            start_join_reaper(job);
            if let Some(retirement) = job.retirement_task.as_mut() {
                let _ = retirement.await;
            }
            if let Some(receiver) = job.joined_receiver.as_mut() {
                if let Ok(facts) = receiver.try_recv() {
                    if facts.producer_panicked {
                        job.lifecycle.get_or_insert(WriterLifecycle::ChildPanicked);
                    }
                    job.joined = facts.producer_joined;
                }
            }
            if let Some(signal) = job.completion_signal.as_mut() {
                if let Ok(exit) = signal.try_recv() {
                    record_child_exit(job, exit);
                }
            }
            job.retirement_task = None;
            if job.task.is_some() {
                if let Some(task) = job.task.as_mut() {
                    let _ = task.await;
                }
                job.task = None;
                job.joined = true;
            }
        }
    }
}

fn witness_matches(db: &Connection, witness: &CaptureWitness) -> Result<bool, rusqlite::Error> {
    let facts=db.query_row(
        "SELECT profile_revision,reasoning_epoch,selection_nonappend_generation,profile_id,model_id,effort FROM thread WHERE id=?1 AND deleted_at IS NULL AND archived_at IS NULL",
        [&witness.thread],
        |row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,Option<String>>(5)?)),
    ).optional()?;
    let Some(facts) = facts else { return Ok(false) };
    if facts.0 != witness.profile_revision
        || facts.1 != witness.epoch
        || facts.2 != witness.nonappend_generation
        || facts.3 != witness.profile
        || facts.4 != witness.model
        || facts.5 != witness.effort
    {
        return Ok(false);
    };
    let stopping: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state='stopping')",
        [&witness.thread],
        |row| row.get(0),
    )?;
    if stopping {
        return Ok(false);
    }
    for source in &witness.sources {
        let current:bool=db.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal=?2 AND p.node_id=?3 AND n.payload_revision=?4 AND n.turn_id IS ?5)",
            params![witness.thread,source.ordinal,source.id,source.revision,source.turn],
            |row|row.get(0),
        )?;
        if !current {
            return Ok(false);
        }
    }
    Ok(true)
}
