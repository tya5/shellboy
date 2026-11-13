//! `config.get` / `config.set` (#266). Temporary files and data directories only.
use crate::runtime_features::{owner, request};
use serde_json::{Value, json};
use shellboy::{
    config::Config,
    config_edit::{
        MAX_FILE_BYTES, apply, apply_with, apply_with_replace, edit_text, looks_like_credential,
        read,
    },
    config_setup::TEMPLATE,
    control::Control,
    protocol::{
        ApiError, Command, ConfigChange, ConfigGetArgs, ConfigSetArgs, ErrorCode, Request,
        ThreadCreateArgs, ThreadSetArgs, ThreadTarget,
    },
};
use std::{
    path::{Path, PathBuf},
    process::Command as Process,
};

const SECRETS: [&str; 6] = [
    "header-secret-value",
    "extra-header-secret-value",
    "nested-body-secret-value",
    "shell-env-secret-value",
    "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
    "sk-pathtoken0123456789abcdef",
];
const FULL: &str = r#"# Top comment
default_connection = "proxy" # why this one
default_model = "model-a"

# Proxy connection
[connections.proxy]
base_url = "https://proxy.example/v1/sk-pathtoken0123456789abcdef"
api_key_env = "ghp_abcdefghijklmnopqrstuvwxyz0123456789"
extra_body = { metadata = { key = "nested-body-secret-value" }, limit = 1 }

[connections.proxy.headers]
X-Org = "header-secret-value"

[connections.proxy.extra_headers]
X-Extra = "extra-header-secret-value"

[connections.plain]
base_url = "https://plain.example/v1"
api_key_env = "PLAIN_API_KEY"

[shell.env]
MY_VAR = "shell-env-secret-value"

# Interface
[ui]
# Ring on completion
bell = true
language = "en"
"#;

fn change(field: &str, value: Value) -> ConfigChange {
    ConfigChange {
        field: field.into(),
        value: Some(value),
    }
}
fn removal(field: &str) -> ConfigChange {
    ConfigChange {
        field: field.into(),
        value: None,
    }
}
fn fixture(text: &str) -> (tempfile::TempDir, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path, text).unwrap();
    let hash = hash_of(&path);
    (root, path, hash)
}
fn hash_of(path: &Path) -> String {
    let loaded = loaded(path);
    read(&loaded).unwrap()["file"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn loaded(path: &Path) -> Config {
    let parent = path.parent().unwrap();
    Config::load_with_overrides(
        Some(path),
        Some(&parent.join("data")),
        parent,
        &shellboy::config::PathEnvironment::current(),
    )
    .unwrap()
    .config
}
fn assert_no_secret(text: &str) {
    for secret in SECRETS {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
}
fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn every_value_that_could_hold_a_secret_is_redacted_and_names_remain() {
    let (_root, path, _) = fixture(FULL);
    let result = read(&loaded(&path)).unwrap();
    assert_no_secret(&result.to_string());
    for side in ["loaded", "file"] {
        let values = &result[side]["values"];
        let proxy = &values["connections"]["proxy"];
        assert_eq!(proxy["headers"]["X-Org"], "[redacted]");
        assert_eq!(proxy["extra_headers"]["X-Extra"], "[redacted]");
        // Top-level names of extra_body only: nothing below them, not even numbers.
        assert_eq!(proxy["extra_body"]["metadata"], "[redacted]");
        assert_eq!(proxy["extra_body"]["limit"], "[redacted]");
        assert_eq!(values["shell"]["env"]["MY_VAR"], "[redacted]");
        assert_eq!(proxy["api_key_env"], "[redacted]");
        assert_eq!(
            proxy["base_url"], "https://proxy.example/v1/[redacted]",
            "credential-like path segment"
        );
        // Ordinary values and variable names stay readable.
        let plain = &values["connections"]["plain"];
        assert_eq!(plain["api_key_env"], "PLAIN_API_KEY");
        assert_eq!(plain["base_url"], "https://plain.example/v1");
        assert_eq!(values["default_model"], "model-a");
        assert_eq!(values["ui"]["bell"], true);
        let listed: Vec<&str> = result[side]["redacted_fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        for name in [
            "connections.proxy.headers.X-Org",
            "connections.proxy.extra_headers.X-Extra",
            "connections.proxy.extra_body.metadata",
            "connections.proxy.api_key_env",
            "connections.proxy.base_url",
            "shell.env.MY_VAR",
        ] {
            assert!(listed.contains(&name), "{name} in {listed:?}");
        }
    }
    assert_eq!(result["differs"], false);
    assert_eq!(result["differing_fields"], json!([]));
    assert!(result["loaded"]["content_hash"].is_string());
    assert_eq!(result["restart"]["all_fields_require_restart"], false);
}

#[test]
fn a_file_with_a_url_password_is_reported_invalid_without_quoting_it() {
    let (_root, path, _) = fixture("default_model = \"a\"\n");
    let config = loaded(&path);
    std::fs::write(
        &path,
        "[connections.x]\nbase_url = \"https://user:url-password-value@host.example/v1\"\n",
    )
    .unwrap();
    let result = read(&config).unwrap();
    assert_eq!(result["file"]["valid"], false);
    assert!(result["file"]["values"].is_null());
    assert!(result["differing_fields"].is_null());
    assert_eq!(result["differs"], true);
    assert!(
        result["file"]["error"]
            .as_str()
            .unwrap()
            .contains("base_url")
    );
    assert!(!result.to_string().contains("url-password-value"));
}

#[test]
fn syntax_and_type_errors_in_the_file_never_quote_a_value() {
    for text in [
        "[connections.x\nbase_url = \"syntax-secret-value\"\n",
        "default_model = \"syntax-secret-value\nnext = 1\n",
        "default_model = [\"syntax-secret-value\"]\n",
    ] {
        let (_root, path, _) = fixture("");
        let config = loaded(&path);
        std::fs::write(&path, text).unwrap();
        let result = read(&config).unwrap();
        assert_eq!(result["file"]["valid"], false, "{text}");
        assert!(
            !result.to_string().contains("syntax-secret-value"),
            "{result}"
        );
    }
}

#[test]
fn only_changed_lines_differ_and_comments_and_order_survive() {
    let edited = edit_text(
        FULL,
        &[
            change("default_model", json!("model-b")),
            change("ui.bell", json!(false)),
            change("connections.plain.ssl_verify", json!(false)),
        ],
    )
    .unwrap();
    let expected = FULL
        .replace("default_model = \"model-a\"", "default_model = \"model-b\"")
        .replace("bell = true", "bell = false")
        .replace(
            "api_key_env = \"PLAIN_API_KEY\"\n",
            "api_key_env = \"PLAIN_API_KEY\"\nssl_verify = false\n",
        );
    assert_eq!(edited, expected);
    // A trailing comment belongs to the value's line and stays.
    let edited = edit_text(FULL, &[change("default_connection", json!("plain"))]).unwrap();
    assert!(edited.contains("default_connection = \"plain\" # why this one\n"));
}

#[test]
fn connection_spill_is_visible_and_editable_as_a_native_boolean() {
    let text = "[connections.proxy]\nbase_url = \"https://proxy.example/v1\"\nspill = false\n";
    let (_root, path, _) = fixture(text);
    let values = read(&loaded(&path)).unwrap();
    assert_eq!(
        values["file"]["values"]["connections"]["proxy"]["spill"],
        false
    );
    let edited = edit_text(text, &[change("connections.proxy.spill", json!(true))]).unwrap();
    assert!(edited.contains("spill = true"));
    let cleared = edit_text(&edited, &[removal("connections.proxy.spill")]).unwrap();
    assert!(!cleared.contains("spill ="));
    assert_eq!(
        Config::parse(&cleared).unwrap().connections["proxy"].spill,
        None
    );
}

#[test]
fn the_commented_template_keeps_every_other_line() {
    let edited = edit_text(TEMPLATE, &[change("default_model", json!("chosen"))]).unwrap();
    let before: Vec<&str> = TEMPLATE.lines().collect();
    let after: Vec<&str> = edited.lines().collect();
    assert_eq!(before.len(), after.len());
    let different: Vec<_> = before
        .iter()
        .zip(&after)
        .filter(|(old, new)| old != new)
        .collect();
    assert_eq!(different.len(), 1, "{different:?}");
    assert!(different[0].1.contains("chosen"));
}

#[test]
fn crlf_line_endings_stay() {
    let text = FULL.replace('\n', "\r\n");
    let edited = edit_text(&text, &[change("ui.bell", json!(false))]).unwrap();
    assert_eq!(edited, text.replace("bell = true", "bell = false"));
}

#[test]
fn removing_a_key_keeps_the_comment_above_it() {
    // Next key in the same table inherits the comment.
    let edited = edit_text(FULL, &[removal("ui.bell")]).unwrap();
    assert!(
        edited.contains("# Ring on completion\nlanguage = \"en\"\n"),
        "{edited}"
    );
    // Last key of a table: the comment moves before the next table.
    let text = "[ui]\nbell = true\n# about the key\nlanguage = \"ja\"\n\n# Later\n[io]\nterm_grace_ms = 5\n";
    let edited = edit_text(text, &[removal("ui.language")]).unwrap();
    assert_eq!(
        edited,
        "[ui]\nbell = true\n# about the key\n\n# Later\n[io]\nterm_grace_ms = 5\n"
    );
    // Last key of the document: it moves to the trailing text.
    let text = "[ui]\nbell = true\n# about the key\nlanguage = \"ja\"\n# End\n";
    let edited = edit_text(text, &[removal("ui.language")]).unwrap();
    assert_eq!(edited, "[ui]\nbell = true\n# about the key\n# End\n");
    // Removing what is not there changes nothing.
    assert_eq!(edit_text(FULL, &[removal("ui.max_fps")]).unwrap(), FULL);
    assert_eq!(
        edit_text(FULL, &[removal("cache.body_bytes")]).unwrap(),
        FULL
    );
}

#[test]
fn a_connection_can_be_added_and_removed() {
    let (_root, path, hash) = fixture(FULL);
    let applied = apply(
        &path,
        &hash,
        &[change(
            "connections.\"new.one\"",
            json!({"base_url":"https://new.example/v1","api_key_env":"NEW_KEY","headers":{"X-New":"new-header-secret"}}),
        )],
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    // Beside the other connections, with every existing line kept in order.
    let added = "[connections.\"new.one\"]\napi_key_env = \"NEW_KEY\"\nbase_url = \"https://new.example/v1\"\nheaders = { X-New = \"new-header-secret\" }\n\n";
    assert_eq!(
        text,
        FULL.replace("[shell.env]", &format!("{added}[shell.env]"))
    );
    let config = Config::parse(&text).unwrap();
    assert_eq!(
        config.connections["new.one"].base_url,
        "https://new.example/v1"
    );
    let result = applied.result.to_string();
    let changes = applied.result["changes"].as_array().unwrap();
    assert!(changes.iter().any(|entry| {
        entry["field"] == "connections.\"new.one\".base_url"
            && entry["value"] == "https://new.example/v1"
    }));
    assert!(!result.contains("new-header-secret"), "{result}");
    assert_eq!(
        config.connections["proxy"].headers["X-Org"],
        "header-secret-value"
    );

    // A referenced connection stays; the validation names the reference.
    let hash = hash_of(&path);
    let error = apply(&path, &hash, &[removal("connections.proxy")]).unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(
        error.message.contains("default_connection"),
        "{}",
        error.message
    );

    let before = std::fs::read_to_string(&path).unwrap();
    apply(&path, &hash, &[removal("connections.plain")]).unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(!after.contains("PLAIN_API_KEY"));
    assert!(after.contains("# Interface\n[ui]\n"));
    assert_ne!(before, after);
    assert!(
        Config::parse(&after)
            .unwrap()
            .connections
            .contains_key("proxy")
    );
    // Other tables and an existing connection are changed field by field.
    let hash = hash_of(&path);
    for bad in [
        removal("ui"),
        change("ui", json!({"bell":false})),
        change(
            "connections.proxy",
            json!({"base_url":"https://x.example/v1"}),
        ),
    ] {
        let error = apply(&path, &hash, &[bad]).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{}", error.message);
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
}

#[test]
fn an_invalid_result_is_refused_and_the_file_is_untouched() {
    let (root, path, hash) = fixture(FULL);
    for bad in [
        change("ui.bell", json!("not-a-boolean-secret")),
        change("ui.unknown_key", json!(1)),
        change("default_connection", json!("no-such-connection")),
        change(
            "connections.plain.base_url",
            json!("https://u:p-secret-value@host.example/v1"),
        ),
        change(
            "connections.plain.base_url",
            json!("https://host.example/v1?key=q-secret-value"),
        ),
        change("limits.draft_bytes", json!(0)),
        change("connections.plain.api_key_env", json!("not a name")),
        change("default_model.child", json!("x")),
        change("data_dir", json!("/elsewhere")),
        removal("data_dir"),
        change("limits.draft_bytes", json!(u64::MAX)),
    ] {
        let name = bad.field.clone();
        let error = apply(&path, &hash, &[change("ui.language", json!("ja")), bad]).unwrap_err();
        assert!(
            matches!(
                error.code,
                ErrorCode::ConfigInvalid | ErrorCode::InvalidArgument
            ),
            "{name}: {error:?}"
        );
        let text = serde_json::to_string(&error).unwrap();
        assert!(!text.contains("secret-value"), "{text}");
        assert!(!text.contains("not-a-boolean-secret"), "{text}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL, "{name}");
    }
    assert_eq!(entries(root.path()), ["config.toml"]);
}

#[test]
fn a_changed_file_is_a_conflict_and_is_left_alone() {
    let (_root, path, hash) = fixture(FULL);
    let outside = format!("{FULL}# Edited elsewhere\n");
    std::fs::write(&path, &outside).unwrap();
    let error = apply(&path, &hash, &[change("ui.bell", json!(false))]).unwrap_err();
    assert_eq!(error.code, ErrorCode::OperationConflict);
    let details = error.details.as_ref().unwrap();
    assert_eq!(details["reason"], "config_changed");
    assert_eq!(details["expected_hash"], hash.as_str());
    assert_eq!(details["current_hash"], hash_of(&path).as_str());
    assert!(error.hint.as_deref().unwrap().contains("config get"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), outside);
}

#[test]
fn failure_before_the_replace_leaves_the_file_and_no_temporary() {
    let (root, path, hash) = fixture(FULL);
    let error = apply_with(
        &path,
        &hash,
        &[change("ui.bell", json!(false))],
        |temporary| {
            assert!(temporary.exists());
            assert_eq!(
                temporary.parent(),
                std::fs::canonicalize(&path).unwrap().parent()
            );
            Err(std::io::Error::other("synthetic write failure"))
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(error.details.unwrap()["written"], false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL);
    assert_eq!(entries(root.path()), ["config.toml"]);
}

#[test]
fn an_edit_arriving_during_the_write_is_detected_at_the_last_look() {
    let (root, path, hash) = fixture(FULL);
    let outside = format!("{FULL}# Raced\n");
    let error = apply_with(&path, &hash, &[change("ui.bell", json!(false))], |_| {
        std::fs::write(&path, &outside)
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::OperationConflict);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), outside);
    assert_eq!(entries(root.path()), ["config.toml"]);
}

#[test]
fn a_missing_file_is_created_from_the_hash_of_nothing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("nested/config.toml");
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let applied = apply(&path, empty, &[change("ui.bell", json!(false))]).unwrap();
    assert!(applied.written);
    assert!(
        !Config::parse(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .ui
            .bell
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[cfg(unix)]
#[test]
fn permissions_and_symbolic_links_are_kept() {
    use std::os::unix::fs::PermissionsExt;
    let (root, path, hash) = fixture(FULL);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    apply(&path, &hash, &[change("ui.bell", json!(false))]).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o640);

    let link = root.path().join("linked.toml");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let hash = hash_of(&link);
    apply(&link, &hash, &[change("ui.bell", json!(true))]).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        Config::parse(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .ui
            .bell
    );
}

#[cfg(windows)]
#[test]
fn the_windows_replace_keeps_the_target_and_refuses_when_it_is_open_exclusively() {
    use std::os::windows::fs::OpenOptionsExt;
    let (root, path, hash) = fixture(FULL);
    apply(&path, &hash, &[change("ui.bell", json!(false))]).unwrap();
    assert!(
        !Config::parse(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .ui
            .bell
    );
    let hash = hash_of(&path);
    let before = std::fs::read_to_string(&path).unwrap();
    // Reads stay allowed, so the hash checks pass and ReplaceFileW itself is what fails.
    const FILE_SHARE_READ: u32 = 1;
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();
    let error = apply(&path, &hash, &[change("ui.bell", json!(true))]).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        error
            .message
            .contains("Cannot replace the configuration file"),
        "{}",
        error.message
    );
    drop(held);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert_eq!(entries(root.path()), ["config.toml"]);
}

fn interrupted(code: i32) -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(code))
}

#[test]
fn an_interrupted_windows_replace_keeps_the_only_copy_and_finishes_the_move() {
    // These run the real recovery on every OS, with the OS failure simulated.
    let bell = || vec![change("ui.bell", json!(false))];
    let (root, path, hash) = fixture(FULL);
    let applied = apply_with_replace(
        &path,
        &hash,
        &bell(),
        |_| Ok(()),
        |_, target, _| {
            std::fs::remove_file(target).unwrap();
            interrupted(1176)
        },
    )
    .unwrap();
    assert!(
        applied.result["warning"]
            .as_str()
            .unwrap()
            .contains("permissions may differ")
    );
    assert!(
        !Config::parse(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .ui
            .bell
    );
    assert_eq!(entries(root.path()), ["config.toml"]);

    let (root, path, hash) = fixture(FULL);
    let aside = root.path().join("windows-chose-this");
    let applied = apply_with_replace(
        &path,
        &hash,
        &bell(),
        |_| Ok(()),
        |_, target, _| {
            std::fs::rename(target, &aside).unwrap();
            interrupted(1177)
        },
    )
    .unwrap();
    assert!(
        applied.result["warning"]
            .as_str()
            .unwrap()
            .contains("previous version remains")
    );
    assert!(
        !Config::parse(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .ui
            .bell
    );
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), FULL);

    // The move fails too: the new contents stay and the error says where.
    for (code, survives) in [(1176, false), (1177, true)] {
        let (root, path, hash) = fixture(FULL);
        let error = apply_with_replace(
            &path,
            &hash,
            &bell(),
            |_| Ok(()),
            |_, target, _| {
                std::fs::remove_file(target).unwrap();
                std::fs::create_dir(target).unwrap();
                interrupted(code)
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed);
        assert!(!error.message.contains("unchanged"), "{}", error.message);
        let details = error.details.unwrap();
        assert_eq!(details["reason"], "replace_interrupted");
        assert_eq!(details["previous_version_survives"], survives);
        let kept = PathBuf::from(details["recovery_file"].as_str().unwrap());
        assert!(
            !Config::parse(&std::fs::read_to_string(&kept).unwrap())
                .unwrap()
                .ui
                .bell
        );
        assert_eq!(entries(root.path()).len(), 2);
    }

    // Every other failure leaves both files under their own names; the temporary goes.
    let (root, path, hash) = fixture(FULL);
    let error = apply_with_replace(
        &path,
        &hash,
        &bell(),
        |_| Ok(()),
        |_, _, _| interrupted(1175),
    )
    .unwrap_err();
    assert!(error.message.contains("unchanged"), "{}", error.message);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL);
    assert_eq!(entries(root.path()), ["config.toml"]);
}

#[test]
fn a_change_that_would_outgrow_the_read_bound_is_refused() {
    let padding = format!("# {}\n", "x".repeat(MAX_FILE_BYTES as usize - 16));
    let (_root, path, hash) = fixture(&padding);
    let error = apply(
        &path,
        &hash,
        &[change("default_model", json!("a-longer-model-name"))],
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert_eq!(error.details.unwrap()["written"], false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), padding);
    assert_eq!(
        hash_of(&path),
        hash,
        "the file can still be read and changed"
    );
}

#[test]
fn writing_back_a_placeholder_is_refused() {
    let (_root, path, hash) = fixture(FULL);
    for bad in [
        change("connections.proxy.headers.X-Org", json!("[redacted]")),
        change("shell.env.MY_VAR", json!("[redacted]")),
        change("connections.proxy.extra_body.metadata", json!("[redacted]")),
        change(
            "connections.proxy.extra_body",
            json!({"metadata":"[redacted]"}),
        ),
        change(
            "connections.proxy.base_url",
            json!("https://proxy.example/v1/[redacted]"),
        ),
        change("connections.proxy.api_key_env", json!("[redacted]")),
        change("shell.env.\"[redacted-name-12345678]\"", json!("x")),
        change("ui.newline_keys", json!(["[redacted]"])),
    ] {
        let name = bad.field.clone();
        let error = apply(&path, &hash, &[bad]).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}");
        assert_eq!(
            error.details.unwrap()["reason"],
            "redacted_placeholder_refused",
            "{name}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL);
    }
}

#[test]
fn a_secret_pasted_as_a_name_is_hidden_and_can_be_removed() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz012345";
    let text = format!(
        "[connections.a]\nbase_url = \"https://a.example/v1\"\n[connections.a.headers]\n{secret} = \"v\"\nX-Real = \"v\"\n[shell.env]\n{secret} = \"v\"\n"
    );
    let (_root, path, hash) = fixture(&text);
    let result = read(&loaded(&path)).unwrap();
    assert!(!result.to_string().contains(secret), "{result}");
    let headers = &result["file"]["values"]["connections"]["a"]["headers"];
    assert_eq!(headers["X-Real"], "[redacted]");
    assert_eq!(headers.as_object().unwrap().len(), 2);
    let applied = apply(&path, &hash, &[removal(&format!("shell.env.{secret}"))]).unwrap();
    assert!(applied.written);
    assert!(!applied.result.to_string().contains(secret));
    assert!(!format!("{:?}", applied.fields).contains(secret));
    assert!(
        Config::parse(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .shell
            .env
            .is_empty()
    );
}

#[test]
fn a_new_root_key_stays_below_the_opening_comment() {
    let model = [change("default_model", json!("m"))];
    assert_eq!(
        edit_text("# Header\n\n# Interface\n[ui]\nbell = true\n", &model).unwrap(),
        "# Header\n\ndefault_model = \"m\"\n\n# Interface\n[ui]\nbell = true\n"
    );
    assert_eq!(
        edit_text("# Only comments\n", &model).unwrap(),
        "# Only comments\ndefault_model = \"m\"\n"
    );
    // With no blank line the comment describes the table, so it stays with the table.
    assert_eq!(
        edit_text("# Interface\n[ui]\nbell = true\n", &model).unwrap(),
        "default_model = \"m\"\n# Interface\n[ui]\nbell = true\n"
    );
}

#[test]
fn a_read_only_file_is_refused() {
    let (root, path, hash) = fixture(FULL);
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    let error = apply(&path, &hash, &[change("ui.bell", json!(false))]).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL);
    assert_eq!(entries(root.path()), ["config.toml"]);
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
}

#[test]
fn each_changed_field_says_it_needs_a_restart_and_sensitive_values_are_left_out() {
    let (_root, path, hash) = fixture(FULL);
    let applied = apply(
        &path,
        &hash,
        &[
            change("default_model", json!("model-b")),
            change("ui.language", json!("ja")),
            change(
                "connections.plain.headers",
                json!({"X-New":"written-secret-value"}),
            ),
            change("shell.env.OTHER", json!("written-secret-value")),
            change("connections.plain.response_max_retries", json!(3)),
            removal("ui.bell"),
        ],
    )
    .unwrap();
    let result = &applied.result;
    assert_eq!(result["written"], true);
    assert_eq!(result["restart_required"], true);
    assert_eq!(result["applied_to_running_owner"], false);
    assert_eq!(result["content_hash"], hash_of(&path).as_str());
    assert_ne!(result["previous_hash"], result["content_hash"]);
    assert!(!result.to_string().contains("written-secret-value"));
    let changes = result["changes"].as_array().unwrap();
    let find = |name: &str| {
        changes
            .iter()
            .find(|entry| entry["field"] == name)
            .unwrap_or_else(|| panic!("{name} in {changes:?}"))
    };
    for entry in changes {
        assert_eq!(entry["restart_required"], true, "{entry}");
    }
    let model = find("default_model");
    assert_eq!(model["previous"], "model-a");
    assert_eq!(model["value"], "model-b");
    assert_eq!(model["sensitive"], false);
    for name in ["connections.plain.headers.X-New", "shell.env.OTHER"] {
        let entry = find(name);
        assert_eq!(entry["sensitive"], true);
        assert!(entry.get("value").is_none() && entry.get("previous").is_none());
    }
    // A removed value's effect: the default that now applies. It was already the default.
    assert!(changes.iter().all(|entry| entry["field"] != "ui.bell"));
    assert!(applied.fields.contains(&"shell.env.OTHER".to_owned()));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("written-secret-value")
    );
}

#[test]
fn a_change_to_the_same_value_writes_nothing() {
    let (_root, path, hash) = fixture(FULL);
    let applied = apply(&path, &hash, &[change("default_model", json!("model-a"))]).unwrap();
    assert!(!applied.written);
    assert_eq!(applied.result["restart_required"], false);
    assert_eq!(applied.result["changes"], json!([]));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL);
}

#[test]
fn a_credential_in_a_name_field_is_refused_without_echoing_it() {
    let (_root, path, hash) = fixture(FULL);
    for (field, value, secret) in [
        (
            "connections.plain.api_key_env",
            json!("sk-abcdef0123456789abcdef"),
            "sk-abcdef",
        ),
        (
            "connections.plain.api_key_env",
            json!("ghp_abcdefghijklmnopqrstuvwxyz012345"),
            "ghp_",
        ),
        (
            "connections.plain.api_key_env",
            json!("AKIAIOSFODNN7EXAMPLE"),
            "AKIAIOSF",
        ),
        (
            "connections.plain.api_key_header",
            json!("Bearer abc.def"),
            "abc.def",
        ),
        (
            "connections.plain.session_header_names",
            json!(["xoxb-1234567890-abcdef"]),
            "xoxb",
        ),
        (
            "connections.new",
            json!({"base_url":"https://n.example/v1","api_key_env":"sk-abcdef0123456789abcdef"}),
            "sk-abcdef",
        ),
        (
            "shell.env.\"sk-abcdef0123456789abcdef\"",
            json!("x"),
            "sk-abcdef",
        ),
        (
            "connections.plain.api_key_env",
            json!("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"),
            "a1b2c3d4",
        ),
        (
            "connections.plain.headers.\"ghp_abcdefghijklmnopqrstuvwxyz012345\"",
            json!("x"),
            "ghp_",
        ),
        (
            "shell.env",
            json!({"ghp_abcdefghijklmnopqrstuvwxyz012345":"x"}),
            "ghp_",
        ),
    ] {
        let error = apply(&path, &hash, &[change(field, value)]).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{field}");
        let details = error.details.as_ref().unwrap();
        assert_eq!(details["reason"], "credential_value_refused", "{field}");
        assert_eq!(details["written"], false);
        let text = serde_json::to_string(&error).unwrap();
        assert!(!text.contains(secret), "{text}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL);
    }
    // Ordinary variable names pass.
    for name in [
        "OPENAI_API_KEY",
        "SHELLBOY_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "MY_SERVICE_PROD_2_API_KEY",
        "Authorization",
        "X-Api-Key",
        "ASIA_PACIFIC_API_KEY",
        "hf_token_for_my_proxy",
        "MyProxyService2ApiKeyName",
        "X-Stainless-Runtime-Version2",
    ] {
        assert!(!looks_like_credential(name), "{name}");
    }
    let applied = apply(
        &path,
        &hash,
        &[change(
            "connections.plain.api_key_env",
            json!("OPENAI_API_KEY"),
        )],
    )
    .unwrap();
    assert_eq!(applied.result["changes"][0]["value"], "OPENAI_API_KEY");
}

#[test]
fn request_debug_output_hides_change_values() {
    let debug = format!("{:?}", change("shell.env.A", json!("debug-secret-value")));
    assert!(debug.contains("shell.env.A"));
    assert!(!debug.contains("debug-secret-value"));
}

#[test]
fn request_validation_bounds_changes_before_the_owner_sees_them() {
    let hash = "a".repeat(64);
    let valid = |changes: Vec<ConfigChange>, hash: &str| {
        Command::ConfigSet(ConfigSetArgs {
            expected_hash: hash.into(),
            changes,
        })
        .validate()
    };
    assert!(valid(vec![change("ui.bell", json!(true))], &hash).is_ok());
    assert!(valid(vec![], &hash).is_err());
    assert!(valid(vec![change("ui.bell", json!(true))], "short").is_err());
    assert!(valid(vec![change("no spaces", json!(1))], &hash).is_err());
    assert!(valid(vec![change("ui.bell", json!(1)), removal("ui.bell")], &hash).is_err());
    assert!(valid(vec![change("a", json!("x".repeat(200 * 1024)))], &hash).is_err());
    let many = (0..65)
        .map(|n| change(&format!("k{n}"), json!(1)))
        .collect();
    assert!(valid(many, &hash).is_err());
    // Unknown argument names are refused on the wire.
    assert!(
        serde_json::from_value::<Command>(json!({"command":"config.get","args":{"x":1}})).is_err()
    );
}

async fn set(
    control: &std::sync::Arc<Control>,
    operation: &str,
    args: ConfigSetArgs,
) -> Result<Value, ApiError> {
    let response = control
        .request(Request::new(operation.to_owned(), Command::ConfigSet(args)))
        .await;
    if response.ok {
        Ok(response.result.unwrap())
    } else {
        Err(response.error.unwrap())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn through_the_owner_set_replays_and_get_shows_the_pending_difference() {
    crate::test_watchdog::watchdog("config through owner", async {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(root.path(), "http://127.0.0.1:1/v1").await;
        let control = Control::new(&owner, root.path().into());
        let path = root.path().join("config.toml");
        let original = std::fs::read_to_string(&path).unwrap();

        let got = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
        assert_eq!(got["differs"], false);
        assert_eq!(got["config_path"], json!(path));
        assert_eq!(got["loaded"]["values"]["default_model"], "fixture");
        let hash = got["file"]["content_hash"].as_str().unwrap().to_owned();

        let args = ConfigSetArgs {
            expected_hash: hash.clone(),
            changes: vec![
                change("default_model", json!("changed")),
                change("shell.env.OWNER_VAR", json!("owner-secret-value")),
            ],
        };
        let operation = format!("{}:config-1", control.store.instance_id);
        let first = set(&control, &operation, args.clone()).await.unwrap();
        assert_eq!(first["written"], true);
        assert!(first["changed_by"].is_null(), "no shell origin here");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            Config::parse(&text).unwrap().default_model.as_deref(),
            Some("changed")
        );

        // Only the shell setting remains pending; the default model is live.
        let got = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
        assert_eq!(got["differs"], true);
        assert_eq!(got["loaded"]["values"]["default_model"], "changed");
        assert_eq!(got["file"]["values"]["default_model"], "changed");
        assert_eq!(
            got["differing_fields"],
            json!(["shell.env.OWNER_VAR"])
        );
        assert_eq!(got["file"]["content_hash"], first["content_hash"]);
        assert!(!got.to_string().contains("owner-secret-value"));

        // Same ID and arguments: the saved outcome, with no second edit even after
        // the file moved on, which would otherwise be a hash conflict.
        std::fs::write(&path, &original).unwrap();
        let replayed = set(&control, &operation, args.clone()).await.unwrap();
        assert_eq!(replayed, first);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        // Same ID with other arguments is refused.
        let mut other = args.clone();
        other.changes.pop();
        let error = set(&control, &operation, other).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::OperationConflict);

        // A failed change is saved too, and a stale hash reaches the caller.
        let operation = format!("{}:config-2", control.store.instance_id);
        let moved = format!("{original}# Edited elsewhere\n");
        std::fs::write(&path, &moved).unwrap();
        let error = set(&control, &operation, args.clone()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::OperationConflict);
        assert_eq!(error.details.unwrap()["reason"], "config_changed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), moved);

        // The receipt records the outcome and holds no sensitive value.
        let saved: Vec<String> = control
            .store
            .database
            .call(|db| {
                let mut statement =
                    db.prepare("SELECT origin_json||request_hash||COALESCE(result_ref_json,'') FROM operation_receipt")?;
                let rows = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|row| row.contains("shell.env.OWNER_VAR")));
        for row in &saved {
            assert!(!row.contains("owner-secret-value"), "{row}");
        }
        // Reads create no receipt: both rows above are the two set operations.

        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_connection_a_thread_uses_stops_it_until_it_is_moved() {
    use shellboy::{
        app::DataOwner,
        protocol::{ThreadCreateArgs, ThreadSetArgs},
    };
    crate::test_watchdog::watchdog("removing a connection in use", async {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(
            &path,
            "default_connection = \"q\"\ndefault_model = \"m\"\n[connections.p]\nbase_url = \"http://127.0.0.1:1/v1\"\n[connections.q]\nbase_url = \"http://127.0.0.1:1/v1\"\n",
        )
        .unwrap();
        let open = || {
            DataOwner::start(Some(path.clone()), Some(root.path().join("data")), root.path().into())
        };
        let owner = open().await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let thread = request(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_id: None,
                connection: Some("p".into()),
                model: Some("m".into()),
                effort: None,
            }),
        )
        .await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let hash = request(&control, Command::ConfigGet(ConfigGetArgs {})).await["file"]
            ["content_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        // config.set doesn't consult the database: this is accepted.
        let operation = format!("{}:remove", control.store.instance_id);
        let args = ConfigSetArgs {
            expected_hash: hash,
            changes: vec![removal("connections.p")],
        };
        assert_eq!(set(&control, &operation, args).await.unwrap()["written"], true);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();

        let owner = open().await.unwrap();
        let control = Control::new(&owner, root.path().into());
        crate::runtime_features::send(&control, &thread, "hello").await;
        crate::runtime_features::settled(&control, &thread).await;
        let target = thread.clone();
        let turn: String = control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT state FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",
                    [target],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(turn, "recovery_wait");
        let args = |connection: Option<&str>| {
            Command::ThreadSet(Box::new(ThreadSetArgs {
                thread_id: Some(thread.clone()),
                connection: connection.map(str::to_owned),
                model: Some("m2".into()),
                ..ThreadSetArgs::default()
            }))
        };
        let response = control
            .request(Request::new(
                format!("{}:model-only", control.store.instance_id),
                args(None),
            ))
            .await;
        assert!(!response.ok, "the saved connection no longer exists");
        let moved = control
            .request(Request::new(
                format!("{}:moved", control.store.instance_id),
                args(Some("q")),
            ))
            .await;
        assert!(moved.ok, "{:?}", moved.error);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_sets_with_one_hash_leave_one_winner() {
    crate::test_watchdog::watchdog("config concurrent", async {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(root.path(), "http://127.0.0.1:1/v1").await;
        let control = Control::new(&owner, root.path().into());
        let got = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
        let hash = got["file"]["content_hash"].as_str().unwrap().to_owned();
        let mut tasks = Vec::new();
        for n in 0..4 {
            let control = control.clone();
            let args = ConfigSetArgs {
                expected_hash: hash.clone(),
                changes: vec![change("default_model", json!(format!("winner-{n}")))],
            };
            let operation = format!("{}:race-{n}", control.store.instance_id);
            tasks.push(tokio::spawn(async move {
                set(&control, &operation, args).await
            }));
        }
        let mut wins = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => wins += 1,
                Err(error) => assert_eq!(error.code, ErrorCode::OperationConflict),
            }
        }
        assert_eq!(wins, 1);
        control.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

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

#[test]
fn with_no_owner_nothing_changes_and_the_error_says_what_to_do() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config.toml");
    std::fs::write(&config, FULL).unwrap();
    let hash = "a".repeat(64);
    for args in [
        vec!["config", "get"],
        vec![
            "config",
            "set",
            "--expected-hash",
            &hash,
            "--set",
            "ui.bell=false",
        ],
    ] {
        let output = cli(root.path())
            .arg("--data-dir")
            .arg(&data)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        assert!(text.contains("app_not_running"), "{text}");
        assert!(text.contains("config check"), "{text}");
    }
    assert_eq!(std::fs::read_to_string(&config).unwrap(), FULL);
    assert!(!data.join("shellboy.db").exists());
}

#[test]
fn config_get_and_set_refuse_a_config_path_and_the_others_stay_standalone() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, FULL).unwrap();
    let output = cli(root.path())
        .args(["config", "get", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--config cannot select"));

    let selected = root.path().join("selected.toml");
    let output = cli(root.path())
        .args(["config", "init", "--config"])
        .arg(&selected)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(selected.exists());
    let output = cli(root.path())
        .args(["config", "check", "--operation-id", "x", "--config"])
        .arg(&selected)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("config check"));
}

#[tokio::test]
async fn the_cli_reads_and_changes_a_headless_owner_end_to_end() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    crate::test_watchdog::watchdog("config cli through serve", async {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let config = root.path().join("config.toml");
        std::fs::write(&config, FULL).unwrap();
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
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        stdout.read_line(&mut ready).await.unwrap();
        let run = |args: Vec<String>| {
            let mut command = cli(root.path());
            command.arg("--data-dir").arg(&data).args(args);
            async move {
                let output = tokio::process::Command::from(command)
                    .output()
                    .await
                    .unwrap();
                serde_json::from_slice::<Value>(&output.stdout).unwrap()
            }
        };
        let got = run(vec!["config".into(), "get".into()]).await;
        assert_eq!(got["ok"], true, "{got}");
        assert_no_secret(&got.to_string());
        let hash = got["result"]["file"]["content_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let changed = run(vec![
            "config".into(),
            "set".into(),
            "--expected-hash".into(),
            hash.clone(),
            "--set".into(),
            "default_model=model-z".into(),
            "--set".into(),
            "ui.max_fps=20".into(),
            "--unset".into(),
            "ui.language".into(),
        ])
        .await;
        assert_eq!(changed["ok"], true, "{changed}");
        assert_eq!(changed["result"]["restart_required"], true);
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("default_model = \"model-z\""));
        assert!(text.contains("max_fps = 20"));
        assert!(!text.contains("language ="));
        assert!(text.contains("# Top comment"));
        let stale = run(vec![
            "config".into(),
            "set".into(),
            "--expected-hash".into(),
            hash,
            "--set".into(),
            "ui.bell=false".into(),
        ])
        .await;
        assert_eq!(stale["ok"], false);
        assert_eq!(stale["error"]["code"], "operation_conflict");
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    })
    .await
    .unwrap();
}

#[test]
fn three_api_output_limit_is_saved_and_read_back_without_other_setting_changes() {
    for api in ["responses", "chat_completions", "anthropic_messages"] {
        let text = format!(
            "# preserve comment\n[connections.synthetic]\napi='{api}'\nbase_url='https://synthetic.invalid/v1'\nextra_body={{future_field={{nested='value'}}}}\n"
        );
        let (_temp, path, hash) = fixture(&text);
        let original = loaded(&path).connections["synthetic"].clone();
        apply(
            &path,
            &hash,
            &[change("connections.synthetic.max_tokens", json!(1234))],
        )
        .unwrap();
        let current = loaded(&path);
        let saved = &current.connections["synthetic"];
        assert_eq!(saved.max_tokens, Some(1234));
        assert_eq!(saved.api, original.api);
        assert_eq!(saved.base_url, original.base_url);
        assert_eq!(saved.extra_body, original.extra_body);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# preserve comment")
        );
        assert_eq!(
            read(&current).unwrap()["file"]["values"]["connections"]["synthetic"]["max_tokens"],
            1234
        );
        for invalid in [json!(0), json!(-1), json!("1234")] {
            let before = std::fs::read(&path).unwrap();
            let hash = hash_of(&path);
            assert!(
                apply(
                    &path,
                    &hash,
                    &[change("connections.synthetic.max_tokens", invalid)]
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let hash = hash_of(&path);
        apply(&path, &hash, &[removal("connections.synthetic.max_tokens")]).unwrap();
        assert_eq!(loaded(&path).connections["synthetic"].max_tokens, None);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_thread_get_resolves_live_connection_spill_with_raw_precedence() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(root.path(), "http://127.0.0.1:1/v1").await;
    let control = Control::new(&owner, root.path().into());
    let created = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_id: None,
        }),
    )
    .await;
    let thread = created["thread_id"].as_str().unwrap().to_owned();
    let get_thread = || {
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.clone()),
        })
    };
    let initial = request(&control, get_thread()).await;
    assert_eq!(initial["spill"], Value::Null);
    assert_eq!(initial["effective_spill"], true);

    async fn config_spill(control: &std::sync::Arc<Control>, op: &str, value: bool) {
        let current = request(control, Command::ConfigGet(ConfigGetArgs {})).await;
        let hash = current["file"]["content_hash"].as_str().unwrap().to_owned();
        let result = set(
            control,
            op,
            ConfigSetArgs {
                expected_hash: hash,
                changes: vec![change("connections.fixture.spill", json!(value))],
            },
        )
        .await
        .unwrap();
        assert_eq!(result["written"], true);
        assert_eq!(result["restart_required"], false);
        assert_eq!(result["applied_to_running_owner"], true);
    }
    config_spill(
        &control,
        &format!("{}:spill-connection-off", control.store.instance_id),
        false,
    )
    .await;
    let inherited_off = request(&control, get_thread()).await;
    assert_eq!(inherited_off["spill"], Value::Null);
    assert_eq!(inherited_off["effective_spill"], false);

    let off = request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            spill: Some("off".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(off["spill"], false);
    config_spill(
        &control,
        &format!("{}:spill-connection-on", control.store.instance_id),
        true,
    )
    .await;
    let thread_off_connection_on = request(&control, get_thread()).await;
    assert_eq!(thread_off_connection_on["spill"], false);
    assert_eq!(thread_off_connection_on["effective_spill"], false);

    let on = request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            spill: Some("on".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(on["spill"], true);
    config_spill(
        &control,
        &format!("{}:spill-connection-off-again", control.store.instance_id),
        false,
    )
    .await;
    let thread_on_connection_off = request(&control, get_thread()).await;
    assert_eq!(thread_on_connection_off["spill"], true);
    assert_eq!(thread_on_connection_off["effective_spill"], true);

    request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            spill: Some(String::new()),
            ..Default::default()
        })),
    )
    .await;
    let cleared = request(&control, get_thread()).await;
    assert_eq!(cleared["spill"], Value::Null);
    assert_eq!(cleared["effective_spill"], false);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
