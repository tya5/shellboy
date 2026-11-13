//! Strict v1 configuration. File loading is blocking and must run before the UI
//! event loop or on a bounded file worker, never on the UI main thread.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};
use thiserror::Error;
use url::Url;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config_invalid: {key}: {message}")]
    Invalid { key: String, message: String },
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot resolve {0}; provide --config or --data-dir explicitly")]
    MissingPath(&'static str),
    #[error("connection credential environment variable {0} is missing or empty")]
    MissingCredential(String),
    #[error(
        "connection sends its key in api_key_header, and neither an api_key_env variable nor a stored key supplies one"
    )]
    MissingKey,
}
fn invalid(key: impl Into<String>, message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        key: key.into(),
        message: message.into(),
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Startup-only source facts; never accepted from TOML or persisted.
    #[serde(skip)]
    pub source: Option<ConfigSource>,
    /// Names of connections with a stored key (#269): read at load and updated by
    /// `credentials.set` / `credentials.clear`, so setup hints need no file I/O.
    /// Request-time lookup always reads the file.
    #[serde(skip)]
    pub stored_keys: StoredKeys,
    #[serde(skip)]
    pub live_connections: LiveConnections,
    pub data_dir: Option<PathBuf>,
    pub extra_instructions: Vec<PathBuf>,
    pub default_connection: Option<String>,
    pub default_model: Option<String>,
    pub connections: BTreeMap<String, Connection>,
    pub embedding: Embedding,
    pub shell: Shell,
    pub retention: Retention,
    pub limits: Limits,
    pub cache: Cache,
    pub io: Io,
    pub workers: Workers,
    pub indexing: Indexing,
    pub ui: Ui,
    pub logging: Logging,
    pub compaction: Compaction,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    #[default]
    Responses,
    ChatCompletions,
    AnthropicMessages,
}
/// Default `max_tokens` for `anthropic_messages`, which requires one. Below the
/// documented 128K output cap of current models, and large enough that routine
/// turns do not stop at the limit.
pub const DEFAULT_MESSAGES_MAX_TOKENS: u64 = 32_768;
impl Api {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
            Self::AnthropicMessages => "anthropic_messages",
        }
    }
    /// Top-level request-body fields Shellboy itself owns for this dialect.
    /// `extra_body` may not contain them; there is no precedence rule to learn.
    pub fn reserved_body_fields(self) -> &'static [&'static str] {
        match self {
            Self::Responses => &[
                "model",
                "input",
                "tools",
                "stream",
                "store",
                "include",
                "reasoning",
                "previous_response_id",
                "prompt_cache_key",
                "prompt_cache_options",
            ],
            Self::ChatCompletions => &[
                "model",
                "messages",
                "tools",
                "stream",
                "stream_options",
                "reasoning_effort",
            ],
            Self::AnthropicMessages => &[
                "model",
                "max_tokens",
                "stream",
                "messages",
                "system",
                "tools",
                "output_config",
                "cache_control",
            ],
        }
    }
    pub fn unavailable_features(self) -> Vec<String> {
        match self {
            Self::Responses => vec![],
            Self::ChatCompletions => [
                "reasoning_item_replay",
                "encrypted_reasoning",
                "reasoning_content",
                "prompt_cache_key",
            ]
            .map(str::to_owned)
            .to_vec(),
            Self::AnthropicMessages => ["reasoning_item_replay", "prompt_cache_key"]
                .map(str::to_owned)
                .to_vec(),
        }
    }
}

/// Role used on the wire for context Shellboy inserts into a conversation
/// (event notices, context changes, instructions). Saved history is unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRole {
    /// Send them as `developer` messages (the meaning OpenAI-style APIs expect).
    #[default]
    Developer,
    /// Send them as `user` messages, for providers behind a gateway that moves
    /// developer messages to the system prompt and then rejects a conversation
    /// that does not end with a user message.
    User,
}

/// Default names of the headers that carry the Thread UUID. The second is the
/// session header some gateways read; other providers ignore unknown headers.
pub const DEFAULT_SESSION_HEADER_NAMES: [&str; 2] =
    ["X-Shellboy-Session-Id", "x-litellm-session-id"];

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Connection {
    pub api: Api,
    pub base_url: String,
    pub api_key_env: Option<String>,
    /// Raw environment key in this header; None keeps Authorization: Bearer.
    pub api_key_header: Option<String>,
    pub default_effort: Option<String>,
    pub context_role: ContextRole,
    /// Output limit in each API's native field; only Messages defaults when unset.
    pub max_tokens: Option<u64>,
    pub headers: BTreeMap<String, String>,
    /// Provider-specific optional request fields, merged into conversation
    /// request bodies without interpretation. See `validate`.
    pub extra_body: serde_json::Map<String, serde_json::Value>,
    /// Provider-specific optional headers for conversation requests only.
    pub extra_headers: BTreeMap<String, String>,
    pub ssl_verify: bool,
    pub session_header: bool,
    /// Header names that each carry the Thread UUID when `session_header` is on.
    pub session_header_names: Vec<String>,
    pub prompt_cache_key: bool,
    pub prompt_cache_diagnostics: bool,
    /// Send the dialect's cache markers for the common cache hint (#127).
    /// Default on; `false` sends nothing cache-related.
    pub prompt_caching: bool,
    pub response_header_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub response_max_retries: u32,
    pub response_retry_max_delay_ms: u64,
    pub response_retry_partial: bool,
    pub auto_trim_on_disconnect: bool,
    /// Overrides `[compaction] auto_at_tokens` for this connection; 0 disables.
    pub auto_at_tokens: Option<u64>,
    #[serde(default, deserialize_with = "target_percent_option")]
    pub auto_compact_target_percent: Option<u64>,
    #[serde(default, deserialize_with = "summary_chars_option")]
    pub summary_max_chars: Option<u64>,
    /// The `credentials.toml` beside the loaded configuration file (#269); never
    /// accepted from TOML. `None` when the connection did not come from a file.
    #[serde(skip)]
    pub credentials_file: Option<PathBuf>,
    /// Which connections have a stored key, shared with `Config::stored_keys`. A request opens
    /// `credentials.toml` only when this lists its connection, so a connection without a stored key
    /// pays no file read (and no extra scheduling point) per request.
    #[serde(skip)]
    pub stored_keys: StoredKeys,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            api: Api::default(),
            base_url: String::new(),
            api_key_env: None,
            api_key_header: None,
            default_effort: None,
            context_role: ContextRole::default(),
            max_tokens: None,
            headers: BTreeMap::new(),
            extra_body: serde_json::Map::new(),
            extra_headers: BTreeMap::new(),
            ssl_verify: true,
            session_header: true,
            session_header_names: DEFAULT_SESSION_HEADER_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            prompt_cache_key: false,
            prompt_cache_diagnostics: false,
            prompt_caching: true,
            response_header_timeout_ms: 120_000,
            stream_idle_timeout_ms: 600_000,
            request_timeout_ms: 60_000,
            response_max_retries: 8,
            response_retry_max_delay_ms: 60_000,
            response_retry_partial: true,
            auto_trim_on_disconnect: true,
            auto_at_tokens: None,
            auto_compact_target_percent: None,
            summary_max_chars: None,
            credentials_file: None,
            stored_keys: StoredKeys::default(),
        }
    }
}
impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("api", &self.api)
            .field("base_url", &"[redacted]")
            .field("api_key_env", &self.api_key_env)
            .field("api_key_header", &self.api_key_header)
            .field("default_effort", &self.default_effort)
            .field("context_role", &self.context_role)
            .field("max_tokens", &self.max_tokens)
            .field("headers", &"[redacted]")
            .field("extra_body", &"[redacted]")
            .field("extra_headers", &"[redacted]")
            .field("ssl_verify", &self.ssl_verify)
            .field("session_header", &self.session_header)
            .field("session_header_names", &self.session_header_names)
            .field("prompt_cache_key", &self.prompt_cache_key)
            .field("prompt_cache_diagnostics", &self.prompt_cache_diagnostics)
            .field("prompt_caching", &self.prompt_caching)
            .field(
                "response_header_timeout_ms",
                &self.response_header_timeout_ms,
            )
            .field("stream_idle_timeout_ms", &self.stream_idle_timeout_ms)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("response_max_retries", &self.response_max_retries)
            .field(
                "response_retry_max_delay_ms",
                &self.response_retry_max_delay_ms,
            )
            .field("response_retry_partial", &self.response_retry_partial)
            .field("auto_trim_on_disconnect", &self.auto_trim_on_disconnect)
            .field("auto_at_tokens", &self.auto_at_tokens)
            .field(
                "auto_compact_target_percent",
                &self.auto_compact_target_percent,
            )
            .field("summary_max_chars", &self.summary_max_chars)
            .field("credentials_file", &self.credentials_file)
            .finish()
    }
}
#[derive(Clone, Copy, Debug)]
pub enum ApiEndpoint {
    Responses,
    Embeddings,
    Models,
}
impl Connection {
    /// The output limit a conversation request sends: the Thread's own value,
    /// then this connection's `max_tokens`. `None` sends no limit field (the
    /// provider decides); `anthropic_messages` requires one and uses
    /// [`DEFAULT_MESSAGES_MAX_TOKENS`].
    pub fn effective_max_tokens(&self, thread: Option<u64>) -> Option<u64> {
        thread
            .or(self.max_tokens)
            .or((self.api == Api::AnthropicMessages).then_some(DEFAULT_MESSAGES_MAX_TOKENS))
    }
    pub fn endpoint(&self, endpoint: ApiEndpoint) -> Result<Url, ConfigError> {
        let mut url = api_root(&self.base_url)?;
        let suffix = match endpoint {
            ApiEndpoint::Responses => match self.api {
                Api::Responses => "responses",
                Api::ChatCompletions => "chat/completions",
                Api::AnthropicMessages => "messages",
            },
            ApiEndpoint::Embeddings => "embeddings",
            ApiEndpoint::Models => "models",
        };
        let path = format!("{}/{suffix}", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(url)
    }
    /// Resolve credentials only immediately before beginning this connection.
    /// No environment values are included in errors or Debug output.
    /// SHA-256 over the canonical JSON of the extra fields actually sent, or
    /// `None` when neither is configured. Contents stay in the config file.
    pub fn extra_digest(&self) -> Option<String> {
        if self.extra_body.is_empty() && self.extra_headers.is_empty() {
            return None;
        }
        let canonical = serde_json::json!({
            "extra_body": self.extra_body,
            "extra_headers": self.extra_headers,
        });
        Some(crate::digest::sha256_hex(canonical.to_string()))
    }
    pub fn credentials(
        &self,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ConnectionCredentials, ConfigError> {
        self.credentials_with(env, None)
    }
    /// A stored key is used only when the environment supplies none (#269).
    pub fn credentials_with(
        &self,
        env: impl Fn(&str) -> Option<String>,
        stored: Option<&str>,
    ) -> Result<ConnectionCredentials, ConfigError> {
        let mut headers = self.headers.clone();
        let from_env = self
            .api_key_env
            .as_deref()
            .and_then(env)
            .filter(|v| !v.trim().is_empty());
        let stored = stored
            .filter(|v| !v.trim().is_empty() && self.stored_key_allowed())
            .map(str::to_owned);
        match (from_env.or(stored), &self.api_key_env) {
            (Some(key), _) => {
                if key.contains(['\r', '\n']) {
                    return Err(invalid(
                        "api_key_env",
                        "credential must not contain a newline",
                    ));
                }
                let (header, value) = match &self.api_key_header {
                    Some(header) => (header.clone(), key),
                    None => ("Authorization".into(), format!("Bearer {key}")),
                };
                headers.insert(header, value);
            }
            (None, Some(name)) => return Err(ConfigError::MissingCredential(name.clone())),
            // A named header promises a key; sending the request without it would only fail upstream.
            (None, None) if self.api_key_header.is_some() => return Err(ConfigError::MissingKey),
            (None, None) => {}
        }
        Ok(ConnectionCredentials { headers })
    }
    /// A stored key goes into the credential header. Without `api_key_env` that
    /// header is `Authorization`, which `headers` may already define; then the
    /// explicit header wins and no key is stored.
    pub fn stored_key_allowed(&self) -> bool {
        self.api_key_env.is_some()
            || self.api_key_header.is_some()
            || !self
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("authorization"))
    }
    /// Which source supplies the key now. A fact about presence, not validity.
    pub fn key_source(
        &self,
        env: impl Fn(&str) -> Option<String>,
        stored_present: bool,
    ) -> KeySource {
        if self
            .api_key_env
            .as_deref()
            .and_then(env)
            .is_some_and(|v| !v.trim().is_empty())
        {
            KeySource::Env
        } else if stored_present && self.stored_key_allowed() {
            KeySource::Stored
        } else {
            KeySource::None
        }
    }
}
/// Where a connection's API key comes from (#269). Never the value or its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    Env,
    Stored,
    None,
}
/// Connection names that have a stored key. Clones of a `Config` share it.
#[derive(Clone, Default)]
pub struct StoredKeys(std::sync::Arc<std::sync::RwLock<std::collections::BTreeSet<String>>>);
impl StoredKeys {
    pub fn contains(&self, name: &str) -> bool {
        self.0.read().is_ok_and(|names| names.contains(name))
    }
    pub fn replace(&self, names: std::collections::BTreeSet<String>) {
        if let Ok(mut current) = self.0.write() {
            *current = names;
        }
    }
    pub fn set(&self, name: &str, stored: bool) {
        if let Ok(mut names) = self.0.write() {
            if stored {
                names.insert(name.to_owned());
            } else {
                names.remove(name);
            }
        }
    }
}
impl fmt::Debug for StoredKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredKeys")
    }
}
pub struct ConnectionCredentials {
    headers: BTreeMap<String, String>,
}
impl ConnectionCredentials {
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }
}
impl fmt::Debug for ConnectionCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionCredentials([redacted])")
    }
}
fn api_root(value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| {
        invalid(
            "base_url",
            "provide an absolute HTTP or HTTPS API root, e.g. https://proxy.example/v1",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "base_url",
            "HTTP(S) API root must have a host and no credentials, query, or fragment",
        ));
    }
    let final_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    if matches!(
        final_segment,
        "responses" | "embeddings" | "models" | "completions"
    ) {
        return Err(invalid(
            "base_url",
            "provide the API root, e.g. https://proxy.example/v1; remove /responses, /embeddings, /models or /chat/completions (not https://proxy.example/v1/responses)",
        ));
    }
    Ok(url)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Embedding {
    pub connection: Option<String>,
    pub model: Option<String>,
    pub batch_max_inputs: usize,
    pub batch_max_chars: usize,
}
impl Default for Embedding {
    fn default() -> Self {
        Self {
            connection: None,
            model: None,
            batch_max_inputs: 1,
            batch_max_chars: 64_000,
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    Bash,
    Powershell,
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    pub describe_calls: bool,
    pub kind: ShellKind,
    pub executable: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}
impl Default for Shell {
    fn default() -> Self {
        Self {
            describe_calls: true,
            kind: if cfg!(windows) {
                ShellKind::Powershell
            } else {
                ShellKind::Bash
            },
            executable: None,
            env: BTreeMap::new(),
        }
    }
}
impl fmt::Debug for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shell")
            .field("kind", &self.kind)
            .field("executable", &self.executable)
            .field("describe_calls", &self.describe_calls)
            .field("env", &"[redacted]")
            .finish()
    }
}
macro_rules! section {
    // All-numeric sections whose fields are all strictly positive need no
    // second parallel field list. Mixed/zero-allowed sections stay explicit.
    (positive $key:literal, $name:ident { $($field:ident : $ty:ty = $default:expr),* $(,)? }) => {
        section!(positive $key, $name { $($field: $ty = $default),* } optional {});
    };
    (positive $key:literal, $name:ident { $($field:ident : $ty:ty = $default:expr),* $(,)? } optional { $($optional:ident : $optional_ty:ty = $optional_default:expr),* $(,)? }) => {
        section!($name { $($field: $ty = $default,)* $($optional: $optional_ty = $optional_default),* });
        impl $name {
            fn validate_positive(&self) -> Result<(), ConfigError> {
                $(if self.$field == 0 { return Err(invalid(concat!($key, ".", stringify!($field)), "must be a positive integer")); })*
                Ok(())
            }
        }
    };
    // `config.get` serializes these sections whole. A value that could hold a
    // secret belongs in `Connection` or `Shell`, whose fields it lists one by one.
    ($name:ident { $($field:ident : $ty:ty = $default:expr),* $(,)? }) => {
        #[derive(Clone, Debug, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name { $(pub $field: $ty,)* }
        impl Default for $name { fn default() -> Self { Self { $($field: $default,)* } } }
    };
}
section!(positive "retention", Retention {
    tool_raw_days: u64 = 7,
    tool_raw_bytes: u64 = GIB,
    clipboard_days: u64 = 7,
    clipboard_bytes: u64 = 256 * MIB
});
section!(positive "limits", Limits {
    attachment_file_bytes: u64 = 20 * MIB,
    attachment_request_bytes: u64 = 40 * MIB,
    shell_raw_bytes: u64 = 64 * MIB,
    tool_excerpt_bytes: u64 = 16 * 1024,
    shell_stdin_bytes: u64 = MIB,
    draft_bytes: u64 = MIB,
    data_warning_bytes: u64 = 40 * GIB,
    data_max_bytes: u64 = 50 * GIB,
    disk_free_min_bytes: u64 = 5 * GIB
});
section!(Cache {
    body_bytes: u64 = 32 * MIB,
    layout_bytes: u64 = 16 * MIB,
    history_bytes: u64 = 32 * MIB,
    attachment_base64_bytes: u64 = 64 * MIB
});
section!(positive "io", Io {
    file_prepare_timeout_ms: u64 = 30_000,
    shell_write_timeout_ms: u64 = 10_000,
    term_grace_ms: u64 = 2_000,
    drain_timeout_ms: u64 = 1_000,
    self_reply_grace_ms: u64 = 250,
    shutdown_grace_ms: u64 = 5_000,
    output_buffer_bytes: u64 = 8 * MIB,
    output_batch_bytes: u64 = 64 * 1024,
    output_batch_ms: u64 = 25
});
section!(positive "workers", Workers {
    attachment: usize = 2,
    control_file: usize = 1
});
/// Node kinds that carry searchable text (`history::search_bodies`). Same words
/// as `history search --kind`; kinds not listed never have an embeddable body.
pub const INDEXABLE_KINDS: [&str; 6] = [
    "user",
    "summary",
    "assistant_response",
    "tool_result",
    "spill",
    "event",
];
/// Kinds that automatic indexing takes in unless `indexing.auto_kinds` says otherwise.
pub fn default_auto_kinds() -> Vec<String> {
    vec!["user".into(), "summary".into(), "assistant_response".into()]
}
section!(Indexing { auto_history: bool = true, auto_kinds: Vec<String> = default_auto_kinds(), concurrency: usize = 1, split_chars: usize = 2_000, retries: usize = 2, common_failure_pause_count: usize = 3, retry_delays_ms: Vec<u64> = vec![1_000, 4_000] });
impl Indexing {
    /// Searchable kinds that automatic indexing leaves out. Their text is not
    /// complete in semantic results; keyword search reads the originals.
    pub fn kinds_not_auto_indexed(&self) -> Vec<&'static str> {
        INDEXABLE_KINDS
            .into_iter()
            .filter(|kind| !self.auto_includes(kind))
            .collect()
    }
    /// Whether automatic indexing takes in nodes of this kind.
    pub fn auto_includes(&self, kind: &str) -> bool {
        self.auto_kinds.iter().any(|k| k == kind)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CopyBackend {
    Auto,
    Os,
    Osc52,
}
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "lowercase")]
pub enum PaletteMode {
    #[default]
    Dark,
    Light,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgramStatus {
    #[default]
    On,
    Off,
}
/// UI language is chosen once from configuration and never from the OS locale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiLanguage {
    #[default]
    En,
    Ja,
}
/// Display motion only: `off` keeps every decoration on its static mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MotionMode {
    #[default]
    On,
    Off,
}
section!(Ui { language: UiLanguage = UiLanguage::En, program_status: ProgramStatus = ProgramStatus::On, motion: MotionMode = MotionMode::On, bell: bool = true, clean_redraw: bool = false, max_fps: u32 = 30, live_work_rows: usize = 3, held_input_rows: usize = 3, awaiting_human_rows: usize = 1, paste_fold_chars: usize = 1_000, paste_fold_lines: usize = 3, send_key: String = "Enter".into(), newline_keys: Vec<String> = vec!["Ctrl+J".into(), "Shift+Enter".into()], copy_backend: CopyBackend = CopyBackend::Auto, palette: PaletteMode = PaletteMode::Dark });
section!(positive "logging", Logging {
    file_bytes: u64 = 5 * MIB,
    files: usize = 3
});

mod validation;

/// Shared with the owner's HTTP registry; never loaded from TOML or serialized.
#[derive(Clone, Default)]
pub struct LiveConnections(
    std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::model::http::HttpClients>>>,
);
impl std::fmt::Debug for LiveConnections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LiveConnections")
    }
}
impl LiveConnections {
    pub(crate) fn clients(
        &self,
        config: &Config,
    ) -> std::sync::Arc<crate::model::http::HttpClients> {
        self.0
            .get_or_init(|| std::sync::Arc::new(crate::model::http::HttpClients::new(config)))
            .clone()
    }
}
impl Config {
    /// One registry generation, with startup-only settings left untouched. No I/O.
    pub(crate) fn current(&self) -> Self {
        let mut current = self.clone();
        if let Some(clients) = self.live_connections.0.get() {
            let snapshot = clients.snapshot();
            current.connections = snapshot.connections.clone();
            current.default_connection = snapshot.default_connection.clone();
            current.default_model = snapshot.default_model.clone();
            current.extra_instructions = snapshot.extra_instructions.clone();
        }
        current
    }
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        // Do not embed TOML's rendered error: it can quote secret header values.
        let config: Self = toml::from_str(source).map_err(|error: toml::de::Error| {
            let location = error
                .span()
                .map(|s| format!(" at bytes {}..{}", s.start, s.end))
                .unwrap_or_default();
            // message() omits source excerpts but can still contain arbitrary values.
            let message = error.message();
            let reason = if message.starts_with("unknown field") {
                message
                    .split(',')
                    .next()
                    .unwrap_or("unknown field")
                    .to_owned()
            } else {
                "invalid TOML syntax or field type".into()
            };
            invalid(
                diagnostic_key(source, error.span().map(|s| s.start)),
                format!("{reason}{location}; check the documented key and type"),
            )
        })?;
        config.validate()?;
        Ok(config)
    }
    /// Blocking filesystem operation; call before UI startup or on a file worker.
    pub fn load(
        config_path: Option<&Path>,
        startup_cwd: &Path,
    ) -> Result<LoadedConfig, ConfigError> {
        Self::load_with_env(config_path, startup_cwd, &PathEnvironment::current())
    }
    pub fn load_with_env(
        config_path: Option<&Path>,
        startup_cwd: &Path,
        env: &PathEnvironment,
    ) -> Result<LoadedConfig, ConfigError> {
        Self::load_with_overrides(config_path, None, startup_cwd, env)
    }
    /// Owner loader including CLI data-dir; performs blocking file I/O.
    pub fn load_with_overrides(
        config_path: Option<&Path>,
        cli_data_dir: Option<&Path>,
        startup_cwd: &Path,
        env: &PathEnvironment,
    ) -> Result<LoadedConfig, ConfigError> {
        if !startup_cwd.is_absolute() {
            return Err(invalid("startup_cwd", "must be absolute"));
        }
        let path = resolve_config_path(config_path, startup_cwd, env)?;
        let (mut config, actual_path, content_hash) = match std::fs::read_to_string(&path) {
            Ok(source) => (
                Self::parse(&source)?,
                Some(path.clone()),
                Some(crate::digest::sha256_hex(&source)),
            ),
            Err(error) if config_path.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                (Self::default(), None, None)
            }
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        let parent = path.parent().unwrap_or(startup_cwd);
        if let Some(executable) = &mut config.shell.executable {
            *executable = absolute(executable, parent);
        }
        let data_dir = resolve_data_dir(
            cli_data_dir,
            env,
            config.data_dir.as_deref(),
            startup_cwd,
            parent,
        )?;
        config.data_dir = Some(data_dir.clone());
        let credentials_file = crate::credential_store::path_beside(&path);
        for connection in config.connections.values_mut() {
            connection.credentials_file = Some(credentials_file.clone());
            connection.stored_keys = config.stored_keys.clone();
        }
        // An unreadable or invalid file is reported by `config get`; a request that
        // needs the key fails then, so the load itself ignores it.
        if let Ok(names) = crate::credential_store::stored_names(&credentials_file) {
            config.stored_keys.replace(names);
        }
        config.source = Some(ConfigSource {
            path,
            present: actual_path.is_some(),
            content_hash,
        });
        Ok(LoadedConfig {
            config,
            config_path: actual_path,
            data_dir,
        })
    }
}
// Only extract simple setting names; never quote values or source lines.
fn diagnostic_key(source: &str, offset: Option<usize>) -> String {
    let Some(offset) = offset.filter(|o| source.is_char_boundary(*o)) else {
        return "TOML".into();
    };
    let before = &source[..offset];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = &source[line_start..];
    let valid = |name: &str| {
        !name.is_empty()
            && name.len() < 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    };
    let field = line
        .split_once('=')
        .map(|(key, _)| key.trim())
        .filter(|key| valid(key));
    let table = before.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
            .filter(|name| valid(name))
    });
    match (table, field) {
        (Some(table), Some(field)) => format!("{table}.{field}"),
        (_, Some(field)) => field.into(),
        (Some(table), _) => table.into(),
        _ => "TOML".into(),
    }
}
fn nonempty(key: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        Err(invalid(key, "must not be empty"))
    } else {
        Ok(())
    }
}
pub(crate) fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
pub(crate) fn valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(header_name_byte)
}
fn header_name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)
}
fn valid_key(key: &str) -> bool {
    let parts: Vec<_> = key.split('+').collect();
    let Some((name, modifiers)) = parts.split_last() else {
        return false;
    };
    let mut seen = std::collections::BTreeSet::new();
    if modifiers.iter().any(|m| {
        !matches!(m.to_ascii_lowercase().as_str(), "ctrl" | "alt" | "shift")
            || !seen.insert(m.to_ascii_lowercase())
    }) {
        return false;
    }
    name.chars().count() == 1
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "enter"
                | "tab"
                | "backtab"
                | "backspace"
                | "delete"
                | "insert"
                | "esc"
                | "escape"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "up"
                | "down"
                | "left"
                | "right"
                | "space"
        )
        || name
            .strip_prefix('F')
            .and_then(|v| v.parse::<u8>().ok())
            .is_some_and(|n| (1..=24).contains(&n))
}

#[derive(Clone, Debug)]
pub struct ConfigSource {
    pub path: PathBuf,
    pub present: bool,
    /// SHA-256 of the bytes this configuration was parsed from.
    pub content_hash: Option<String>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub data_dir: PathBuf,
}
#[derive(Clone, Copy, Debug)]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}
#[derive(Clone, Debug)]
pub struct PathEnvironment {
    pub platform: Platform,
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub localappdata: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}
impl PathEnvironment {
    pub fn current() -> Self {
        fn var(name: &str) -> Option<PathBuf> {
            std::env::var_os(name)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        }
        Self {
            platform: if cfg!(windows) {
                Platform::Windows
            } else if cfg!(target_os = "macos") {
                Platform::Macos
            } else {
                Platform::Linux
            },
            home: var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
            appdata: var("APPDATA"),
            localappdata: var("LOCALAPPDATA"),
            data_dir: var("SHELLBOY_DATA_DIR"),
        }
    }
    pub fn default_config_path(&self) -> Result<PathBuf, ConfigError> {
        let path = match self.platform {
            Platform::Linux => self
                .xdg_config_home
                .clone()
                .or_else(|| self.home.as_ref().map(|h| h.join(".config")))
                .ok_or(ConfigError::MissingPath("XDG_CONFIG_HOME/HOME"))?
                .join("shellboy/config.toml"),
            Platform::Macos => self
                .home
                .as_ref()
                .ok_or(ConfigError::MissingPath("HOME"))?
                .join("Library/Application Support/shellboy/config.toml"),
            Platform::Windows => self
                .appdata
                .as_ref()
                .ok_or(ConfigError::MissingPath("APPDATA"))?
                .join("shellboy")
                .join("config.toml"),
        };
        require_absolute_default(path)
    }
    pub fn default_data_dir(&self) -> Result<PathBuf, ConfigError> {
        let path = match self.platform {
            Platform::Linux => self
                .xdg_data_home
                .clone()
                .or_else(|| self.home.as_ref().map(|h| h.join(".local/share")))
                .ok_or(ConfigError::MissingPath("XDG_DATA_HOME/HOME"))?
                .join("shellboy"),
            Platform::Macos => self
                .home
                .as_ref()
                .ok_or(ConfigError::MissingPath("HOME"))?
                .join("Library/Application Support/shellboy/data"),
            Platform::Windows => self
                .localappdata
                .as_ref()
                .ok_or(ConfigError::MissingPath("LOCALAPPDATA"))?
                .join("shellboy")
                .join("data"),
        };
        require_absolute_default(path)
    }
}
fn require_absolute_default(path: PathBuf) -> Result<PathBuf, ConfigError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(invalid(
            "OS paths",
            "environment paths must be absolute; provide an explicit path",
        ))
    }
}
pub(crate) fn absolute(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}
/// Resolve the selected file even when it has not been created yet.
pub fn resolve_config_path(
    path: Option<&Path>,
    cwd: &Path,
    env: &PathEnvironment,
) -> Result<PathBuf, ConfigError> {
    if !cwd.is_absolute() {
        return Err(invalid("startup_cwd", "must be absolute"));
    }
    match path {
        Some(path) => Ok(absolute(path, cwd)),
        None => env.default_config_path(),
    }
}
/// Owner precedence: CLI > SHELLBOY_DATA_DIR > TOML (relative to config) > OS.
pub fn resolve_data_dir(
    cli: Option<&Path>,
    env: &PathEnvironment,
    toml: Option<&Path>,
    startup_cwd: &Path,
    config_parent: &Path,
) -> Result<PathBuf, ConfigError> {
    if !startup_cwd.is_absolute() || !config_parent.is_absolute() {
        return Err(invalid(
            "paths",
            "startup_cwd and config parent must be absolute",
        ));
    }
    if let Some(path) = cli.or(env.data_dir.as_deref()) {
        if path.as_os_str().is_empty() {
            return Err(invalid("data_dir", "must not be empty"));
        }
        return Ok(absolute(path, startup_cwd));
    }
    if let Some(path) = toml {
        if path.as_os_str().is_empty() {
            return Err(invalid("data_dir", "must not be empty"));
        }
        return Ok(absolute(path, config_parent));
    }
    env.default_data_dir()
}
/// Connection CLI deliberately does not read the owner's TOML.
pub fn resolve_cli_data_dir(
    cli: Option<&Path>,
    env: &PathEnvironment,
    startup_cwd: &Path,
) -> Result<PathBuf, ConfigError> {
    resolve_data_dir(cli, env, None, startup_cwd, startup_cwd)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "CompactionFields")]
pub struct Compaction {
    pub summary_max_chars: Option<u64>,
    /// Proactive compaction threshold on the provider-reported input tokens of
    /// the last adopted attempt. 0 disables; connections may override it.
    pub auto_at_tokens: u64,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CompactionFields {
    #[serde(deserialize_with = "summary_chars_option")]
    summary_max_chars: Option<u64>,
    auto_at_tokens: Option<u64>,
}
impl From<CompactionFields> for Compaction {
    fn from(fields: CompactionFields) -> Self {
        Self {
            summary_max_chars: fields.summary_max_chars.or(Some(20_000)),
            auto_at_tokens: fields.auto_at_tokens.unwrap_or(DEFAULT_AUTO_AT_TOKENS),
        }
    }
}
impl Default for Compaction {
    fn default() -> Self {
        Self {
            summary_max_chars: Some(20_000),
            auto_at_tokens: DEFAULT_AUTO_AT_TOKENS,
        }
    }
}
fn bounded_setting<'de, D: serde::Deserializer<'de>>(
    d: D,
    max: u64,
) -> Result<Option<u64>, D::Error> {
    let value = Option::<u64>::deserialize(d)?;
    if value.is_some_and(|n| n == 0 || n > max) {
        return Err(serde::de::Error::custom(
            "compaction setting is out of range",
        ));
    }
    Ok(value)
}
fn target_percent_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    bounded_setting(d, 100)
}
fn summary_chars_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    bounded_setting(d, i64::MAX as u64)
}
/// Authoritative request settings: manual overrides only output size; Thread
/// overrides connection defaults. Callers must validate persisted inputs first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SummarySettings {
    pub target_percent: u64,
    pub output_characters: u64,
}
impl SummarySettings {
    pub fn resolve(
        thread_percent: Option<u64>,
        thread_characters: Option<u64>,
        connection: Option<&Connection>,
        manual_characters: Option<u64>,
        compaction: &Compaction,
    ) -> Self {
        Self {
            target_percent: thread_percent
                .or_else(|| connection.and_then(|c| c.auto_compact_target_percent))
                .unwrap_or(20),
            output_characters: manual_characters
                .or(thread_characters)
                .or_else(|| connection.and_then(|c| c.summary_max_chars))
                .or(compaction.summary_max_chars)
                .unwrap_or(20_000),
        }
    }
}
pub const DEFAULT_AUTO_AT_TOKENS: u64 = 200_000;
impl Compaction {
    pub fn valid(&self) -> bool {
        self.summary_max_chars != Some(0)
    }
    /// The Thread override wins, then the connection; 0 means proactive compaction is off.
    pub fn auto_at_tokens_for(&self, thread: Option<u64>, connection: Option<&Connection>) -> u64 {
        thread
            .or_else(|| connection.and_then(|c| c.auto_at_tokens))
            .unwrap_or(self.auto_at_tokens)
    }
}
