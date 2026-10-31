//! Bounded parent-Thread lineage observations; Run ownership is not ancestry.
use crate::{
    activity::ActivityPhase,
    control::db::thread_summary::{self, RawActivity},
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::Connection;
use std::collections::{BTreeSet, HashSet};

const ROOT_LIMIT: usize = 2;
const MEMBER_LIMIT: usize = 256;
// First-child/next-sibling successor seeks admit at most two frontier rows per
// visit. A million siblings never enter the recursive queue in one expansion.
const WALK_SQL: &str = r"
WITH RECURSIVE root AS MATERIALIZED (
 SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL
), walk(id,parent,depth,path,cycle) AS (
 SELECT c.id,c.parent_thread_id,1,json_array(?1,c.id),c.id=?1
 FROM root CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=root.id)
 UNION ALL
 SELECT c.id,c.parent_thread_id,w.depth+1,json_insert(w.path,'$[#]',c.id),
        EXISTS(SELECT 1 FROM json_each(w.path) WHERE value=c.id)
 FROM walk w CROSS JOIN thread c ON c.id=(SELECT min(id) FROM thread WHERE parent_thread_id=w.id)
 WHERE w.depth<8 AND NOT w.cycle
 UNION ALL
 SELECT s.id,s.parent_thread_id,w.depth,json_insert(json_remove(w.path,'$[#-1]'),'$[#]',s.id),
        EXISTS(SELECT 1 FROM json_each(json_remove(w.path,'$[#-1]')) WHERE value=s.id)
 FROM walk w CROSS JOIN thread s ON s.id=(SELECT min(id) FROM thread WHERE parent_thread_id=w.parent AND id>w.id)
 ORDER BY 3,1 LIMIT 257
), numbered AS MATERIALIZED (
 SELECT *,row_number()OVER(ORDER BY depth,id) AS position FROM walk
), bounds AS MATERIALIZED (
 SELECT (SELECT count(*) FROM numbered)>256 AS row_limited,
        EXISTS(SELECT 1 FROM numbered w WHERE w.position<=256 AND w.depth=8 AND NOT w.cycle
          AND EXISTS(SELECT 1 FROM thread child WHERE child.parent_thread_id=w.id)) AS depth_limited,
        EXISTS(SELECT 1 FROM numbered WHERE cycle) AS cycle
)
";

pub(super) struct RawLineage {
    root: String,
    available: bool,
    rows: Vec<RawMember>,
    truncated: bool,
    cycle: bool,
}
impl RawLineage {
    pub(super) fn root(&self) -> &str {
        &self.root
    }
    pub(super) fn witnesses(&self) -> usize {
        self.rows.len()
    }
    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }
}
/// Bounded saved membership for another consumer in the same transaction.
/// Count classification remains in prepare outside the database worker.
pub(super) fn membership(raw: &RawLineage) -> (bool, Vec<String>) {
    let mut seen = HashSet::new();
    (
        raw.available,
        raw.rows
            .iter()
            .filter(|row| {
                row.position <= MEMBER_LIMIT
                    && !row.deleted
                    && !row.cycle
                    && row.id != raw.root
                    && seen.insert(row.id.clone())
            })
            .map(|row| row.id.clone())
            .collect(),
    )
}
struct RawMember {
    id: String,
    position: usize,
    deleted: bool,
    cycle: bool,
    activity: RawActivity,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DescendantSnapshot {
    pub root: String,
    pub available: bool,
    pub active: u64,
    pub recovery: u64,
    pub truncated: bool,
    pub cycle: bool,
    /// Nondeleted descendants in the bounded scope, independent of Run owner.
    pub threads: Vec<String>,
    /// Includes deleted bridges and the row sentinel for change invalidation.
    pub members: Vec<String>,
}
#[derive(Clone, Debug)]
pub(super) struct LineageScope {
    pub root: String,
    pub members: Vec<String>,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
}
fn invalid(message: &str) -> StorageError {
    ApiError::new(ErrorCode::InputLimit, message).into()
}
fn query() -> String {
    // Count membership is unchanged by inflight/retry/tools/owned refinements.
    // Do not scan irrelevant global tool-group or attempt history per member.
    format!(
        "{WALK_SQL}SELECT root.id,w.id,w.position,t.deleted_at IS NOT NULL,w.cycle,
      bounds.row_limited OR bounds.depth_limited OR bounds.cycle,bounds.cycle,
      latest.state,u.state,0,NULL,NULL,0 FROM root CROSS JOIN bounds LEFT JOIN numbered w ON 1
      LEFT JOIN thread t ON t.id=w.id {} ORDER BY w.position",
        thread_summary::RUN_TURN_JOINS
    )
}
/// One statement for each actual visible root, at most two in one caller-owned
/// transaction. No titles, source bodies or per-descendant reader calls.
pub(super) fn read_roots_tx(
    db: &Connection,
    roots: &[String],
) -> Result<Vec<RawLineage>, StorageError> {
    if roots.len() > ROOT_LIMIT || roots.iter().any(|id| !valid_id(id)) {
        return Err(invalid("Lineage scope exceeds its two-root identity bound"));
    }
    let sql = query();
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        if !seen.insert(root) {
            continue;
        }
        let mut raw = RawLineage {
            root: root.clone(),
            available: false,
            rows: Vec::new(),
            truncated: false,
            cycle: false,
        };
        let mut statement = db.prepare(&sql)?;
        let mut rows = statement.query([root])?;
        while let Some(row) = rows.next()? {
            raw.available = true;
            raw.truncated = row.get(5)?;
            raw.cycle = row.get(6)?;
            if let Some(id) = row.get::<_, Option<String>>(1)? {
                raw.rows.push(RawMember {
                    id,
                    position: row.get::<_, i64>(2)? as usize,
                    deleted: row.get(3)?,
                    cycle: row.get(4)?,
                    activity: thread_summary::decode_activity(row, 7)?,
                });
            }
        }
        result.push(raw);
    }
    Ok(result)
}
pub(super) fn prepare(raw: RawLineage) -> DescendantSnapshot {
    let mut result = DescendantSnapshot {
        root: raw.root,
        available: raw.available,
        active: 0,
        recovery: 0,
        truncated: raw.truncated,
        cycle: raw.cycle,
        threads: Vec::new(),
        members: Vec::new(),
    };
    let mut seen = HashSet::new();
    for row in raw.rows {
        if row.id == result.root || row.cycle || !seen.insert(row.id.clone()) {
            continue;
        }
        result.members.push(row.id.clone());
        if row.position > MEMBER_LIMIT || row.deleted {
            continue;
        }
        result.threads.push(row.id);
        match thread_summary::coarse_activity(&row.activity) {
            ActivityPhase::Recovery => result.recovery += 1,
            ActivityPhase::Idle
            | ActivityPhase::Completed
            | ActivityPhase::Stopped
            | ActivityPhase::Failed => {}
            _ => result.active += 1,
        }
    }
    result
}

const AFFECTED_SQL: &str = r"
WITH RECURSIVE requested AS MATERIALIZED(SELECT DISTINCT value AS id FROM json_each(?1)),
wanted AS MATERIALIZED(SELECT json_extract(value,'$[0]') AS root,json_extract(value,'$[1]') AS id FROM json_each(?2)),
ancestors(id,parent,depth,path) AS (
 SELECT t.id,t.parent_thread_id,0,json_array(t.id) FROM requested q CROSS JOIN thread t ON t.id=q.id
 UNION ALL
 SELECT t.id,t.parent_thread_id,a.depth+1,json_insert(a.path,'$[#]',t.id)
 FROM ancestors a CROSS JOIN thread t ON t.id=a.parent
 WHERE a.depth<8 AND NOT EXISTS(SELECT 1 FROM json_each(a.path) WHERE value=t.id)
)
SELECT DISTINCT wanted.root FROM ancestors a JOIN wanted ON wanted.id=a.id ORDER BY wanted.root";
/// Existing cached identities are positive membership evidence. Otherwise one
/// bounded PK-upward batch finds new descendants/frontiers without a tree scan.
pub(super) fn affected_tx(
    db: &Connection,
    changed: &[String],
    scopes: &[LineageScope],
) -> Result<Vec<String>, StorageError> {
    if changed.len() > 128
        || changed.iter().any(|id| !valid_id(id))
        || scopes.len() > ROOT_LIMIT
        || scopes.iter().any(|scope| {
            !valid_id(&scope.root)
                || scope.members.len() > MEMBER_LIMIT + 1
                || scope.members.iter().any(|id| !valid_id(id))
        })
    {
        return Err(invalid("Lineage invalidation exceeds its bounded scope"));
    }
    let mut affected = BTreeSet::new();
    let mut remaining = Vec::new();
    for scope in scopes {
        if changed
            .iter()
            .any(|id| id == &scope.root || scope.members.contains(id))
        {
            affected.insert(scope.root.clone());
        } else {
            remaining.push(scope);
        }
    }
    if changed.is_empty() || remaining.is_empty() {
        return Ok(affected.into_iter().collect());
    }
    let wanted = remaining
        .into_iter()
        .flat_map(|scope| {
            std::iter::once((&scope.root, &scope.root))
                .chain(scope.members.iter().map(move |id| (&scope.root, id)))
        })
        .collect::<Vec<_>>();
    let requested = serde_json::to_string(changed).expect("Changed identities serialize");
    let wanted = serde_json::to_string(&wanted).expect("Lineage identities serialize");
    let roots = db
        .prepare(AFFECTED_SQL)?
        .query_map(rusqlite::params![requested, wanted], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    affected.extend(roots);
    Ok(affected.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use rusqlite::{StatementStatus, params};
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db
    }
    fn thread(db: &Connection, id: &str, parent: Option<&str>) {
        db.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,'/','','p','m','now')",params![id,parent]).unwrap();
    }
    fn run(db: &Connection, thread: &str, state: &str, turn: &str) {
        let run = format!("run-{thread}");
        let node = format!("turn-{thread}");
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,?3,'now')",
            params![run, thread, state],
        )
        .unwrap();
        db.execute(
            "INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,?4,'now')",
            params![node, thread, run, turn],
        )
        .unwrap();
    }
    fn one(db: &Connection, root: &str) -> DescendantSnapshot {
        prepare(read_roots_tx(db, &[root.into()]).unwrap().pop().unwrap())
    }
    #[test]
    fn saved_descendants_are_readable_on_a_readonly_connection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lineage.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(crate::storage::CURRENT_SCHEMA)
            .unwrap();
        thread(&writer, "root", None);
        thread(&writer, "child", Some("root"));
        run(&writer, "child", "live", "running");
        writer.close().unwrap();
        let reader =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(reader.execute("DELETE FROM thread", []).is_err());
        let trace = Statements::new(&reader);
        let snapshot = one(&reader, "root");
        assert_eq!(trace.count(), 1);
        assert_eq!(snapshot.threads, ["child"]);
        assert_eq!((snapshot.active, snapshot.recovery), (1, 0));
        assert!(!snapshot.truncated);
    }
    #[test]
    fn one_statement_classifies_unowned_runs_and_traverses_deleted_bridges() {
        let db = fixture();
        thread(&db, "root", None);
        for (id, parent, state, turn) in [
            ("active", "root", "live", "running"),
            ("recover", "root", "live", "recovery_wait"),
            ("stopping", "root", "stopping", "running"),
            ("done", "root", "completed", "completed"),
            ("stopped", "root", "stopped", "stopped"),
            ("failed", "root", "failed", "failed"),
            ("bridge", "root", "completed", "completed"),
            ("grandchild", "bridge", "live", "running"),
        ] {
            thread(&db, id, Some(parent));
            run(&db, id, state, turn);
        }
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='bridge'", [])
            .unwrap();
        let trace = Statements::new(&db);
        let raw = read_roots_tx(&db, &["root".into()]).unwrap().pop().unwrap();
        assert_eq!(trace.count(), 1);
        drop(trace);
        // Facts stay owned after later writes and connection release.
        db.execute("UPDATE run SET state='completed'", []).unwrap();
        drop(db);
        let snapshot = prepare(raw);
        assert!(snapshot.available);
        assert_eq!((snapshot.active, snapshot.recovery), (3, 1));
        assert!(!snapshot.truncated);
        assert!(snapshot.members.contains(&"bridge".into()));
        assert!(!snapshot.threads.contains(&"bridge".into()));
        assert!(snapshot.threads.contains(&"grandchild".into()));
    }
    #[test]
    fn broad_bfs_depth_frontier_cycles_missing_and_exact_zero_are_distinct() {
        let mut db = fixture();
        thread(&db, "wide", None);
        let tx = db.transaction().unwrap();
        for n in 0..1000 {
            thread(&tx, &format!("child-{n:04}"), Some("wide"));
        }
        tx.commit().unwrap();
        let wide = one(&db, "wide");
        assert_eq!(wide.threads.len(), 256);
        assert_eq!(wide.members.len(), 257);
        assert!(wide.truncated);
        assert_eq!(wide.threads[0], "child-0000");
        assert_eq!(wide.threads[255], "child-0255");
        thread(&db, "deep", None);
        let mut parent = "deep".to_string();
        for n in 1..=9 {
            let id = format!("depth-{n}");
            thread(&db, &id, Some(&parent));
            parent = id;
        }
        let deep = one(&db, "deep");
        assert_eq!(deep.threads.len(), 8);
        assert!(deep.truncated);
        assert!(!deep.threads.contains(&"depth-9".into()));
        thread(&db, "cycle", None);
        thread(&db, "a", Some("cycle"));
        thread(&db, "b", Some("a"));
        db.execute(
            "UPDATE thread SET parent_thread_id='b' WHERE id='cycle'",
            [],
        )
        .unwrap();
        let cycle = one(&db, "cycle");
        assert!(cycle.cycle && cycle.truncated);
        assert_eq!(cycle.threads, vec!["a", "b"]);
        assert!(!cycle.members.contains(&"cycle".into()));
        thread(&db, "empty", None);
        let empty = one(&db, "empty");
        assert!(empty.available && !empty.truncated);
        assert_eq!((empty.active, empty.recovery), (0, 0));
        assert!(!one(&db, "missing").available);
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='empty'", [])
            .unwrap();
        assert!(!one(&db, "empty").available);
    }
    #[test]
    fn invalidation_finds_new_descendants_and_depth_frontiers_without_global_scans() {
        let db = fixture();
        thread(&db, "root", None);
        let mut parent = "root".to_string();
        for n in 1..=8 {
            let id = format!("d{n}");
            thread(&db, &id, Some(&parent));
            parent = id;
        }
        let old = one(&db, "root");
        let scope = LineageScope {
            root: "root".into(),
            members: old.members,
        };
        let trace = Statements::new(&db);
        assert_eq!(
            affected_tx(&db, &["d1".into()], std::slice::from_ref(&scope)).unwrap(),
            vec!["root"]
        );
        assert_eq!(trace.count(), 0);
        drop(trace);
        thread(&db, "new-child", Some("d8"));
        thread(&db, "unrelated", None);
        let trace = Statements::new(&db);
        assert_eq!(
            affected_tx(
                &db,
                &["new-child".into(), "unrelated".into()],
                std::slice::from_ref(&scope)
            )
            .unwrap(),
            vec!["root"]
        );
        assert_eq!(trace.count(), 1);
        drop(trace);
        assert!(
            affected_tx(&db, &["unrelated".into()], std::slice::from_ref(&scope))
                .unwrap()
                .is_empty()
        );
        let trace = Statements::new(&db);
        assert!(affected_tx(&db, &vec!["x".into(); 129], &[scope]).is_err());
        assert!(read_roots_tx(&db, &vec!["root".into(); 3]).is_err());
        assert_eq!(trace.count(), 0);
    }
    #[test]
    fn indexed_successor_and_latest_fact_work_ignore_unrelated_cardinality() {
        let mut db = fixture();
        thread(&db, "root", None);
        for n in 0..300 {
            thread(&db, &format!("z-child-{n:04}"), Some("root"));
        }
        let sql = query();
        let measure = |db: &Connection| {
            let mut statement = db.prepare(&sql).unwrap();
            let rows = statement
                .query_map(["root"], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows.len(), 257);
            statement.get_status(StatementStatus::VmStep)
        };
        let before = measure(&db);
        let tx = db.transaction().unwrap();
        for n in 0..10_000 {
            thread(&tx, &format!("a-unrelated-{n:05}"), None);
        }
        tx.commit().unwrap();
        let after = measure(&db);
        assert!(after <= before + 100, "{before}->{after}");
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(["root"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter()
                .filter(|step| step.contains("thread_parent") && step.contains("SEARCH"))
                .count()
                >= 3,
            "{plan:?}"
        );
        assert!(
            plan.iter().any(|step| step.contains("run_thread_created")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("tool_group")
                || step.contains("model_attempt")
                || step.contains("shell_execution")),
            "count-only classification must not inspect irrelevant work: {plan:?}"
        );
    }
    #[test]
    fn two_actual_roots_share_callers_transaction_without_per_descendant_jobs() {
        let db = fixture();
        thread(&db, "r1", None);
        thread(&db, "r2", None);
        thread(&db, "c", Some("r1"));
        let trace = Statements::new(&db);
        let rows = read_roots_tx(&db, &["r1".into(), "r2".into()]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(trace.count(), 2);
    }
    #[test]
    #[ignore = "P4 native timing observation; run explicitly without competing builds"]
    fn measured_256_descendants_one_statement_p95_under_fifty_ms() {
        for depth in [1, 8] {
            let mut db = fixture();
            thread(&db, "root", None);
            let tx = db.transaction().unwrap();
            let count = if depth == 1 { 300 } else { 256 };
            for n in 0..count {
                let id = format!("child-{n:04}");
                let parent = if depth == 8 && n % 8 != 0 {
                    format!("child-{:04}", n - 1)
                } else {
                    "root".to_owned()
                };
                thread(&tx, &id, Some(&parent));
                run(
                    &tx,
                    &id,
                    "live",
                    if n % 10 == 0 {
                        "recovery_wait"
                    } else {
                        "running"
                    },
                );
            }
            tx.commit().unwrap();
            let mut samples = Vec::new();
            for n in 0..40 {
                let start = std::time::Instant::now();
                let snapshot = one(&db, "root");
                assert_eq!(snapshot.threads.len(), 256);
                assert_eq!(snapshot.truncated, depth == 1);
                if n >= 10 {
                    samples.push(start.elapsed());
                }
            }
            samples.sort();
            let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
            println!(
                "P4_LINEAGE depth={depth} samples={} p50_us={} p95_us={} max_us={}",
                samples.len(),
                samples[samples.len() / 2].as_micros(),
                p95.as_micros(),
                samples.last().unwrap().as_micros()
            );
            assert!(
                p95 < std::time::Duration::from_millis(50),
                "depth={depth} p95={p95:?}"
            );
        }
    }
    #[test]
    fn neutral_activity_refinements_preserve_all_native_count_categories() {
        let category = |phase: ActivityPhase| match phase {
            ActivityPhase::Recovery => 1,
            ActivityPhase::Idle
            | ActivityPhase::Completed
            | ActivityPhase::Stopped
            | ActivityPhase::Failed => 0,
            _ => 2,
        };
        for run in [
            None,
            Some("live"),
            Some("stopping"),
            Some("completed"),
            Some("stopped"),
            Some("failed"),
        ] {
            for turn in [
                None,
                Some("preparing"),
                Some("running"),
                Some("waiting"),
                Some("recovery_wait"),
                Some("completed"),
                Some("stopped"),
                Some("failed"),
            ] {
                let neutral = category(crate::activity::classify(
                    run, turn, false, false, false, false,
                ));
                for owned in [false, true] {
                    for inflight in [false, true] {
                        for retry in [false, true] {
                            for tools in [false, true] {
                                let full = crate::activity::classify(
                                    run, turn, owned, inflight, retry, tools,
                                );
                                assert_eq!(
                                    neutral,
                                    category(full),
                                    "run={run:?} turn={turn:?} owned={owned} inflight={inflight} retry={retry} tools={tools}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    #[ignore = "P4 optional million-sibling stress; large in-memory synthetic fixture"]
    fn million_siblings_never_enter_one_recursive_expansion() {
        let db = fixture();
        thread(&db, "root", None);
        db.execute_batch(
            "BEGIN; WITH RECURSIVE n(x)AS(VALUES(0)UNION ALL SELECT x+1 FROM n WHERE x<999999)
          INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at)
          SELECT printf('wide-%07d',x),'root','/','','p','m','now' FROM n; COMMIT;",
        )
        .unwrap();
        let sql = query();
        let mut statement = db.prepare(&sql).unwrap();
        let ids = statement
            .query_map(["root"], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids.len(), 257);
        let vm = statement.get_status(StatementStatus::VmStep);
        println!("P4_MILLION_SIBLINGS returned={} vm_steps={vm}", ids.len());
        assert!(
            vm < 100_000,
            "successor work must not scale with one million siblings: {vm}"
        );
    }
}
