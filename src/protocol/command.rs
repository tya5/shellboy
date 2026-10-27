//! Command names and independent lifecycle classifications; no dispatch engine.
use super::Command;

/// Dispatch ownership only; independent from mutation and receipt semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandRoute {
    Database,
    Runtime,
    StatusProjection,
}
impl Command {
    fn name_and_route(&self) -> (&'static str, CommandRoute) {
        match self {
            Self::Status(_) => ("status", CommandRoute::StatusProjection),
            Self::ModelList(_) => ("model.list", CommandRoute::Runtime),
            Self::ThreadList(_) => ("thread.list", CommandRoute::Database),
            Self::ThreadGet(_) => ("thread.get", CommandRoute::Database),
            Self::ThreadCreate(_) => ("thread.create", CommandRoute::Database),
            Self::ThreadFork(_) => ("thread.fork", CommandRoute::Database),
            Self::ThreadDelete(_) => ("thread.delete", CommandRoute::Database),
            Self::ThreadDeletePreview(_) => ("thread.delete_preview", CommandRoute::Database),
            Self::ThreadSet(_) => ("thread.set", CommandRoute::Database),
            Self::MessageSend(_) => ("message.send", CommandRoute::Database),
            Self::MessageGet(_) => ("message.get", CommandRoute::Database),
            Self::OperationGet(_) => ("operation.get", CommandRoute::Database),
            Self::RunGet(_) => ("run.get", CommandRoute::Database),
            Self::RunRetry(_) => ("run.retry", CommandRoute::Database),
            Self::RunStop(_) => ("run.stop", CommandRoute::Database),
            Self::TurnGet(_) => ("turn.get", CommandRoute::Database),
            Self::Wait(_) => ("wait", CommandRoute::Runtime),
            Self::HistoryList(_) => ("history.list", CommandRoute::Database),
            Self::HistoryGet(_) => ("history.get", CommandRoute::Database),
            Self::HistorySearch(_) => ("history.search", CommandRoute::Runtime),
            Self::HistoryRelations(_) => ("history.relations", CommandRoute::Database),
            Self::ShellGet(_) => ("shell.get", CommandRoute::Runtime),
            Self::ShellRead(_) => ("shell.read", CommandRoute::Runtime),
            Self::ShellWrite(_) => ("shell.write", CommandRoute::Runtime),
            Self::ShellEof(_) => ("shell.eof", CommandRoute::Runtime),
            Self::ShellStop(_) => ("shell.stop", CommandRoute::Runtime),
            Self::AttachmentAdd(_) => ("attachment.add", CommandRoute::Database),
            Self::AttachmentList(_) => ("attachment.list", CommandRoute::Database),
            Self::AttachmentRemove(_) => ("attachment.remove", CommandRoute::Database),
            Self::Compact(_) => ("compact", CommandRoute::Database),
            Self::ContextEdit(_) => ("context.edit", CommandRoute::Database),
            Self::IndexStatus(_) => ("index.status", CommandRoute::Runtime),
            Self::IndexPreview(_) => ("index.preview", CommandRoute::Runtime),
            Self::IndexBackfill(_) => ("index.backfill", CommandRoute::Runtime),
            Self::IndexRetry(_) => ("index.retry", CommandRoute::Runtime),
            Self::IndexRebuild(_) => ("index.rebuild", CommandRoute::Runtime),
            Self::StorageStatus(_) => ("storage.status", CommandRoute::Runtime),
            Self::StorageCollect(_) => ("storage.collect", CommandRoute::Runtime),
        }
    }
    /// Canonical protocol name, independent from translated UI titles.
    pub fn name(&self) -> &'static str {
        self.name_and_route().0
    }
    pub(crate) fn route(&self) -> CommandRoute {
        self.name_and_route().1
    }
    /// Observations are evaluated afresh and never consume durable receipts.
    /// Wait remains stateful: it owns registration and changes caller activity.
    pub(crate) fn requires_receipt(&self) -> bool {
        !self.is_read_only()
    }
    /// May owner shutdown drop the runtime action before receipt finalization?
    pub(crate) fn runtime_cancellable(&self) -> bool {
        match self {
            Self::ModelList(_)
            | Self::IndexPreview(_)
            | Self::HistorySearch(_)
            | Self::ShellGet(_)
            | Self::ShellRead(_)
            | Self::IndexBackfill(_)
            | Self::IndexRetry(_)
            | Self::IndexRebuild(_)
            | Self::StorageStatus(_)
            | Self::StorageCollect(_) => true,
            Self::Status(_)
            | Self::ThreadList(_)
            | Self::ThreadGet(_)
            | Self::ThreadCreate(_)
            | Self::ThreadFork(_)
            | Self::ThreadDelete(_)
            | Self::ThreadDeletePreview(_)
            | Self::ThreadSet(_)
            | Self::MessageSend(_)
            | Self::MessageGet(_)
            | Self::OperationGet(_)
            | Self::RunGet(_)
            | Self::RunRetry(_)
            | Self::RunStop(_)
            | Self::TurnGet(_)
            | Self::Wait(_)
            | Self::HistoryList(_)
            | Self::HistoryGet(_)
            | Self::HistoryRelations(_)
            | Self::ShellWrite(_)
            | Self::ShellEof(_)
            | Self::ShellStop(_)
            | Self::AttachmentAdd(_)
            | Self::AttachmentList(_)
            | Self::AttachmentRemove(_)
            | Self::Compact(_)
            | Self::ContextEdit(_)
            | Self::IndexStatus(_) => false,
        }
    }
    /// Late runtime cleanup allowed after cancellation; not the IPC reserved lane.
    pub(crate) fn runtime_allowed_while_stopping(&self) -> bool {
        match self {
            Self::ShellEof(_) | Self::ShellStop(_) => true,
            Self::Status(_)
            | Self::ModelList(_)
            | Self::ThreadList(_)
            | Self::ThreadGet(_)
            | Self::ThreadCreate(_)
            | Self::ThreadFork(_)
            | Self::ThreadDelete(_)
            | Self::ThreadDeletePreview(_)
            | Self::ThreadSet(_)
            | Self::MessageSend(_)
            | Self::MessageGet(_)
            | Self::OperationGet(_)
            | Self::RunGet(_)
            | Self::RunRetry(_)
            | Self::RunStop(_)
            | Self::TurnGet(_)
            | Self::Wait(_)
            | Self::HistoryList(_)
            | Self::HistoryGet(_)
            | Self::HistorySearch(_)
            | Self::HistoryRelations(_)
            | Self::ShellGet(_)
            | Self::ShellRead(_)
            | Self::ShellWrite(_)
            | Self::AttachmentAdd(_)
            | Self::AttachmentList(_)
            | Self::AttachmentRemove(_)
            | Self::Compact(_)
            | Self::ContextEdit(_)
            | Self::IndexStatus(_)
            | Self::IndexPreview(_)
            | Self::IndexBackfill(_)
            | Self::IndexRetry(_)
            | Self::IndexRebuild(_)
            | Self::StorageStatus(_)
            | Self::StorageCollect(_) => false,
        }
    }
    /// Must finish admission before the graceful-shutdown context capture?
    pub(crate) fn changes_context(&self) -> bool {
        match self {
            Self::ThreadCreate(_)
            | Self::ThreadFork(_)
            | Self::ThreadDelete(_)
            | Self::ThreadSet(_)
            | Self::MessageSend(_)
            | Self::RunRetry(_)
            | Self::RunStop(_)
            | Self::AttachmentAdd(_)
            | Self::AttachmentRemove(_)
            | Self::Compact(_)
            | Self::ContextEdit(_) => true,
            Self::Status(_)
            | Self::ModelList(_)
            | Self::ThreadList(_)
            | Self::ThreadGet(_)
            | Self::ThreadDeletePreview(_)
            | Self::MessageGet(_)
            | Self::OperationGet(_)
            | Self::RunGet(_)
            | Self::TurnGet(_)
            | Self::Wait(_)
            | Self::HistoryList(_)
            | Self::HistoryGet(_)
            | Self::HistorySearch(_)
            | Self::HistoryRelations(_)
            | Self::ShellGet(_)
            | Self::ShellRead(_)
            | Self::ShellWrite(_)
            | Self::ShellEof(_)
            | Self::ShellStop(_)
            | Self::AttachmentList(_)
            | Self::IndexStatus(_)
            | Self::IndexPreview(_)
            | Self::IndexBackfill(_)
            | Self::IndexRetry(_)
            | Self::IndexRebuild(_)
            | Self::StorageStatus(_)
            | Self::StorageCollect(_) => false,
        }
    }
    /// Observes targets without changing their state; receipt metadata and API cost are separate.
    pub fn is_read_only(&self) -> bool {
        match self {
            Self::Status(_)
            | Self::ModelList(_)
            | Self::ThreadList(_)
            | Self::ThreadGet(_)
            | Self::ThreadDeletePreview(_)
            | Self::MessageGet(_)
            | Self::OperationGet(_)
            | Self::RunGet(_)
            | Self::TurnGet(_)
            | Self::HistoryList(_)
            | Self::HistoryGet(_)
            | Self::HistorySearch(_)
            | Self::HistoryRelations(_)
            | Self::ShellGet(_)
            | Self::ShellRead(_)
            | Self::AttachmentList(_)
            | Self::IndexStatus(_)
            | Self::IndexPreview(_)
            | Self::StorageStatus(_) => true,
            Self::ThreadCreate(_)
            | Self::ThreadFork(_)
            | Self::ThreadDelete(_)
            | Self::ThreadSet(_)
            | Self::MessageSend(_)
            | Self::RunRetry(_)
            | Self::RunStop(_)
            | Self::Wait(_)
            | Self::ShellWrite(_)
            | Self::ShellEof(_)
            | Self::ShellStop(_)
            | Self::AttachmentAdd(_)
            | Self::AttachmentRemove(_)
            | Self::Compact(_)
            | Self::ContextEdit(_)
            | Self::IndexBackfill(_)
            | Self::IndexRetry(_)
            | Self::IndexRebuild(_)
            | Self::StorageCollect(_) => false,
        }
    }
}
