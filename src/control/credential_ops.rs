//! `credentials.set`, `credentials.clear` and `connection.test` (#269).
//! File work runs on the owner's bounded control-file pool under `config_write`, like
//! `config.set`. The test sends one model-catalog GET through the existing HTTP client.
//! No value is returned, logged or put into an error by any of them.
use super::*;
use crate::{
    config::{ApiEndpoint, Connection, KeySource},
    credential_store,
    model::http::HttpModelClient,
    protocol::{ConnectionTestArgs, CredentialsClearArgs, CredentialsSetArgs},
};
use std::path::Path;

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

    /// A read: `runtime_observation` already drops this future when the caller disconnects or
    /// the owner shuts down, which also drops the HTTP request.
    pub(super) async fn connection_test(
        &self,
        args: ConnectionTestArgs,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        let config = self.store.config.clone();
        let test = self
            .attachments
            .control_files()
            .execute(self.config_deadline(), disconnected, move || {
                prepare_test(&config, &args)
            })
            .await?;
        if test.url_invalid {
            return Ok(test.failed("url", None, false));
        }
        let name = test.name.as_deref().unwrap_or("candidate");
        let Ok(client) = HttpModelClient::for_connection(name, test.connection.clone()) else {
            return Ok(test.failed("invalid_configuration", None, false));
        };
        Ok(
            match client.models_with_status(CancellationToken::new()).await {
                Ok((status, models)) => test.succeeded(status, models),
                Err(failure) => {
                    let class = classify(&failure.error);
                    let attempted =
                        !matches!(class, "credential_missing" | "invalid_configuration");
                    test.failed(class, failure.http_status, attempted)
                }
            },
        )
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
        _ => "response",
    }
}

/// One `connection.test`: the connection to try and the facts the reply starts from.
struct Test {
    name: Option<String>,
    connection: Connection,
    source: &'static str,
    env_name: Option<String>,
    env_set: Option<bool>,
    credentials_file_error: Option<String>,
    /// `base_url` fails validation: nothing can be sent.
    url_invalid: bool,
}
impl Test {
    fn new(
        name: Option<String>,
        connection: Connection,
        source: &'static str,
        credentials_file_error: Option<String>,
    ) -> Self {
        let env_name = connection
            .api_key_env
            .clone()
            .filter(|name| !crate::config_edit::looks_like_credential(name));
        let env_set = env_name
            .as_deref()
            .map(|var| env_value(var).is_some_and(|value| !value.trim().is_empty()));
        Self {
            url_invalid: connection.endpoint(ApiEndpoint::Models).is_err(),
            name,
            connection,
            source,
            env_name,
            env_set,
            credentials_file_error,
        }
    }
    fn base(&self) -> Value {
        json!({
            "request": "GET model catalog only",
            "connection": self.name,
            "candidate": self.name.is_none(),
            "api": self.connection.api.as_str(),
            "credential": {
                "source": self.source,
                "api_key_env": self.env_name,
                "env_set": self.env_set,
                "credentials_file_error": self.credentials_file_error,
            },
        })
    }
    /// `attempted` says a request was made, not that it was delivered: `http_status` is
    /// non-null only when a response came back.
    fn failed(&self, failure: &str, http_status: Option<u16>, attempted: bool) -> Value {
        let mut result = self.base();
        result["sent"] = json!(attempted);
        result["ok"] = json!(false);
        result["http_status"] = json!(http_status);
        result["failure"] = json!(failure);
        result["models"] = json!([]);
        result["model_count"] = json!(0);
        result
    }
    fn succeeded(&self, status: u16, models: Vec<String>) -> Value {
        let mut result = self.base();
        result["sent"] = json!(true);
        result["ok"] = json!(true);
        result["http_status"] = json!(status);
        result["failure"] = Value::Null;
        result["model_count"] = json!(models.len());
        result["models"] = json!(models);
        result
    }
}

/// Blocking: reads the saved file and, for a saved connection, the credentials file.
/// A candidate never touches `credentials.toml`.
fn prepare_test(
    loaded: &crate::config::Config,
    args: &ConnectionTestArgs,
) -> Result<Test, ApiError> {
    let config_path = loaded.source.as_ref().map(|source| source.path.as_path());
    if let Some(name) = &args.connection {
        let mut connection = match config_path {
            Some(path) => known_connection(loaded, path, name)?,
            None => loaded.connections.get(name).cloned().ok_or_else(|| {
                ApiError::new(ErrorCode::ConfigInvalid, "Connection is not configured")
            })?,
        };
        let credentials = config_path.map(credential_store::path_beside);
        let (present, file_error) = match credentials.as_deref().map(credential_store::stored_names)
        {
            Some(Ok(names)) => (names.contains(name), None),
            Some(Err(error)) => (false, Some(error.to_string())),
            None => (false, None),
        };
        connection.credentials_file = credentials;
        // A private registry: a clone of the loaded connection must not share the owner's.
        connection.stored_keys = crate::config::StoredKeys::default();
        connection.stored_keys.set(name, present);
        let source = source_name(connection.key_source(env_value, present));
        return Ok(Test::new(
            Some(name.clone()),
            connection,
            source,
            file_error,
        ));
    }
    let refused = |field: &str| {
        ApiError::new(
            ErrorCode::InvalidArgument,
            format!("{field} takes a name, and this value looks like a credential; it was not used and is not shown"),
        )
        .hint("Pass the key as api_key (from standard input in the CLI), and the variable's name as api_key_env")
        .details(json!({"reason":"credential_value_refused","field":field}))
    };
    for (field, value) in [
        ("api_key_env", &args.api_key_env),
        ("api_key_header", &args.api_key_header),
    ] {
        if value
            .as_deref()
            .is_some_and(crate::config_edit::looks_like_credential)
        {
            return Err(refused(field));
        }
    }
    let mut connection = Connection {
        api: args.api.unwrap_or_default(),
        base_url: args.base_url.clone().unwrap_or_default(),
        api_key_env: args.api_key_env.clone(),
        api_key_header: args.api_key_header.clone(),
        ssl_verify: args.ssl_verify.unwrap_or(true),
        ..Connection::default()
    };
    let source = if let Some(key) = &args.api_key {
        // An inline key is only a header of this one connection value. Headers are masked
        // in `Debug` and in error bodies like any credential, and nothing stores it.
        let (header, value) = match connection.api_key_header.take() {
            Some(header) => (header, key.clone()),
            None => ("Authorization".to_owned(), format!("Bearer {key}")),
        };
        connection.headers.insert(header, value);
        "inline"
    } else {
        source_name(connection.key_source(env_value, false))
    };
    Ok(Test::new(None, connection, source, None))
}
fn source_name(source: KeySource) -> &'static str {
    match source {
        KeySource::Env => "env",
        KeySource::Stored => "stored",
        KeySource::None => "none",
    }
}
