//! Bounded UI-only control delivery, independent of ordinary action admission.
use super::UiAction;
use std::{
    collections::BTreeSet,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, mpsc};

const CANCEL_TARGETS: usize = 32;
#[derive(Default)]
struct Controls {
    quit_requested: bool,
    quit: Option<Vec<UiAction>>,
    cancels: BTreeSet<String>,
    active_cancel: Option<String>,
    cancel_wait: bool,
    wait: bool,
    force: bool,
    alive: bool,
}
struct Shared {
    controls: Mutex<Controls>,
    changed: Notify,
}

#[derive(Clone)]
pub struct ActionSender {
    normal: mpsc::Sender<UiAction>,
    shared: Option<Arc<Shared>>,
}
pub struct ActionReceiver {
    normal: mpsc::Receiver<UiAction>,
    shared: Option<Arc<Shared>>,
    closing: bool,
    quit_returned: bool,
    snapshots: std::collections::VecDeque<UiAction>,
    cancel_enabled: bool,
}

pub fn channel(capacity: usize) -> (ActionSender, ActionReceiver) {
    let (normal, receiver) = mpsc::channel(capacity);
    let shared = Arc::new(Shared {
        controls: Mutex::new(Controls {
            alive: true,
            ..Controls::default()
        }),
        changed: Notify::new(),
    });
    (
        ActionSender {
            normal,
            shared: Some(shared.clone()),
        },
        ActionReceiver {
            normal: receiver,
            shared: Some(shared),
            closing: false,
            quit_returned: false,
            snapshots: Default::default(),
            cancel_enabled: true,
        },
    )
}
impl From<mpsc::Sender<UiAction>> for ActionSender {
    fn from(normal: mpsc::Sender<UiAction>) -> Self {
        Self {
            normal,
            shared: None,
        }
    }
}
impl From<mpsc::Receiver<UiAction>> for ActionReceiver {
    fn from(normal: mpsc::Receiver<UiAction>) -> Self {
        Self {
            normal,
            shared: None,
            closing: false,
            quit_returned: false,
            snapshots: Default::default(),
            cancel_enabled: true,
        }
    }
}
impl ActionSender {
    // Preserve Tokio's lossless rejected-action contract without allocating a wrapper.
    #[allow(clippy::result_large_err)]
    pub fn try_send(&self, action: UiAction) -> Result<(), mpsc::error::TrySendError<UiAction>> {
        let Some(shared) = &self.shared else {
            return self.normal.try_send(action);
        };
        let mut controls = shared.controls.lock().expect("UI control mailbox poisoned");
        if !controls.alive {
            return Err(mpsc::error::TrySendError::Closed(action));
        }
        if !matches!(
            action,
            UiAction::Quit
                | UiAction::Cancel { .. }
                | UiAction::CancelWait
                | UiAction::WaitForShutdown
                | UiAction::ForceQuit
        ) {
            return if controls.quit_requested {
                Err(mpsc::error::TrySendError::Closed(action))
            } else {
                self.normal.try_send(action)
            };
        }
        match action {
            UiAction::Quit => {
                if !controls.quit_requested {
                    controls.quit_requested = true;
                    controls.quit = Some(vec![]);
                }
            }
            UiAction::Cancel { thread_id } => {
                if controls.active_cancel.as_ref() != Some(&thread_id)
                    && !controls.cancels.contains(&thread_id)
                {
                    if controls.cancels.len() + usize::from(controls.active_cancel.is_some())
                        >= CANCEL_TARGETS
                    {
                        return Err(mpsc::error::TrySendError::Full(UiAction::Cancel {
                            thread_id,
                        }));
                    }
                    controls.cancels.insert(thread_id);
                }
            }
            UiAction::CancelWait => controls.cancel_wait = true,
            UiAction::WaitForShutdown => controls.wait = true,
            UiAction::ForceQuit => controls.force = true,
            _ => unreachable!(),
        }
        drop(controls);
        shared.changed.notify_one();
        Ok(())
    }
    /// Quit carries its latest draft even when ordinary admission is saturated.
    /// The frozen UI contributes at most one latest snapshot per retained Thread.
    pub fn try_send_batch(
        &self,
        actions: Vec<UiAction>,
    ) -> Vec<mpsc::error::TrySendError<UiAction>> {
        if let Some(shared) = &self.shared
            && actions
                .iter()
                .any(|action| matches!(action, UiAction::Quit))
        {
            let mut snapshots = Vec::new();
            for action in actions {
                if matches!(action, UiAction::SaveDraft { .. }) {
                    snapshots.push(action);
                }
            }
            let mut controls = shared.controls.lock().expect("UI control mailbox poisoned");
            if !controls.alive {
                return vec![mpsc::error::TrySendError::Closed(UiAction::Quit)];
            }
            if !controls.quit_requested {
                controls.quit_requested = true;
                controls.quit = Some(snapshots);
            }
            drop(controls);
            shared.changed.notify_one();
            Vec::new()
        } else {
            actions
                .into_iter()
                .filter_map(|action| self.try_send(action).err())
                .collect()
        }
    }
}
impl ActionReceiver {
    pub fn is_closing(&self) -> bool {
        self.closing
    }
    pub fn enable_cancel(&mut self, enabled: bool) {
        self.cancel_enabled = enabled;
    }
    pub fn complete_cancel(&self) {
        if let Some(shared) = &self.shared {
            shared
                .controls
                .lock()
                .expect("UI control mailbox poisoned")
                .active_cancel = None;
            shared.changed.notify_one();
        }
    }
    fn take_control(&mut self) -> Option<UiAction> {
        let shared = self.shared.as_ref()?;
        let mut controls = shared.controls.lock().expect("UI control mailbox poisoned");
        if let Some(snapshot) = controls.quit.take() {
            self.normal.close();
            self.closing = true;
            self.snapshots = snapshot.into();
        }
        if (!self.closing || self.quit_returned) && controls.wait {
            controls.wait = false;
            return Some(UiAction::WaitForShutdown);
        }
        if (!self.closing || self.quit_returned) && controls.force {
            controls.force = false;
            return Some(UiAction::ForceQuit);
        }
        if controls.cancel_wait {
            controls.cancel_wait = false;
            return Some(UiAction::CancelWait);
        }
        if !self.closing
            && self.cancel_enabled
            && controls.active_cancel.is_none()
            && let Some(thread_id) = controls.cancels.pop_first()
        {
            controls.active_cancel = Some(thread_id.clone());
            return Some(UiAction::Cancel { thread_id });
        }
        None
    }
    pub async fn recv(&mut self) -> Option<UiAction> {
        loop {
            // Register before checking slots so a concurrent delivery is retained.
            let shared = self.shared.clone();
            let notified = async {
                if let Some(shared) = &shared {
                    shared.changed.notified().await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::pin!(notified);
            if let Some(action) = self.take_control() {
                return Some(action);
            }
            if self.closing && !self.quit_returned {
                while let Ok(action) = self.normal.try_recv() {
                    if matches!(
                        action,
                        UiAction::Send { .. }
                            | UiAction::SendEdited { .. }
                            | UiAction::SaveDraft { .. }
                    ) {
                        return Some(action);
                    }
                }
                if let Some(snapshot) = self.snapshots.pop_front() {
                    return Some(snapshot);
                }
                self.quit_returned = true;
                return Some(UiAction::Quit);
            }
            if self.closing {
                notified.await;
                continue;
            }
            tokio::select! {biased;_= &mut notified=>{},action=self.normal.recv()=>return action}
        }
    }
}
impl Drop for ActionReceiver {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared
                .controls
                .lock()
                .expect("UI control mailbox poisoned")
                .alive = false;
        }
    }
}
/// Existing callers can continue using an ordinary Tokio receiver for shutdown.
pub trait ActionStream {
    fn next_action(&mut self) -> impl Future<Output = Option<UiAction>> + Send;
}
impl ActionStream for ActionReceiver {
    fn next_action(&mut self) -> impl Future<Output = Option<UiAction>> + Send {
        self.recv()
    }
}
impl ActionStream for mpsc::Receiver<UiAction> {
    fn next_action(&mut self) -> impl Future<Output = Option<UiAction>> + Send {
        self.recv()
    }
}
