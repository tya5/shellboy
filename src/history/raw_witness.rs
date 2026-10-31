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

// Metadata-only observation. Unlike is_current, this helper neither requires a
// terminal shell nor treats a growing saved_sequence as a revoked raw cursor.
pub(crate) const RETAINED_EXECUTION_FACTS_SQL: &str = "WITH requested AS (
    SELECT CAST(key AS INTEGER) ordinal,json_extract(value,'$.execution') execution,
           json_extract(value,'$.cursor_sequence') sequence FROM json_each(?1)
    ) SELECT q.ordinal,e.id IS NOT NULL,e.retention_generation,e.saved_sequence,
        CASE WHEN length(CAST(e.output_encoding AS BLOB))<=128 THEN e.output_encoding END,
        CASE WHEN length(CAST(e.state AS BLOB))<=32 THEN e.state END,
        EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||q.execution),
        (SELECT length(bytes) FROM output_chunk WHERE execution_id=q.execution AND sequence=q.sequence),
        (SELECT sequence FROM output_chunk WHERE execution_id=q.execution ORDER BY sequence LIMIT 1),
        CASE WHEN e.id IS NOT NULL THEN length(CAST(e.output_encoding AS BLOB))>128 OR length(CAST(e.state AS BLOB))>32 ELSE 0 END
    FROM requested q LEFT JOIN shell_execution e ON e.id=q.execution ORDER BY q.ordinal";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RetainedExecutionRequest {
    pub(crate) execution: String,
    pub(crate) cursor_sequence: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetainedExecutionFacts {
    pub(crate) execution: String,
    pub(crate) exists: bool,
    pub(crate) generation: Option<i64>,
    pub(crate) saved_sequence: Option<i64>,
    pub(crate) encoding: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) tombstone: bool,
    pub(crate) cursor_chunk_length: Option<u64>,
    pub(crate) oldest_sequence: Option<u64>,
    pub(crate) metadata_limited: bool,
}
impl RawWitness {
    /// One batch statement, at most 200 exact requests. No bytes cross into Rust;
    /// cursor authorization stays with control::output's authoritative rules.
    pub(crate) fn for_retained_execution(
        db: &Connection,
        requests: &[RetainedExecutionRequest],
    ) -> rusqlite::Result<Vec<RetainedExecutionFacts>> {
        if requests.len() > 200 || requests.iter().any(|r| {
            r.execution.is_empty() || r.execution.len() > 128
                || r.cursor_sequence.is_some_and(|s| s > i64::MAX as u64)
        }) {
            return Err(rusqlite::Error::InvalidParameterName("retained execution observation limit".into()));
        }
        if requests.is_empty() { return Ok(Vec::new()); }
        let parameter = serde_json::to_string(requests)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        db.prepare(RETAINED_EXECUTION_FACTS_SQL)?.query_map([parameter], |row| {
            let ordinal = row.get::<_,usize>(0)?;
            let request = requests.get(ordinal).ok_or(rusqlite::Error::InvalidQuery)?;
            Ok(RetainedExecutionFacts {
                execution:request.execution.clone(),exists:row.get(1)?,generation:row.get(2)?,saved_sequence:row.get(3)?,
                encoding:row.get(4)?,state:row.get(5)?,tombstone:row.get(6)?,cursor_chunk_length:row.get(7)?,oldest_sequence:row.get(8)?,metadata_limited:row.get(9)?,
            })
        })?.collect()
    }
}

#[cfg(test)]
mod retained_execution_tests {
    use super::*;
    fn fixture() -> Connection {
        let db=Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE shell_execution(id TEXT PRIMARY KEY,retention_generation INTEGER,saved_sequence INTEGER,output_encoding TEXT,state TEXT);
          CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT);
          CREATE TABLE output_chunk(execution_id TEXT,sequence INTEGER,bytes BLOB,PRIMARY KEY(execution_id,sequence));
          INSERT INTO shell_execution VALUES('e',2,1,'UTF-8','running');
          INSERT INTO output_chunk VALUES('e',0,x'616263');").unwrap();db
    }
    #[test]
    fn retained_metadata_growth_terminal_and_tombstone_do_not_read_output_bytes() {
        let db=fixture();let requests=[RetainedExecutionRequest { execution:"e".into(),cursor_sequence:Some(0) }];
        let original=RawWitness::for_retained_execution(&db,&requests).unwrap().remove(0);
        assert!(original.exists);assert_eq!(original.cursor_chunk_length,Some(3));assert_eq!(original.oldest_sequence,Some(0));
        db.execute_batch("INSERT INTO output_chunk VALUES('e',1,x'646566');UPDATE shell_execution SET saved_sequence=2,state='exited';").unwrap();
        let next=RawWitness::for_retained_execution(&db,&requests).unwrap().remove(0);
        assert_eq!(next.generation,original.generation);assert_eq!(next.saved_sequence,Some(2));assert_eq!(next.cursor_chunk_length,Some(3));
        assert_eq!(next.state.as_deref(),Some("exited"));assert!(!next.tombstone);
        db.execute_batch("INSERT INTO app_meta VALUES('raw_tombstone.e','corrupt marker deliberately not decoded');DELETE FROM output_chunk WHERE sequence=0;").unwrap();
        let gone=RawWitness::for_retained_execution(&db,&requests).unwrap().remove(0);
        assert!(gone.tombstone);assert_eq!(gone.cursor_chunk_length,None);assert_eq!(gone.oldest_sequence,Some(1));
        assert_eq!(original.cursor_chunk_length,Some(3));
    }
    #[test]
    fn retained_batch_has_indexed_points_and_pre_copy_limits() {
        let db=fixture();
        let requests=[RetainedExecutionRequest{execution:"e".into(),cursor_sequence:None},RetainedExecutionRequest{execution:"missing".into(),cursor_sequence:None}];
        let before=db.total_changes();db.pragma_update(None,"query_only",true).unwrap();
        let facts=RawWitness::for_retained_execution(&db,&requests).unwrap();
        assert_eq!(facts.len(),2);assert!(!facts[1].exists);assert_eq!(db.total_changes(),before);
        let plan=db.prepare(&format!("EXPLAIN QUERY PLAN {RETAINED_EXECUTION_FACTS_SQL}")).unwrap().query_map([serde_json::to_string(&requests).unwrap()],|r|r.get::<_,String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap().join("\n");
        for index in ["sqlite_autoindex_shell_execution_1","sqlite_autoindex_app_meta_1","sqlite_autoindex_output_chunk_1"] {assert!(plan.contains(index),"{plan}");}
        assert!(RawWitness::for_retained_execution(&db,&vec![requests[0].clone();201]).is_err());
        assert!(RawWitness::for_retained_execution(&db,&[RetainedExecutionRequest{execution:"x".repeat(129),cursor_sequence:None}]).is_err());
    }
}
