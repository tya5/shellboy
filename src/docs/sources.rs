use super::*;
use crate::input_path::{FOLDER_WALK_MAX_DEPTH, FOLDER_WALK_MAX_ENTRIES};
use crate::{
    config::{PathEnvironment, resolve_config_path},
    config_edit,
    protocol::{ApiError, ConfigChange, ErrorCode, PROTOCOL_VERSION},
};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

pub(crate) const DOCS_LIST_MAX: usize = 200;
pub(crate) const DOCS_SEARCH_MAX: usize = 100;
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
        && name != "shellboy"
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
impl DocsConfig {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        let mut names = HashSet::new();
        if self.sources.iter().any(|row| {
            !valid_name(&row.name)
                || !row.path.is_absolute()
                || row.path.to_str().is_none_or(|p| p.len() > 4096)
                || !names.insert(&row.name)
        }) {
            return Err(
                "Expected unique, non-reserved names and absolute UTF-8 paths (up to 4096 bytes)",
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

pub fn configured_response(command: DocsRead, explicit: Option<&Path>, cwd: &Path) -> Value {
    // Explicit bundled scope is also the recovery route for broken config.
    let bundled_only = match &command {
        DocsRead::Get { id, sources, .. } => {
            is_bundled(id) && sources.iter().all(|s| s == "shellboy")
        }
        DocsRead::List {
            parent, sources, ..
        }
        | DocsRead::Search {
            parent, sources, ..
        } => {
            (parent.as_deref().is_some_and(is_bundled) || !sources.is_empty())
                && sources.iter().all(|s| s == "shellboy")
        }
    };
    envelope(if bundled_only {
        browse(command, &[]).map_err(browse_error)
    } else {
        load_sources(explicit, cwd)
            .and_then(|(_, _, settings)| browse(command, &settings.sources).map_err(browse_error))
    })
}

pub fn source_response(command: SourceCommand, explicit: Option<&Path>, cwd: &Path) -> Value {
    envelope(
        load_sources(explicit, cwd).and_then(|(path, state, settings)| {
            registration_command(command, settings, &path, cwd, &state.hash)
        }),
    )
}

fn envelope(result: Result<Value, ApiError>) -> Value {
    match result {
        Ok(result) => json!({"protocol_version":PROTOCOL_VERSION,"ok":true,"result":result}),
        Err(error) => json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error":error}),
    }
}

pub(super) fn load_sources(
    explicit: Option<&Path>,
    cwd: &Path,
) -> Result<(PathBuf, config_edit::FileState, DocsConfig), ApiError> {
    let path = resolve_config_path(explicit, cwd, &PathEnvironment::current())
        .map_err(|e| refusal(ErrorCode::ConfigInvalid, e.to_string()))?;
    let state = config_edit::read_state(&path)?;
    let settings = config_edit::parse_file(&state.bytes, &path)
            .map_err(|_| refusal(ErrorCode::ConfigInvalid,
                "Cannot load docs sources. For bundled recovery guides use docs list --source shellboy or docs get --id shellboy/configuration"))?
            .docs;
    Ok((path, state, settings))
}

fn is_bundled(id: &str) -> bool {
    id.split('/').next() == Some("shellboy")
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
    (
        title
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| fallback.into()),
        summary.unwrap_or_default(),
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
    for name in names.iter().skip(offset).take(DOCS_LIST_MAX) {
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
        let mut walk = FolderWalk::default();
        walk.enter(String::new(), &root);
        let mut directories = BTreeMap::new();
        for _ in 0..=FOLDER_WALK_MAX_ENTRIES {
            match walk.step(&root, &mut directories)? {
                WalkStep::Done => return Ok(()),
                WalkStep::Directory(rel, path) => walk.enter(rel, &path),
                WalkStep::File(_, path) => {
                    prefix(&path, 0, TEXT_WORK)?;
                    count += 1;
                    bytes = bytes.saturating_add(fs::metadata(path).map_err(io_error)?.len());
                }
                WalkStep::Unavailable(_, reason) => return Err(reason),
                WalkStep::Advanced => {}
            }
        }
        Err("work_limit".into())
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

fn browse(command: DocsRead, rows: &[Registration]) -> Result<Value, String> {
    let version = hash(serde_json::to_vec(&json!([BUNDLED.version, rows])).expect("version"));
    let version = version.as_str();
    if let DocsRead::Get { id, sources, .. } = &command {
        selected(rows, sources)?;
        let name = id.split('/').next().unwrap_or_default();
        if !included(sources, name) {
            return Err("target_unavailable: Guide is outside the selected sources".into());
        }
    }
    match command {
        DocsRead::Get {
            id,
            sources,
            section,
            limit_bytes,
            cursor,
        } if !is_bundled(&id) => {
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
        DocsRead::List {
            parent,
            sources,
            limit: count,
            cursor,
        } => {
            limit(count, DOCS_LIST_MAX)?;
            selected(rows, &sources)?;
            let scope = hash(serde_json::to_vec(&json!(["list", parent, sources])).expect("scope"));
            let mut items = Vec::new();
            if let Some(id) = &parent {
                if is_bundled(id) {
                    if included(&sources, "shellboy") {
                        items = bundled_list(Some(id))?;
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
            }
            let registered = rows
                .iter()
                .filter(|r| parent.is_none() && included(&sources, &r.name));
            let offset = read_cursor(cursor.as_deref(), version, &scope)?;
            let total = items.len() + registered.clone().count();
            if offset > total {
                return Err("cursor_expired: Offset is outside list".into());
            }
            let end = (offset + count).min(total);
            let registered_skip = offset.saturating_sub(items.len());
            let mut page: Vec<_> = items.into_iter().skip(offset).take(count).collect();
            page.extend(
                registered
                    .skip(registered_skip)
                    .take(count - page.len())
                    .map(|row| {
                        branch(
                            &row.name,
                            canonical_root(&row.path).is_ok_and(|p| fs::read_dir(p).is_ok()),
                            version,
                        )
                    }),
            );
            Ok(paginated(
                json!({"version":version,"items":page}),
                (end < total).then(|| next_cursor(version, &scope, end)),
            ))
        }
        DocsRead::Search {
            query,
            parent,
            sources,
            limit: count,
            cursor,
        } => search(query, parent, sources, count, cursor, rows, version),
        DocsRead::Get {
            id,
            sources,
            section,
            limit_bytes,
            cursor,
        } => bundled_get(id, sources, section, limit_bytes, cursor),
    }
}
fn included(filters: &[String], name: &str) -> bool {
    filters.is_empty() || filters.iter().any(|s| s == name)
}
pub(super) fn selected(rows: &[Registration], filters: &[String]) -> Result<(), String> {
    if filters
        .iter()
        .any(|name| name != "shellboy" && !rows.iter().any(|row| &row.name == name))
    {
        return Err("target_unavailable: Unknown docs source filter".into());
    }
    Ok(())
}
fn bundled_list(parent: Option<&str>) -> Result<Vec<Value>, String> {
    let bundled = &*BUNDLED;
    if let Some(id) = parent {
        find(&bundled.docs, id)?;
    }
    Ok(bundled
        .docs
        .iter()
        .filter(|doc| doc.parent_id == parent.unwrap_or_default())
        .map(|doc| summary(doc, &bundled.version))
        .collect())
}

#[derive(Default, Serialize, Deserialize)]
struct FolderWalk {
    path: String,
    levels: Vec<(String, usize)>,
}

enum WalkStep {
    Directory(String, PathBuf),
    File(String, PathBuf),
    Unavailable(String, String),
    Advanced,
    Done,
}

impl FolderWalk {
    fn enter(&mut self, path: String, folder: &Path) {
        self.path = path;
        self.levels.push((revision(folder), 0));
    }

    fn leave(&mut self) {
        self.levels.pop();
        self.path.truncate(if self.levels.is_empty() {
            0
        } else {
            self.path.rfind('/').unwrap_or(0)
        });
    }

    fn step(
        &mut self,
        root: &Path,
        dirs: &mut BTreeMap<String, Vec<std::ffi::OsString>>,
    ) -> Result<WalkStep, String> {
        use std::collections::btree_map::Entry;
        let Some((saved, next)) = self.levels.last_mut() else {
            return Ok(WalkStep::Done);
        };
        let names = match dirs.entry(self.path.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let names = under(root, &self.path).and_then(|path| {
                    if revision(&path) != *saved {
                        return Err("cursor_expired: Docs folder changed".into());
                    }
                    sorted_names(&path)
                });
                match names {
                    Ok(names) => entry.insert(names),
                    Err(reason) if reason.starts_with("cursor_expired:") => return Err(reason),
                    Err(reason) => {
                        let path = self.path.clone();
                        self.leave();
                        return Ok(WalkStep::Unavailable(path, reason));
                    }
                }
            }
        };
        if *next > names.len() {
            return Err("cursor_expired: Invalid folder position".into());
        }
        let Some(name) = names.get(*next) else {
            dirs.remove(&self.path);
            self.leave();
            return Ok(if self.levels.is_empty() {
                WalkStep::Done
            } else {
                WalkStep::Advanced
            });
        };
        *next += 1;
        Ok(match child(root, &self.path, name) {
            Ok(Some((rel, path, false))) => WalkStep::File(rel, path),
            Ok(Some((rel, path, true))) if self.levels.len() <= FOLDER_WALK_MAX_DEPTH => {
                WalkStep::Directory(rel, path)
            }
            Ok(Some((rel, _, true))) => WalkStep::Unavailable(rel, "depth_limit".into()),
            Ok(None) => WalkStep::Advanced,
            Err(reason) => {
                WalkStep::Unavailable(child_path(&self.path, &name.to_string_lossy()), reason)
            }
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SearchMark {
    bundled: usize,
    source: usize,
    walk: FolderWalk,
    file: Option<(String, usize, usize, String)>,
}

fn hit(
    id: &str,
    title: &str,
    parent: &str,
    text: &str,
    needle: &str,
    offset: usize,
    scanned: usize,
    version: &str,
) -> Option<Value> {
    let lowered = text.to_lowercase();
    let overlap = text.floor_char_boundary(scanned.saturating_sub(offset).min(text.len()));
    let boundary = text[..overlap]
        .chars()
        .flat_map(char::to_lowercase)
        .map(char::len_utf8)
        .sum::<usize>();
    let mut matches = lowered
        .match_indices(needle)
        .filter(|(at, _)| at + needle.len() > boundary);
    let (at, _) = matches.next()?;
    let (start, end) = original_span(text, at, at + needle.len());
    let before = text.ceil_char_boundary(start.saturating_sub(96));
    let after = text.floor_char_boundary((end + 192).min(text.len()));
    Some(
        json!({"id":id,"title":title,"parent_id":parent,"version":version,"excerpt":&text[before..after],"spans":[{"start":offset+start,"end":offset+end}],"score":1 + matches.count()}),
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
) -> Result<Value, String> {
    limit(count, DOCS_SEARCH_MAX)?;
    if query.trim().is_empty() || query.len() > 4096 {
        return Err("invalid_argument: Query must contain 1 to 4096 bytes".into());
    }
    selected(rows, &sources)?;
    if let Some(id) = &parent {
        if id.len() > 4096 {
            return Err("invalid_argument: Docs parent ID exceeds 4096 bytes".into());
        }
        if is_bundled(id) {
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
    let mut pos: SearchMark = decode_cursor(cursor.as_deref(), version, &scope)?;
    let depth = if pos.walk.path == root_rel {
        Some(0)
    } else {
        let tail = if root_rel.is_empty() {
            Some(pos.walk.path.as_str())
        } else {
            pos.walk
                .path
                .strip_prefix(root_rel)
                .and_then(|tail| tail.strip_prefix('/'))
        };
        tail.map(|tail| tail.split('/').count())
    };
    if (!pos.walk.levels.is_empty()
        && (depth.is_none_or(|n| n > FOLDER_WALK_MAX_DEPTH || n + 1 != pos.walk.levels.len())
            || pos
                .file
                .as_ref()
                .is_some_and(|(name, _, _, _)| name.contains('/'))))
        || (pos.walk.levels.is_empty()
            && (!pos.walk.path.is_empty()
                || pos
                    .file
                    .as_ref()
                    .is_some_and(|(name, _, _, _)| name != root_rel)))
    {
        return Err("cursor_expired: Invalid search position".into());
    }
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    let mut unavailable = Vec::new();
    let mut work = 0;
    let bundled = &BUNDLED.docs;
    let mut bundled_hits: Vec<_> = bundled
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            included(&sources, "shellboy")
                && !category(&d.id)
                && parent
                    .as_ref()
                    .is_none_or(|p| d.id == *p || descendant(d, p, bundled))
        })
        .filter_map(|(index, d)| {
            let mut found = hit(
                &d.id,
                &d.title,
                &d.parent_id,
                &d.text,
                &needle,
                0,
                0,
                version,
            )?;
            found["content_hash"] = json!(BUNDLED.hashes[index].get_or_init(|| hash(&d.text)));
            Some(found)
        })
        .collect();
    bundled_hits.sort_by(|a, b| {
        b["score"]
            .as_u64()
            .cmp(&a["score"].as_u64())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    if pos.bundled > bundled_hits.len() || pos.source > rows.len() {
        return Err("cursor_expired: Invalid search position".into());
    }
    let bundled_count = bundled_hits.len();
    hits.extend(bundled_hits.into_iter().skip(pos.bundled).take(count));
    pos.bundled += hits.len();
    let cursor_fits = |mark: &SearchMark| next_cursor(version, &scope, mark).len() <= CURSOR_BYTES;
    let mut dirs = BTreeMap::new();
    let mut current_root = None;
    while pos.source < rows.len() && hits.len() < count && work < DOCS_SEARCH_MAX {
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
        if current_root
            .as_ref()
            .is_none_or(|(source, _)| *source != pos.source)
        {
            dirs.clear();
            match canonical_root(&row.path) {
                Ok(root) => current_root = Some((pos.source, root)),
                Err(_) => {
                    unavailable.push(branch(&row.name, false, version));
                    pos.walk = FolderWalk::default();
                    pos.file = None;
                    pos.source += 1;
                    work += 1;
                    continue;
                }
            }
        }
        let (_, root) = current_root.as_ref().expect("opened source root");
        if let Some((name, offset, scanned, rev)) = pos.file.take() {
            work += 1;
            let rel = child_path(&pos.walk.path, &name);
            let observation = under(root, &rel).and_then(|path| {
                let meta = fs::metadata(&path).map_err(io_error)?;
                Ok((path, meta))
            });
            let (path, meta) = match observation {
                Ok(value) => value,
                Err(reason) => {
                    unavailable.push(json!({"source":row.name,"path":rel,"reason":reason}));
                    pos.advance_source_if_idle();
                    continue;
                }
            };
            if revision(&path) != rev
                || offset > scanned
                || scanned as u64 > meta.len()
                || !meta.is_file()
                || !rel.ends_with(".md")
            {
                return Err("cursor_expired: Invalid or changed docs file".into());
            }
            let (text, more) = match prefix(&path, offset, TEXT_WORK) {
                Ok(value) => value,
                Err(reason) => {
                    unavailable.push(json!({"source":row.name,"path":rel,"reason":reason}));
                    pos.advance_source_if_idle();
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
            if let Some(h) = hit(&id, &title, dir, &text, &needle, offset, scanned, version) {
                hits.push(h);
            }
            if more {
                // Repeat only enough UTF-8 text to catch a match across chunks.
                let overlap = text.ceil_char_boundary(text.len().saturating_sub(query.len() * 4));
                if !pos.admit_file(
                    (name, offset + overlap, offset + text.len(), rev),
                    &cursor_fits,
                ) {
                    unavailable.push(long_path(&row.name, &rel));
                }
            } else {
                pos.advance_source_if_idle();
            }
            continue;
        }
        work += 1;
        if pos.walk.levels.is_empty() {
            match under(root, root_rel) {
                Ok(path) if path.is_dir() => {
                    if !pos.admit_folder(root_rel, &path, &cursor_fits) {
                        unavailable.push(long_path(&row.name, root_rel));
                        continue;
                    }
                }
                Ok(path) if path.is_file() && root_rel.ends_with(".md") => {
                    if !pos.admit_file((root_rel.into(), 0, 0, revision(&path)), &cursor_fits) {
                        unavailable.push(long_path(&row.name, root_rel));
                    }
                    continue;
                }
                _ => {
                    unavailable.push(branch(&row.name, false, version));
                    pos.source += 1;
                    continue;
                }
            }
        }
        match pos.walk.step(root, &mut dirs)? {
            WalkStep::Directory(rel, path) => {
                if !pos.admit_folder(&rel, &path, &cursor_fits) {
                    unavailable.push(long_path(&row.name, &rel));
                }
            }
            WalkStep::File(rel, path) => {
                let name = rel.rsplit('/').next().expect("guide filename");
                if !pos.admit_file((name.into(), 0, 0, revision(&path)), &cursor_fits) {
                    unavailable.push(long_path(&row.name, &rel));
                }
            }
            WalkStep::Unavailable(path, reason) => {
                unavailable.push(json!({"source":row.name,"path":path,"reason":reason}));
                pos.advance_source_if_idle();
            }
            WalkStep::Done => pos.source += 1,
            WalkStep::Advanced => {}
        }
    }
    let next = (pos.source < rows.len() || pos.bundled < bundled_count)
        .then(|| next_cursor(version, &scope, &pos));
    Ok(paginated(
        json!({"version":version,"mode":"keyword","items":hits,"unavailable":unavailable}),
        next,
    ))
}

impl SearchMark {
    fn admit_folder(&mut self, rel: &str, path: &Path, fits: &impl Fn(&Self) -> bool) -> bool {
        self.walk.enter(rel.into(), path);
        self.walk.levels.last_mut().expect("entered folder").1 = FOLDER_WALK_MAX_ENTRIES;
        if fits(self) {
            self.walk.levels.last_mut().expect("entered folder").1 = 0;
            return true;
        }
        self.walk.leave();
        self.advance_source_if_idle();
        false
    }

    fn admit_file(
        &mut self,
        mut file: (String, usize, usize, String),
        fits: &impl Fn(&Self) -> bool,
    ) -> bool {
        // Reserve the full integer widths at entry so a later chunk always fits.
        let (offset, scanned) = (file.1, file.2);
        (file.1, file.2) = (usize::MAX, usize::MAX);
        self.file = Some(file);
        if fits(self) {
            let file = self.file.as_mut().expect("admitted file");
            (file.1, file.2) = (offset, scanned);
            return true;
        }
        self.file = None;
        self.advance_source_if_idle();
        false
    }

    fn advance_source_if_idle(&mut self) {
        if self.walk.levels.is_empty() {
            self.source += 1;
        }
    }
}

fn long_path(source: &str, path: &str) -> Value {
    json!({"source":source,"path":path,"reason":"path_too_long"})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn deep_escaped_search_resumes_with_the_production_cursor_limit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        let component = "\"".repeat(180);
        let relative = [component.as_str(); 3].join("/");
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
            )
            .unwrap()
        };
        let first = search(None);
        assert_eq!(first["items"][0]["id"], format!("local/{relative}/a.md"));
        let cursor = first["next_cursor"].as_str().unwrap();
        let second = search(Some(cursor.into()));
        assert_eq!(second["items"][0]["id"], format!("local/{relative}/b.md"));
    }

    #[cfg(unix)]
    #[test]
    fn search_refuses_an_oversized_file_marker_and_keeps_its_sibling() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let path = root.path().canonicalize().unwrap();
        let component = "\x01".repeat(26);
        let relative = vec![component.as_str(); 26].join("/");
        let deep = path.join(&relative);
        fs::create_dir_all(&deep).unwrap();
        let oversized = "\x01".repeat(246) + ".md";
        fs::write(deep.join(&oversized), "cursor-witness").unwrap();
        fs::write(deep.join("a.md"), "cursor-witness").unwrap();
        let page = browse(
            DocsRead::Search {
                query: "cursor-witness".into(),
                parent: Some("local".into()),
                sources: vec!["local".into()],
                limit: 1,
                cursor: None,
            },
            &[Registration {
                name: "local".into(),
                path,
            }],
        )
        .unwrap();
        assert_eq!(
            page["unavailable"],
            json!([{"source":"local","path":format!("{relative}/{oversized}"),"reason":"path_too_long"}])
        );
        assert_eq!(page["items"][0]["id"], format!("local/{relative}/a.md"));
    }

    #[test]
    fn search_overlap_yields_crossing_match_but_not_a_duplicate() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        let needle = "match-token";
        let first = TEXT_WORK - needle.len() * 2;
        let crossing = TEXT_WORK - 3;
        let text =
            "x".repeat(first) + needle + &"x".repeat(crossing - first - needle.len()) + needle;
        fs::write(path.join("guide.md"), text).unwrap();
        let rows = [Registration {
            name: "local".into(),
            path,
        }];
        let run = |cursor| {
            browse(
                DocsRead::Search {
                    query: needle.into(),
                    parent: Some("local/guide.md".into()),
                    sources: vec!["local".into()],
                    limit: 1,
                    cursor,
                },
                &rows,
            )
            .unwrap()
        };
        let mut page = run(None);
        let mut spans = Vec::new();
        let mut seen = HashSet::new();
        loop {
            for hit in page["items"].as_array().unwrap() {
                assert_eq!(hit["score"], 1);
                spans.push(hit["spans"][0].clone());
            }
            let Some(cursor) = page["next_cursor"].as_str() else {
                break;
            };
            assert!(seen.insert(cursor.to_owned()));
            page = run(Some(cursor.into()));
        }
        assert_eq!(
            spans,
            [
                json!({"start":first,"end":first+needle.len()}),
                json!({"start":crossing,"end":crossing+needle.len()})
            ]
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
                DocsRead::List {
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
            DocsRead::Search {
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
        let manage = |command| source_response(command, Some(&config), &cwd);
        let add = || SourceCommand::Add {
            name: "recipes".into(),
            path: "skills".into(),
        };
        assert_eq!(manage(add())["result"]["written"], true);
        assert_eq!(manage(add())["result"]["written"], false);
        assert!(
            fs::read_to_string(&config)
                .unwrap()
                .contains("[[docs.sources]]")
        );
        let list = run(DocsRead::List {
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
            run(DocsRead::Get {
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
        let query = run(DocsRead::Search {
            parent: None,
            sources: vec!["recipes".into()],
            query: "frobnicate".into(),
            limit: 1,
            cursor: None,
        });
        assert_eq!(query["result"]["items"][0]["id"], "recipes/demo/SKILL.md");
        fs::rename(&folder, cwd.join("moved")).unwrap();
        let list = run(DocsRead::List {
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
            manage(SourceCommand::Remove {
                name: "recipes".into()
            })["ok"],
            true
        );
        assert!(cwd.join("moved/demo/SKILL.md").is_file());
    }
}
