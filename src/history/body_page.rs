use super::*;

fn expired() -> ApiError {
    ApiError::new(ErrorCode::CursorExpired, "body position is unavailable")
}

fn no_character_fits() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidArgument,
        "limit_bytes must fit the next UTF-8 character",
    )
}

pub(super) fn projected(
    text: &str,
    start: usize,
    limit: usize,
) -> Result<(String, usize), ApiError> {
    if !text.is_char_boundary(start) {
        return Err(expired());
    }
    let end = text.floor_char_boundary(start.saturating_add(limit).min(text.len()));
    if end == start && end < text.len() {
        return Err(no_character_fits());
    }
    Ok((text[start..end].to_owned(), text.len()))
}

/// SQLite's incremental I/O returns the saved UTF-8 JSON directly without
/// materializing the TEXT column, including for late byte-cursor positions.
pub(super) fn saved_json(
    db: &Connection,
    rowid: i64,
    start: usize,
    limit: usize,
) -> Result<(String, usize), ApiError> {
    let blob = db
        .blob_open("main", "node", "payload_json", rowid, true)
        .map_err(sql_error)?;
    read_page(blob.len(), start, limit, |at, bytes| {
        blob.read_at_exact(bytes, at)
    })
}

fn read_page(
    total: usize,
    start: usize,
    limit: usize,
    read: impl FnOnce(usize, &mut [u8]) -> rusqlite::Result<()>,
) -> Result<(String, usize), ApiError> {
    let remaining = total.checked_sub(start).ok_or_else(expired)?;
    let mut bytes = vec![0; remaining.min(limit)];
    if !bytes.is_empty() {
        read(start, &mut bytes).map_err(sql_error)?;
    }
    if bytes.first().is_some_and(|byte| byte & 0xc0 == 0x80) {
        return Err(expired());
    }
    if let Err(error) = std::str::from_utf8(&bytes) {
        if error.error_len().is_some() || bytes.len() == remaining {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "stored history payload is malformed",
            ));
        }
        bytes.truncate(error.valid_up_to());
    }
    if bytes.is_empty() && remaining > 0 {
        return Err(no_character_fits());
    }
    Ok((String::from_utf8(bytes).expect("page was validated"), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_blob_reads_only_the_requested_range_and_preserves_utf8_boundaries() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let payload = format!("\"{}\u{754c}tail\"", "a".repeat(2_000_000));
        db.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('n','incomplete_response','t',?1)", [&payload]).unwrap();
        let rowid = db.last_insert_rowid();
        let start = 2_000_000;
        let blob = db
            .blob_open("main", "node", "payload_json", rowid, true)
            .unwrap();
        let page = read_page(blob.len(), start, 3, |at, bytes| {
            assert_eq!((at, bytes.len()), (start, 3));
            blob.read_at_exact(bytes, at)
        })
        .unwrap();
        assert_eq!(page, ("a".into(), payload.len()));
        assert_eq!(
            saved_json(&db, rowid, start + 1, 3).unwrap(),
            ("\u{754c}".into(), payload.len())
        );
        assert_eq!(
            saved_json(&db, rowid, start + 2, 3).unwrap_err().code,
            ErrorCode::CursorExpired
        );
        assert_eq!(
            saved_json(&db, rowid, start + 1, 2).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
}
