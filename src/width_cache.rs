//! Disposable terminal width observations; failures always fall back to probing.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn identity(
    program: Option<&str>,
    version: Option<&str>,
    term: Option<&str>,
) -> Option<String> {
    let valid = |s: &str| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
    if let Some(p) = program.filter(|s| valid(s)) {
        let v = version.filter(|s| valid(s)).unwrap_or("");
        Some(format!("program:{p}:{}:{v}", v.len()))
    } else {
        term.filter(|s| valid(s)).map(|t| format!("term:{t}"))
    }
}

pub fn current_identity() -> Option<String> {
    identity(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM_PROGRAM_VERSION").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

fn path(data: &Path, identity: &str) -> PathBuf {
    let hash = Sha256::digest(identity.as_bytes());
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    data.join(format!(".terminal-width-{hex}"))
}

pub fn read(data: &Path, identity: &str) -> Option<u8> {
    use std::io::Read;
    let cache = path(data, identity);
    let metadata = std::fs::symlink_metadata(&cache).ok()?;
    if !metadata.is_file() || metadata.len() != 2 {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(cache)
        .ok()?
        .take(3)
        .read_to_end(&mut bytes)
        .ok()?;
    match bytes.as_slice() {
        b"1\n" => Some(1),
        b"2\n" => Some(2),
        _ => None,
    }
}

/// Called only with owner exclusion held. Rename avoids partial cache records.
pub fn write(data: &Path, identity: &str, width: u8) -> std::io::Result<()> {
    use std::io::Write;
    if width != 1 && width != 2 {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let target = path(data, identity);
    let temp = data.join(format!(".terminal-width-tmp-{}", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        writeln!(file, "{width}")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_and_cache_are_separate_and_bounded() {
        assert_eq!(identity(None, None, None), None);
        assert_ne!(
            identity(Some("a"), Some("1"), None),
            identity(Some("a"), Some("2"), None)
        );
        assert_ne!(
            identity(Some("a"), Some("1"), None),
            identity(None, None, Some("a"))
        );
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "one", 1).unwrap();
        assert_eq!(read(root.path(), "one"), Some(1));
        assert_eq!(read(root.path(), "two"), None);
        write(root.path(), "one", 2).unwrap();
        assert_eq!(read(root.path(), "one"), Some(2));
    }
}
