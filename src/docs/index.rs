use super::{Registration, hash};
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
    {
        let entry =
            entry.map_err(|_| error(ErrorCode::IoFailed, "Cannot inspect an export entry"))?;
        let name = entry.file_name();
        let kind = entry
            .file_type()
            .map_err(|_| error(ErrorCode::IoFailed, "Cannot inspect export entry type"))?;
        let Some(name) = name.to_str() else {
            return Err(error(
                ErrorCode::IoFailed,
                "Unexpected entry in managed export",
            ));
        };
        let managed = name
            .strip_prefix("guide-")
            .and_then(|s| s.strip_suffix(".md"))
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
        // The root is passed to generic RAG. Unexpected material must not be
        // indexed, and is not ours to delete.
        if !managed || !kind.is_file() {
            return Err(error(
                ErrorCode::IoFailed,
                "Unexpected entry in managed export",
            ));
        }
        if !wanted.contains_key(name) {
            fs::remove_file(entry.path())
                .map_err(|_| error(ErrorCode::IoFailed, "Cannot retire old exported guide"))?;
        }
    }
    Ok((root, lock))
}

fn preview(row: Option<&Registration>) -> Value {
    match row {
        Some(row) => super::sources::preview(row),
        None => {
            let docs = super::documents();
            json!({"source":"shellboy","files":docs.len(),"bytes":docs.iter().map(|d|d.text.len()).sum::<usize>(),"complete":true})
        }
    }
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
        Ok(result) if result["partial_failure"] == true => {
            json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"result":result,"error":{"code":"partial_failure","message":"Docs indexing failed for one or more steps; inspect the step results"}})
        }
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
