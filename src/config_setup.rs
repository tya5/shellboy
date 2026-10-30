//! Standalone setup and shared, credential-free first-run diagnostics.
use crate::{
    config::{Config, PathEnvironment},
    model::http::HttpModelClient,
    protocol::{ApiError, ErrorCode},
};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Subcommand)]
pub enum ConfigCommand {
    /// Write config.example.toml at the selected path; never overwrite.
    Init,
    /// Read configuration and GET the model catalog; no chat requests or owner.
    Check,
}

pub const TEMPLATE: &str = include_str!("../config.example.toml");
pub const MINIMAL: &str = "default_connection = \"proxy\"\ndefault_model = \"REPLACE_WITH_MODEL_ID\"\n[connections.proxy]\nbase_url = \"https://proxy.example/v1\"\napi_key_env = \"SHELLBOY_API_KEY\"";
pub const ROOT_HINT: &str = "Use the API root, e.g. https://proxy.example/v1, not https://proxy.example/v1/responses; Shellboy appends /models or /responses. Check the proxy's API-root path.";
pub const PRECEDENCE: &str = "config: --config > OS default; data-dir: --data-dir > SHELLBOY_DATA_DIR > TOML data_dir > OS default";
pub const NEXT: &str = "shellboy config init -> edit the selected file and set its api_key_env variable -> shellboy config check -> restart shellboy; guide: shellboy docs get --id shellboy/configuration (use the same --config/--data-dir options)";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupReport {
    pub config_path: Option<PathBuf>,
    pub config_file_present: bool,
    pub data_dir: Option<PathBuf>,
    pub missing: Vec<String>,
    pub credential_env: Option<String>,
    pub credential_set: Option<bool>,
    pub precedence: String,
    pub next: String,
}

pub fn credential_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

pub fn report(
    config: &Config,
    connection: Option<&str>,
    model: Option<&str>,
    env: impl Fn(&str) -> bool,
) -> SetupReport {
    let mut missing = Vec::new();
    let connection = connection.and_then(|name| config.connections.get(name));
    if connection.is_none() {
        missing.push("connection (default_connection or Thread connection)".into());
    }
    if model.is_none_or(|model| model.trim().is_empty() || model == "REPLACE_WITH_MODEL_ID") {
        missing.push("model (default_model or Thread model)".into());
    }
    if connection.is_some_and(|connection| {
        connection.base_url.trim_end_matches('/') == "https://proxy.example/v1"
    }) {
        missing.push("base_url (replace the template API root)".into());
    }
    let credential_env = connection.and_then(|connection| connection.api_key_env.clone());
    let credential_set = credential_env.as_deref().map(env);
    if credential_set == Some(false) {
        missing.push(format!(
            "API key environment variable {} (missing or empty)",
            credential_env.as_deref().unwrap_or_default()
        ));
    }
    SetupReport {
        config_path: config.source.as_ref().map(|source| source.path.clone()),
        config_file_present: config.source.as_ref().is_some_and(|source| source.present),
        data_dir: config.data_dir.clone(),
        missing,
        credential_env,
        credential_set,
        precedence: PRECEDENCE.into(),
        next: NEXT.into(),
    }
}

impl SetupReport {
    pub fn japanese(&self, example: bool) -> String {
        let mut facts = Vec::new();
        if self.missing.iter().any(|m| m.starts_with("connection")) {
            facts.push("接続が未設定です".to_owned());
        }
        if self.missing.iter().any(|m| m.starts_with("model")) {
            facts.push("モデルが未設定です".to_owned());
        }
        if self.missing.iter().any(|m| m.starts_with("base_url")) {
            facts.push("base_url が雛形のままです。接続先の API root に変更してください".into());
        }
        if self.credential_set == Some(false) {
            facts.push(format!(
                "API key の環境変数 {} が未設定または空です（値は表示しません）",
                self.credential_env.as_deref().unwrap_or_default()
            ));
        }
        if facts.is_empty() {
            facts.push("設定を確認してください".into());
        }
        let path = self
            .config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "--config で指定した設定ファイル".into());
        let mut text = format!(
            "設定が必要です: {}\n設定ファイル: {path}\n",
            facts.join(" · ")
        );
        if !self.config_file_present {
            text.push_str("設定ファイルは読み込まれていません。\n");
        }
        if example {
            text.push_str(MINIMAL);
            text.push('\n');
        }
        text.push_str("同じ --config を指定して config init → 設定を編集・環境変数を設定 → config check → owner を再起動してください。\n");
        text.push_str("shellboy config init / shellboy config check\nガイド: shellboy docs get --id shellboy/configuration\n優先: --config > OS既定 / --data-dir > SHELLBOY_DATA_DIR > TOML > OS既定");
        text
    }
}

pub fn enrich(
    config: &Config,
    connection: Option<&str>,
    model: Option<&str>,
    mut error: ApiError,
) -> ApiError {
    if error.code == ErrorCode::ConfigInvalid
        || error.details.as_ref().is_some_and(|details| {
            matches!(
                details["failure_kind"].as_str(),
                Some("config_invalid" | "config_missing")
            )
        })
    {
        let setup = report(config, connection, model, credential_set);
        let details = error.details.get_or_insert_with(|| json!({}));
        if let Some(details) = details.as_object_mut() {
            details.insert("setup".into(), json!(setup));
        }
        error.hint = Some(NEXT.into());
    }
    error
}

pub fn tui_error(error: &ApiError) -> String {
    error
        .details
        .as_ref()
        .and_then(|d| d.get("setup"))
        .and_then(|value| serde_json::from_value::<SetupReport>(value.clone()).ok())
        .map_or_else(|| error.message.clone(), |report| report.japanese(false))
}

pub fn selected_path(
    path: Option<&Path>,
    cwd: &Path,
    env: &PathEnvironment,
) -> Result<PathBuf, ApiError> {
    crate::config::resolve_config_path(path, cwd, env)
        .map_err(|error| ApiError::new(ErrorCode::ConfigInvalid, error.to_string()))
}

pub async fn execute(
    command: ConfigCommand,
    path: Option<PathBuf>,
    data: Option<PathBuf>,
    cwd: &Path,
) -> Result<Value, ApiError> {
    let cwd = cwd.to_path_buf();
    let selected = selected_path(path.as_deref(), &cwd, &PathEnvironment::current())?;
    if matches!(command, ConfigCommand::Init) {
        return tokio::task::spawn_blocking(move || init(&selected))
            .await
            .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Configuration file worker failed"))?;
    }
    let error_path = selected.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        Config::load_with_overrides(
            path.as_deref(),
            data.as_deref(),
            &cwd,
            &PathEnvironment::current(),
        )
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Configuration file worker failed"))?
    .map_err(|error| {
        ApiError::new(ErrorCode::ConfigInvalid, error.to_string())
            .hint(format!("{ROOT_HINT} {NEXT}"))
            .details(json!({"config_path":error_path,"precedence":PRECEDENCE}))
    })?;
    let config = loaded.config;
    let setup = report(
        &config,
        config.default_connection.as_deref(),
        config.default_model.as_deref(),
        credential_set,
    );
    if !setup.missing.is_empty() || !setup.config_file_present {
        return Err(ApiError::new(
            ErrorCode::ConfigInvalid,
            "Configuration is incomplete; inspect details.setup.missing",
        )
        .hint(NEXT)
        .details(json!({"setup":setup})));
    }
    let name = config
        .default_connection
        .as_deref()
        .expect("reported connection");
    let client =
        HttpModelClient::for_connection(name, config.connections[name].clone()).map_err(|_| {
            ApiError::new(
                ErrorCode::ConfigInvalid,
                "Cannot prepare model catalog client",
            )
            .details(json!({"setup":setup}))
        })?;
    let models = client.models(CancellationToken::new()).await.map_err(|error| {
        let message = match error.status { Some(status) => format!("Model catalog request failed: HTTP {status}"), None => format!("Model catalog request failed ({})", error.kind) };
        ApiError::new(ErrorCode::IoFailed, message)
            .hint(if error.status == Some(404) { ROOT_HINT } else { "Check the configured endpoint, credential environment variable and network; no conversation request was sent" })
            .details(json!({"setup":setup,"http_status":error.status,"failure_kind":error.kind,"request":"GET model catalog only"}))
    })?;
    let model = config.default_model.as_deref().expect("reported model");
    if !models.iter().any(|candidate| candidate == model) {
        return Err(ApiError::new(ErrorCode::ConfigInvalid, "Configured model is not advertised by the model catalog (this does not prove it is unusable)")
            .hint("Choose an advertised default_model, or confirm the alias with your proxy administrator")
            .details(json!({"setup":setup,"model":model,"model_count":models.len()})));
    }
    Ok(
        json!({"setup":setup,"connection":name,"model":model,"model_count":models.len(),"request":"GET model catalog only","message":"Configuration and model catalog checked; no conversation request sent"}),
    )
}

fn init(path: &Path) -> Result<Value, ApiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| {
            ApiError::new(ErrorCode::IoFailed, "Cannot create configuration directory")
                .details(json!({"config_path":path}))
        })?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(
                json!({"config_path":path,"created":false,"message":"Configuration path already exists; not overwritten","precedence":PRECEDENCE,"next":NEXT}),
            );
        }
        Err(_) => {
            return Err(ApiError::new(
                ErrorCode::IoFailed,
                "Cannot create configuration file; nothing overwritten",
            )
            .details(json!({"config_path":path})));
        }
    };
    file.write_all(TEMPLATE.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| {
            ApiError::new(ErrorCode::IoFailed, "Cannot finish configuration template")
                .details(json!({"config_path":path}))
        })?;
    Ok(
        json!({"config_path":path,"created":true,"message":"Template created; edit the endpoint/model and set api_key_env in your environment, not in this file","precedence":PRECEDENCE,"next":NEXT}),
    )
}
