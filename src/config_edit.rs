//! Reading and changing the configuration file for `config.get` and `config.set`.
//! Every file function here blocks and must run on a bounded file worker.
//!
//! `fields` is the one list of what may leave the owner: `Connection`, `Shell` and
//! `Config` are destructured without `..`, so a new field does not compile until it
//! is classified. Errors never quote a value from the file or the request.
use crate::{
    config::{Config, Connection, Shell, absolute},
    digest::sha256_hex,
    protocol::{ApiError, ConfigChange, ErrorCode},
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, InlineTable, Item, Key, Table, TableLike};

/// Larger files are refused: a settings response must fit one protocol frame.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Certified bound of a `config.set` result, checked before the file is replaced.
const MAX_RESULT_BYTES: usize = 512 * 1024;
const REDACTED: &str = "[redacted]";
/// Also matches a redacted `base_url` and a redacted name.
const REDACTED_MARK: &str = "[redacted";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Not meant to hold a secret; a value with a known credential prefix is still hidden.
    Plain,
    /// Any value may be a secret: only the name leaves the owner.
    Secret,
    /// Userinfo, query and fragment never leave; nor does a credential-like path segment.
    Url,
    /// A variable or header name; hidden when it reads as a credential instead.
    Name,
}
/// May hold a secret's real value, so it has no Debug or Serialize.
struct Leaf {
    path: Vec<String>,
    value: Value,
    class: Class,
}
#[derive(Default)]
struct Fields(BTreeMap<String, Leaf>);
impl Fields {
    fn add(&mut self, path: &[&str], value: Value, class: Class) {
        let path: Vec<String> = path.iter().map(|part| (*part).to_owned()).collect();
        self.0
            .insert(field_name(&path), Leaf { path, value, class });
    }
    fn plain(&mut self, path: &[&str], value: impl Serialize) {
        let value = serde_json::to_value(value).unwrap_or(Value::Null);
        self.add(path, value, Class::Plain);
    }
    fn section(&mut self, name: &str, section: impl Serialize) {
        let Ok(Value::Object(section)) = serde_json::to_value(section) else {
            return;
        };
        for (key, value) in section {
            self.add(&[name, &key], value, Class::Plain);
        }
    }
    /// Only names are ever shown. The value stays here so a change can be detected,
    /// and an empty map stays visible as an empty object.
    fn secret_map(&mut self, path: &[&str], entries: impl Iterator<Item = (String, Value)>) {
        let mut empty = true;
        for (name, value) in entries {
            empty = false;
            // A name can be a pasted secret too. The digest keeps it distinct and stable.
            let name = if looks_like_credential(&name) {
                format!("[redacted-name-{}]", &sha256_hex(&name)[..8])
            } else {
                name
            };
            let mut full = path.to_vec();
            full.push(&name);
            self.add(&full, value, Class::Secret);
        }
        if empty {
            self.add(path, json!({}), Class::Plain);
        }
    }
}
fn strings<'a>(
    map: &'a BTreeMap<String, String>,
) -> impl Iterator<Item = (String, Value)> + use<'a> {
    map.iter().map(|(name, value)| (name.clone(), json!(value)))
}
fn fields(config: &Config) -> BTreeMap<String, Leaf> {
    let Config {
        source: _,
        stored_keys: _,
        live_connections: _,
        data_dir,
        extra_instructions,
        default_connection,
        default_model,
        connections,
        embedding,
        shell,
        retention,
        limits,
        cache,
        io,
        workers,
        indexing,
        ui,
        logging,
        compaction,
    } = config;
    let mut out = Fields::default();
    out.plain(&["data_dir"], data_dir);
    out.plain(&["extra_instructions"], extra_instructions);
    out.plain(&["default_connection"], default_connection);
    out.plain(&["default_model"], default_model);
    if connections.is_empty() {
        out.plain(&["connections"], json!({}));
    }
    for (name, connection) in connections {
        connection_fields(&mut out, name, connection);
    }
    shell_fields(&mut out, shell);
    out.section("embedding", embedding);
    out.section("retention", retention);
    out.section("limits", limits);
    out.section("cache", cache);
    out.section("io", io);
    out.section("workers", workers);
    out.section("indexing", indexing);
    out.section("ui", ui);
    out.section("logging", logging);
    out.section("compaction", compaction);
    out.0
}
fn connection_fields(out: &mut Fields, name: &str, connection: &Connection) {
    let Connection {
        api,
        base_url,
        api_key_env,
        api_key_header,
        default_effort,
        context_role,
        max_tokens,
        headers,
        extra_body,
        extra_headers,
        ssl_verify,
        session_header,
        session_header_names,
        prompt_cache_key,
        prompt_cache_diagnostics,
        prompt_caching,
        response_header_timeout_ms,
        stream_idle_timeout_ms,
        request_timeout_ms,
        response_max_retries,
        response_retry_max_delay_ms,
        response_retry_partial,
        auto_trim_on_disconnect,
        auto_at_tokens,
        auto_compact_target_percent,
        summary_max_chars,
        spill,
        credentials_file: _,
        stored_keys: _,
    } = connection;
    let at = |field| ["connections", name, field];
    out.plain(&at("api"), api);
    out.add(&at("base_url"), json!(base_url), Class::Url);
    out.add(&at("api_key_env"), json!(api_key_env), Class::Name);
    out.add(&at("api_key_header"), json!(api_key_header), Class::Name);
    out.plain(&at("default_effort"), default_effort);
    out.plain(&at("context_role"), context_role);
    out.plain(&at("max_tokens"), max_tokens);
    out.secret_map(&at("headers"), strings(headers));
    out.secret_map(
        &at("extra_body"),
        extra_body.iter().map(|(k, v)| (k.clone(), v.clone())),
    );
    out.secret_map(&at("extra_headers"), strings(extra_headers));
    out.plain(&at("ssl_verify"), ssl_verify);
    out.plain(&at("session_header"), session_header);
    out.plain(&at("session_header_names"), session_header_names);
    out.plain(&at("prompt_cache_key"), prompt_cache_key);
    out.plain(&at("prompt_cache_diagnostics"), prompt_cache_diagnostics);
    out.plain(&at("prompt_caching"), prompt_caching);
    out.plain(
        &at("response_header_timeout_ms"),
        response_header_timeout_ms,
    );
    out.plain(&at("stream_idle_timeout_ms"), stream_idle_timeout_ms);
    out.plain(&at("request_timeout_ms"), request_timeout_ms);
    out.plain(&at("response_max_retries"), response_max_retries);
    out.plain(
        &at("response_retry_max_delay_ms"),
        response_retry_max_delay_ms,
    );
    out.plain(&at("response_retry_partial"), response_retry_partial);
    out.plain(&at("auto_trim_on_disconnect"), auto_trim_on_disconnect);
    out.plain(&at("auto_at_tokens"), auto_at_tokens);
    out.plain(
        &at("auto_compact_target_percent"),
        auto_compact_target_percent,
    );
    out.plain(&at("summary_max_chars"), summary_max_chars);
    out.plain(&at("spill"), spill);
}
fn shell_fields(out: &mut Fields, shell: &Shell) {
    let Shell {
        describe_calls,
        kind,
        executable,
        env,
    } = shell;
    out.plain(&["shell", "describe_calls"], describe_calls);
    out.plain(&["shell", "kind"], kind);
    out.plain(&["shell", "executable"], executable);
    out.secret_map(&["shell", "env"], strings(env));
}

/// Canonical TOML dotted key; quoted only where a part requires it.
fn field_name(path: &[String]) -> String {
    path.iter()
        .map(|part| Key::new(part.as_str()).display_repr().into_owned())
        .collect::<Vec<_>>()
        .join(".")
}
fn has_credential_prefix(value: &str) -> bool {
    const PREFIXES: [&str; 18] = [
        "sk-",
        "sk_",
        "pk_",
        "rk_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xapp-",
        "hf_",
        "ya29.",
        "eyJ",
    ];
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let bytes = value.as_bytes();
    let aws = bytes.len() == 20
        && (value.starts_with("AKIA") || value.starts_with("ASIA"))
        && bytes[4..]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let google = bytes.len() == 39 && value.starts_with("AIza");
    // Generated keys carry digits; names like `hf_token_for_my_proxy` usually don't.
    let prefixed = value.len() >= 16
        && bytes.iter().any(u8::is_ascii_digit)
        && PREFIXES.iter().any(|prefix| value.starts_with(prefix));
    lower.starts_with("bearer ") || lower.starts_with("basic ") || aws || google || prefixed
}
/// A protective heuristic for name fields: known key shapes and long opaque tokens.
/// It is never proof of either, so a refusal says how to proceed by hand.
pub fn looks_like_credential(value: &str) -> bool {
    let value = value.trim();
    has_credential_prefix(value)
        || (value.len() >= 32
            && value.bytes().any(|b| b.is_ascii_digit())
            && value.bytes().all(|b| b.is_ascii_alphanumeric()))
}
fn redact_url(value: &str) -> (Value, bool) {
    let Ok(mut url) = url::Url::parse(value) else {
        return (json!(REDACTED), true);
    };
    let mut redacted = !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let segments: Vec<String> = url
        .path()
        .split('/')
        .map(|segment| {
            if looks_like_credential(segment) {
                redacted = true;
                REDACTED.to_owned()
            } else {
                segment.to_owned()
            }
        })
        .collect();
    if !redacted {
        // Keep the configured spelling when nothing had to be hidden.
        return (json!(value), false);
    }
    url.set_path(&segments.join("/"));
    (json!(url.as_str()), true)
}
fn scrub_plain(value: &Value, redacted: &mut bool) -> Value {
    match value {
        Value::String(text) if has_credential_prefix(text) => {
            *redacted = true;
            json!(REDACTED)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| scrub_plain(v, redacted)).collect())
        }
        other => other.clone(),
    }
}
/// What may be returned for a leaf, and whether anything was hidden.
fn shown(leaf: &Leaf) -> (Value, bool) {
    match leaf.class {
        Class::Plain => {
            let mut redacted = false;
            let value = scrub_plain(&leaf.value, &mut redacted);
            (value, redacted)
        }
        Class::Secret => (json!(REDACTED), true),
        Class::Url => leaf
            .value
            .as_str()
            .map_or((json!(REDACTED), true), redact_url),
        Class::Name => match leaf.value.as_str() {
            Some(name) if looks_like_credential(name) => (json!(REDACTED), true),
            _ => (leaf.value.clone(), false),
        },
    }
}
/// Nested values with every sensitive value replaced, and the paths that were.
fn view(config: &Config) -> (Value, Vec<String>) {
    let mut root = Map::new();
    let mut hidden = Vec::new();
    for (name, leaf) in fields(config) {
        let (value, redacted) = shown(&leaf);
        if redacted {
            hidden.push(name);
        }
        insert_nested(&mut root, &leaf.path, value);
    }
    (Value::Object(root), hidden)
}
fn insert_nested(root: &mut Map<String, Value>, path: &[String], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current = root;
    for part in parents {
        current = current
            .entry(part.clone())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("parents of a field are objects");
    }
    current.insert(last.clone(), value);
}

pub(crate) struct FileState {
    pub(crate) target: PathBuf,
    pub(crate) present: bool,
    pub(crate) bytes: Vec<u8>,
    pub(crate) hash: String,
    pub(crate) read_only: bool,
    /// Give the new file the replaced file's mode (the configuration file keeps its
    /// permissions). A file holding a secret sets this false, so the temporary file stays
    /// `0600` from creation and is never readable by others, not even before the replace.
    pub(crate) inherit_mode: bool,
}
fn file_error(message: &str, path: &Path) -> ApiError {
    ApiError::new(ErrorCode::IoFailed, message).details(json!({"config_path":path,"written":false}))
}
/// A symlink resolves to its file, so a linked dotfile stays linked after a write.
pub(crate) fn read_state(path: &Path) -> Result<FileState, ApiError> {
    let target = match std::fs::canonicalize(path) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(file_error(
                    "The configuration path is a broken symbolic link",
                    path,
                ));
            }
            return Ok(FileState {
                target: path.to_owned(),
                present: false,
                bytes: Vec::new(),
                hash: sha256_hex([]),
                read_only: false,
                inherit_mode: true,
            });
        }
        Err(_) => return Err(file_error("Cannot resolve the configuration path", path)),
    };
    let file = std::fs::File::open(&target)
        .map_err(|_| file_error("Cannot open the configuration file", path))?;
    let read_only = file
        .metadata()
        .map_err(|_| file_error("Cannot inspect the configuration file", path))?
        .permissions()
        .readonly();
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| file_error("Cannot read the configuration file", path))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            format!("The configuration file exceeds {MAX_FILE_BYTES} bytes"),
        )
        .details(json!({"config_path":path,"written":false})));
    }
    Ok(FileState {
        target,
        present: true,
        hash: sha256_hex(&bytes),
        bytes,
        read_only,
        inherit_mode: true,
    })
}
/// Same strict rules as loading and `config check`, with the paths loading resolves.
pub(crate) fn parse_file(bytes: &[u8], path: &Path) -> Result<Config, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "The file is not valid UTF-8".to_owned())?;
    let mut config = Config::parse(text).map_err(|error| error.to_string())?;
    if let (Some(executable), Some(parent)) = (&mut config.shell.executable, path.parent()) {
        *executable = absolute(executable, parent);
    }
    Ok(config)
}

/// The `config.get` result. Blocking; `loaded` is what the owner parsed at startup.
pub fn read(loaded: &Config) -> Result<Value, ApiError> {
    let loaded = &loaded.current();
    let source = loaded.source.as_ref().ok_or_else(no_source)?;
    let state = read_state(&source.path)?;
    let parsed = parse_file(&state.bytes, &source.path);
    let (loaded_values, loaded_hidden) = view(loaded);
    let credentials_path = crate::credential_store::path_beside(&source.path);
    let stored = crate::credential_store::stored_names(&credentials_path);
    let stored_names = stored.as_ref().ok();
    let key_sources = |config: &Config| {
        let mut sources = Map::new();
        for (name, connection) in &config.connections {
            let present = stored_names.is_some_and(|names| names.contains(name));
            let source = connection.key_source(|var| std::env::var(var).ok(), present);
            sources.insert(name.clone(), json!(source));
        }
        Value::Object(sources)
    };
    let mut environment = Map::new();
    for connection in loaded.connections.values() {
        if let Some(name) = connection
            .api_key_env
            .as_deref()
            .filter(|name| !looks_like_credential(name))
        {
            environment.insert(
                name.to_owned(),
                json!(crate::config_setup::credential_set(name)),
            );
        }
    }
    let differing_fields = parsed.as_ref().ok().map(|file| {
        let before = fields(loaded);
        let after = fields(file);
        changed_names(&before, &after)
            .into_iter()
            // The loaded value is the resolved directory, which flags and the
            // environment can override, so it is not comparable with the file's.
            .filter(|name| name != "data_dir")
            .collect::<Vec<_>>()
    });
    let loaded_hash = source.content_hash.as_deref();
    let file_hash = state.present.then_some(state.hash.as_str());
    let (file_values, file_hidden, error) = match &parsed {
        Ok(config) => {
            let (values, hidden) = view(config);
            (values, hidden, Value::Null)
        }
        Err(message) => (Value::Null, Vec::new(), json!(message)),
    };
    let file_sources = parsed.as_ref().map_or(Value::Null, key_sources);
    Ok(json!({
        "config_path": source.path,
        "loaded": {
            "present": source.present,
            "content_hash": loaded_hash,
            "values": loaded_values,
            "redacted_fields": loaded_hidden,
            // Presence in the owner's own environment; never the value.
            "credential_env_set": environment,
            // Where each connection's key comes from now: "env", "stored" or "none".
            "credential_source": key_sources(loaded),
        },
        "file": {
            "present": state.present,
            "content_hash": state.hash,
            "valid": parsed.is_ok(),
            "error": error,
            "values": file_values,
            "redacted_fields": file_hidden,
            "credential_source": file_sources,
        },
        // The stored-key file beside the configuration; its values are never read out.
        "credentials_file": {
            "path": credentials_path,
            "present": credentials_path.exists(),
            "valid": stored.is_ok(),
            "error": stored.as_ref().err().map(ToString::to_string),
        },
        "differs": loaded_hash != file_hash,
        "differing_fields": differing_fields,
        "restart": {"all_fields_require_restart": false},
    }))
}
/// The connection as the configuration file holds it now, and whether the file parses.
/// Blocking.
pub(crate) fn saved_connection(path: &Path, name: &str) -> Result<Option<Connection>, ApiError> {
    let state = read_state(path)?;
    let config = parse_file(&state.bytes, path).map_err(|message| {
        ApiError::new(ErrorCode::ConfigInvalid, message).details(json!({"config_path":path}))
    })?;
    Ok(config.connections.get(name).cloned())
}
fn no_source() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidState,
        "This owner was started without a configuration path",
    )
}
fn changed_names(before: &BTreeMap<String, Leaf>, after: &BTreeMap<String, Leaf>) -> Vec<String> {
    let mut names: Vec<String> = before.keys().chain(after.keys()).cloned().collect();
    names.sort();
    names.dedup();
    names.retain(|name| {
        let old = before.get(name).map_or(&Value::Null, |leaf| &leaf.value);
        let new = after.get(name).map_or(&Value::Null, |leaf| &leaf.value);
        let nothing = |value: &Value| {
            value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty)
        };
        old != new && !(nothing(old) && nothing(new))
    });
    names
}
/// What a finished `config.set` reports. `fields` holds names only, for the record.
#[derive(Debug)]
pub struct Applied {
    pub result: Value,
    pub written: bool,
    pub fields: Vec<String>,
}
/// Blocking. Reads, compares the hash, validates the whole result, then replaces the file.
pub fn apply(
    path: &Path,
    expected_hash: &str,
    changes: &[ConfigChange],
) -> Result<Applied, ApiError> {
    apply_with(path, expected_hash, changes, |_| Ok(()))
}
/// `before_replace` runs with the finished temporary file; tests fail or race it here.
pub fn apply_with(
    path: &Path,
    expected_hash: &str,
    changes: &[ConfigChange],
    before_replace: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<Applied, ApiError> {
    apply_with_replace(path, expected_hash, changes, before_replace, replace_file)
}
/// `replace` takes (temporary, target, target exists); tests return OS failures from it.
pub fn apply_with_replace(
    path: &Path,
    expected_hash: &str,
    changes: &[ConfigChange],
    before_replace: impl FnOnce(&Path) -> std::io::Result<()>,
    replace: impl FnOnce(&Path, &Path, bool) -> std::io::Result<()>,
) -> Result<Applied, ApiError> {
    apply_runtime(path, expected_hash, changes, before_replace, replace, None)
}

pub(crate) fn apply_live(
    path: &Path,
    expected_hash: &str,
    changes: &[ConfigChange],
    loaded: &Config,
    clients: &crate::model::http::HttpClients,
) -> Result<Applied, ApiError> {
    apply_runtime(
        path,
        expected_hash,
        changes,
        |_| Ok(()),
        replace_file,
        Some((loaded, clients)),
    )
}

fn apply_runtime(
    path: &Path,
    expected_hash: &str,
    changes: &[ConfigChange],
    before_replace: impl FnOnce(&Path) -> std::io::Result<()>,
    replace: impl FnOnce(&Path, &Path, bool) -> std::io::Result<()>,
    runtime: Option<(&Config, &crate::model::http::HttpClients)>,
) -> Result<Applied, ApiError> {
    let state = read_state(path)?;
    if state.hash != expected_hash {
        return Err(conflict(expected_hash, &state.hash));
    }
    let text = std::str::from_utf8(&state.bytes).map_err(|_| {
        ApiError::new(
            ErrorCode::ConfigInvalid,
            "The configuration file is not valid UTF-8; fix it by hand first",
        )
        .details(json!({"written":false}))
    })?;
    let edited = edit_text(text, changes)?;
    // A bigger file could no longer be read back here, which would lock this out.
    if edited.len() as u64 > MAX_FILE_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            format!(
                "The changed configuration would exceed {MAX_FILE_BYTES} bytes; nothing was written"
            ),
        )
        .details(json!({"written":false})));
    }
    let mut after = parse_file(edited.as_bytes(), path).map_err(|message| {
        ApiError::new(ErrorCode::ConfigInvalid, message)
            .hint("Nothing was written. Correct the value; the whole file must pass the same rules as config check")
            .details(json!({"written":false}))
    })?;
    if let Some((loaded, _)) = runtime {
        let current = loaded.current();
        for name in [
            current.default_connection.as_deref(),
            loaded.embedding.connection.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !after.connections.contains_key(name) {
                return Err(ApiError::new(
                    ErrorCode::ConfigInvalid,
                    "Cannot remove the default or embedding connection",
                )
                .details(json!({"written":false})));
            }
        }
        for connection in after.connections.values_mut() {
            connection.credentials_file = Some(crate::credential_store::path_beside(path));
            connection.stored_keys = loaded.stored_keys.clone();
        }
    }
    let before = parse_file(&state.bytes, path).ok();
    let written = edited != text;
    let after_fields = fields(&after);
    let before_fields = before.as_ref().map(fields);
    let empty = BTreeMap::new();
    let names = changed_names(before_fields.as_ref().unwrap_or(&empty), &after_fields);
    let entries: Vec<Value> = names
        .iter()
        .map(|name| {
            change_entry(
                name,
                before_fields.as_ref().map(|all| all.get(name)),
                after_fields.get(name),
            )
        })
        .collect();
    let new_hash = sha256_hex(&edited);
    let mut result = json!({
        "config_path": path,
        "written": written,
        "previous_hash": state.hash,
        "content_hash": new_hash,
        "changes": entries,
        "restart_required": written,
        "applied_to_running_owner": false,
    });
    if let Some((loaded, _)) = runtime {
        let entries = result["changes"].as_array_mut().expect("change entries");
        for entry in entries.iter_mut() {
            let field = entry["field"].as_str().expect("field name");
            let live = live_field(field, loaded.embedding.connection.as_deref());
            entry["restart_required"] = json!(!live);
            entry["applied_to_running_owner"] = json!(live);
        }
        let restart = entries.iter().any(|e| e["restart_required"] == true);
        let live = entries
            .iter()
            .any(|e| e["applied_to_running_owner"] == true);
        result["restart_required"] = json!(restart);
        result["applied_to_running_owner"] = json!(live);
    }
    // Checked before the replace: a saved outcome must never hide a finished write.
    if serde_json::to_vec(&result).map_or(usize::MAX, |bytes| bytes.len()) > MAX_RESULT_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "This change would report more than the result bound; nothing was written. Change fewer fields at once",
        )
        .details(json!({"written":false})));
    }
    if written && state.read_only {
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "The configuration file is read-only; nothing was written",
        )
        .details(json!({"config_path":path,"written":false})));
    }
    if written
        && let Some(warning) =
            replace_atomically(path, &state, edited.as_bytes(), before_replace, replace)?
    {
        result["warning"] = json!(warning);
    }
    if let Some((loaded, clients)) = runtime {
        clients.adopt(&after, loaded.embedding.connection.as_deref());
    }
    Ok(Applied {
        result,
        written,
        fields: names,
    })
}
fn live_field(name: &str, embedding: Option<&str>) -> bool {
    if matches!(
        name,
        "default_connection" | "default_model" | "extra_instructions"
    ) {
        return true;
    }
    if name == "connections" {
        return true;
    }
    let Some(rest) = name.strip_prefix("connections.") else {
        return false;
    };
    !embedding.is_some_and(|name| {
        let name = Key::new(name).display_repr().into_owned();
        rest == name
            || rest
                .strip_prefix(&name)
                .is_some_and(|suffix| suffix.starts_with('.'))
    })
}

/// Compare every classified definition field, including secrets, without emitting it.
pub(crate) fn same_connection(a: &Connection, b: &Connection) -> bool {
    // Spill is consumed from the config snapshot by Control; changing it does
    // not alter provider request construction or the HTTP client.
    let mut left_connection = a.clone();
    let mut right_connection = b.clone();
    left_connection.spill = None;
    right_connection.spill = None;
    let mut left = Fields::default();
    let mut right = Fields::default();
    connection_fields(&mut left, "connection", &left_connection);
    connection_fields(&mut right, "connection", &right_connection);
    changed_names(&left.0, &right.0).is_empty()
}

fn change_entry(name: &str, before: Option<Option<&Leaf>>, after: Option<&Leaf>) -> Value {
    let hidden = |leaf: Option<&Leaf>| leaf.is_some_and(|leaf| shown(leaf).1);
    let sensitive = before.is_some_and(hidden) || hidden(after);
    let mut entry = json!({
        "field": name,
        "sensitive": sensitive,
        "restart_required": true,
    });
    if !sensitive {
        // Effective values, so removing a key shows the default that now applies.
        if let Some(before) = before {
            entry["previous"] = before.map_or(Value::Null, |leaf| shown(leaf).0);
        }
        entry["value"] = after.map_or(Value::Null, |leaf| shown(leaf).0);
    }
    entry
}
fn conflict(expected: &str, current: &str) -> ApiError {
    ApiError::new(
        ErrorCode::OperationConflict,
        "The configuration file changed since it was read; nothing was written",
    )
    .hint("Read shellboy config get again, review the current values, and repeat the change with its content_hash and a new operation ID")
    .details(json!({"reason":"config_changed","expected_hash":expected,"current_hash":current,"written":false}))
}

fn change_error(index: usize, message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
        .details(json!({"change_index":index,"written":false}))
}
/// `field` must be safe to return: a name in the path is reported by its parent.
fn refused_name(index: usize, field: &str) -> ApiError {
    ApiError::new(
        ErrorCode::InvalidArgument,
        format!("{field} takes a name, and this value looks like a credential; it was not written and is not shown"),
    )
    .hint("Put the credential in the environment and set the variable's name here. If this really is a name, edit the file by hand and run shellboy config check")
    .details(json!({"reason":"credential_value_refused","change_index":index,"field":field,"written":false}))
}
fn has_placeholder(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains(REDACTED_MARK),
        Value::Array(items) => items.iter().any(has_placeholder),
        Value::Object(entries) => entries
            .iter()
            .any(|(key, item)| key.contains(REDACTED_MARK) || has_placeholder(item)),
        _ => false,
    }
}
fn has_credential_key(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(has_credential_key),
        Value::Object(entries) => entries
            .iter()
            .any(|(key, item)| looks_like_credential(key) || has_credential_key(item)),
        _ => false,
    }
}
fn check_names(path: &[String], value: &Value, index: usize) -> Result<(), ApiError> {
    if let Some(at) = path.iter().position(|part| looks_like_credential(part)) {
        let parent = if at == 0 {
            "The field path".to_owned()
        } else {
            field_name(&path[..at])
        };
        return Err(refused_name(index, &parent));
    }
    let name = field_name(path);
    if has_credential_key(value) {
        return Err(refused_name(index, &name));
    }
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    let refuse = |text: &str| {
        if looks_like_credential(text) {
            Err(refused_name(index, &name))
        } else {
            Ok(())
        }
    };
    match parts.as_slice() {
        ["connections", _, "api_key_env" | "api_key_header"] => {
            refuse(value.as_str().unwrap_or_default())
        }
        ["connections", _, "session_header_names"] => {
            for item in value.as_array().into_iter().flatten() {
                refuse(item.as_str().unwrap_or_default())?;
            }
            Ok(())
        }
        ["connections", _] => {
            for (key, inner) in value.as_object().into_iter().flatten() {
                let mut inner_path = path.to_vec();
                inner_path.push(key.clone());
                check_names(&inner_path, inner, index)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
fn toml_value(value: &Value, index: usize) -> Result<toml_edit::Value, ApiError> {
    Ok(match value {
        Value::Null => {
            return Err(change_error(
                index,
                "TOML has no null; remove the key instead",
            ));
        }
        Value::Bool(flag) => (*flag).into(),
        Value::String(text) => text.as_str().into(),
        Value::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(integer), _) => integer.into(),
            (None, Some(float)) if number.is_f64() => float.into(),
            _ => {
                return Err(change_error(
                    index,
                    "The number is outside TOML's integer range",
                ));
            }
        },
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(toml_value(item, index)?);
            }
            array.into()
        }
        Value::Object(entries) => {
            let mut table = InlineTable::new();
            for (key, item) in entries {
                table.insert(key, toml_value(item, index)?);
            }
            table.into()
        }
    })
}
fn is_connection(path: &[String]) -> bool {
    path.len() == 2 && path[0] == "connections"
}
/// The table holding `path`'s last key; `create` adds missing tables on the way.
fn parent_table<'a>(
    doc: &'a mut DocumentMut,
    path: &[String],
    create: bool,
    index: usize,
) -> Result<Option<&'a mut dyn TableLike>, ApiError> {
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for part in &path[..path.len() - 1] {
        if !current.contains_key(part) {
            if !create {
                return Ok(None);
            }
            let mut table = Table::new();
            table.set_implicit(true);
            current.insert(part, Item::Table(table));
        }
        current = current
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                change_error(index, "A parent of this field is not a table in the file")
            })?;
    }
    Ok(Some(current))
}
fn set_field(
    doc: &mut DocumentMut,
    path: &[String],
    value: &Value,
    index: usize,
) -> Result<(), ApiError> {
    let last = path.last().expect("parsed fields have a part");
    let header = (path.len() == 1 && !doc.as_table().contains_key(last))
        .then(|| file_header(doc))
        .flatten();
    let parent = parent_table(doc, path, true, index)?.expect("created");
    match parent.get_mut(last) {
        Some(Item::Value(existing)) => {
            let mut new = toml_value(value, index)?;
            *new.decor_mut() = existing.decor().clone();
            *existing = new;
        }
        Some(_) => {
            return Err(change_error(
                index,
                "This field is a table in the file; set its fields one by one",
            ));
        }
        None if is_connection(path) && value.is_object() => {
            let mut table = Table::new();
            for (key, item) in value.as_object().into_iter().flatten() {
                table.insert(key, Item::Value(toml_value(item, index)?));
            }
            parent.insert(last, Item::Table(table));
        }
        None => {
            parent.insert(last, Item::Value(toml_value(value, index)?));
            if let (Some(header), Some(mut key)) = (header, parent.key_mut(last)) {
                key.leaf_decor_mut().set_prefix(header);
            }
        }
    }
    Ok(())
}
/// A root key prints before every table. With no root key yet, the file's opening
/// comment (up to a blank line) stays above it instead of ending up below it.
fn file_header(doc: &mut DocumentMut) -> Option<String> {
    let has_root_key = doc
        .as_table()
        .iter()
        .any(|(_, item)| item.is_value() || item.as_table().is_some_and(Table::is_dotted));
    if has_root_key {
        return None;
    }
    let mut first = None;
    next_position(doc.as_table(), None, &mut first);
    let Some(table) = first.and_then(|at| table_at(doc.as_table_mut(), at)) else {
        let trailing = doc.trailing().as_str().unwrap_or_default().to_owned();
        if !trailing.contains('#') {
            return None;
        }
        doc.set_trailing("");
        return Some(trailing);
    };
    let prefix = table.decor().prefix()?.as_str()?.to_owned();
    let (head, rest) = prefix.rsplit_once("\n\n")?;
    let rest = format!("\n{rest}");
    table.decor_mut().set_prefix(rest);
    Some(format!("{head}\n\n"))
}
fn remove_field(doc: &mut DocumentMut, path: &[String], index: usize) -> Result<(), ApiError> {
    let last = path.last().expect("parsed fields have a part");
    let position = owning_position(doc, path);
    let Some(parent) = parent_table(doc, path, false, index)? else {
        return Ok(());
    };
    match parent.get(last) {
        None => return Ok(()),
        Some(Item::Value(_)) => {}
        Some(_) if is_connection(path) => {
            parent.remove(last);
            return Ok(());
        }
        Some(_) => {
            return Err(change_error(
                index,
                "This field is a table in the file; remove its fields one by one",
            ));
        }
    }
    let keys: Vec<String> = parent.iter().map(|(key, _)| key.to_owned()).collect();
    let next = keys
        .iter()
        .skip_while(|key| *key != last)
        .skip(1)
        .find(|key| parent.get(key).is_some_and(Item::is_value))
        .cloned();
    let comment = parent
        .key(last)
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .filter(|prefix| prefix.contains('#'))
        .map(str::to_owned);
    parent.remove(last);
    // A comment above the key often describes more than this key, so it stays.
    let Some(comment) = comment else {
        return Ok(());
    };
    if let Some(next) = next {
        let Some(mut key) = parent.key_mut(&next) else {
            return Ok(());
        };
        let old = key
            .leaf_decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default()
            .to_owned();
        key.leaf_decor_mut().set_prefix(comment + &old);
        return Ok(());
    }
    let mut best = None;
    next_position(doc.as_table(), position, &mut best);
    if let Some(table) = best.and_then(|at| table_at(doc.as_table_mut(), at)) {
        let old = table
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default()
            .to_owned();
        table.decor_mut().set_prefix(comment + &old);
    } else {
        let old = doc.trailing().as_str().unwrap_or_default().to_owned();
        doc.set_trailing(comment + &old);
    }
    Ok(())
}
fn owning_position(doc: &DocumentMut, path: &[String]) -> Option<isize> {
    let mut table = Some(doc.as_table());
    let mut position = None;
    for part in &path[..path.len() - 1] {
        table = table
            .and_then(|table| table.get(part))
            .and_then(Item::as_table);
        if let Some(found) = table
            .filter(|table| !table.is_dotted())
            .and_then(Table::position)
        {
            position = Some(found);
        }
    }
    position
}
fn next_position(table: &Table, after: Option<isize>, best: &mut Option<isize>) {
    for (_, item) in table.iter() {
        let Some(table) = item.as_table() else {
            continue;
        };
        if !table.is_implicit()
            && !table.is_dotted()
            && let Some(position) = table.position()
            && after.is_none_or(|after| position > after)
            && best.is_none_or(|best| position < best)
        {
            *best = Some(position);
        }
        next_position(table, after, best);
    }
}
fn table_at(table: &mut Table, position: isize) -> Option<&mut Table> {
    for (_, item) in table.iter_mut() {
        let Some(table) = item.as_table_mut() else {
            continue;
        };
        if !table.is_implicit() && !table.is_dotted() && table.position() == Some(position) {
            return Some(table);
        }
        if let Some(found) = table_at(table, position) {
            return Some(found);
        }
    }
    None
}
/// Comments, order and untouched formatting survive. Pure; no file access.
pub fn edit_text(text: &str, changes: &[ConfigChange]) -> Result<String, ApiError> {
    let mut doc: DocumentMut = text.parse().map_err(|_| {
        ApiError::new(
            ErrorCode::ConfigInvalid,
            "The configuration file has a TOML syntax error, so it cannot be edited; fix it by hand first",
        )
        .details(json!({"written":false}))
    })?;
    for (index, change) in changes.iter().enumerate() {
        let path: Vec<String> = Key::parse(&change.field)
            .map_err(|_| change_error(index, "field must be a TOML dotted key"))?
            .iter()
            .map(|key| key.get().to_owned())
            .collect();
        if path.first().is_some_and(|part| part == "data_dir") {
            return Err(change_error(
                index,
                "data_dir cannot be changed here: after a restart the owner would open another, possibly empty, data directory. Edit the file by hand and move the data yourself",
            ));
        }
        match &change.value {
            Some(value) => {
                // A form posting back what config.get showed would destroy the secret.
                if has_placeholder(value) || path.iter().any(|part| part.contains(REDACTED_MARK)) {
                    return Err(ApiError::new(
                        ErrorCode::InvalidArgument,
                        "This value is the placeholder config.get shows for a hidden value; writing it would replace the real one, so nothing was written",
                    )
                    .hint("Send only the fields you are changing, with their real values")
                    .details(json!({"reason":"redacted_placeholder_refused","change_index":index,"written":false})));
                }
                check_names(&path, value, index)?;
                set_field(&mut doc, &path, value, index)?;
            }
            None => remove_field(&mut doc, &path, index)?,
        }
    }
    let edited = doc.to_string();
    // The parser normalizes line endings; a consistently CRLF file stays CRLF.
    let crlf = text.contains("\r\n") && text.matches('\n').count() == text.matches("\r\n").count();
    Ok(if crlf {
        edited.replace('\n', "\r\n")
    } else {
        edited
    })
}

/// `ReplaceFileW` without a backup name: the replaced file is gone and the
/// replacement remains under its own name.
const ERROR_UNABLE_TO_MOVE_REPLACEMENT: i32 = 1176;
/// The replaced file survives under a name Windows chose; the replacement remains.
const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: i32 = 1177;
/// Returns a warning when the file is in place but a replace was interrupted.
pub(crate) fn replace_atomically(
    path: &Path,
    state: &FileState,
    bytes: &[u8],
    before_replace: impl FnOnce(&Path) -> std::io::Result<()>,
    replace: impl FnOnce(&Path, &Path, bool) -> std::io::Result<()>,
) -> Result<Option<&'static str>, ApiError> {
    let target = &state.target;
    let parent = target
        .parent()
        .ok_or_else(|| file_error("The configuration path has no parent directory", path))?;
    if !state.present {
        std::fs::create_dir_all(parent)
            .map_err(|_| file_error("Cannot create the configuration directory", path))?;
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let prepared = write_temporary(&temporary, state, bytes)
        .and_then(|()| before_replace(&temporary))
        .map_err(|_| {
            file_error(
                "Cannot write the new configuration; the file is unchanged",
                path,
            )
        })
        .and_then(|()| {
            // The last look shrinks the race with another editor to this one step.
            let again = read_state(path)?;
            if again.hash != state.hash {
                return Err(conflict(&state.hash, &again.hash));
            }
            Ok(())
        });
    if let Err(error) = prepared {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    let warning = match replace(&temporary, target, state.present) {
        Ok(()) => None,
        Err(error) => match error.raw_os_error() {
            Some(
                code @ (ERROR_UNABLE_TO_MOVE_REPLACEMENT | ERROR_UNABLE_TO_MOVE_REPLACEMENT_2),
            ) => {
                // The temporary file may be the only copy: never delete it here.
                Some(finish_interrupted_replace(path, &temporary, target, code)?)
            }
            _ => {
                // Every other failure leaves both files under their own names.
                let _ = std::fs::remove_file(&temporary);
                return Err(file_error(
                    "Cannot replace the configuration file; it is unchanged",
                    path,
                ));
            }
        },
    };
    #[cfg(unix)]
    if let Ok(directory) = std::fs::File::open(parent) {
        // The replace has happened; this only hardens it against power loss.
        let _ = directory.sync_all();
    }
    Ok(warning)
}
fn finish_interrupted_replace(
    path: &Path,
    temporary: &Path,
    target: &Path,
    code: i32,
) -> Result<&'static str, ApiError> {
    let previous_survives = code == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2;
    if std::fs::rename(temporary, target).is_ok() {
        return Ok(if previous_survives {
            "The replace was interrupted and finished with a plain move. The new file is in place; the previous version remains in the same directory under a name Windows chose, and the file's permissions may differ"
        } else {
            "The replace was interrupted and finished with a plain move. The new file is in place, but its permissions may differ from the previous file's"
        });
    }
    Err(ApiError::new(
        ErrorCode::IoFailed,
        if previous_survives {
            "Replacing the configuration file was interrupted: nothing is at its path. The new contents are in details.recovery_file and the previous version remains in the same directory under a name Windows chose"
        } else {
            "Replacing the configuration file was interrupted: nothing is at its path and the previous version is gone. The new contents are in details.recovery_file"
        },
    )
    .hint("Move details.recovery_file to details.config_path, then restart the owner. Without that, the next owner start finds no configuration at this path")
    .details(json!({
        "reason": "replace_interrupted",
        "config_path": path,
        "recovery_file": temporary,
        "previous_version_survives": previous_survives,
        "written": false,
    })))
}
fn write_temporary(temporary: &Path, state: &FileState, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temporary)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    if state.present && state.inherit_mode {
        file.set_permissions(std::fs::metadata(&state.target)?.permissions())?;
    }
    #[cfg(not(unix))]
    let _ = state;
    file.sync_all()
}
/// Windows `ReplaceFileW` keeps the replaced file's ACL and attributes, which a
/// rename would lose. Errors 1176 and 1177 mean it stopped midway and the target is
/// no longer at its path; every other error leaves both files under their own names.
#[cfg(windows)]
pub fn replace_file(temporary: &Path, target: &Path, present: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    if !present {
        return std::fs::rename(temporary, target);
    }
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (target, temporary) = (wide(target), wide(temporary));
    // SAFETY: both paths are NUL-terminated and outlive the call; the other
    // pointers are the documented optional nulls.
    let replaced = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
            target.as_ptr(),
            temporary.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(windows))]
pub fn replace_file(temporary: &Path, target: &Path, _present: bool) -> std::io::Result<()> {
    std::fs::rename(temporary, target)
}
