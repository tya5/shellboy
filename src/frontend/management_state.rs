//! Result generation and wait-observation ownership, independent of draft images.
use crate::protocol::Command;
use serde_json::Value;
use std::sync::Arc;
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
    pub(super) fn usage_ready(&self) -> Option<UsageWatch> {
        self.usage_watch
            .as_ref()
            .filter(|w| w.dirty && !w.inflight)
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
        if let Some(w) = &mut self.usage_watch
            && w.generation == generation
        {
            w.inflight = false;
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
        assert!(m.usage_ready().is_none());
        m.usage_changed("other");
        m.usage_finished(generation, None);
        assert!(m.usage_ready().is_none());
        m.usage_changed("t");
        m.usage_changed("t");
        assert!(m.usage_ready().is_some());
        m.usage_begin_refresh(generation);
        assert!(m.usage_ready().is_none());
        m.usage_changed("t");
        m.usage_finished(generation, None);
        assert!(m.usage_ready().is_some());
        m.close_usage();
        assert!(m.usage_ready().is_none());
        m.usage_changed("t");
        assert!(!m.usage_accepts(generation));
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
        m.usage_finished(generation, None);
        assert!(m.usage_ready().is_some());
        m.next_generation();
        assert!(!m.usage_accepts(generation));
    }
}
