//! Current-view raw header facts and bounded title evidence, never CLI policy.
use crate::{
    context::ExecutionContext,
    protocol::{ApiError, ErrorCode, ThreadDetails},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension};

const PREFIX_ROWS: usize = 128;
const SOURCE_BYTES: usize = 128 * 1024;
const WITNESS_BYTES: usize = 512 * 1024;
const LIMITED: &str = "名称未設定（元本文の取得範囲外）";

pub(super) struct RawHeader {
    details: ThreadDetails,
    updated_at: String,
    title: Option<TitleWitness>,
}
#[derive(Default)]
pub(super) struct TitleWitness {
    path: Candidates,
    pending: Candidates,
}
#[derive(Default)]
struct Candidates {
    rows: Vec<Candidate>,
    truncated: bool,
    copied: usize,
}
struct Candidate {
    user: bool,
    body: Option<String>,
    omitted: bool,
}

// The indexed membership prefixes are limited BEFORE kind/body filtering. A
// path with millions of tool rows cannot turn title discovery into a full scan.
const PATH_PREFIX: &str = "prefix AS MATERIALIZED (
    SELECT p.node_id,p.ordinal AS position FROM thread_path p
    WHERE p.thread_id=?1 ORDER BY p.ordinal LIMIT 129
), bounded AS MATERIALIZED (
    SELECT node_id,position FROM prefix ORDER BY position LIMIT 128
), witnesses AS MATERIALIZED (
    SELECT b.position,n.id,n.kind,n.kind='user' AS eligible,
           CASE WHEN n.kind='user' THEN octet_length(n.payload_json) END AS bytes
    FROM bounded b JOIN node n ON n.id=b.node_id WHERE n.kind='user'
)";

// Per-state prefixes seek receipt_target. Final ordering on the bounded union
// agrees with CLI created_at/node-rowid ordering when neither prefix overflows.
const PENDING_PREFIX: &str = "accepted AS MATERIALIZED (
    SELECT r.message_node_id AS node_id,r.created_at FROM receipt r INDEXED BY receipt_target
    WHERE r.target_thread_id=?1 AND r.state='accepted' ORDER BY r.created_at,r.rowid LIMIT 129
), held AS MATERIALIZED (
    SELECT r.message_node_id AS node_id,r.created_at FROM receipt r INDEXED BY receipt_target
    WHERE r.target_thread_id=?1 AND r.state='held' ORDER BY r.created_at,r.rowid LIMIT 129
), prefix AS MATERIALIZED (
    SELECT * FROM accepted UNION ALL SELECT * FROM held
), ordered AS MATERIALIZED (
    SELECT p.node_id,n.kind,p.created_at,n.rowid AS node_row,
           n.kind='user' AND NOT EXISTS(
               SELECT 1 FROM thread_path path WHERE path.thread_id=?1 AND path.node_id=n.id
           ) AS eligible
    FROM prefix p JOIN node n ON n.id=p.node_id
    ORDER BY p.created_at,n.rowid LIMIT 128
), witnesses AS MATERIALIZED (
    SELECT row_number() OVER (ORDER BY created_at,node_row) AS position,
           node_id AS id,kind,eligible,
           CASE WHEN eligible THEN (SELECT octet_length(n.payload_json) FROM node n WHERE n.id=ordered.node_id) END AS bytes
    FROM ordered WHERE eligible
)";

pub(super) fn read_tx(db: &Connection, thread: &str) -> Result<RawHeader, StorageError> {
    let (details, updated_at) = db.query_row(
        "SELECT t.title,t.cwd,t.profile_id,t.model_id,t.effort,t.parent_thread_id,t.agents_path,t.updated_at,
                r.id,u.id,u.recovery_reason_json,
                (SELECT count(*) FROM run child WHERE child.owner_run_id=r.id AND child.state IN ('live','stopping')),
                (SELECT count(*) FROM shell_execution s WHERE s.owner_run_id=r.id AND s.state IN ('starting','running','stopping'))
         FROM thread t LEFT JOIN run r ON r.thread_id=t.id AND r.state IN ('live','stopping')
         LEFT JOIN turn u ON u.thread_id=t.id AND u.run_id=r.id AND u.state IN ('preparing','running','waiting','recovery_wait')
         WHERE t.id=?1 AND t.deleted_at IS NULL",
        [thread], |row| Ok((ThreadDetails {
            thread_id:thread.into(), title:row.get(0)?, cwd:row.get(1)?, connection:row.get(2)?,
            model:row.get(3)?, effort:row.get(4)?, parent_id:row.get(5)?, agents_path:row.get(6)?,
            run_id:row.get(8)?,turn_id:row.get(9)?,recovery_reason:row.get(10)?,
            children_count:row.get::<_,i64>(11)? as u64,shells_count:row.get::<_,i64>(12)? as u64,
            usage_totals:None,context_usage:None,context:None,
        },row.get::<_,String>(7)?)),
    ).optional()?.ok_or_else(||ApiError::new(ErrorCode::ThreadNotFound,"Thread not found"))?;
    let title = read_title_tx(db, thread, &details.title)?;
    Ok(RawHeader {
        details,
        updated_at,
        title,
    })
}

/// Same bounded title evidence for the header and at most3 displayed children.
pub(super) fn read_title_tx(
    db: &Connection,
    thread: &str,
    stored: &str,
) -> Result<Option<TitleWitness>, StorageError> {
    if !stored.trim().is_empty() {
        return Ok(None);
    }
    let path = read_candidates(db, thread, PATH_PREFIX, WITNESS_BYTES, false)?;
    let pending = read_candidates(
        db,
        thread,
        PENDING_PREFIX,
        WITNESS_BYTES.saturating_sub(path.copied),
        true,
    )?;
    Ok(Some(TitleWitness { path, pending }))
}

fn read_candidates(
    db: &Connection,
    thread: &str,
    prefix: &str,
    budget: usize,
    pending: bool,
) -> Result<Candidates, StorageError> {
    let overflow = if pending {
        "(SELECT count(*) FROM accepted)>128 OR (SELECT count(*) FROM held)>128 OR (SELECT count(*) FROM prefix)>128"
    } else {
        "(SELECT count(*) FROM prefix)>128"
    };
    // Filter only after the materialized membership prefix. Window work then
    // covers real user candidates, never every non-user row or the full source.
    // A singleton left join retains the overflow witness even with no users.
    // A skipped large source is a missing witness, not permission to choose a
    // later title. Never materialize an uncapped payload inside a CTE.
    let sql = format!("WITH {prefix}, budgeted AS (
        SELECT *,sum(CASE WHEN eligible AND bytes<=?2 THEN bytes ELSE 0 END)
                    OVER (ORDER BY position ROWS UNBOUNDED PRECEDING) AS used
        FROM witnesses
    ) SELECT COALESCE(eligible,0),
        CASE WHEN eligible AND bytes<=?2 AND used<=?3 THEN (SELECT n.payload_json FROM node n WHERE n.id=budgeted.id) END,
        COALESCE(eligible AND bytes IS NOT NULL AND (bytes>?2 OR used>?3),0),
        ({overflow})
      FROM (SELECT 1) LEFT JOIN budgeted ON 1 ORDER BY position");
    let mut result = Candidates::default();
    let mut statement = db.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params![thread, SOURCE_BYTES as i64, budget as i64],
        |row| {
            Ok((
                Candidate {
                    user: row.get(0)?,
                    body: row.get(1)?,
                    omitted: row.get(2)?,
                },
                row.get::<_, bool>(3)?,
            ))
        },
    )?;
    for row in rows {
        let (candidate, truncated) = row?;
        result.copied += candidate.body.as_ref().map_or(0, String::len);
        result.rows.push(candidate);
        result.truncated |= truncated;
    }
    debug_assert!(result.rows.len() <= PREFIX_ROWS && result.copied <= budget);
    Ok(result)
}

impl RawHeader {
    /// Called on the existing finite presentation worker after the transaction.
    pub(super) fn prepare(self) -> ThreadDetails {
        let Self {
            mut details,
            updated_at,
            title,
        } = self;
        if let Some(title) = title {
            details.title = title.prepare();
        }
        details.context = Some(ExecutionContext {
            state: "active".into(),
            parent_thread_id: details.parent_id.clone(),
            updated_at: Some(updated_at),
            thread_id: Some(details.thread_id.clone()),
            ..Default::default()
        });
        details
    }
}
impl TitleWitness {
    pub(super) fn prepare(self) -> String {
        self.prepare_outcome().0
    }
    fn prepare_outcome(self) -> (String, bool) {
        let mut user = false;
        for (index, source) in [self.path, self.pending].into_iter().enumerate() {
            if index == 1 && source.truncated {
                return (LIMITED.into(), true);
            }
            for candidate in source.rows {
                if !candidate.user {
                    continue;
                }
                user = true;
                if candidate.omitted {
                    return (LIMITED.into(), true);
                }
                let title = candidate
                    .body
                    .and_then(|body| serde_json::from_str(&body).ok())
                    .and_then(|body| crate::control::db::title_from_user_payload(&body));
                if let Some(title) = title {
                    return (title, false);
                }
            }
            if source.truncated {
                return (LIMITED.into(), true);
            }
        }
        ((if user { "名称未設定" } else { "未送信" }).into(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DataOwner;
    use serde_json::json;

    async fn fixture() -> (tempfile::TempDir, DataOwner) {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        owner.db.call(|db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/synthetic','/synthetic/AGENTS.md','connection','model','saved-update')",[])?;
            Ok(())
        }).await.unwrap();
        (root, owner)
    }
    fn input(
        db: &Connection,
        id: &str,
        text: &str,
        admitted: bool,
        stamp: &str,
        state: &str,
    ) -> Result<(), StorageError> {
        let payload =
            json!({"role":"user","content":[{"type":"input_text","text":text}]}).to_string();
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user',?2,?3)",
            rusqlite::params![id, stamp, payload],
        )?;
        if admitted {
            db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('t',(SELECT count(*) FROM thread_path WHERE thread_id='t'),?1)",[id])?;
        } else {
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES(?1,?1,'null','t',?1,?2,?3)",rusqlite::params![id,state,stamp])?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn owned_header_matches_cli_facts_and_prepares_after_database_shutdown() {
        let (_root, owner) = fixture().await;
        let (raw,expected)=owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('p','/','','p','m','now'),('child','/','','p','m','now'),('unowned','/','','p','m','now');
                UPDATE thread SET parent_thread_id='p',effort='high' WHERE id='t';
                INSERT INTO run(id,thread_id,state,created_at,owner_run_id)VALUES('r','t','live','now',NULL),('owned','child','live','now','r'),('other','unowned','live','now',NULL);
                INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES('u','t','r','recovery_wait','now','{\"reason\":\"saved\"}');
                INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('e','r','u','e','running','/','{}','UTF-8','UTF-8','open','now');")?;
            input(&tx,"path","\u{1b}[31mfirst\u{1b}[0m\r\n selected request",true,"later","applied")?;
            input(&tx,"pending","do not choose this earlier pending message",false,"earlier","accepted")?;
            let expected=crate::control::db::thread_details(&tx,"t").unwrap();
            let trace=crate::frontend::sql_trace::Statements::new(&tx);
            let raw=read_tx(&tx,"t")?;
            assert_eq!(trace.count(),3,"one header and two bounded witness statements");
            drop(trace);
            tx.commit()?;
            db.execute("UPDATE thread SET title='subsequent mutation',model_id='changed',cwd='/changed' WHERE id='t'",[])?;
            Ok((raw,expected))
        }).await.unwrap();
        owner.shutdown().await.unwrap();
        let actual = raw.prepare();
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(actual.title, "first selected request");
        assert_eq!((actual.children_count, actual.shells_count), (1, 1));
        assert_eq!(
            actual.context.unwrap().updated_at.as_deref(),
            Some("saved-update")
        );
    }

    #[tokio::test]
    async fn explicit_titles_pending_order_and_empty_evidence_keep_their_distinct_meanings() {
        let (_root, owner) = fixture().await;
        owner.db.call(|db| {
            assert_eq!(read_tx(db,"t")?.prepare().title,"未送信");
            input(db,"excluded","unselected applied history",false,"000","applied")?;
            input(db,"held-first","held first",false,"001","held")?;
            input(db,"accepted-later","accepted later",false,"002","accepted")?;
            assert_eq!(read_tx(db,"t")?.prepare().title,"held first");
            // Equal saved time uses node-rowid, not receipt state ordering.
            db.execute("UPDATE receipt SET created_at='001' WHERE id='accepted-later'",[])?;
            assert_eq!(read_tx(db,"t")?.prepare().title,"held first");
            // Reverse receipt insertion while keeping the original node order.
            db.execute("DELETE FROM receipt WHERE id IN ('held-first','accepted-later')",[])?;
            for id in ["accepted-later","held-first"] {
                db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES(?1,?1,'null','t',?1,'accepted','001')",[id])?;
            }
            assert_eq!(read_tx(db,"t")?.prepare().title,"held first","node order wins the timestamp tie within a complete prefix");
            db.execute("UPDATE thread SET title='  explicit\u{1b}[31m title  ' WHERE id='t'",[])?;
            let trace=crate::frontend::sql_trace::Statements::new(db);
            let raw=read_tx(db,"t")?;
            assert_eq!(trace.count(),1,"explicit title requires only the header query");
            drop(trace);
            assert!(raw.title.is_none(),"explicit title requires no body witness");
            assert_eq!(raw.prepare().title,"  explicit\u{1b}[31m title  ");
            db.execute("UPDATE thread SET title='' WHERE id='t'",[])?;
            db.execute("DELETE FROM receipt",[])?;
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('missing','user','now',NULL)",[])?;
            db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('t',0,'missing')",[])?;
            assert_eq!(read_tx(db,"t")?.prepare().title,"名称未設定");
            input(db,"pending-after-null","pending after absent payload",false,"003","held")?;
            assert_eq!(read_tx(db,"t")?.prepare().title,"pending after absent payload");
            db.execute("UPDATE thread SET deleted_at='now' WHERE id='t'",[])?;
            assert_eq!(read_tx(db,"t").err().unwrap().into_api_error().code,ErrorCode::ThreadNotFound);
            Ok(())
        }).await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn title_membership_and_byte_exhaustion_never_choose_a_later_request_or_claim_unsent() {
        let (_root, owner) = fixture().await;
        owner.db.call(|db| {
            let tx=db.transaction()?;
            for ordinal in 0..128 {
                let id=format!("nonuser-{ordinal}");
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'assistant_response','now','{}')",[&id])?;
                tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('t',?1,?2)",rusqlite::params![ordinal,id])?;
            }
            input(&tx,"outside","not a proven first request",true,"now","applied")?;
            let raw=read_tx(&tx,"t")?;
            let witness=raw.title.as_ref().unwrap();
            assert_eq!(witness.path.rows.len(),1,"one empty status witness instead of128 non-user rows");
            assert!(witness.path.truncated);
            assert_eq!(witness.path.copied,0);
            assert_eq!(raw.prepare().title,LIMITED);
            tx.execute("DELETE FROM thread_path WHERE thread_id='t'",[])?;
            input(&tx,"too-big",&"x".repeat(SOURCE_BYTES),true,"now","applied")?;
            input(&tx,"after-big","later text is not first",true,"now","applied")?;
            let raw=read_tx(&tx,"t")?;
            assert!(raw.title.as_ref().unwrap().path.rows[0].body.is_none());
            assert_eq!(raw.prepare().title,LIMITED);
            tx.execute("DELETE FROM thread_path WHERE thread_id='t'",[])?;
            for n in 0..5 {
                input(&tx,&format!("blank-{n}"),&" ".repeat(120*1024),true,"now","applied")?;
            }
            input(&tx,"after-budget","later after shared budget",true,"now","applied")?;
            let raw=read_tx(&tx,"t")?;
            let witness=raw.title.as_ref().unwrap();
            assert!(witness.path.copied+witness.pending.copied<=WITNESS_BYTES);
            assert!(witness.path.rows[4].omitted);
            assert_eq!(raw.prepare().title,LIMITED);
            tx.execute("DELETE FROM thread_path WHERE thread_id='t'", [])?;
            for n in 0..4 {
                input(&tx,&format!("shared-budget-{n}"),&" ".repeat(120*1024),true,"now","applied")?;
            }
            input(&tx,"pending-over-shared-budget",&" ".repeat(100*1024),false,"001","accepted")?;
            input(&tx,"pending-after-budget","cannot replace the omitted earlier user",false,"002","accepted")?;
            let raw=read_tx(&tx,"t")?;
            let witness=raw.title.as_ref().unwrap();
            assert!(witness.path.copied>0 && witness.path.copied+witness.pending.copied<=WITNESS_BYTES);
            assert!(witness.pending.rows[0].omitted,"path and pending share one byte allowance");
            assert_eq!(raw.prepare().title,LIMITED);
            tx.commit()?;Ok(())
        }).await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pending_overflow_is_explicit_but_cannot_hide_an_earlier_selected_title() {
        let (_root, owner) = fixture().await;
        owner.db.call(|db| {
            let tx=db.transaction()?;
            for n in 0..129 {input(&tx,&format!("queued-{n}"),"pending",false,"same","held")?;}
            assert_eq!(read_tx(&tx,"t")?.prepare().title,LIMITED);
            input(&tx,"selected","proven earlier selected request",true,"now","applied")?;
            assert_eq!(read_tx(&tx,"t")?.prepare().title,"proven earlier selected request");
            tx.execute("DELETE FROM receipt",[])?;
            tx.execute("DELETE FROM thread_path",[])?;
            for n in 0..129 {
                let id=format!("pending-nonuser-{n}");
                tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'assistant_response','now','{}')",[&id])?;
                tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES(?1,?1,'null','t',?1,'held','same')",[id])?;
            }
            input(&tx,"after-nonuser-prefix","must not scan through the whole pending queue",false,"zzzz","held")?;
            let raw=read_tx(&tx,"t")?;
            let pending=&raw.title.as_ref().unwrap().pending;
            assert!(pending.truncated && pending.rows.len()==1 && pending.copied==0);
            assert_eq!(raw.prepare().title,LIMITED);
            tx.commit()?;Ok(())
        }).await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn title_policy_keeps_text_part_order_unicode_bounds_and_unreadable_fallback() {
        use unicode_segmentation::UnicodeSegmentation;
        let body=json!({"role":"user","content":[{"type":"image","text":"not title"},{"type":"input_text","text":"e\u{301}".repeat(100)},{"type":"text","text":"tail"}]}).to_string();
        let witness = TitleWitness {
            path: Candidates {
                rows: vec![Candidate {
                    user: true,
                    body: Some(body),
                    omitted: false,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let title = witness.prepare();
        assert!(
            title.ends_with('…')
                && title.graphemes(true).count() <= 80
                && title.chars().count() <= 160
        );
        let witness = TitleWitness {
            path: Candidates {
                rows: vec![Candidate {
                    user: true,
                    body: Some("malformed".into()),
                    omitted: false,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(witness.prepare(), "名称未設定");
        let body = json!({"role":"user","content":[{"type":"image","text":"ignored"}]}).to_string();
        let witness = TitleWitness {
            path: Candidates {
                rows: vec![Candidate {
                    user: true,
                    body: Some(body),
                    omitted: false,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(witness.prepare(), "名称未設定");
    }
}

/// Owned title evidence for one bounded browser ID set.
pub(super) struct RawTitles {
    entries: std::collections::BTreeMap<String, (String, Option<TitleWitness>)>,
}
impl RawTitles {
    pub(super) fn prepare(self) -> (std::collections::BTreeMap<String, String>, Vec<String>) {
        let mut titles = std::collections::BTreeMap::new();
        let mut limited = Vec::new();
        for (id, (stored, witness)) in self.entries {
            let (title, is_limited) = witness
                .map(TitleWitness::prepare_outcome)
                .unwrap_or((stored, false));
            if is_limited {
                limited.push(id.clone());
            }
            titles.insert(id, title);
        }
        (titles, limited)
    }
}
// Recursive successors seek the existing membership indexes. Shared LIMITs
// apply before user/body filtering: neither lane can inspect an entire history.
const TITLE_BATCH_SQL: &str = r"
WITH RECURSIVE keys AS MATERIALIZED (
 SELECT json_extract(value,'$[0]') AS thread,CAST(key AS INTEGER) AS input_pos FROM json_each(?1)
), states(state) AS (VALUES('accepted'),('held')),
path_walk(thread,input_pos,depth,ordinal) AS (
 SELECT thread,input_pos,1,(SELECT min(ordinal) FROM thread_path p WHERE p.thread_id=keys.thread) FROM keys
 UNION ALL
 SELECT thread,input_pos,depth+1,(SELECT min(ordinal) FROM thread_path p WHERE p.thread_id=path_walk.thread AND p.ordinal>path_walk.ordinal)
 FROM path_walk WHERE ordinal IS NOT NULL AND depth<129 ORDER BY 3,2 LIMIT 1025
), pending_walk(thread,input_pos,state,depth,receipt_row) AS (
 SELECT thread,input_pos,state,1,(SELECT rowid FROM receipt r INDEXED BY receipt_target WHERE r.target_thread_id=keys.thread AND r.state=states.state ORDER BY created_at,rowid LIMIT 1)
 FROM keys CROSS JOIN states
 UNION ALL
 SELECT w.thread,w.input_pos,w.state,w.depth+1,
   (SELECT r.rowid FROM receipt r INDEXED BY receipt_target WHERE r.target_thread_id=w.thread AND r.state=w.state
      AND (r.created_at,r.rowid)>(current.created_at,current.rowid) ORDER BY r.created_at,r.rowid LIMIT 1)
 FROM pending_walk w JOIN receipt current ON current.rowid=w.receipt_row
 WHERE w.depth<129 ORDER BY 4,2,3 LIMIT 1025
), path_numbered AS MATERIALIZED (
 SELECT *,row_number() OVER(ORDER BY depth,input_pos) AS inspected FROM path_walk
), pending_numbered AS MATERIALIZED (
 SELECT *,row_number() OVER(ORDER BY depth,input_pos,state) AS inspected FROM pending_walk
), path_members AS MATERIALIZED (
 SELECT w.thread,w.input_pos,w.depth,p.node_id FROM path_numbered w CROSS JOIN thread_path p
 ON p.thread_id=w.thread AND p.ordinal=w.ordinal WHERE w.depth<=128 AND w.inspected<=1024
), pending_members AS MATERIALIZED (
 SELECT w.thread,w.input_pos,w.depth,w.state,r.created_at,r.message_node_id AS node_id,n.rowid AS node_row
 FROM pending_numbered w CROSS JOIN receipt r ON r.rowid=w.receipt_row CROSS JOIN node n ON n.id=r.message_node_id
 WHERE w.inspected<=1024
), pending_ordered AS MATERIALIZED (
 SELECT *,row_number() OVER(PARTITION BY thread ORDER BY created_at,node_row) AS position FROM pending_members
), status AS MATERIALIZED (
 SELECT k.thread,k.input_pos,
   EXISTS(SELECT 1 FROM path_walk w WHERE w.thread=k.thread AND w.depth=129 AND w.ordinal IS NOT NULL)
     OR ((SELECT count(*) FROM path_walk)>=1025 AND NOT EXISTS(SELECT 1 FROM path_walk w WHERE w.thread=k.thread AND w.ordinal IS NULL)) AS path_limited,
   EXISTS(SELECT 1 FROM pending_walk w WHERE w.thread=k.thread AND w.depth=129 AND w.receipt_row IS NOT NULL)
     OR (SELECT count(*) FROM pending_members p WHERE p.thread=k.thread)>128
     OR ((SELECT count(*) FROM pending_walk)>=1025 AND EXISTS(SELECT 1 FROM states s WHERE NOT EXISTS(SELECT 1 FROM pending_walk w WHERE w.thread=k.thread AND w.state=s.state AND w.receipt_row IS NULL))) AS pending_limited
 FROM keys k
), witnesses AS MATERIALIZED (
 SELECT p.thread,p.input_pos,0 AS lane,p.depth AS position,n.id,octet_length(n.payload_json) AS bytes
 FROM path_members p CROSS JOIN node n ON n.id=p.node_id WHERE n.kind='user'
 UNION ALL
 SELECT p.thread,p.input_pos,1 AS lane,p.position,n.id,octet_length(n.payload_json) AS bytes
 FROM pending_ordered p CROSS JOIN node n ON n.id=p.node_id
 WHERE p.position<=128 AND n.kind='user' AND NOT EXISTS(SELECT 1 FROM thread_path path WHERE path.thread_id=p.thread AND path.node_id=p.node_id)
), budgeted AS MATERIALIZED (
 SELECT *,sum(CASE WHEN bytes<=?2 THEN bytes ELSE 0 END)
   OVER(ORDER BY input_pos,lane,position ROWS UNBOUNDED PRECEDING) AS used FROM witnesses
)
SELECT thread,input_pos,-1 AS lane,0 AS position,NULL AS body,0 AS omitted,path_limited,pending_limited FROM status
UNION ALL
SELECT thread,input_pos,lane,position,
 CASE WHEN bytes<=?2 AND used<=?3 THEN (SELECT payload_json FROM node n WHERE n.id=budgeted.id) END,
 bytes IS NOT NULL AND (bytes>?2 OR used>?3),0,0 FROM budgeted
ORDER BY input_pos,lane,position";

pub(super) fn read_titles_tx(
    db: &Connection,
    requests: &[(String, String)],
) -> Result<RawTitles, StorageError> {
    if requests.len() > 128 {
        return Err(
            ApiError::new(ErrorCode::InputLimit, "Browser title scope exceeds128 IDs").into(),
        );
    }
    let mut entries = std::collections::BTreeMap::new();
    let mut untitled = Vec::new();
    for (id, stored) in requests {
        if entries.contains_key(id) {
            continue;
        }
        if stored.trim().is_empty() {
            entries.insert(id.clone(), (stored.clone(), Some(TitleWitness::default())));
            untitled.push((id.clone(), String::new()));
        } else {
            entries.insert(id.clone(), (stored.clone(), None));
        }
    }
    if untitled.is_empty() {
        return Ok(RawTitles { entries });
    }
    let encoded = serde_json::to_string(&untitled).expect("title IDs serialize");
    let mut statement = db.prepare(TITLE_BATCH_SQL)?;
    let mut rows = statement.query(rusqlite::params![
        encoded,
        SOURCE_BYTES as i64,
        WITNESS_BYTES as i64
    ])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let witness = entries
            .get_mut(&id)
            .and_then(|(_, w)| w.as_mut())
            .expect("requested title witness");
        let lane: i64 = row.get(2)?;
        if lane < 0 {
            witness.path.truncated = row.get(6)?;
            witness.pending.truncated = row.get(7)?;
        } else {
            let candidates = if lane == 0 {
                &mut witness.path
            } else {
                &mut witness.pending
            };
            let body: Option<String> = row.get(4)?;
            candidates.copied += body.as_ref().map_or(0, String::len);
            candidates.rows.push(Candidate {
                user: true,
                body,
                omitted: row.get(5)?,
            });
        }
    }
    Ok(RawTitles { entries })
}

#[cfg(test)]
mod browser_title_batch_tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use rusqlite::StatementStatus;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db
    }
    fn thread(db: &Connection, id: &str) {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','','p','m','now')",[id]).unwrap();
    }
    fn source(db: &Connection, thread: &str, n: i64, user: bool) {
        let id = format!("{thread}-{n}");
        let body = if user {
            r#"{"role":"user","content":"question"}"#
        } else {
            "{}"
        };
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,?2,'now',?3)",
            rusqlite::params![id, if user { "user" } else { "tool_result" }, body],
        )
        .unwrap();
        db.execute(
            "INSERT INTO thread_path VALUES(?1,?2,?3)",
            rusqlite::params![thread, n, id],
        )
        .unwrap();
    }
    #[test]
    fn explicit_titles_and_invalid_scope_execute_no_source_sql() {
        let db = fixture();
        let trace = Statements::new(&db);
        let (titles, limited) = read_titles_tx(&db, &[("t".into(), "Known".into())])
            .unwrap()
            .prepare();
        assert_eq!(titles["t"], "Known");
        assert!(limited.is_empty());
        assert!(read_titles_tx(&db, &vec![("x".into(), String::new()); 129]).is_err());
        assert_eq!(trace.count(), 0);
    }
    #[test]
    fn global_membership_ceiling_cannot_choose_a_later_unobserved_title() {
        let mut db = fixture();
        let tx = db.transaction().unwrap();
        let mut requests = Vec::new();
        for i in 0..128 {
            let id = format!("t{i}");
            thread(&tx, &id);
            for n in 0..20 {
                source(&tx, &id, n, n == 10);
            }
            requests.push((id, String::new()));
        }
        tx.commit().unwrap();
        let encoded = serde_json::to_string(&requests).unwrap();
        let prefix = TITLE_BATCH_SQL
            .split("SELECT thread,input_pos,-1 AS lane")
            .next()
            .unwrap();
        let inspected:(i64,i64)=db.query_row(&format!("{prefix}SELECT (SELECT count(*) FROM path_walk),(SELECT count(*) FROM pending_walk) WHERE ?3>=0"),rusqlite::params![encoded,128*1024,512*1024],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(inspected.0, 1025);
        assert!(inspected.1 <= 1025);
        let (_, limited) = read_titles_tx(&db, &requests).unwrap().prepare();
        assert_eq!(limited.len(), 128);
    }
    #[test]
    fn indexed_title_work_does_not_scale_with_unrelated_membership() {
        let mut db = fixture();
        thread(&db, "selected");
        source(&db, "selected", 0, true);
        thread(&db, "unrelated");
        let encoded = serde_json::to_string(&[("selected", "")]).unwrap();
        let measure = |db: &Connection| {
            let mut statement = db.prepare(TITLE_BATCH_SQL).unwrap();
            let rows = statement
                .query_map(rusqlite::params![encoded, 128 * 1024, 512 * 1024], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows.len(), 2);
            statement.get_status(StatementStatus::VmStep)
        };
        let before = measure(&db);
        let tx = db.transaction().unwrap();
        for n in 0..10_000 {
            source(&tx, "unrelated", n, true);
        }
        tx.commit().unwrap();
        let after = measure(&db);
        assert!(after <= before + 100, "{before}->{after}");
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {TITLE_BATCH_SQL}"))
            .unwrap()
            .query_map(rusqlite::params![encoded, 128 * 1024, 512 * 1024], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|p| p.contains("thread_path") && p.contains("SEARCH")),
            "{plan:?}"
        );
        assert!(
            plan.iter().any(|p| p.contains("receipt_target")),
            "{plan:?}"
        );
    }
}
