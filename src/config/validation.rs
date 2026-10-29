//! Ordered configuration validation phases. Local invariants stay with their
//! section; Config alone resolves references between sections.
use super::*;

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Keep first-error precedence: numeric bounds, local relationships,
        // connection contents/references, then environment/path/key checks.
        self.retention.validate_positive()?;
        self.limits.validate_positive()?;
        self.io.validate_positive()?;
        self.workers.validate_positive()?;
        self.indexing.validate_positive()?;
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
        api_root(&self.base_url).map_err(|_| invalid(format!("connections.{name}.base_url"), "provide an HTTP(S) API root without credentials/query/fragment or /responses or /embeddings suffix"))?;
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
        if self.compact_hint_tokens == Some(0) {
            return Err(invalid(
                format!("connections.{name}.compact_hint_tokens"),
                "must be a positive integer when specified",
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
        if let Some(env) = &self.api_key_env {
            if !valid_env_name(env) {
                return Err(invalid(
                    format!("connections.{name}.api_key_env"),
                    "must be an environment variable name",
                ));
            }
            if self
                .headers
                .keys()
                .any(|key| key.eq_ignore_ascii_case("authorization"))
            {
                return Err(invalid(
                    format!("connections.{name}.headers"),
                    "Authorization and api_key_env cannot both be defined",
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
        Ok(())
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
