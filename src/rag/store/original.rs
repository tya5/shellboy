use super::*;

fn decode_component(value: &str) -> Option<String> {
    let mut input = value.bytes();
    let mut output = Vec::new();
    while let Some(byte) = input.next() {
        output.push(if byte == b'%' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(output).ok()
}

fn locator(reference: &str) -> Option<(String, String)> {
    if reference.len() > 8192 {
        return None;
    }
    let (source, key) = reference.strip_prefix("file:")?.split_once('/')?;
    let source = decode_component(source)?;
    let key = key
        .split('/')
        .map(decode_component)
        .collect::<Option<Vec<_>>>()?
        .join("/");
    (!source.is_empty() && !key.is_empty() && file_ref(&source, &key) == reference)
        .then_some((source, key))
}

pub(in crate::rag) fn read(
    database: &Path,
    reference: &str,
    limit: u32,
    cursor: Option<&str>,
) -> Result<Value> {
    let (source, key) =
        locator(reference).ok_or_else(|| err("invalid_argument", "Invalid file ref"))?;
    let store = open(database, false)?;
    let unavailable = || err("original_unavailable", "Indexed original is unavailable");
    let (root, kind, observed, provenance): (String, String, String, String) = store.db.query_row(
        "SELECT s.root,s.kind,r.source_hash,r.provenance FROM sources s JOIN documents d ON d.source_id=s.id JOIN revisions r ON r.id=d.active_revision WHERE s.id=?1 AND d.key=?2",
        params![source, key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?.ok_or_else(unavailable)?;
    let path = if source == "@derived" {
        let value: Value = serde_json::from_str(&provenance).map_err(|_| unavailable())?;
        let path = PathBuf::from(value["original_path"].as_str().ok_or_else(unavailable)?);
        if !path.is_absolute() {
            return Err(unavailable());
        }
        path
    } else if kind == "file" {
        let root = Path::new(&root);
        if !root.is_absolute() || root.canonicalize().ok().as_deref() != Some(root) {
            return Err(unavailable());
        }
        let relative = Path::new(&key);
        if key
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
            || (cfg!(windows) && key.contains(['\\', ':']))
            || !relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(unavailable());
        }
        if root.is_file() {
            if root.file_name() != Some(relative.as_os_str()) {
                return Err(unavailable());
            }
            root.to_owned()
        } else {
            root.join(relative)
        }
    } else {
        return Err(unavailable());
    };
    let file = open_original(&path).map_err(|_| unavailable())?;
    let bytes = bounded_read_file(file).map_err(|error| {
        if error.code == "input_limit" {
            error
        } else {
            unavailable()
        }
    })?;
    let current = hash(&bytes);
    let mut result = original_page(&bytes, &format!("{reference}@{current}"), limit, cursor)?;
    result["ref"] = json!(reference);
    result["fingerprint"] = json!(current);
    result["matches_indexed"] = json!(observed == current);
    Ok(result)
}

#[cfg(unix)]
fn open_original(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    use std::path::Component;
    let mut parts = path.components().peekable();
    if parts.next() != Some(Component::RootDir) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    // Do not resolve any component through a symlink; each descent uses the opened parent.
    let mut current = std::fs::File::open("/")?;
    while let Some(Component::Normal(part)) = parts.next() {
        let part = std::ffi::CString::new(part.as_bytes())?;
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        if parts.peek().is_some() {
            flags |= libc::O_DIRECTORY;
        }
        // The CString and parent descriptor live through the call; a successful fd is owned.
        let fd = unsafe { libc::openat(current.as_raw_fd(), part.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        current = unsafe { std::fs::File::from_raw_fd(fd) };
        if parts.peek().is_none() {
            return Ok(current);
        }
    }
    Err(std::io::ErrorKind::InvalidInput.into())
}

#[cfg(windows)]
fn open_original(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, GetFinalPathNameByHandleW,
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut name = vec![0u16; 32768];
    // Resolve the already open handle, not a second lookup of the supplied path.
    let len = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            name.as_mut_ptr(),
            name.len() as u32,
            0,
        )
    } as usize;
    if len == 0
        || len >= name.len()
        || Path::new(&std::ffi::OsString::from_wide(&name[..len])) != path
    {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}
