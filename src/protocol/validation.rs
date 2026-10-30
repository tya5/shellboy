//! Argument-local validation. Command retains variant-dependent dispatch rules.
use super::*;

fn limit(value: u32, max: u32) -> Result<(), ApiError> {
    if value == 0 || value > max {
        Err(invalid(format!("limit must be in 1..={max}")))
    } else {
        Ok(())
    }
}
pub(crate) fn rfc3339(
    value: &str,
    error: impl FnOnce() -> ApiError,
) -> Result<chrono::DateTime<chrono::FixedOffset>, ApiError> {
    chrono::DateTime::parse_from_rfc3339(value).map_err(|_| error())
}

impl StatusArgs {
    fn validate(&self) -> Result<(), ApiError> {
        limit(self.limit, MAX_LIST_LIMIT)?;
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        optional_nonempty(self.run_id.as_deref(), "run_id")?;
        optional_nonempty(self.cursor.as_deref(), "cursor")?;
        if let Some(since) = &self.since {
            rfc3339(since, || invalid("since must be RFC3339 with timezone"))?;
        }
        if self.state.len() > 32 || self.state.iter().any(|s| s.is_empty() || s.len() > 64) {
            return Err(invalid(
                "state accepts at most 32 nonempty values of at most 64 bytes",
            ));
        }
        Ok(())
    }
}

impl ModelListArgs {
    fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.connection.as_deref(), "connection")?;
        optional_nonempty(self.thread_id.as_deref(), "thread_id")
    }
}

impl ThreadListArgs {
    fn validate(&self) -> Result<(), ApiError> {
        limit(self.limit, MAX_LIST_LIMIT)?;
        optional_nonempty(self.cursor.as_deref(), "cursor")
    }
}

impl ContextEditArgs {
    pub fn validate(&self) -> Result<(), ApiError> {
        for (name, value) in [
            ("thread_id", &self.thread_id),
            ("from_node_id", &self.from_node_id),
            ("to_node_id", &self.to_node_id),
        ] {
            optional_nonempty(value.as_deref(), name)?;
            if value.as_ref().is_some_and(|value| value.len() > 1024) {
                return Err(invalid(format!("{name} must not exceed 1024 bytes")));
            }
        }
        match self.action {
            ContextEditAction::Spill | ContextEditAction::Summary | ContextEditAction::Omit => {
                if self.from_node_id.is_none() || self.to_node_id.is_none() {
                    return Err(invalid(
                        "spill, summary and omit require both from_node_id and to_node_id",
                    ));
                }
            }
            ContextEditAction::Trim => {
                if self.from_node_id.is_some() || self.to_node_id.is_none() {
                    return Err(invalid(
                        "trim requires only to_node_id (inclusive removed prefix end)",
                    ));
                }
            }
            ContextEditAction::Restore => {
                if self.to_node_id.is_some() {
                    return Err(invalid(
                        "restore accepts only an optional from_node_id marker",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl ThreadCreateArgs {
    fn validate(&self) -> Result<(), ApiError> {
        for (name, value) in [
            ("connection", &self.connection),
            ("cwd", &self.cwd),
            ("agents", &self.agents),
            ("model", &self.model),
            ("parent_id", &self.parent_id),
        ] {
            optional_nonempty(value.as_deref(), name)?;
        }
        Ok(())
    }
}

impl ThreadForkArgs {
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.thread_id, "thread_id")?;
        nonempty(&self.node_id, "node_id")?;
        optional_nonempty(self.agents.as_deref(), "agents")?;
        optional_nonempty(self.cwd.as_deref(), "cwd")
    }
}

impl ThreadSetArgs {
    fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.connection.as_deref(), "connection")?;
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        optional_nonempty(self.cwd.as_deref(), "cwd")?;
        optional_nonempty(self.agents.as_deref(), "agents")?;
        optional_nonempty(self.model.as_deref(), "model")?;
        if let Some(title) = &self.title {
            if title
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
            {
                return Err(invalid("title must be one line without control characters"));
            }
            if title.trim().chars().count() > 512 {
                return Err(invalid("title must not exceed 512 Unicode characters"));
            }
        }
        if self.connection.is_none()
            && self.cwd.is_none()
            && self.model.is_none()
            && self.effort.is_none()
            && self.title.is_none()
            && self.agents.is_none()
        {
            Err(invalid("thread.set requires a change"))
        } else {
            Ok(())
        }
    }
}

impl WaitArgs {
    fn validate(&self) -> Result<(), ApiError> {
        if self.conditions.is_empty() {
            return Err(invalid("wait requires at least one condition"));
        }
        for condition in &self.conditions {
            match condition {
                WaitCondition::TurnCompleted { turn_id } => nonempty(turn_id, "turn_id")?,
                WaitCondition::RunEnded { run_id } => nonempty(run_id, "run_id")?,
                WaitCondition::ShellEnded { execution_id } => {
                    nonempty(execution_id, "execution_id")?
                }
                WaitCondition::ShellOutput {
                    execution_id,
                    cursor,
                } => {
                    nonempty(execution_id, "execution_id")?;
                    nonempty(cursor, "cursor")?;
                }
            }
        }
        Ok(())
    }
}

impl HistoryListArgs {
    pub(crate) fn validate_excerpt(&self) -> Result<(), ApiError> {
        if self.excerpt_chars > MAX_HISTORY_EXCERPT_CHARS {
            return Err(super::invalid("excerpt-chars must be in 0..=4096"));
        }
        Ok(())
    }
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.thread_id, "thread_id")?;
        limit(self.limit, MAX_LIST_LIMIT)?;
        optional_nonempty(self.cursor.as_deref(), "cursor")?;
        optional_nonempty(self.run_id.as_deref(), "run_id")?;
        optional_nonempty(self.since.as_deref(), "since")?;
        optional_nonempty(self.state.as_deref(), "state")?;
        self.validate_excerpt()?;
        self.validate_state()?;
        self.parsed_since().map(|_| ())
    }
    // Shared with the direct reader to preserve its native-state-before-time
    // precedence while moving rejection ahead of operation admission.
    pub(crate) fn validate_state(&self) -> Result<(), ApiError> {
        if self.state.as_deref().is_some_and(|state| {
            !matches!(
                state,
                "starting" | "running" | "stopping" | "exited" | "failed" | "stopped"
            )
        }) {
            return Err(super::invalid(
                "status must be a saved shell execution state: starting, running, stopping, exited, failed, stopped",
            ));
        }
        Ok(())
    }
    pub(crate) fn parsed_since(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::FixedOffset>>, ApiError> {
        self.since
            .as_deref()
            .map(|value| rfc3339(value, || invalid("since must be RFC3339 with timezone")))
            .transpose()
    }
}

impl HistoryGetArgs {
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.node_id, "node_id")?;
        nonempty(&self.encoding, "encoding")?;
        limit(self.limit_bytes, MAX_BODY_BYTES)?;
        optional_nonempty(self.cursor.as_deref(), "cursor")
    }
}

impl HistorySearchArgs {
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.query, "query")?;
        limit(self.limit, MAX_SEARCH_LIMIT)?;
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        for kinds in [&self.kinds, &self.exclude_kinds] {
            if kinds.len() > 32 {
                return Err(invalid("kind filters exceed 32 entries"));
            }
            for kind in kinds {
                nonempty(kind, "kind")?;
                if kind.chars().count() > 64 {
                    return Err(invalid("kind exceeds 64 characters"));
                }
            }
        }
        let parse = |value: &Option<String>| -> Result<_, ApiError> {
            value
                .as_ref()
                .map(|v| {
                    rfc3339(v, || {
                        invalid("time filters must be RFC3339 timestamps with offsets")
                    })
                })
                .transpose()
        };
        let from = parse(&self.from)?;
        let to = parse(&self.to)?;
        if from.zip(to).is_some_and(|(from, to)| from >= to) {
            return Err(invalid("from must precede to"));
        }
        Ok(())
    }
}

impl HistoryRelationsArgs {
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.node_id, "node_id")?;
        limit(self.limit, MAX_LIST_LIMIT)?;
        optional_nonempty(self.cursor.as_deref(), "cursor")
    }
}

impl ShellReadArgs {
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.execution_id, "execution_id")?;
        optional_nonempty(self.encoding.as_deref(), "encoding")?;
        limit(self.limit_bytes, MAX_BODY_BYTES)?;
        optional_nonempty(self.cursor.as_deref(), "cursor")
    }
}

impl ShellWriteArgs {
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.execution_id, "execution_id")?;
        nonempty(&self.encoding, "encoding")
    }
}

impl AttachmentAddArgs {
    fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        nonempty(&self.path, "path")
    }
}

impl AttachmentRemoveArgs {
    fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        nonempty(&self.attachment_id, "attachment_id")
    }
}

impl IndexArgs {
    fn validate_targets(&self) -> Result<(), ApiError> {
        optional_nonempty(self.space_id.as_deref(), "space_id")?;
        for id in self.thread_ids.iter().chain(self.source_ids.iter()) {
            nonempty(id, "index target")?;
        }
        Ok(())
    }
}
impl Command {
    pub fn validate(&self) -> Result<(), ApiError> {
        match self {
            Self::Status(a) => a.validate(),
            Self::ModelList(a) => a.validate(),
            Self::ThreadList(a) => a.validate(),
            Self::ThreadGet(a) | Self::AttachmentList(a) | Self::Compact(a) => {
                optional_nonempty(a.thread_id.as_deref(), "thread_id")
            }
            Self::ContextEdit(a) => a.validate(),
            Self::ThreadCreate(a) => a.validate(),
            Self::ThreadFork(a) => a.validate(),
            Self::ThreadDelete(a) | Self::ThreadDeletePreview(a) => {
                nonempty(&a.thread_id, "thread_id")
            }
            Self::ThreadSet(a) => a.validate(),
            Self::MessageSend(a) => optional_nonempty(a.thread_id.as_deref(), "thread_id"),
            Self::MessageGet(a) => nonempty(&a.message_id, "message_id"),
            Self::OperationGet(a) => nonempty(&a.operation_id, "operation_id"),
            Self::RunGet(a) | Self::RunRetry(a) | Self::RunStop(a) => nonempty(&a.run_id, "run_id"),
            Self::RevisitGet(a) | Self::RevisitCancel(a) => {
                optional_nonempty(a.run_id.as_deref(), "run_id")
            }
            Self::RevisitSet(a) => {
                optional_nonempty(a.run_id.as_deref(), "run_id")?;
                if a.text.len() > MAX_REVISIT_TEXT_BYTES {
                    return Err(ApiError::new(
                        ErrorCode::InputLimit,
                        "Revisit text exceeds 8192 UTF-8 bytes",
                    ));
                }
                nonempty(&a.text, "text")
            }
            Self::TurnGet(a) => nonempty(&a.turn_id, "turn_id"),
            Self::Wait(a) => a.validate(),
            Self::HistoryList(a) => a.validate(),
            Self::HistoryGet(a) => a.validate(),
            Self::HistorySearch(a) => a.validate(),
            Self::HistoryRelations(a) => a.validate(),
            Self::ShellGet(a) | Self::ShellEof(a) | Self::ShellStop(a) => {
                nonempty(&a.execution_id, "execution_id")
            }
            Self::ShellRead(a) => a.validate(),
            Self::ShellWrite(a) => a.validate(),
            Self::AttachmentAdd(a) => a.validate(),
            Self::AttachmentRemove(a) => a.validate(),
            Self::IndexStatus(a)
            | Self::IndexPreview(a)
            | Self::IndexBackfill(a)
            | Self::IndexRetry(a)
            | Self::IndexRebuild(a) => {
                a.validate_targets()?;
                if matches!(
                    self,
                    Self::IndexPreview(_) | Self::IndexBackfill(_) | Self::IndexRebuild(_)
                ) && a.scope.is_none()
                    && a.thread_ids.is_empty()
                    && a.source_ids.is_empty()
                {
                    return Err(invalid(
                        "index operation requires explicit target selection",
                    ));
                }
                Ok(())
            }
            Self::StorageStatus(_) | Self::StorageCollect(_) => Ok(()),
        }
    }
}
