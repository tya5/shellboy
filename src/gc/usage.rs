//! Logical byte accounting and physical storage observations.
use super::{Result, sql_error};
use crate::protocol::{ApiError, ErrorCode};
use rusqlite::Connection;
use std::path::Path;

#[derive(Default)]
pub(super) struct Logical {
    pub(super) history: u64,
    pub(super) raw: u64,
    pub(super) index: u64,
    pub(super) managed: u64,
    pub(super) reusable: u64,
}
impl Logical {
    pub(super) fn total(&self) -> u64 {
        self.history
            .saturating_add(self.raw)
            .saturating_add(self.index)
            .saturating_add(self.managed)
    }
}

fn sum(db: &Connection, sql: &str) -> Result<u64> {
    db.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map(|value| value as u64)
        .map_err(sql_error)
}
// All payload measurements use narrow derived indexes. The explicit index
// selection prevents a planner choice from reintroducing body-table scans.
const HISTORY_QUERIES: [&str; 10] = [
    "SELECT COALESCE(SUM(octet_length(payload_json)),0) FROM node INDEXED BY gc_bytes_node",
    "SELECT COALESCE(SUM(octet_length(payload_json)),0) FROM output_item INDEXED BY gc_bytes_output_item",
    "SELECT COALESCE(SUM(octet_length(body)),0) FROM instruction_snapshot INDEXED BY gc_bytes_instruction_snapshot",
    "SELECT COALESCE(SUM(octet_length(body_json)),0) FROM draft INDEXED BY gc_bytes_draft",
    "SELECT COALESCE(SUM(COALESCE(octet_length(input_manifest_json),0)+COALESCE(octet_length(error_json),0)+COALESCE(octet_length(usage_json),0)),0) FROM model_attempt INDEXED BY gc_bytes_model_attempt",
    "SELECT COALESCE(SUM(octet_length(payload_ref_json)+octet_length(source_json)),0) FROM event INDEXED BY gc_bytes_event",
    "SELECT COALESCE(SUM(octet_length(sender_json)+COALESCE(octet_length(result_json),0)),0) FROM receipt INDEXED BY gc_bytes_receipt",
    "SELECT COALESCE(SUM(octet_length(origin_json)+COALESCE(octet_length(result_ref_json),0)),0) FROM operation_receipt INDEXED BY gc_bytes_operation_receipt",
    "SELECT COALESCE(SUM(octet_length(arguments_json)),0) FROM tool_call INDEXED BY gc_bytes_tool_call",
    "SELECT COALESCE(SUM(octet_length(value)),0) FROM app_meta INDEXED BY gc_bytes_app_meta_history WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%'",
];
const RAW_QUERY: &str =
    "SELECT COALESCE(SUM(length(bytes)),0) FROM output_chunk INDEXED BY gc_bytes_output_chunk";
const MANAGED_QUERY: &str = "SELECT COALESCE(SUM(raw_size),0) FROM managed_file INDEXED BY gc_bytes_managed_file WHERE state<>'deleted'";
const INDEX_META_QUERY: &str = "SELECT COALESCE(SUM(octet_length(value)),0) FROM app_meta INDEXED BY gc_bytes_app_meta_index WHERE key LIKE 'index.%'";
const VECTOR_QUERY: &str = "SELECT COALESCE(SUM(s.dimension*4),0) FROM embedding_chunk c JOIN index_space s ON s.id=c.space_id";

pub(super) fn logical_usage(db: &Connection) -> Result<Logical> {
    let history = HISTORY_QUERIES.iter().try_fold(0u64, |total, query| {
        sum(db, query).map(|bytes| total.saturating_add(bytes))
    })?;
    let raw = sum(db, RAW_QUERY)?;
    let managed = sum(db, MANAGED_QUERY)?;
    let index = sum(db, VECTOR_QUERY)?.saturating_add(sum(db, INDEX_META_QUERY)?);
    let free: u64 = db
        .query_row("PRAGMA freelist_count", [], |row| row.get::<_, i64>(0))
        .map_err(sql_error)? as u64;
    let page: u64 = db
        .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
        .map_err(sql_error)? as u64;
    Ok(Logical {
        history,
        raw,
        index,
        managed,
        reusable: free.saturating_mul(page),
    })
}
pub(super) fn physical_usage(directory: &Path) -> Result<(u64, u64, u64, u64, Option<u64>)> {
    fn size(path: &Path) -> Result<u64> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => Ok(meta.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(ApiError::new(ErrorCode::IoFailed, error.to_string())),
        }
    }
    fn files(path: &Path) -> Result<u64> {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(ApiError::new(ErrorCode::IoFailed, error.to_string())),
        };
        let mut total = 0u64;
        for entry in entries {
            let entry =
                entry.map_err(|error| ApiError::new(ErrorCode::IoFailed, error.to_string()))?;
            let meta = std::fs::symlink_metadata(entry.path())
                .map_err(|error| ApiError::new(ErrorCode::IoFailed, error.to_string()))?;
            if !meta.is_dir() {
                total = total.saturating_add(meta.len());
            }
        }
        Ok(total)
    }
    Ok((
        size(&directory.join("shellboy.db"))?,
        size(&directory.join("shellboy.db-wal"))?,
        files(&directory.join("managed/clipboard"))?.saturating_add(files(&directory.join("tmp"))?),
        files(&directory.join("logs"))?,
        fs2::available_space(directory).ok(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_QUERIES: [&str; 14] = [
        "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM node",
        "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM output_item",
        "SELECT COALESCE(SUM(length(CAST(body AS BLOB))),0) FROM instruction_snapshot",
        "SELECT COALESCE(SUM(length(CAST(body_json AS BLOB))),0) FROM draft",
        "SELECT COALESCE(SUM(COALESCE(length(CAST(input_manifest_json AS BLOB)),0)+COALESCE(length(CAST(error_json AS BLOB)),0)+COALESCE(length(CAST(usage_json AS BLOB)),0)),0) FROM model_attempt",
        "SELECT COALESCE(SUM(length(CAST(payload_ref_json AS BLOB))+length(CAST(source_json AS BLOB))),0) FROM event",
        "SELECT COALESCE(SUM(length(CAST(sender_json AS BLOB))+COALESCE(length(CAST(result_json AS BLOB)),0)),0) FROM receipt",
        "SELECT COALESCE(SUM(length(CAST(origin_json AS BLOB))+COALESCE(length(CAST(result_ref_json AS BLOB)),0)),0) FROM operation_receipt",
        "SELECT COALESCE(SUM(length(CAST(arguments_json AS BLOB))),0) FROM tool_call",
        "SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM app_meta WHERE key LIKE 'summary.part.%' OR key LIKE 'wait.pending.%' OR key LIKE 'compact.result.%' OR key LIKE 'revisit.%'",
        "SELECT COALESCE(SUM(length(bytes)),0) FROM output_chunk",
        "SELECT COALESCE(SUM(raw_size),0) FROM managed_file WHERE state<>'deleted'",
        "SELECT COALESCE(SUM(s.dimension*4),0) FROM embedding_chunk c JOIN index_space s ON s.id=c.space_id",
        "SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM app_meta WHERE key LIKE 'index.%'",
    ];

    fn current_schema() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        initialize_current(&db);
        db
    }

    fn initialize_current(db: &Connection) {
        // Deliberately isolate accounting from FK graph setup; real GC/Store
        // integration tests exercise the enabled-FK mutation paths separately.
        db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    }

    fn seed(db: &Connection) {
        db.execute_batch(r#"
INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('n','message','now','{"text":"日本語🌿"}',999);
INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('a',0,'message','completed','{"text":"audit 日本語"}','n');
INSERT INTO instruction_snapshot(id,source_path,content_hash,body,created_at)VALUES('i','/fixture','hash','instructions 日本語'||char(0)||'tail','now');
INSERT INTO draft(id,revision,body_json,state,updated_at)VALUES('d',0,'{"text":"draft 🌿"}','editing','now');
INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,usage_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','["日本語"]',NULL,'{"tokens":2}','now');
INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('e','fixture','{"source":"日本語"}','t','{"payload":"🌿"}','now');
INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES('r','client','{"sender":"🌿"}','t','n','accepted','now',NULL);
INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at,reserved_bytes)VALUES('o','instance','{"origin":"日本語"}','hash','pending','{"result":"🌿"}','now',1048576);
INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g','c',0,'shell','{"command":"echo 日本語"}','completed');
INSERT INTO output_chunk VALUES('x',0,'stdout',0,x'00ff80fe','now');
INSERT INTO managed_file(id,absolute_path,state,raw_size,created_at)VALUES('ready','/ready','ready',17,'now'),('deleting','/deleting','deleting',19,'now'),('deleted','/deleted','deleted',999,'now'),('creating','/creating','creating',NULL,'now'),('failed','/failed','failed',23,'now');
INSERT INTO index_space VALUES('s1','p','m',3,'v','active','vec1'),('s2','p','m',7,'v','inactive','vec2'),('empty','p','m',9223372036854775807,'v','candidate','empty_vec');
INSERT INTO embedding_chunk VALUES('c1','j1','s1','n',0,0,1,'hash',1),('c2','j2','s1','n',0,1,2,'hash',2),('c3','j3','s2','n',0,0,1,'hash',1);
INSERT INTO app_meta(key,value)VALUES('summary.part.fixture','summary 日本語'),('wait.pending.fixture','wait 🌿'),('compact.result.fixture','compact 日本語'),('revisit.fixture','revisit 日本語'),('index.preview.fixture','preview 🌿'),('index.input.fixture','input 日本語'),('index.bootstrap.bad','broken JSON'),('unrelated.fixture','large uncounted 日本語');
"#).unwrap();
    }

    fn assert_reference(db: &Connection) {
        let actual = logical_usage(db).unwrap();
        let expected: Vec<_> = LEGACY_QUERIES.iter().map(|q| sum(db, q).unwrap()).collect();
        assert_eq!(actual.history, expected[..10].iter().sum::<u64>());
        assert_eq!(actual.raw, expected[10]);
        assert_eq!(actual.managed, expected[11]);
        assert_eq!(actual.index, expected[12] + expected[13]);
    }

    #[test]
    fn byte_indexes_match_reference_payload_measurements_after_mutation_and_rollback() {
        let mut db = current_schema();
        seed(&db);
        let payload: i64 = db
            .query_row(
                "SELECT length(CAST(payload_json AS BLOB)) FROM node",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(
            payload, 999,
            "fixture must not assume payload_bytes is authoritative"
        );
        assert_reference(&db);
        let before = logical_usage(&db).unwrap().total();
        {
            let tx = db.transaction().unwrap();
            tx.execute_batch("UPDATE node SET payload_json=NULL,payload_bytes=0; UPDATE output_item SET payload_json='{\"type\":\"tombstone\"}'; DELETE FROM output_chunk; UPDATE managed_file SET state='deleted'; UPDATE model_attempt SET error_json='{\"error\":\"日本語\"}'; UPDATE operation_receipt SET result_ref_json=NULL,reserved_bytes=0; DELETE FROM app_meta WHERE key LIKE 'index.%'; UPDATE index_space SET dimension=11 WHERE id='s1'; DELETE FROM embedding_chunk WHERE id='c3';").unwrap();
            assert_reference(&tx);
            assert_ne!(logical_usage(&tx).unwrap().total(), before);
            tx.rollback().unwrap();
        }
        assert_reference(&db);
        assert_eq!(logical_usage(&db).unwrap().total(), before);
        db.execute_batch("DELETE FROM node; DELETE FROM output_item; DELETE FROM instruction_snapshot; DELETE FROM draft; DELETE FROM model_attempt; DELETE FROM event; DELETE FROM receipt; DELETE FROM operation_receipt; DELETE FROM tool_call; DELETE FROM output_chunk; DELETE FROM managed_file; DELETE FROM embedding_chunk; DELETE FROM index_space; DELETE FROM app_meta;").unwrap();
        assert_reference(&db);
        assert_eq!(logical_usage(&db).unwrap().total(), 0);
    }

    #[test]
    fn logical_aggregation_uses_scalar_indexes_without_payload_functions() {
        let db = current_schema();
        seed(&db);
        for query in
            HISTORY_QUERIES
                .iter()
                .copied()
                .chain([RAW_QUERY, MANAGED_QUERY, INDEX_META_QUERY])
        {
            let mut plan = db.prepare(&format!("EXPLAIN {query}")).unwrap();
            let opcodes: Vec<String> = plan
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(
                !opcodes.iter().any(|op| op == "Function"),
                "payload functions returned to aggregation: {query}"
            );
            let mut plan = db.prepare(&format!("EXPLAIN QUERY PLAN {query}")).unwrap();
            let detail: Vec<String> = plan
                .query_map([], |row| row.get(3))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(
                detail.iter().any(|line| line.contains("gc_bytes_")),
                "missing narrow index: {detail:?}"
            );
        }
    }

    #[test]
    #[ignore = "controlled 10k/100k SQLite accounting/source lookup measurement; temporary DB only"]
    fn measure_indexed_accounting_and_source_lookup() {
        use std::time::Instant;
        fn median(mut samples: Vec<f64>) -> f64 {
            samples.sort_by(f64::total_cmp);
            samples[samples.len() / 2]
        }
        let temp = tempfile::tempdir().unwrap();
        let mut db = Connection::open(temp.path().join("measurement.sqlite")).unwrap();
        initialize_current(&db);
        // Seed-only speed setting on this disposable fixture; no production
        // commit/durability result is inferred from this read/lookup measurement.
        db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;")
            .unwrap();
        let payload = serde_json::json!({"text":"あ".repeat(1360)}).to_string();
        let mut previous = 0;
        println!(
            "sqlite={} payload_bytes={} existing host CPU load retained",
            db.query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0))
                .unwrap(),
            payload.len()
        );
        for count in [10_000, 100_000] {
            let tx = db.transaction().unwrap();
            {
                let mut node=tx.prepare("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'message','now',?2)").unwrap();
                let mut meta = tx
                    .prepare("INSERT INTO app_meta(key,value)VALUES(?1,?2)")
                    .unwrap();
                for i in previous..count {
                    node.execute(rusqlite::params![format!("n{i}"), payload])
                        .unwrap();
                    meta.execute(rusqlite::params![format!("index.preview.{i}"), payload])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
            let mut legacy = Vec::new();
            let mut indexed = Vec::new();
            let mut old_lookup = Vec::new();
            let mut new_lookup = Vec::new();
            for _ in 0..7 {
                let start = Instant::now();
                let expected = LEGACY_QUERIES
                    .iter()
                    .map(|q| sum(&db, q).unwrap())
                    .sum::<u64>();
                legacy.push(start.elapsed().as_secs_f64() * 1000.);
                let start = Instant::now();
                let actual = logical_usage(&db).unwrap().total();
                indexed.push(start.elapsed().as_secs_f64() * 1000.);
                assert_eq!(actual, expected);
                for (samples, queries) in [
                    (
                        &mut old_lookup,
                        [
                            "DELETE FROM app_meta WHERE CASE WHEN key LIKE 'index.bootstrap.%' THEN json_extract(value,'$.witness.node_id') END=?1",
                            "DELETE FROM app_meta WHERE CASE WHEN key LIKE 'index.auto.%' THEN json_extract(value,'$.node_id') END=?1",
                        ],
                    ),
                    (
                        &mut new_lookup,
                        [
                            "DELETE FROM app_meta INDEXED BY index_bootstrap_source WHERE key LIKE 'index.bootstrap.%' AND json_valid(value) AND json_extract(value,'$.witness.node_id')=?1",
                            "DELETE FROM app_meta INDEXED BY index_auto_source WHERE key LIKE 'index.auto.%' AND json_valid(value) AND json_extract(value,'$.node_id')=?1",
                        ],
                    ),
                ] {
                    let start = Instant::now();
                    for sql in queries {
                        assert_eq!(db.execute(sql, ["absent-source"]).unwrap(), 0);
                    }
                    samples.push(start.elapsed().as_secs_f64() * 1000.);
                }
            }
            println!(
                "rows={count} logical_bytes={} legacy_sum_ms={:.3} indexed_sum_ms={:.3} old_source_pair_ms={:.3} indexed_source_pair_ms={:.3}",
                logical_usage(&db).unwrap().total(),
                median(legacy),
                median(indexed),
                median(old_lookup),
                median(new_lookup)
            );
            previous = count;
        }
        let free: i64 = db
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .unwrap();
        let names:Vec<String>=db.prepare("SELECT name FROM sqlite_schema WHERE type='index' AND (name LIKE 'gc_bytes_%' OR name IN ('index_bootstrap_source','index_auto_source'))").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        for name in names {
            db.execute_batch(&format!("DROP INDEX {name}")).unwrap();
        }
        let freed: i64 = db
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))
            .unwrap();
        let page: i64 = db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        println!(
            "derived_indexes_allocated_bytes_100k={}",
            (freed - free) * page
        );
    }
}
