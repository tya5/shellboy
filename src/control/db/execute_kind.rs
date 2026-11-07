//! Fixed command labels for DB jobs without capturing request data in diagnostics.
use crate::{
    protocol::Command,
    storage::{DbWorker, StorageError},
};
use rusqlite::Connection;
use std::any::Any;

type Output = Box<dyn Any + Send>;
type Operation = Box<dyn FnOnce(&mut Connection) -> Result<Output, StorageError> + Send>;

macro_rules! named_jobs {
    ($($variant:ident => $function:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(super) enum Kind { $($variant),+ }

        impl Kind {
            #[cfg(test)] const ALL: &'static [Kind] = &[$(Kind::$variant),+];
            pub(super) fn of(command: &Command) -> Self {
                match command {
                    $(Command::$variant(_) => Self::$variant,)+
                }
            }
        }

        $(async fn $function(db: &DbWorker, requester: Option<&'static str>, operation: Operation) -> Result<Output, StorageError> {
            db.call_with_requester(requester, move |connection| operation(connection)).await
        })+

        #[cfg(test)]
        pub(super) async fn call<T, F>(db: &DbWorker, kind: Kind, operation: F) -> Result<T, StorageError>
        where T: Send + 'static, F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static {
            call_with_requester(db, kind, None, operation).await
        }

        pub(super) async fn call_with_requester<T, F>(
            db: &DbWorker,
            kind: Kind,
            requester: Option<&'static str>,
            operation: F,
        ) -> Result<T, StorageError>
        where
            T: Send + 'static,
            F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
        {
            // Keep the named job's type independent of request-specific closure types.
            let boxed: Operation = Box::new(move |connection| {
                operation(connection).map(|value| Box::new(value) as Output)
            });
            let value = match kind {
                $(Kind::$variant => $function(db, requester, boxed).await,)+
            }?;
            Ok(*value.downcast::<T>().unwrap_or_else(|_| unreachable!("job output type")))
        }
    };
}

// The exhaustive match ensures every future protocol variant gets a fixed label.
named_jobs! {
    OwnerShutdown => owner_shutdown, Status => status, ModelList => model_list,
    ThreadList => thread_list, ThreadGet => thread_get, ThreadBranches => thread_branches, UsageGet => usage_get,
    CompactList => compact_list, CompactPreview => compact_preview, ShellList => shell_list,
    ThreadCreate => thread_create, ThreadFork => thread_fork, ThreadDelete => thread_delete,
    ThreadDeletePreview => thread_delete_preview, ThreadPrune => thread_prune, ThreadRewind => thread_rewind, ThreadArchive => thread_archive,
    ThreadUnarchive => thread_unarchive, ThreadSet => thread_set,
    MessageSend => message_send, MessageReply => message_reply, MessageCancel => message_cancel, MessageGet => message_get,
    OperationGet => operation_get, RunGet => run_get, RunRetry => run_retry, RunStop => run_stop,
    TurnGet => turn_get, Wait => wait, EventsWait => events_wait, HistoryList => history_list, HistoryGet => history_get,
    HistorySearch => history_search, HistoryRelations => history_relations, ShellGet => shell_get,
    ShellRead => shell_read, ShellWrite => shell_write, ShellEof => shell_eof, ShellStop => shell_stop,
    AttachmentAdd => attachment_add, AttachmentList => attachment_list, AttachmentRemove => attachment_remove,
    Compact => compact, ContextEdit => context_edit, IndexStatus => index_status,
    IndexPreview => index_preview, IndexBackfill => index_backfill, IndexRetry => index_retry,
    IndexRebuild => index_rebuild, IndexProbe => index_probe, IndexShrink => index_shrink, IndexRemove => index_remove, StorageStatus => storage_status,
    StorageCollect => storage_collect,
}

// Existing mutations awaiting the bounds audit in #200. This list must only shrink.
const UNDECLARED_WRITER_BOUNDS: &[Kind] = &[
    Kind::OwnerShutdown,
    Kind::ThreadCreate,
    Kind::ThreadFork,
    Kind::ThreadDelete,
    Kind::ThreadSet,
    Kind::MessageSend,
    Kind::MessageCancel,
    Kind::RunRetry,
    Kind::RunStop,
    Kind::AttachmentAdd,
    Kind::AttachmentRemove,
    Kind::Compact,
    Kind::ContextEdit,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Classification {
    PureRead,
    #[allow(dead_code)] // Reserved for audited bounds; #200 tracks the remaining mutations.
    MutationDeclaredBound,
    MutationExempt,
    Runtime,
}

impl Kind {
    fn classification(self) -> Classification {
        use Classification::*;
        match self {
            Kind::Status
            | Kind::ThreadList
            | Kind::ThreadGet
            | Kind::ThreadBranches
            | Kind::UsageGet
            | Kind::CompactList
            | Kind::CompactPreview
            | Kind::ShellList
            | Kind::ThreadDeletePreview
            | Kind::MessageGet
            | Kind::OperationGet
            | Kind::RunGet
            | Kind::TurnGet
            | Kind::HistoryList
            | Kind::HistoryGet
            | Kind::HistoryRelations
            | Kind::AttachmentList => PureRead,
            // Bounded closure (65,536 sources) in one transaction; see history::prune.
            // Rewind refuses paths over history::rewind::PATH_LIMIT nodes.
            // Archive: at most 200 IDs, descendants walk capped at 256 (control::archive).
            Kind::MessageReply
            | Kind::ThreadPrune
            | Kind::ThreadRewind
            | Kind::ThreadArchive
            | Kind::ThreadUnarchive => MutationDeclaredBound,
            Kind::OwnerShutdown
            | Kind::ThreadCreate
            | Kind::ThreadFork
            | Kind::ThreadDelete
            | Kind::ThreadSet
            | Kind::MessageSend
            | Kind::MessageCancel
            | Kind::RunRetry
            | Kind::RunStop
            | Kind::AttachmentAdd
            | Kind::AttachmentRemove
            | Kind::Compact
            | Kind::ContextEdit => MutationExempt,
            Kind::ModelList
            | Kind::Wait
            | Kind::EventsWait
            | Kind::HistorySearch
            | Kind::ShellGet
            | Kind::ShellRead
            | Kind::ShellWrite
            | Kind::ShellEof
            | Kind::ShellStop
            | Kind::IndexStatus
            | Kind::IndexPreview
            | Kind::IndexBackfill
            | Kind::IndexRetry
            | Kind::IndexRebuild
            | Kind::IndexProbe
            | Kind::IndexShrink
            | Kind::IndexRemove
            | Kind::StorageStatus
            | Kind::StorageCollect => Runtime,
        }
    }
    pub(super) fn read_lane(self) -> bool {
        self.classification() == Classification::PureRead
    }
    fn declared_writer_bound(self) -> bool {
        self.classification() == Classification::MutationDeclaredBound
    }
    pub(super) fn writer_allowed(self, command: &Command) -> bool {
        !command.is_read_only()
            && (self.declared_writer_bound()
                || (self.classification() == Classification::MutationExempt
                    && UNDECLARED_WRITER_BOUNDS.contains(&self)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_bound_exemptions_are_unique_and_not_declared() {
        for (i, kind) in UNDECLARED_WRITER_BOUNDS.iter().enumerate() {
            assert!(
                !kind.declared_writer_bound(),
                "remove bound-declared kind from exemptions"
            );
            assert!(
                !UNDECLARED_WRITER_BOUNDS[..i].contains(kind),
                "duplicate exemption"
            );
        }
        assert_eq!(
            Kind::ALL.len(),
            54,
            "classify new kinds and update this ratchet"
        );
    }

    #[test]
    fn every_writer_kind_has_a_bound_or_a_ratchet_exemption() {
        let writers = [
            Kind::MessageReply,
            Kind::ThreadPrune,
            Kind::ThreadRewind,
            Kind::ThreadArchive,
            Kind::ThreadUnarchive,
            Kind::OwnerShutdown,
            Kind::ThreadCreate,
            Kind::ThreadFork,
            Kind::ThreadDelete,
            Kind::ThreadSet,
            Kind::MessageSend,
            Kind::MessageCancel,
            Kind::RunRetry,
            Kind::RunStop,
            Kind::AttachmentAdd,
            Kind::AttachmentRemove,
            Kind::Compact,
            Kind::ContextEdit,
        ];
        for kind in writers {
            assert!(
                kind.declared_writer_bound() || UNDECLARED_WRITER_BOUNDS.contains(&kind),
                "writer kind {kind:?} has neither a bound nor a ratchet exemption"
            );
        }
        for kind in UNDECLARED_WRITER_BOUNDS {
            assert!(
                writers.contains(kind),
                "exemption is not a writer kind: {kind:?}"
            );
            assert!(
                !kind.declared_writer_bound(),
                "remove bounded kind from exemptions: {kind:?}"
            );
        }
    }

    #[test]
    fn exhaustive_read_routes_and_exemptions_match_checked_in_list() {
        let read_command = Command::Status(Default::default());
        let mutation: Command = serde_json::from_value(
            serde_json::json!({"command":"message.send","args":{"text":"test"}}),
        )
        .unwrap();
        let expected = [
            Kind::OwnerShutdown,
            Kind::ThreadCreate,
            Kind::ThreadFork,
            Kind::ThreadDelete,
            Kind::ThreadSet,
            Kind::MessageSend,
            Kind::MessageCancel,
            Kind::RunRetry,
            Kind::RunStop,
            Kind::AttachmentAdd,
            Kind::AttachmentRemove,
            Kind::Compact,
            Kind::ContextEdit,
        ];
        let actual: Vec<_> = Kind::ALL
            .iter()
            .copied()
            .filter(|k| k.classification() == Classification::MutationExempt)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(UNDECLARED_WRITER_BOUNDS, expected);
        for kind in Kind::ALL {
            match kind.classification() {
                Classification::PureRead => {
                    assert!(kind.read_lane());
                    assert!(!kind.writer_allowed(&read_command));
                    assert!(!kind.writer_allowed(&mutation));
                }
                Classification::MutationDeclaredBound | Classification::MutationExempt => {
                    assert!(!kind.read_lane())
                }
                Classification::Runtime => {
                    assert!(!kind.read_lane());
                    assert!(!kind.writer_allowed(&mutation));
                }
            }
        }
    }

    #[test]
    fn reply_declares_its_recipient_scoped_batch_without_growing_exemptions() {
        let command: Command = serde_json::from_value(serde_json::json!({"command":"message.reply","args":{"message_id":"synthetic","text":"reply"}})).unwrap();
        assert_eq!(Kind::of(&command), Kind::MessageReply);
        assert!(Kind::MessageReply.declared_writer_bound());
        assert!(Kind::MessageReply.writer_allowed(&command));
        assert!(!UNDECLARED_WRITER_BOUNDS.contains(&Kind::MessageReply));
    }

    #[test]
    fn pure_read_is_never_allowed_on_writer() {
        let command = Command::Status(Default::default());
        assert!(command.is_read_only());
        for kind in Kind::ALL {
            assert!(
                !kind.writer_allowed(&command),
                "read was admitted as {kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn labeled_jobs_preserve_values_and_storage_errors() {
        let root = tempfile::tempdir().unwrap();
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        let value = call(&db, Kind::MessageSend, |_| Ok(vec![1_u8, 2, 3]))
            .await
            .unwrap();
        assert_eq!(value, vec![1, 2, 3]);
        let error = call::<(), _>(&db, Kind::Status, |_| Err(StorageError::QueueFull))
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::QueueFull));
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn slow_jobs_name_commands_without_captured_data() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = crate::diagnostics::Diagnostics::start(
            root.path().join("logs"),
            crate::config::Logging::default(),
        );
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        db.attach_diagnostics(diagnostics.clone());
        let commands: Vec<Command> = vec![
            serde_json::from_value(serde_json::json!({
                "command": "message.send", "args": {"text": "PRIVATE REQUEST BODY AND ID"}
            }))
            .unwrap(),
            Command::Status(Default::default()),
        ];
        for command in commands {
            let kind = Kind::of(&command);
            let private = "PRIVATE REQUEST BODY AND ID".to_owned();
            call(&db, kind, move |_| {
                assert!(!private.is_empty());
                std::thread::sleep(std::time::Duration::from_millis(1100));
                Ok(())
            })
            .await
            .unwrap();
        }
        db.shutdown().await.unwrap();
        diagnostics.shutdown().await;
        let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
        assert!(!log.contains("PRIVATE"));
        let kinds: Vec<String> = log
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(row["ids"], serde_json::json!({}));
                assert!(row["db_job_slow"]["run_ms"].as_u64().unwrap() >= 1000);
                assert!(row["db_job_slow"]["queue_ms"].is_u64());
                row["db_job_slow"]["kind"].as_str().unwrap().to_owned()
            })
            .collect();
        for name in [
            "control.db.execute_kind.message_send",
            "control.db.execute_kind.status",
        ] {
            assert!(
                kinds.iter().any(|kind| kind == name),
                "missing {name}: {kinds:?}"
            );
        }
    }
}
