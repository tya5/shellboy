//! Source-relative glob selection and bounded, deletion-safe traversal.
use super::*;
use globset::{GlobBuilder, GlobMatcher};
use std::collections::BTreeMap;

const MAX_VISITED: usize = 100_000;
const MAX_TARGETS: usize = 10_000;
const SAMPLE_LIMIT: usize = 20;
pub(super) fn validate_patterns(patterns: &[String]) -> Result<()> {
    if patterns.len() > 64 {
        return Err(err(
            "input_limit",
            "At most 64 source exclusion patterns are allowed",
        ));
    }
    for pattern in patterns {
        if pattern.is_empty() || pattern.len() > 1024 || pattern.contains('\0') {
            return Err(err(
                "invalid_argument",
                "Exclusion patterns must contain 1..=1024 bytes and no NUL",
            ));
        }
        GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|_| err("invalid_argument", "Invalid source exclusion glob"))?;
    }
    Ok(())
}
struct Rule {
    matcher: GlobMatcher,
    basename: bool,
}
#[derive(Default, Serialize)]
pub(super) struct Scan {
    #[serde(skip)]
    pub(super) files: Vec<(String, PathBuf)>,
    pub(super) excluded: usize,
    pub(super) unsupported: usize,
    pub(super) symlinks: usize,
    pub(super) targets: usize,
    pub(super) target_examples: Vec<String>,
    pub(super) excluded_examples: Vec<Value>,
    pub(super) unsupported_examples: Vec<String>,
    pub(super) unsupported_by_extension: BTreeMap<String, usize>,
    pub(super) examples_limit: usize,
    #[serde(skip)]
    visited: usize,
}
impl Scan {
    fn excluded(&mut self, key: &str, reason: &str) {
        self.excluded += 1;
        if self.excluded_examples.len() < SAMPLE_LIMIT {
            self.excluded_examples
                .push(json!({"key":key,"reason":reason}));
        }
    }
}
fn relative(path: &Path, root: &Path) -> Result<String> {
    let path = path
        .strip_prefix(root)
        .ok()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| path.file_name().map(Path::new).unwrap_or(path));
    path.to_str()
        .map(|value| {
            if cfg!(windows) {
                value.replace('\\', "/")
            } else {
                value.to_owned()
            }
        })
        .ok_or_else(|| err("invalid_argument", "Source filenames must be Unicode"))
}
pub(super) fn collect(root: &Path, database: &Path, patterns: &[String]) -> Result<Scan> {
    validate_patterns(patterns)?;
    let rules = patterns
        .iter()
        .map(|pattern| {
            Ok(Rule {
                matcher: GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .map_err(|_| err("invalid_argument", "Invalid source exclusion glob"))?
                    .compile_matcher(),
                basename: !pattern.contains('/'),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let database = std::fs::canonicalize(database).map_err(|_| {
        err(
            "db_not_found",
            "Cannot resolve the active knowledge database",
        )
    })?;
    let mut own = vec![database.clone(), database.with_extension("rag-lock")];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = database.as_os_str().to_owned();
        name.push(suffix);
        own.push(PathBuf::from(name));
    }
    let mut scan = Scan {
        examples_limit: SAMPLE_LIMIT,
        ..Default::default()
    };
    walk(root, root, &own, &rules, 0, &mut scan)?;
    Ok(scan)
}
fn walk(
    path: &Path,
    root: &Path,
    own: &[PathBuf],
    rules: &[Rule],
    depth: usize,
    scan: &mut Scan,
) -> Result<()> {
    if depth == 0 {
        scan.visited = 1;
    }
    if depth > 64 || scan.visited > MAX_VISITED {
        return Err(err(
            "input_limit",
            "Source traversal exceeds 64 levels or 100000 entries; deletion detection was skipped",
        ));
    }
    let key = relative(path, root)?;
    if own.iter().any(|candidate| candidate == path) {
        scan.excluded(&key, "active_knowledge_database");
        return Ok(());
    }
    // An explicitly registered hidden root is still selected; only entries
    // underneath it receive the default dot-entry exclusion.
    if path != root
        && path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.starts_with('.'))
    {
        scan.excluded(&key, "dot_entry");
        return Ok(());
    }
    // Only the root needs metadata before matching: descendants excluded by a
    // glob should not require a metadata read just to be skipped.
    let unavailable = || {
        err(
            "source_unavailable",
            "Source traversal failed; deletion detection was skipped",
        )
    };
    let root_metadata = if path == root {
        Some(std::fs::symlink_metadata(path).map_err(|_| unavailable())?)
    } else {
        None
    };
    // A symlink or special-file root is unavailable, not an empty source. Check
    // before globs so an excluded basename cannot turn this into a complete walk.
    if root_metadata
        .as_ref()
        .is_some_and(|meta| !meta.is_dir() && !meta.is_file())
    {
        return Err(unavailable());
    }
    if !root_metadata.as_ref().is_some_and(|meta| meta.is_dir())
        && rules.iter().any(|rule| {
            rule.matcher.is_match(&key)
                || (rule.basename
                    && path
                        .file_name()
                        .is_some_and(|name| rule.matcher.is_match(Path::new(name))))
        })
    {
        scan.excluded(&key, "source_glob");
        return Ok(());
    }
    let meta = match root_metadata {
        Some(meta) => meta,
        None => std::fs::symlink_metadata(path).map_err(|_| unavailable())?,
    };
    if meta.file_type().is_symlink() {
        scan.symlinks += 1;
        return Ok(());
    }
    if meta.is_dir() {
        let remaining = MAX_VISITED.saturating_sub(scan.visited);
        let mut entries = std::fs::read_dir(path)?
            .take(remaining + 1)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if entries.len() > remaining {
            return Err(err(
                "input_limit",
                "Source directory exceeds 100000 entries; deletion detection was skipped",
            ));
        }
        scan.visited += entries.len();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            walk(&entry.path(), root, own, rules, depth + 1, scan)?;
        }
    } else if meta.is_file() {
        let supported = extract::supported(path);
        if !supported {
            scan.unsupported += 1;
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let extension = if scan.unsupported_by_extension.contains_key(&extension)
                || scan.unsupported_by_extension.len() < 64
            {
                extension
            } else {
                "<other>".into()
            };
            *scan.unsupported_by_extension.entry(extension).or_default() += 1;
            if scan.unsupported_examples.len() < SAMPLE_LIMIT {
                scan.unsupported_examples.push(key);
            }
            return Ok(());
        }
        if scan.files.len() >= MAX_TARGETS {
            return Err(err(
                "input_limit",
                "Source exceeds 10000 supported files; deletion detection was skipped",
            ));
        }
        if scan.target_examples.len() < SAMPLE_LIMIT {
            scan.target_examples.push(key.clone());
        }
        scan.targets += 1;
        scan.files.push((key, path.into()));
    }
    Ok(())
}
