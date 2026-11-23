//! Client-side JSONL export. Only a complete, synced file is published.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::Path;
use tempfile::NamedTempFile;

struct ExportFile {
    file: NamedTempFile,
    hash: Sha256,
    lines: u64,
}

#[derive(Serialize)]
struct Trailer {
    #[serde(rename = "type")]
    kind: &'static str,
    lines: u64,
    sha256: String,
}

impl ExportFile {
    fn new(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(Self {
            file: NamedTempFile::new_in(parent)?,
            hash: Sha256::new(),
            lines: 0,
        })
    }

    fn line(&mut self, record: &impl Serialize) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        self.file.write_all(&bytes)?;
        self.hash.update(bytes);
        self.lines += 1;
        Ok(())
    }

    fn publish(mut self, path: &Path) -> io::Result<Trailer> {
        let trailer = Trailer {
            kind: "trailer",
            lines: self.lines,
            sha256: format!("{:x}", self.hash.finalize()),
        };
        serde_json::to_writer(&mut self.file, &trailer)?;
        self.file.write_all(b"\n")?;
        self.file.as_file().sync_all()?;
        self.file.persist(path).map_err(|error| error.error)?;
        Ok(trailer)
    }
}

#[cfg(test)]
mod tests;
