//! Client-side JSONL export. Only a complete, synced file is published.
use crate::atomic_file::AtomicFile;
use crate::protocol::{
    ApiError, Command, ErrorCode, HistoryExportArgs, HistoryExportPage, Origin, PROTOCOL_VERSION,
    ThreadTarget,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, clap::Args)]
pub struct ExportOptions {
    #[arg(long = "thread")]
    pub thread_id: String,
    #[arg(long)]
    pub out: PathBuf,
}

pub async fn execute(
    options: ExportOptions,
    data_dir: &Path,
    origin: Option<Origin>,
) -> Result<Value, ApiError> {
    let target = Command::ThreadGet(ThreadTarget {
        thread_id: Some(options.thread_id),
    });
    let target = crate::cli::resolve_id_arguments(target, |kind, prefix| {
        crate::cli::resolve_owner_prefix(data_dir, origin.as_ref(), kind, prefix, false)
    })
    .await?;
    let Command::ThreadGet(ThreadTarget {
        thread_id: Some(thread_id),
    }) = &target
    else {
        unreachable!()
    };
    let mut args = HistoryExportArgs {
        thread_id: thread_id.clone(),
        cursor: None,
        limit: 32,
    };
    let call = |command| async {
        let response = crate::ipc::call(data_dir, command, None, origin.clone()).await?;
        if response.ok {
            response.result
        } else {
            return Err(response.error.unwrap_or_else(bad_response));
        }
        .ok_or_else(bad_response)
    };
    let thread = call(target).await?;
    let mut page: HistoryExportPage =
        serde_json::from_value(call(Command::HistoryExport(args.clone())).await?)
            .map_err(|_| bad_response())?;
    let mut file = ExportFile::new(&options.out).map_err(write_error)?;
    file.line(&json!({"type":"header","format":"shellboy-thread-export","version":1,"thread":thread,
        "exported_at":chrono::Utc::now().to_rfc3339(),"core_protocol":PROTOCOL_VERSION,"change_position":page.change_position})).map_err(write_error)?;
    loop {
        for record in page.items {
            file.line(&record).map_err(write_error)?;
        }
        let Some(next) = page.next_cursor else {
            break;
        };
        args.cursor = Some(next);
        page = serde_json::from_value(call(Command::HistoryExport(args.clone())).await?)
            .map_err(|_| bad_response())?;
    }
    let trailer = file
        .publish(&options.out, page.change_position)
        .map_err(write_error)?;
    let result = json!({"lines":trailer.lines,"sha256":trailer.sha256});
    #[cfg(windows)]
    let result = match crate::atomic_file::privacy_warning(&options.out) {
        Some(warning) => {
            let mut result = result;
            result["warning"] = json!(warning);
            result
        }
        None => result,
    };
    Ok(result)
}

fn bad_response() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidState,
        "Owner returned an invalid Thread export result",
    )
}
fn write_error(error: io::Error) -> ApiError {
    ApiError::new(
        ErrorCode::IoFailed,
        format!("Cannot write Thread export: {error}"),
    )
}

struct ExportFile {
    file: AtomicFile,
    hash: Sha256,
    lines: u64,
}

#[derive(Serialize)]
struct Trailer {
    #[serde(rename = "type")]
    kind: &'static str,
    lines: u64,
    sha256: String,
    change_position: u64,
}

impl ExportFile {
    fn new(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(Self {
            file: AtomicFile::new(
                parent.join(format!(".thread-export-{}.tmp", uuid::Uuid::new_v4())),
                true,
            )?,
            hash: Sha256::new(),
            lines: 0,
        })
    }

    fn line(&mut self, record: &impl Serialize) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        self.file.file().write_all(&bytes)?;
        self.hash.update(bytes);
        self.lines += 1;
        Ok(())
    }

    fn publish(mut self, path: &Path, change_position: u64) -> io::Result<Trailer> {
        let trailer = Trailer {
            kind: "trailer",
            lines: self.lines,
            change_position,
            sha256: digest_hex(&self.hash.finalize()),
        };
        serde_json::to_writer(self.file.file(), &trailer)?;
        self.file.file().write_all(b"\n")?;
        self.file.publish(path)?;
        Ok(trailer)
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests;
