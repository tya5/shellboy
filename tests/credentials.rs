//! Stored API keys, `credentials.set`, `credentials.clear` and `connection.test` (#269).
//! Temporary directories and a local mock server only: no real provider, no real user data.
use crate::runtime_features::{answer, settled, thread};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shellboy::{
    app::DataOwner,
    config::{Connection, KeySource},
    control::Control,
    credential_store,
    model::http::HttpModelClient,
    protocol::{
        Command, ConfigGetArgs, ConfigSetArgs, ConnectionTestArgs, CredentialsClearArgs,
        CredentialsSetArgs, ErrorCode, Request,
    },
};
use std::{
    path::{Path, PathBuf},
    process::Command as Process,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

/// A value that no redaction heuristic is built around: the tests must hold because the
/// product never emits the value, not because the value looks like a key.
const KEY: &str = "zq7-leakcheck-Tn4Pw9-value-0001";
const OTHER_KEY: &str = "zq7-leakcheck-Rv2Hc8-value-0002";

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
}
struct Mock {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// The mock answers 401 unless this exact bearer key arrives; `None` accepts anything.
    required: Arc<Mutex<Option<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Mock {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    fn require(&self, key: Option<&str>) {
        *self.required.lock().unwrap() = key.map(str::to_owned);
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let required = Arc::new(Mutex::new(None::<String>));
    let (log, rule) = (seen.clone(), required.clone());
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (log, rule) = (log.clone(), rule.clone());
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let (head, mut body_bytes) = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (
                            String::from_utf8_lossy(&bytes[..i]).into_owned(),
                            bytes.split_off(i + 4),
                        );
                    }
                };
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while body_bytes.len() < length {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body_bytes.extend_from_slice(&chunk[..n]);
                }
                let mut first = head.lines().next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_owned();
                let path = first.next().unwrap_or("").to_owned();
                let authorization = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("authorization")
                        .then(|| value.trim().to_owned())
                });
                let x_api_key = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("x-api-key")
                        .then(|| value.trim().to_owned())
                });
                log.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: path.clone(),
                    authorization: authorization.clone(),
                    x_api_key,
                });
                let wanted = rule.lock().unwrap().clone();
                let (status, body) = if wanted.as_ref().is_some_and(|key| {
                    authorization.as_deref() != Some(format!("Bearer {key}").as_str())
                }) {
                    // Real providers often repeat part of the key they refused.
                    (
                        401,
                        json!({"error":{"message":format!("Incorrect key provided: {}", authorization.unwrap_or_default())}}),
                    )
                } else if path.ends_with("/missing/models") {
                    (404, json!({"error":"not found"}))
                } else if path.ends_with("/html/models") {
                    (200, Value::String("<html>wrong page</html>".into()))
                } else if path.ends_with("/nodata/models") {
                    (200, json!({"object":"list"}))
                } else if method == "GET" && path.ends_with("/models") {
                    (200, json!({"data":[{"id":"model-b"},{"id":"model-a"}]}))
                } else {
                    (200, answer("ok"))
                };
                let body = match body {
                    Value::String(raw) => raw,
                    other => other.to_string(),
                };
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    Mock {
        url,
        seen,
        required,
        task,
    }
}

fn write_config(root: &Path, text: &str) -> PathBuf {
    let path = root.join("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}
async fn start(root: &Path) -> (DataOwner, Arc<Control>) {
    let owner = DataOwner::start(
        Some(root.join("config.toml")),
        Some(root.join("data")),
        root.into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.into());
    (owner, control)
}
async fn call(
    control: &Arc<Control>,
    command: Command,
) -> Result<Value, shellboy::protocol::ApiError> {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    if response.ok {
        Ok(response.result.unwrap())
    } else {
        Err(response.error.unwrap())
    }
}
fn set(connection: &str, value: &str) -> Command {
    Command::CredentialsSet(CredentialsSetArgs {
        connection: connection.into(),
        value: value.into(),
    })
}
fn clear(connection: &str) -> Command {
    Command::CredentialsClear(CredentialsClearArgs {
        connection: connection.into(),
    })
}
fn test_saved(connection: &str) -> Command {
    Command::ConnectionTest(ConnectionTestArgs {
        connection: Some(connection.into()),
        ..ConnectionTestArgs::default()
    })
}
fn candidate(base_url: &str) -> ConnectionTestArgs {
    ConnectionTestArgs {
        base_url: Some(base_url.into()),
        ..ConnectionTestArgs::default()
    }
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn mentions(text: &str) -> bool {
    text.contains(KEY) || text.contains(OTHER_KEY)
}
/// Every file under `root` except the credentials file, as lossy text.
fn scan_files(root: &Path, found: &mut Vec<(PathBuf, bool)>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan_files(&path, found);
        } else if path
            .file_name()
            .is_some_and(|name| name != "credentials.toml")
        {
            let bytes = std::fs::read(&path).unwrap_or_default();
            let hit = [KEY, OTHER_KEY]
                .iter()
                .any(|key| bytes.windows(key.len()).any(|w| w == key.as_bytes()));
            found.push((path, hit));
        }
    }
}

const ONE_CONNECTION: &str = "default_connection=\"proxy\"\ndefault_model=\"model-a\"\n";

fn proxy_config(url: &str, extra: &str) -> String {
    format!("{ONE_CONNECTION}[connections.proxy]\nbase_url={url:?}\n{extra}")
}

// ---- stage A: the stored key and its precedence ----

#[test]
fn environment_wins_then_the_stored_key_and_neither_is_the_old_error() {
    let env = |name: &str| (name == "SET_VAR").then(|| "from-env".to_owned());
    let with = |var: &str, header: Option<&str>| Connection {
        api_key_env: Some(var.into()),
        api_key_header: header.map(str::to_owned),
        ..Connection::default()
    };
    let auth = |c: &Connection, stored: Option<&str>| {
        c.credentials_with(env, stored)
            .map(|credentials| credentials.headers().clone())
    };
    // The variable has a value: it wins over a stored key.
    let headers = auth(&with("SET_VAR", None), Some("stored")).unwrap();
    assert_eq!(headers["Authorization"], "Bearer from-env");
    // No value: the stored key is used, in the same header.
    let headers = auth(&with("UNSET_VAR", None), Some("stored")).unwrap();
    assert_eq!(headers["Authorization"], "Bearer stored");
    let headers = auth(&with("UNSET_VAR", Some("x-api-key")), Some("stored")).unwrap();
    assert_eq!(headers["x-api-key"], "stored");
    // A blank value is no value.
    let blank = |_: &str| Some("  ".to_owned());
    let headers = with("BLANK", None)
        .credentials_with(blank, Some("stored"))
        .unwrap();
    assert_eq!(headers.headers()["Authorization"], "Bearer stored");
    // Neither: the same error as before stored keys existed.
    let error = auth(&with("UNSET_VAR", None), None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "connection credential environment variable UNSET_VAR is missing or empty"
    );
    assert_eq!(
        auth(&with("UNSET_VAR", None), Some("   "))
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
    // A connection without api_key_env uses the stored key, and sends nothing without one.
    let plain = Connection::default();
    assert_eq!(
        auth(&plain, Some("stored")).unwrap()["Authorization"],
        "Bearer stored"
    );
    assert!(auth(&plain, None).unwrap().is_empty());
    // An explicit Authorization header is never replaced by a stored key.
    let mut explicit = Connection::default();
    explicit
        .headers
        .insert("authorization".into(), "Bearer explicit".into());
    assert!(!explicit.stored_key_allowed());
    assert_eq!(
        auth(&explicit, Some("stored")).unwrap()["authorization"],
        "Bearer explicit"
    );
    assert_eq!(explicit.key_source(env, true), KeySource::None);
    // Source facts.
    assert_eq!(with("SET_VAR", None).key_source(env, true), KeySource::Env);
    assert_eq!(
        with("UNSET_VAR", None).key_source(env, true),
        KeySource::Stored
    );
    assert_eq!(
        with("UNSET_VAR", None).key_source(env, false),
        KeySource::None
    );
    // A key with a line break is refused whichever source it came from.
    assert!(auth(&with("UNSET_VAR", None), Some("a\nb")).is_err());
}

#[test]
fn a_key_header_without_a_variable_takes_a_stored_key_and_never_sends_nothing() {
    let connection = Connection {
        api_key_header: Some("x-api-key".into()),
        ..Connection::default()
    };
    let no_env = |_: &str| None;
    let sent = connection.credentials_with(no_env, Some("stored")).unwrap();
    assert_eq!(sent.headers()["x-api-key"], "stored");
    assert!(!sent.headers().contains_key("Authorization"));
    // Neither source: an error at the request, not a connection that loads and sends nothing.
    let error = connection.credentials_with(no_env, None).unwrap_err();
    assert!(error.to_string().contains("api_key_header"), "{error}");
    // An unrelated variable in the environment cannot override the typed key: none is named.
    let env = |_: &str| Some("unrelated".to_owned());
    assert_eq!(
        connection
            .credentials_with(env, Some("stored"))
            .unwrap()
            .headers()["x-api-key"],
        "stored"
    );
    assert_eq!(connection.key_source(env, true), KeySource::Stored);
    assert_eq!(connection.key_source(env, false), KeySource::None);
    // The file parses without a variable; the header is not allowed twice.
    let config = shellboy::config::Config::parse(
        "[connections.a]\nbase_url='https://example.test/v1'\napi_key_header='x-api-key'\n",
    )
    .unwrap();
    assert!(config.connections["a"].stored_key_allowed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_header_only_connection_sends_its_stored_key_in_that_header_and_fails_without_one() {
    crate::test_watchdog::watchdog("header only", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        let file = root.path().join("credentials.toml");
        let connection = Connection {
            base_url: mock.url.clone(),
            api_key_header: Some("x-api-key".into()),
            credentials_file: Some(file.clone()),
            ..Connection::default()
        };
        connection.stored_keys.set("proxy", true);
        let client = HttpModelClient::for_connection("proxy", connection).unwrap();
        // Loads fine; the request fails before sending, with a missing-credential error.
        let error = client.models(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind, "config_missing");
        assert!(mock.seen().is_empty());
        credential_store::set(&file, "proxy", KEY).unwrap();
        client.models(CancellationToken::new()).await.unwrap();
        let seen = mock.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].x_api_key.as_deref(), Some(KEY));
        assert_eq!(seen[0].authorization, None);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_reads_the_stored_key_when_each_request_starts() {
    crate::test_watchdog::watchdog("stored key is read lazily", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        let file = root.path().join("credentials.toml");
        let connection = Connection {
            base_url: mock.url.clone(),
            api_key_env: Some("SHELLBOY_TEST_ENV_NEVER_SET_269".into()),
            credentials_file: Some(file.clone()),
            ..Connection::default()
        };
        connection.stored_keys.set("proxy", true);
        let client = HttpModelClient::for_connection("proxy", connection).unwrap();
        // No file: the old error, and nothing reaches the server.
        let error = client.models(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind, "config_missing");
        assert!(mock.seen().is_empty());
        // The key is stored after the client exists: the next request carries it.
        credential_store::set(&file, "proxy", KEY).unwrap();
        mock.require(Some(KEY));
        client.models(CancellationToken::new()).await.unwrap();
        assert_eq!(
            mock.seen().last().unwrap().authorization.as_deref(),
            Some(format!("Bearer {KEY}").as_str())
        );
        // Replaced: the following request uses the new key.
        credential_store::set(&file, "proxy", OTHER_KEY).unwrap();
        mock.require(Some(OTHER_KEY));
        client.models(CancellationToken::new()).await.unwrap();
        // Another connection's entry is not this connection's key.
        credential_store::set(&file, "other", KEY).unwrap();
        credential_store::clear(&file, "proxy").unwrap();
        assert_eq!(
            client
                .models(CancellationToken::new())
                .await
                .unwrap_err()
                .kind,
            "config_missing"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_credentials_file_fails_the_request_without_quoting_it() {
    crate::test_watchdog::watchdog("invalid credentials file", async {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("credentials.toml");
        std::fs::write(
            &file,
            format!("[connections.proxy]\napi_key = \"{KEY}\"\nsurprise = 1\n"),
        )
        .unwrap();
        let connection = Connection {
            base_url: "http://127.0.0.1:1/v1".into(),
            credentials_file: Some(file),
            api_key_env: Some("SHELLBOY_TEST_ENV_NEVER_SET_269".into()),
            ..Connection::default()
        };
        connection.stored_keys.set("proxy", true);
        let client = HttpModelClient::for_connection("proxy", connection).unwrap();
        let error = client.models(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind, "config_invalid");
        assert!(!mentions(&error.message), "{}", error.message);
        assert!(!mentions(&format!("{error:?}")));
        assert!(error.message.contains("line 3"), "{}", error.message);
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stored_file_is_private_replaced_atomically_and_keeps_other_entries() {
    use std::os::unix::fs::PermissionsExt;
    crate::test_watchdog::watchdog("credentials file mode", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        write_config(
            root.path(),
            &format!(
                "{}[connections.second]\nbase_url={:?}\n",
                proxy_config(&mock.url, ""),
                mock.url
            ),
        );
        let (_owner, control) = start(root.path()).await;
        let file = root.path().join("credentials.toml");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        let first = call(&control, set("proxy", KEY)).await.unwrap();
        assert_eq!(first["stored"], true);
        assert_eq!(first["replaced"], false);
        assert_eq!(first["permissions_tightened"], false);
        assert_eq!(mode(&file), 0o600);

        // A loose file is corrected by the save, and other entries survive.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let second = call(&control, set("second", OTHER_KEY)).await.unwrap();
        assert_eq!(second["permissions_tightened"], true);
        assert_eq!(mode(&file), 0o600);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(KEY) && text.contains(OTHER_KEY));
        assert!(text.starts_with("# Shellboy stored API keys. Plain text"));

        // A loose existing file never makes the new key readable: the temporary file is
        // 0600 when the replace starts, not only after it.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let seen = std::cell::Cell::new(0);
        credential_store::set_with(&file, "second", KEY, |temporary| {
            seen.set(mode(temporary));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.get(), 0o600);
        assert_eq!(mode(&file), 0o600);

        // Replacing leaves the other entry and a single file in the directory.
        let third = call(&control, set("proxy", OTHER_KEY)).await.unwrap();
        assert_eq!(third["replaced"], true);
        let names: Vec<String> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|name| name.ends_with(".tmp")),
            "no temporary file remains: {names:?}"
        );

        // Clearing removes only the named entry.
        let cleared = call(&control, clear("proxy")).await.unwrap();
        assert_eq!(cleared["cleared"], true);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("proxy") && text.contains("second"));
        let again = call(&control, clear("proxy")).await.unwrap();
        assert_eq!(again["cleared"], false);
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_refuses_an_unknown_connection_a_bad_value_and_an_invalid_file_without_writing() {
    crate::test_watchdog::watchdog("credentials refusals", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        write_config(root.path(), &proxy_config(&mock.url, ""));
        let (_owner, control) = start(root.path()).await;
        let file = root.path().join("credentials.toml");

        let error = call(&control, set("nobody", KEY)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(!mentions(&serde_json::to_string(&error).unwrap()));
        assert!(!file.exists());

        for bad in [
            "",
            "   ",
            "line\nbreak",
            " leading",
            "trailing ",
            "zero\u{200b}width",
            "inner space",
            "caf\u{e9}",
            &"k".repeat(5000),
        ] {
            let error = call(&control, set("proxy", bad)).await.unwrap_err();
            assert!(
                matches!(
                    error.code,
                    ErrorCode::InvalidArgument | ErrorCode::InputLimit
                ),
                "{:?}",
                error.code
            );
            let text = serde_json::to_string(&error).unwrap();
            assert!(
                bad.trim().is_empty() || !text.contains(bad.trim()),
                "the refused value must not be echoed"
            );
        }
        assert!(!file.exists());

        // An invalid existing file is left exactly as it is.
        let broken = format!("[connections.proxy]\napi_key = \"{OTHER_KEY}\"\nsurprise = 1\n");
        std::fs::write(&file, &broken).unwrap();
        let error = call(&control, set("proxy", KEY)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(!mentions(&serde_json::to_string(&error).unwrap()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
        let got = call(&control, Command::ConfigGet(ConfigGetArgs {}))
            .await
            .unwrap();
        assert_eq!(got["credentials_file"]["valid"], false);
        assert!(!mentions(&got.to_string()));
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_key_value_is_never_returned_logged_hashed_or_put_in_config() {
    crate::test_watchdog::watchdog("no leak", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        mock.require(Some(KEY));
        let config_path = write_config(root.path(), &proxy_config(&mock.url, ""));
        let config_before = std::fs::read(&config_path).unwrap();
        let (owner, control) = start(root.path()).await;
        let mut replies = Vec::new();

        let command = set("proxy", KEY);
        // Debug of the request and of the command never shows the value.
        assert!(!mentions(&format!("{command:?}")));
        assert!(!mentions(&format!(
            "{:?}",
            Request::new("x".to_owned(), command.clone())
        )));
        let operation = format!("{}:leak-1", control.store.instance_id);
        let response = control
            .request(Request::new(operation.clone(), command.clone()))
            .await;
        replies.push(serde_json::to_string(&response).unwrap());
        assert!(response.ok, "{:?}", response.error);
        // The same operation replays the saved outcome, still without the value.
        let replay = control.request(Request::new(operation.clone(), command)).await;
        replies.push(serde_json::to_string(&replay).unwrap());
        assert_eq!(replay.result, response.result);
        // The same ID with another value conflicts, and the error holds no value either.
        let conflict = control
            .request(Request::new(operation.clone(), set("proxy", OTHER_KEY)))
            .await;
        assert_eq!(conflict.error.as_ref().unwrap().code, ErrorCode::OperationConflict);
        replies.push(serde_json::to_string(&conflict).unwrap());

        // A test that the mock refuses: the upstream body repeats the key it saw.
        mock.require(Some(OTHER_KEY));
        let failed = call(&control, test_saved("proxy")).await.unwrap();
        assert_eq!(failed["failure"], "auth");
        replies.push(failed.to_string());
        let inline = call(
            &control,
            Command::ConnectionTest(ConnectionTestArgs {
                api_key: Some(KEY.into()),
                ..candidate(&mock.url)
            }),
        )
        .await
        .unwrap();
        assert_eq!(inline["failure"], "auth");
        replies.push(inline.to_string());
        let got = call(&control, Command::ConfigGet(ConfigGetArgs {})).await.unwrap();
        replies.push(got.to_string());
        let operation_get = call(
            &control,
            Command::OperationGet(shellboy::protocol::OperationTarget {
                operation_id: operation.clone(),
            }),
        )
        .await
        .unwrap();
        replies.push(operation_get.to_string());
        for reply in &replies {
            assert!(!mentions(reply), "a reply contains the key: {reply}");
        }

        // The receipt table holds neither the value nor a plain hash of the request.
        let rows: Vec<String> = control
            .store
            .database
            .call(|db| {
                let mut statement = db.prepare(
                    "SELECT origin_json||'|'||request_hash||'|'||COALESCE(result_ref_json,'') FROM operation_receipt",
                )?;
                Ok(statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap();
        assert!(!rows.is_empty());
        let plain = sha(&serde_json::to_vec(
            &json!({"command":set("proxy", KEY),"caller":null}),
        )
        .unwrap());
        for row in &rows {
            assert!(!mentions(row), "{row}");
            assert!(!row.contains(&plain), "the request hash must not be unsalted");
        }

        // config.toml is unchanged, so its content_hash is too.
        assert_eq!(std::fs::read(&config_path).unwrap(), config_before);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();

        // Nothing outside credentials.toml holds the key: database, WAL, logs, config.
        let mut files = Vec::new();
        scan_files(root.path(), &mut files);
        assert!(files.len() > 2, "{files:?}");
        for (path, hit) in files {
            assert!(!hit, "{} contains the key", path.display());
        }
        assert!(std::fs::read_to_string(root.path().join("credentials.toml"))
            .unwrap()
            .contains(KEY));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_get_states_where_each_key_comes_from_and_nothing_else() {
    crate::test_watchdog::watchdog("key source", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        let text = format!(
            "default_connection=\"envy\"\ndefault_model=\"m\"\n\
             [connections.envy]\nbase_url={url:?}\napi_key_env=\"PATH\"\n\
             [connections.saved]\nbase_url={url:?}\napi_key_env=\"SHELLBOY_TEST_ENV_NEVER_SET_269\"\n\
             [connections.bare]\nbase_url={url:?}\n",
            url = mock.url
        );
        write_config(root.path(), &text);
        let (_owner, control) = start(root.path()).await;
        let sources = |got: &Value, view: &str| got[view]["credential_source"].clone();

        let got = call(&control, Command::ConfigGet(ConfigGetArgs {})).await.unwrap();
        let expected = json!({"envy":"env","saved":"none","bare":"none"});
        assert_eq!(sources(&got, "loaded"), expected);
        assert_eq!(sources(&got, "file"), expected);
        assert_eq!(got["credentials_file"]["present"], false);
        assert_eq!(got["credentials_file"]["valid"], true);

        // A stored key on a connection whose variable is unset; one whose variable is set.
        call(&control, set("saved", KEY)).await.unwrap();
        call(&control, set("envy", OTHER_KEY)).await.unwrap();
        call(&control, set("bare", KEY)).await.unwrap();
        let got = call(&control, Command::ConfigGet(ConfigGetArgs {})).await.unwrap();
        let expected = json!({"envy":"env","saved":"stored","bare":"stored"});
        assert_eq!(sources(&got, "loaded"), expected);
        assert_eq!(got["credentials_file"]["present"], true);
        // Only a name and a fact: no value, no length.
        assert!(!mentions(&got.to_string()));
        let shown = got["credentials_file"].to_string();
        assert!(!shown.contains("api_key") && !shown.contains(&KEY.len().to_string()));

        let set_reply = call(&control, set("envy", OTHER_KEY)).await.unwrap();
        assert_eq!(set_reply["key_source"], "env", "the variable still wins");
        call(&control, clear("saved")).await.unwrap();
        let got = call(&control, Command::ConfigGet(ConfigGetArgs {})).await.unwrap();
        assert_eq!(sources(&got, "loaded")["saved"], "none");
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_hints_count_a_stored_key_and_follow_set_and_clear() {
    crate::test_watchdog::watchdog("setup hints", async {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            &proxy_config(
                "http://127.0.0.1:1/v1",
                "api_key_env=\"SHELLBOY_TEST_ENV_NEVER_SET_269\"\n",
            ),
        );
        let (owner, control) = start(root.path()).await;
        let missing = |config: &shellboy::config::Config| {
            shellboy::config_setup::report(config, Some("proxy"), Some("model-a"), |_| false)
                .missing
        };
        let config = control.store.config.clone();
        assert_eq!(missing(&config).len(), 1);
        call(&control, set("proxy", KEY)).await.unwrap();
        assert!(missing(&config).is_empty());
        call(&control, clear("proxy")).await.unwrap();
        assert_eq!(missing(&config).len(), 1);
        control.shutdown().await.unwrap();
        drop(owner);
    })
    .await
    .unwrap();
}

// ---- stage B: connection.test ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_connection_is_tested_with_one_catalog_request_and_facts_only() {
    crate::test_watchdog::watchdog("connection test saved", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        mock.require(Some(KEY));
        write_config(
            root.path(),
            &proxy_config(
                &mock.url,
                "api_key_env=\"SHELLBOY_TEST_ENV_NEVER_SET_269\"\n",
            ),
        );
        let (_owner, control) = start(root.path()).await;

        // Nothing to send yet: the variable is unset and no key is stored.
        let before = call(&control, test_saved("proxy")).await.unwrap();
        assert_eq!(before["ok"], false);
        assert_eq!(before["failure"], "credential_missing");
        assert_eq!(before["sent"], false);
        assert_eq!(
            before["credential"]["api_key_env"],
            "SHELLBOY_TEST_ENV_NEVER_SET_269"
        );
        assert_eq!(before["credential"]["env_set"], false);
        assert_eq!(before["credential"]["source"], "none");
        assert!(mock.seen().is_empty());

        call(&control, set("proxy", KEY)).await.unwrap();
        let after = call(&control, test_saved("proxy")).await.unwrap();
        assert_eq!(after["ok"], true);
        assert_eq!(after["http_status"], 200);
        assert_eq!(after["models"], json!(["model-a", "model-b"]));
        assert_eq!(after["model_count"], 2);
        assert_eq!(after["failure"], Value::Null);
        assert_eq!(after["credential"]["source"], "stored");
        assert_eq!(after["request"], "GET model catalog only");
        // Exactly one request, and it was the catalog GET.
        let seen = mock.seen();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(
            (seen[0].method.as_str(), seen[0].path.as_str()),
            ("GET", "/v1/models")
        );
        // The reply makes no claim about the key being valid.
        let text = after.to_string();
        assert!(!text.contains("valid") && !mentions(&text));

        // A wrong stored key: the status and a class, never the provider's text.
        call(&control, set("proxy", OTHER_KEY)).await.unwrap();
        let denied = call(&control, test_saved("proxy")).await.unwrap();
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["http_status"], 401);
        assert_eq!(denied["failure"], "auth");
        assert_eq!(denied["sent"], true);
        assert!(!mentions(&denied.to_string()));
        assert!(!denied.to_string().contains("Incorrect"));

        // Not in the file at all.
        let error = call(&control, test_saved("nobody")).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_candidate_is_tested_with_an_inline_key_or_a_variable_and_failures_are_classed() {
    crate::test_watchdog::watchdog("connection test candidate", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        mock.require(Some(KEY));
        write_config(root.path(), "");
        let (_owner, control) = start(root.path()).await;

        let ok = call(
            &control,
            Command::ConnectionTest(ConnectionTestArgs {
                api_key: Some(KEY.into()),
                ..candidate(&mock.url)
            }),
        )
        .await
        .unwrap();
        assert_eq!(ok["ok"], true, "{ok}");
        assert_eq!(ok["candidate"], true);
        assert_eq!(ok["connection"], Value::Null);
        assert_eq!(ok["credential"]["source"], "inline");
        assert_eq!(
            mock.seen()[0].authorization.as_deref(),
            Some(format!("Bearer {KEY}").as_str())
        );
        assert!(!mentions(&ok.to_string()));
        // The inline key was kept nowhere: no file, no config entry.
        assert!(!root.path().join("credentials.toml").exists());

        // A variable that is not set where the owner runs: reported, nothing sent.
        let before = mock.seen().len();
        let unset = call(
            &control,
            Command::ConnectionTest(ConnectionTestArgs {
                api_key_env: Some("SHELLBOY_TEST_ENV_NEVER_SET_269".into()),
                ..candidate(&mock.url)
            }),
        )
        .await
        .unwrap();
        assert_eq!(unset["failure"], "credential_missing");
        assert_eq!(unset["credential"]["env_set"], false);
        assert_eq!(mock.seen().len(), before);

        // A variable that is set is detected by name and used.
        mock.require(None);
        let set_var = call(
            &control,
            Command::ConnectionTest(ConnectionTestArgs {
                api_key_env: Some("PATH".into()),
                ..candidate(&mock.url)
            }),
        )
        .await
        .unwrap();
        assert_eq!(set_var["ok"], true);
        assert_eq!(set_var["credential"]["env_set"], true);
        assert_eq!(set_var["credential"]["source"], "env");
        assert!(
            !set_var
                .to_string()
                .contains(&std::env::var("PATH").unwrap())
        );

        // URL class: a suffix the API root must not have, and a 404 from the server.
        let suffix = call(
            &control,
            Command::ConnectionTest(candidate("http://127.0.0.1:9/v1/responses")),
        )
        .await
        .unwrap();
        assert_eq!(
            (suffix["failure"].as_str(), suffix["sent"].as_bool()),
            (Some("url"), Some(false))
        );
        let missing = call(
            &control,
            Command::ConnectionTest(candidate(&format!("{}/missing", mock.url))),
        )
        .await
        .unwrap();
        assert_eq!(missing["failure"], "url");
        assert_eq!(missing["http_status"], 404);
        // Unreachable: nothing listens on port 1.
        let down = call(
            &control,
            Command::ConnectionTest(candidate("http://127.0.0.1:1/v1")),
        )
        .await
        .unwrap();
        assert_eq!(down["failure"], "unreachable");
        assert_eq!(down["http_status"], Value::Null);
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_test_arguments_are_validated_and_never_echoed() {
    crate::test_watchdog::watchdog("connection test arguments", async {
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), "");
        let (_owner, control) = start(root.path()).await;
        let both = ConnectionTestArgs {
            connection: Some("a".into()),
            ..candidate("http://127.0.0.1:1/v1")
        };
        let neither = ConnectionTestArgs::default();
        let two_keys = ConnectionTestArgs {
            api_key: Some(KEY.into()),
            api_key_env: Some("SOME_VAR".into()),
            ..candidate("http://127.0.0.1:1/v1")
        };
        let bad_key = ConnectionTestArgs {
            api_key: Some(format!("{KEY}\nextra")),
            ..candidate("http://127.0.0.1:1/v1")
        };
        for args in [both, neither, two_keys, bad_key] {
            let error = call(&control, Command::ConnectionTest(args))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument);
            assert!(!mentions(&serde_json::to_string(&error).unwrap()));
        }
        // A secret pasted where a variable name belongs is refused without being shown.
        let pasted = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let error = call(
            &control,
            Command::ConnectionTest(ConnectionTestArgs {
                api_key_env: Some(pasted.into()),
                ..candidate("http://127.0.0.1:1/v1")
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.details.unwrap()["reason"], "credential_value_refused");
        assert!(!error.message.contains(pasted));
        // The wire refuses unknown fields and shows no value in Debug.
        assert!(
            serde_json::from_value::<Command>(
                json!({"command":"credentials.set","args":{"connection":"a","value":"b","extra":1}})
            )
            .is_err()
        );
        let debug = format!(
            "{:?}",
            ConnectionTestArgs {
                api_key: Some(KEY.into()),
                ..candidate("http://secret-host.example/v1")
            }
        );
        assert!(!mentions(&debug) && !debug.contains("secret-host"));
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

// ---- stages A and B together: first send with no hand-edited TOML ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_an_empty_configuration_to_a_first_send_using_only_owner_operations() {
    crate::test_watchdog::watchdog("first send", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        mock.require(Some(KEY));
        let config_path = write_config(root.path(), "");
        {
            let (owner, control) = start(root.path()).await;
            let got = call(&control, Command::ConfigGet(ConfigGetArgs {})).await.unwrap();
            assert_eq!(got["loaded"]["values"]["connections"], json!({}));
            let hash = got["file"]["content_hash"].as_str().unwrap().to_owned();
            // Connection, default connection and model: written without opening an editor.
            call(
                &control,
                Command::ConfigSet(ConfigSetArgs {
                    expected_hash: hash,
                    changes: vec![
                        shellboy::protocol::ConfigChange {
                            field: "connections.proxy".into(),
                            value: Some(json!({"base_url": mock.url, "api_key_env": "SHELLBOY_TEST_ENV_NEVER_SET_269"})),
                        },
                        shellboy::protocol::ConfigChange {
                            field: "default_connection".into(),
                            value: Some(json!("proxy")),
                        },
                        shellboy::protocol::ConfigChange {
                            field: "default_model".into(),
                            value: Some(json!("model-a")),
                        },
                    ],
                }),
            )
            .await
            .unwrap();
            // The key is stored for the saved connection, and the saved connection is tested.
            call(&control, set("proxy", KEY)).await.unwrap();
            let tested = call(&control, test_saved("proxy")).await.unwrap();
            assert_eq!(tested["ok"], true, "{tested}");
            assert_eq!(tested["models"], json!(["model-a", "model-b"]));
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        }
        assert!(std::fs::read_to_string(&config_path).unwrap().contains("[connections.proxy]"));
        assert!(!std::fs::read_to_string(&config_path).unwrap().contains(KEY));

        // The next owner start (a new connection needs a restart until stage C) sends with it.
        let (_owner, control) = start(root.path()).await;
        let thread = thread(&control).await;
        crate::runtime_features::send(&control, &thread, "hello").await;
        settled(&control, &thread).await;
        let posts: Vec<Seen> = mock
            .seen()
            .into_iter()
            .filter(|seen| seen.method == "POST")
            .collect();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert!(posts[0].path.ends_with("/responses"));
        assert_eq!(
            posts[0].authorization.as_deref(),
            Some(format!("Bearer {KEY}").as_str())
        );
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_response_that_is_not_a_catalog_keeps_its_status() {
    crate::test_watchdog::watchdog("not a catalog", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        write_config(root.path(), "");
        let (_owner, control) = start(root.path()).await;
        for page in ["html", "nodata"] {
            let reply = call(
                &control,
                Command::ConnectionTest(candidate(&format!("{}/{page}", mock.url))),
            )
            .await
            .unwrap();
            assert_eq!(reply["ok"], false, "{page}");
            assert_eq!(reply["failure"], "response", "{page}");
            // A response arrived, so the status is known and null stays "no response".
            assert_eq!(reply["http_status"], 200, "{page}");
            assert_eq!(reply["sent"], true);
        }
        let down = call(
            &control,
            Command::ConnectionTest(candidate("http://127.0.0.1:1/v1")),
        )
        .await
        .unwrap();
        assert_eq!(down["http_status"], Value::Null);
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_candidate_never_reads_the_credentials_file() {
    crate::test_watchdog::watchdog("candidate ignores file", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        write_config(root.path(), "");
        std::fs::write(root.path().join("credentials.toml"), "not toml [[[").unwrap();
        let (_owner, control) = start(root.path()).await;
        let reply = call(&control, Command::ConnectionTest(candidate(&mock.url)))
            .await
            .unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["credential"]["credentials_file_error"], Value::Null);
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_header_only_connection_without_a_key_is_reported_missing() {
    crate::test_watchdog::watchdog("header only setup", async {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            &proxy_config("http://127.0.0.1:1/v1", "api_key_header=\"x-api-key\"\n"),
        );
        let (_owner, control) = start(root.path()).await;
        let config = control.store.config.clone();
        let report = |config: &shellboy::config::Config| {
            shellboy::config_setup::report(config, Some("proxy"), Some("model-a"), |_| false)
        };
        let missing = report(&config).missing;
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("api_key_header"));
        assert!(
            report(&config)
                .render(shellboy::config::UiLanguage::En, false)
                .contains("api_key_header")
        );
        call(&control, set("proxy", KEY)).await.unwrap();
        assert!(report(&config).missing.is_empty());
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conflicting_stored_key_retry_explains_the_per_process_salt() {
    crate::test_watchdog::watchdog("conflict hint", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        write_config(root.path(), &proxy_config(&mock.url, ""));
        let (_owner, control) = start(root.path()).await;
        let operation = format!("{}:retry-1", control.store.instance_id);
        let first = control
            .request(Request::new(operation.clone(), set("proxy", KEY)))
            .await;
        assert!(first.ok);
        // The fingerprint is salted per owner process, so after a restart even the same key
        // cannot match; a different key conflicts here for the same reason. Both say so.
        let error = control
            .request(Request::new(operation, set("proxy", OTHER_KEY)))
            .await
            .error
            .unwrap();
        assert_eq!(error.code, ErrorCode::OperationConflict);
        assert!(error.hint.unwrap().contains("new operation ID"));
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

// ---- the CLI: the key arrives on standard input only ----

fn cli(root: &Path) -> Process {
    let mut command = Process::new(env!("CARGO_BIN_EXE_shellboy"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("SHELLBOY_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root.join("Roaming"))
        .env("LOCALAPPDATA", root.join("Local"))
        .env("XDG_CONFIG_HOME", root.join("xdg-config"))
        .env("XDG_DATA_HOME", root.join("xdg-data"));
    command
}
async fn run_with_stdin(mut command: Process, input: &str) -> (bool, String, String) {
    use std::process::Stdio;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = tokio::process::Command::from(command).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).await.unwrap();
    drop(stdin);
    let output = child.wait_with_output().await.unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cli_takes_the_key_from_stdin_only_and_never_prints_it() {
    use tokio::io::AsyncBufReadExt;
    crate::test_watchdog::watchdog("credentials cli", async {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let mock = mock().await;
        mock.require(Some(KEY));
        let config = write_config(root.path(), &proxy_config(&mock.url, ""));
        let mut child = tokio::process::Command::from(cli(root.path()))
            .args(["serve", "--data-dir"])
            .arg(&data)
            .arg("--config")
            .arg(&config)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        stdout.read_line(&mut ready).await.unwrap();
        let owner_command = |args: &[&str]| {
            let mut command = cli(root.path());
            command.arg("--data-dir").arg(&data).args(args);
            command
        };

        // There is no argument that carries the key.
        let (ok, out, err) = run_with_stdin(
            owner_command(&[
                "credentials",
                "set",
                "--connection",
                "proxy",
                "--value",
                KEY,
            ]),
            "",
        )
        .await;
        assert!(!ok);
        assert!(out.contains("UnknownArgument"), "{out}{err}");
        assert!(!mentions(&out) && !mentions(&err));
        assert!(!root.path().join("credentials.toml").exists());

        // Standard input, one trailing line break dropped.
        let (ok, out, err) = run_with_stdin(
            owner_command(&["credentials", "set", "--connection", "proxy"]),
            &format!("{KEY}\r\n"),
        )
        .await;
        assert!(ok, "{out}{err}");
        let reply: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["result"]["stored"], true);
        assert!(!mentions(&out) && !mentions(&err));
        assert!(
            std::fs::read_to_string(root.path().join("credentials.toml"))
                .unwrap()
                .contains(&format!("api_key = \"{KEY}\""))
        );

        // The stored key works for a saved-connection test through the CLI.
        let (ok, out, err) = run_with_stdin(
            owner_command(&["connection", "test", "--connection", "proxy"]),
            "",
        )
        .await;
        assert!(ok, "{out}{err}");
        let reply: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(reply["result"]["ok"], true, "{out}");
        assert!(!mentions(&out));

        // A candidate's key also comes from stdin.
        mock.require(Some(OTHER_KEY));
        let (ok, out, _) = run_with_stdin(
            owner_command(&[
                "connection",
                "test",
                "--base-url",
                &mock.url,
                "--api-key-stdin",
            ]),
            OTHER_KEY,
        )
        .await;
        assert!(ok);
        let reply: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(reply["result"]["ok"], true, "{out}");
        assert!(!mentions(&out));

        // An empty stdin is refused rather than stored.
        let (ok, out, _) = run_with_stdin(
            owner_command(&["credentials", "set", "--connection", "proxy"]),
            "\n",
        )
        .await;
        assert!(!ok);
        assert!(out.contains("invalid_argument"), "{out}");

        let (ok, out, _) = run_with_stdin(
            owner_command(&["credentials", "clear", "--connection", "proxy"]),
            "",
        )
        .await;
        assert!(ok, "{out}");
        assert!(
            !std::fs::read_to_string(root.path().join("credentials.toml"))
                .unwrap()
                .contains(KEY)
        );

        // --config is refused: the owner decides which file is meant.
        let (ok, out, _) = run_with_stdin(
            {
                let mut command = cli(root.path());
                command
                    .args(["credentials", "clear", "--connection", "proxy", "--config"])
                    .arg(&config);
                command
            },
            "",
        )
        .await;
        assert!(!ok);
        assert!(out.contains("--config cannot select"), "{out}");
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    })
    .await
    .unwrap();
}

#[test]
fn with_no_owner_the_cli_changes_nothing_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    write_config(root.path(), &proxy_config("http://127.0.0.1:1/v1", ""));
    for args in [
        vec!["credentials", "clear", "--connection", "proxy"],
        vec!["connection", "test", "--connection", "proxy"],
    ] {
        let output = cli(root.path())
            .arg("--data-dir")
            .arg(root.path().join("data"))
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(text.contains("app_not_running"), "{text}");
    }
    assert!(!root.path().join("credentials.toml").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_connections_concurrent_definition_and_key_reach_the_next_http_request() {
    crate::test_watchdog::watchdog("live config and key HTTP", async {
        let root = tempfile::tempdir().unwrap();
        let mock = mock().await;
        mock.require(Some(KEY));
        write_config(root.path(), &format!("default_connection='proxy'\ndefault_model='model-a'\n[connections.proxy]\nbase_url={:?}\n", mock.url));
        let (owner, control) = start(root.path()).await;
        let got = call(&control, Command::ConfigGet(ConfigGetArgs {})).await.unwrap();
        let config = call(&control, Command::ConfigSet(ConfigSetArgs {
            expected_hash: got["file"]["content_hash"].as_str().unwrap().into(),
            changes: vec![shellboy::protocol::ConfigChange {
                field:"connections.proxy.request_timeout_ms".into(), value:Some(json!(12345)),
            }],
        }));
        let key = call(&control, set("proxy", KEY));
        let (config, key) = tokio::join!(config, key);
        assert_eq!(config.unwrap()["applied_to_running_owner"], true);
        assert!(!mentions(&key.unwrap().to_string()));
        let target = thread(&control).await;
        crate::runtime_features::send(&control, &target, "first send without restart").await;
        settled(&control, &target).await;
        let posts: Vec<_> = mock.seen().into_iter().filter(|s| s.method == "POST").collect();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].authorization.as_deref(), Some(format!("Bearer {KEY}").as_str()));
        let id = target;
        let state: String = control.store.database.call(move |db| Ok(db.query_row(
            "SELECT state FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1", [id], |r|r.get(0))?)).await.unwrap();
        assert_eq!(state,"completed");
        control.shutdown().await.unwrap(); owner.shutdown().await.unwrap();
    }).await.unwrap();
}
