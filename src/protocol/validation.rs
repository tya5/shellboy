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
                || self.ended_after.is_some()
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
        if let Some(ended_after) = &self.ended_after {
            rfc3339(ended_after, || {
                invalid("ended_after must be RFC3339 with timezone")
            })?;
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

fn service_name(name: &str) -> Result<(), ApiError> {
    if (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Ok(())
    } else {
        Err(invalid(
            "Service name must contain 1 to 64 lowercase ASCII letters, digits or hyphens",
        ))
    }
}

fn validate_recipient_name(name: &str) -> Result<(), ApiError> {
    nonempty(name, "recipient name")?;
    if name.len() > 256 {
        return Err(invalid("recipient name exceeds 256 bytes"));
    }
    Ok(())
}

impl ThreadListArgs {
    fn validate(&self) -> Result<(), ApiError> {
        limit(self.limit, MAX_LIST_LIMIT)?;
        optional_nonempty(self.root.as_deref(), "root")?;
        if self.root.as_ref().is_some_and(|root| root.len() > 256) {
            return Err(invalid("root exceeds the Thread identity bound"));
        }
        if self.root.is_some()
            && (self.order == ThreadListOrder::Created || self.awaiting.is_some())
        {
            return Err(invalid(
                "root cannot be combined with order=created or awaiting",
            ));
        }
        if self.include_detail && self.thread_ids.is_none() {
            return Err(invalid("include_detail requires exact Thread IDs"));
        }
        if let Some(ids) = &self.thread_ids {
            if ids.len() > MAX_LIST_LIMIT as usize {
                return Err(ApiError::new(
                    ErrorCode::InputLimit,
                    format!("thread_ids accepts at most {MAX_LIST_LIMIT} IDs"),
                ));
            }
            if self.cursor.is_some()
                || !self.order.is_updated()
                || self.query.is_some()
                || self.awaiting.is_some()
                || self.archived != ArchivedFilter::Exclude
                || !self.tags.is_empty()
                || self.cwd.is_some()
            {
                return Err(invalid(
                    "thread_ids cannot be combined with a cursor, non-default order, or discovery filters other than state",
                ));
            }
            let mut seen = std::collections::HashSet::with_capacity(ids.len());
            for id in ids {
                nonempty(id, "thread ID")?;
                if !seen.insert(id) {
                    return Err(invalid("thread_ids must not contain duplicates"));
                }
            }
            return Ok(());
        }
        optional_nonempty(self.cursor.as_deref(), "cursor")?;
        optional_nonempty(self.cwd.as_deref(), "cwd")?;
        if self.order == ThreadListOrder::Created
            && (self.query.is_some()
                || !self.tags.is_empty()
                || self.cwd.is_some()
                || self.awaiting.is_some()
                || self.state.is_some())
        {
            return Err(invalid(
                "order=created cannot be combined with query, tags, cwd, awaiting, or state",
            ));
        }
        if let Some(name) = self.awaiting.as_deref() {
            validate_recipient_name(name)?;
        }
        if self.awaiting.is_some()
            && (self.query.is_some()
                || !self.tags.is_empty()
                || self.cwd.is_some()
                || self.state.is_some())
        {
            return Err(invalid(
                "awaiting cannot be combined with query, tags, cwd, or state",
            ));
        }
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
        if let Some(chars) = self.max_chars
            && (self.action != ContextEditAction::Summary || chars == 0 || chars > i64::MAX as u64)
        {
            return Err(invalid(
                "summary max_chars requires summary action and integer 1..i64::MAX",
            ));
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
            }
            .validate()
        } else {
            optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
            if self.thread_id.as_ref().is_some_and(|id| id.len() > 1024)
                || self.from_node_id.is_some()
                || self.to_node_id.is_some()
                || self.max_chars.is_some()
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
            ("parent_thread_id", &self.parent_thread_id),
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
        optional_nonempty(self.recovery_run_id.as_deref(), "run_id")?;
        if self.recovery_run_id.is_some() && self.thread_id.is_none() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "recovery rewind requires an explicit Thread ID",
            ));
        }
        if self.recovery_run_id.is_some() && self.dry_run {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "recovery rewind cannot be a dry run",
            ));
        }
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
pub(crate) fn parse_target_percent(value: &str) -> Result<Option<i64>, ApiError> {
    parse_bounded_override(value, "auto_compact_target_percent", &(1..=100))
}
pub(crate) fn parse_summary_chars(value: &str) -> Result<Option<i64>, ApiError> {
    parse_bounded_override(value, "summary_max_chars", &(1..=i64::MAX as usize))
}
/// Per-Thread output limit. Only a positive integer; the provider's own limit
/// is the real bound, so Shellboy adds none of its own.
pub(crate) fn parse_max_tokens(value: &str) -> Result<Option<i64>, ApiError> {
    parse_bounded_override(value, "max_tokens", &(1..=i64::MAX as usize))
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
        if let Some(value) = &self.auto_compact_target_percent {
            parse_target_percent(value)?;
        }
        if let Some(value) = &self.summary_max_chars {
            parse_summary_chars(value)?;
        }
        if let Some(value) = &self.max_tokens {
            parse_max_tokens(value)?;
        }
        if self
            .spill
            .as_deref()
            .is_some_and(|value| !matches!(value, "on" | "off" | ""))
        {
            return Err(invalid("spill must be on, off, or empty to inherit"));
        }
        if self.connection.is_none()
            && self.cwd.is_none()
            && self.model.is_none()
            && self.effort.is_none()
            && self.title.is_none()
            && self.agents.is_none()
            && self.auto_compact_at.is_none()
            && self.auto_compact_target_percent.is_none()
            && self.summary_max_chars.is_none()
            && self.max_tokens.is_none()
            && self.spill.is_none()
            && self.parent_thread_id.is_none()
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
                WaitCondition::ReplyReceived { message_id }
                | WaitCondition::MessageDelivered { message_id } => {
                    nonempty(message_id, "message_id")?
                }
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
        if self.expected_hash.as_ref().is_some_and(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
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
impl CredentialsSetArgs {
    fn validate(&self) -> Result<(), ApiError> {
        crate::credential_store::check_connection_name(&self.connection)?;
        crate::credential_store::check_key(&self.value)
    }
}
const MAX_TEST_URL_BYTES: usize = 2048;
impl ConnectionTestArgs {
    fn validate(&self) -> Result<(), ApiError> {
        if let Some(model) = &self.post_model
            && (model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control))
        {
            return Err(invalid(
                "post_model must be a nonempty model ID of at most 256 bytes",
            ));
        }
        match (&self.connection, &self.base_url) {
            (Some(name), None) => {
                crate::credential_store::check_connection_name(name)?;
                if self.api.is_some()
                    || self.api_key.is_some()
                    || self.api_key_env.is_some()
                    || self.api_key_header.is_some()
                    || self.ssl_verify.is_some()
                {
                    return Err(invalid(
                        "candidate settings require base_url, not a saved connection",
                    ));
                }
                Ok(())
            }
            (None, Some(url)) => {
                if url.trim().is_empty() || url.len() > MAX_TEST_URL_BYTES {
                    return Err(invalid(format!(
                        "base_url must be 1 to {MAX_TEST_URL_BYTES} bytes"
                    )));
                }
                if self.api_key.is_some() && self.api_key_env.is_some() {
                    return Err(invalid("give api_key or api_key_env, not both"));
                }
                if let Some(key) = &self.api_key {
                    crate::credential_store::check_key(key)?;
                }
                if let Some(name) = &self.api_key_env
                    && !crate::config::valid_env_name(name)
                {
                    return Err(invalid("api_key_env must be an environment variable name"));
                }
                if let Some(header) = &self.api_key_header
                    && (header.is_empty()
                        || header.len() > 256
                        || !crate::config::valid_header_name(header))
                {
                    return Err(invalid("api_key_header must be an HTTP header name"));
                }
                Ok(())
            }
            _ => Err(invalid(
                "give exactly one of connection (a saved connection) and base_url (a candidate)",
            )),
        }
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
        optional_nonempty(self.shell_state.as_deref(), "shell_state")?;
        self.validate_excerpt()?;
        self.validate_state()?;
        self.parsed_since().map(|_| ())
    }
    // Shared with the direct reader to preserve its native-state-before-time
    // precedence while moving rejection ahead of operation admission.
    pub(crate) fn validate_state(&self) -> Result<(), ApiError> {
        if self.shell_state.as_deref().is_some_and(|state| {
            !matches!(
                state,
                "starting" | "running" | "stopping" | "exited" | "failed" | "stopped"
            )
        }) {
            return Err(super::invalid(
                "shell-state must be a saved shell execution state: starting, running, stopping, exited, failed, stopped",
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

impl HistoryReadArgs {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.node_id, "node_id")?;
        nonempty(&self.encoding, "encoding")?;
        limit(self.limit_bytes, MAX_BODY_BYTES)?;
        if self.parts && self.end_message_only {
            return Err(invalid("parts and end_message_only cannot be combined"));
        }
        if let Some(display) = &self.display {
            if self.parts || self.end_message_only {
                return Err(invalid(
                    "display cannot be combined with parts or end_message_only",
                ));
            }
            if let Some(selection) = &display.selection {
                nonempty(&selection.thread_id, "display.selection.thread_id")?;
            }
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

impl HistoryWindowArgs {
    fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.thread_id, "thread_id")?;
        optional_nonempty(self.node_id.as_deref(), "node_id")?;
        optional_nonempty(self.expect_node_id.as_deref(), "expect_node_id")?;
        if self.ordinal.is_some() && self.node_id.is_some() {
            return Err(invalid("give at most one of ordinal and node_id"));
        }
        if self.expect_node_id.is_some() && self.ordinal.is_none() {
            return Err(invalid("expect_node_id needs ordinal"));
        }
        if u64::from(self.before) + u64::from(self.after) + 1 > u64::from(MAX_LIST_LIMIT) {
            return Err(invalid("before + after + 1 must be at most 200"));
        }
        if self.excerpt_chars > MAX_HISTORY_EXCERPT_CHARS {
            return Err(invalid("excerpt-chars must be in 0..=4096"));
        }
        if !self.observe_node_ids.is_empty() && !self.include_observation {
            return Err(invalid("observe_node_ids needs include_observation"));
        }
        if self.observe_node_ids.len() > 128
            || self
                .observe_node_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 512)
        {
            return Err(invalid(
                "observe_node_ids takes at most 128 nonempty node IDs",
            ));
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
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        if self.tail && self.cursor.is_some() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "tail cannot be combined with cursor",
            ));
        }
        nonempty(&self.execution_id, "execution_id")?;
        if let Some(owner) = &self.expected_owner {
            nonempty(&owner.thread_id, "thread_id")?;
            nonempty(&owner.run_id, "run_id")?;
        }
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
            Self::DraftImage(a) => {
                nonempty(&a.instance_id, "instance_id")?;
                nonempty(&a.draft_id, "draft_id")?;
                match &a.action {
                    super::DraftImageAction::Begin { encoded_bytes } if *encoded_bytes == 0 => {
                        Err(invalid("Image upload must contain bytes"))
                    }
                    super::DraftImageAction::Begin { .. } => Ok(()),
                    super::DraftImageAction::Append {
                        upload_id, base64, ..
                    } => {
                        nonempty(upload_id, "upload_id")?;
                        if base64.is_empty()
                            || base64.len() > super::DRAFT_IMAGE_CHUNK_BYTES.div_ceil(3) * 4
                        {
                            return Err(invalid("Invalid image upload chunk size"));
                        }
                        Ok(())
                    }
                    super::DraftImageAction::Commit { upload_id } => {
                        nonempty(upload_id, "upload_id")
                    }
                    super::DraftImageAction::Release { attachment_id } => {
                        nonempty(attachment_id, "attachment_id")
                    }
                }
            }
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
                        || a.since_node_id.is_some()
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
                optional_nonempty(a.since_node_id.as_deref(), "since_node_id")?;
                if a.since_node_id.is_some() && a.since.is_some() {
                    return Err(invalid("usage.get since_node_id conflicts with since"));
                }
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
            Self::DraftSubmit(a) => {
                optional_nonempty(
                    a.expected_source_message.as_deref(),
                    "expected_source_message",
                )?;
                if a.expected_source_message.is_some() && a.draft.edit.is_none() {
                    return Err(invalid("expected_source_message requires an edited draft"));
                }
                nonempty(&a.draft.draft_id, "draft_id")?;
                optional_nonempty(a.thread_id.as_deref(), "thread_id")?;
                if (a.expected_thread_revision.is_some() && a.thread_id.is_none())
                    || a.draft.edit.as_ref().is_some_and(|edit| {
                        Some(edit.thread_id.as_str()) != a.thread_id.as_deref()
                            || edit.node_id.is_empty()
                    })
                {
                    return Err(invalid(
                        "Draft submission selectors must match the target Thread",
                    ));
                }
                Self::ThreadCreate(a.create.clone()).validate()
            }
            Self::DraftRestore(a) => nonempty(&a.archive_id, "archive_id"),
            Self::DraftGet(a) => {
                if let Some(thread) = &a.thread_id {
                    nonempty(thread, "thread_id")?;
                }
                if let Some(id) = &a.operation_id {
                    nonempty(id, "operation_id")?;
                } else if a.thread_id.is_none() {
                    return Err(invalid("draft.get requires thread_id or operation_id"));
                }
                Ok(())
            }
            Self::DraftList(a) => {
                if !(1..=MAX_LIST_LIMIT).contains(&a.limit) {
                    return Err(invalid("limit must be 1..=200"));
                }
                if let Some(thread) = &a.thread_id {
                    nonempty(thread, "thread_id")?;
                }
                if a.kind == DraftListKind::Submitted && a.thread_id.is_none() {
                    return Err(invalid("submitted drafts require thread_id"));
                }
                Ok(())
            }
            Self::DraftSave(a) => {
                nonempty(&a.thread_id, "thread_id")?;
                optional_nonempty(
                    a.expected_source_message.as_deref(),
                    "expected_source_message",
                )?;
                nonempty(&a.draft.draft_id, "draft_id")
            }
            Self::ModelList(a) => a.validate(),
            Self::ThreadList(a) => a.validate(),
            Self::ThreadGet(a) => optional_nonempty(a.thread_id.as_deref(), "thread_id"),
            Self::Compact(a) => optional_nonempty(a.thread_id.as_deref(), "thread_id"),
            Self::AttachmentReload(a) => nonempty(&a.thread_id, "thread_id"),
            Self::AttachmentList(a) => a.validate(),
            Self::ContextEdit(a) => a.validate(),
            Self::CompactPreview(a) => a.validate(),
            Self::ThreadImport(a) => {
                if a.document.len() > MAX_FRAME_BYTES {
                    Err(crate::thread_import::too_large())
                } else {
                    Ok(())
                }
            }
            Self::ThreadCreate(a) => a.validate(),
            Self::ThreadFork(a) => a.validate(),
            Self::ThreadPrune(a) => a.validate(),
            Self::ThreadRewind(a) => a.validate(),
            Self::ThreadArchive(a) | Self::ThreadUnarchive(a) => a.validate(),
            Self::ThreadTag(a) => a.validate(),
            Self::ScheduleSet(a) => {
                nonempty(&a.schedule_id, "schedule_id")?;
                if a.description.is_none() && a.notify.is_none() {
                    return Err(invalid("schedule set needs description or notify"));
                }
                a.notify
                    .as_deref()
                    .map_or(Ok(()), |notify| ScheduleNotify::saved(notify).map(drop))
            }
            Self::ScheduleEnable(a) | Self::ScheduleDisable(a) | Self::ScheduleRemove(a) => {
                nonempty(&a.schedule_id, "schedule_id")
            }
            Self::ArtifactAdd(a) => nonempty(&a.thread_id, "thread_id"),
            Self::ArtifactUpdate(a) => nonempty(&a.artifact_id, "artifact_id"),
            Self::ArtifactList(a) => {
                if let Some(id) = &a.thread_id {
                    nonempty(id, "thread_id")?;
                }
                crate::artifacts::validate_list_scope(a).map(|_| ())
            }
            Self::ArtifactGet(a) => nonempty(&a.artifact_id, "artifact_id"),
            Self::ArtifactRemove(a) => nonempty(&a.artifact_id, "artifact_id"),
            Self::ArtifactRead(a) => {
                nonempty(&a.thread_id, "thread_id")?;
                nonempty(&a.artifact_id, "artifact_id")?;
                if a.version < 1 {
                    return Err(invalid("artifact version must be positive"));
                }
                if a.path.as_deref().is_some_and(str::is_empty) {
                    return Err(invalid("artifact path must not be empty"));
                }
                Ok(())
            }
            Self::ServiceAdd(a) => {
                optional_nonempty(a.thread_id.as_deref(), "thread_id")?;
                a.name.as_deref().map_or(Ok(()), service_name)
            }
            Self::ServiceSet(a) => {
                nonempty(&a.service_id, "service_id")?;
                if a.name.is_empty() {
                    Ok(())
                } else {
                    service_name(&a.name)
                }
            }
            Self::ServiceList(a) => optional_nonempty(a.thread_id.as_deref(), "thread_id"),
            Self::ServiceStart(a)
            | Self::ServiceStop(a)
            | Self::ServiceRemove(a)
            | Self::ServiceGet(a) => nonempty(&a.service_id, "service_id"),
            Self::ServiceEndpoint(a) => match (
                a.service_id.as_deref(),
                a.thread_id.as_deref(),
                a.name.as_deref(),
            ) {
                (Some(id), None, None) => nonempty(id, "service_id"),
                (None, Some(thread), Some(name)) => {
                    nonempty(thread, "thread_id")?;
                    service_name(name)
                }
                _ => Err(invalid("Specify service_id or both thread_id and name")),
            },
            Self::ScheduleAdd(a) => {
                optional_nonempty(a.thread_id.as_deref(), "thread_id")?;
                ScheduleNotify::saved(&a.notify).map(drop)
            }
            Self::ScheduleList(a) => optional_nonempty(a.thread_id.as_deref(), "thread_id"),
            Self::ThreadTags(a) => a.validate(),
            Self::ThreadCwds(a) => a.validate(),
            Self::ThreadBranches(a) => a.validate(),
            Self::ThreadDeleteSet(a) => crate::deletion::validate_set(&a.thread_ids),
            Self::ThreadDeleteSetPreview(a) => crate::deletion::validate_set(&a.thread_ids),
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
            Self::MessageAsk(a) => {
                optional_nonempty(a.thread_id.as_deref(), "thread_id")?;
                if a.thread_id.is_some() && a.to.is_some() {
                    return Err(invalid("message.ask takes a name or a Thread, not both"));
                }
                if a.thread_id.is_none() && a.deliver != super::Deliver::Next {
                    return Err(invalid("message.ask delivery timing requires a Thread"));
                }
                if let Some(name) = &a.to {
                    validate_recipient_name(name)?;
                }
                nonempty(&a.text, "text")?;
                validate_sender_label(a.sender_label.as_deref())
            }
            Self::IdResolve(a) => {
                let chars = a.prefix.chars().count();
                if !(8..36).contains(&chars) {
                    return Err(invalid("id.resolve prefix must contain 8..=35 characters"));
                }
                Ok(())
            }
            Self::MessageReply(a) => {
                nonempty(&a.message_id, "message_id")?;
                validate_sender_label(a.sender_label.as_deref())
            }
            Self::MessageList(a) => {
                limit(a.limit, MAX_LIST_LIMIT)?;
                optional_nonempty(a.cursor.as_deref(), "cursor")?;
                optional_nonempty(a.asked_by_thread_id.as_deref(), "asked_by_thread_id")?;
                optional_nonempty(a.owed_by_thread_id.as_deref(), "owed_by_thread_id")?;
                if a.owed_by_thread_id.is_some() && a.to.is_some() {
                    return Err(invalid(
                        "owed_by_thread_id selects unnamed duties and cannot be combined with to",
                    ));
                }
                if let Some(name) = a.to.as_deref() {
                    validate_recipient_name(name)?;
                }
                Ok(())
            }
            Self::MessageGet(a) => {
                nonempty(&a.message_id, "message_id")?;
                optional_nonempty(a.source_thread_id.as_deref(), "source_thread_id")
            }
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
            Self::RunGet(a) => nonempty(&a.run_id, "run_id"),
            Self::RunRetry(a) => {
                nonempty(&a.run_id, "run_id")?;
                if a.expected_turn.is_some() && a.expected_selection.is_some() {
                    return Err(ApiError::new(
                        ErrorCode::InvalidArgument,
                        "Specify only one Run fence",
                    ));
                }
                validate_run_selection(a.expected_selection.as_ref())?;
                validate_expected_turn(a.expected_turn.as_ref())
            }
            Self::RunStop(super::RunStopTarget::Run {
                run_id,
                expected_selection,
            }) => {
                nonempty(run_id, "run_id")?;
                validate_run_selection(expected_selection.as_ref())
            }
            Self::RunStop(super::RunStopTarget::Thread {
                thread_id,
                expected_turn,
            }) => {
                nonempty(thread_id, "thread_id")?;
                validate_expected_turn(expected_turn.as_ref())
            }
            Self::TurnGet(a) | Self::TurnInterrupt(a) => nonempty(&a.turn_id, "turn_id"),
            Self::Wait(a) => a.validate(),
            Self::HistoryList(a) => a.validate(),
            Self::HistoryWindow(a) => a.validate(),
            Self::HistoryReceived(a) => {
                nonempty(&a.thread_id, "thread_id")?;
                if a.thread_id.len() > 512
                    || a.cursor.as_ref().is_some_and(|token| token.len() > 8192)
                {
                    return Err(invalid("Invalid received history scope or cursor"));
                }
                Ok(())
            }
            Self::HistoryRecall(a) => {
                nonempty(&a.thread_id, "thread_id")?;
                if a.thread_id.len() > 512
                    || a.expected_revision.is_some() && a.anchor.is_none()
                    || a.anchor.as_ref().is_some_and(|anchor| {
                        anchor.node_id.is_empty()
                            || anchor.node_id.len() > 512
                            || i64::try_from(anchor.ordinal).is_err()
                    })
                {
                    return Err(invalid("Invalid history recall scope or anchor"));
                }
                Ok(())
            }
            Self::HistoryRead(a) => a.validate(),
            Self::HistorySearch(a) => a.validate(),
            Self::HistoryExport(a) => limit(a.limit, MAX_LIST_LIMIT),
            Self::HistoryRelations(a) => a.validate(),
            Self::ShellGet(a)
            | Self::ShellEof(a)
            | Self::ShellStop(a)
            | Self::ShellBackground(a) => nonempty(&a.execution_id, "execution_id"),
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
            Self::IndexStatus(a) => {
                a.selection().validate_targets()?;
                if a.coverage_cursor.is_some() && !a.coverage {
                    return Err(invalid("coverage_cursor requires coverage"));
                }
                Ok(())
            }
            Self::IndexPreview(a)
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
            Self::CredentialsSet(a) => a.validate(),
            Self::CredentialsClear(a) => {
                crate::credential_store::check_connection_name(&a.connection)
            }
            Self::ConnectionTest(a) => a.validate(),
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

#[cfg(test)]
mod message_cancel_tests {
    use super::*;

    #[test]
    fn cancellation_requires_one_to_128_distinct_message_ids() {
        let validate =
            |message_ids| Command::MessageCancel(MessageCancelArgs { message_ids }).validate();
        let ids: Vec<_> = (0..128).map(|n| format!("message-{n}")).collect();
        assert!(validate(ids.clone()).is_ok());

        let mut too_many = ids;
        too_many.push("extra".into());
        for invalid in [
            vec![],
            too_many,
            vec!["duplicate".into(), "duplicate".into()],
        ] {
            assert_eq!(
                validate(invalid).unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
    }
}

#[cfg(test)]
mod thread_list_id_tests {
    use super::*;

    #[test]
    fn exact_thread_list_validates_one_page_of_unique_ids() {
        let command = |ids| {
            Command::ThreadList(ThreadListArgs {
                thread_ids: Some(ids),
                ..Default::default()
            })
        };
        let mut full_page: Vec<_> = (0..MAX_LIST_LIMIT).map(|n| format!("t-{n}")).collect();
        assert!(command(full_page.clone()).validate().is_ok());
        full_page.push("extra".into());
        assert_eq!(
            command(full_page).validate().unwrap_err().code,
            ErrorCode::InputLimit
        );
        assert_eq!(
            command(vec!["same".into(), "same".into()])
                .validate()
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            command(vec![String::new()]).validate().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        for args in [
            ThreadListArgs {
                cursor: Some("cursor".into()),
                ..Default::default()
            },
            ThreadListArgs {
                order: ThreadListOrder::Created,
                ..Default::default()
            },
        ] {
            assert_eq!(
                Command::ThreadList(ThreadListArgs {
                    thread_ids: Some(vec!["t".into()]),
                    ..args
                })
                .validate()
                .unwrap_err()
                .code,
                ErrorCode::InvalidArgument
            );
        }
        assert!(
            serde_json::to_value(ThreadListArgs::default())
                .unwrap()
                .get("thread_ids")
                .is_none()
        );
    }
}

fn validate_expected_turn(value: Option<&super::ExpectedTurn>) -> Result<(), ApiError> {
    if value.is_some_and(|turn| {
        turn.turn_id.is_empty()
            || turn.turn_id.len() > 256
            || i64::try_from(turn.generation).is_err()
    }) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Invalid expected Turn identity",
        ));
    }
    Ok(())
}

fn validate_run_selection(value: Option<&super::ExpectedRunSelection>) -> Result<(), ApiError> {
    if let Some(value) = value {
        nonempty(&value.thread_id, "expected_selection.thread_id")?;
        if value.thread_id.len() > 256 {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Invalid expected Thread identity",
            ));
        }
        validate_expected_turn(value.turn.as_ref())?;
    }
    Ok(())
}

impl AttachmentListArgs {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        optional_nonempty(self.thread_id.as_deref(), "thread_id")?;
        limit(self.limit, MAX_LIST_LIMIT)?;
        if let Some(ids) = &self.attachment_ids {
            if self.thread_id.is_none() || self.cursor.is_some() || ids.is_empty() {
                return Err(invalid(
                    "attachment_ids requires an explicit Thread, at least one ID, and no cursor",
                ));
            }
            if ids.len() > self.limit as usize {
                return Err(ApiError::new(
                    ErrorCode::InputLimit,
                    "attachment_ids exceeds the page limit",
                ));
            }
            let mut unique = std::collections::HashSet::with_capacity(ids.len());
            for id in ids {
                nonempty(id, "attachment ID")?;
                if id.len() > 256 || !unique.insert(id) {
                    return Err(invalid("attachment_ids must be unique bounded IDs"));
                }
            }
        }
        Ok(())
    }
}
