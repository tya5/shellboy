//! Bounded source metadata read in the selected owner's observation transaction.
use crate::{
    protocol::{ApiError, ErrorCode, HistoryWindowSource, MessageSender},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, HashMap};

pub(crate) const MAX_NODES: usize = 128;
pub(crate) const STAMP_CANDIDATES: usize = 128;
// Each recursive chain seeks one exact indexed target/state/stamp range. Its
// 129th witness proves overflow before requested-node matching or sender copies.
pub(crate) const RECEIPTS_AT_STAMP: &str = "WITH RECURSIVE
keys AS MATERIALIZED (
    SELECT CAST(j.key AS INTEGER) AS bucket,json_extract(j.value,'$.target') AS target,
        json_extract(j.value,'$.stamp') AS stamp FROM json_each(?1) j),
states(state) AS (VALUES('accepted'),('held'),('applied'),('cancelled')),
candidates(bucket,state,receipt_id,depth) AS (
    SELECT b.bucket,s.state,(SELECT r.rowid FROM receipt r INDEXED BY receipt_target
        WHERE r.target_thread_id=b.target AND r.state=s.state AND r.created_at=b.stamp
        ORDER BY r.rowid LIMIT 1),1 FROM keys b CROSS JOIN states s
    UNION ALL
    SELECT c.bucket,c.state,(SELECT r.rowid FROM receipt r INDEXED BY receipt_target
        WHERE r.target_thread_id=b.target AND r.state=c.state AND r.created_at=b.stamp
            AND r.rowid>c.receipt_id ORDER BY r.rowid LIMIT 1),c.depth+1
    FROM candidates c JOIN keys b ON b.bucket=c.bucket
    WHERE c.receipt_id IS NOT NULL AND c.depth<129)
SELECT c.bucket,c.state,c.receipt_id,
    CASE WHEN r.message_node_id IN (SELECT value FROM json_each(?2))
        THEN r.message_node_id END
FROM candidates c JOIN receipt r ON r.rowid=c.receipt_id";
pub(crate) const COMMON_SOURCES: &str =
    "WITH requested AS (SELECT DISTINCT value AS id FROM json_each(?1))
    SELECT q.id,n.created_at,COALESCE(n.kind IN ('user','user_message','message'),0),
        COALESCE(u.thread_id,?2),EXISTS(SELECT 1 FROM receipt r INDEXED BY receipt_message_node
            WHERE r.message_node_id=q.id AND r.target_thread_id=COALESCE(u.thread_id,?2)
            AND json_extract(r.result_json,'$.delivery_context.reason')='run_completion')
    FROM requested q LEFT JOIN node n ON n.id=q.id
    LEFT JOIN turn u ON u.id=n.turn_id";

pub(crate) struct RawSource {
    pub(crate) node_id: String,
    pub(crate) created_at: Option<String>,
    pub(crate) revisit: bool,
    // Preserve raw legacy null separately from missing/ambiguous evidence.
    // No sender JSON is interpreted on the DB worker.
    pub(crate) sender: Option<String>,
    pub(crate) reply_state: Option<String>,
    pub(crate) reply_excerpt: Option<String>,
}
pub(crate) struct SourceFacts {
    pub(crate) viewed: Option<String>,
    pub(crate) parent: Option<String>,
    pub(crate) rows: Vec<RawSource>,
}

pub(crate) fn read_tx(
    db: &Connection,
    viewed: Option<&str>,
    ids: &[String],
) -> Result<SourceFacts, StorageError> {
    if ids.len() > MAX_NODES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Display source metadata exceeds its node bound",
        )
        .into());
    }
    let parent: Option<String> = match viewed {
        Some(thread) => db
            .query_row(
                "SELECT parent_thread_id FROM thread WHERE id=?1",
                [thread],
                |row| row.get(0),
            )
            .optional()?
            .flatten(),
        None => None,
    };
    let encoded = serde_json::to_string(ids).expect("String IDs serialize");
    let mut query = db.prepare_cached(COMMON_SOURCES)?;
    let common = query
        .query_map(params![encoded, viewed], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rows = Vec::with_capacity(common.len());
    let mut buckets = BTreeMap::<(String, String), Vec<usize>>::new();
    for (node_id, stamp, user, target, revisit) in common {
        let index = rows.len();
        if user
            && !revisit
            && let (Some(target), Some(stamp)) = (target, stamp.as_ref())
        {
            buckets
                .entry((target, stamp.clone()))
                .or_default()
                .push(index);
        }
        rows.push(RawSource {
            node_id,
            created_at: stamp.filter(|stamp| stamp.len() <= 128),
            revisit,
            sender: None,
            reply_state: None,
            reply_excerpt: None,
        });
    }
    // All distinct timestamp/state buckets share one bounded indexed query.
    // Only bucket IDs, rowids and matching requested IDs cross the DB boundary.
    let buckets = buckets.into_iter().collect::<Vec<_>>();
    let mut witnesses = Vec::new();
    if !buckets.is_empty() {
        let keys = buckets
            .iter()
            .map(|((target, stamp), _)| serde_json::json!({"target":target,"stamp":stamp}))
            .collect::<Vec<_>>();
        let keys = serde_json::to_string(&keys).expect("Receipt keys serialize");
        let mut counts = vec![[0_usize; 4]; buckets.len()];
        let mut matched = vec![HashMap::<String, Vec<i64>>::new(); buckets.len()];
        let mut node_buckets = HashMap::new();
        for (bucket, (_, indices)) in buckets.iter().enumerate() {
            for &index in indices {
                node_buckets.insert(rows[index].node_id.as_str(), bucket);
            }
        }
        let mut receipts = db.prepare_cached(RECEIPTS_AT_STAMP)?;
        let candidates = receipts.query_map(params![keys, encoded], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for candidate in candidates {
            let (bucket, state, receipt, node) = candidate?;
            let state_index = match state.as_str() {
                "accepted" => 0,
                "held" => 1,
                "applied" => 2,
                "cancelled" => 3,
                _ => unreachable!("receipt states come from fixed VALUES"),
            };
            counts[bucket][state_index] += 1;
            if let Some(node) = node
                && node_buckets.get(node.as_str()) == Some(&bucket)
            {
                let matches = matched[bucket].entry(node).or_default();
                if matches.len() < 2 {
                    matches.push(receipt);
                }
            }
        }
        for (bucket, (_, indices)) in buckets.iter().enumerate() {
            if counts[bucket].iter().any(|&count| count > STAMP_CANDIDATES) {
                continue;
            }
            for &index in indices {
                if let Some(matches) = matched[bucket].get(&rows[index].node_id)
                    && let [receipt] = matches.as_slice()
                {
                    witnesses.push((index, *receipt));
                }
            }
        }
    }
    if !witnesses.is_empty() {
        let receipt_ids = witnesses
            .iter()
            .map(|(_, receipt)| *receipt)
            .collect::<Vec<_>>();
        let encoded = serde_json::to_string(&receipt_ids).expect("Receipt rowids serialize");
        let mut senders = db.prepare_cached("SELECT r.rowid,substr(r.sender_json,1,8193),json_extract(r.result_json,'$.reply.state'),CASE WHEN json_extract(r.result_json,'$.reply_to') IS NOT NULL THEN COALESCE((SELECT substr(json_extract(n.payload_json,'$.content[0].text'),1,513) FROM node n WHERE n.id=json_extract(r.result_json,'$.reply_to')),'') END FROM json_each(?1) q JOIN receipt r ON r.rowid=q.value")?;
        let mut senders = senders
            .query_map([encoded], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    (
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ),
                ))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        for (index, receipt) in witnesses {
            if let Some((sender, state, excerpt)) = senders.remove(&receipt) {
                rows[index].sender = Some(sender);
                rows[index].reply_state = state;
                rows[index].reply_excerpt = excerpt;
            }
        }
    }
    Ok(SourceFacts {
        viewed: viewed.map(str::to_owned),
        parent,
        rows,
    })
}

pub(crate) fn relationship(
    raw: &RawSource,
    viewed: Option<&str>,
    parent: Option<&str>,
) -> crate::presentation::InputSource {
    use crate::presentation::InputSource;
    if raw.revisit {
        InputSource::Revisit
    } else {
        match raw
            .sender
            .as_deref()
            .and_then(|sender| serde_json::from_str::<serde_json::Value>(sender).ok())
        {
            Some(serde_json::Value::Null) => InputSource::Human,
            Some(sender)
                if sender.get("kind").and_then(serde_json::Value::as_str) == Some("human") =>
            {
                InputSource::Human
            }
            Some(sender) => match sender.get("thread_id").and_then(serde_json::Value::as_str) {
                Some(sender) if Some(sender) == viewed => InputSource::SelfModel,
                Some(sender) if Some(sender) == parent => InputSource::Parent,
                Some(_) => InputSource::OtherModel,
                None => InputSource::Unknown,
            },
            None => InputSource::Unknown,
        }
    }
}

pub(crate) fn pinned(
    db: &Connection,
    viewed: &str,
    ids: &[String],
) -> Result<Vec<HistoryWindowSource>, StorageError> {
    // Only return metadata for nodes still on the requested, live selected path.
    let encoded = serde_json::to_string(ids).expect("IDs serialize");
    let selected = db.prepare_cached("SELECT DISTINCT p.node_id FROM json_each(?1) q JOIN thread_path p ON p.thread_id=?2 AND p.node_id=q.value")?
        .query_map(params![encoded, viewed], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut sources = Vec::with_capacity(selected.len());
    for chunk in selected.chunks(MAX_NODES) {
        let facts = read_tx(db, Some(viewed), chunk)?;
        sources.extend(facts.rows.into_iter().map(|row| {
            let sender = MessageSender::from_saved(row.sender.as_deref());
            let relationship = relationship(&row, facts.viewed.as_deref(), facts.parent.as_deref());
            HistoryWindowSource {
                node_id: row.node_id,
                created_at: row.created_at,
                relationship,
                sender,
                reply_state: row.reply_state.filter(|state| {
                    matches!(
                        state.as_str(),
                        "pending" | "replied" | "ignored" | "cancelled" | "recipient_deleted"
                    )
                }),
                reply_excerpt: row
                    .reply_excerpt
                    .map(|text| text.chars().take(512).collect()),
            }
        }));
    }
    Ok(sources)
}
