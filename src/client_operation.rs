//! Typed standalone-client requests from the UI; never Core protocol commands.

#[derive(Clone, Debug)]
pub enum ClientOperation {
    Docs(crate::docs::DocsCommand),
    Config(crate::config_setup::ConfigCommand),
    Rag {
        options: crate::rag::RagOptions,
        owned_stdin: Option<String>,
    },
    Exec(crate::exec_client::ExecOptions),
}

impl ClientOperation {
    pub(crate) fn catalogue(&self) -> crate::operations::OperationId {
        use crate::{
            config_setup::ConfigCommand,
            docs::DocsCommand,
            operations::OperationId as Id,
            rag::{RagCommand, SourceCommand},
        };
        match self {
            Self::Docs(DocsCommand::List { .. }) => Id::DocsList,
            Self::Docs(DocsCommand::Get { .. }) => Id::DocsGet,
            Self::Docs(DocsCommand::Search { .. }) => Id::DocsSearch,
            Self::Config(ConfigCommand::Init) => Id::ConfigInit,
            Self::Config(ConfigCommand::Check) => Id::ConfigCheck,
            Self::Exec(_) => Id::Exec,
            Self::Rag { options, .. } => match &options.command {
                RagCommand::Init => Id::RagInit,
                RagCommand::Source {
                    command: SourceCommand::Add { .. },
                } => Id::RagSourceAdd,
                RagCommand::Source {
                    command: SourceCommand::List,
                } => Id::RagSourceList,
                RagCommand::Source {
                    command: SourceCommand::Remove { .. },
                } => Id::RagSourceRemove,
                RagCommand::Index { .. } => Id::RagIndex,
                RagCommand::Ingest { .. } => Id::RagIngest,
                RagCommand::Status => Id::RagStatus,
                RagCommand::Inspect { .. } => Id::RagInspect,
                RagCommand::Search { .. } => Id::RagSearch,
                RagCommand::Get { .. } => Id::RagGet,
            },
        }
    }

    pub(crate) fn effect(&self) -> crate::operations::Effect {
        if let Self::Rag {
            options:
                crate::rag::RagOptions {
                    command: crate::rag::RagCommand::Index { dry_run, .. },
                    ..
                },
            ..
        } = self
        {
            crate::operations::rag_index_effect(*dry_run)
        } else {
            self.catalogue().spec().effect
        }
    }
}
