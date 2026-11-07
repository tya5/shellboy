//! `credentials.set`, `credentials.clear` and `connection.test` (#269).
//! File work runs on the owner's bounded control-file pool under `config_write`, like
//! `config.set`. The test sends one model-catalog GET through the existing HTTP client.
//! No value is returned, logged or put into an error by any of them.
use super::*;
use crate::{
    config::{Connection, KeySource},
    credential_store,
    protocol::{ConnectionTestArgs, CredentialsClearArgs, CredentialsSetArgs},
};
use std::path::Path;

/// The reply carries ids only up to these bounds; `model_count` is always the full count.
const MAX_TEST_MODELS: usize = 1_000;
const MAX_TEST_MODEL_BYTES: usize = 256 * 1024;

fn caller_ids(caller: Option<&Origin>) -> crate::diagnostics::DiagnosticIds {
    caller.map_or_else(crate::diagnostics::DiagnosticIds::default, |origin| {
        crate::diagnostics::DiagnosticIds {
            thread_id: Some(origin.thread_id.clone()),
            run_id: Some(origin.run_id.clone()),
            turn_id: Some(origin.turn_id.clone()),
            execution_id: Some(origin.execution_id.clone()),
        }
    })
}
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl Control {
    /// Runs `work` with the configuration file's path while holding `config_write`.
    /// Not cancellable: a started write is finished and reported by its saved outcome.
    async fn credential_write<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Path) -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let deadline = self.config_deadline();
        let path = self
            .store
            .config
            .source
            .as_ref()
            .map(|source| source.path.clone())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "This owner was started without a configuration path",
                )
            })?;
        let guard = tokio::time::timeout_at(deadline, self.config_write.clone().lock_owned())
            .await
            .map_err(|_| {
                ApiError::new(
                    ErrorCode::IoFailed,
                    "Another configuration change is still running; nothing was written",
                )
                .details(json!({"written":false}))
            })?;
        self.attachments
            .control_files()
            .execute(deadline, CancellationToken::new(), move || {
                // A timed-out caller stops waiting, but the next change must not start
                // until this one has really ended, so the closure owns the guard.
                let _guard = guard;
                work(&path)
            })
            .await
            .map_err(|error| {
                if error.code == ErrorCode::IoFailed && error.details.is_none() {
                    error.hint("The change may still complete. Read shellboy config get before repeating it")
                } else {
                    error
                }
            })
    }

    pub(super) async fn credentials_set(
        &self,
        args: CredentialsSetArgs,
        caller: Option<&Origin>,
    ) -> Result<Value, ApiError> {
        let config = self.store.config.clone();
        let connection = args.connection.clone();
        let mut result = self
            .credential_write(move |config_path| {
                let known = known_connection(&config, config_path, &args.connection)?;
                if !known.stored_key_allowed() {
                    return Err(ApiError::new(
                        ErrorCode::InvalidArgument,
                        "This connection sends its own Authorization header from headers, so a stored key would never be used",
                    )
                    .hint("Remove that header from the connection, or keep the credential where it is")
                    .details(json!({"field":"connection","written":false})));
                }
                let path = credential_store::path_beside(config_path);
                let outcome = credential_store::set(&path, &args.connection, &args.value)?;
                config.stored_keys.set(&args.connection, true);
                let source = known.key_source(env_value, true);
                let mut result = json!({
                    "connection": args.connection,
                    "stored": true,
                    "replaced": outcome.existed,
                    "key_source": source,
                    "credentials_path": path,
                    "permissions_tightened": outcome.permissions_tightened,
                });
                if let Some(warning) = outcome.warning {
                    result["warning"] = json!(warning);
                }
                Ok(result)
            })
            .await?;
        self.diagnostics.record_config_change(
            caller_ids(caller),
            vec![format!("credentials.{connection}")],
        );
        result["changed_by"] = json!(caller.map(Origin::identity));
        Ok(result)
    }

    pub(super) async fn credentials_clear(
        &self,
        args: CredentialsClearArgs,
        caller: Option<&Origin>,
    ) -> Result<Value, ApiError> {
        let config = self.store.config.clone();
        let connection = args.connection.clone();
        let (mut result, cleared) = self
            .credential_write(move |config_path| {
                let path = credential_store::path_beside(config_path);
                let outcome = credential_store::clear(&path, &args.connection)?;
                config.stored_keys.set(&args.connection, false);
                // A removed or unknown connection is still cleaned up; its source is then none.
                let source = known_connection(&config, config_path, &args.connection)
                    .map_or(KeySource::None, |known| known.key_source(env_value, false));
                Ok((
                    json!({
                        "connection": args.connection,
                        "cleared": outcome.written,
                        "key_source": source,
                        "credentials_path": path,
                    }),
                    outcome.written,
                ))
            })
            .await?;
        if cleared {
            self.diagnostics.record_config_change(
                caller_ids(caller),
                vec![format!("credentials.{connection}")],
            );
        }
        result["changed_by"] = json!(caller.map(Origin::identity));
        Ok(result)
    }

    pub(super) async fn connection_test(
        &self,
        args: ConnectionTestArgs,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        let config = self.store.config.clone();
        let prepared = self
            .attachments
            .control_files()
            .execute(self.config_deadline(), disconnected.clone(), move || {
                prepare_test(&config, &args)
            })
            .await?;
        let facts = prepared.facts();
        let client = match prepared.client() {
            Ok(client) => client,
            Err(error) => {
                // Validation refused the connection before anything was sent.
                let failure = if error.message.contains("base_url") {
                    "url"
                } else {
                    "invalid_configuration"
                };
                return Ok(facts.failed(failure, None, false));
            }
        };
        let fetch = client.models_with_status(self.stop.child_token());
        // A read-only catalog request has no target work to keep alive on disconnect.
        let outcome = tokio::select! { biased;
            _ = disconnected.cancelled() => return Err(ApiError::new(ErrorCode::InvalidState, "Connection test caller disconnected")),
            outcome = fetch => outcome,
        };
        Ok(match outcome {
            Ok((status, models)) => facts.succeeded(status, models),
            Err(error) => {
                let failure = classify(&error);
                let sent = !matches!(failure, "credential_missing" | "invalid_configuration");
                facts.failed(failure, error.status, sent)
            }
        })
    }
}

/// The connection as the next owner start would see it, else as this owner loaded it.
fn known_connection(
    loaded: &crate::config::Config,
    config_path: &Path,
    name: &str,
) -> Result<Connection, ApiError> {
    if let Some(saved) = crate::config_edit::saved_connection(config_path, name)? {
        return Ok(saved);
    }
    loaded.connections.get(name).cloned().ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            "The connection is not in the configuration file",
        )
        .hint("Add it with shellboy config set first")
        .details(json!({"field":"connection","written":false}))
    })
}

/// Why a request did not give a catalog. A fact about the exchange, never "the key is
/// invalid": a 401 can come from a wrong key, a missing scope or a gateway rule.
fn classify(error: &crate::model::ModelError) -> &'static str {
    match (error.kind.as_str(), error.status) {
        (_, Some(401 | 403)) => "auth",
        (_, Some(404)) => "url",
        (_, Some(429)) => "rate_limited",
        (_, Some(500..=599)) => "server",
        (_, Some(_)) => "http_status",
        ("timeout", _) => "timeout",
        ("transport", _) => "unreachable",
        ("config_missing", _) => "credential_missing",
        ("config_invalid", _) => "invalid_configuration",
        ("cancelled", _) => "cancelled",
        _ => "response",
    }
}

enum Key {
    /// Given with the request; replaces every other source.
    Inline(String),
    /// Resolved the way a request resolves it: the environment, then the stored key.
    Resolved,
}
struct Prepared {
    name: Option<String>,
    connection: Connection,
    key: Key,
    source: &'static str,
    env_name: Option<String>,
    env_set: Option<bool>,
    credentials_file_error: Option<String>,
}
struct Facts {
    name: Option<String>,
    api: &'static str,
    source: &'static str,
    env_name: Option<String>,
    env_set: Option<bool>,
    credentials_file_error: Option<String>,
}
impl Prepared {
    fn facts(&self) -> Facts {
        Facts {
            name: self.name.clone(),
            api: self.connection.api.as_str(),
            source: self.source,
            env_name: self.env_name.clone(),
            env_set: self.env_set,
            credentials_file_error: self.credentials_file_error.clone(),
        }
    }
    fn client(self) -> Result<crate::model::http::HttpModelClient, crate::model::ModelError> {
        let name = self.name.as_deref().unwrap_or("candidate");
        let client = crate::model::http::HttpModelClient::for_connection(name, self.connection)?;
        Ok(match self.key {
            Key::Inline(key) => client.with_inline_key(&key),
            Key::Resolved => client,
        })
    }
}
impl Facts {
    fn base(&self) -> Value {
        json!({
            "request": "GET model catalog only",
            "connection": self.name,
            "candidate": self.name.is_none(),
            "api": self.api,
            "credential": {
                "source": self.source,
                "api_key_env": self.env_name,
                "env_set": self.env_set,
                "credentials_file_error": self.credentials_file_error,
            },
        })
    }
    fn failed(&self, failure: &str, status: Option<u16>, sent: bool) -> Value {
        let mut result = self.base();
        result["sent"] = json!(sent);
        result["ok"] = json!(false);
        result["http_status"] = json!(status);
        result["failure"] = json!(failure);
        result["models"] = json!([]);
        result["model_count"] = json!(0);
        result["models_truncated"] = json!(false);
        result
    }
    fn succeeded(&self, status: u16, models: Vec<String>) -> Value {
        let mut bytes = 0;
        let shown: Vec<&String> = models
            .iter()
            .take(MAX_TEST_MODELS)
            .take_while(|id| {
                bytes += id.len() + 4;
                bytes <= MAX_TEST_MODEL_BYTES
            })
            .collect();
        let mut result = self.base();
        result["sent"] = json!(true);
        result["ok"] = json!(true);
        result["http_status"] = json!(status);
        result["failure"] = Value::Null;
        result["models_truncated"] = json!(shown.len() < models.len());
        result["model_count"] = json!(models.len());
        result["models"] = json!(shown);
        result
    }
}

/// Blocking: reads the saved file and the credentials file.
fn prepare_test(
    loaded: &crate::config::Config,
    args: &ConnectionTestArgs,
) -> Result<Prepared, ApiError> {
    let config_path = loaded.source.as_ref().map(|source| source.path.as_path());
    let credentials = config_path.map(credential_store::path_beside);
    let stored = credentials
        .as_deref()
        .map(credential_store::stored_names)
        .transpose();
    let (stored_names, credentials_file_error) = match stored {
        Ok(names) => (names.unwrap_or_default(), None),
        Err(error) => (Default::default(), Some(error.to_string())),
    };
    if let Some(name) = &args.connection {
        let mut connection = match config_path {
            Some(path) => known_connection(loaded, path, name)?,
            None => loaded.connections.get(name).cloned().ok_or_else(|| {
                ApiError::new(ErrorCode::ConfigInvalid, "Connection is not configured")
            })?,
        };
        connection.credentials_file = credentials;
        let present = stored_names.contains(name);
        let key_source = connection.key_source(env_value, present);
        let env_name = connection
            .api_key_env
            .clone()
            .filter(|name| !crate::config_edit::looks_like_credential(name));
        let env_set = env_name
            .as_deref()
            .map(|var| env_value(var).is_some_and(|value| !value.trim().is_empty()));
        return Ok(Prepared {
            name: Some(name.clone()),
            connection,
            key: Key::Resolved,
            source: source_name(key_source),
            env_name,
            env_set,
            credentials_file_error,
        });
    }
    let refused = |field: &str| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            format!("{field} takes a name, and this value looks like a credential; it was not used and is not shown"),
        )
        .hint("Pass the key as api_key (from standard input in the CLI), and the variable's name as api_key_env")
        .details(json!({"reason":"credential_value_refused","field":field}))
    };
    if args
        .api_key_env
        .as_deref()
        .is_some_and(crate::config_edit::looks_like_credential)
    {
        return Err(refused("api_key_env"));
    }
    if args
        .api_key_header
        .as_deref()
        .is_some_and(crate::config_edit::looks_like_credential)
    {
        return Err(refused("api_key_header"));
    }
    let inline = args.api_key.clone();
    let connection = Connection {
        api: args.api.unwrap_or_default(),
        base_url: args.base_url.clone().unwrap_or_default(),
        api_key_env: args.api_key_env.clone(),
        api_key_header: args.api_key_header.clone(),
        ssl_verify: args.ssl_verify.unwrap_or(true),
        ..Connection::default()
    };
    let env_name = args.api_key_env.clone();
    let env_set = env_name
        .as_deref()
        .map(|var| env_value(var).is_some_and(|value| !value.trim().is_empty()));
    let source = if inline.is_some() {
        "inline"
    } else {
        source_name(connection.key_source(env_value, false))
    };
    Ok(Prepared {
        name: None,
        connection,
        key: inline.map_or(Key::Resolved, Key::Inline),
        source,
        env_name,
        env_set,
        credentials_file_error,
    })
}
fn source_name(source: KeySource) -> &'static str {
    match source {
        KeySource::Env => "env",
        KeySource::Stored => "stored",
        KeySource::None => "none",
    }
}
