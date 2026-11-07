//! Stored API keys (#269): `credentials.toml` beside `config.toml`.
//!
//! ```toml
//! [connections.proxy]
//! api_key = "..."
//! ```
//!
//! The value is plain text, like a password in a dotfile. It is written only here,
//! never into `config.toml`, and no function in this module returns it except
//! `read_key`, whose single caller puts it in a request header. Errors and `Debug`
//! output never contain a value, a line of the file or a parser excerpt: the TOML
//! libraries quote source text in their messages, so those messages are dropped.
//! Every function that writes blocks and runs on the owner's bounded control-file
//! pool; `read_key` is the request-time lookup on the async runtime.
use crate::{
    config_edit::{read_state, replace_atomically, replace_file},
    protocol::{ApiError, ErrorCode},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table};

pub const FILE_NAME: &str = "credentials.toml";
/// The file holds one short entry per connection; a larger one is not ours.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Longer than any provider key, short enough for a header value.
pub const MAX_KEY_BYTES: usize = 4096;
const HEADER: &str = "# Shellboy stored API keys. Plain text: treat this file like a password.\n# Do not commit or share it. Written by `shellboy credentials set`.\n";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    api_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    #[serde(default)]
    connections: BTreeMap<String, Entry>,
}

/// Why the file could not be used. The messages never quote the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Unreadable,
    TooLarge,
    Invalid { line: Option<usize> },
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable => f.write_str("cannot read credentials.toml"),
            Self::TooLarge => write!(f, "credentials.toml exceeds {MAX_FILE_BYTES} bytes"),
            Self::Invalid { line: Some(line) } => write!(
                f,
                "credentials.toml is not valid: unexpected content near line {line}"
            ),
            Self::Invalid { line: None } => f.write_str("credentials.toml is not valid"),
        }
    }
}
impl std::error::Error for StoreError {}

/// The credentials file for a configuration file path.
pub fn path_beside(config_path: &Path) -> PathBuf {
    config_path.with_file_name(FILE_NAME)
}

fn parse(bytes: &[u8]) -> Result<Stored, StoreError> {
    let text = std::str::from_utf8(bytes).map_err(|_| StoreError::Invalid { line: None })?;
    toml::from_str::<Stored>(text).map_err(|error| StoreError::Invalid {
        line: error
            .span()
            .filter(|span| text.is_char_boundary(span.start))
            .map(|span| text[..span.start].matches('\n').count() + 1),
    })
}

/// Blocking. `None` when the file does not exist.
fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StoreError::Unreadable),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StoreError::Unreadable)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(StoreError::TooLarge);
    }
    Ok(Some(bytes))
}

/// Blocking. Names of connections with a non-blank stored key; none when absent.
pub fn stored_names(path: &Path) -> Result<BTreeSet<String>, StoreError> {
    let Some(bytes) = read_bounded(path)? else {
        return Ok(BTreeSet::new());
    };
    Ok(parse(&bytes)?
        .connections
        .into_iter()
        .filter(|(_, entry)| !entry.api_key.trim().is_empty())
        .map(|(name, _)| name)
        .collect())
}

/// The stored key of one connection, read when a request starts so a new key
/// applies to the next request. A missing file or entry is `None`.
pub async fn read_key(path: &Path, name: &str) -> Result<Option<String>, StoreError> {
    use tokio::io::AsyncReadExt;
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StoreError::Unreadable),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| StoreError::Unreadable)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(StoreError::TooLarge);
    }
    Ok(parse(&bytes)?
        .connections
        .remove(name)
        .map(|entry| entry.api_key))
}

/// Shared by request validation and the writer. The message never contains the value.
pub fn check_key(value: &str) -> Result<(), ApiError> {
    let bad = |message: &str| {
        Err(ApiError::new(ErrorCode::InvalidArgument, message)
            .details(json!({"field":"value","written":false})))
    };
    if value.trim().is_empty() {
        return bad("the key must not be empty");
    }
    if value.len() > MAX_KEY_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            format!("the key must be at most {MAX_KEY_BYTES} bytes"),
        )
        .details(json!({"field":"value","written":false})));
    }
    if value.chars().any(char::is_control) {
        return bad("the key must not contain control characters or line breaks");
    }
    if value.trim() != value {
        return bad("the key must not start or end with whitespace");
    }
    Ok(())
}

pub fn check_connection_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "connection must be a non-empty name of at most 256 bytes without control characters",
        ));
    }
    Ok(())
}

/// The file's own errors name the credentials file, not the configuration.
fn relabel(mut error: ApiError) -> ApiError {
    error.message = error
        .message
        .replace("configuration file", "credentials file")
        .replace("configuration directory", "credentials directory")
        .replace("new configuration", "new credentials");
    if error.code == ErrorCode::OperationConflict {
        error.message =
            "The credentials file changed while it was being written; nothing was written".into();
        error.hint = Some("Repeat the command".into());
        error.details = Some(json!({"written":false}));
    }
    error
}
fn invalid_file(error: StoreError, path: &Path) -> ApiError {
    ApiError::new(ErrorCode::ConfigInvalid, error.to_string())
        .hint("Nothing was written. Fix or remove the file by hand; its entries are never shown")
        .details(json!({"credentials_path":path,"written":false}))
}

/// What a change did, for the reply. Never holds the value.
#[derive(Debug)]
pub struct Outcome {
    /// `set`: an entry existed before. `clear`: an entry was removed.
    pub existed: bool,
    pub written: bool,
    /// Unix only: the file was group- or world-accessible and is now `0600`.
    pub permissions_tightened: bool,
    pub warning: Option<&'static str>,
}

fn load(path: &Path) -> Result<(crate::config_edit::FileState, DocumentMut), ApiError> {
    let state = read_state(path).map_err(relabel)?;
    let text = std::str::from_utf8(&state.bytes)
        .map_err(|_| invalid_file(StoreError::Invalid { line: None }, path))?;
    // Same rules as the request-time reader, so a file this command writes is read back.
    parse(&state.bytes).map_err(|error| invalid_file(error, path))?;
    let doc = if state.present {
        text.parse::<DocumentMut>()
            .map_err(|_| invalid_file(StoreError::Invalid { line: None }, path))?
    } else {
        DocumentMut::new()
    };
    Ok((state, doc))
}
fn connections_table<'a>(doc: &'a mut DocumentMut, path: &Path) -> Result<&'a mut Table, ApiError> {
    let unsupported = || {
        ApiError::new(
            ErrorCode::ConfigInvalid,
            "credentials.toml writes its entries as [connections.NAME] tables; this file uses another form",
        )
        .hint("Nothing was written. Edit the file by hand")
        .details(json!({"credentials_path":path,"written":false}))
    };
    let item = doc.entry("connections").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    item.as_table_mut().ok_or_else(unsupported)
}
#[cfg(unix)]
fn loose(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0)
}
#[cfg(not(unix))]
fn loose(_path: &Path) -> bool {
    false
}
fn restrict(temporary: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temporary, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = temporary;
        Ok(())
    }
}
fn write(
    path: &Path,
    state: &crate::config_edit::FileState,
    doc: &DocumentMut,
    existed: bool,
) -> Result<Outcome, ApiError> {
    let edited = if state.present {
        doc.to_string()
    } else {
        format!("{HEADER}\n{doc}")
    };
    if edited.len() as u64 > MAX_FILE_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            format!("credentials.toml would exceed {MAX_FILE_BYTES} bytes; nothing was written"),
        )
        .details(json!({"written":false})));
    }
    if state.read_only {
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "The credentials file is read-only; nothing was written",
        )
        .details(json!({"credentials_path":path,"written":false})));
    }
    let permissions_tightened = state.present && loose(&state.target);
    // The temporary file is created 0600; a replaced file's mode is not inherited,
    // so a loose file is corrected by the save itself.
    let warning = replace_atomically(path, state, edited.as_bytes(), restrict, replace_file)
        .map_err(relabel)?;
    Ok(Outcome {
        existed,
        written: true,
        permissions_tightened,
        warning,
    })
}

/// Blocking. Stores or replaces the key of one connection.
pub fn set(path: &Path, connection: &str, value: &str) -> Result<Outcome, ApiError> {
    check_key(value)?;
    check_connection_name(connection)?;
    let (state, mut doc) = load(path)?;
    let connections = connections_table(&mut doc, path)?;
    let existed = connections.contains_key(connection);
    let entry = connections
        .entry(connection)
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(entry) = entry.as_table_mut() else {
        return Err(invalid_file(StoreError::Invalid { line: None }, path));
    };
    entry["api_key"] = toml_edit::value(value);
    write(path, &state, &doc, existed)
}

/// Blocking. Removes the key of one connection; nothing is written when there is none.
pub fn clear(path: &Path, connection: &str) -> Result<Outcome, ApiError> {
    check_connection_name(connection)?;
    let (state, mut doc) = load(path)?;
    if !state.present {
        return Ok(Outcome {
            existed: false,
            written: false,
            permissions_tightened: false,
            warning: None,
        });
    }
    let connections = connections_table(&mut doc, path)?;
    if connections.remove(connection).is_none() {
        return Ok(Outcome {
            existed: false,
            written: false,
            permissions_tightened: false,
            warning: None,
        });
    }
    if connections.is_empty() {
        doc.remove("connections");
    }
    write(path, &state, &doc, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_never_quote_the_file() {
        let secret = "sk-parse-error-secret-0123456789";
        let text = format!("[connections.a]\napi_key = \"{secret}\"\nunknown = 1\n");
        let error = parse(text.as_bytes()).err().unwrap();
        assert_eq!(error, StoreError::Invalid { line: Some(3) });
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
    }

    #[test]
    fn key_checks_do_not_echo_the_value() {
        for value in [
            "",
            "  ",
            "a\nb",
            " lead",
            "trail ",
            &"x".repeat(MAX_KEY_BYTES + 1),
        ] {
            let error = check_key(value).unwrap_err();
            assert!(!serde_json::to_string(&error).unwrap().contains("lead"));
        }
        assert!(check_key("sk-ok").is_ok());
    }
}
