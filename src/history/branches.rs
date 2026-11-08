//! `thread branches` (#237): the branch tree of a Thread, read from existing facts.
//!
//! Branches are Threads. A branch is a child Thread made by fork, `/edit` or
//! `thread rewind` that shares a path prefix with its parent. Nothing is stored
//! for the tree: the family is walked through `thread.parent_thread_id`, and a
//! child's divergence point is the last node its selected path still shares with
//! its parent's, found by bisection over `(thread_id, ordinal)` point lookups.
//!
//! Bounds (all documented in the CLI reference):
//! - ancestors followed upward: [`ANCESTOR_LIMIT`];
//! - Threads in one family: [`FAMILY_LIMIT`] (the ancestor chain always stays);
//! - items per page: 1..=200 (default 50);
//! - last-request search: [`SCAN_ROWS`] path rows back from the tip;
//! - bisection: about `2 * log2(path length)` point lookups per branch.
//!
//! Bisection assumes the paths agree up to the divergence and disagree after it.
//! A wrong point is possible only when a stretch of disagreement is followed by
//! ordinals that agree again, which needs a replacement that keeps later ordinals
//! in place (for example both paths separately compacted the same range into
//! stretches of equal length). A one-sided compaction shifts later ordinals, so
//! they stay different.
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use super::sql_error;
use crate::protocol::{ApiError, ErrorCode, MAX_LIST_LIMIT, Page, PageCursor, ThreadBranchesArgs};

/// Ancestors followed upward from the requested Thread.
pub const ANCESTOR_LIMIT: usize = 32;
/// Threads listed in one family (ancestor chain first, then breadth first).
pub const FAMILY_LIMIT: usize = 256;
/// Characters kept in a request excerpt.
pub const EXCERPT_CHARS: usize = 160;
/// Path rows searched back from a tip for its last request.
pub const SCAN_ROWS: i64 = 256;
/// Request payloads larger than this are not parsed for an excerpt.
const PAYLOAD_BYTES: i64 = 65_536;
const SCOPE: &str = "thread.branches";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Excerpt {
    pub node_id: String,
    /// None when the payload is collected, malformed or over the parse bound.
    pub text: Option<String>,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Divergence {
    pub node_id: String,
    /// Position of the last shared node in both selected paths.
    pub ordinal: i64,
    pub request: Option<Excerpt>,
    /// The parent has nodes after the divergence point.
    pub parent_continues: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tip {
    pub node_id: String,
    pub at: String,
    pub last_request: Option<Excerpt>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Size {
    pub path_nodes: i64,
    /// Nodes after the divergence point; absent for a family root.
    pub nodes_after_divergence: Option<i64>,
    /// Payload bytes are not summed: that is unbounded for long Threads.
    pub bytes: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchItem {
    pub thread_id: String,
    pub title: Option<String>,
    pub parent_thread_id: Option<String>,
    /// Persisted creation path; unknown for roots and pre-migration branches.
    pub branch_kind: &'static str,
    pub depth: u32,
    /// `current` (the requested Thread), `ancestor`, or `branch`.
    pub state: &'static str,
    pub running: bool,
    /// The Thread holds a pruned replacement: those originals are gone.
    pub pruned_originals: bool,
    pub divergence: Option<Divergence>,
    /// Why a child has no divergence point.
    pub divergence_unknown: Option<&'static str>,
    pub tip: Option<Tip>,
    pub size: Size,
}

struct Member {
    id: String,
    title: String,
    parent: Option<String>,
    branch_kind: Option<String>,
    depth: u32,
}
struct Family {
    root_parent_unavailable: bool,
    chain: Vec<String>,
    members: Vec<Member>,
    ancestors_truncated: bool,
    family_truncated: bool,
}

/// Which part of the family to read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every Thread under the topmost live ancestor (CLI and panel).
    Full,
    /// Ancestor chain and their direct children (inline markers).
    Local,
}

fn not_found() -> ApiError {
    ApiError::new(ErrorCode::ThreadNotFound, "Thread not found")
}
type LiveThread = (String, Option<String>, Option<String>);

fn live_thread(db: &Connection, id: &str) -> Result<Option<LiveThread>, ApiError> {
    db.prepare_cached(
        "SELECT title,parent_thread_id,branch_kind FROM thread WHERE id=?1 AND deleted_at IS NULL",
    )
    .map_err(sql_error)?
    .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .optional()
    .map_err(sql_error)
}
fn children(
    db: &Connection,
    parent: &str,
    limit: usize,
) -> Result<Vec<(String, String, Option<String>)>, ApiError> {
    db.prepare_cached("SELECT id,title,branch_kind FROM thread WHERE parent_thread_id=?1 AND deleted_at IS NULL ORDER BY id LIMIT ?2")
        .map_err(sql_error)?
        .query_map(params![parent, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)
}

fn family(db: &Connection, thread: &str, scope: Scope) -> Result<Family, ApiError> {
    let (title, mut parent, branch_kind) = live_thread(db, thread)?.ok_or_else(not_found)?;
    // Upward: stop at a missing or deleted parent (the Thread then roots its own family).
    let mut upward = vec![(thread.to_owned(), title, branch_kind)];
    let mut seen: HashSet<String> = HashSet::from([thread.to_owned()]);
    let mut ancestors_truncated = false;
    let mut root_parent_unavailable = false;
    while let Some(parent_id) = parent.take() {
        if upward.len() > ANCESTOR_LIMIT {
            ancestors_truncated = true;
            break;
        }
        match live_thread(db, &parent_id)? {
            Some((title, next, kind)) if seen.insert(parent_id.clone()) => {
                upward.push((parent_id, title, kind));
                parent = next;
            }
            _ => root_parent_unavailable = true,
        }
    }
    upward.reverse();
    let chain: Vec<String> = upward.iter().map(|(id, _, _)| id.clone()).collect();
    let mut members: Vec<Member> = upward
        .into_iter()
        .enumerate()
        .map(|(depth, (id, title, branch_kind))| Member {
            parent: depth.checked_sub(1).map(|i| chain[i].clone()),
            id,
            title,
            branch_kind,
            depth: depth as u32,
        })
        .collect();
    let mut included: HashSet<String> = chain.iter().cloned().collect();
    let mut family_truncated = false;
    let mut cursor = 0;
    // Breadth first from the chain; a child of a chain member keeps its parent's depth + 1.
    while cursor < members.len() {
        let (id, depth) = (members[cursor].id.clone(), members[cursor].depth);
        cursor += 1;
        if scope == Scope::Local && cursor > chain.len() {
            break;
        }
        let room = FAMILY_LIMIT.saturating_sub(members.len());
        let found = children(db, &id, room + 1)?;
        for (index, (child, title, branch_kind)) in found.into_iter().enumerate() {
            if included.contains(&child) {
                continue;
            }
            if index >= room {
                family_truncated = true;
                break;
            }
            included.insert(child.clone());
            members.push(Member {
                id: child,
                title,
                parent: Some(id.clone()),
                branch_kind,
                depth: depth + 1,
            });
        }
    }
    Ok(Family {
        root_parent_unavailable,
        chain,
        members,
        ancestors_truncated,
        family_truncated,
    })
}

fn path_len(db: &Connection, thread: &str) -> Result<i64, ApiError> {
    db.prepare_cached("SELECT COALESCE(MAX(ordinal)+1,0) FROM thread_path WHERE thread_id=?1")
        .map_err(sql_error)?
        .query_row([thread], |r| r.get(0))
        .map_err(sql_error)
}
fn node_at(db: &Connection, thread: &str, ordinal: i64) -> Result<Option<String>, ApiError> {
    db.prepare_cached("SELECT node_id FROM thread_path WHERE thread_id=?1 AND ordinal=?2")
        .map_err(sql_error)?
        .query_row(params![thread, ordinal], |r| r.get(0))
        .optional()
        .map_err(sql_error)
}
/// Ordinal of the last node both paths share, or None when they share none.
fn shared_end(
    db: &Connection,
    child: &str,
    parent: &str,
    child_len: i64,
    parent_len: i64,
) -> Result<Option<i64>, ApiError> {
    let common = child_len.min(parent_len);
    if common == 0 {
        return Ok(None);
    }
    let same = |ordinal: i64| -> Result<bool, ApiError> {
        Ok(
            match (node_at(db, child, ordinal)?, node_at(db, parent, ordinal)?) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        )
    };
    if !same(0)? {
        return Ok(None);
    }
    let (mut low, mut high) = (0, common - 1);
    while low < high {
        let middle = (low + high + 1) / 2;
        if same(middle)? {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Ok(Some(low))
}
fn excerpt_of(node_id: String, payload: Option<String>) -> Excerpt {
    let text = payload
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .map(|payload| super::conversation_excerpt(&payload, EXCERPT_CHARS + 1))
        .map(|text| crate::presentation::sanitize_plain(&text, 0).0)
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "));
    let truncated = text
        .as_ref()
        .is_some_and(|t| t.chars().count() > EXCERPT_CHARS);
    Excerpt {
        node_id,
        text: text.map(|t| t.chars().take(EXCERPT_CHARS).collect()),
        truncated,
    }
}
/// The last user request at or before `upto`, searched `SCAN_ROWS` rows back.
fn last_request(db: &Connection, thread: &str, upto: i64) -> Result<Option<Excerpt>, ApiError> {
    let row: Option<(String, Option<String>)> = db
        .prepare_cached("SELECT n.id,CASE WHEN n.payload_bytes<=?4 THEN n.payload_json END FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal<=?2 AND p.ordinal>?3 AND n.kind='user' ORDER BY p.ordinal DESC LIMIT 1")
        .map_err(sql_error)?
        .query_row(params![thread, upto, upto - SCAN_ROWS, PAYLOAD_BYTES], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(sql_error)?;
    Ok(row.map(|(id, payload)| excerpt_of(id, payload)))
}
fn exists(db: &Connection, sql: &str, thread: &str) -> Result<bool, ApiError> {
    db.prepare_cached(sql)
        .map_err(sql_error)?
        .query_row([thread], |r| r.get(0))
        .map_err(sql_error)
}

/// Where a child leaves its parent: lookups only, no payload is read.
struct Point {
    node_id: String,
    ordinal: i64,
    parent_continues: bool,
}
type Located = (Option<Point>, Option<&'static str>);
fn locate(db: &Connection, member: &Member, length: i64) -> Result<Located, ApiError> {
    let Some(parent) = &member.parent else {
        return Ok((None, None));
    };
    let parent_len = path_len(db, parent)?;
    let Some(end) = shared_end(db, &member.id, parent, length, parent_len)? else {
        return Ok((None, Some("no_shared_prefix")));
    };
    let node_id = node_at(db, &member.id, end)?.ok_or_else(|| {
        ApiError::new(ErrorCode::InvalidState, "branch divergence node is missing")
    })?;
    Ok((
        Some(Point {
            node_id,
            ordinal: end,
            parent_continues: parent_len > end + 1,
        }),
        None,
    ))
}

fn item(
    db: &Connection,
    member: &Member,
    requested: &str,
    chain: &[String],
) -> Result<BranchItem, ApiError> {
    let length = path_len(db, &member.id)?;
    let tip = if length > 0 {
        let tip_node = node_at(db, &member.id, length - 1)?;
        match tip_node {
            Some(node_id) => {
                let at: String = db
                    .prepare_cached("SELECT created_at FROM node WHERE id=?1")
                    .map_err(sql_error)?
                    .query_row([&node_id], |r| r.get(0))
                    .map_err(sql_error)?;
                Some(Tip {
                    last_request: last_request(db, &member.id, length - 1)?,
                    node_id,
                    at,
                })
            }
            None => None,
        }
    } else {
        None
    };
    let (point, unknown) = locate(db, member, length)?;
    let divergence = match point {
        Some(point) => Some(Divergence {
            request: last_request(db, &member.id, point.ordinal)?,
            node_id: point.node_id,
            ordinal: point.ordinal,
            parent_continues: point.parent_continues,
        }),
        None => None,
    };
    Ok(BranchItem {
        thread_id: member.id.clone(),
        title: (!member.title.trim().is_empty()).then(|| member.title.clone()),
        parent_thread_id: member.parent.clone(),
        branch_kind: match member.branch_kind.as_deref() {
            Some("fork") => "fork",
            Some("rewind") => "rewind",
            _ => "unknown",
        },
        depth: member.depth,
        state: if member.id == requested {
            "current"
        } else if chain.contains(&member.id) {
            "ancestor"
        } else {
            "branch"
        },
        running: exists(
            db,
            "SELECT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state IN ('live','stopping'))",
            &member.id,
        )?,
        pruned_originals: exists(
            db,
            "SELECT EXISTS(SELECT 1 FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id WHERE tr.thread_id=?1 AND r.state='pruned')",
            &member.id,
        )?,
        size: Size {
            path_nodes: length,
            nodes_after_divergence: divergence.as_ref().map(|d| length - d.ordinal - 1),
            bytes: None,
        },
        divergence,
        divergence_unknown: unknown,
        tip,
    })
}

/// Identity of the listed tree: the ordered (Thread, parent) pairs and the
/// truncation flags. A cursor is valid exactly while this is unchanged, so
/// appends in any Thread (which bump `root_revision`) do not expire it.
fn fingerprint(family: &Family) -> u64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for member in &family.members {
        hasher.update(member.id.as_bytes());
        hasher.update([0]);
        hasher.update(member.parent.as_deref().unwrap_or("").as_bytes());
        hasher.update([1]);
    }
    hasher.update([
        family.family_truncated as u8,
        family.ancestors_truncated as u8,
    ]);
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().expect("8 bytes"))
}

/// `thread branches`: one page of the family tree plus its bounds.
pub fn branches(
    db: &Connection,
    requested: &str,
    args: &ThreadBranchesArgs,
) -> Result<Value, ApiError> {
    if args.limit == 0 || args.limit > MAX_LIST_LIMIT {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "branch limit must be in 1..=200",
        ));
    }
    let family = family(db, requested, Scope::Full)?;
    let revision = fingerprint(&family);
    let filter = serde_json::to_string(&(requested, "branches")).map_err(|_| expired())?;
    let offset = match &args.cursor {
        Some(token) => {
            let cursor = PageCursor::decode(token, SCOPE, &filter, revision, None)?;
            cursor
                .last_sort_key
                .parse::<usize>()
                .map_err(|_| expired())?
        }
        None => 0,
    };
    if offset > family.members.len() {
        return Err(expired());
    }
    let end = (offset + args.limit as usize).min(family.members.len());
    let items = family.members[offset..end]
        .iter()
        .map(|member| item(db, member, requested, &family.chain))
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if end < family.members.len() {
        Some(
            PageCursor {
                version: 1,
                scope: SCOPE.into(),
                filter,
                last_sort_key: end.to_string(),
                watermark: 0,
                revision,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    let page = Page { items, next_cursor };
    let mut value = serde_json::to_value(page)
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "branches are not serializable"))?;
    value["thread_id"] = requested.into();
    value["root_thread_id"] = family.chain[0].clone().into();
    value["family_threads"] = family.members.len().into();
    value["family_truncated"] = family.family_truncated.into();
    value["ancestors_truncated"] = family.ancestors_truncated.into();
    value["root_parent_unavailable"] = family.root_parent_unavailable.into();
    value["limits"] = serde_json::json!({
        "family_threads": FAMILY_LIMIT,
        "ancestors": ANCESTOR_LIMIT,
        "request_scan_rows": SCAN_ROWS,
        "excerpt_chars": EXCERPT_CHARS,
        "bytes": "not_measured",
    });
    Ok(value)
}
fn expired() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "branch page position is unavailable",
    )
}

/// One alternative at a divergence point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alternative {
    pub thread_id: String,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub running: bool,
    pub pruned_originals: bool,
}
/// The alternatives meeting at one node of the viewed Thread's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub anchor_node_id: String,
    pub alternatives: Vec<Alternative>,
    /// The alternative the viewed Thread follows here.
    pub current: usize,
}

/// Markers for the nodes of `thread`'s path that appear in `nodes`.
///
/// Divergence points come from lookups alone; the tip scan, JSON parse and run
/// and prune flags are read only for the Threads of a point that is on the page.
pub fn markers(db: &Connection, thread: &str, nodes: &[String]) -> Result<Vec<Marker>, ApiError> {
    let wanted: HashSet<&str> = nodes.iter().map(String::as_str).collect();
    // A Thread with no parent and no child has no divergence point.
    let has_relative = exists(
        db,
        "SELECT EXISTS(SELECT 1 FROM thread t WHERE t.id=?1 AND t.parent_thread_id IS NOT NULL) OR EXISTS(SELECT 1 FROM thread c WHERE c.parent_thread_id=?1 AND c.deleted_at IS NULL)",
        thread,
    )?;
    if !has_relative || wanted.is_empty() {
        return Ok(Vec::new());
    }
    let family = family(db, thread, Scope::Local)?;
    // (parent, anchor node) -> (child index, divergence ordinal), for points on the page.
    type PagePoint = (String, String, Vec<(usize, i64)>);
    let mut points: Vec<PagePoint> = Vec::new();
    for (index, member) in family.members.iter().enumerate() {
        let Some(parent) = &member.parent else {
            continue;
        };
        if !family.chain.contains(parent) {
            continue;
        }
        let length = path_len(db, &member.id)?;
        let (Some(point), _) = locate(db, member, length)? else {
            continue;
        };
        if !wanted.contains(point.node_id.as_str()) {
            continue;
        }
        match points
            .iter_mut()
            .find(|(p, n, _)| p == parent && n == &point.node_id)
        {
            Some((_, _, kids)) => kids.push((index, point.ordinal)),
            None => points.push((parent.clone(), point.node_id, vec![(index, point.ordinal)])),
        }
    }
    let mut cache: Vec<Option<BranchItem>> = vec![None; family.members.len()];
    let mut load = |index: usize| -> Result<BranchItem, ApiError> {
        if cache[index].is_none() {
            cache[index] = Some(item(db, &family.members[index], thread, &family.chain)?);
        }
        Ok(cache[index].clone().expect("loaded"))
    };
    let viewed = family
        .members
        .iter()
        .position(|m| m.id == thread)
        .expect("the requested Thread is listed");
    let mut out = Vec::new();
    for (parent, anchor, kids) in points {
        let ordinal = kids[0].1;
        let parent_index = family
            .members
            .iter()
            .position(|m| m.id == parent)
            .expect("a parent in the chain is listed");
        let mut indexes: Vec<usize> = vec![parent_index];
        indexes.extend(kids.iter().map(|(i, _)| *i));
        // Write order of each alternative's first node after the divergence point.
        let mut keyed = Vec::new();
        for index in indexes {
            let id = &family.members[index].id;
            let first: Option<String> = match node_at(db, id, ordinal + 1)? {
                Some(next) => db
                    .query_row("SELECT created_at FROM node WHERE id=?1", [next], |r| {
                        r.get(0)
                    })
                    .optional()
                    .map_err(sql_error)?,
                None => None,
            };
            // An alternative that ends at the point sorts first.
            keyed.push((first.unwrap_or_default(), id.clone(), index));
        }
        keyed.sort();
        // The deepest lineage member among the alternatives is the one followed.
        let Some(current) = keyed
            .iter()
            .enumerate()
            .filter_map(|(i, (_, id, _))| {
                family
                    .chain
                    .iter()
                    .position(|c| c == id)
                    .map(|depth| (depth, i))
            })
            .max()
            .map(|(_, i)| i)
        else {
            continue;
        };
        if keyed.len() < 2 {
            continue;
        }
        let mut alternatives = Vec::new();
        for (position, (_, _, index)) in keyed.iter().enumerate() {
            // The followed alternative shows the viewed Thread's own last request.
            let source = load(if position == current { viewed } else { *index })?;
            alternatives.push(Alternative {
                thread_id: family.members[*index].id.clone(),
                title: source.title.clone(),
                excerpt: source
                    .tip
                    .as_ref()
                    .and_then(|t| t.last_request.as_ref())
                    .and_then(|r| r.text.clone()),
                running: source.running,
                pruned_originals: source.pruned_originals,
            });
        }
        out.push(Marker {
            anchor_node_id: anchor,
            alternatives,
            current,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
