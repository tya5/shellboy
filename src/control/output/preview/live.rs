//! One open display's decoder continuity; never a CLI read pin or durable cursor.
use super::*;

pub(crate) struct LiveCursor {
    execution: String,
    generation: u64,
    encoding: String,
    offsets: [usize; 2],
    decoders: Decoders,
    text: [String; 2],
    errors: bool,
    uncertain: bool,
    skipped: bool,
    finished: bool,
}

fn trim_tail(text: &mut String, budget: usize) {
    if text.len() > budget {
        let cut = text.ceil_char_boundary(text.len() - budget);
        *text = text[cut..].to_owned();
    } else if text.capacity() > budget {
        // Retained allocation, not just logical length, obeys this allowance.
        *text = text.as_str().to_owned();
    }
}

/// Read at most4KiB/128 metadata rows, carrying decoder state only when the
/// current retained range is contiguous with the preceding finite read.
pub(crate) fn read(
    db: &Connection,
    execution: &str,
    previous: Option<LiveCursor>,
    limits: PreviewLimits,
) -> Result<(LiveCursor, PreviewPage)> {
    let tx = db.unchecked_transaction().map_err(sql_error)?;
    if let Some(error) = raw_tombstone(&tx, execution)? {
        return Err(error);
    }
    let state = snapshot(&tx, execution)?;
    let encoding = canonical_encoding(&state.encoding)?;
    let budget = limits.bytes.clamp(1, 4096);
    let err_rows = tail_rows(&tx, execution, "stderr", 64)?;
    let out_rows = tail_rows(&tx, execution, "stdout", 128 - err_rows.len())?;
    let rows = [&out_rows, &err_rows];
    let ends = [end(&out_rows), end(&err_rows)];
    let retained = |index: usize, offset: usize| {
        if offset == ends[index] {
            return true;
        }
        let mut expected = ends[index];
        for row in rows[index] {
            if row.offset.saturating_add(row.len) != expected {
                return false;
            }
            if offset >= row.offset && offset <= expected {
                return true;
            }
            expected = row.offset;
        }
        false
    };
    let continuous = previous.as_ref().is_some_and(|old| {
        old.execution == execution
            && old.generation == state.generation
            && old.encoding == encoding.name()
            && (!old.finished
                || (state.terminal() && state.details.output_complete && old.offsets == ends))
            && (0..2).all(|i| retained(i, old.offsets[i]))
            && ends[0]
                .saturating_sub(old.offsets[0])
                .saturating_add(ends[1].saturating_sub(old.offsets[1]))
                <= budget
    });
    let had_previous = previous.is_some();
    let mut cursor = if continuous {
        previous.expect("continuity requires previous cursor")
    } else {
        LiveCursor {
            execution: execution.into(),
            generation: state.generation,
            encoding: encoding.name().into(),
            offsets: [0; 2],
            decoders: Decoders::new(encoding),
            text: Default::default(),
            errors: false,
            uncertain: false,
            skipped: had_previous,
            finished: false,
        }
    };
    let budgets = if continuous {
        [ends[0] - cursor.offsets[0], ends[1] - cursor.offsets[1]]
    } else {
        let reserved = ends[1].min(budget.min(1024));
        let out = ends[0].min(budget - reserved);
        [out, ends[1].min(budget - out)]
    };
    let mut raw_bytes = 0;
    let terminal = state.terminal();
    let complete = terminal && state.details.output_complete;
    for i in 0..2 {
        let stream = if i == 0 { "stdout" } else { "stderr" };
        let (bytes, start) = suffix(&tx, execution, rows[i], budgets[i])?;
        raw_bytes += bytes.len();
        // Metadata gaps invalidate continuity just as retention/encoding changes
        // do. Never feed bytes after an unknown gap into a stateful decoder.
        let reset = !continuous || start != cursor.offsets[i];
        let (cut, uncertain) = if reset {
            boundary_cut(&bytes, start, encoding)
        } else {
            (0, false)
        };
        if reset && continuous {
            return Err(error(
                ErrorCode::InvalidState,
                "Live output retained range changed within its read snapshot",
            ));
        }
        cursor.skipped |= start != 0 && reset;
        cursor.uncertain |= uncertain;
        if !cursor.finished {
            let (part, errors) = cursor.decoders.decode(stream, &bytes[cut..], complete);
            cursor.text[i].push_str(&part);
            cursor.errors |= errors;
        }
        cursor.offsets[i] = ends[i];
    }
    cursor.finished = complete;
    // Both streams share one retained decoded-text allowance, reserving errors.
    trim_tail(&mut cursor.text[1], budget);
    let stdout_budget = budget.saturating_sub(cursor.text[1].len());
    trim_tail(&mut cursor.text[0], stdout_budget);
    let page = PreviewPage {
        stdout: cursor.text[0].clone(),
        stderr: cursor.text[1].clone(),
        raw_bytes,
        inspected_rows: out_rows.len() + err_rows.len(),
        skipped: cursor.skipped,
        decode_uncertain: cursor.uncertain,
        decode_errors: cursor.errors,
        terminal,
        output_complete: state.details.output_complete,
    };
    tx.commit().map_err(sql_error)?;
    Ok((cursor, page))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(encoding: &str) -> Connection {
        let db = super::super::tests::fixture(encoding);
        db.execute(
            "UPDATE shell_execution SET state='running',output_complete=0",
            [],
        )
        .unwrap();
        db
    }
    fn chunk(db: &Connection, sequence: i64, stream: &str, offset: i64, bytes: &[u8]) {
        db.execute(
            "INSERT INTO output_chunk VALUES('e',?1,?2,?3,?4,'2026-10-31T00:00:00Z')",
            params![sequence, stream, offset, bytes],
        )
        .unwrap();
    }
    #[test]
    fn utf8_and_utf16_partial_units_survive_contiguous_reads_without_replacement() {
        for (encoding, first, second, expected) in [
            ("UTF-8", vec![0xe3, 0x81], vec![0x82], "あ"),
            ("UTF-16LE", vec![0x3d, 0xd8], vec![0x00, 0xde], "😀"),
        ] {
            let db = fixture(encoding);
            chunk(&db, 1, "stdout", 0, &first);
            let (cursor, page) = read(&db, "e", None, PreviewLimits { bytes: 4096 }).unwrap();
            assert!(page.stdout.is_empty() && !page.decode_errors);
            chunk(&db, 2, "stdout", first.len() as i64, &second);
            let (cursor, page) =
                read(&db, "e", Some(cursor), PreviewLimits { bytes: 4096 }).unwrap();
            assert_eq!(page.stdout, expected);
            assert!(!page.decode_errors && !page.skipped);
            db.execute(
                "UPDATE shell_execution SET state='exited',output_complete=1",
                [],
            )
            .unwrap();
            let (cursor, page) =
                read(&db, "e", Some(cursor), PreviewLimits { bytes: 4096 }).unwrap();
            assert!(page.terminal && page.output_complete);
            let (_, page) = read(&db, "e", Some(cursor), PreviewLimits { bytes: 4096 }).unwrap();
            assert_eq!(page.stdout, expected);
        }
    }
    #[test]
    fn stateful_encoding_continuity_and_backlog_resets_are_explicit_and_bounded() {
        let db = fixture("ISO-2022-JP");
        chunk(&db, 1, "stdout", 0, b"\x1b$B$");
        let (cursor, page) = read(&db, "e", None, PreviewLimits { bytes: 4096 }).unwrap();
        assert!(page.stdout.is_empty());
        chunk(&db, 2, "stdout", 4, b"\x22\x1b(B");
        let (cursor, page) = read(&db, "e", Some(cursor), PreviewLimits { bytes: 4096 }).unwrap();
        assert_eq!(page.stdout, "あ");
        assert!(!page.decode_uncertain);
        chunk(&db, 3, "stdout", 8, &vec![b'x'; 8192]);
        let (cursor, page) = read(&db, "e", Some(cursor), PreviewLimits { bytes: 4096 }).unwrap();
        assert!(page.skipped && page.decode_uncertain && page.raw_bytes <= 4096);
        assert!(page.stdout.len() + page.stderr.len() <= 4096);
        assert!(cursor.text.iter().map(String::capacity).sum::<usize>() <= 4096);
        db.execute(
            "UPDATE shell_execution SET retention_generation=8,output_encoding='UTF-8'",
            [],
        )
        .unwrap();
        let (_, page) = read(&db, "e", Some(cursor), PreviewLimits { bytes: 4096 }).unwrap();
        assert!(page.skipped && page.raw_bytes <= 4096 && !page.decode_uncertain);
    }
    #[test]
    fn complete_terminal_flushes_pending_units_and_streams_share_the_byte_allowance() {
        let db = fixture("UTF-8");
        chunk(&db, 1, "stdout", 0, &vec![b'o'; 5000]);
        chunk(&db, 2, "stderr", 0, &[0xe3, 0x81]);
        let (cursor, page) = read(&db, "e", None, PreviewLimits { bytes: 4096 }).unwrap();
        assert!(page.raw_bytes <= 4096 && !page.decode_errors && page.stderr.is_empty());
        db.execute(
            "UPDATE shell_execution SET state='exited',output_complete=1",
            [],
        )
        .unwrap();
        let (_, page) = read(&db, "e", Some(cursor), PreviewLimits { bytes: 4096 }).unwrap();
        assert!(page.decode_errors && page.stderr.contains('�'));
        assert!(page.stdout.len() + page.stderr.len() <= 4096);
    }
}
