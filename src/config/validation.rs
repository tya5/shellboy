//! Ordered configuration validation phases. Local invariants stay with their
//! section; Config alone resolves references between sections.
use super::*;

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Keep first-error precedence: numeric bounds, local relationships,
        // connection contents/references, then environment/path/key checks.
        if !self.compaction.valid() {
            return Err(invalid(
                "compaction",
                "set positive summary_max_chars and/or finite summary_max_percent in (0,100], and summary map/reduce request limits in 1..=4096",
            ));
        }
        self.retention.validate_positive()?;
        self.limits.validate_positive()?;
        self.io.validate_positive()?;
        self.workers.validate_positive()?;
        self.indexing.validate_positive()?;
        if !(1..=128).contains(&self.embedding.batch_max_inputs) {
            return Err(invalid("embedding.batch_max_inputs", "must be in 1..=128"));
        }
        if !(1..=1_048_576).contains(&self.embedding.batch_max_chars) {
            return Err(invalid(
                "embedding.batch_max_chars",
                "must be in 1..=1048576",
            ));
        }
        if self.embedding.batch_max_inputs > 1
            && self.embedding.batch_max_chars < self.indexing.split_chars
        {
            return Err(invalid(
                "embedding.batch_max_chars",
                "must cover indexing.split_chars when batching is enabled",
            ));
        }
        let max_workers = tokio::sync::Semaphore::MAX_PERMITS.min(u32::MAX as usize);
        self.workers.validate_capacity(max_workers)?;
        self.indexing.validate_capacity(max_workers)?;
        self.ui.validate_positive()?;
        self.logging.validate_positive()?;
        self.limits.validate_relations()?;
        self.io.validate_relations()?;
        self.indexing.validate_retries()?;
        for (name, connection) in &self.connections {
            connection.validate(name)?;
        }
        self.validate_references()?;
        self.shell.validate_env()?;
        if self
            .data_dir
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(invalid("data_dir", "must not be empty"));
        }
        self.shell.validate_executable()?;
        self.ui.validate_keys()
    }
    fn validate_references(&self) -> Result<(), ConfigError> {
        if let Some(connection) = &self.default_connection {
            self.validate_connection_reference("default_connection", connection)?;
        }
        if let Some(model) = &self.default_model {
            nonempty("default_model", model)?;
        }
        match (&self.embedding.connection, &self.embedding.model) {
            (Some(connection), Some(model)) => {
                self.validate_connection_reference("embedding.connection", connection)?;
                nonempty("embedding.model", model)?;
            }
            (None, None) => {}
            _ => {
                return Err(invalid(
                    "embedding",
                    "set both connection and model, or omit both",
                ));
            }
        }
        Ok(())
    }
    fn validate_connection_reference(&self, key: &str, name: &str) -> Result<(), ConfigError> {
        if !self.connections.contains_key(name) {
            return Err(invalid(key, "must reference a configured connection"));
        }
        Ok(())
    }
}
impl Connection {
    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        nonempty(&format!("connections.{name}"), name)?;
        if self.prompt_cache_diagnostics && (self.api != Api::Responses || !self.prompt_caching) {
            return Err(invalid(
                format!("connections.{name}.prompt_cache_diagnostics"),
                "requires api = responses and prompt_caching = true",
            ));
        }
        if self.api != Api::Responses && self.prompt_cache_key {
            return Err(invalid(
                format!("connections.{name}.prompt_cache_key"),
                format!("Responses-only setting; omit it for {}", self.api.as_str()),
            ));
        }
        if self.prompt_cache_key && !self.prompt_caching {
            return Err(invalid(
                format!("connections.{name}.prompt_cache_key"),
                "requires prompt_caching; omit prompt_cache_key when prompt_caching = false",
            ));
        }
        match self.max_tokens {
            Some(_) if self.api != Api::AnthropicMessages => {
                return Err(invalid(
                    format!("connections.{name}.max_tokens"),
                    "only valid with api = \"anthropic_messages\"",
                ));
            }
            Some(0) => {
                return Err(invalid(
                    format!("connections.{name}.max_tokens"),
                    "must be a positive integer",
                ));
            }
            _ => {}
        }
        api_root(&self.base_url).map_err(|_| invalid(format!("connections.{name}.base_url"), "provide an HTTP(S) API root, e.g. https://proxy.example/v1, not https://proxy.example/v1/responses; omit credentials/query/fragment and /responses, /embeddings, /models or /chat/completions suffix"))?;
        if self.response_header_timeout_ms == 0 {
            return Err(invalid(
                format!("connections.{name}.response_header_timeout_ms"),
                "must be positive; the response header deadline cannot be disabled",
            ));
        }
        if self.request_timeout_ms < 1_000 {
            return Err(invalid(
                format!("connections.{name}.request_timeout_ms"),
                "must be at least 1000 milliseconds",
            ));
        }

        if self.response_max_retries > 20 {
            return Err(invalid(
                format!("connections.{name}.response_max_retries"),
                "must be between 0 and 20",
            ));
        }
        if !(1..=300_000).contains(&self.response_retry_max_delay_ms) {
            return Err(invalid(
                format!("connections.{name}.response_retry_max_delay_ms"),
                "must be between 1 and 300000 milliseconds",
            ));
        }
        if let Some(effort) = &self.default_effort {
            nonempty(&format!("connections.{name}.default_effort"), effort)?;
        }
        if let Some(header) = &self.api_key_header
            && (self.api_key_env.is_none()
                || header.is_empty()
                || header.len() > 256
                || !header.bytes().all(header_name_byte)
                || matches!(
                    header.to_ascii_lowercase().as_str(),
                    "host"
                        | "content-type"
                        | "content-length"
                        | "accept"
                        | "transfer-encoding"
                        | "connection"
                        | "x-shellboy-session-id"
                        | "anthropic-version"
                ))
        {
            return Err(invalid(
                format!("connections.{name}.api_key_header"),
                "requires api_key_env and a valid non-transport-owned HTTP header name of at most 256 bytes",
            ));
        }
        if let Some(env) = &self.api_key_env {
            if !valid_env_name(env) {
                return Err(invalid(
                    format!("connections.{name}.api_key_env"),
                    "must be an environment variable name",
                ));
            }
            if self.headers.keys().any(|key| {
                key.eq_ignore_ascii_case(self.api_key_header.as_deref().unwrap_or("authorization"))
            }) {
                return Err(invalid(
                    format!("connections.{name}.headers"),
                    "the api_key_env credential header cannot also be defined in headers",
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for (key, value) in &self.headers {
            if key.is_empty()
                || !key.bytes().all(header_name_byte)
                || value
                    .chars()
                    .any(|c| c == '\r' || c == '\n' || c == '\0' || (c.is_control() && c != '\t'))
            {
                return Err(invalid(
                    format!("connections.{name}.headers"),
                    "must contain valid HTTP header names and values",
                ));
            }
            if !seen.insert(key.to_ascii_lowercase()) {
                return Err(invalid(
                    format!("connections.{name}.headers"),
                    "header names must be unique case-insensitively",
                ));
            }
        }
        self.validate_session_header_names(name)?;
        self.validate_extra(name)
    }
    /// Thread UUID header names follow the `extra_headers` rules: no
    /// credential-like or transport-owned name, no repeat inside the list.
    /// While the header is enabled they also must not repeat another
    /// configured header. The legacy default name is exempt from the reserved
    /// and `headers` checks so a static header of that name is still overridden.
    /// An empty list sends nothing.
    fn validate_session_header_names(&self, name: &str) -> Result<(), ConfigError> {
        let key = format!("connections.{name}.session_header_names");
        if self.session_header_names.len() > 8 {
            return Err(invalid(key, "at most 8 names are allowed"));
        }
        let credential = self.api_key_env.is_some().then(|| {
            self.api_key_header
                .as_deref()
                .unwrap_or("authorization")
                .to_ascii_lowercase()
        });
        let mut seen = std::collections::BTreeSet::new();
        for value in &self.session_header_names {
            if value.is_empty() || value.len() > 256 || !value.bytes().all(header_name_byte) {
                return Err(invalid(
                    key,
                    "names must be valid HTTP header names of at most 256 bytes",
                ));
            }
            let lower = value.to_ascii_lowercase();
            if !seen.insert(lower.clone()) {
                return Err(invalid(key, "names must be unique case-insensitively"));
            }
            if lower == super::DEFAULT_SESSION_HEADER_NAMES[0].to_ascii_lowercase() {
                continue;
            }
            if RESERVED_EXTRA_HEADERS.contains(&lower.as_str())
                || lower == "anthropic-version"
                || CREDENTIAL_WORDS.iter().any(|word| lower.contains(word))
            {
                return Err(invalid(
                    key,
                    "credential-like and transport-owned header names are not accepted",
                ));
            }
            if self.session_header
                && (credential.as_deref() == Some(lower.as_str())
                    || self
                        .headers
                        .keys()
                        .chain(self.extra_headers.keys())
                        .any(|other| other.eq_ignore_ascii_case(value)))
            {
                return Err(invalid(
                    key,
                    "must not repeat a headers, extra_headers or credential header; override session_header_names to drop the name",
                ));
            }
        }
        Ok(())
    }
    /// `extra_body` and `extra_headers` are never interpreted. Only shape,
    /// size and collisions with fields Shellboy itself owns are checked.
    fn validate_extra(&self, name: &str) -> Result<(), ConfigError> {
        let body_key = format!("connections.{name}.extra_body");
        for key in self.extra_body.keys() {
            if self.api.reserved_body_fields().contains(&key.as_str()) {
                return Err(invalid(
                    body_key,
                    format!(
                        "`{key}` is a field Shellboy sends itself for api={}; remove it from extra_body",
                        self.api.as_str()
                    ),
                ));
            }
        }
        let value = serde_json::Value::Object(self.extra_body.clone());
        if contains_toml_datetime(&value) {
            return Err(invalid(
                body_key,
                "must be representable as JSON; use strings instead of TOML dates/times",
            ));
        }
        if value.to_string().len() > MAX_EXTRA_BODY_BYTES {
            return Err(invalid(
                body_key,
                format!("serialized JSON must be at most {MAX_EXTRA_BODY_BYTES} bytes"),
            ));
        }
        let headers_key = format!("connections.{name}.extra_headers");
        if self.extra_headers.len() > MAX_EXTRA_HEADERS {
            return Err(invalid(
                headers_key,
                format!("at most {MAX_EXTRA_HEADERS} headers are allowed"),
            ));
        }
        let mut seen: std::collections::BTreeSet<String> = self
            .headers
            .keys()
            .map(|key| key.to_ascii_lowercase())
            .collect();
        if self.api_key_env.is_some() {
            seen.insert(
                self.api_key_header
                    .as_deref()
                    .unwrap_or("authorization")
                    .to_ascii_lowercase(),
            );
        }
        for (key, value) in &self.extra_headers {
            if key.is_empty()
                || !key.bytes().all(header_name_byte)
                || value.len() > MAX_EXTRA_HEADER_VALUE_BYTES
                || value
                    .chars()
                    .any(|c| c == '\r' || c == '\n' || c == '\0' || (c.is_control() && c != '\t'))
            {
                return Err(invalid(
                    headers_key,
                    "must contain valid HTTP header names and values",
                ));
            }
            let lower = key.to_ascii_lowercase();
            if RESERVED_EXTRA_HEADERS.contains(&lower.as_str())
                || CREDENTIAL_WORDS.iter().any(|word| lower.contains(word))
            {
                return Err(invalid(
                    headers_key,
                    "credential-like and transport-owned headers are not accepted; use api_key_env or headers for credentials",
                ));
            }
            if !seen.insert(lower) {
                return Err(invalid(
                    headers_key,
                    "names must be unique case-insensitively and must not repeat a headers or environment-credential entry",
                ));
            }
        }
        Ok(())
    }
}
const CREDENTIAL_WORDS: [&str; 5] = ["api-key", "apikey", "secret", "token", "credential"];
const MAX_EXTRA_BODY_BYTES: usize = 64 * 1024;
const MAX_EXTRA_HEADERS: usize = 32;
const MAX_EXTRA_HEADER_VALUE_BYTES: usize = 8 * 1024;
/// Headers the transport or the credential path owns.
const RESERVED_EXTRA_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "host",
    "content-type",
    "content-length",
    "accept",
    "transfer-encoding",
    "connection",
    "x-shellboy-session-id",
];
fn contains_toml_datetime(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.keys().any(|key| key.starts_with("$__toml_private"))
                || map.values().any(contains_toml_datetime)
        }
        serde_json::Value::Array(items) => items.iter().any(contains_toml_datetime),
        _ => false,
    }
}
impl Limits {
    fn validate_relations(&self) -> Result<(), ConfigError> {
        if self.attachment_file_bytes > self.attachment_request_bytes {
            return Err(invalid(
                "limits.attachment_file_bytes",
                "must be <= attachment_request_bytes",
            ));
        }
        if self.data_warning_bytes >= self.data_max_bytes {
            return Err(invalid(
                "limits.data_warning_bytes",
                "must be < data_max_bytes",
            ));
        }
        Ok(())
    }
}
impl Io {
    fn validate_relations(&self) -> Result<(), ConfigError> {
        if self.output_batch_bytes > self.output_buffer_bytes {
            return Err(invalid(
                "io.output_batch_bytes",
                "must be <= output_buffer_bytes",
            ));
        }
        Ok(())
    }
}
impl Workers {
    fn validate_capacity(&self, max: usize) -> Result<(), ConfigError> {
        for (key, count) in [
            ("workers.attachment", self.attachment),
            ("workers.control_file", self.control_file),
        ] {
            if count > max {
                return Err(invalid(key, "exceeds the supported worker permit range"));
            }
        }
        Ok(())
    }
}
impl Indexing {
    fn validate_positive(&self) -> Result<(), ConfigError> {
        if self.concurrency == 0 {
            return Err(invalid(
                "indexing.concurrency",
                "must be a positive integer",
            ));
        }
        if self.split_chars == 0 {
            return Err(invalid(
                "indexing.split_chars",
                "must be a positive integer",
            ));
        }
        if self.common_failure_pause_count == 0 {
            return Err(invalid(
                "indexing.common_failure_pause_count",
                "must be a positive integer",
            ));
        }
        Ok(())
    }
    fn validate_capacity(&self, max: usize) -> Result<(), ConfigError> {
        if self.concurrency > max {
            return Err(invalid(
                "indexing.concurrency",
                "exceeds the supported worker permit range",
            ));
        }
        Ok(())
    }
    fn validate_retries(&self) -> Result<(), ConfigError> {
        if self.retry_delays_ms.len() != self.retries || self.retry_delays_ms.contains(&0) {
            return Err(invalid(
                "indexing.retry_delays_ms",
                "must contain exactly retries positive delays",
            ));
        }
        Ok(())
    }
}
impl Shell {
    fn validate_env(&self) -> Result<(), ConfigError> {
        for key in self.env.keys() {
            if !valid_env_name(key) {
                return Err(invalid(
                    "shell.env",
                    "keys must be environment variable names",
                ));
            }
        }
        if self.env.values().any(|v| v.contains('\0')) {
            return Err(invalid("shell.env", "values must not contain NUL"));
        }
        Ok(())
    }
    fn validate_executable(&self) -> Result<(), ConfigError> {
        if self
            .executable
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(invalid("shell.executable", "must not be empty"));
        }
        Ok(())
    }
}
impl Ui {
    fn validate_positive(&self) -> Result<(), ConfigError> {
        if self.max_fps == 0 {
            return Err(invalid("ui.max_fps", "must be a positive integer"));
        }
        if !(1..=10).contains(&self.live_work_rows) {
            return Err(invalid("ui.live_work_rows", "must be in 1..=10"));
        }
        if !(1..=10).contains(&self.held_input_rows) {
            return Err(invalid("ui.held_input_rows", "must be in 1..=10"));
        }
        if self.paste_fold_chars == 0 {
            return Err(invalid("ui.paste_fold_chars", "must be a positive integer"));
        }
        if self.paste_fold_lines == 0 {
            return Err(invalid("ui.paste_fold_lines", "must be a positive integer"));
        }
        Ok(())
    }
    fn validate_keys(&self) -> Result<(), ConfigError> {
        if !valid_key(&self.send_key)
            || self.newline_keys.is_empty()
            || self.newline_keys.iter().any(|key| !valid_key(key))
        {
            return Err(invalid(
                "ui.send_key/newline_keys",
                "use a supported key name such as Enter, Ctrl+J, or Shift+Enter",
            ));
        }
        if self
            .newline_keys
            .iter()
            .any(|key| key.eq_ignore_ascii_case(&self.send_key))
        {
            return Err(invalid("ui.newline_keys", "must not contain send_key"));
        }
        Ok(())
    }
}
