use shellboy::config::*;
use std::path::{Path, PathBuf};
fn abs(path: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{path}"))
    } else {
        PathBuf::from(path)
    }
}
fn env(platform: Platform) -> PathEnvironment {
    PathEnvironment {
        platform,
        home: Some(abs("/home/test")),
        xdg_config_home: None,
        xdg_data_home: None,
        appdata: Some(abs("/appdata")),
        localappdata: Some(abs("/localappdata")),
        data_dir: None,
    }
}
#[test]
fn unconfigured_defaults_are_valid_and_do_not_invent_models() {
    let c = Config::parse("").unwrap();
    assert!(c.default_connection.is_none());
    assert!(c.default_model.is_none());
    assert!(c.connections.is_empty());
    assert!(c.embedding.model.is_none());
    assert_eq!(c.retention.tool_raw_bytes, 1_073_741_824);
    assert_eq!(c.retention.clipboard_bytes, 268_435_456);
    assert_eq!(c.cache.body_bytes, 33_554_432);
    assert_eq!(c.indexing.retry_delays_ms, [1000, 4000]);
    assert_eq!(c.ui.send_key, "Enter");
}
#[test]
fn full_contract_and_partial_nested_defaults() {
    let c = Config::parse(
        r#"
data_dir = "state"
default_connection = "proxy"
default_model = "arbitrary-new-model"
[connections.proxy]
base_url = "https://proxy.example/v1/"
api_key_env = "SHELLBOY_API_KEY"
default_effort = "future-effort"
headers = { "X-Proxy" = "fixture" }
[embedding]
connection = "proxy"
model = "embed-fixture"
[shell]
kind = "bash"
executable = "bin/bash"
env = { LANG = "C.UTF-8" }
[retention]
tool_raw_days = 30
[limits]
attachment_file_bytes = 1024
attachment_request_bytes = 2048
[cache]
body_bytes = 0
[io]
output_buffer_bytes = 100
output_batch_bytes = 100
[workers]
attachment = 1
[indexing]
retries = 0
retry_delays_ms = []
[ui]
send_key = "Ctrl+Enter"
newline_keys = ["Enter"]
copy_backend = "osc52"
[logging]
files = 1
"#,
    )
    .unwrap();
    assert_eq!(c.retention.tool_raw_days, 30);
    assert_eq!(c.retention.clipboard_days, 7);
    assert_eq!(c.cache.body_bytes, 0);
    assert_eq!(c.cache.layout_bytes, 16 * 1024 * 1024);
    let connection = &c.connections["proxy"];
    assert_eq!(
        connection
            .endpoint(ApiEndpoint::Responses)
            .unwrap()
            .as_str(),
        "https://proxy.example/v1/responses"
    );
    assert_eq!(
        connection
            .endpoint(ApiEndpoint::Embeddings)
            .unwrap()
            .as_str(),
        "https://proxy.example/v1/embeddings"
    );
}
#[test]
fn every_section_rejects_unknown_keys_and_wrong_types() {
    for source in [
        "bogus = true",
        "[connections.x]\nbase_url='https://example.com'\nbogus=1",
        "[embedding]\nbogus=1",
        "[shell]\nbogus=1",
        "[retention]\nbogus=1",
        "[limits]\nbogus=1",
        "[cache]\nbogus=1",
        "[io]\nbogus=1",
        "[workers]\nbogus=1",
        "[indexing]\nbogus=1",
        "[ui]\nbogus=1",
        "[logging]\nbogus=1",
        "[compaction]\nconcurrency=1",
        "[limits]\ndraft_bytes='1 MiB'",
        "[cache]\nbody_bytes=-1",
    ] {
        assert!(Config::parse(source).is_err(), "accepted {source}");
    }
}
#[test]
fn invariants_are_rejected_without_clamping() {
    for source in [
        "[retention]\ntool_raw_days=0",
        "[limits]\nattachment_file_bytes=100\nattachment_request_bytes=99",
        "[limits]\ndata_warning_bytes=100\ndata_max_bytes=100",
        "[io]\noutput_batch_bytes=10\noutput_buffer_bytes=9",
        "[indexing]\nretries=1",
        "[indexing]\nretry_delays_ms=[0,1]",
        "[workers]\nattachment=0",
        "[ui]\nmax_fps=0",
        "[logging]\nfiles=0",
        "[ui]\nnewline_keys=['Enter']",
        "[ui]\nsend_key='not a key'",
        "default_connection='missing'",
        "default_model=''",
        "[embedding]\nmodel='half-configured'",
        "[shell]\nexecutable=''",
        "data_dir=''",
    ] {
        assert!(Config::parse(source).is_err(), "accepted {source}");
    }
}
#[test]
fn bad_urls_and_header_ambiguity_fail() {
    for url in [
        "ftp://example.com",
        "relative",
        "https://user:password@example.com/v1",
        "https://example.com/v1/responses",
        "https://example.com/v1/embeddings/",
        "https://example.com/v1?token=secret",
        "https://example.com/#fragment",
    ] {
        let source = format!("[connections.proxy]\nbase_url='{url}'");
        assert!(Config::parse(&source).is_err(), "accepted {url}");
    }
    for headers in ["{authorization='secret'}", "{Authorization='secret'}"] {
        assert!(Config::parse(&format!("[connections.proxy]\nbase_url='https://example.com'\napi_key_env='KEY'\nheaders={headers}")).is_err());
    }
    assert!(
        Config::parse(
            "[connections.proxy]\nbase_url='https://example.com'\nheaders={ Test='a', test='b' }"
        )
        .is_err()
    );
}
#[test]
fn secrets_are_resolved_lazily_and_debug_is_redacted() {
    let c = Config::parse("[connections.proxy]\nbase_url='https://example.com'\napi_key_env='KEY'\nheaders={ 'X-Private'='private-fixed-value' }").unwrap();
    let connection = &c.connections["proxy"];
    assert!(connection.credentials(|_| None).is_err());
    assert!(connection.credentials(|_| Some(String::new())).is_err());
    let credentials = connection
        .credentials(|name| {
            assert_eq!(name, "KEY");
            Some("private-env-value".into())
        })
        .unwrap();
    assert_eq!(
        credentials.headers()["Authorization"],
        "Bearer private-env-value"
    );
    let debug = format!("{c:?} {credentials:?}");
    assert!(!debug.contains("private-fixed-value"));
    assert!(!debug.contains("private-env-value"));
    let parse_error =
        Config::parse("[connections.proxy]\nheaders={ private='secret-diagnostic-value'\n")
            .unwrap_err()
            .to_string();
    assert!(!parse_error.contains("secret-diagnostic-value"));
}
#[test]
fn platform_paths_and_precedence_are_deterministic() {
    let mut linux = env(Platform::Linux);
    assert_eq!(
        linux.default_config_path().unwrap(),
        abs("/home/test/.config/shellboy/config.toml")
    );
    assert_eq!(
        linux.default_data_dir().unwrap(),
        abs("/home/test/.local/share/shellboy")
    );
    linux.xdg_config_home = Some(abs("/config"));
    linux.xdg_data_home = Some(abs("/data"));
    assert_eq!(
        linux.default_config_path().unwrap(),
        abs("/config/shellboy/config.toml")
    );
    assert_eq!(linux.default_data_dir().unwrap(), abs("/data/shellboy"));
    linux.data_dir = Some("env-state".into());
    let cwd_path = abs("/startup");
    let cwd = cwd_path.as_path();
    let parent_path = abs("/settings");
    let config_parent = parent_path.as_path();
    assert_eq!(
        resolve_data_dir(
            Some(Path::new("cli")),
            &linux,
            Some(Path::new("toml")),
            cwd,
            config_parent
        )
        .unwrap(),
        cwd.join("cli")
    );
    assert_eq!(
        resolve_data_dir(None, &linux, Some(Path::new("toml")), cwd, config_parent).unwrap(),
        cwd.join("env-state")
    );
    linux.data_dir = None;
    assert_eq!(
        resolve_data_dir(None, &linux, Some(Path::new("toml")), cwd, config_parent).unwrap(),
        config_parent.join("toml")
    );
    assert_eq!(
        resolve_cli_data_dir(None, &linux, cwd).unwrap(),
        abs("/data/shellboy")
    );
    assert_eq!(
        env(Platform::Macos).default_data_dir().unwrap(),
        abs("/home/test/Library/Application Support/shellboy/data")
    );
    assert_eq!(
        env(Platform::Windows).default_config_path().unwrap(),
        abs("/appdata").join("shellboy").join("config.toml")
    );
    assert_eq!(
        env(Platform::Windows).default_data_dir().unwrap(),
        abs("/localappdata").join("shellboy").join("data")
    );
    linux.xdg_data_home = Some("relative".into());
    assert!(linux.default_data_dir().is_err());
}
#[test]
fn config_loading_distinguishes_default_missing_explicit_missing_and_errors() {
    let temp = tempfile::tempdir().unwrap();
    let mut environment = env(Platform::Linux);
    environment.xdg_config_home = Some(temp.path().join("config"));
    environment.xdg_data_home = Some(temp.path().join("data"));
    let loaded = Config::load_with_env(None, temp.path(), &environment).unwrap();
    assert!(loaded.config_path.is_none());
    assert!(
        Config::load_with_env(Some(Path::new("missing.toml")), temp.path(), &environment).is_err()
    );
    std::fs::create_dir_all(temp.path().join("settings")).unwrap();
    std::fs::write(
        temp.path().join("settings/custom.toml"),
        "data_dir='state'\n[shell]\nexecutable='bin/bash'",
    )
    .unwrap();
    let loaded = Config::load_with_env(
        Some(Path::new("settings/custom.toml")),
        temp.path(),
        &environment,
    )
    .unwrap();
    assert_eq!(loaded.data_dir, temp.path().join("settings/state"));
    assert_eq!(
        loaded.config.shell.executable.unwrap(),
        temp.path().join("settings/bin/bash")
    );
    let overridden = Config::load_with_overrides(
        Some(Path::new("settings/custom.toml")),
        Some(Path::new("cli-state")),
        temp.path(),
        &environment,
    )
    .unwrap();
    assert_eq!(overridden.data_dir, temp.path().join("cli-state"));
    std::fs::create_dir_all(temp.path().join("config/shellboy")).unwrap();
    std::fs::write(
        temp.path().join("config/shellboy/config.toml"),
        "unknown = 1",
    )
    .unwrap();
    assert!(Config::load_with_env(None, temp.path(), &environment).is_err());
}

#[test]
fn worker_counts_exceeding_pool_and_shutdown_permits_fail_before_startup() {
    let too_many = tokio::sync::Semaphore::MAX_PERMITS.min(u32::MAX as usize) + 1;
    for (section, key) in [
        ("workers", "attachment"),
        ("workers", "control_file"),
        ("indexing", "concurrency"),
    ] {
        let source = format!("[{section}]\n{key}={too_many}\n");
        let error = Config::parse(&source).unwrap_err().to_string();
        assert!(error.contains(&format!("{section}.{key}")), "{error}");
        assert!(error.contains("permit range"), "{error}");
    }
}

#[test]
fn every_positive_field_and_zero_allowed_section_keeps_the_existing_contract() {
    for (section, fields) in [
        (
            "retention",
            &[
                "tool_raw_days",
                "tool_raw_bytes",
                "clipboard_days",
                "clipboard_bytes",
            ][..],
        ),
        (
            "limits",
            &[
                "attachment_file_bytes",
                "attachment_request_bytes",
                "shell_raw_bytes",
                "tool_excerpt_bytes",
                "shell_stdin_bytes",
                "draft_bytes",
                "data_warning_bytes",
                "data_max_bytes",
            ][..],
        ),
        (
            "io",
            &[
                "file_prepare_timeout_ms",
                "shell_write_timeout_ms",
                "term_grace_ms",
                "drain_timeout_ms",
                "self_reply_grace_ms",
                "shutdown_grace_ms",
                "output_buffer_bytes",
                "output_batch_bytes",
                "output_batch_ms",
            ][..],
        ),
        ("workers", &["attachment", "control_file"][..]),
        (
            "indexing",
            &["concurrency", "split_chars", "common_failure_pause_count"][..],
        ),
        (
            "ui",
            &["max_fps", "paste_fold_chars", "paste_fold_lines"][..],
        ),
        ("logging", &["file_bytes", "files"][..]),
    ] {
        for field in fields {
            let error = Config::parse(&format!("[{section}]\n{field}=0\n")).unwrap_err();
            assert!(matches!(error, ConfigError::Invalid { key, message }
                if key == format!("{section}.{field}") && message == "must be a positive integer"));
        }
    }
    Config::parse("[cache]\nbody_bytes=0\nlayout_bytes=0\nhistory_bytes=0\nattachment_base64_bytes=0\n[indexing]\nretries=0\nretry_delays_ms=[]\n").unwrap();
}

#[test]
fn validation_phases_preserve_first_error_and_do_not_echo_secret_values() {
    let too_many = tokio::sync::Semaphore::MAX_PERMITS.min(u32::MAX as usize) + 1;
    for (source, key) in [
        ("[limits]\nattachment_file_bytes=0\n[io]\nfile_prepare_timeout_ms=0\n".into(), "limits.attachment_file_bytes"),
        ("[limits]\nattachment_file_bytes=50\nattachment_request_bytes=40\n[io]\nfile_prepare_timeout_ms=0\n".into(), "io.file_prepare_timeout_ms"),
        (format!("[workers]\nattachment={too_many}\n[indexing]\nconcurrency=0\n"), "indexing.concurrency"),
        (format!("[workers]\nattachment={too_many}\n[ui]\nmax_fps=0\n"), "workers.attachment"),
        ("[limits]\nattachment_file_bytes=50\nattachment_request_bytes=40\n[connections.bad]\nbase_url='https://user:PRIVATE_PASSWORD@example.test/v1'\n".into(), "limits.attachment_file_bytes"),
        ("[connections.bad]\nbase_url='https://user:PRIVATE_PASSWORD@example.test/v1'\ndefault_effort=''\napi_key_env='BAD-NAME'\n".into(), "connections.bad.base_url"),
        ("[connections.bad]\nbase_url='https://example.test/v1'\ndefault_effort=''\napi_key_env='BAD-NAME'\n".into(), "connections.bad.default_effort"),
        ("[connections.bad]\nbase_url='https://example.test/v1'\napi_key_env='BAD-NAME'\nheaders={Authorization='PRIVATE_HEADER'}\n".into(), "connections.bad.api_key_env"),
        ("default_connection='missing'\n[shell.env]\n'BAD-NAME'='PRIVATE_ENV'\n".into(), "default_connection"),
        ("data_dir=''\n[shell]\nexecutable=''\n".into(), "data_dir"),
        ("data_dir=''\n[shell.env]\n'BAD-NAME'='PRIVATE_ENV'\n".into(), "shell.env"),
    ] {
        let error = Config::parse(&source).unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, ConfigError::Invalid { key: found, .. } if found == key), "{key}: {text}");
        assert!(!text.contains("PRIVATE_"));
    }
}

#[test]
fn tls_verification_defaults_to_enabled_and_is_connection_local() {
    assert!(Connection::default().ssl_verify);
    let direct: Connection = toml::from_str("base_url='https://example.test/v1'").unwrap();
    assert!(direct.ssl_verify);
    let config = Config::parse(
        "[connections.omitted]\nbase_url='https://example.test/v1'\n\
         [connections.verified]\nbase_url='https://example.test/v1'\nssl_verify=true\n\
         [connections.unverified]\nbase_url='https://example.test/v1'\nssl_verify=false\n",
    )
    .unwrap();
    assert!(config.connections["omitted"].ssl_verify);
    assert!(config.connections["verified"].ssl_verify);
    assert!(!config.connections["unverified"].ssl_verify);
    let diagnostic = format!("{:?}", config.connections["unverified"]);
    assert!(diagnostic.contains("ssl_verify: false"));
    assert!(!diagnostic.contains("https://example.test"));
}

#[test]
fn tls_verification_rejects_non_boolean_values() {
    for value in ["'false'", "'true'", "0", "1", "[]", "{}"] {
        let source = format!(
            "[connections.proxy]\nbase_url='https://example.test/v1'\nssl_verify={value}\n"
        );
        assert!(Config::parse(&source).is_err(), "accepted {value}");
    }
}

#[test]
fn http_timeouts_have_defaults_and_independent_connection_overrides() {
    let defaults = Connection::default();
    assert_eq!(defaults.response_header_timeout_ms, 120_000);
    assert_eq!(defaults.stream_idle_timeout_ms, 600_000);
    assert_eq!(defaults.request_timeout_ms, 60_000);
    let direct: Connection = toml::from_str("base_url='https://example.test/v1'").unwrap();
    assert_eq!(direct.stream_idle_timeout_ms, 600_000);
    assert_eq!(direct.request_timeout_ms, 60_000);
    let config = Config::parse(
        "[connections.defaulted]\nbase_url='https://example.test/v1'\n\
         [connections.unlimited]\nbase_url='https://example.test/v1'\nstream_idle_timeout_ms=0\nrequest_timeout_ms=1000\n\
         [connections.long_running]\nbase_url='https://example.test/v1'\nstream_idle_timeout_ms=1800000\nrequest_timeout_ms=120000\n",
    )
    .unwrap();
    assert_eq!(
        config.connections["defaulted"].stream_idle_timeout_ms,
        600_000
    );
    assert_eq!(config.connections["defaulted"].request_timeout_ms, 60_000);
    assert_eq!(config.connections["unlimited"].stream_idle_timeout_ms, 0);
    assert_eq!(config.connections["unlimited"].request_timeout_ms, 1_000);
    assert_eq!(
        config.connections["long_running"].stream_idle_timeout_ms,
        1_800_000
    );
    assert_eq!(
        config.connections["long_running"].request_timeout_ms,
        120_000
    );
    let debug = format!("{:?}", config.connections["unlimited"]);
    assert!(debug.contains("stream_idle_timeout_ms: 0"));
    assert!(debug.contains("request_timeout_ms: 1000"));
    assert!(!debug.contains("https://example.test"));
}

#[test]
fn http_timeouts_reject_invalid_values_without_clamping() {
    for key in [
        "response_header_timeout_ms",
        "stream_idle_timeout_ms",
        "request_timeout_ms",
    ] {
        for value in [
            "-1",
            "'60000'",
            "1.5",
            "true",
            "[]",
            "{}",
            "18446744073709551616",
        ] {
            let source =
                format!("[connections.proxy]\nbase_url='https://example.test/v1'\n{key}={value}\n");
            assert!(Config::parse(&source).is_err(), "accepted {key}={value}");
        }
    }
    for value in [0, 1, 999] {
        let source = format!(
            "[connections.proxy]\nbase_url='https://example.test/v1'\nrequest_timeout_ms={value}\n"
        );
        let error = Config::parse(&source).unwrap_err();
        assert!(
            matches!(error, ConfigError::Invalid { key, .. } if key == "connections.proxy.request_timeout_ms")
        );
    }
    let config = Config::parse("[connections.proxy]\nbase_url='https://example.test/v1'\nstream_idle_timeout_ms=1\nrequest_timeout_ms=1000\n").unwrap();
    assert_eq!(config.connections["proxy"].stream_idle_timeout_ms, 1);
}

#[test]
fn response_retries_have_defaults_and_independent_connection_overrides() {
    let defaults = Connection::default();
    assert_eq!(defaults.response_max_retries, 8);
    assert_eq!(defaults.response_retry_max_delay_ms, 60_000);
    assert!(defaults.response_retry_partial);
    let direct: Connection = toml::from_str("base_url='https://example.test/v1'").unwrap();
    assert_eq!(direct.response_max_retries, 8);
    assert_eq!(direct.response_retry_max_delay_ms, 60_000);
    assert!(direct.response_retry_partial);
    let config = Config::parse(
        "[connections.defaulted]\nbase_url='https://example.test/v1'\n\
         [connections.disabled]\nbase_url='https://example.test/v1'\nresponse_max_retries=0\nresponse_retry_max_delay_ms=1\nresponse_retry_partial=false\n\
         [connections.maximum]\nbase_url='https://example.test/v1'\nresponse_max_retries=20\nresponse_retry_max_delay_ms=300000\n",
    )
    .unwrap();
    assert_eq!(config.connections["defaulted"].response_max_retries, 8);
    assert_eq!(
        config.connections["defaulted"].response_retry_max_delay_ms,
        60_000
    );
    assert!(config.connections["defaulted"].response_retry_partial);
    assert!(!config.connections["disabled"].response_retry_partial);
    assert_eq!(config.connections["disabled"].response_max_retries, 0);
    assert_eq!(
        config.connections["disabled"].response_retry_max_delay_ms,
        1
    );
    assert_eq!(config.connections["maximum"].response_max_retries, 20);
    assert_eq!(
        config.connections["maximum"].response_retry_max_delay_ms,
        300_000
    );
    assert_eq!(
        config.connections["maximum"].stream_idle_timeout_ms,
        600_000
    );
    assert_eq!(config.connections["maximum"].request_timeout_ms, 60_000);
    let debug = format!("{:?}", config.connections["maximum"]);
    assert!(debug.contains("response_max_retries: 20"));
    assert!(debug.contains("response_retry_max_delay_ms: 300000"));
    assert!(debug.contains("response_retry_partial: true"));
    assert!(!debug.contains("https://example.test"));
}

#[test]
fn response_retries_reject_invalid_values_without_clamping() {
    for key in ["response_max_retries", "response_retry_max_delay_ms"] {
        for value in [
            "-1",
            "'2'",
            "1.5",
            "true",
            "[]",
            "{}",
            "18446744073709551616",
        ] {
            let source =
                format!("[connections.proxy]\nbase_url='https://example.test/v1'\n{key}={value}\n");
            assert!(Config::parse(&source).is_err(), "accepted {key}={value}");
        }
    }
    for (key, value) in [
        ("response_max_retries", 21_u64),
        ("response_max_retries", 4_294_967_296),
        ("response_retry_max_delay_ms", 0),
        ("response_retry_max_delay_ms", 300_001),
    ] {
        let source =
            format!("[connections.proxy]\nbase_url='https://example.test/v1'\n{key}={value}\n");
        let error = Config::parse(&source).unwrap_err();
        assert!(
            matches!(error, ConfigError::Invalid { key: ref actual, .. } if actual == &format!("connections.proxy.{key}")),
            "unexpected error for {key}={value}: {error}"
        );
    }
}

#[test]
fn response_partial_retry_and_ui_bell_are_strict_default_on_booleans() {
    assert!(Config::default().ui.bell);
    assert!(Config::parse("").unwrap().ui.bell);
    let config = Config::parse(
        "[connections.proxy]\nbase_url='https://example.test/v1'\nresponse_retry_partial=false\n[ui]\nbell=false\n",
    ).unwrap();
    assert!(!config.connections["proxy"].response_retry_partial);
    assert!(!config.ui.bell);
    for value in ["'false'", "0", "1", "[]", "{}"] {
        assert!(Config::parse(&format!("[ui]\nbell={value}\n")).is_err());
        assert!(Config::parse(&format!(
            "[connections.proxy]\nbase_url='https://example.test/v1'\nresponse_retry_partial={value}\n"
        )).is_err());
    }
}

#[test]
fn thread_session_settings_have_independent_defaults_and_boolean_overrides() {
    let rust_defaults = Connection::default();
    assert!(rust_defaults.session_header);
    assert!(!rust_defaults.prompt_cache_key);
    let serde_defaults: Connection = toml::from_str("base_url='https://example.test/v1'").unwrap();
    assert!(serde_defaults.session_header);
    assert!(!serde_defaults.prompt_cache_key);
    let config = Config::parse(
        "[connections.defaulted]\nbase_url='https://example.test/v1'\n\
         [connections.enabled]\nbase_url='https://example.test/v1'\nsession_header=true\nprompt_cache_key=true\n\
         [connections.disabled]\nbase_url='https://example.test/v1'\nsession_header=false\nprompt_cache_key=false\n",
    ).unwrap();
    assert!(config.connections["defaulted"].session_header);
    assert!(!config.connections["defaulted"].prompt_cache_key);
    assert!(config.connections["enabled"].session_header);
    assert!(config.connections["enabled"].prompt_cache_key);
    assert!(!config.connections["disabled"].session_header);
    assert!(!config.connections["disabled"].prompt_cache_key);
    let debug = format!("{:?}", config.connections["defaulted"]);
    assert!(debug.contains("session_header: true"));
    assert!(debug.contains("prompt_cache_key: false"));
    assert!(!debug.contains("https://example.test"));
}

#[test]
fn thread_session_settings_reject_non_boolean_values() {
    for key in ["session_header", "prompt_cache_key"] {
        for value in ["'true'", "'false'", "0", "1", "1.5", "[]", "{}"] {
            let source =
                format!("[connections.proxy]\nbase_url='https://example.test/v1'\n{key}={value}\n");
            assert!(Config::parse(&source).is_err(), "accepted {key}={value}");
        }
    }
}

#[test]
fn disconnect_trim_settings_have_defaults_and_strict_connection_overrides() {
    let defaults = Connection::default();
    assert!(defaults.auto_trim_on_disconnect);
    assert_eq!(defaults.compact_hint_tokens, None);
    let defaulted: Connection = toml::from_str("base_url='https://example.test/v1'").unwrap();
    assert!(defaulted.auto_trim_on_disconnect);
    assert_eq!(defaulted.compact_hint_tokens, None);
    let config = Config::parse("[connections.proxy]\nbase_url='https://example.test/v1'\nauto_trim_on_disconnect=false\ncompact_hint_tokens=1\n").unwrap();
    assert!(!config.connections["proxy"].auto_trim_on_disconnect);
    assert_eq!(config.connections["proxy"].compact_hint_tokens, Some(1));
    let debug = format!("{:?}", config.connections["proxy"]);
    assert!(debug.contains("auto_trim_on_disconnect: false"));
    assert!(debug.contains("compact_hint_tokens: Some(1)"));
    for (key, values) in [
        (
            "auto_trim_on_disconnect",
            vec!["'false'", "0", "1", "[]", "{}"],
        ),
        (
            "compact_hint_tokens",
            vec!["0", "-1", "'75000'", "1.5", "true", "[]", "{}"],
        ),
    ] {
        for value in values {
            assert!(
                Config::parse(&format!(
                    "[connections.proxy]\nbase_url='https://example.test/v1'\n{key}={value}\n"
                ))
                .is_err(),
                "accepted {key}={value}"
            );
        }
    }
}

#[test]
fn obsolete_history_read_budget_is_rejected_instead_of_silently_ignored() {
    assert!(
        toml::from_str::<shellboy::config::Config>("[limits]\nhistory_read_bytes=65536\n").is_err()
    );
}

#[test]
fn response_header_timeout_is_independent_positive_and_debuggable() {
    let config = Config::parse("[connections.proxy]\nbase_url='https://example.test/v1'\nresponse_header_timeout_ms=1234\nstream_idle_timeout_ms=0\n").unwrap();
    assert_eq!(config.connections["proxy"].response_header_timeout_ms, 1234);
    assert_eq!(config.connections["proxy"].stream_idle_timeout_ms, 0);
    assert!(
        format!("{:?}", config.connections["proxy"]).contains("response_header_timeout_ms: 1234")
    );
    let error = Config::parse(
        "[connections.proxy]\nbase_url='https://example.test/v1'\nresponse_header_timeout_ms=0\n",
    )
    .unwrap_err();
    assert!(
        matches!(error,ConfigError::Invalid{key,..} if key=="connections.proxy.response_header_timeout_ms")
    );
}
