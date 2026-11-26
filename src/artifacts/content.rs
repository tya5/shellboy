//! `artifact.read`: the owner alone applies the URI scheme contract to an exact
//! saved version. The read runs on the attachment blocking-file pool; nothing
//! is cached, copied, fetched or written.
use super::{Result, missing, sql};
use crate::{
    attachments::BlockingFiles,
    protocol::{ApiError, ArtifactContent, ArtifactReadArgs, ErrorCode, MAX_FRAME_BYTES, Response},
    storage::DbWorker,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

/// A folder published as a web app; `artifact.read` serves files inside it.
pub(crate) const APP_SCHEME: &str = "shellboy-app";

/// RFC 3986 scheme: a letter, then letters, digits, `+`, `-` or `.`, then `:`.
pub(crate) fn uri_scheme(uri: &str) -> Option<&str> {
    let (name, _) = uri.split_once(':')?;
    let mut bytes = name.bytes();
    (bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')))
    .then_some(name)
}

#[derive(Debug, PartialEq)]
enum Source {
    Folder(PathBuf),
    File(PathBuf),
}

fn refused(reason: &str, message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message).details(json!({ "reason": reason }))
}
fn unavailable(error: std::io::Error) -> ApiError {
    ApiError::new(
        ErrorCode::TargetUnavailable,
        format!("Artifact file is not readable: {error}"),
    )
    .details(json!({"reason": "file_unavailable"}))
}
fn outside() -> ApiError {
    refused(
        "path_outside_folder",
        "Artifact path leaves the shellboy-app: folder",
    )
}

/// A `file:` authority (after `//`, either slash kind) must be empty or `localhost`.
fn local_authority(rest: &str) -> bool {
    let slash = |c: char| c == '/' || c == '\\';
    let mut chars = rest.chars();
    if !(chars.next().is_some_and(slash) && chars.next().is_some_and(slash)) {
        return true;
    }
    let authority = chars
        .as_str()
        .split(|c: char| slash(c) || c == '?' || c == '#')
        .next()
        .unwrap_or_default();
    authority.is_empty() || authority.eq_ignore_ascii_case("localhost")
}

fn source(uri: &str) -> Result<Source> {
    let scheme = uri_scheme(uri);
    if let Some(name) = scheme
        && name.eq_ignore_ascii_case(APP_SCHEME)
    {
        let folder = Path::new(&uri[name.len() + 1..]);
        if !folder.is_absolute() {
            return Err(refused(
                "uri_not_readable",
                "A shellboy-app: URI must name an absolute folder",
            ));
        }
        return Ok(Source::Folder(folder.into()));
    }
    if Path::new(uri).is_absolute() {
        return Ok(Source::File(uri.into()));
    }
    if let Some(name) = scheme
        && name.eq_ignore_ascii_case("file")
    {
        let not_local = || {
            refused(
                "uri_not_readable",
                "A file: URI must name a local absolute path",
            )
        };
        // Read the raw authority before parsing: URL parsing drops a host in
        // front of a drive letter, and Windows turns a kept host into a network path.
        if !local_authority(&uri[name.len() + 1..]) {
            return Err(not_local());
        }
        return url::Url::parse(uri)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .map(Source::File)
            .ok_or_else(not_local);
    }
    Err(refused(
        "uri_not_readable",
        "The owner reads only shellboy-app: folders, file: URIs and absolute paths",
    ))
}

/// What the blocking worker opens: a file, or a relative path inside a folder.
#[derive(Debug, PartialEq)]
enum Target {
    File(PathBuf),
    InFolder(PathBuf, PathBuf),
}

fn target(source: Source, path: Option<&str>) -> Result<Target> {
    match (source, path) {
        (Source::File(file), None) => Ok(Target::File(file)),
        (Source::File(_), Some(_)) => Err(refused(
            "path_not_allowed",
            "This artifact version is one file; omit the path",
        )),
        (Source::Folder(_), None) => Err(refused(
            "path_required",
            "A shellboy-app: artifact needs a relative path, such as index.html",
        )),
        (Source::Folder(folder), Some(relative)) => {
            let relative = Path::new(relative);
            if !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
            {
                return Err(outside());
            }
            Ok(Target::InFolder(folder, relative.into()))
        }
    }
}

/// Resolves every link before the containment check, then opens the resolved
/// path. A link inside the folder that stays inside it is followed.
fn resolve(target: Target) -> Result<PathBuf> {
    match target {
        Target::File(file) => Ok(file),
        Target::InFolder(folder, relative) => {
            let root = folder.canonicalize().map_err(unavailable)?;
            let resolved = root.join(relative).canonicalize().map_err(unavailable)?;
            if !resolved.starts_with(&root) {
                return Err(outside());
            }
            Ok(resolved)
        }
    }
}

fn read_file(target: Target, limit: u64) -> Result<(u64, String)> {
    let path = resolve(target)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // A FIFO or device must not block the worker before its type is checked.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(&path).map_err(unavailable)?;
    // The type comes from the opened handle, which is also the one read below.
    let meta = file.metadata().map_err(unavailable)?;
    if !meta.is_file() {
        return Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            "Artifact path is not a regular file",
        )
        .details(json!({"reason": "not_a_file"})));
    }
    let too_large = |bytes: u64| {
        ApiError::new(
            ErrorCode::InputLimit,
            "Artifact file exceeds the single-file read limit",
        )
        .details(json!({"reason": "file_too_large", "raw_bytes": bytes, "limit_bytes": limit}))
    };
    if meta.len() > limit {
        return Err(too_large(meta.len()));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(meta.len() as usize)
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Cannot allocate the artifact read"))?;
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(unavailable)?;
    if bytes.len() as u64 > limit {
        return Err(too_large(bytes.len() as u64));
    }
    Ok((bytes.len() as u64, STANDARD.encode(&bytes)))
}

/// The single read bound: `limits.attachment_file_bytes`, and the largest file
/// whose reply to this request, envelope included, still fits one frame.
fn effective_limit(file_limit: u64, operation_id: &str, args: &ArtifactReadArgs) -> u64 {
    let empty = ArtifactContent {
        artifact_id: args.artifact_id.clone(),
        version: args.version,
        path: args.path.clone(),
        raw_bytes: u64::MAX,
        base64: String::new(),
    };
    let envelope = serde_json::to_vec(&Response::success(operation_id, empty))
        .map_or(MAX_FRAME_BYTES, |bytes| bytes.len());
    // Base64 needs no JSON escaping: n bytes become 4 * ceil(n / 3) characters.
    let room = MAX_FRAME_BYTES.saturating_sub(envelope) / 4 * 3;
    file_limit.min(room as u64)
}

pub(super) fn version_uri(db: &Connection, args: &ArtifactReadArgs) -> Result<String> {
    super::check_thread(db, &args.thread_id)?;
    db.query_row(
        "SELECT v.uri FROM artifact a JOIN artifact_version v ON v.artifact_id=a.id AND v.version=?3 WHERE a.id=?1 AND a.thread_id=?2",
        params![args.artifact_id, args.thread_id, args.version],
        |row| row.get(0),
    )
    .optional()
    .map_err(sql)?
    .ok_or_else(missing)
}

/// Bounded by [`effective_limit`]; larger files are refused before any read.
pub(crate) async fn read(
    operation_id: &str,
    database: &DbWorker,
    files: &BlockingFiles,
    file_limit: u64,
    timeout_ms: u64,
    cancel: CancellationToken,
    args: ArtifactReadArgs,
) -> Result<ArtifactContent> {
    let lookup = args.clone();
    let uri = database
        .call(move |db| Ok(version_uri(db, &lookup)?))
        .await
        .map_err(|error| error.into_api_error())?;
    let target = target(source(&uri)?, args.path.as_deref())?;
    let limit = effective_limit(file_limit, operation_id, &args);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    // Pool errors carry attachment-selection hints that do not apply here.
    let (raw_bytes, base64) = files
        .execute(deadline, cancel, move || read_file(target, limit))
        .await
        .map_err(|error| ApiError {
            hint: None,
            ..error
        })?;
    Ok(ArtifactContent {
        artifact_id: args.artifact_id,
        version: args.version,
        path: args.path,
        raw_bytes,
        base64,
    })
}

#[cfg(test)]
mod tests;
