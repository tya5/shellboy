//! Bounded, explicitly selected document extraction. No shell or implicit downloads.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_INPUT: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);
const XBERG_VERSION: &str = "1.3.10";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub text: String,
    pub location: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extracted {
    pub text: String,
    pub blocks: Vec<Block>,
    pub extractor: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    id: String,
    version: String,
    executable: PathBuf,
    argv: Vec<String>,
    extensions: Vec<String>,
}

/// Classify the extension without opening its contents or running an extractor.
/// Explicit adapters remain responsible for their own extension declarations.
pub(crate) fn supported(path: &Path, adapter: Option<&Path>) -> bool {
    adapter.is_some()
        || matches!(
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "" | "txt"
                | "md"
                | "markdown"
                | "text"
                | "rst"
                | "csv"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "log"
                | "pdf"
                | "docx"
                | "pptx"
                | "xlsx"
        )
}
pub(crate) fn native_text(path: &Path) -> bool {
    supported(path, None)
        && !matches!(
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "pdf" | "docx" | "pptx" | "xlsx"
        )
}

pub(crate) fn adapter_extensions(path: &Path) -> Result<Vec<String>, String> {
    let manifest: Manifest = serde_json::from_slice(&bounded_file(path, 64 * 1024)?)
        .map_err(|_| "extractor_failed: invalid adapter manifest".to_owned())?;
    if manifest.schema_version != 1
        || manifest.extensions.len() > 256
        || manifest.extensions.iter().any(|value| value.len() > 64)
    {
        return Err("extractor_failed: invalid adapter extension declarations".into());
    }
    Ok(manifest.extensions)
}

pub fn extract(
    path: &Path,
    backend: Option<&Path>,
    adapter: Option<&Path>,
) -> Result<Extracted, String> {
    let path = fs::canonicalize(path)
        .map_err(|_| "source_unavailable: cannot resolve input".to_string())?;
    let meta =
        fs::metadata(&path).map_err(|_| "source_unavailable: cannot stat input".to_string())?;
    if !meta.is_file() || meta.len() > MAX_INPUT {
        return Err("extractor_failed: input must be a file of at most 16 MiB".into());
    }
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if let Some(manifest) = adapter {
        return external_adapter(&path, &extension, manifest);
    }
    if matches!(extension.as_str(), "pdf" | "docx" | "pptx" | "xlsx") {
        return xberg(
            &path,
            backend.ok_or(
                "extractor_failed: PDF/Office requires --extractor with Xberg 1.3.10 executable",
            )?,
        );
    }
    if !matches!(
        extension.as_str(),
        "" | "txt"
            | "md"
            | "markdown"
            | "text"
            | "rst"
            | "csv"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "log"
    ) {
        return Err("extractor_failed: unsupported file format; use a registered adapter".into());
    }
    let bytes = bounded_file(&path, MAX_INPUT as usize)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| "extractor_failed: text input must be UTF-8".to_string())?;
    Ok(Extracted {
        blocks: vec![Block {
            text: text.clone(),
            location: json!({"unit":"document"}),
        }],
        text,
        extractor: "text:utf8-v1".into(),
    })
}

fn bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut data = Vec::new();
    fs::File::open(path)
        .map_err(|_| "source_unavailable: cannot open file")?
        .take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| "source_unavailable: cannot read file")?;
    if data.len() > limit {
        return Err("extractor_failed: file exceeds input limit".into());
    }
    Ok(data)
}

fn external_adapter(
    path: &Path,
    extension: &str,
    manifest_path: &Path,
) -> Result<Extracted, String> {
    let bytes = bounded_file(manifest_path, 64 * 1024)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "extractor_failed: invalid adapter manifest")?;
    if manifest.schema_version != 1
        || manifest.id.is_empty()
        || manifest.id.len() > 128
        || manifest.version.is_empty()
        || manifest.version.len() > 128
        || manifest.argv.len() > 64
        || !manifest
            .extensions
            .iter()
            .any(|e| e.eq_ignore_ascii_case(extension))
        || !manifest.argv.iter().any(|a| a == "{input}")
        || manifest.argv.iter().any(|a| a.len() > 8192)
    {
        return Err("extractor_failed: unsupported adapter manifest or input extension".into());
    }
    let executable = if manifest.executable.is_absolute() {
        manifest.executable.clone()
    } else {
        manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&manifest.executable)
    };
    let argv: Vec<OsString> = manifest
        .argv
        .iter()
        .map(|a| {
            if a == "{input}" {
                path.as_os_str().to_owned()
            } else {
                a.into()
            }
        })
        .collect();
    let output = run(&executable, &argv, TIMEOUT)?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        blocks: Vec<Block>,
    }
    let output: Output = serde_json::from_slice(&output)
        .map_err(|_| "extractor_failed: adapter must return JSON blocks")?;
    if output.blocks.is_empty() || output.blocks.len() > 100_000 {
        return Err("extractor_failed: invalid adapter block count".into());
    }
    assemble(
        output.blocks,
        format!(
            "adapter:{}:{}:{}",
            manifest.id,
            manifest.version,
            Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ),
    )
}

fn xberg(path: &Path, backend: &Path) -> Result<Extracted, String> {
    let version = run(
        backend,
        &["version".into(), "--format".into(), "json".into()],
        Duration::from_secs(5),
    )?;
    let version: Value = serde_json::from_slice(&version)
        .map_err(|_| "extractor_failed: invalid Xberg version response")?;
    if version.get("version").and_then(Value::as_str) != Some(XBERG_VERSION) {
        return Err("extractor_failed: supported Xberg version is 1.3.10".into());
    }
    let mut argv: Vec<OsString> = ["extract"].into_iter().map(Into::into).collect();
    argv.push(path.as_os_str().to_owned());
    argv.extend(
        [
            "--no-config-discovery",
            "--format",
            "json",
            "--content-format",
            "plain",
            "--ocr",
            "false",
            "--layout",
            "false",
            "--extract-pages",
            "true",
            "--page-markers",
            "false",
            "--extract-images",
            "false",
            "--chunk",
            "false",
            "--max-threads",
            "2",
        ]
        .into_iter()
        .map(Into::into),
    );
    let bytes = run(backend, &argv, TIMEOUT)?;
    let output: Value =
        serde_json::from_slice(&bytes).map_err(|_| "extractor_failed: invalid Xberg JSON")?;
    let result = output
        .get("result")
        .ok_or("extractor_failed: missing Xberg result")?;
    let content = result
        .get("content")
        .and_then(Value::as_str)
        .ok_or("extractor_failed: missing Xberg content")?;
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut blocks = Vec::new();
    if let Some(pages) = result.get("pages").and_then(Value::as_array) {
        for page in pages {
            let text = page
                .get("content")
                .and_then(Value::as_str)
                .ok_or("extractor_failed: invalid Xberg page content")?;
            let number = page
                .get("page_number")
                .and_then(Value::as_u64)
                .ok_or("extractor_failed: invalid Xberg page number")?;
            let unit = match extension.as_str() {
                "pptx" => "slide",
                "xlsx" => "sheet",
                "pdf" => "page",
                _ => "document_block",
            };
            let mut location = json!({"unit":unit,"number":number});
            if let Some(name) = page.get("sheet_name").and_then(Value::as_str) {
                location["name"] = name.into();
            }
            blocks.push(Block {
                text: text.into(),
                location,
            });
        }
    }
    if blocks.is_empty() {
        blocks.push(Block {
            text: content.into(),
            location: json!({"unit":"document"}),
        });
    }
    if let Some(warnings) = result.get("processing_warnings") {
        blocks[0].location["processing_warnings"] = warnings.clone();
    }
    assemble(blocks, format!("xberg:{XBERG_VERSION}"))
}

fn assemble(blocks: Vec<Block>, extractor: String) -> Result<Extracted, String> {
    let mut text = String::new();
    for block in &blocks {
        if text
            .len()
            .saturating_add(block.text.len())
            .saturating_add(1)
            > MAX_OUTPUT
        {
            return Err("extractor_failed: extracted content exceeds 16 MiB".into());
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&block.text);
    }
    Ok(Extracted {
        text,
        blocks,
        extractor,
    })
}

async fn read_bounded(stream: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    stream
        .take(limit as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|_| "extractor_failed: output read failed".to_string())?;
    if data.len() > limit {
        return Err("extractor_failed: subprocess output limit exceeded".into());
    }
    Ok(data)
}

fn run(executable: &Path, argv: &[OsString], deadline: Duration) -> Result<Vec<u8>, String> {
    // This synchronous adapter may be called from a standalone runtime's blocking
    // worker. Keep its finite child runtime outside any entered Tokio context.
    let executable = executable.to_owned();
    let argv = argv.to_owned();
    std::thread::Builder::new()
        .name("shellboy-extractor".into())
        .spawn(move || run_isolated(&executable, &argv, deadline))
        .map_err(|_| "extractor_failed: cannot start subprocess supervisor")?
        .join()
        .map_err(|_| "extractor_failed: subprocess supervisor failed")?
}

fn run_isolated(
    executable: &Path,
    argv: &[OsString],
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    let executable = fs::canonicalize(executable)
        .map_err(|_| "extractor_failed: executable path unavailable")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "extractor_failed: worker runtime unavailable")?;
    runtime.block_on(async {
        let mut command = tokio::process::Command::new(executable);
        command
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // External document tools must not inherit API credentials or user extraction settings.
        let safe_env: Vec<_> = std::env::vars_os()
            .filter(|(k, _)| {
                matches!(
                    k.to_str(),
                    Some(
                        "PATH"
                            | "SystemRoot"
                            | "WINDIR"
                            | "TMPDIR"
                            | "TEMP"
                            | "TMP"
                            | "LANG"
                            | "LC_ALL"
                    )
                )
            })
            .collect();
        command.env_clear().envs(safe_env).env("NO_COLOR", "1");
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|_| "extractor_failed: could not start executable")?;
        #[cfg(unix)]
        let child_id = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or("extractor_failed: stdout unavailable")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("extractor_failed: stderr unavailable")?;
        let result = tokio::time::timeout(deadline, async {
            let (out, _, status) = tokio::try_join!(
                read_bounded(stdout, MAX_OUTPUT),
                read_bounded(stderr, MAX_STDERR),
                async {
                    child
                        .wait()
                        .await
                        .map_err(|_| "extractor_failed: could not reap subprocess".to_string())
                }
            )?;
            if !status.success() {
                return Err("extractor_failed: external tool exited unsuccessfully".into());
            }
            Ok(out)
        })
        .await;
        let result = result.unwrap_or_else(|_| Err("extractor_failed: subprocess timeout".into()));
        if result.is_err() {
            #[cfg(unix)]
            if let Some(pid) = child_id {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
        result
    })
}

#[cfg(all(test, unix))]
mod boundary_tests {
    use super::*;
    #[test]
    fn deadline_and_output_limits_reap_subprocesses() {
        let now = std::time::Instant::now();
        let err = run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 10".into()],
            Duration::from_millis(40),
        )
        .unwrap_err();
        assert!(err.contains("timeout"));
        assert!(now.elapsed() < Duration::from_secs(3));
        let err = run(
            Path::new("/bin/sh"),
            &["-c".into(), "yes x".into()],
            Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(err.contains("output limit"));
        let err = run(
            Path::new("/bin/sh"),
            &["-c".into(), "yes error >&2".into()],
            Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(err.contains("output limit"));
        let err = run(
            Path::new("/bin/sh"),
            &["-c".into(), "echo private_diagnostic >&2; exit 12".into()],
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.contains("unsuccessfully"));
        assert!(!err.contains("private_diagnostic"));
    }
}
