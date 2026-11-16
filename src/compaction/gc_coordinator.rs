//! Single writer-owned summary job slot. Completion events own readiness and
//! lifecycle cleanup; selected-R source facts are checked only at adoption.
use super::{gc_capture::CapturedSummaryInput, single_request::Dispatched};
use crate::protocol::ApiError;
use futures_util::FutureExt;
use rusqlite::Connection;
use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WriterJobId(u64);

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
    Rejected(super::discard::RejectionReason),
    Stale,
}

pub(crate) fn owner_rejection_reason(
    reason: super::discard::RejectionReason,
    owner_cancelled: bool,
) -> super::discard::RejectionReason {
    if owner_cancelled {
        super::discard::RejectionReason::Cancelled
    } else {
        reason
    }
}

#[derive(Debug)]
pub(crate) enum WriterCompletion {
    Ready,
    Failed,
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

struct OwnedGcJob {
    id: WriterJobId,
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
    rejection: Option<super::discard::RejectionReason>,
}

#[derive(Debug)]
struct JoinedFacts {
    producer_panicked: bool,
    producer_joined: bool,
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
            job.terminal.get_or_insert(terminal);
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
            rejection: None,
        });
        Ok(id)
    }

    pub(crate) fn reconcile_joined_retirement(&mut self, id: WriterJobId) -> bool {
        if !self.active.as_ref().is_some_and(|job| job.id == id) {
            return false;
        }
        self.process_lifecycle_before_db_reply();
        if self
            .active
            .as_ref()
            .is_some_and(|job| job.id == id && job.joined)
        {
            self.active = None;
            return true;
        }
        false
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
    pub(crate) fn complete_before_db_reply(
        &mut self,
        id: WriterJobId,
        result: Result<Dispatched, ApiError>,
    ) -> CompletionDisposition {
        let Some(job) = self.active.as_mut().filter(|job| job.id == id) else {
            return CompletionDisposition::Stale;
        };
        if job.terminal.is_some() {
            return CompletionDisposition::Closed;
        }
        match result {
            Ok(summary) => {
                job.terminal = Some(WriterCompletion::Ready);
                if job.phase == WriterJobPhase::Computing && !job.cancel.is_cancelled() {
                    job.adoption_candidate = Some(summary);
                    job.phase = WriterJobPhase::Ready;
                }
            }
            Err(_error) => {
                job.terminal = Some(WriterCompletion::Failed);
                job.cancel.cancel();
                job.phase = WriterJobPhase::Closing;
            }
        }
        if job.phase == WriterJobPhase::Ready {
            CompletionDisposition::Accepted
        } else if let Some(rejection) = job.rejection {
            CompletionDisposition::Rejected(rejection)
        } else {
            CompletionDisposition::Closed
        }
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
        )
            -> Result<(T, crate::compaction::GcStageFact), crate::storage::StorageError>,
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
        let Some(candidate) = job.adoption_candidate.as_ref() else {
            return Err(crate::storage::StorageError::InvalidInput(
                "GC adoption candidate is absent".into(),
            ));
        };
        let cancel = job.cancel.clone();
        let outcome: Result<T, crate::storage::StorageError> =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let tx = db.transaction()?;
                if cancel.is_cancelled() {
                    return Err(crate::storage::StorageError::from(
                        super::discard::rejected(
                            super::discard::RejectionReason::Cancelled,
                            "GC cancelled before final adoption",
                        ),
                    ));
                }
                let (value, fact) = apply(&tx, candidate)?;
                if cancel.is_cancelled() {
                    return Err(crate::storage::StorageError::from(
                        super::discard::rejected(
                            super::discard::RejectionReason::Cancelled,
                            "GC cancelled during final adoption",
                        ),
                    ));
                }
                super::attempt::record_gc_stage_tx(&tx, stage, &fact)?;
                super::attempt::finish_gc_tx(&tx, stage)?;
                if cancel.is_cancelled() {
                    return Err(crate::storage::StorageError::from(
                        super::discard::rejected(
                            super::discard::RejectionReason::Cancelled,
                            "GC cancelled before final commit",
                        ),
                    ));
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

    pub(crate) fn process_lifecycle_before_db_reply(&mut self) {
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
            return;
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
            if let Some(receiver) = job.joined_receiver.as_mut()
                && let Ok(facts) = receiver.try_recv()
            {
                if facts.producer_panicked {
                    job.lifecycle.get_or_insert(WriterLifecycle::ChildPanicked);
                }
                job.joined = facts.producer_joined;
                job.joined_receiver = None;
            }
        }
    }

    pub(crate) fn failure_write_allowed(&self, id: WriterJobId) -> bool {
        self.active.as_ref().is_some_and(|job| {
            job.id == id && job.phase == WriterJobPhase::Computing && !job.cancel.is_cancelled()
        })
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
    pub(crate) async fn shutdown(&mut self) -> bool {
        let id = self.active.as_ref().map(|job| job.id);
        if let Some(job) = self.active.as_mut() {
            job.phase = WriterJobPhase::Closing;
            job.cancel.cancel();
            start_join_reaper(job);
            if let Some(retirement) = job.retirement_task.as_mut() {
                let _ = retirement.await;
            }
            if let Some(receiver) = job.joined_receiver.as_mut()
                && let Ok(facts) = receiver.try_recv()
            {
                if facts.producer_panicked {
                    job.lifecycle.get_or_insert(WriterLifecycle::ChildPanicked);
                }
                job.joined = facts.producer_joined;
            }
            if let Some(signal) = job.completion_signal.as_mut()
                && let Ok(exit) = signal.try_recv()
            {
                record_child_exit(job, exit);
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
        match id {
            None => true,
            Some(id) => self.retire_joined(id),
        }
    }
}
