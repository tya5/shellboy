//! UTF-8 text input only. Format conversion belongs to external workflows.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, io::Read, path::Path};

const MAX_INPUT: usize = 16 * 1024 * 1024;
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

/// Classify without reading contents or launching external programs.
pub(crate) fn supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|v| v.to_str())
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
    )
}
pub fn extract(path: &Path) -> Result<Extracted, String> {
    if !fs::metadata(path)
        .map_err(|_| "source_unavailable: cannot stat input")?
        .is_file()
    {
        return Err("extractor_failed: input must be a regular file".into());
    }
    let file = fs::File::open(path).map_err(|_| "source_unavailable: cannot open input")?;
    if !file
        .metadata()
        .map_err(|_| "source_unavailable: cannot stat input")?
        .is_file()
    {
        return Err("extractor_failed: input must be a regular file".into());
    }
    if !supported(path) {
        return Err("extractor_failed: convert externally and register using rag ingest".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_INPUT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "source_unavailable: cannot read input")?;
    if bytes.len() > MAX_INPUT {
        return Err("extractor_failed: text input exceeds 16 MiB".into());
    }
    let text =
        String::from_utf8(bytes).map_err(|_| "extractor_failed: text input must be UTF-8")?;
    Ok(Extracted {
        blocks: vec![Block {
            text: text.clone(),
            location: json!({"unit":"document"}),
        }],
        text,
        extractor: "text:utf8-v1".into(),
    })
}
