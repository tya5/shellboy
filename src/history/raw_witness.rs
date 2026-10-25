//! Retained shell-source identity shared by index authorization and RAG readers.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct RawWitness {
    pub(crate) execution: String,
    pub(crate) generation: i64,
    pub(crate) saved_sequence: i64,
    pub(crate) encoding: String,
}
impl RawWitness {
    /// Preserve query-specific selection: index also observes running sources;
    /// RAG only selects completed executions before it creates a reference.
    pub(crate) fn for_node(
        db: &Connection,
        node: &str,
        terminal_only: bool,
    ) -> rusqlite::Result<Option<(Self, String)>> {
        let sql = if terminal_only {
            "SELECT e.id,e.retention_generation,e.saved_sequence,e.output_encoding,e.state FROM tool_result r JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id JOIN shell_execution e ON e.id=c.operation_id OR e.operation_id=c.operation_id WHERE r.node_id=?1 AND e.state IN('exited','failed','stopped')"
        } else {
            "SELECT e.id,e.retention_generation,e.saved_sequence,e.output_encoding,e.state FROM tool_result r JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id JOIN shell_execution e ON e.id=c.operation_id OR e.operation_id=c.operation_id WHERE r.node_id=?1"
        };
        db.query_row(sql, [node], |row| {
            Ok((
                Self {
                    execution: row.get(0)?,
                    generation: row.get(1)?,
                    saved_sequence: row.get(2)?,
                    encoding: row.get(3)?,
                },
                row.get(4)?,
            ))
        })
        .optional()
    }
    pub(crate) fn is_current(&self, db: &Connection) -> rusqlite::Result<bool> {
        db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE id=?1 AND retention_generation=?2 AND saved_sequence=?3 AND output_encoding=?4 AND state IN('exited','failed','stopped')) AND NOT EXISTS(SELECT 1 FROM app_meta WHERE key=?5)", params![self.execution,self.generation,self.saved_sequence,self.encoding,format!("raw_tombstone.{}",self.execution)], |row|row.get(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_selection_and_retention_facts_remain_distinct() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE tool_result(node_id TEXT,group_id TEXT,call_id TEXT);
            CREATE TABLE tool_call(group_id TEXT,call_id TEXT,operation_id TEXT);
            CREATE TABLE shell_execution(id TEXT,operation_id TEXT,retention_generation INTEGER,saved_sequence INTEGER,output_encoding TEXT,state TEXT);
            CREATE TABLE app_meta(key TEXT,value TEXT);
            INSERT INTO tool_result VALUES('node','group','call');
            INSERT INTO tool_call VALUES('group','call','operation');
            INSERT INTO shell_execution VALUES('execution','operation',2,8,'UTF-8','running');").unwrap();
        assert!(RawWitness::for_node(&db, "node", true).unwrap().is_none());
        let (witness, state) = RawWitness::for_node(&db, "node", false).unwrap().unwrap();
        assert_eq!(state, "running");
        assert!(!witness.is_current(&db).unwrap());
        db.execute("UPDATE shell_execution SET state='exited'", [])
            .unwrap();
        assert_eq!(
            RawWitness::for_node(&db, "node", true).unwrap().unwrap().0,
            witness
        );
        assert!(witness.is_current(&db).unwrap());
        for changed in [
            RawWitness {
                generation: 3,
                ..witness.clone()
            },
            RawWitness {
                saved_sequence: 9,
                ..witness.clone()
            },
            RawWitness {
                encoding: "Shift_JIS".into(),
                ..witness.clone()
            },
            RawWitness {
                execution: "other".into(),
                ..witness.clone()
            },
        ] {
            assert!(!changed.is_current(&db).unwrap());
        }
        db.execute(
            "INSERT INTO app_meta VALUES('raw_tombstone.execution','reason')",
            [],
        )
        .unwrap();
        // Selection and current retention are separate: a stale reference can
        // still be decoded into its facts, but can never authorize raw reads.
        assert!(RawWitness::for_node(&db, "node", true).unwrap().is_some());
        assert!(!witness.is_current(&db).unwrap());
        assert_eq!(
            serde_json::to_string(&witness).unwrap(),
            r#"{"execution":"execution","generation":2,"saved_sequence":8,"encoding":"UTF-8"}"#
        );
    }
}
