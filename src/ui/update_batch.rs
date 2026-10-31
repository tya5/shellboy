//! Synchronous UI notification admission; no I/O or waiting for a producer.
use super::*;
const MAX_UPDATES: usize = 32;
const UPDATE_BUDGET: Duration = Duration::from_millis(4);
impl UiUpdate {
    pub(super) fn needs_retention(&self) -> bool {
        // Exhaustive: new update kinds must choose their cache responsibility.
        match self {
            Self::CurrentView { .. }
            | Self::ToolPreview { .. }
            | Self::ToolFacts { .. }
            | Self::HistoryWindow { .. }
            | Self::History { .. }
            | Self::StreamCommitted { .. }
            | Self::Stream { .. }
            | Self::StreamSettled { .. }
            | Self::IncompleteResponses { .. }
            | Self::ViewState { .. }
            | Self::NodeWindow { .. }
            | Self::EditRequestLoaded { .. }
            | Self::SavedMessage { .. } => true,
            // Scoped updates delegate to apply, which owns the inner effect.
            Self::ProgramStatus(_)
            | Self::LiveWorkObserved { .. }
            | Self::LiveWorkPreview { .. }
            | Self::DetailSourcesChanged { .. }
            | Self::DetailLinks { .. }
            | Self::SendOperation { .. }
            | Self::DraftRecovery { .. }
            | Self::RestoreArchives { .. }
            | Self::SavedMessagePending { .. }
            | Self::AgentsSelected { .. }
            | Self::ThreadDeletion { .. }
            | Self::ScopedManagement { .. }
            | Self::RecoveryBell
            | Self::CommandOutcome { .. }
            | Self::Activity { .. }
            | Self::ActivityUnavailable { .. }
            | Self::ModelReceive { .. }
            | Self::Models { .. }
            | Self::RecalledInput { .. }
            | Self::OriginalPage { .. }
            | Self::ReceivedList { .. }
            | Self::StructuredManagement { .. }
            | Self::ManagementDetail { .. }
            | Self::Initialize { .. }
            | Self::ThreadListFailed { .. }
            | Self::Threads { .. }
            | Self::Thread { .. }
            | Self::StorageFailed { .. }
            | Self::DraftSaveFailed { .. }
            | Self::ReceiptSource { .. }
            | Self::RestoredSnapshot { .. }
            | Self::LoadFailed { .. }
            | Self::HistoryInvalidated { .. }
            | Self::Receipt { .. }
            | Self::HistoryRebaseNotice { .. }
            | Self::DraftStatus { .. }
            | Self::TerminalRequest { .. }
            | Self::Status { .. }
            | Self::ClipboardAttachment { .. }
            | Self::Progress { .. }
            | Self::EditConfirmationRequired { .. }
            | Self::Management { .. }
            | Self::IndexPreviewReady { .. }
            | Self::IndexPreviewStarted { .. }
            | Self::IndexPreviewFailed { .. }
            | Self::ShutdownProgress { .. }
            | Self::ForceShutdown
            | Self::Shutdown => false,
        }
    }
}
impl UiState {
    pub(super) fn finish_retention(&mut self) {
        if std::mem::take(&mut self.retention_pending) {
            self.trim_bodies();
        }
    }
    pub(super) fn drain_updates(&mut self, updates: &Receiver<UiUpdate>) {
        self.drain_update_turn(updates, UPDATE_BUDGET);
    }
    fn drain_update_turn(&mut self, updates: &Receiver<UiUpdate>, budget: Duration) -> usize {
        let started = Instant::now();
        self.batching_updates = true;
        let mut applied = 0;
        for _ in 0..MAX_UPDATES {
            // Always make progress, then return an input opportunity. An
            // individual bounded projection cannot be preempted midway.
            if applied != 0 && started.elapsed() >= budget {
                break;
            }
            match updates.try_recv() {
                Ok(update) => {
                    self.apply(update);
                    applied += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.lifecycle.stop(false);
                    break;
                }
            }
        }
        self.batching_updates = false;
        self.finish_retention();
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn history(ui: &UiState, sequence: usize) -> UiUpdate {
        UiUpdate::History {
            pending: Default::default(),
            read_revision: 0,
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes: vec![UiNode {
                node_id: format!("node-{sequence}"),
                text: "日本語 body".repeat(100),
                role: "assistant".into(),
                total_chars: 1000,
                ..Default::default()
            }],
            before_cursor: None,
            append: false,
            first_ordinal: sequence as u64,
            selection_revision: 0,
            total_nodes: sequence as u64 + 1,
        }
    }
    #[test]
    fn status_flood_keeps_input_opportunities_without_any_history_retention_scan() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let before = ui.retention_passes;
        let (send, recv) = std::sync::mpsc::sync_channel(128);
        for i in 0..100 {
            send.send(UiUpdate::Status {
                thread_id: None,
                text: i.to_string(),
            })
            .unwrap();
        }
        assert_eq!(ui.drain_update_turn(&recv, Duration::MAX), 32);
        ui.insert("input between notifications", false);
        assert_eq!(ui.view().draft.text, "input between notifications");
        assert_eq!(ui.retention_passes, before);
        assert_eq!(ui.drain_update_turn(&recv, Duration::MAX), 32);
        assert_eq!(ui.view().status, "63");
        assert_eq!(ui.view().draft.text, "input between notifications");
        assert_eq!(ui.retention_passes, before);
    }
    #[test]
    fn body_batch_trims_once_and_single_apply_keeps_the_immediate_budget_contract() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let (send, recv) = std::sync::mpsc::sync_channel(128);
        for i in 0..64 {
            send.send(history(&ui, i)).unwrap();
        }
        let before = ui.retention_passes;
        assert_eq!(ui.drain_update_turn(&recv, Duration::MAX), 32);
        assert_eq!(ui.retention_passes, before + 1);
        assert_eq!(ui.view().nodes()[0].node_id, "node-31");
        ui.apply(history(&ui, 100));
        assert_eq!(ui.retention_passes, before + 2);
        assert!(!ui.batching_updates && !ui.retention_pending);
    }
    #[test]
    fn inactive_payloads_are_trimmed_before_the_batch_returns_an_input_opportunity() {
        let mut config = Config::default();
        config.cache.body_bytes = 0;
        let mut ui = UiState::new(&config, std::env::temp_dir());
        ui.select_thread("thread".into());
        let mut notifications = Vec::new();
        for i in 0..32 {
            notifications.push(history(&ui, i));
        }
        ui.select_thread("active".into());
        let (send, recv) = std::sync::mpsc::sync_channel(32);
        for update in notifications {
            send.send(update).unwrap();
        }
        let before = ui.retention_passes;
        assert_eq!(ui.drain_update_turn(&recv, Duration::MAX), 32);
        assert_eq!(ui.retention_passes, before + 1);
        let inactive = ui
            .views
            .values()
            .find(|view| view.thread_id.as_deref() == Some("thread"))
            .unwrap();
        assert_eq!(inactive.retained_body_bytes(&[]), 0);
        ui.insert("input after bounded retention", false);
        assert_eq!(ui.view().draft.text, "input after bounded retention");
    }
    #[test]
    fn elapsed_budget_yields_after_one_update_and_closed_input_still_stops() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        let (send, recv) = std::sync::mpsc::sync_channel(2);
        send.send(UiUpdate::Status {
            thread_id: None,
            text: "one".into(),
        })
        .unwrap();
        send.send(UiUpdate::Status {
            thread_id: None,
            text: "two".into(),
        })
        .unwrap();
        assert_eq!(ui.drain_update_turn(&recv, Duration::ZERO), 1);
        assert_eq!(ui.view().status, "one");
        drop(send);
        ui.drain_update_turn(&recv, Duration::MAX);
        assert_eq!(ui.view().status, "two");
        assert!(ui.is_stopped());
    }
}

#[cfg(test)]
mod committed_delivery_tests {
    use super::*;
    #[test]
    fn commit_at_the_update_boundary_keeps_paid_text_until_canonical_delivery() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        ui.apply(UiUpdate::Stream {
            thread_id: "thread".into(),
            generation,
            node_id: "attempt".into(),
            role: "assistant".into(),
            text: "paid intermediate text".into(),
            base_char_offset: 0,
            total_chars: 22,
            reasoning_ranges: vec![],
            presentation: Default::default(),
            ack: None,
        });
        let (send, recv) = std::sync::mpsc::sync_channel(64);
        for _ in 0..31 {
            send.send(UiUpdate::Status {
                thread_id: None,
                text: "unrelated".into(),
            })
            .unwrap();
        }
        send.send(UiUpdate::StreamCommitted {
            thread_id: "thread".into(),
            generation,
            attempt_id: "attempt".into(),
            node_id: "saved".into(),
        })
        .unwrap();
        send.send(UiUpdate::History {
            pending: Default::default(),
            read_revision: 0,
            thread_id: "thread".into(),
            generation,
            nodes: vec![UiNode {
                node_id: "saved".into(),
                role: "assistant".into(),
                text: "canonical final text".into(),
                total_chars: 20,
                ..Default::default()
            }],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        })
        .unwrap();
        assert_eq!(ui.drain_update_turn(&recv, Duration::MAX), 32);
        assert_eq!(ui.view().nodes()[0].text, "paid intermediate text");
        let before = ui.history_layout(80, 20);
        assert!(before.text.contains("paid intermediate text"));
        assert_eq!(ui.drain_update_turn(&recv, Duration::MAX), 1);
        assert_eq!(ui.view().nodes().len(), 1);
        assert_eq!(ui.view().nodes()[0].node_id, "saved");
        let after = ui.history_layout(80, 20);
        assert!(after.text.contains("canonical final text"));
        assert!(!after.text.contains("paid intermediate text"));
    }
}

#[cfg(test)]
mod committed_source_tests {
    use super::*;
    #[test]
    fn metadata_only_history_and_late_settled_keep_paid_source_until_body_arrives() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        ui.apply(UiUpdate::Stream {
            thread_id: "thread".into(),
            generation,
            node_id: "attempt".into(),
            role: "assistant".into(),
            text: "paid output".into(),
            base_char_offset: 0,
            total_chars: 11,
            reasoning_ranges: vec![],
            presentation: Default::default(),
            ack: None,
        });
        ui.apply(UiUpdate::StreamCommitted {
            thread_id: "thread".into(),
            generation,
            attempt_id: "attempt".into(),
            node_id: "saved".into(),
        });
        ui.apply(UiUpdate::StreamSettled {
            thread_id: "thread".into(),
            generation,
            attempt_id: "attempt".into(),
            received_source: None,
            ack: None,
        });
        ui.apply(UiUpdate::History {
            pending: Default::default(),
            read_revision: 0,
            thread_id: "thread".into(),
            generation,
            nodes: vec![UiNode {
                node_id: "saved".into(),
                total_chars: 20,
                ..Default::default()
            }],
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        });
        assert!(
            ui.view()
                .nodes()
                .iter()
                .any(|node| node.text == "paid output")
        );
        ui.active_view_mut()
            .on_load_failed("body unavailable".into());
        assert!(ui.history_layout(80, 20).text.contains("paid output"));
        // The next canonical body may arrive from a source-window request,
        // independently of another full history page.
        let revision = ui.view().view_revision;
        ui.active_view_mut().on_node_window(
            revision,
            UiNode {
                node_id: "saved".into(),
                role: "assistant".into(),
                text: "canonical final text".into(),
                total_chars: 20,
                ..Default::default()
            },
        );
        assert_eq!(ui.view().nodes().len(), 1);
        assert_eq!(ui.view().nodes()[0].node_id, "saved");
        assert!(
            ui.history_layout(80, 20)
                .text
                .contains("canonical final text")
        );
    }
}
