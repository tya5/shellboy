//! Result generation and wait-observation ownership, independent of draft images.
use crate::protocol::Command;
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
const USAGE_COOLDOWN: Duration = Duration::from_secs(3);
use tokio_util::sync::CancellationToken;
#[derive(Default)]
pub(super) struct Management {
    pub(super) request_id: u64,
    pub(super) detail: Option<Arc<Value>>,
    generation: u64,
    usage_watch: Option<UsageWatch>,
    usage_closed_epoch: Option<u64>,
    wait_cancel: CancellationToken,
}
impl Management {
    pub(super) fn next_generation(&mut self) -> u64 {
        self.close_usage();
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
    pub(super) fn accepts(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub(super) fn begin_result(&mut self) -> u64 {
        self.request_id = self.request_id.wrapping_add(1);
        self.detail = None;
        self.request_id
    }
    pub(super) fn observe(&self, command: &Command) -> CancellationToken {
        if matches!(command, Command::Wait(_)) {
            self.wait_cancel.child_token()
        } else {
            CancellationToken::new()
        }
    }
    pub(super) fn cancel_wait(&mut self) {
        self.wait_cancel.cancel();
        self.wait_cancel = CancellationToken::new();
    }
}

fn epoch_after(value: u64, earlier: u64) -> bool {
    let delta = value.wrapping_sub(earlier);
    delta != 0 && delta < (1_u64 << 63)
}

/// One UI-only cached read; Changed invalidates it, rendering never runs SQL.
#[derive(Clone)]
pub(super) struct UsageWatch {
    pub command: Command,
    pub context: crate::ui::ManagementContext,
    pub generation: u64,
    thread: Option<String>,
    children: bool,
    dirty: bool,
    inflight: bool,
    next_refresh: Option<Instant>,
}
impl Management {
    pub(super) fn watch_usage(
        &mut self,
        command: &Command,
        context: Option<&crate::ui::ManagementContext>,
        generation: u64,
    ) {
        let recorded = matches!(
            command,
            Command::UsageGet(_) | Command::ThreadGet(_) | Command::RunGet(_) | Command::TurnGet(_)
        );
        self.usage_watch = if recorded {
            context
                .filter(|context| {
                    self.usage_closed_epoch
                        .is_none_or(|closed| epoch_after(context.epoch, closed))
                })
                .map(|context| UsageWatch {
                    command: command.clone(),
                    context: context.clone(),
                    generation,
                    thread: match command {
                        Command::UsageGet(a) => {
                            a.thread_id.clone().or(context.owner_thread.clone())
                        }
                        Command::ThreadGet(a) => {
                            a.thread_id.clone().or(context.owner_thread.clone())
                        }
                        _ => context.owner_thread.clone(),
                    },
                    children: matches!(command,Command::UsageGet(a) if a.include_children),
                    dirty: false,
                    inflight: true,
                    next_refresh: None,
                })
        } else {
            None
        };
    }
    pub(super) fn close_usage(&mut self) {
        self.usage_watch = None;
    }
    pub(super) fn close_usage_at(&mut self, epoch: u64) {
        if self
            .usage_closed_epoch
            .is_none_or(|old| epoch_after(epoch, old))
        {
            self.usage_closed_epoch = Some(epoch);
        }
        if let Some(closed) = self.usage_closed_epoch
            && self
                .usage_watch
                .as_ref()
                .is_some_and(|watch| !epoch_after(watch.context.epoch, closed))
        {
            self.close_usage();
        }
    }

    pub(super) fn usage_changed(&mut self, thread: &str) {
        if let Some(w) = &mut self.usage_watch
            && (w.children || w.thread.as_deref() == Some(thread))
        {
            w.dirty = true;
        }
    }
    pub(super) fn usage_lagged(&mut self) {
        if let Some(w) = &mut self.usage_watch {
            w.dirty = true;
        }
    }
    pub(super) fn usage_waiting(&self) -> bool {
        self.usage_watch
            .as_ref()
            .is_some_and(|w| w.dirty && !w.inflight)
    }
    pub(super) fn usage_ready(&self) -> Option<UsageWatch> {
        self.usage_ready_at(Instant::now())
    }
    pub(super) fn usage_ready_at(&self, now: Instant) -> Option<UsageWatch> {
        self.usage_watch
            .as_ref()
            .filter(|w| w.dirty && !w.inflight && w.next_refresh.is_none_or(|at| now >= at))
            .cloned()
    }
    pub(super) fn usage_begin_refresh(&mut self, generation: u64) {
        if let Some(w) = &mut self.usage_watch
            && w.generation == generation
        {
            w.inflight = true;
            w.dirty = false;
        }
    }
    pub(super) fn usage_accepts(&self, generation: u64) -> bool {
        self.usage_watch
            .as_ref()
            .is_some_and(|w| w.generation == generation)
    }
    pub(super) fn usage_finished(&mut self, generation: u64, value: Option<&Value>) {
        self.usage_finished_at(generation, value, Instant::now());
    }
    pub(super) fn usage_finished_at(
        &mut self,
        generation: u64,
        value: Option<&Value>,
        now: Instant,
    ) {
        if let Some(w) = &mut self.usage_watch
            && w.generation == generation
        {
            w.inflight = false;
            w.next_refresh = Some(now + USAGE_COOLDOWN);
            if let Some(thread) = value
                .and_then(|v| v.get("thread_id"))
                .and_then(Value::as_str)
            {
                w.thread = Some(thread.into());
            }
        }
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use crate::{
        protocol::{ThreadTarget, UsageGetArgs},
        ui::ManagementContext,
    };
    #[test]
    fn priority_close_cannot_rearm_a_queued_old_usage_request() {
        let mut m = Management::default();
        let mut ctx = ManagementContext {
            epoch: 1,
            owner_draft: "d".into(),
            owner_thread: Some("t".into()),
        };
        m.close_usage_at(2);
        let generation = m.next_generation();
        let command = Command::ThreadGet(ThreadTarget {
            thread_id: Some("t".into()),
        });
        m.watch_usage(&command, Some(&ctx), generation);
        assert!(!m.usage_accepts(generation));
        ctx.epoch = 3;
        m.watch_usage(&command, Some(&ctx), generation);
        assert!(m.usage_accepts(generation));
        m.close_usage_at(2);
        assert!(m.usage_accepts(generation));
        m.close_usage_at(4);
        assert!(!m.usage_accepts(generation));
        assert!(epoch_after(0, u64::MAX));
    }
    #[test]
    fn usage_reads_are_change_driven_coalesced_and_generation_bound() {
        let now = Instant::now();
        let mut m = Management::default();
        let ctx = ManagementContext {
            epoch: 1,
            owner_draft: "d".into(),
            owner_thread: Some("t".into()),
        };
        let generation = m.next_generation();
        m.watch_usage(
            &Command::ThreadGet(ThreadTarget {
                thread_id: Some("t".into()),
            }),
            Some(&ctx),
            generation,
        );
        assert!(m.usage_ready_at(now).is_none());
        m.usage_finished_at(generation, None, now);
        for _ in 0..1000 {
            m.usage_changed("t");
        }
        assert!(
            m.usage_ready_at(now + USAGE_COOLDOWN - Duration::from_nanos(1))
                .is_none()
        );
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN).is_some());
        m.usage_begin_refresh(generation);
        for _ in 0..1000 {
            m.usage_changed("t");
        }
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN * 2).is_none());
        m.usage_finished_at(generation, None, now + USAGE_COOLDOWN);
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN * 2).is_some());
        m.close_usage();
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN * 3).is_none());
        let generation = m.next_generation();
        m.watch_usage(
            &Command::UsageGet(UsageGetArgs {
                thread_id: Some("t".into()),
                include_children: true,
                ..Default::default()
            }),
            Some(&ctx),
            generation,
        );
        m.usage_changed("child");
        m.usage_finished_at(generation, None, now);
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN).is_some());
        m.next_generation();
        assert!(!m.usage_accepts(generation));
    }
}

#[cfg(test)]
mod usage_throttle_log_tests {
    use super::*;
    use crate::{
        diagnostics::{Diagnostics, UsageTrigger},
        protocol::UsageGetArgs,
        storage::DbWorker,
        ui::ManagementContext,
        usage_totals::{ReadContext, Scope, read_logged},
    };
    #[tokio::test]
    async fn changed_bursts_execute_one_pending_read_and_fast_reads_log_nothing() {
        for elapsed in [Duration::from_micros(99_999), Duration::from_millis(100)] {
            let root = tempfile::tempdir().unwrap();
            let db = DbWorker::open(root.path().join("db.sqlite")).await.unwrap();
            db.call(|db|{db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','CONTENT_SENTINEL_NEVER_LOG','.','AGENTS.md','fixture','fixture','now'); INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2026-10-30T00:00:00Z','2026-10-30T00:00:01Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-10-30T00:00:00Z','2026-10-30T00:00:01Z'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('a','u',0,0,'fixture','fixture',0,'completed','{\"text\":\"CONTENT_SENTINEL_NEVER_LOG\"}','{\"input_tokens\":1,\"attribution\":\"CONTENT_SENTINEL_NEVER_LOG\"}','2026-10-30T00:00:00Z','2026-10-30T00:00:00.010Z');")?;Ok(())}).await.unwrap();
            let logs = root.path().join("logs");
            let diagnostics = Diagnostics::start(logs.clone(), crate::config::Logging::default());
            let mut m = Management::default();
            let context = ManagementContext {
                epoch: 1,
                owner_draft: "d".into(),
                owner_thread: Some("t".into()),
            };
            let generation = m.next_generation();
            m.watch_usage(
                &Command::UsageGet(UsageGetArgs {
                    thread_id: Some("t".into()),
                    ..Default::default()
                }),
                Some(&context),
                generation,
            );
            let mut now = Instant::now();
            let mut queries = 0;
            for trigger in [
                UsageTrigger::Open,
                UsageTrigger::Changed,
                UsageTrigger::Changed,
            ] {
                if queries > 0 {
                    assert!(m.usage_ready_at(now).is_some());
                    m.usage_begin_refresh(generation);
                }
                let diagnostic = diagnostics.clone();
                let value = db
                    .call(move |db| {
                        read_logged(
                            db,
                            &Scope::Thread {
                                id: "t".into(),
                                children: false,
                            },
                            ReadContext {
                                trigger,
                                elapsed_override: Some(elapsed),
                            },
                            Some(&diagnostic),
                        )
                        .map_err(crate::storage::StorageError::from)
                    })
                    .await
                    .unwrap();
                queries += 1;
                m.usage_finished_at(generation, Some(&value), now);
                for _ in 0..1000 {
                    m.usage_changed("t");
                }
                assert!(
                    m.usage_ready_at(now + USAGE_COOLDOWN - Duration::from_nanos(1))
                        .is_none()
                );
                now += USAGE_COOLDOWN;
            }
            assert_eq!(queries, 3);
            m.close_usage_at(2);
            assert!(m.usage_ready_at(now + USAGE_COOLDOWN).is_none());
            diagnostics.shutdown().await;
            db.shutdown().await.unwrap();
            let text = std::fs::read_to_string(logs.join("shellboy.log")).unwrap_or_default();
            assert!(!text.contains("CONTENT_SENTINEL_NEVER_LOG"));
            let rows = text
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            if elapsed < crate::diagnostics::USAGE_SLOW_THRESHOLD {
                assert!(rows.is_empty());
            } else {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0]["usage_slow"]["trigger"], "open");
                assert!(
                    rows[1..]
                        .iter()
                        .all(|row| row["usage_slow"]["trigger"] == "changed")
                );
            }
        }
    }
}

#[cfg(test)]
mod usage_lag_tests {
    use super::*;
    use crate::{protocol::UsageGetArgs, ui::ManagementContext};
    #[test]
    fn lost_changed_reinspects_one_open_scope_without_bypassing_cooldown() {
        let mut m = Management::default();
        let now = Instant::now();
        let context = ManagementContext {
            epoch: 1,
            owner_draft: "d".into(),
            owner_thread: Some("t".into()),
        };
        let generation = m.next_generation();
        m.watch_usage(
            &Command::UsageGet(UsageGetArgs {
                thread_id: Some("t".into()),
                ..Default::default()
            }),
            Some(&context),
            generation,
        );
        m.usage_finished_at(generation, None, now);
        m.usage_lagged();
        m.usage_lagged();
        assert!(m.usage_waiting());
        assert!(
            m.usage_ready_at(now + USAGE_COOLDOWN - Duration::from_nanos(1))
                .is_none()
        );
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN).is_some());
        m.usage_begin_refresh(generation);
        m.usage_lagged();
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN * 2).is_none());
        m.usage_finished_at(generation, None, now + USAGE_COOLDOWN);
        assert!(m.usage_ready_at(now + USAGE_COOLDOWN * 2).is_some());
        m.close_usage();
        m.usage_lagged();
        assert!(!m.usage_waiting());
        assert!(!m.usage_accepts(generation));
    }
}
