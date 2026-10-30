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
    pub data_dir: Option<PathBuf>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    #[default]
    Responses,
    ChatCompletions,
}
impl Api {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
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
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Connection {
    pub api: Api,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub default_effort: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub ssl_verify: bool,
    pub session_header: bool,
    pub prompt_cache_key: bool,
    pub response_header_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub response_max_retries: u32,
    pub response_retry_max_delay_ms: u64,
    pub response_retry_partial: bool,
    pub auto_trim_on_disconnect: bool,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            api: Api::default(),
            base_url: String::new(),
            api_key_env: None,
            default_effort: None,
            headers: BTreeMap::new(),
            ssl_verify: true,
            session_header: true,
            prompt_cache_key: false,
            response_header_timeout_ms: 120_000,
            stream_idle_timeout_ms: 600_000,
            request_timeout_ms: 60_000,
            response_max_retries: 8,
            response_retry_max_delay_ms: 60_000,
            response_retry_partial: true,
            auto_trim_on_disconnect: true,
        }
    }
}
impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("api", &self.api)
            .field("base_url", &"[redacted]")
            .field("api_key_env", &self.api_key_env)
            .field("default_effort", &self.default_effort)
            .field("headers", &"[redacted]")
            .field("ssl_verify", &self.ssl_verify)
            .field("session_header", &self.session_header)
            .field("prompt_cache_key", &self.prompt_cache_key)
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
    pub fn endpoint(&self, endpoint: ApiEndpoint) -> Result<Url, ConfigError> {
        let mut url = api_root(&self.base_url)?;
        let suffix = match endpoint {
            ApiEndpoint::Responses => match self.api {
                Api::Responses => "responses",
                Api::ChatCompletions => "chat/completions",
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
    pub fn credentials(
        &self,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ConnectionCredentials, ConfigError> {
        let mut headers = self.headers.clone();
        if let Some(name) = &self.api_key_env {
            let key = env(name)
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| ConfigError::MissingCredential(name.clone()))?;
            if key.contains(['\r', '\n']) {
                return Err(invalid(
                    "api_key_env",
                    "credential must not contain a newline",
                ));
            }
            headers.insert("Authorization".into(), format!("Bearer {key}"));
        }
        Ok(ConnectionCredentials { headers })
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

#[derive(Clone, Debug, Deserialize)]
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
    pub kind: ShellKind,
    pub executable: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}
impl Default for Shell {
    fn default() -> Self {
        Self {
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
    ($name:ident { $($field:ident : $ty:ty = $default:expr),* $(,)? }) => {
        #[derive(Clone, Debug, Deserialize)]
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
    data_warning_bytes: u64 = 8 * GIB,
    data_max_bytes: u64 = 10 * GIB
} optional {
    revisit_max_per_run: Option<u64> = None
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
section!(Indexing { auto_history: bool = true, concurrency: usize = 1, split_chars: usize = 2_000, retries: usize = 2, common_failure_pause_count: usize = 3, retry_delays_ms: Vec<u64> = vec![1_000, 4_000] });
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CopyBackend {
    Auto,
    Os,
    Osc52,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PaletteMode {
    #[default]
    Dark,
    Light,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgramStatus {
    #[default]
    On,
    Off,
}
section!(Ui { program_status: ProgramStatus = ProgramStatus::On, bell: bool = true, max_fps: u32 = 30, paste_fold_chars: usize = 1_000, paste_fold_lines: usize = 3, send_key: String = "Enter".into(), newline_keys: Vec<String> = vec!["Ctrl+J".into(), "Shift+Enter".into()], copy_backend: CopyBackend = CopyBackend::Auto, palette: PaletteMode = PaletteMode::Dark });
section!(positive "logging", Logging {
    file_bytes: u64 = 5 * MIB,
    files: usize = 3
});

mod validation;

impl Config {
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
        let (mut config, actual_path) = match std::fs::read_to_string(&path) {
            Ok(source) => (Self::parse(&source)?, Some(path.clone())),
            Err(error) if config_path.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                (Self::default(), None)
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
        config.source = Some(ConfigSource {
            path,
            present: actual_path.is_some(),
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
fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
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
fn absolute(path: &Path, base: &Path) -> PathBuf {
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
#[serde(deny_unknown_fields)]
pub struct Compaction {
    #[serde(default)]
    pub summary_max_chars: Option<u64>,
    #[serde(default)]
    pub summary_max_percent: Option<f64>,
}
impl Default for Compaction {
    fn default() -> Self {
        Self {
            summary_max_chars: Some(20_000),
            summary_max_percent: Some(5.0),
        }
    }
}
impl Compaction {
    pub fn valid(&self) -> bool {
        (self.summary_max_chars.is_some() || self.summary_max_percent.is_some())
            && self.summary_max_chars != Some(0)
            && self
                .summary_max_percent
                .is_none_or(|p| p.is_finite() && p > 0.0 && p <= 100.0)
    }
    pub fn summary_limit(&self, source: u64) -> u64 {
        let absolute = self.summary_max_chars.unwrap_or(u64::MAX);
        let relative = self
            .summary_max_percent
            .map(|p| (source as f64 * (p / 100.0)).floor() as u64)
            .unwrap_or(u64::MAX);
        absolute.min(relative).max(2_000)
    }
}
