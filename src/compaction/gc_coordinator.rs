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

type SourceWitness = super::gc_capture::GcSourceFacts;

#[derive(Clone, Debug)]
struct CaptureWitness {
    thread: String,
    profile_revision: i64,
    epoch: i64,
    nonappend_generation: i64,
    profile: String,
    model: String,
    effort: Option<String>,
    origin_turn: Option<String>,
    origin_generation: Option<i64>,
    request_turn_id: Option<String>,
    request_generation: Option<i64>,
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
            origin_turn: input.origin_turn.clone(),
            origin_generation: input.origin_generation,
            request_turn_id: input.request_turn_id.clone(),
            request_generation: input.request_generation,
            sources: input
                .groups
                .iter()
                .flat_map(|group| group.stamps.iter().map(|stamp| stamp.source.clone()))
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
            rejection: None,
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
                job.terminal = Some(WriterCompletion::Ready);
                if job.phase == WriterJobPhase::Computing && !job.cancel.is_cancelled() {
                    match witness_rejection(db, &job.witness) {
                        Ok(None) => {
                            job.adoption_candidate = Some(summary);
                            job.phase = WriterJobPhase::Ready;
                        }
                        Ok(Some(rejection)) => {
                            job.rejection = Some(rejection);
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
            Err(_error) => {
                job.terminal = Some(WriterCompletion::Failed);
                job.cancel.cancel();
                job.phase = WriterJobPhase::Closing;
            }
        }
        Ok(if job.phase == WriterJobPhase::Ready {
            CompletionDisposition::Accepted
        } else if let Some(rejection) = job.rejection {
            CompletionDisposition::Rejected(rejection)
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
        match witness_rejection(db, &job.witness) {
            Ok(None) => {}
            Ok(Some(rejection)) => {
                job.rejection = Some(rejection);
                job.adoption_candidate = None;
                job.phase = WriterJobPhase::Closing;
                job.cancel.cancel();
                return Err(crate::storage::StorageError::InvalidInput(format!(
                    "GC source witness rejected before strict adoption: {:?}",
                    rejection
                )));
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
                super::attempt::record_gc_stage_tx(&tx, stage, &fact)?;
                super::attempt::finish_gc_tx(&tx, stage)?;
                if fact.drop_all_reasoning {
                    let captured_epoch = fact.captured_reasoning_epoch;
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
    ) -> Result<Option<super::discard::RejectionReason>, rusqlite::Error> {
        let Some(job) = self.active.as_mut().filter(|job| job.id == id) else {
            return Ok(Some(super::discard::RejectionReason::SourceChanged));
        };
        if job.cancel.is_cancelled() {
            return Ok(Some(
                job.rejection
                    .unwrap_or(super::discard::RejectionReason::Cancelled),
            ));
        }
        if job.phase != WriterJobPhase::Computing {
            return Ok(if job.phase == WriterJobPhase::Ready {
                None
            } else {
                Some(
                    job.rejection
                        .unwrap_or(super::discard::RejectionReason::SourceChanged),
                )
            });
        }
        match witness_rejection(db, &job.witness) {
            Ok(None) => Ok(None),
            Ok(Some(rejection)) => {
                job.rejection = Some(rejection);
                job.cancel.cancel();
                job.phase = WriterJobPhase::Closing;
                Ok(Some(rejection))
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
            if let Some(receiver) = job.joined_receiver.as_mut()
                && let Ok(facts) = receiver.try_recv()
            {
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
            return Ok(true);
        }
        match witness_rejection(db, &job.witness) {
            Ok(None) => Ok(false),
            Ok(Some(rejection)) => {
                job.rejection = Some(rejection);
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

fn witness_rejection(
    db: &Connection,
    witness: &CaptureWitness,
) -> Result<Option<super::discard::RejectionReason>, rusqlite::Error> {
    use super::discard::RejectionReason as R;
    let facts:Option<(i64,i64,i64,String,String,Option<String>)>=db.query_row("SELECT profile_revision,reasoning_epoch,selection_nonappend_generation,profile_id,model_id,effort FROM thread WHERE id=?1 AND deleted_at IS NULL AND archived_at IS NULL",[&witness.thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let Some(facts) = facts else {
        return Ok(Some(R::ThreadUnavailable));
    };
    if facts.0 != witness.profile_revision
        || facts.3 != witness.profile
        || facts.4 != witness.model
        || facts.5 != witness.effort
    {
        return Ok(Some(R::ProfileChanged));
    }
    if facts.1 != witness.epoch {
        return Ok(Some(R::EpochChanged));
    }
    if facts.2 != witness.nonappend_generation {
        return Ok(Some(R::NonappendGenerationChanged));
    }
    let stopping: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state='stopping')",
        [&witness.thread],
        |row| row.get(0),
    )?;
    if stopping {
        return Ok(Some(R::RunStopped));
    }
    for (turn, expected) in [
        (witness.origin_turn.as_deref(), witness.origin_generation),
        (
            witness.request_turn_id.as_deref(),
            witness.request_generation,
        ),
    ] {
        if let Some(turn) = turn {
            let current: Option<i64> = db
                .query_row(
                    "SELECT generation FROM turn WHERE id=?1 AND thread_id=?2",
                    params![turn, witness.thread],
                    |r| r.get(0),
                )
                .optional()?;
            if current != expected {
                return Ok(Some(R::TurnChanged));
            }
        }
    }
    let Some(first) = witness.sources.first() else {
        return Ok(None);
    };
    let last = witness.sources.last().expect("first source checked");
    let Some(end) = last.ordinal.checked_add(1) else {
        return Ok(Some(R::SourceChanged));
    };
    let current =
        super::gc_capture::selected_source_facts(db, &witness.thread, first.ordinal..end)?;
    Ok(super::gc_capture::classify_selected_source_facts(
        &witness.sources,
        &current,
        super::gc_capture::SourceFactPolicy::EarlyReconcile,
    )
    .err())
}

#[cfg(test)]
mod attachment_witness_tests {
    use super::*;
    use rusqlite::Connection;

    fn setup() -> (Connection, CaptureWitness) {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE thread(id TEXT,profile_revision INTEGER,reasoning_epoch INTEGER,selection_nonappend_generation INTEGER,profile_id TEXT,model_id TEXT,effort TEXT,deleted_at TEXT,archived_at TEXT,selection_revision INTEGER);
          CREATE TABLE run(id TEXT,thread_id TEXT,state TEXT,stop_reason TEXT);
          CREATE TABLE turn(id TEXT,thread_id TEXT,generation INTEGER);
          CREATE TABLE node(id TEXT,payload_revision INTEGER,turn_id TEXT,payload_json TEXT);
          CREATE TABLE thread_path(thread_id TEXT,ordinal INTEGER,node_id TEXT);
          CREATE TABLE attachment_ref(id TEXT,source_node_id TEXT,absolute_path TEXT,input_kind TEXT,ownership TEXT,created_at TEXT);
          CREATE TABLE attachment_selection(thread_id TEXT,attachment_id TEXT,state TEXT,reason TEXT);
          CREATE TABLE receipt(target_thread_id TEXT,message_node_id TEXT,state TEXT);
          CREATE TABLE tool_group(id TEXT,state TEXT);
          CREATE TABLE tool_call(group_id TEXT,call_id TEXT,state TEXT);
          CREATE TABLE tool_result(group_id TEXT,call_id TEXT);
          INSERT INTO thread VALUES('t',3,7,2,'p','m',NULL,NULL,NULL,8);
          INSERT INTO run VALUES('r','t','completed',NULL);
          INSERT INTO node VALUES('source',4,NULL,'{}');
          INSERT INTO thread_path VALUES('t',0,'source');").unwrap();
        let attachments = super::super::gc_capture::selected_source_facts(&db, "t", 0..1).unwrap()
            [0]
        .attachments
        .clone();
        let witness = CaptureWitness {
            thread: "t".into(),
            profile_revision: 3,
            epoch: 7,
            nonappend_generation: 2,
            profile: "p".into(),
            model: "m".into(),
            effort: None,
            origin_turn: None,
            origin_generation: None,
            request_turn_id: None,
            request_generation: None,
            sources: vec![SourceWitness {
                ordinal: 0,
                id: "source".into(),
                turn: None,
                revision: 4,
                group_id: None,
                protected: false,
                group_complete: true,
                has_summary_marker: false,
                attachments,
            }],
        };
        (db, witness)
    }

    #[test]
    fn selected_attachment_mutation_rejects_but_append_only_tail_remains_eligible() {
        use super::super::discard::RejectionReason as R;
        let (db, witness) = setup();
        assert_eq!(witness_rejection(&db, &witness).unwrap(), None);
        db.execute(
            "INSERT INTO attachment_ref VALUES('a','source','/synthetic','file','external','now')",
            [],
        )
        .unwrap();
        assert_eq!(
            witness_rejection(&db, &witness).unwrap(),
            Some(R::SourceChanged)
        );

        let (db, witness) = setup();
        db.execute_batch("INSERT INTO attachment_ref VALUES('a','source','/synthetic','file','external','now'); INSERT INTO attachment_selection VALUES('t','a','active',NULL);").unwrap();
        assert_eq!(
            witness_rejection(&db, &witness).unwrap(),
            Some(R::SourceChanged)
        );

        let (db, witness) = setup();
        db.execute_batch("UPDATE thread SET selection_revision=selection_revision+1; INSERT INTO node VALUES('tail',1,NULL,'{}'); INSERT INTO thread_path VALUES('t',1,'tail');").unwrap();
        assert_eq!(witness_rejection(&db, &witness).unwrap(), None);

        let (db, witness) = setup();
        db.execute(
            "UPDATE run SET state='stopping',stop_reason='stopping' WHERE thread_id='t'",
            [],
        )
        .unwrap();
        assert_eq!(
            witness_rejection(&db, &witness).unwrap(),
            Some(R::RunStopped)
        );
        db.execute(
            "UPDATE run SET stop_reason='explicit_stop' WHERE thread_id='t'",
            [],
        )
        .unwrap();
        assert_eq!(
            witness_rejection(&db, &witness).unwrap(),
            Some(R::RunStopped)
        );
    }
}

#[cfg(test)]
mod run_stop_order_tests {
    use super::super::discard::RejectionReason as R;

    #[test]
    fn source_witness_first_keeps_run_stopped_reason() {
        assert_eq!(
            super::owner_rejection_reason(R::RunStopped, false),
            R::RunStopped
        );
    }

    #[test]
    fn owner_token_first_reports_cancelled() {
        assert_eq!(
            super::owner_rejection_reason(R::RunStopped, true),
            R::Cancelled
        );
    }
}
