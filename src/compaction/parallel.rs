//! #224 stage 3: the requests of one map stage, or of one reduce round, run
//! together. There is no concurrency counter or window: the number is bounded
//! by the segments / batches and the existing request budget, which is
//! reserved before each dispatch, and throttling is left to the profile's FIFO
//! gate (after a 429 every request of the profile waits there).
//!
//! Admission is requested one at a time on the summary's own task, so the
//! summary has at most one pending gate ticket and queues in arrival order;
//! only the HTTP exchange runs in a task. All database writes, cache saves and
//! fences stay on the summary's task. Nothing here adopts.
use super::*;
use crate::model::http::{AttemptFailure, HttpModelClient};
use std::{collections::BTreeMap, time::Duration};
use tokio::task::JoinSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Map,
    Reduce,
}

pub(super) struct Job {
    /// Position in original order; a context-limit split extends it.
    path: Vec<usize>,
    /// Full per-part cache key.
    cache_key: String,
    request: Value,
    sources: Vec<String>,
    /// `(start, end, depth)` of a map segment, for context-limit splitting.
    span: (usize, usize, u8),
    /// 1-based reduce round; 0 for a map segment.
    round: u32,
    retries: u32,
    delay: Duration,
    cached_max_chars: Option<u64>,
}
impl Job {
    pub(super) fn cached_max_chars(mut self, limit: u64) -> Self {
        self.cached_max_chars = Some(limit);
        self
    }
    pub(super) fn reduce_round(mut self, round: u32) -> Self {
        self.round = round;
        self
    }
    pub(super) fn new(
        path: Vec<usize>,
        cache_key: String,
        request: Value,
        sources: Vec<String>,
        span: (usize, usize, u8),
    ) -> Self {
        Self {
            path,
            cache_key,
            request,
            sources,
            span,
            round: 0,
            retries: 0,
            delay: Duration::ZERO,
            cached_max_chars: None,
        }
    }
}

pub(super) type Split<'a> = &'a (dyn Fn((usize, usize, u8), Vec<usize>) -> Job + Sync);
pub(super) struct JobContext<'a> {
    pub(super) kind: Kind,
    pub(super) snapshot: &'a Snapshot,
    pub(super) connection: &'a crate::config::Connection,
    pub(super) client: &'a HttpModelClient,
    pub(super) cancel: &'a CancellationToken,
    /// Builds the two halves of a map segment rejected for its context size.
    pub(super) split: Option<Split<'a>>,
}
pub(super) struct JobParts {
    /// In original order, whatever the completion order.
    pub(super) parts: Vec<CachedPart>,
    pub(super) created: usize,
    pub(super) reused: usize,
}
struct Finished {
    job: Job,
    ledger: usize,
    result: Result<model::ModelResponse, AttemptFailure>,
}

impl JobContext<'_> {
    fn cancelled(&self, budget: &SummaryBudget<'_>) -> ApiError {
        match self.kind {
            Kind::Map => discard::rejected(
                discard::DiscardReason::Cancelled,
                "Compaction cancelled; original path retained",
            ),
            Kind::Reduce => discard::with_reason(
                budget.error("Summary cancelled; original path retained"),
                discard::DiscardReason::Cancelled,
            ),
        }
    }
    fn ensure_slot(&self, budget: &SummaryBudget<'_>) -> Result<(), ApiError> {
        match self.kind {
            Kind::Map => budget.ensure_map(),
            Kind::Reduce => budget.ensure_reduce(),
        }
    }
    fn failed(&self, budget: &SummaryBudget<'_>, failure: &AttemptFailure, note: &str) -> ApiError {
        match self.kind {
            Kind::Map => ApiError::new(ErrorCode::IoFailed, failure.error.to_string()).hint(note),
            Kind::Reduce => budget
                .error(&format!(
                    "Summary reduction failed: {}; original path retained",
                    failure.error
                ))
                .hint(note),
        }
    }
}

impl Compactor {
    /// Runs every job, retrying transient failures inside the request budget,
    /// and returns the parts in original order.
    pub(super) async fn run_jobs(
        &self,
        ctx: &JobContext<'_>,
        jobs: Vec<Job>,
        budget: &mut SummaryBudget<'_>,
        mut boundary: Option<&mut boundary::Boundary<'_>>,
    ) -> Result<JobParts, ApiError> {
        let run = ctx.cancel.child_token();
        let mut queue: VecDeque<Job> = jobs.into();
        let mut tasks: JoinSet<Finished> = JoinSet::new();
        let mut done: BTreeMap<Vec<usize>, CachedPart> = BTreeMap::new();
        let (mut created, mut reused) = (0usize, 0usize);
        let mut failure: Option<ApiError> = None;
        loop {
            while failure.is_none() && !queue.is_empty() {
                if ctx.cancel.is_cancelled() {
                    failure = Some(ctx.cancelled(budget));
                    break;
                }
                let job = queue.pop_front().expect("queue is not empty");
                match self
                    .launch(ctx, job, budget, &run, &mut tasks, boundary.as_deref_mut())
                    .await
                {
                    Ok(Some((path, part))) => {
                        if ctx.kind == Kind::Map {
                            budget.summary_characters = budget
                                .summary_characters
                                .saturating_add(part.text.chars().count() as u64);
                        }
                        done.insert(path, part);
                        reused += 1;
                    }
                    Ok(None) => {}
                    Err(error) => failure = Some(error),
                }
            }
            let Some(joined) = tasks.join_next().await else {
                break;
            };
            let Ok(finished) = joined else {
                failure.get_or_insert_with(|| {
                    ApiError::new(ErrorCode::IoFailed, "Summary request task failed")
                });
                run.cancel();
                continue;
            };
            let draining = failure.is_some();
            if let Err(error) = self
                .finished(
                    ctx,
                    finished,
                    budget,
                    &mut queue,
                    &mut done,
                    &mut created,
                    draining,
                )
                .await
            {
                // A fatal request failure stops the siblings with the same token.
                run.cancel();
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if ctx.cancel.is_cancelled() {
            return Err(ctx.cancelled(budget));
        }
        debug_assert!(queue.is_empty());
        Ok(JobParts {
            parts: done.into_values().collect(),
            created,
            reused,
        })
    }

    /// Reserves one budget slot and one admission, then dispatches the HTTP
    /// exchange as a task. A cached part is returned instead of a request.
    async fn launch(
        &self,
        ctx: &JobContext<'_>,
        job: Job,
        budget: &mut SummaryBudget<'_>,
        run: &CancellationToken,
        tasks: &mut JoinSet<Finished>,
        mut boundary: Option<&mut boundary::Boundary<'_>>,
    ) -> Result<Option<(Vec<usize>, CachedPart)>, ApiError> {
        if job.retries == 0 {
            let lookup = job.cache_key.clone();
            let cached = self
                .database
                .call(move |db| CachedPart::load(db, &lookup))
                .await
                .map_err(storage_error)?;
            if let Some(part) = cached.filter(CachedPart::reusable) {
                if let Some(limit) = job.cached_max_chars
                    && part.text.chars().count() as u64 > limit
                {
                    // Reserve before sending: a crash or cancellation must not
                    // permit repeated paid retries for the same cache witness.
                    let marker = format!("{}.oversize-retry.{limit}", job.cache_key);
                    let marker_value = json!({"sources": job.sources}).to_string();
                    let reserved = self
                        .database
                        .call(move |db| {
                            Ok(db.execute(
                                "INSERT OR IGNORE INTO app_meta(key,value) VALUES(?1,?2)",
                                rusqlite::params![marker, marker_value],
                            )? == 1)
                        })
                        .await
                        .map_err(storage_error)?;
                    if !reserved {
                        return Err(budget.error("Oversized summary retry already used for this selection; original path retained"));
                    }
                } else {
                    return Ok(Some((job.path, part)));
                }
            }
        }
        ctx.ensure_slot(budget)?;
        self.summary_backoff(ctx.snapshot, job.delay, ctx.cancel, boundary.as_deref_mut())
            .await?;
        let admission = self
            .summary_admission(ctx.client, ctx.snapshot, ctx.cancel, boundary)
            .await?;
        match ctx.kind {
            Kind::Map => budget.request_map()?,
            Kind::Reduce => budget.request_reduce()?,
        }
        let ledger = budget
            .usage
            .begin(&self.database, ctx.kind == Kind::Reduce)
            .await?;
        let client = ctx.client.clone();
        let thread = ctx.snapshot.thread.clone();
        let run = run.clone();
        let request = job.request.clone();
        tasks.spawn(async move {
            let result = Self::summary_response(&client, &thread, request, run, admission).await;
            Finished {
                job,
                ledger,
                result,
            }
        });
        // Let other ready tasks (for example another thread's conversation
        // request) take their own gate ticket before this summary's next one.
        tokio::task::yield_now().await;
        Ok(None)
    }

    /// Accounts one finished request. While `draining` (a failure or
    /// cancellation already stopped the run) a late success is only cached by
    /// its content key and nothing else advances.
    #[allow(clippy::too_many_arguments)]
    async fn finished(
        &self,
        ctx: &JobContext<'_>,
        finished: Finished,
        budget: &mut SummaryBudget<'_>,
        queue: &mut VecDeque<Job>,
        done: &mut BTreeMap<Vec<usize>, CachedPart>,
        created: &mut usize,
        draining: bool,
    ) -> Result<(), ApiError> {
        let Finished {
            mut job,
            ledger,
            result,
        } = finished;
        let response = match result {
            Ok(response) => {
                budget
                    .usage
                    .observe_http(&self.database, ledger, response.response_facts)
                    .await?;
                budget
                    .usage
                    .observe(&self.database, usage::usage_of(Some(&response.raw)))
                    .await?;
                response
            }
            Err(failure) => {
                budget
                    .usage
                    .observe_http(&self.database, ledger, failure.facts)
                    .await?;
                budget
                    .usage
                    .observe(
                        &self.database,
                        usage::usage_of(failure.raw_response.as_ref()),
                    )
                    .await?;
                if draining {
                    return Ok(());
                }
                let (start, end, depth) = job.span;
                if let Some(split) = ctx.split
                    && failure.error.is_context_limit()
                    && end - start > 1
                    && depth < 8
                {
                    let middle = start + (end - start) / 2;
                    for (position, span) in [(start, middle), (middle, end)]
                        .into_iter()
                        .enumerate()
                        .rev()
                    {
                        let mut path = job.path.clone();
                        path.push(position);
                        queue.push_front(split((span.0, span.1, depth + 1), path));
                    }
                    return Ok(());
                }
                return match super::retry::retry_delay(ctx.connection, &failure, job.retries) {
                    Some(delay) if ctx.ensure_slot(budget).is_ok() => {
                        job.retries += 1;
                        job.delay = delay;
                        queue.push_front(job);
                        Ok(())
                    }
                    Some(_) => Err(ctx.failed(
                        budget,
                        &failure,
                        "Summary request budget exhausted while retrying; original history and successful summary parts were retained; retry explicitly",
                    )),
                    None => Err(ctx.failed(
                        budget,
                        &failure,
                        "Original history and successful summary parts were retained; retry explicitly",
                    )),
                };
            }
        };
        let refused = summary_refused(&response.output);
        let text = response.text;
        let empty = text.trim().is_empty();
        if draining && (refused || empty) {
            return Ok(());
        }
        let part = CachedPart {
            text,
            sources: job.sources.clone(),
            cache_key: Some(job.cache_key.clone()),
            raw_response: Some(response.raw),
            stage: Some(if job.round == 0 { "map" } else { "reduce" }.into()),
            round: (job.round > 0).then_some(job.round),
        };
        part.save(&self.database, job.cache_key.clone()).await?;
        if draining {
            return Ok(());
        }
        if refused || empty {
            return Err(match ctx.kind {
                Kind::Reduce => {
                    budget.error("Summary reduction was refused or empty; original path retained")
                }
                Kind::Map if refused => {
                    invalid("Summary response contained a refusal; original history retained")
                        .details(json!({"summary_part_key":part.cache_key}))
                }
                Kind::Map => {
                    invalid("Summary returned no assistant text; original history retained")
                        .details(json!({"summary_part_key":part.cache_key}))
                }
            });
        }
        if ctx.kind == Kind::Map {
            budget.summary_characters = budget
                .summary_characters
                .saturating_add(part.text.chars().count() as u64);
        }
        done.insert(job.path, part);
        *created += 1;
        Ok(())
    }
}
