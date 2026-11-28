use super::{BUNDLED, Registration, category, hash};
use crate::{
    config::{PathEnvironment, resolve_cli_data_dir, resolve_config_path},
    config_edit,
    protocol::{ApiError, ErrorCode, PROTOCOL_VERSION},
    rag::{IndexMode, RagCommand, RagOptions, SourceCommand},
};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}

fn export(data: &Path) -> Result<(PathBuf, File), ApiError> {
    let parent = data.join("docs-export");
    fs::create_dir_all(&parent)
        .map_err(|_| error(ErrorCode::IoFailed, "Cannot create docs export directory"))?;
    if fs::symlink_metadata(&parent).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(error(
            ErrorCode::IoFailed,
            "Docs export directory cannot be a symlink",
        ));
    }
    let lock = OpenOptions::new()
        .write(true)
        .read(true)
        .create(true)
        .truncate(false)
        .open(parent.join("export.lock"))
        .map_err(|_| error(ErrorCode::IoFailed, "Cannot open docs export lock"))?;
    lock.try_lock_exclusive().map_err(|_| {
        error(
            ErrorCode::OperationConflict,
            "Another docs export is active",
        )
    })?;
    let root = parent.join("shellboy");
    fs::create_dir_all(&root)
        .map_err(|_| error(ErrorCode::IoFailed, "Cannot create bundled guide export"))?;
    if fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(error(
            ErrorCode::IoFailed,
            "Bundled guide export cannot be a symlink",
        ));
    }
    let docs = super::documents();
    let wanted: std::collections::BTreeMap<_, _> = docs
        .iter()
        .map(|d| {
            (
                format!("guide-{}.md", hash(d.id.as_bytes())),
                d.text.as_bytes(),
            )
        })
        .collect();
    for (name, body) in &wanted {
        let path = root.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            return Err(error(
                ErrorCode::IoFailed,
                "Bundled guide export contains a non-file destination",
            ));
        }
        let state = config_edit::read_state(&path)?;
        if state.bytes != *body {
            config_edit::replace_atomically(
                &path,
                &state,
                body,
                |_| Ok(()),
                config_edit::replace_file,
            )?;
        }
    }
    for entry in fs::read_dir(&root)
        .map_err(|_| error(ErrorCode::IoFailed, "Cannot inspect bundled guide export"))?
        .take(201)
    {
        let entry =
            entry.map_err(|_| error(ErrorCode::IoFailed, "Cannot inspect an export entry"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name
            .strip_prefix("guide-")
            .and_then(|s| s.strip_suffix(".md"))
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            && !wanted.contains_key(name)
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            fs::remove_file(entry.path())
                .map_err(|_| error(ErrorCode::IoFailed, "Cannot retire old exported guide"))?;
        }
    }
    Ok((root, lock))
}

fn preview(row: Option<&Registration>) -> Value {
    let Some(row) = row else {
        let docs = BUNDLED.docs.iter().filter(|d| !category(&d.id));
        return json!({"source":"shellboy","files":docs.clone().count(),"bytes":docs.map(|d|d.text.len()).sum::<usize>(),"complete":true});
    };
    let mut pending = vec![(row.path.clone(), 0)];
    let mut examined = 0;
    let mut files = 0;
    let mut bytes = 0u64;
    let mut complete = true;
    'walk: while let Some((path, depth)) = pending.pop() {
        if depth > 32 || fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            complete = false;
            continue;
        }
        let Ok(dir) = fs::read_dir(path) else {
            complete = false;
            continue;
        };
        for entry in dir {
            if examined >= 200 {
                complete = false;
                break 'walk;
            }
            examined += 1;
            let Ok(entry) = entry else {
                complete = false;
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                complete = false;
                continue;
            };
            if kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() && entry.path().extension().is_some_and(|s| s == "md") {
                match entry.metadata() {
                    Ok(meta) => {
                        files += 1;
                        bytes = bytes.saturating_add(meta.len());
                    }
                    Err(_) => complete = false,
                }
            }
        }
    }
    json!({"source":row.name,"files":files,"bytes":bytes,"complete":complete})
}

pub async fn execute(
    names: Vec<String>,
    all: bool,
    dry_run: bool,
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    cwd: &Path,
) -> Value {
    match index(names, all, dry_run, config, data, cwd).await {
        Ok(result) => json!({"protocol_version":PROTOCOL_VERSION,"ok":true,"result":result}),
        Err(error) => json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error":error}),
    }
}
async fn index(
    names: Vec<String>,
    all: bool,
    dry_run: bool,
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    cwd: &Path,
) -> Result<Value, ApiError> {
    if (names.is_empty() != all) || names.len() > 201 {
        return Err(error(
            ErrorCode::InvalidArgument,
            "Choose --source NAME or --all",
        ));
    }
    let env = PathEnvironment::current();
    let path = resolve_config_path(config.as_deref(), cwd, &env)
        .map_err(|e| error(ErrorCode::ConfigInvalid, e.to_string()))?;
    let state = config_edit::read_state(&path)?;
    let rows = config_edit::parse_file(&state.bytes, &path)
        .map_err(|e| error(ErrorCode::ConfigInvalid, e))?
        .docs
        .sources;
    if names
        .iter()
        .any(|name| name != "shellboy" && !rows.iter().any(|row| row.name == *name))
    {
        return Err(error(ErrorCode::TargetUnavailable, "Unknown docs source"));
    }
    let mut selected = Vec::new();
    if all || names.iter().any(|n| n == "shellboy") {
        selected.push(("shellboy", None));
    }
    for row in &rows {
        if all || names.contains(&row.name) {
            selected.push((row.name.as_str(), Some(row)));
        }
    }
    if dry_run {
        return Ok(
            json!({"dry_run":true,"sources":selected.iter().map(|(_,row)|preview(*row)).collect::<Vec<_>>(),"embedding_may_charge":true,"counts":"docs_only_not_rag_changes"}),
        );
    }
    let data = resolve_cli_data_dir(data.as_deref(), &env, cwd)
        .map_err(|e| error(ErrorCode::ConfigInvalid, e.to_string()))?;
    let invoke = |command| {
        crate::rag::execute_with_text(
            RagOptions { db: None, command },
            config.clone(),
            Some(data.clone()),
            cwd,
            None,
        )
    };
    let init = invoke(RagCommand::Init).await;
    if init["ok"] != true {
        return Ok(
            json!({"dry_run":false,"initialization":init,"sources":[],"partial_failure":true,"embedding_may_charge":true}),
        );
    }
    let mut results = Vec::new();
    let mut partial = false;
    for (name, row) in selected {
        let exported;
        let root = if let Some(row) = row {
            row.path.clone()
        } else {
            match export(&data) {
                Ok(value) => {
                    exported = value;
                    exported.0.clone()
                }
                Err(error) => {
                    partial = true;
                    results.push(json!({"source":name,"export":{"ok":false,"error":error}}));
                    continue;
                }
            }
        };
        let registered = invoke(RagCommand::Source {
            command: SourceCommand::Add {
                path: root,
                name: name.into(),
                exclude: Vec::new(),
                clear_excludes: false,
            },
        })
        .await;
        if let Some(id) = registered["result"]["source_id"]
            .as_str()
            .filter(|_| registered["ok"] == true)
        {
            let indexed = invoke(RagCommand::Index {
                source: Some(id.into()),
                dry_run: false,
                mode: IndexMode::Semantic,
            })
            .await;
            partial |= indexed["ok"] != true || indexed["result"]["partial_failure"] == true;
            results.push(json!({"source":name,"registration":registered,"index":indexed}));
        } else {
            partial = true;
            results.push(json!({"source":name,"registration":registered}));
        }
    }
    Ok(
        json!({"dry_run":false,"initialization":init,"sources":results,"partial_failure":partial,"embedding_may_charge":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preview_never_creates_the_store_or_export() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("not-created");
        let result = execute(
            vec!["shellboy".into()],
            false,
            true,
            Some(root.path().join("config.toml")),
            Some(data.clone()),
            root.path(),
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        assert!(result["result"]["sources"][0]["files"].as_u64().unwrap() > 0);
        assert_eq!(result["result"]["embedding_may_charge"], true);
        assert!(!data.exists());
    }
}
