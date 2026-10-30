use serde_json::Value;
use shellboy::config_setup::{MINIMAL, TEMPLATE, report};
use std::{path::Path, process::Command, time::Duration};

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shellboy"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("SHELLBOY_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("Roaming"))
        .env("LOCALAPPDATA", home.join("Local"))
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .env("NO_PROXY", "*")
        .env("no_proxy", "*");
    command
}
fn run(command: &mut Command) -> (bool, Value, String) {
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!(
            "not JSON: {stdout} / {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.success(), json, stdout)
}
fn drain_connections(listener: std::net::TcpListener) -> usize {
    listener.set_nonblocking(true).unwrap();
    let mut count = 0;
    loop {
        match listener.accept() {
            Ok(_) => count += 1,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => panic!("cannot inspect completed mock connections: {error}"),
        }
    }
}
fn expected_path(home: &Path) -> std::path::PathBuf {
    if cfg!(windows) {
        home.join("Roaming").join("shellboy").join("config.toml")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/shellboy/config.toml")
    } else {
        home.join("xdg-config/shellboy/config.toml")
    }
}
fn configuration(url: &str, key: bool) -> String {
    format!(
        "default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='{url}'\n{}",
        if key {
            "api_key_env='SHELLBOY_SETUP_TEST_KEY'\n"
        } else {
            ""
        }
    )
}

#[test]
fn native_default_missing_path_init_and_non_overwrite_use_only_temporary_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let expected = expected_path(home);
    let (ok, value, text) = run(command(home).args(["config", "check"]));
    assert!(!ok, "{text}");
    assert_eq!(
        value["error"]["details"]["setup"]["config_path"],
        expected.to_str().unwrap()
    );
    assert!(text.contains("connection") && text.contains("model"));
    assert!(!expected.exists());
    if cfg!(target_os = "linux") {
        let (ok, value, _) = run(command(home)
            .env_remove("XDG_CONFIG_HOME")
            .args(["config", "check"]));
        assert!(!ok);
        assert_eq!(
            value["error"]["details"]["setup"]["config_path"],
            home.join(".config/shellboy/config.toml").to_str().unwrap()
        );
    }
    let (ok, init, _) = run(command(home).args(["config", "init"]));
    assert!(ok && init["result"]["created"] == true);
    assert_eq!(init["result"]["config_path"], expected.to_str().unwrap());
    assert_eq!(std::fs::read_to_string(&expected).unwrap(), TEMPLATE);
    std::fs::write(&expected, "sentinel existing configuration").unwrap();
    let (ok, value, _) = run(command(home).args(["config", "init"]));
    assert!(ok && value["result"]["created"] == false);
    assert_eq!(
        std::fs::read_to_string(&expected).unwrap(),
        "sentinel existing configuration"
    );
    let explicit = home.join("explicit").join("nested.toml");
    let (ok, value, _) = run(command(home)
        .arg("--config")
        .arg("explicit/nested.toml")
        .args(["config", "init"]));
    assert!(ok);
    assert_eq!(
        std::path::Path::new(value["result"]["config_path"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        explicit.canonicalize().unwrap()
    );
    assert!(!home.join("Local/shellboy/data").exists());
    assert!(
        !home
            .join("Library/Application Support/shellboy/data")
            .exists()
    );
    assert!(!home.join("xdg-data/shellboy").exists());
}

#[test]
fn check_separates_bad_toml_url_missing_file_defaults_and_empty_key_without_values() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    for (source, expected) in [
        ("compact_hint_tokens=1".to_owned(), "compact_hint_tokens"),
        ("".to_owned(), "connection"),
        (
            "default_connection='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'"
                .into(),
            "model",
        ),
        (
            configuration("http://127.0.0.1:1/v1/responses", false),
            "https://proxy.example/v1",
        ),
        (
            configuration("http://127.0.0.1:1/v1/models", false),
            "https://proxy.example/v1",
        ),
        (
            configuration("http://127.0.0.1:1/v1", true),
            "SHELLBOY_SETUP_TEST_KEY",
        ),
    ] {
        std::fs::write(&path, source).unwrap();
        let (ok, value, text) = run(command(root.path())
            .arg("--config")
            .arg(&path)
            .args(["config", "check"])
            .env("SHELLBOY_SETUP_TEST_KEY", "  "));
        assert!(!ok && text.contains(expected), "{text}");
        let details = &value["error"]["details"];
        assert!(
            details["config_path"] == path.to_str().unwrap()
                || details["setup"]["config_path"] == path.to_str().unwrap(),
            "{text}"
        );
    }
    let missing = root.path().join("absent.toml");
    let (ok, _, text) = run(command(root.path())
        .arg("--config")
        .arg(&missing)
        .args(["config", "check"]));
    assert!(!ok && text.contains("cannot read configuration"));
    assert!(!missing.exists());
    let (ok, _, text) = run(command(root.path())
        .arg("--config")
        .arg(root.path())
        .args(["config", "check"]));
    assert!(!ok && text.contains("cannot read configuration"));
    std::fs::write(&path, "[connections.fixture]\nheaders={Authorization='PRIVATE-TOML-SENTINEL'}\ncompact_hint_tokens=1\n").unwrap();
    let (_, _, text) = run(command(root.path())
        .arg("--config")
        .arg(&path)
        .args(["config", "check"]));
    assert!(!text.contains("PRIVATE-TOML-SENTINEL"));
    let (ok, value, _) =
        run(command(root.path()).args(["--operation-id", "invalid", "config", "check"]));
    assert!(!ok && value["error"]["code"] == "invalid_argument");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_only_gets_catalog_and_preserves_status_and_base_root_hint_without_secrets() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let key = "PRIVATE-API-KEY-SENTINEL";
    for (status, body, key_enabled, advertised) in [
        (200, r#"{"data":[{"id":"fixture-model"}]}"#, true, true),
        (200, r#"{"data":[{"id":"fixture-model"}]}"#, false, true),
        (
            200,
            r#"{"data":[{"id":"PRIVATE-API-KEY-SENTINEL"}]}"#,
            true,
            false,
        ),
        (404, "PRIVATE-API-KEY-SENTINEL", true, false),
        (401, "PRIVATE-API-KEY-SENTINEL", true, false),
        (200, "not JSON PRIVATE-API-KEY-SENTINEL", true, false),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0 && request.len() < 16384);
                request.extend_from_slice(&buffer[..n]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(
                request.starts_with("GET /v1/models HTTP/1.1\r\n"),
                "unexpected request method/path"
            );
            assert_eq!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer private-api-key-sentinel"),
                key_enabled
            );
            let reply = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
            drop(stream);
            listener
        });
        std::fs::write(
            &path,
            format!(
                "{}\n[indexing]\nauto_history=false\n",
                configuration(&format!("http://{address}/v1"), key_enabled)
            ),
        )
        .unwrap();
        let home = root.path().to_path_buf();
        let file = path.clone();
        let (ok, value, text) = tokio::task::spawn_blocking(move || {
            run(command(&home)
                .arg("--config")
                .arg(file)
                .args(["config", "check"])
                .env("SHELLBOY_SETUP_TEST_KEY", key))
        })
        .await
        .unwrap();
        assert_eq!(ok, status == 200 && body.starts_with('{'), "{text}");
        if ok {
            assert_eq!(value["result"]["auto_history"], false);
        }
        if ok && !advertised {
            assert_eq!(value["result"]["model_advertised"], false);
            assert_eq!(value["result"]["warnings"].as_array().unwrap().len(), 1);
        }
        assert!(!text.contains(key), "credential value leaked");
        if status != 200 {
            assert_eq!(value["error"]["details"]["http_status"], status);
        }
        if status == 404 {
            assert!(text.contains("https://proxy.example/v1") && text.contains("/responses"));
        }
        let listener = tokio::time::timeout(Duration::from_secs(60), server)
            .await
            .unwrap()
            .unwrap()
            .into_std()
            .unwrap();
        let connections = 1 + drain_connections(listener);
        assert_eq!(
            connections, 1,
            "extra connections after the completed command"
        );
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    std::fs::write(&path, configuration(&format!("http://{address}/v1"), false)).unwrap();
    let home = root.path().to_path_buf();
    let (ok, value, text) = tokio::task::spawn_blocking(move || {
        run(command(&home)
            .arg("--config")
            .arg(path)
            .args(["config", "check"]))
    })
    .await
    .unwrap();
    assert!(
        !ok && value["error"]["details"]["failure_kind"] == "transport",
        "{text}"
    );
}

#[test]
fn setup_report_is_pure_and_minimal_template_matches_bundled_defaults() {
    use shellboy::{config::Config, config_setup};
    assert!(Config::default().indexing.auto_history);
    assert!(
        !Config::parse("[indexing]\nauto_history=false")
            .unwrap()
            .indexing
            .auto_history
    );
    assert!(Config::parse("[indexing]\nauto_history='off'").is_err());
    let config = Config::parse(MINIMAL).unwrap();
    let full = Config::parse(TEMPLATE).unwrap();
    assert_eq!(config.default_connection, full.default_connection);
    assert_eq!(config.default_model, full.default_model);
    assert_eq!(
        config.connections["proxy"].base_url,
        full.connections["proxy"].base_url
    );
    assert_eq!(
        config.connections["proxy"].api_key_env,
        full.connections["proxy"].api_key_env
    );
    let missing = report(
        &config,
        Some("proxy"),
        config.default_model.as_deref(),
        |_| false,
    );
    assert_eq!(missing.credential_env.as_deref(), Some("SHELLBOY_API_KEY"));
    assert_eq!(missing.credential_set, Some(false));
    assert!(missing.japanese(true).contains("SHELLBOY_API_KEY"));
    assert!(
        missing
            .missing
            .iter()
            .any(|fact| fact.starts_with("base_url"))
    );
    assert!(missing.missing.iter().any(|fact| fact.starts_with("model")));
    let mut configured = config.clone();
    configured.default_model = Some("fixture-model".into());
    configured.connections.get_mut("proxy").unwrap().base_url = "http://127.0.0.1:1/v1".into();
    let ready = report(
        &configured,
        Some("proxy"),
        configured.default_model.as_deref(),
        |_| true,
    );
    assert!(ready.missing.is_empty());
    let mut no_auth = configured.clone();
    no_auth.connections.get_mut("proxy").unwrap().api_key_env = None;
    assert!(
        report(&no_auth, Some("proxy"), Some("local-model"), |_| false)
            .missing
            .is_empty()
    );
    assert!(config_setup::PRECEDENCE.contains("SHELLBOY_DATA_DIR"));
    assert!(Config::parse("source='not a TOML option'").is_err());
}

#[cfg(unix)]
#[test]
fn init_never_follows_existing_or_dangling_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("absent");
    let path = root.path().join("config.toml");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let (ok, value, _) = run(command(root.path())
        .arg("--config")
        .arg(&path)
        .args(["config", "init"]));
    assert!(ok && value["result"]["created"] == false);
    assert!(!target.exists());
}

#[tokio::test]
async fn first_send_failure_has_source_and_missing_details_and_localized_next_actions() {
    use shellboy::{
        config::{Config, ConfigSource},
        control::db::Store,
        protocol::ThreadCreateArgs,
        storage::DbWorker,
    };
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("selected/config.toml");
    let config = Config {
        source: Some(ConfigSource {
            path: path.clone(),
            present: false,
        }),
        ..Config::default()
    };
    let db = DbWorker::open(root.path().join("fixture.db"))
        .await
        .unwrap();
    let store = Store::new(db, Arc::new(config), root.path().into(), "fixture".into());
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    };
    let error = store
        .send_new_thread(
            format!("fixture:{}", uuid::Uuid::new_v4()),
            args,
            "test only".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.details.as_ref().unwrap()["setup"]["config_path"],
        path.to_str().unwrap()
    );
    let notice = shellboy::config_setup::tui_error(&error);
    for required in [
        "接続が未設定",
        "モデルが未設定",
        path.to_str().unwrap(),
        "config init",
        "config check",
    ] {
        assert!(notice.contains(required), "{notice}");
    }
    store.database.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_key_recovery_has_setup_details_and_never_opens_an_http_connection() {
    use shellboy::{app::DataOwner, control::Control, protocol::*};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let key_name = format!("SHELLBOY_SETUP_ABSENT_{}", uuid::Uuid::new_v4().simple());
    std::fs::write(
        &path,
        configuration(&format!("http://{address}/v1"), true)
            .replace("SHELLBOY_SETUP_TEST_KEY", &key_name),
    )
    .unwrap();
    let owner = DataOwner::start(
        Some(path.clone()),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    let request = |command| {
        Request::new(
            format!("{}:{}", owner.instance_id, uuid::Uuid::new_v4()),
            command,
        )
    };
    let reply = control
        .request(request(Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_id: None,
        })))
        .await;
    assert!(reply.ok, "{reply:?}");
    let thread = reply.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut changes = control.subscribe();
    let sent = control
        .request(request(Command::MessageSend(MessageSendArgs {
            sender_label: None,
            now: false,
            thread_id: Some(thread.clone()),
            text: "fixture only".into(),
        })))
        .await;
    assert!(sent.ok, "{sent:?}");
    let reason = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let reply = control
                .request(request(Command::ThreadGet(ThreadTarget {
                    thread_id: Some(thread.clone()),
                })))
                .await;
            let value = reply.result.unwrap();
            if let Some(reason) = value["recovery_reason"].as_str() {
                break serde_json::from_str::<ApiError>(reason).unwrap();
            }
            match changes.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("state notification closed: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        reason.details.as_ref().unwrap()["setup"]["config_path"],
        path.to_str().unwrap()
    );
    let notice = shellboy::config_setup::tui_error(&reason);
    assert!(
        notice.contains(&key_name)
            && notice.contains("未設定または空")
            && notice.contains("config check"),
        "{notice}"
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    let listener = listener.into_std().unwrap();
    let connections = drain_connections(listener);
    assert_eq!(connections, 0, "connections after all owned work ended");
}

#[cfg(unix)]
#[test]
fn non_unicode_setup_paths_return_json_before_io_and_diagnostics_cannot_panic() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let root = tempfile::tempdir().unwrap();
    let invalid = root.path().join(OsStr::from_bytes(b"bad-\xff.toml"));
    let (ok, value, _) = run(command(root.path())
        .arg("--config")
        .arg(&invalid)
        .args(["config", "init"]));
    assert!(!ok && value["error"]["code"] == "invalid_argument");
    assert!(!invalid.exists());
    let file = root.path().join("config.toml");
    std::fs::write(&file, "").unwrap();
    let (ok, value, _) = run(command(root.path())
        .arg("--config")
        .arg(&file)
        .arg("--data-dir")
        .arg(&invalid)
        .args(["config", "check"]));
    assert!(!ok && value["error"]["details"]["setup"]["data_dir"].is_string());
    assert!(!invalid.exists());
}
