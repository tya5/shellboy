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

            assert_eq!(value["result"]["compaction"]["summary_max_chars"], 20000);
            assert_eq!(value["result"]["compaction"]["summary_max_percent"], 5.0);
