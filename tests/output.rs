use rusqlite::params;
use shellboy::{
    control::output::{OutputCursor, ShellOutputReader, StreamSelection, wait_position},
    protocol::{ErrorCode, ShellReadArgs},
    storage::DbWorker,
};
use tempfile::TempDir;
async fn fixture() -> (TempDir, DbWorker, ShellOutputReader) {
    let dir = tempfile::tempdir().unwrap();
    let db = DbWorker::open(dir.path().join("db")).await.unwrap();
    db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('x','r','u','op','running','/','{}','UTF-8','UTF-8','closed','now');")?;Ok(())}).await.unwrap();
    let reader = ShellOutputReader::new(db.clone(), "instance");
    (dir, db, reader)
}
async fn append(db: &DbWorker, stream: &str, bytes: &[u8]) {
    let bytes = bytes.to_vec();
    let stream = stream.to_owned();
    db.call(move|db|{let tx=db.transaction()?;let seq:i64=tx.query_row("SELECT saved_sequence FROM shell_execution WHERE id='x'",[],|r|r.get(0))?;let offset:i64=tx.query_row("SELECT COALESCE(SUM(length(bytes)),0) FROM output_chunk WHERE execution_id='x' AND stream=?1",[&stream],|r|r.get(0))?;tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('x',?1,?2,?3,?4,'now')",params![seq,stream,offset,bytes])?;tx.execute("UPDATE shell_execution SET saved_sequence=?1 WHERE id='x'",[seq+1])?;tx.commit()?;Ok(())}).await.unwrap();
}
async fn finish(db: &DbWorker) {
    db.call(|db|{db.execute("UPDATE shell_execution SET state='exited',exit_code=0,output_complete=1,ended_at='now' WHERE id='x'",[])?;Ok(())}).await.unwrap();
}
fn args(cursor: Option<String>, encoding: &str, limit: u32) -> ShellReadArgs {
    ShellReadArgs {
        execution_id: "x".into(),
        cursor,
        encoding: Some(encoding.into()),
        limit_bytes: limit,
    }
}
async fn pin_count(db: &DbWorker) -> i64 {
    db.call(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM runtime_pin WHERE resource_kind='shell_output'",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn get_exposes_saved_command_context_without_inventing_legacy_commands() {
    let (_dir, db, reader) = fixture().await;
    let legacy = reader.get("x").await.unwrap();
    assert_eq!(legacy.command, None);
    assert_eq!(legacy.cwd.as_deref(), Some("/"));
    assert_eq!(legacy.thread_id.as_deref(), Some("t"));
    assert_eq!(legacy.turn_id.as_deref(), Some("u"));
    let command = "shellboy message send --thread t --text 'a request, not a shell execution'";
    db.call(move |db| {
        db.execute(
            "UPDATE shell_execution SET shell_json=?1 WHERE id='x'",
            [serde_json::json!({"command":command}).to_string()],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        reader.get("x").await.unwrap().command.as_deref(),
        Some(command)
    );
    // New clients must still accept a get response from a running older owner.
    let old: shellboy::protocol::ShellDetails = serde_json::from_value(serde_json::json!({
        "execution_id":"x", "run_id":"r", "state":"running", "output_complete":false
    }))
    .unwrap();
    assert_eq!(old.command, None);
    assert_eq!(old.thread_id, None);
    assert_eq!(old.forced, None);
    assert_eq!(old.exit_signal, None);
    assert_eq!(old.group_cleanup_confirmed, None);
    assert_eq!(old.elapsed_ms, None);
    let old = serde_json::to_value(old).unwrap();
    for field in [
        "forced",
        "exit_signal",
        "group_cleanup_confirmed",
        "elapsed_ms",
    ] {
        assert!(!old.as_object().unwrap().contains_key(field));
    }
}

#[tokio::test]
async fn utf8_split_bytes_live_pages_wait_and_terminal_flush_preserve_positions() {
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", &[0xe6, 0x97]).await;
    let first = reader.read(args(None, "UTF-8", 1)).await.unwrap();
    assert_eq!(first.text, "");
    assert_eq!(first.raw_bytes, 1);
    assert!(!first.decode_errors);
    let second = reader
        .read(args(Some(first.cursor), "UTF-8", 1))
        .await
        .unwrap();
    assert_eq!(second.text, "");
    assert_eq!(OutputCursor::decode(&second.cursor).unwrap().sequence, 1);
    let cursor = second.cursor.clone();
    assert!(
        !db.call(move |db| Ok(wait_position(db, "x", &cursor).unwrap()))
            .await
            .unwrap()
    );
    append(&db, "stdout", &[0xa5]).await;
    let cursor = second.cursor.clone();
    assert!(
        db.call(move |db| Ok(wait_position(db, "x", &cursor).unwrap()))
            .await
            .unwrap()
    );
    let third = reader
        .read(args(Some(second.cursor), "UTF-8", 10))
        .await
        .unwrap();
    assert_eq!(third.text, "日");
    assert!(!third.decode_errors);
    finish(&db).await;
    let end = reader
        .read(args(Some(third.cursor), "UTF-8", 10))
        .await
        .unwrap();
    assert!(OutputCursor::decode(&end.cursor).unwrap().eof_flushed);
    assert!(end.text.is_empty());
    assert!(end.output_complete);
    assert_eq!(pin_count(&db).await, 0);
}
#[tokio::test]
async fn incomplete_character_is_replaced_only_at_true_eof_and_exactly_once() {
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", &[0xe6]).await;
    let live = reader.read(args(None, "UTF-8", 10)).await.unwrap();
    assert_eq!(live.text, "");
    assert!(!live.decode_errors);
    finish(&db).await;
    let eof = reader
        .read(args(Some(live.cursor), "UTF-8", 10))
        .await
        .unwrap();
    assert_eq!(eof.text, "�");
    assert!(eof.decode_errors);
    let again = reader
        .read(args(Some(eof.cursor), "UTF-8", 10))
        .await
        .unwrap();
    assert_eq!(again.text, "");
    assert!(!again.decode_errors);
}
#[tokio::test]
async fn cp932_redecode_keeps_raw_unchanged_and_decoders_are_independent_per_stream() {
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", &[0x93]).await;
    append(&db, "stderr", &[0x82, 0xa0]).await;
    append(&db, "stdout", &[0xfa]).await;
    finish(&db).await;
    let first = reader.read(args(None, "CP932", 64)).await.unwrap();
    assert_eq!(first.text, "あ日");
    assert_eq!(first.encoding, "Shift_JIS");
    assert!(!first.decode_errors);
    let inherited = reader
        .read(ShellReadArgs {
            execution_id: "x".into(),
            cursor: None,
            encoding: None,
            limit_bytes: 64,
        })
        .await
        .unwrap();
    assert_eq!(inherited.text, "あ日");
    assert_eq!(inherited.encoding, "Shift_JIS");
    assert_eq!(
        reader
            .read(args(Some(first.cursor.clone()), "UTF-8", 64))
            .await
            .unwrap_err()
            .code,
        ErrorCode::CursorEncodingMismatch
    );
    let changed = reader.read(args(None, "UTF-8", 64)).await.unwrap();
    assert!(changed.decode_errors);
    assert_eq!(
        reader
            .read(args(Some(first.cursor), "CP932", 64))
            .await
            .unwrap_err()
            .code,
        ErrorCode::CursorEncodingMismatch
    );
    let raw: Vec<Vec<u8>> = db
        .call(|db| {
            let mut s = db.prepare("SELECT bytes FROM output_chunk ORDER BY sequence")?;
            Ok(s.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(raw, vec![vec![0x93], vec![0x82, 0xa0], vec![0xfa]]);
    let stdout = reader
        .read_stream(args(None, "CP932", 64), StreamSelection::Stdout)
        .await
        .unwrap();
    assert_eq!(stdout.text, "日");
}
#[tokio::test]
async fn iso2022_state_and_utf16_surrogates_survive_one_byte_cursor_pages() {
    let (_dir, db, reader) = fixture().await;
    let (bytes, _, bad) = encoding_rs::ISO_2022_JP.encode("日本語");
    assert!(!bad);
    for byte in bytes.iter() {
        append(&db, "stdout", &[*byte]).await;
    }
    finish(&db).await;
    let mut cursor = None;
    let mut text = String::new();
    loop {
        let page = reader.read(args(cursor, "ISO-2022-JP", 1)).await.unwrap();
        text.push_str(&page.text);
        assert!(!page.decode_errors);
        let done = OutputCursor::decode(&page.cursor).unwrap().eof_flushed;
        cursor = Some(page.cursor);
        if done {
            break;
        }
    }
    assert_eq!(text, "日本語");
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", &[0x3d, 0xd8, 0x00]).await;
    append(&db, "stdout", &[0xde]).await;
    finish(&db).await;
    let mut cursor = None;
    let mut text = String::new();
    loop {
        let page = reader.read(args(cursor, "UTF-16LE", 1)).await.unwrap();
        text.push_str(&page.text);
        let done = OutputCursor::decode(&page.cursor).unwrap().eof_flushed;
        cursor = Some(page.cursor);
        if done {
            break;
        }
    }
    assert_eq!(text, "😀");
}
#[tokio::test]
async fn streaming_snapshot_pins_raw_and_drops_pin_at_end_or_session_drop() {
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", b"abcdef").await;
    finish(&db).await;
    let mut stream = reader.stream("x").await.unwrap();
    assert_eq!(stream.retention_generation(), 0);
    assert_eq!(stream.saved_sequence(), 1);
    assert_eq!(stream.encoding(), "UTF-8");
    assert_eq!(pin_count(&db).await, 1);
    let deleted=db.call(|db|Ok(db.execute("DELETE FROM output_chunk WHERE execution_id='x' AND NOT EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id='x')",[])?)).await.unwrap();
    assert_eq!(deleted, 0);
    let a = stream.next_page(2).await.unwrap().unwrap();
    assert_eq!(a.text, "ab");
    assert!(!a.done);
    assert_eq!(pin_count(&db).await, 1);
    let b = stream.next_page(64).await.unwrap().unwrap();
    assert_eq!(b.text, "cdef");
    assert!(b.done);
    assert!(stream.next_page(64).await.unwrap().is_none());
    assert_eq!(pin_count(&db).await, 0);
    let stream = reader.stream("x").await.unwrap();
    assert_eq!(pin_count(&db).await, 1);
    drop(stream);
    for _ in 0..10 {
        tokio::task::yield_now().await;
        if pin_count(&db).await == 0 {
            return;
        }
    }
    panic!("dropped stream retained raw pin");
}
#[tokio::test]
async fn expiry_scope_and_encoding_are_errors_instead_of_empty_success() {
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", b"abc").await;
    let first = reader.read(args(None, "UTF-8", 1)).await.unwrap();
    let mut cursor = OutputCursor::decode(&first.cursor).unwrap();
    cursor.execution_id = "other".into();
    assert_eq!(
        reader
            .read(args(Some(cursor.encode().unwrap()), "UTF-8", 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::CursorExpired
    );
    db.call(|db| {
        db.execute("DELETE FROM output_chunk WHERE execution_id='x'", [])?;
        db.execute(
            "UPDATE shell_execution SET retention_generation=1 WHERE id='x'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        reader
            .read(args(Some(first.cursor.clone()), "UTF-8", 1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::OutputExpired
    );
    assert_eq!(
        reader.read(args(None, "UTF-8", 1)).await.unwrap_err().code,
        ErrorCode::OutputExpired
    );
    let cursor = first.cursor;
    assert_eq!(
        db.call(move |db| Ok(wait_position(db, "x", &cursor).unwrap_err().code))
            .await
            .unwrap(),
        ErrorCode::OutputExpired
    );
    assert_eq!(pin_count(&db).await, 0);
}
#[tokio::test]
async fn bounded_excerpt_uses_stored_encoding_and_unicode_safe_head_tail() {
    let (_dir, db, reader) = fixture().await;
    let text = format!("START{}END", "日本語".repeat(2000));
    let (bytes, _, bad) = encoding_rs::SHIFT_JIS.encode(&text);
    assert!(!bad);
    append(&db, "stdout", &bytes).await;
    finish(&db).await;
    reader.read(args(None, "CP932", 1)).await.unwrap();
    let excerpt = reader.excerpt("x", 100).await.unwrap();
    assert!(excerpt.len() <= 100);
    assert!(excerpt.starts_with("START"));
    assert!(excerpt.ends_with("END"));
    assert!(excerpt.contains("output omitted"));
    assert!(!excerpt.contains('�'));
    let mut stream = reader.stream("x").await.unwrap();
    let mut decoded = String::new();
    while let Some(page) = stream.next_page(67).await.unwrap() {
        decoded.push_str(&page.text);
        assert!(!page.decode_errors);
    }
    assert_eq!(decoded, text);
    assert_eq!(pin_count(&db).await, 0);
}

#[tokio::test]
async fn streaming_reader_uses_fixed_watermark_and_missing_middle_raw_is_explicit() {
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", b"first").await;
    let mut stream = reader.stream("x").await.unwrap();
    append(&db, "stderr", b"later").await;
    let page = stream.next_page(64).await.unwrap().unwrap();
    assert_eq!(page.text, "first");
    assert!(page.done);
    assert!(stream.next_page(64).await.unwrap().is_none());
    let all = reader
        .read(ShellReadArgs {
            execution_id: "x".into(),
            cursor: None,
            encoding: None,
            limit_bytes: 64,
        })
        .await
        .unwrap();
    assert_eq!(all.text, "firstlater");
    db.call(|db| {
        db.execute(
            "DELETE FROM output_chunk WHERE execution_id='x' AND sequence=1",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let error = reader.read(args(None, "UTF-8", 64)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::OutputExpired);
    assert_eq!(error.details.unwrap()["oldest_sequence"], 0);
    assert_eq!(pin_count(&db).await, 0);
}

#[tokio::test]
async fn raw_deletion_marker_blocks_remaining_chunks_and_excerpt_reports_expiry() {
    let (_dir, db, reader) = fixture().await;
    append(&db, "stdout", b"first").await;
    append(&db, "stdout", b"remaining").await;
    finish(&db).await;
    let cursor = reader.read(args(None, "UTF-8", 1)).await.unwrap().cursor;
    db.call(|db|{let tx=db.transaction()?;tx.execute("UPDATE shell_execution SET retention_generation=1 WHERE id='x'",[])?;tx.execute("INSERT INTO app_meta(key,value)VALUES('raw_tombstone.x',?1)",[serde_json::json!({"reason":"raw_retention_capacity","state":"deleting","generation":1}).to_string()])?;tx.commit()?;Ok(())}).await.unwrap();
    for cursor in [None, Some(cursor.clone())] {
        let error = reader.read(args(cursor, "UTF-8", 64)).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputExpired);
        assert_eq!(error.details.unwrap()["reason"], "raw_retention_capacity");
    }
    assert!(reader.stream("x").await.is_err());
    let cursor_copy = cursor;
    assert_eq!(
        db.call(move |db| Ok(wait_position(db, "x", &cursor_copy).unwrap_err().code))
            .await
            .unwrap(),
        ErrorCode::OutputExpired
    );
    let excerpt = reader.excerpt("x", 128).await.unwrap();
    assert!(excerpt.contains("expired") && excerpt.contains("raw_retention_capacity"));
    assert!(!excerpt.contains("remaining"));
    assert_eq!(pin_count(&db).await, 0);
}
