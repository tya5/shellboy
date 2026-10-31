//! Independent, read-only display tails. CLI cursors and read pins are untouched.
use super::*;
pub(crate) mod live;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PreviewLimits {
    pub bytes: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct PreviewPage {
    pub stdout: String,
    pub stderr: String,
    pub raw_bytes: usize,
    pub inspected_rows: usize,
    pub skipped: bool,
    pub decode_uncertain: bool,
    pub decode_errors: bool,
    pub terminal: bool,
    pub output_complete: bool,
}

struct Chunk {
    sequence: i64,
    offset: usize,
    len: usize,
}

fn tail_rows(db: &Connection, execution: &str, stream: &str, rows: usize) -> Result<Vec<Chunk>> {
    // Metadata only: never return an entire large BLOB to find its suffix.
    let mut query = db.prepare("SELECT sequence,stream_offset,length(bytes) FROM output_chunk INDEXED BY output_stream WHERE execution_id=?1 AND stream=?2 ORDER BY stream_offset DESC LIMIT ?3").map_err(sql_error)?;
    let found = query
        .query_map(params![execution, stream, rows as i64], |row| {
            Ok(Chunk {
                sequence: row.get(0)?,
                offset: usize::try_from(row.get::<_, i64>(1)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
                len: usize::try_from(row.get::<_, i64>(2)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
            })
        })
        .map_err(sql_error)?;
    found
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_error)
}

fn end(rows: &[Chunk]) -> usize {
    rows.first()
        .map_or(0, |row| row.offset.saturating_add(row.len))
}

fn suffix(
    db: &Connection,
    execution: &str,
    rows: &[Chunk],
    budget: usize,
) -> Result<(Vec<u8>, usize)> {
    let mut pieces = Vec::new();
    let mut remaining = budget;
    let mut start = end(rows);
    for row in rows {
        if remaining == 0 || row.offset.saturating_add(row.len) != start {
            break;
        }
        let count = row.len.min(remaining);
        let bytes: Vec<u8> = db.query_row("SELECT substr(bytes,?3+1,?4) FROM output_chunk WHERE execution_id=?1 AND sequence=?2", params![execution,row.sequence,(row.len-count) as i64,count as i64], |r|r.get(0)).map_err(sql_error)?;
        remaining -= bytes.len();
        start = row.offset + row.len - bytes.len();
        pieces.push(bytes);
    }
    let mut bytes = Vec::with_capacity(budget - remaining);
    for piece in pieces.into_iter().rev() {
        bytes.extend_from_slice(&piece);
    }
    Ok((bytes, start))
}

fn boundary_cut(bytes: &[u8], start: usize, encoding: &'static Encoding) -> (usize, bool) {
    let mut cut = 0;
    let uncertain = start != 0
        && encoding != encoding_rs::UTF_8
        && encoding != encoding_rs::UTF_16LE
        && encoding != encoding_rs::UTF_16BE;
    if start != 0 && encoding == encoding_rs::UTF_8 {
        while cut < bytes.len().min(3) && bytes[cut] & 0xc0 == 0x80 {
            cut += 1;
        }
    } else if !start.is_multiple_of(2)
        && (encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE)
    {
        cut = bytes.len().min(1);
    }
    if start != 0
        && bytes.len() >= cut + 2
        && (encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE)
    {
        let unit = if encoding == encoding_rs::UTF_16LE {
            u16::from_le_bytes([bytes[cut], bytes[cut + 1]])
        } else {
            u16::from_be_bytes([bytes[cut], bytes[cut + 1]])
        };
        if (0xdc00..=0xdfff).contains(&unit) {
            cut += 2;
        }
    }
    (cut, uncertain)
}
fn decode(bytes: &[u8], start: usize, encoding: &'static Encoding) -> (String, bool, bool) {
    let (cut, uncertain) = boundary_cut(bytes, start, encoding);
    let (text, errors) = encoding.decode_without_bom_handling(&bytes[cut..]);
    (text.into_owned(), errors, uncertain)
}

/// Own one finite snapshot. The caller opens this connection read-only/query_only.
pub(crate) fn preview_tail(
    db: &Connection,
    execution: &str,
    limits: PreviewLimits,
) -> Result<PreviewPage> {
    let tx = db.unchecked_transaction().map_err(sql_error)?;
    if let Some(error) = raw_tombstone(&tx, execution)? {
        return Err(error);
    }
    let state = snapshot(&tx, execution)?;
    let encoding = canonical_encoding(&state.encoding)?;
    let budget = limits.bytes.clamp(1, 16 * 1024);
    // The two indexed metadata ranges together inspect at most 128 rows.
    let stderr = tail_rows(&tx, execution, "stderr", 64)?;
    let stdout = tail_rows(&tx, execution, "stdout", 128 - stderr.len())?;
    let stderr_end = end(&stderr);
    let stdout_end = end(&stdout);
    let reserved = stderr_end.min(budget.min(1024));
    let stdout_budget = stdout_end.min(budget - reserved);
    let stderr_budget = stderr_end.min(budget - stdout_budget);
    let (out, out_start) = suffix(&tx, execution, &stdout, stdout_budget)?;
    let (err, err_start) = suffix(&tx, execution, &stderr, stderr_budget)?;
    let (stdout_text, stdout_errors, stdout_uncertain) = decode(&out, out_start, encoding);
    let (stderr_text, stderr_errors, stderr_uncertain) = decode(&err, err_start, encoding);
    let terminal = state.terminal();
    tx.commit().map_err(sql_error)?;
    Ok(PreviewPage {
        stdout: stdout_text,
        stderr: stderr_text,
        raw_bytes: out.len() + err.len(),
        inspected_rows: stdout.len() + stderr.len(),
        skipped: out_start != 0 || err_start != 0,
        decode_uncertain: stdout_uncertain || stderr_uncertain,
        decode_errors: stdout_errors || stderr_errors,
        terminal,
        output_complete: state.details.output_complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn fixture(encoding: &str) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE shell_execution(id TEXT PRIMARY KEY,owner_run_id TEXT,state TEXT,exit_code INTEGER,end_reason TEXT,output_complete INTEGER,saved_sequence INTEGER,retention_generation INTEGER,output_encoding TEXT,started_at TEXT,ended_at TEXT);
            CREATE TABLE output_chunk(execution_id TEXT,sequence INTEGER,stream TEXT,stream_offset INTEGER,bytes BLOB,received_at TEXT,PRIMARY KEY(execution_id,sequence));
            CREATE INDEX output_stream ON output_chunk(execution_id,stream,stream_offset);
            INSERT INTO shell_execution VALUES('e','r','exited',1,NULL,1,999,7,'UTF-8','2026-10-30T00:00:00Z','2026-10-30T00:00:01Z');").unwrap();
        db.execute("UPDATE shell_execution SET output_encoding=?1", [encoding])
            .unwrap();
        db
    }

    fn insert(db: &Connection, sequence: i64, stream: &str, offset: usize, bytes: &[u8]) {
        db.execute(
            "INSERT INTO output_chunk VALUES('e',?1,?2,?3,?4,'2026-10-30T00:00:00Z')",
            params![sequence, stream, offset as i64, bytes],
        )
        .unwrap();
    }

    #[test]
    fn giant_chunks_return_true_suffix_with_one_shared_budget_and_no_writes() {
        let db = fixture("UTF-8");
        let out = format!("{}OUT-END", "x".repeat(40_000));
        let err = format!("{}ERR-END", "y".repeat(40_000));
        insert(&db, 0, "stdout", 0, out.as_bytes());
        insert(&db, 1, "stderr", 0, err.as_bytes());
        let changes = db.total_changes();
        db.pragma_update(None, "query_only", true).unwrap();
        let page = preview_tail(&db, "e", PreviewLimits { bytes: 2048 }).unwrap();
        assert_eq!(page.raw_bytes, 2048);
        assert!(page.stdout.ends_with("OUT-END") && page.stderr.ends_with("ERR-END"));
        assert_eq!(page.stdout.len(), 1024);
        assert_eq!(page.stderr.len(), 1024);
        assert!(page.skipped && page.terminal && page.output_complete);
        assert_eq!(page.inspected_rows, 2);
        assert_eq!(db.total_changes(), changes);
    }

    #[test]
    fn tiny_fragmentation_is_row_bounded_and_uses_indexed_tail_ranges() {
        let db = fixture("UTF-8");
        for n in 0..200 {
            insert(&db, n, "stdout", n as usize, b"x");
        }
        let page = preview_tail(&db, "e", PreviewLimits { bytes: 16 * 1024 }).unwrap();
        assert_eq!(page.inspected_rows, 128);
        assert_eq!(page.raw_bytes, 128);
        assert!(page.skipped);
        let plan:String=db.query_row("EXPLAIN QUERY PLAN SELECT sequence,stream_offset,length(bytes) FROM output_chunk INDEXED BY output_stream WHERE execution_id='e' AND stream='stdout' ORDER BY stream_offset DESC LIMIT 128",[],|r|r.get(3)).unwrap();
        assert!(
            plan.contains("SEARCH") && plan.contains("output_stream"),
            "{plan}"
        );
        let plan:String=db.query_row("EXPLAIN QUERY PLAN SELECT substr(bytes,5,8) FROM output_chunk WHERE execution_id='e' AND sequence=199",[],|r|r.get(3)).unwrap();
        assert!(plan.contains("SEARCH") && !plan.contains("SCAN"), "{plan}");
    }

    #[test]
    fn unicode_alignment_and_arbitrary_state_are_honest_without_prefix_scanning() {
        let db = fixture("UTF-8");
        insert(&db, 0, "stdout", 0, "prefix😀tail".as_bytes());
        let page = preview_tail(&db, "e", PreviewLimits { bytes: 7 }).unwrap();
        assert_eq!(page.stdout, "tail");
        assert!(!page.decode_errors && !page.decode_uncertain);
        db.execute("UPDATE shell_execution SET output_encoding='UTF-16LE'", [])
            .unwrap();
        db.execute("DELETE FROM output_chunk", []).unwrap();
        let bytes = "prefix😀tail"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        insert(&db, 0, "stdout", 0, &bytes);
        let page = preview_tail(&db, "e", PreviewLimits { bytes: 11 }).unwrap();
        assert_eq!(page.stdout, "tail");
        assert!(!page.decode_errors);
        db.execute(
            "UPDATE shell_execution SET output_encoding='ISO-2022-JP'",
            [],
        )
        .unwrap();
        let page = preview_tail(&db, "e", PreviewLimits { bytes: 5 }).unwrap();
        assert!(page.decode_uncertain);
    }

    #[test]
    fn retention_and_encoding_metadata_are_snapshot_facts() {
        let db = fixture("UTF-8");
        insert(&db, 0, "stdout", 100, b"tail");
        let page = preview_tail(&db, "e", PreviewLimits { bytes: 2048 }).unwrap();
        assert!(page.skipped);
        db.execute(
            "UPDATE shell_execution SET retention_generation=8,output_encoding='CP932'",
            [],
        )
        .unwrap();
        let next = preview_tail(&db, "e", PreviewLimits { bytes: 2048 }).unwrap();
        assert!(next.decode_uncertain);
        db.execute(
            "UPDATE shell_execution SET output_encoding='not-an-encoding'",
            [],
        )
        .unwrap();
        assert_eq!(
            preview_tail(&db, "e", PreviewLimits { bytes: 2048 })
                .unwrap_err()
                .code,
            ErrorCode::InvalidState
        );
        db.execute("UPDATE shell_execution SET output_encoding='UTF-8'", [])
            .unwrap();
        db.execute(
            "INSERT INTO app_meta VALUES('raw_tombstone.e','{\"reason\":\"gc\"}')",
            [],
        )
        .unwrap();
        assert_eq!(
            preview_tail(&db, "e", PreviewLimits { bytes: 2048 })
                .unwrap_err()
                .code,
            ErrorCode::OutputExpired
        );
    }
}
