//! Immutable node facts for readers that already require the payload.
use rusqlite::{Connection, OptionalExtension};
pub(crate) struct NodeRow {
    pub(crate) kind: String,
    pub(crate) payload: Option<String>,
    pub(crate) revision: i64,
    pub(crate) body_hash: Option<String>,
    pub(crate) deleted_reason: Option<String>,
}
impl NodeRow {
    pub(crate) fn load(db: &Connection, id: &str) -> rusqlite::Result<Option<Self>> {
        Self::query(
            db,
            id,
            "SELECT kind,payload_json,payload_revision,body_hash,deleted_reason FROM node WHERE id=?1",
        )
    }
    /// History body pagination does not validate or materialize the unused hash.
    pub(crate) fn load_payload(db: &Connection, id: &str) -> rusqlite::Result<Option<Self>> {
        Self::query(
            db,
            id,
            "SELECT kind,payload_json,payload_revision,NULL,deleted_reason FROM node WHERE id=?1",
        )
    }
    fn query(db: &Connection, id: &str, sql: &str) -> rusqlite::Result<Option<Self>> {
        db.query_row(sql, [id], |row| {
            Ok(Self {
                kind: row.get(0)?,
                payload: row.get(1)?,
                revision: row.get(2)?,
                body_hash: row.get(3)?,
                deleted_reason: row.get(4)?,
            })
        })
        .optional()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payload_only_projection_keeps_hash_validation_at_source_boundary() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE node(id TEXT,kind TEXT,payload_json TEXT,payload_revision INTEGER,body_hash,deleted_reason TEXT); INSERT INTO node VALUES('n','summary','{\"text\":\"界\"}',7,X'FE','collected');").unwrap();
        let body = NodeRow::load_payload(&db, "n").unwrap().unwrap();
        assert_eq!(body.kind, "summary");
        assert_eq!(body.revision, 7);
        assert_eq!(body.payload.as_deref(), Some(r#"{"text":"界"}"#));
        assert_eq!(body.deleted_reason.as_deref(), Some("collected"));
        assert!(NodeRow::load(&db, "n").is_err());
        assert!(NodeRow::load_payload(&db, "missing").unwrap().is_none());
    }
}
