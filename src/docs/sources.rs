use super::*;
use crate::{
    config::{PathEnvironment, resolve_config_path},
    config_edit,
    protocol::{ApiError, ConfigChange, ErrorCode, PROTOCOL_VERSION},
};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

const LIST_WORK: usize = 200;
const SEARCH_WORK: usize = 100;
const TEXT_WORK: usize = 65536;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DocsConfig {
    pub sources: Vec<Registration>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub name: String,
    pub path: PathBuf,
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "shellboy"
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
impl DocsConfig {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        let mut names = HashSet::new();
        if self.sources.len() > LIST_WORK
            || self.sources.iter().any(|row| {
                !valid_name(&row.name)
                    || !row.path.is_absolute()
                    || row.path.to_str().is_none_or(|p| p.len() > 4096)
                    || !names.insert(&row.name)
            })
        {
            return Err(
                "Expected up to 200 unique, non-reserved names and absolute UTF-8 paths (up to 4096 bytes)",
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Subcommand)]
pub enum SourceCommand {
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: PathBuf,
    },
    List,
    Get {
        #[arg(long)]
        name: String,
    },
    Update {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: PathBuf,
    },
    Remove {
        #[arg(long)]
        name: String,
    },
}
fn refusal(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}
fn registration_command(
    command: SourceCommand,
    mut settings: DocsConfig,
    path: &Path,
    cwd: &Path,
    expected_hash: &str,
) -> Result<Value, ApiError> {
    let (name, folder, add) = match command {
        SourceCommand::List => return Ok(json!({"items":settings.sources})),
        SourceCommand::Get { name } => {
            return settings
                .sources
                .iter()
                .find(|row| row.name == name)
                .map(|row| json!({"source":row}))
                .ok_or_else(|| refusal(ErrorCode::TargetUnavailable, "Unknown docs source"));
        }
        SourceCommand::Add { name, path } => (name, Some(path), true),
        SourceCommand::Update { name, path } => (name, Some(path), false),
        SourceCommand::Remove { name } => (name, None, false),
    };
    if !valid_name(&name) {
        return Err(refusal(
            ErrorCode::InvalidArgument,
            "Invalid or reserved docs source name",
        ));
    }
    let existing = settings.sources.iter().position(|row| row.name == name);
    if !add && existing.is_none() {
        return Err(refusal(ErrorCode::TargetUnavailable, "Unknown docs source"));
    }
    let output = if let Some(folder) = folder {
        let candidate = cwd.join(folder);
        let folder = if existing.is_some_and(|i| settings.sources[i].path == candidate) {
            candidate
        } else {
            fs::canonicalize(candidate)
                .map_err(|_| refusal(ErrorCode::TargetUnavailable, "Docs folder is unavailable"))?
        };
        if let Some(i) = existing {
            if settings.sources[i].path == folder {
                return Ok(json!({"source":settings.sources[i],"written":false}));
            }
            if add {
                return Err(refusal(
                    ErrorCode::OperationConflict,
                    "The source already has a different path; use docs source update",
                ));
            }
        }
        if !folder.is_dir() {
            return Err(refusal(
                ErrorCode::InvalidArgument,
                "Docs source must be a folder",
            ));
        }
        let source = Registration { name, path: folder };
        match existing {
            Some(i) => settings.sources[i] = source.clone(),
            None => settings.sources.push(source.clone()),
        }
        json!({"source":source,"written":true})
    } else {
        json!({"source":settings.sources.remove(existing.expect("checked above")),"removed":true,"written":true})
    };
    settings
        .validate()
        .map_err(|e| refusal(ErrorCode::InputLimit, e))?;
    config_edit::apply(
        path,
        expected_hash,
        &[ConfigChange {
            field: "docs.sources".into(),
            value: Some(json!(settings.sources)),
        }],
    )?;
    Ok(output)
}

fn browse_error(message: String) -> ApiError {
    let (code, detail) = message
        .split_once(": ")
        .unwrap_or(("invalid_argument", &message));
    refusal(
        serde_json::from_value(json!(code)).unwrap_or(ErrorCode::InvalidArgument),
        detail,
    )
}

pub fn configured_response(command: DocsCommand, explicit: Option<&Path>, cwd: &Path) -> Value {
    let result = (|| {
        // Explicit bundled scope never opens configuration. This is also the
        // recovery route when the combined catalogue cannot read its sources.
        let bundled_only = match &command {
            DocsCommand::Get { id, sources, .. } => {
                (id == "shellboy" || id.starts_with("shellboy/"))
                    && sources.iter().all(|s| s == "shellboy")
            }
            DocsCommand::List {
                parent, sources, ..
            }
            | DocsCommand::Search {
                parent, sources, ..
            } => {
                (parent
                    .as_ref()
                    .is_some_and(|id| id == "shellboy" || id.starts_with("shellboy/"))
                    || !sources.is_empty())
                    && sources.iter().all(|s| s == "shellboy")
            }
            _ => false,
        };
        if bundled_only {
            return browse(command, &[]).map_err(browse_error);
        }
        let path = resolve_config_path(explicit, cwd, &PathEnvironment::current())
            .map_err(|e| refusal(ErrorCode::ConfigInvalid, e.to_string()))?;
        let state = config_edit::read_state(&path)?;
        let settings = config_edit::parse_file(&state.bytes, &path)
            .map_err(|_| refusal(ErrorCode::ConfigInvalid,
                "Cannot load docs sources. For bundled recovery guides use docs list --source shellboy or docs get --id shellboy/configuration"))?
            .docs;
        match command {
            DocsCommand::Source { command } => {
                registration_command(command, settings, &path, cwd, &state.hash)
            }
            other => browse(other, &settings.sources).map_err(browse_error),
        }
    })();
    match result {
        Ok(result) => json!({"protocol_version":PROTOCOL_VERSION,"ok":true,"result":result}),
        Err(error) => json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error":error}),
    }
}

fn canonical_root(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(io_error)
}

fn io_error(_: std::io::Error) -> String {
    "target_unavailable: Docs folder or file is unavailable".into()
}
fn under(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let mut path = root.to_owned();
    if !relative.is_empty() {
        for part in relative.split('/') {
            if part.is_empty()
                || part.contains('\\')
                || !matches!(
                    Path::new(part).components().collect::<Vec<_>>().as_slice(),
                    [Component::Normal(_)]
                )
            {
                return Err("invalid_argument: Invalid docs path".into());
            }
            path.push(part);
            if fs::symlink_metadata(&path)
                .map_err(io_error)?
                .file_type()
                .is_symlink()
            {
                return Err("target_unavailable: Docs does not follow symbolic links".into());
            }
        }
    }
    let resolved = fs::canonicalize(&path).map_err(io_error)?;
    if resolved != path || !resolved.starts_with(root) {
        return Err("target_unavailable: Docs path changed or resolves outside its root".into());
    }
    Ok(resolved)
}

fn prefix(path: &Path, offset: usize, max: usize) -> Result<(String, bool), String> {
    let mut file = File::open(path).map_err(io_error)?;
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(io_error)?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let more = bytes.len() > max;
    bytes.truncate(max);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if more && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("checked UTF-8 prefix")
        }
        Err(_) => return Err("target_unavailable: Docs guide is not UTF-8".into()),
    };
    if more && text.is_empty() {
        return Err("invalid_argument: Byte limit cannot hold the next UTF-8 character".into());
    }
    Ok((text.to_owned(), more))
}

fn metadata(text: &str, fallback: &str) -> (String, String) {
    let mut title = None;
    let mut summary = None;
    let mut capture = None;
    let mut value = String::new();
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::Heading { .. }) if title.is_none() && capture.is_none() => {
                capture = Some(true);
                value.clear();
            }
            Event::Start(Tag::Paragraph) if summary.is_none() && capture.is_none() => {
                capture = Some(false);
                value.clear();
            }
            Event::Text(t) | Event::Code(t) if capture.is_some() => value.push_str(&t),
            Event::SoftBreak | Event::HardBreak if capture.is_some() => value.push(' '),
            Event::End(TagEnd::Heading(_)) if capture == Some(true) => {
                title = Some(value.trim().to_owned());
                capture = None;
            }
            Event::End(TagEnd::Paragraph) if capture == Some(false) => {
                summary = Some(value.trim().to_owned());
                capture = None;
            }
            _ => {}
        }
        if title.is_some() && summary.is_some() {
            break;
        }
    }
    fn bounded(mut text: String) -> String {
        text.truncate(text.floor_char_boundary(1024));
        text
    }
    (
        bounded(
            title
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| fallback.into()),
        ),
        bounded(summary.unwrap_or_default()),
    )
}

const TOO_WIDE: &str = "too_wide";

fn sorted_names(path: &Path) -> Result<Vec<std::ffi::OsString>, String> {
    use crate::input_path::FOLDER_WALK_MAX_ENTRIES as CAP;
    let mut names = fs::read_dir(path)
        .map_err(io_error)?
        .take(CAP + 1)
        .map(|entry| entry.map(|entry| entry.file_name()).map_err(io_error))
        .collect::<Result<Vec<_>, _>>()?;
    if names.len() > CAP {
        return Err(TOO_WIDE.into());
    }
    names.sort_unstable_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
    Ok(names)
}

fn child(
    root: &Path,
    parent: &str,
    name: &std::ffi::OsStr,
) -> Result<Option<(String, PathBuf, bool)>, String> {
    let Some(name) = name.to_str().filter(|name| !name.contains('\\')) else {
        return Ok(None);
    };
    let rel = child_path(parent, name);
    if rel.len() > 4096 {
        return Err("path_too_long".into());
    }
    // The parent was checked when opening its sorted name list. Do not follow a child symlink.
    let meta = fs::symlink_metadata(root.join(&rel)).map_err(io_error)?;
    if !(meta.is_dir() || (meta.is_file() && name.ends_with(".md"))) {
        return Ok(None);
    }
    let path = under(root, &rel)?;
    Ok(Some((rel, path, meta.is_dir())))
}

fn list_directory(
    root: &Path,
    id: &str,
    tail: &str,
    count: usize,
    cursor: Option<&str>,
    version: &str,
    scope: &str,
) -> Result<Value, String> {
    let path = under(root, tail)?;
    let scope = format!("{scope}:{}", revision(&path));
    let mut offset = read_cursor(cursor, version, &scope)?;
    let names = match sorted_names(&path) {
        Ok(names) => names,
        Err(reason) => {
            return Ok(paginated(
                json!({"version":version,"items":[],"unavailable":[{"id":id,"reason":reason}]}),
                None,
            ));
        }
    };
    if offset > names.len() {
        return Err("cursor_expired: Offset is outside list".into());
    }
    let mut items = Vec::new();
    let source = id.split('/').next().expect("source ID");
    for name in names.iter().skip(offset).take(LIST_WORK) {
        offset += 1;
        let Ok(Some((rel, path, directory))) = child(root, tail, name) else {
            continue;
        };
        let id = child_path(source, &rel);
        if directory {
            items.push(branch(&id, true, version));
        } else if let Ok(item) = file_view(&id, &path, version) {
            items.push(item);
        }
        if items.len() == count {
            break;
        }
    }
    Ok(paginated(
        json!({"version":version,"items":items}),
        (offset < names.len()).then(|| next_cursor(version, &scope, offset)),
    ))
}

fn child_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}

pub(super) fn preview(row: &Registration) -> Value {
    let mut count = 0;
    let mut bytes = 0u64;
    let observed = (|| {
        let root = canonical_root(&row.path)?;
        let mut pending = vec![(String::new(), 0usize)];
        let mut work = LIST_WORK;
        while let Some((rel, depth)) = pending.pop() {
            if work == 0 {
                return Err("work_limit".into());
            }
            let path = under(&root, &rel)?;
            for name in sorted_names(&path)? {
                work = work.checked_sub(1).ok_or("work_limit")?;
                let Some((rel, path, directory)) = child(&root, &rel, &name)? else {
                    continue;
                };
                if directory {
                    if depth >= 31 {
                        return Err("depth_limit".into());
                    }
                    pending.push((rel, depth + 1));
                } else {
                    prefix(&path, 0, TEXT_WORK)?;
                    count += 1;
                    bytes = bytes.saturating_add(fs::metadata(path).map_err(io_error)?.len());
                }
            }
        }
        Ok::<(), String>(())
    })();
    let mut result =
        json!({"source":row.name,"files":count,"bytes":bytes,"complete":observed.is_ok()});
    if let Err(reason) = observed {
        result["reason"] = json!(reason);
    }
    result
}

fn revision(path: &Path) -> String {
    let Ok(meta) = fs::metadata(path) else {
        return "0000000000000000".into();
    };
    hash(format!("{}:{:?}", meta.len(), meta.modified().ok()))[..16].into()
}
fn source_for<'a>(id: &str, rows: &'a [Registration]) -> Result<&'a Registration, String> {
    let name = id.split('/').next().unwrap_or_default();
    rows.iter()
        .find(|row| row.name == name)
        .ok_or_else(|| "target_unavailable: Unknown docs source".into())
}
fn file_view(id: &str, path: &Path, version: &str) -> Result<Value, String> {
    let (text, _) = prefix(path, 0, TEXT_WORK)?;
    let (title, summary) = metadata(&text, id.rsplit('/').next().unwrap_or(id));
    Ok(
        json!({"id":id,"parent_id":id.rsplit_once('/').map(|p|p.0),"kind":"guide","title":title,"summary":summary,"version":version}),
    )
}
fn branch(id: &str, available: bool, version: &str) -> Value {
    let (parent, title) = id.rsplit_once('/').unwrap_or(("", id));
    json!({"id":id,"parent_id":parent,"kind":"category","title":title,"summary":"","version":version,"availability":if available {json!({"state":"available"})} else {json!({"state":"unavailable","reason":"folder_unavailable"})}})
}

fn browse(command: DocsCommand, rows: &[Registration]) -> Result<Value, String> {
    let version = hash(serde_json::to_vec(&json!([BUNDLED.version, rows])).expect("version"));
    let version = version.as_str();
    if let DocsCommand::Get { id, sources, .. } = &command {
        selected(rows, sources)?;
        let name = id.split('/').next().unwrap_or_default();
        if !included(sources, name) {
            return Err("target_unavailable: Guide is outside the selected sources".into());
        }
    }
    match command {
        DocsCommand::Get {
            id,
            sources,
            section,
            limit_bytes,
            cursor,
        } if id != "shellboy" && !id.starts_with("shellboy/") => {
            limit(limit_bytes, TEXT_WORK)?;
            let source = source_for(&id, rows)?;
            let root = canonical_root(&source.path)?;
            let path = under(&root, id.split_once('/').map_or("", |p| p.1))?;
            let kind = fs::metadata(&path).map_err(io_error)?;
            if kind.is_dir() {
                return Ok(branch(&id, true, version));
            }
            if !kind.is_file() || !id.ends_with(".md") {
                return Err("target_unavailable: Unknown docs guide".into());
            }
            let scope = hash(
                serde_json::to_vec(&json!(["get", id, section, revision(&path), sources]))
                    .expect("scope"),
            );
            let offset = read_cursor(cursor.as_deref(), version, &scope)?;
            if offset as u64 > kind.len() {
                return Err("cursor_expired: File offset out of range".into());
            }
            let (head, truncated) = prefix(&path, 0, TEXT_WORK)?;
            let mut headings = sections(&head);
            if truncated {
                headings.retain(|section| section.end < head.len());
            }
            let (start, end) = match &section {
                None => (
                    0,
                    usize::try_from(kind.len()).map_err(|_| "input_limit: File too large")?,
                ),
                Some(section) => {
                    let found = headings.iter().find(|s| s.id == *section).ok_or(
                        "target_unavailable: Section has no complete boundary in the first 64 KiB",
                    )?;
                    (found.start, found.end)
                }
            };
            let offset = if cursor.is_some() { offset } else { start };
            if !(start..=end).contains(&offset) {
                return Err("cursor_expired: Invalid section offset".into());
            }
            let text = if offset == end {
                String::new()
            } else {
                prefix(&path, offset, limit_bytes.min(end - offset))?.0
            };
            let page_end = offset + text.len();
            let (title, _) = metadata(&head, id.rsplit('/').next().unwrap_or(&id));
            Ok(paginated(
                json!({"id":id,"title":title,"version":version,"text":text,"start":offset,"end":page_end,"total_bytes":kind.len(),"section":section,"sections":headings,"links":[]}),
                (page_end < end && page_end > offset)
                    .then(|| next_cursor(version, &scope, page_end)),
            ))
        }
        DocsCommand::List {
            parent,
            sources,
            limit: count,
            cursor,
        } => {
            limit(count, LIST_WORK)?;
            selected(rows, &sources)?;
            let scope = hash(serde_json::to_vec(&json!(["list", parent, sources])).expect("scope"));
            let mut items = Vec::new();
            if let Some(id) = &parent {
                if id == "shellboy" || id.starts_with("shellboy/") {
                    if included(&sources, "shellboy") {
                        items = bundled_list(Some(id.clone()))?;
                    }
                } else {
                    let source = source_for(id, rows)?;
                    if included(&sources, &source.name) {
                        let tail = id.split_once('/').map_or("", |p| p.1);
                        let root = canonical_root(&source.path)?;
                        return list_directory(
                            &root,
                            id,
                            tail,
                            count,
                            cursor.as_deref(),
                            version,
                            &scope,
                        );
                    }
                }
            } else {
                if included(&sources, "shellboy") {
                    items = bundled_list(None)?;
                }
                for row in rows.iter().filter(|r| included(&sources, &r.name)) {
                    items.push(branch(
                        &row.name,
                        canonical_root(&row.path).is_ok_and(|p| fs::read_dir(p).is_ok()),
                        version,
                    ));
                }
            }
            let offset = read_cursor(cursor.as_deref(), version, &scope)?;
            if offset > items.len() {
                return Err("cursor_expired: Offset is outside list".into());
            }
            let end = (offset + count).min(items.len());
            Ok(paginated(
                json!({"version":version,"items":items[offset..end]}),
                (end < items.len()).then(|| next_cursor(version, &scope, end)),
            ))
        }
        DocsCommand::Search {
            query,
            parent,
            sources,
            limit: count,
            cursor,
        } => search(
            query,
            parent,
            sources,
            count,
            cursor,
            rows,
            version,
            SEARCH_DIRECTORY_BYTES,
        ),
        other => super::execute(other),
    }
}
fn included(filters: &[String], name: &str) -> bool {
    filters.is_empty() || filters.iter().any(|s| s == name)
}
fn selected(rows: &[Registration], filters: &[String]) -> Result<(), String> {
    if filters.len() > LIST_WORK
        || filters
            .iter()
            .any(|name| name != "shellboy" && !rows.iter().any(|row| &row.name == name))
    {
        return Err("target_unavailable: Unknown docs source filter".into());
    }
    Ok(())
}
fn bundled_list(parent: Option<String>) -> Result<Vec<Value>, String> {
    Ok(super::execute(DocsCommand::List {
        parent,
        sources: Vec::new(),
        limit: LIST_WORK,
        cursor: None,
    })?["items"]
        .as_array()
        .expect("list items")
        .clone())
}

#[derive(Default)]
struct SearchPosition {
    bundled: usize,
    source: usize,
    frames: Vec<Frame>,
    file: Option<(String, usize, String)>,
}
struct Frame {
    name: String,
    after: Vec<u8>,
    revision: String,
}

// Keep enough room for the last native filename and 32 directory revision witnesses.
const SEARCH_DIRECTORY_BYTES: usize = 3072;

#[derive(Default, Serialize, Deserialize)]
struct SearchMark {
    bundled: usize,
    source: usize,
    path: String,
    revisions: Vec<String>,
    file: Option<(usize, String)>,
}

impl SearchMark {
    fn from_position(pos: &SearchPosition) -> Self {
        let mut path = frame_path(&pos.frames).into_bytes();
        if let Some(frame) = pos.frames.last() {
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend(&frame.after);
        } else if let Some((name, _, _)) = &pos.file {
            path.extend(name.as_bytes());
        }
        Self {
            bundled: pos.bundled,
            source: pos.source,
            path: URL_SAFE_NO_PAD.encode(path),
            revisions: pos
                .frames
                .iter()
                .map(|frame| frame.revision.clone())
                .collect(),
            file: pos
                .file
                .as_ref()
                .map(|(_, offset, rev)| (*offset, rev.clone())),
        }
    }

    fn resume(self, root: &str) -> Result<SearchPosition, String> {
        let invalid = "cursor_expired: Invalid search position";
        let path = URL_SAFE_NO_PAD.decode(self.path).map_err(|_| invalid)?;
        let mut pos = SearchPosition {
            bundled: self.bundled,
            source: self.source,
            ..Default::default()
        };
        if self.revisions.len() > 32 {
            return Err(invalid.into());
        }
        let last = if self.revisions.is_empty() {
            path.as_slice()
        } else {
            let tail = if root.is_empty() {
                path.as_slice()
            } else {
                path.strip_prefix(root.as_bytes())
                    .and_then(|tail| tail.strip_prefix(b"/"))
                    .ok_or(invalid)?
            };
            let parts: Vec<_> = tail.split(|&b| b == b'/').collect();
            if parts.len() != self.revisions.len() {
                return Err(invalid.into());
            }
            let mut name = root;
            for (part, revision) in parts.iter().zip(self.revisions) {
                pos.frames.push(Frame {
                    name: name.into(),
                    after: part.to_vec(),
                    revision,
                });
                // The final key may be an unprojectable native filename.
                name = std::str::from_utf8(part).unwrap_or("");
            }
            parts.last().copied().ok_or(invalid)?
        };
        if let Some((offset, rev)) = self.file {
            pos.file = Some((
                std::str::from_utf8(last).map_err(|_| invalid)?.into(),
                offset,
                rev,
            ));
        }
        Ok(pos)
    }
}

fn frame_path(frames: &[Frame]) -> String {
    frames
        .iter()
        .map(|frame| frame.name.as_str())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn hit(
    id: &str,
    title: &str,
    parent: &str,
    text: &str,
    needle: &str,
    offset: usize,
    version: &str,
) -> Option<Value> {
    let lowered = text.to_lowercase();
    let at = lowered.find(needle)?;
    let (start, end) = original_span(text, at, at + needle.len());
    let before = text.ceil_char_boundary(start.saturating_sub(96));
    let after = text.floor_char_boundary((end + 192).min(text.len()));
    Some(
        json!({"id":id,"title":title,"parent_id":parent,"version":version,"excerpt":&text[before..after],"spans":[{"start":offset+start,"end":offset+end}],"score":lowered.matches(needle).count()}),
    )
}
fn search(
    query: String,
    parent: Option<String>,
    sources: Vec<String>,
    count: usize,
    cursor: Option<String>,
    rows: &[Registration],
    version: &str,
    directory_bytes: usize,
) -> Result<Value, String> {
    limit(count, SEARCH_WORK)?;
    if query.trim().is_empty() || query.len() > 4096 {
        return Err("invalid_argument: Query must contain 1 to 4096 bytes".into());
    }
    selected(rows, &sources)?;
    if let Some(id) = &parent {
        if id.len() > 4096 {
            return Err("invalid_argument: Docs parent ID exceeds 4096 bytes".into());
        }
        if id == "shellboy" || id.starts_with("shellboy/") {
            find(&BUNDLED.docs, id)?;
        } else {
            source_for(id, rows)?;
        }
    }
    let scope =
        hash(serde_json::to_vec(&json!(["search", query, parent, sources])).expect("scope"));
    let root_rel = parent
        .as_deref()
        .and_then(|p| p.split_once('/'))
        .map_or("", |p| p.1);
    let mark: SearchMark = decode_cursor(cursor.as_deref(), version, &scope, CURSOR_BYTES)?;
    let mut pos = mark.resume(root_rel)?;
    if pos.bundled > BUNDLED.docs.len() || pos.source > rows.len() {
        return Err("cursor_expired: Invalid search position".into());
    }
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    let mut unavailable = Vec::new();
    let mut work = 0;
    let bundled = &BUNDLED.docs;
    while pos.bundled < bundled.len() && hits.len() < count && work < SEARCH_WORK {
        let d = &bundled[pos.bundled];
        pos.bundled += 1;
        work += 1;
        if included(&sources, "shellboy")
            && !category(&d.id)
            && parent
                .as_ref()
                .is_none_or(|p| d.id == *p || descendant(d, p, bundled))
            && let Some(h) = hit(&d.id, &d.title, &d.parent_id, &d.text, &needle, 0, version)
        {
            hits.push(h);
        }
    }
    let mut page_dirs = std::collections::BTreeMap::new();
    let mut page_roots = std::collections::BTreeMap::new();
    while pos.source < rows.len() && hits.len() < count && work < SEARCH_WORK {
        let row = &rows[pos.source];
        if !included(&sources, &row.name)
            || parent
                .as_ref()
                .is_some_and(|p| p.split('/').next() != Some(row.name.as_str()))
        {
            pos.source += 1;
            work += 1;
            continue;
        }
        let root = match page_roots
            .entry(pos.source)
            .or_insert_with(|| canonical_root(&row.path))
        {
            Ok(path) => path,
            Err(_) => {
                unavailable.push(branch(&row.name, false, version));
                pos.frames.clear();
                pos.file = None;
                pos.source += 1;
                work += 1;
                continue;
            }
        };
        if let Some((name, offset, rev)) = pos.file.take() {
            work += 1;
            let rel = child_path(&frame_path(&pos.frames), &name);
            let observation = under(root, &rel).and_then(|path| {
                let meta = fs::metadata(&path).map_err(io_error)?;
                Ok((path, meta))
            });
            let (path, meta) = match observation {
                Ok(value) => value,
                Err(reason) => {
                    unavailable.push(json!({"source":row.name,"path":rel,"reason":reason}));
                    if pos.frames.is_empty() {
                        pos.source += 1;
                    }
                    continue;
                }
            };
            if revision(&path) != rev
                || offset as u64 > meta.len()
                || !meta.is_file()
                || !rel.ends_with(".md")
            {
                return Err("cursor_expired: Invalid or changed docs file".into());
            }
            let (text, more) = match prefix(&path, offset, TEXT_WORK) {
                Ok(value) => value,
                Err(reason) => {
                    unavailable.push(json!({"source":row.name,"path":rel,"reason":reason}));
                    if pos.frames.is_empty() {
                        pos.source += 1;
                    }
                    continue;
                }
            };
            let id = format!("{}/{rel}", row.name);
            let (dir, basename) = id.rsplit_once('/').expect("guide id");
            let title = if offset == 0 {
                metadata(&text, basename).0
            } else {
                basename.into()
            };
            if let Some(h) = hit(&id, &title, dir, &text, &needle, offset, version) {
                hits.push(h);
            }
            if more {
                // Repeat only enough UTF-8 text to catch a match across chunks.
                let overlap = text.ceil_char_boundary(text.len().saturating_sub(query.len() * 4));
                pos.file = Some((name, offset + overlap, rev));
            } else if pos.frames.is_empty() {
                pos.source += 1;
            }
            continue;
        }
        if pos.frames.is_empty() {
            work += 1;
            let rel = root_rel;
            if rel.len() > directory_bytes {
                unavailable.push(json!({"source":row.name,"path":rel,"reason":"path_too_long"}));
                pos.source += 1;
                continue;
            }
            match under(root, rel) {
                Ok(path) if path.is_dir() => pos.frames.push(Frame {
                    name: rel.into(),
                    after: Vec::new(),
                    revision: revision(&path),
                }),
                Ok(path) if path.is_file() && rel.ends_with(".md") => {
                    pos.file = Some((rel.into(), 0, revision(&path)));
                    continue;
                }
                _ => {
                    unavailable.push(branch(&row.name, false, version));
                    pos.source += 1;
                    continue;
                }
            }
        }
        // A frame is reopened only once per page (then kept in this page's cache).
        while !pos.frames.is_empty() {
            if work >= SEARCH_WORK || pos.frames.len() > 32 {
                break;
            }
            let parent = frame_path(&pos.frames);
            let frame = pos.frames.last_mut().expect("frame");
            let key = (pos.source, parent.clone());
            if let std::collections::btree_map::Entry::Vacant(slot) = page_dirs.entry(key.clone()) {
                let names = under(root, &parent).and_then(|path| {
                    if revision(&path) != frame.revision {
                        return Err("cursor_expired: Docs folder changed".into());
                    }
                    sorted_names(&path)
                });
                match names {
                    Ok(names) => {
                        slot.insert(names);
                    }
                    Err(reason) if reason.starts_with("cursor_expired:") => return Err(reason),
                    Err(reason) => {
                        unavailable.push(json!({"source":row.name,"path":parent,"reason":reason}));
                        pos.frames.pop();
                        work += 1;
                        continue;
                    }
                }
            }
            let names = &page_dirs[&key];
            work += 1;
            let next =
                names.partition_point(|name| name.as_encoded_bytes() <= frame.after.as_slice());
            let Some(name) = names.get(next) else {
                page_dirs.remove(&key);
                pos.frames.pop();
                continue;
            };
            frame.after = name.as_encoded_bytes().to_vec();
            let (rel, path, directory) = match child(root, &parent, name) {
                Ok(Some(entry)) => entry,
                Ok(None) => continue,
                Err(reason) => {
                    unavailable.push(json!({"source":row.name,"path":child_path(&parent, &name.to_string_lossy()),"reason":reason}));
                    continue;
                }
            };
            let name = name.to_string_lossy().into_owned();
            let rev = revision(&path);
            if directory {
                if rel.len() > directory_bytes {
                    unavailable
                        .push(json!({"source":row.name,"path":rel,"reason":"path_too_long"}));
                } else if pos.frames.len() >= 32 {
                    unavailable.push(json!({"source":row.name,"path":rel,"reason":"depth_limit"}));
                } else {
                    pos.frames.push(Frame {
                        name,
                        after: Vec::new(),
                        revision: rev,
                    });
                }
            } else {
                pos.file = Some((name, 0, rev));
                break;
            }
        }
        if pos.frames.is_empty() && pos.file.is_none() {
            pos.source += 1;
        }
    }
    let next = if pos.source < rows.len() || pos.bundled < bundled.len() {
        Some(next_cursor(
            version,
            &scope,
            SearchMark::from_position(&pos),
        ))
    } else {
        None
    };
    Ok(paginated(
        json!({"version":version,"mode":"keyword","items":hits,"unavailable":unavailable}),
        next,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn deep_escaped_search_resumes_and_reports_a_path_that_cannot_be_continued() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        let component = "\"".repeat(180);
        let relative = vec![component.as_str(); 3].join("/");
        let deep = path.join(&relative);
        fs::create_dir_all(deep.join(format!("z{component}"))).unwrap();
        fs::write(deep.join("a.md"), "cursor-witness").unwrap();
        fs::write(deep.join("b.md"), "cursor-witness").unwrap();
        let rows = [Registration {
            name: "local".into(),
            path,
        }];
        let search = |cursor| {
            search(
                "cursor-witness".into(),
                Some("local".into()),
                vec!["local".into()],
                1,
                cursor,
                &rows,
                "fixture",
                650,
            )
            .unwrap()
        };
        let first = search(None);
        assert_eq!(first["items"][0]["id"], format!("local/{relative}/a.md"));
        let cursor = first["next_cursor"].as_str().unwrap();
        let second = search(Some(cursor.into()));
        assert_eq!(second["items"][0]["id"], format!("local/{relative}/b.md"));
        let last = search(Some(second["next_cursor"].as_str().unwrap().into()));
        assert_eq!(last["unavailable"][0]["reason"], "path_too_long");
        assert_eq!(
            last["unavailable"][0]["path"],
            format!("{relative}/z{component}")
        );
    }

    #[test]
    fn over_cap_folder_reports_its_branch_and_keeps_healthy_docs_visible() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        fs::create_dir(path.join("a-wide")).unwrap();
        fs::create_dir(path.join("b-healthy")).unwrap();
        for i in 0..=crate::input_path::FOLDER_WALK_MAX_ENTRIES {
            File::create(path.join("a-wide").join(i.to_string())).unwrap();
        }
        fs::write(path.join("b-healthy/guide.md"), "# Branch witness\n").unwrap();
        let rows = [Registration {
            name: "local".into(),
            path,
        }];
        let list = |parent: &str| {
            browse(
                DocsCommand::List {
                    parent: Some(parent.into()),
                    sources: vec![],
                    limit: 20,
                    cursor: None,
                },
                &rows,
            )
            .unwrap()
        };
        assert_eq!(list("local/a-wide")["unavailable"][0]["reason"], "too_wide");
        assert_eq!(
            list("local/b-healthy")["items"][0]["id"],
            "local/b-healthy/guide.md"
        );
        let search = browse(
            DocsCommand::Search {
                query: "Branch witness".into(),
                parent: Some("local".into()),
                sources: vec!["local".into()],
                limit: 20,
                cursor: None,
            },
            &rows,
        )
        .unwrap();
        assert_eq!(search["unavailable"][0]["reason"], "too_wide");
        assert_eq!(search["items"][0]["id"], "local/b-healthy/guide.md");
    }

    #[test]
    fn registration_and_ownerless_browse_share_the_same_config() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().canonicalize().unwrap();
        let config = cwd.join("config.toml");
        let folder = cwd.join("skills");
        fs::create_dir_all(folder.join("demo")).unwrap();
        fs::write(
            folder.join("demo/SKILL.md"),
            "# Literal heading\n\nLook up frobnicate here.\n",
        )
        .unwrap();
        let run = |command| configured_response(command, Some(&config), &cwd);
        let add = || DocsCommand::Source {
            command: SourceCommand::Add {
                name: "recipes".into(),
                path: "skills".into(),
            },
        };
        assert_eq!(run(add())["result"]["written"], true);
        assert_eq!(run(add())["result"]["written"], false);
        assert!(
            fs::read_to_string(&config)
                .unwrap()
                .contains("[[docs.sources]]")
        );
        let list = run(DocsCommand::List {
            parent: Some("recipes/demo".into()),
            sources: vec!["shellboy".into(), "recipes".into()],
            limit: 1,
            cursor: None,
        });
        assert_eq!(list["result"]["items"][0]["title"], "Literal heading");
        assert_eq!(
            list["result"]["items"][0]["summary"],
            "Look up frobnicate here."
        );
        let get = |sources| {
            run(DocsCommand::Get {
                id: "recipes/demo/SKILL.md".into(),
                sources,
                section: None,
                limit_bytes: 64,
                cursor: None,
            })
        };
        assert_eq!(
            get(vec!["shellboy".into()])["error"]["code"],
            "target_unavailable"
        );
        assert_eq!(
            get(vec!["shellboy".into(), "recipes".into()])["result"]["text"],
            "# Literal heading\n\nLook up frobnicate here.\n"
        );
        let query = run(DocsCommand::Search {
            parent: None,
            sources: vec!["recipes".into()],
            query: "frobnicate".into(),
            limit: 1,
            cursor: None,
        });
        assert_eq!(query["result"]["items"][0]["id"], "recipes/demo/SKILL.md");
        fs::rename(&folder, cwd.join("moved")).unwrap();
        let list = run(DocsCommand::List {
            parent: None,
            sources: Vec::new(),
            limit: 50,
            cursor: None,
        });
        assert_eq!(list["result"]["items"][0]["id"], "shellboy");
        assert_eq!(
            list["result"]["items"][1]["availability"]["state"],
            "unavailable"
        );
        assert_eq!(
            run(DocsCommand::Source {
                command: SourceCommand::Remove {
                    name: "recipes".into()
                }
            })["ok"],
            true
        );
        assert!(cwd.join("moved/demo/SKILL.md").is_file());
    }
}
