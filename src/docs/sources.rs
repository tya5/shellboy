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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    name: String,
    directory: bool,
}
fn projected_entry(entry: fs::DirEntry) -> Result<Option<Entry>, String> {
    let kind = entry.file_type().map_err(io_error)?;
    let Ok(name) = entry.file_name().into_string() else {
        return Ok(None);
    };
    if name.contains('\\') || !(kind.is_dir() || (kind.is_file() && name.ends_with(".md"))) {
        return Ok(None);
    }
    Ok(Some(Entry {
        name,
        directory: kind.is_dir(),
    }))
}

// read_dir does not expose an ordered seek. Keep only the nearest keys in
// memory; classify and read candidates only after selecting the next window.
struct DirectoryWindow {
    candidates: std::collections::VecDeque<(Vec<u8>, fs::DirEntry)>,
    more: bool,
}

impl DirectoryWindow {
    fn after(path: &Path, key: &[u8], work: usize) -> Result<Self, String> {
        let mut selected = std::collections::BTreeMap::<Vec<u8>, fs::DirEntry>::new();
        let mut more = false;
        for entry in fs::read_dir(path).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name();
            let name = name.as_encoded_bytes();
            if name <= key {
                continue;
            }
            if selected.len() == work {
                more = true;
                if selected
                    .last_key_value()
                    .is_some_and(|(last, _)| name >= last.as_slice())
                {
                    continue;
                }
                selected.pop_last();
            }
            selected.insert(name.to_vec(), entry);
        }
        Ok(Self {
            candidates: selected.into_iter().collect(),
            more,
        })
    }
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
    let after = if let Some(cursor) = cursor {
        if cursor.len() > 8192 {
            return Err("cursor_expired: Cursor too large".into());
        }
        let invalid = "cursor_expired: Invalid or stale directory cursor";
        let raw = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid)?;
        let (v, s, key): (String, String, String) =
            serde_json::from_slice(&raw).map_err(|_| invalid)?;
        if v != version || s != scope {
            return Err(invalid.into());
        }
        URL_SAFE_NO_PAD.decode(key).map_err(|_| invalid)?
    } else {
        Vec::new()
    };
    let mut window = DirectoryWindow::after(&path, &after, LIST_WORK)?;
    let mut items = Vec::new();
    let mut last = after;
    while items.len() < count {
        let Some((key, raw)) = window.candidates.pop_front() else {
            break;
        };
        last = key;
        let Some(entry) = projected_entry(raw)? else {
            continue;
        };
        let rel = child_path(tail, &entry.name);
        let Ok(path) = under(root, &rel) else {
            continue;
        };
        let id = child_path(id, &entry.name);
        if entry.directory {
            items.push(branch(&id, true, version));
        } else if let Ok(item) = file_view(&id, &path, version) {
            items.push(item);
        }
    }
    let next = (window.more || !window.candidates.is_empty()).then(|| {
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&(version, scope, URL_SAFE_NO_PAD.encode(last)))
                .expect("directory cursor"),
        )
    });
    Ok(paginated(json!({"version":version,"items":items}), next))
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
    let complete = (|| {
        let root = canonical_root(&row.path)?;
        let mut pending = vec![(String::new(), 0usize)];
        let mut work = LIST_WORK;
        while let Some((rel, depth)) = pending.pop() {
            let path = under(&root, &rel)?;
            if work == 0 {
                return Err("input_limit: Preview budget exhausted".into());
            }
            let window = DirectoryWindow::after(&path, &[], work)?;
            for (_, raw) in window.candidates {
                work -= 1;
                let Some(entry) = projected_entry(raw)? else {
                    continue;
                };
                let rel = child_path(&rel, &entry.name);
                if rel.len() > 4096 || depth >= 32 {
                    return Err("input_limit: Docs path too deep".into());
                }
                if entry.directory {
                    pending.push((rel, depth + 1));
                } else {
                    let path = under(&root, &rel)?;
                    prefix(&path, 0, TEXT_WORK)?;
                    count += 1;
                    bytes = bytes.saturating_add(fs::metadata(path).map_err(io_error)?.len());
                }
            }
            if window.more {
                return Err("input_limit: Preview budget exhausted".into());
            }
        }
        Ok::<(), String>(())
    })()
    .is_ok();
    json!({"source":row.name,"files":count,"bytes":bytes,"complete":complete})
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
        } => search(query, parent, sources, count, cursor, rows, version),
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
    path: String,
    after: Vec<u8>,
    revision: String,
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
// A path is carried once; stack frames only need an ancestor byte length,
// position, and revision witness. Encode the path separately so JSON escaping
// cannot inflate a legitimate deep path beyond the cursor input limit.
#[derive(Serialize, Deserialize)]
struct SearchCursor(
    String,
    String,
    usize,
    usize,
    Vec<(usize, String)>,
    Option<(usize, String)>,
    String,
);

fn search_cursor(version: &str, scope: &str, pos: SearchPosition) -> String {
    let path = pos
        .file
        .as_ref()
        .map(|f| f.0.as_str())
        .or_else(|| pos.frames.last().map(|f| f.path.as_str()))
        .unwrap_or("");
    let state = SearchCursor(
        version.into(),
        scope.into(),
        pos.bundled,
        pos.source,
        pos.frames
            .iter()
            .map(|f| (f.path.len(), f.revision.clone()))
            .collect(),
        pos.file.as_ref().map(|f| (f.1, f.2.clone())),
        pos.frames
            .last()
            .map(|f| URL_SAFE_NO_PAD.encode(&f.after))
            .unwrap_or_default(),
    );
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(path),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&state).expect("cursor state"))
    )
}

fn stream_cursor(
    cursor: Option<&str>,
    version: &str,
    scope: &str,
) -> Result<SearchPosition, String> {
    let Some(cursor) = cursor else {
        return Ok(SearchPosition::default());
    };
    let invalid = "cursor_expired: Invalid or stale search cursor";
    if cursor.len() > 8192 {
        return Err(invalid.into());
    }
    let (path, state) = cursor.split_once('.').ok_or(invalid)?;
    let path = String::from_utf8(URL_SAFE_NO_PAD.decode(path).map_err(|_| invalid)?)
        .map_err(|_| invalid)?;
    let state = URL_SAFE_NO_PAD.decode(state).map_err(|_| invalid)?;
    let SearchCursor(v, s, bundled, source, stack, file, after) =
        serde_json::from_slice(&state).map_err(|_| invalid)?;
    if v != version
        || s != scope
        || stack.len() > 32
        || bundled > BUNDLED.docs.len()
        || path.len() > 4096
    {
        return Err(invalid.into());
    }
    let mut frames = Vec::new();
    let mut prior = None;
    let frame_count = stack.len();
    let after = URL_SAFE_NO_PAD.decode(after).map_err(|_| invalid)?;
    for (index, (len, revision)) in stack.into_iter().enumerate() {
        let ancestor = path.get(..len).ok_or(invalid)?;
        if (len != 0 && len != path.len() && path.as_bytes()[len] != b'/')
            || prior.is_some_and(|length| len <= length)
            || revision.len() != 16
        {
            return Err(invalid.into());
        }
        frames.push(Frame {
            path: ancestor.into(),
            after: if index + 1 == frame_count {
                after.clone()
            } else {
                let tail = path
                    .get(if len == 0 { 0 } else { len + 1 }..)
                    .ok_or(invalid)?;
                tail.split('/').next().ok_or(invalid)?.as_bytes().to_vec()
            },
            revision,
        });
        prior = Some(len);
    }
    let file = file.map(|(offset, rev)| (path, offset, rev));
    Ok(SearchPosition {
        bundled,
        source,
        frames,
        file,
    })
}
fn search(
    query: String,
    parent: Option<String>,
    sources: Vec<String>,
    count: usize,
    cursor: Option<String>,
    rows: &[Registration],
    version: &str,
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
    let mut pos = stream_cursor(cursor.as_deref(), version, &scope)?;
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
        if let Some((rel, offset, rev)) = pos.file.take() {
            work += 1;
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
            let (dir, name) = id.rsplit_once('/').expect("guide id");
            let title = if offset == 0 {
                metadata(&text, name).0
            } else {
                name.into()
            };
            if let Some(h) = hit(&id, &title, dir, &text, &needle, offset, version) {
                hits.push(h);
            }
            if more {
                // Repeat only enough UTF-8 text to catch a match across chunks.
                let overlap = text.ceil_char_boundary(text.len().saturating_sub(query.len() * 4));
                pos.file = Some((rel, offset + overlap, rev));
            } else if pos.frames.is_empty() {
                pos.source += 1;
            }
            continue;
        }
        if pos.frames.is_empty() {
            work += 1;
            let rel = parent
                .as_deref()
                .and_then(|p| p.split_once('/'))
                .map_or("", |p| p.1);
            match under(root, rel) {
                Ok(path) if path.is_dir() => pos.frames.push(Frame {
                    path: rel.into(),
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
            let frame = pos.frames.last_mut().expect("frame");
            let key = (pos.source, frame.path.clone());
            if !page_dirs.contains_key(&key) {
                let path = match under(root, &frame.path) {
                    Ok(path) => path,
                    Err(reason) => {
                        unavailable
                            .push(json!({"source":row.name,"path":frame.path,"reason":reason}));
                        pos.frames.pop();
                        work += 1;
                        continue;
                    }
                };
                if revision(&path) != frame.revision {
                    return Err("cursor_expired: Docs folder changed".into());
                }
                match DirectoryWindow::after(&path, &frame.after, SEARCH_WORK - work) {
                    Ok(window) => {
                        page_dirs.insert(key.clone(), window);
                    }
                    Err(reason) => {
                        unavailable
                            .push(json!({"source":row.name,"path":frame.path,"reason":reason}));
                        pos.frames.pop();
                        work += 1;
                        continue;
                    }
                }
            }
            let window = page_dirs.get_mut(&key).expect("open window");
            let Some((name, raw)) = window.candidates.pop_front() else {
                if window.more {
                    work = SEARCH_WORK;
                    break;
                }
                pos.frames.pop();
                work += 1;
                continue;
            };
            frame.after = name;
            work += 1;
            let entry = match projected_entry(raw) {
                Ok(Some(entry)) => entry,
                Ok(None) => continue,
                Err(reason) => {
                    unavailable.push(json!({"source":row.name,"path":frame.path,"reason":reason}));
                    continue;
                }
            };
            let rel = child_path(&frame.path, &entry.name);
            if rel.len() > 4096 {
                continue;
            }
            let Ok(path) = under(root, &rel) else {
                continue;
            };
            let rev = revision(&path);
            if entry.directory {
                if pos.frames.len() >= 32 {
                    unavailable.push(json!({"source":row.name,"path":rel,"reason":"depth_limit"}));
                } else {
                    pos.frames.push(Frame {
                        path: rel,
                        after: Vec::new(),
                        revision: rev,
                    });
                }
            } else {
                pos.file = Some((rel, 0, rev));
                break;
            }
        }
        if pos.frames.is_empty() && pos.file.is_none() {
            pos.source += 1;
        }
    }
    let next = if pos.source < rows.len() || pos.bundled < bundled.len() {
        Some(search_cursor(version, &scope, pos))
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
