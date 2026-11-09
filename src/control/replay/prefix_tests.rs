use super::*;

#[test]
fn inspection_prefix_uses_saved_numeric_tombstones_and_rejects_rewrites() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../../storage/current.sql")).unwrap();
    db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/AGENTS.md','p','m','now')", []).unwrap();
    let tx = db.transaction().unwrap();
    crate::history::append_node(&tx,"t","n",None,"fixture",&serde_json::json!({"role":"user","content":"synthetic"}),"now").unwrap();
    tx.commit().unwrap();
    for tombstone in [false, true] {
        db.execute("UPDATE node SET deleted_reason=?1 WHERE id='n'", [tombstone.then_some("synthetic")]).unwrap();
        let tx = db.transaction().unwrap();
        let saved = load_uncached(&tx, "t", None, 0).unwrap().witness();
        // Independent expected bytes match the established persisted contract.
        let revision:i64 = tx.query_row("SELECT payload_revision FROM node WHERE id='n'",[],|r|r.get(0)).unwrap();
        assert_eq!(saved.sha256, crate::digest::sha256_hex(format!("n\u{1f}{revision}\u{1f}{}\u{1e}",u8::from(tombstone))));
        assert_eq!(saved.nodes, 1);
        let inspected = load_for_inspection(&tx, "t", Some(&saved), 0).unwrap().expect("unchanged nonempty prefix");
        assert_eq!(inspected.witness(), saved);
        tx.execute("UPDATE node SET payload_revision=payload_revision+1 WHERE id='n'", []).unwrap();
        assert!(load_for_inspection(&tx, "t", Some(&saved), 0).unwrap().is_none(), "rewritten prefix must remain unknown");
        tx.rollback().unwrap();
    }
}
