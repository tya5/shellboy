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
        if self.memory
            && (self.thread_id.is_some()
                || self.run_id.is_some()
                || self.since.is_some()
                || !self.state.is_empty()
                || self.all
                || self.cursor.is_some()
                || self.limit != DEFAULT_LIST_LIMIT)
        {
            return Err(invalid(
                "status memory is owner-wide and cannot use activity filters",
            ));
        }
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
        optional_nonempty(self.cursor.as_deref(), "cursor")?;
        optional_nonempty(self.cwd.as_deref(), "cwd")?;
        if self.tags.len() > MAX_THREAD_TAGS {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                format!("thread list accepts at most {MAX_THREAD_TAGS} tag filters"),
            ));
        }
        for tag in &self.tags {
            normalize_tag(tag)?;
        }
        Ok(())
    }
}

impl ThreadTagsArgs {
    fn validate(&self) -> Result<(), ApiError> {
        limit(self.limit, MAX_LIST_LIMIT)?;
        optional_nonempty(self.cursor.as_deref(), "cursor")
    }
}

impl ThreadCwdsArgs {
    fn validate(&self) -> Result<(), ApiError> {
        limit(self.limit, MAX_LIST_LIMIT)?;
        optional_nonempty(self.cursor.as_deref(), "cursor")
    }
}

/// Most tags one Thread may carry, and the most tags one call may add, remove or filter by.
pub(crate) const MAX_THREAD_TAGS: usize = 64;
/// Longest tag in Unicode scalar values.
pub(crate) const MAX_TAG_CHARS: usize = 128;

/// A tag is a title-like label: trimmed, one line, no control characters, 1..=128 scalars.
pub(crate) fn normalize_tag(raw: &str) -> Result<String, ApiError> {
    let tag = raw.trim();
    if tag.is_empty() {
        return Err(invalid("tag must not be empty"));
    }
    if tag
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    {
        return Err(invalid("tag must be one line without control characters"));
    }
    if tag.chars().count() > MAX_TAG_CHARS {
        return Err(invalid(format!(
            "tag must not exceed {MAX_TAG_CHARS} Unicode characters"
        )));
    }
    Ok(tag.to_owned())
}

impl ThreadTagArgs {
    fn validate(&self) -> Result<(), ApiError> {
        if self.thread_ids.len() > MAX_ARCHIVE_THREADS {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                format!("thread tag accepts at most {MAX_ARCHIVE_THREADS} Thread IDs"),
            ));
        }
        for id in &self.thread_ids {
            nonempty(id, "thread_id")?;
        }
        if self.add.is_empty() && self.remove.is_empty() {
            return Err(invalid(
                "thread.tag requires at least one tag to add or remove",
            ));
        }
        if self.add.len() > MAX_THREAD_TAGS || self.remove.len() > MAX_THREAD_TAGS {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                format!("thread tag accepts at most {MAX_THREAD_TAGS} tags to add and to remove"),
            ));
        }
        let added = self
            .add
            .iter()
            .map(|tag| normalize_tag(tag))
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        for tag in &self.remove {
            if added.contains(&normalize_tag(tag)?) {
                return Err(invalid(
                    "a tag cannot be both added and removed in one call",
                ));
            }
        }
        Ok(())
    }
}

impl ContextEditArgs {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.max_chars.is_some() || self.max_percent.is_some() {
            if self.action != ContextEditAction::Summary {
                return Err(invalid("summary limits require the summary action"));
            }
            if !(crate::config::Compaction {
                summary_max_chars: self.max_chars,
                summary_max_percent: self.max_percent,
                ..Default::default()
            })
            .valid()
            {
                return Err(invalid(
                    "summary limits require positive characters and/or a finite percent in (0,100]",
                ));
            }
        }
        if self.segment_chars.is_some() || self.reduce_rounds.is_some() {
            if self.action != ContextEditAction::Summary {
                return Err(invalid("summary parameters require the summary action"));
            }
            check_summary_parameters(self.segment_chars, self.reduce_rounds)?;
        }
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
            ContextEditAction::Maximal => {
                if self.from_node_id.is_some() || self.to_node_id.is_some() {
                    return Err(invalid("maximal accepts no node endpoints"));
                }
            }
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

impl CompactPreviewArgs {
    pub fn validate(&self) -> Result<(), ApiError> {
        if let Some(action) = self.action {
            if action == ContextEditAction::Restore {
                return Err(invalid("compact --dry-run does not support restore"));
            }
            ContextEditArgs {
                thread_id: self.thread_id.clone(),
                action,
                from_node_id: self.from_node_id.clone(),
                to_node_id: self.to_node_id.clone(),
                max_chars: self.max_chars,
                max_percent: self.max_percent,
                segment_chars: self.segment_chars,
                reduce_rounds: self.reduce_rounds,
            }
            .validate()
        } else {
            optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
            if self.thread_id.as_ref().is_some_and(|id| id.len() > 1024)
                || self.from_node_id.is_some()
                || self.to_node_id.is_some()
                || self.max_chars.is_some()
                || self.max_percent.is_some()
                || self.segment_chars.is_some()
                || self.reduce_rounds.is_some()
            {
                return Err(invalid(
                    "default compact preview accepts only a Thread scope",
                ));
            }
            Ok(())
        }
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

impl ThreadPruneArgs {
    fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        nonempty(&self.before_node_id, "before_node_id")
    }
}

impl ThreadBranchesArgs {
    fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        optional_nonempty(self.cursor.as_deref(), "cursor")?;
        if self.limit == 0 || self.limit > MAX_LIST_LIMIT {
            return Err(invalid("branch limit must be in 1..=200"));
        }
        Ok(())
    }
}

impl ThreadRewindArgs {
    fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        nonempty(&self.node_id, "node_id")
    }
}

/// Most explicit Thread IDs one archive or unarchive call accepts.
pub(crate) const MAX_ARCHIVE_THREADS: usize = 200;

impl ThreadArchiveArgs {
    fn validate(&self) -> Result<(), ApiError> {
        if self.thread_ids.len() > MAX_ARCHIVE_THREADS {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                format!("thread archive accepts at most {MAX_ARCHIVE_THREADS} Thread IDs"),
            ));
        }
        for id in &self.thread_ids {
            nonempty(id, "thread_id")?;
        }
        Ok(())
    }
}

/// `""` clears the Thread override; digits set it (0 disables proactive compaction).
pub(crate) fn parse_auto_compact_at(value: &str) -> Result<Option<i64>, ApiError> {
    if value.is_empty() {
        return Ok(None);
    }
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(
            "auto_compact_at must be a non-negative integer, 0 to disable, or an empty string to clear",
        ));
    }
    value
        .parse::<i64>()
        .map(Some)
        .map_err(|_| invalid("auto_compact_at is out of range"))
}

/// Digits set the value; `""` clears the Thread override. Both are bounded by `range`.
pub(crate) fn parse_bounded_override(
    value: &str,
    name: &str,
    range: &std::ops::RangeInclusive<usize>,
) -> Result<Option<i64>, ApiError> {
    if value.is_empty() {
        return Ok(None);
    }
    let message = || {
        format!(
            "{name} must be an integer in {}..={}, or an empty string to clear",
            range.start(),
            range.end()
        )
    };
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(message()));
    }
    value
        .parse::<usize>()
        .ok()
        .filter(|v| range.contains(v))
        .map(|v| Some(v as i64))
        .ok_or_else(|| invalid(message()))
}
pub(crate) fn parse_segment_chars(value: &str) -> Result<Option<i64>, ApiError> {
    parse_bounded_override(value, "segment_chars", &crate::config::SEGMENT_CHARS_RANGE)
}
pub(crate) fn parse_reduce_rounds(value: &str) -> Result<Option<i64>, ApiError> {
    parse_bounded_override(value, "reduce_rounds", &crate::config::REDUCE_ROUNDS_RANGE)
}
fn check_summary_parameters(segment: Option<u64>, rounds: Option<u64>) -> Result<(), ApiError> {
    if let Some(value) = segment {
        parse_segment_chars(&value.to_string())?;
    }
    if let Some(value) = rounds {
        parse_reduce_rounds(&value.to_string())?;
    }
    Ok(())
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
        if let Some(value) = &self.auto_compact_at {
            parse_auto_compact_at(value)?;
        }
        if let Some(value) = &self.segment_chars {
            parse_segment_chars(value)?;
        }
        if let Some(value) = &self.reduce_rounds {
            parse_reduce_rounds(value)?;
        }
        if self.connection.is_none()
            && self.cwd.is_none()
            && self.model.is_none()
            && self.effort.is_none()
            && self.title.is_none()
            && self.agents.is_none()
            && self.auto_compact_at.is_none()
            && self.segment_chars.is_none()
            && self.reduce_rounds.is_none()
            && self.parent_id.is_none()
        {
            Err(invalid("thread.set requires a change"))
        } else {
            Ok(())
        }
    }
}

impl WaitArgs {
    fn validate(&self) -> Result<(), ApiError> {
        crate::thread_observation::validate(&self.conditions, self.after.as_deref())?;
        if self.conditions.is_empty() {
            return Err(invalid("wait requires at least one condition"));
        }
        for condition in &self.conditions {
            match condition {
                WaitCondition::ReplyReceived { message_id } => nonempty(message_id, "message_id")?,
                WaitCondition::ThreadSettled { .. } => {}
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

/// Bounds the file edit and its result, whose size is checked again before writing.
const MAX_CONFIG_CHANGES: usize = 64;
const MAX_CONFIG_FIELD_BYTES: usize = 512;
const MAX_CONFIG_VALUE_BYTES: usize = 128 * 1024;
impl ConfigSetArgs {
    fn validate(&self) -> Result<(), ApiError> {
        if self.expected_hash.len() != 64
            || !self
                .expected_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid(
                "expected_hash must be file.content_hash from config.get",
            ));
        }
        if self.changes.is_empty() || self.changes.len() > MAX_CONFIG_CHANGES {
            return Err(invalid(format!(
                "config.set takes 1 to {MAX_CONFIG_CHANGES} changes"
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for change in &self.changes {
            if change.field.len() > MAX_CONFIG_FIELD_BYTES
                || toml_edit::Key::parse(&change.field).is_err()
            {
                return Err(invalid("field must be a TOML dotted key"));
            }
            if !seen.insert(change.field.as_str()) {
                return Err(invalid("each field may be changed once per config.set"));
            }
            let bytes = change
                .value
                .as_ref()
                .map_or(0, |value| value.to_string().len());
            if bytes > MAX_CONFIG_VALUE_BYTES {
                return Err(ApiError::new(
                    ErrorCode::InputLimit,
                    format!("a config.set value must be at most {MAX_CONFIG_VALUE_BYTES} bytes"),
                ));
            }
        }
        Ok(())
    }
}
impl EventsWaitArgs {
    fn validate(&self) -> Result<(), ApiError> {
        if let Some(after) = &self.after {
            EventCursor::decode(after)?;
        }
        if self.thread_ids.len() > MAX_EVENT_THREAD_FILTER {
            return Err(invalid(format!(
                "events.wait accepts at most {MAX_EVENT_THREAD_FILTER} thread filters"
            )));
        }
        for id in &self.thread_ids {
            nonempty(id, "thread_id")?;
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
        if self.parts && self.end_message_only {
            return Err(invalid("parts and end_message_only cannot be combined"));
        }
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
        if !self.kinds.is_empty()
            && self.scope != Some(IndexScope::All)
            && self.thread_ids.is_empty()
        {
            return Err(invalid(
                "--kind applies to --thread or --scope all selections; --source IDs are never filtered by kind",
            ));
        }
        if self
            .kinds
            .iter()
            .any(|kind| kind != "all" && !crate::config::INDEXABLE_KINDS.contains(&kind.as_str()))
        {
            return Err(invalid(
                "index kind must be all, user, summary, assistant_response, tool_result, spill or event",
            ));
        }
        Ok(())
    }
}
impl Command {
    pub fn validate(&self) -> Result<(), ApiError> {
        match self {
            Self::OwnerShutdown(a) => nonempty(&a.instance_id, "instance_id"),
            Self::Status(a) => a.validate(),
            Self::CompactList(a) => a.validate(),
            Self::ShellList(a) => a.validate(),
            Self::UsageGet(a) => {
                if let Some(attempt) = &a.attempt_id {
                    optional_nonempty(Some(attempt), "attempt_id")?;
                    if a.thread_id.is_some()
                        || a.run_id.is_some()
                        || a.turn_id.is_some()
                        || a.include_children
                        || a.since.is_some()
                        || a.until.is_some()
                        || a.group_by.is_some()
                        || a.timezone.is_some()
                        || a.limit.is_some()
                        || a.cursor.is_some()
                    {
                        return Err(invalid(
                            "usage.get attempt selector conflicts with aggregate options",
                        ));
                    }
                    return Ok(());
                }
                crate::usage_totals::breakdown::validate(a)?;
                crate::readback::Window::new(a.since.as_deref(), a.until.as_deref())?;
                if [
                    a.thread_id.is_some(),
                    a.run_id.is_some(),
                    a.turn_id.is_some(),
                ]
                .into_iter()
                .filter(|v| *v)
                .count()
                    > 1
                    || (a.include_children && (a.run_id.is_some() || a.turn_id.is_some()))
                {
                    return Err(invalid(
                        "usage.get needs one scope; children apply only to Thread scope",
                    ));
                }
                optional_nonempty(a.thread_id.as_deref(), "thread_id")?;
                optional_nonempty(a.run_id.as_deref(), "run_id")?;
                optional_nonempty(a.turn_id.as_deref(), "turn_id")
            }
            Self::ModelList(a) => a.validate(),
            Self::ThreadList(a) => a.validate(),
            Self::ThreadGet(a) | Self::AttachmentList(a) | Self::Compact(a) => {
                optional_nonempty(a.thread_id.as_deref(), "thread_id")
            }
            Self::ContextEdit(a) => a.validate(),
            Self::CompactPreview(a) => a.validate(),
            Self::ThreadCreate(a) => a.validate(),
            Self::ThreadFork(a) => a.validate(),
            Self::ThreadPrune(a) => a.validate(),
            Self::ThreadRewind(a) => a.validate(),
            Self::ThreadArchive(a) | Self::ThreadUnarchive(a) => a.validate(),
            Self::ThreadTag(a) => a.validate(),
            Self::ThreadTags(a) => a.validate(),
            Self::ThreadCwds(a) => a.validate(),
            Self::ThreadBranches(a) => a.validate(),
            Self::ThreadDelete(a) | Self::ThreadDeletePreview(a) => {
                nonempty(&a.thread_id, "thread_id")?;
                if let Some(marker) = &a.preview_marker
                    && (!(1..=crate::deletion::MAX_PREVIEW_THREADS).contains(&marker.thread_count)
                        || marker.ids_hash.len() != 64
                        || !marker
                            .ids_hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                {
                    return Err(invalid(
                        "preview_marker requires thread_count 1..257 and a 64-character lowercase SHA-256 ids_hash",
                    ));
                }
                Ok(())
            }
            Self::ThreadSet(a) => a.validate(),
            Self::MessageSend(a) => {
                optional_nonempty(a.thread_id.as_deref(), "thread_id")?;
                validate_sender_label(a.sender_label.as_deref())
            }
            Self::MessageReply(a) => {
                nonempty(&a.message_id, "message_id")?;
                validate_sender_label(a.sender_label.as_deref())
            }
            Self::MessageGet(a) => nonempty(&a.message_id, "message_id"),
            Self::MessageCancel(a) => {
                if a.message_ids.is_empty() || a.message_ids.len() > 128 {
                    return Err(invalid(
                        "message cancel requires 1..=128 explicit message IDs",
                    ));
                }
                let mut unique = std::collections::HashSet::new();
                for id in &a.message_ids {
                    nonempty(id, "message_id")?;
                    if id.len() > 4096 || !unique.insert(id) {
                        return Err(invalid(
                            "message cancel IDs must be unique and at most 4096 bytes",
                        ));
                    }
                }
                Ok(())
            }
            Self::OperationGet(a) => nonempty(&a.operation_id, "operation_id"),
            Self::RunGet(a) | Self::RunRetry(a) | Self::RunStop(a) => nonempty(&a.run_id, "run_id"),
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
            Self::IndexProbe(a) => validate_dimensions(a.dimensions),
            Self::IndexShrink(a) => {
                if a.cancel && (a.dry_run || a.revert) {
                    return Err(invalid(
                        "index shrink --cancel conflicts with --dry-run and --revert",
                    ));
                }
                Ok(())
            }
            Self::IndexRemove(a) => nonempty(&a.space_id, "space_id"),
            Self::IndexStatus(a)
            | Self::IndexPreview(a)
            | Self::IndexBackfill(a)
            | Self::IndexRetry(a)
            | Self::IndexRebuild(a) => {
                a.validate_targets()?;
                validate_dimensions(a.dimensions)?;
                if a.dimensions.is_some() && !matches!(self, Self::IndexRebuild(_)) {
                    return Err(invalid(
                        "dimensions is only supported by index rebuild/probe",
                    ));
                }
                if matches!(self, Self::IndexBackfill(_) | Self::IndexRebuild(_))
                    && a.scope.is_none()
                    && a.thread_ids.is_empty()
                    && a.source_ids.is_empty()
                {
                    return Err(invalid(
                        "index operation requires explicit target selection",
                    ));
                }
                Ok(())
            }
            Self::StorageStatus(_) | Self::StorageCollect(_) | Self::ConfigGet(_) => Ok(()),
            Self::ConfigSet(a) => a.validate(),
            Self::EventsWait(a) => a.validate(),
        }
    }
}

fn validate_dimensions(dimensions: Option<usize>) -> Result<(), ApiError> {
    if dimensions.is_some_and(|n| !(1..=crate::storage::MAX_VECTOR_DIMENSIONS).contains(&n)) {
        return Err(invalid("dimensions must be in 1..=8192"));
    }
    Ok(())
}
