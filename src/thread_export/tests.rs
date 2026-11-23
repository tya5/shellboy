use super::*;
use serde_json::json;

#[test]
fn thread_export_trailer_counts_and_hashes_exact_written_lines() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.jsonl");
    let mut output = ExportFile::new(&path).unwrap();
    output.line(&json!({"type":"header"})).unwrap();
    for id in ["current", "source-1", "source-2", "source-3"] {
        output.line(&json!({"type":"node","id":id})).unwrap();
    }
    output
        .line(&json!({"type":"tombstone","id":"collected","reason":"pruned_unreachable"}))
        .unwrap();
    assert!(!path.exists());
    let trailer = output.publish(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let tail_start = bytes[..bytes.len() - 1]
        .iter()
        .rposition(|b| *b == b'\n')
        .unwrap()
        + 1;
    let mut hashed = bytes[..tail_start].to_vec();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes[tail_start..]).unwrap(),
        json!({"type":"trailer","lines":6,"sha256":trailer.sha256})
    );
    assert_eq!(trailer.lines, 6);
    assert_eq!(digest_hex(&Sha256::digest(&hashed)), trailer.sha256);
    hashed[1] ^= 1;
    assert_ne!(digest_hex(&Sha256::digest(&hashed)), trailer.sha256);
}

#[test]
fn dropping_thread_export_leaves_destination_and_removes_temporary_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("thread.jsonl");
    std::fs::write(&path, b"previous export").unwrap();
    let mut output = ExportFile::new(&path).unwrap();
    output.line(&json!({"type":"header"})).unwrap();
    let temporary = output.file.path().to_owned();
    drop(output);
    assert!(!temporary.exists());
    assert_eq!(std::fs::read(path).unwrap(), b"previous export");
}
