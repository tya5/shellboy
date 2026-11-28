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
        let candidate = root.join(relative);
        if candidate.canonicalize().ok().as_ref() != Some(&candidate) {
            return Err(unavailable());
        }
        candidate
    } else {
        return Err(unavailable());
    };
    let bytes = bounded_read(&path).map_err(|error| {
        if error.code == "input_limit" {
            error
        } else {
            unavailable()
        }
    })?;
    let current = hash(&bytes);
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| err("invalid_argument", "Original file is not UTF-8"))?;
    let mut result = page(text, &format!("{reference}@{current}"), limit, cursor)?;
    result["ref"] = json!(reference);
    result["fingerprint"] = json!(current);
    result["matches_indexed"] = json!(observed == current);
    Ok(result)
}
