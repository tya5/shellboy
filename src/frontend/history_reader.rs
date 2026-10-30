//! Bounded saved-history query, projection, enrichment and delivery.
use super::*;
use crate::storage::StorageError;
use rusqlite::{Connection, OptionalExtension};

pub(super) struct HistoryReader<'a> {
    control: &'a Control,
    updates: &'a dyn UpdateTarget,
    width: u16,
}
pub(super) struct Reply {
    thread: String,
    generation: u64,
}
impl Reply {
    pub(super) fn new(thread: impl Into<String>, generation: u64) -> Self {
        Self {
            thread: thread.into(),
            generation,
        }
    }
    pub(super) async fn failed(&self, updates: &dyn UpdateTarget, error: ApiError) {
        load_failed(updates, self.thread.clone(), self.generation, error).await;
    }
}
pub(super) struct WindowReply {
    target: Reply,
    view_revision: u64,
}
impl WindowReply {
    pub(super) fn new(thread: impl Into<String>, generation: u64, view_revision: u64) -> Self {
        Self {
            target: Reply::new(thread, generation),
            view_revision,
        }
    }
    pub(super) async fn failed(&self, updates: &dyn UpdateTarget, error: ApiError) {
        load_failed_at(
            updates,
            self.target.thread.clone(),
            self.target.generation,
            Some(self.view_revision),
            error,
        )
        .await;
    }
    pub(super) async fn deliver_node(&self, updates: &dyn UpdateTarget, node: UiNode) {
        update(
            updates,
            UiUpdate::NodeWindow {
                thread_id: self.target.thread.clone(),
                generation: self.target.generation,
                view_revision: self.view_revision,
                node,
            },
        )
        .await;
    }
    async fn deliver(
        &self,
        updates: &dyn UpdateTarget,
        nodes: Vec<UiNode>,
        info: HistoryInfo,
        anchor: ViewAnchor,
    ) {
        stream_commits(
            updates,
            &self.target.thread,
            self.target.generation,
            info.committed,
        )
        .await;
        update(
            updates,
            UiUpdate::HistoryWindow {
                thread_id: self.target.thread.clone(),
                generation: self.target.generation,
                view_revision: self.view_revision,
                nodes,
                pending: info.pending,
                read_revision: info.read_revision,
                first_ordinal: info.first,
                selection_revision: info.revision,
                total_nodes: info.total,
                anchor,
            },
        )
        .await;
    }
}
pub(super) struct WindowRequest {
    ordinal: u64,
    position: NodePosition,
    expected_revision: u64,
}
impl WindowRequest {
    pub(super) fn new(ordinal: u64, offset: usize, from_end: bool, expected_revision: u64) -> Self {
        Self {
            ordinal,
            position: if from_end {
                NodePosition::FromEnd(offset)
            } else {
                NodePosition::At(offset)
            },
            expected_revision,
        }
    }
}
struct HistoryInfo {
    first: u64,
    revision: u64,
    total: u64,
    committed: Vec<(String, String)>,
    pending: PendingMessages,
    read_revision: u64,
}
struct RawHistory {
    rows: Vec<PathNode>,
    info: HistoryInfo,
}
impl RawHistory {
    // Called only on the existing finite presentation worker. Choose an offset
    // before consuming each row, then normalize the resulting canonical anchor.
    fn project(
        self,
        width: u16,
        mut position: impl FnMut(usize, &PathNode) -> Option<NodePosition>,
    ) -> (Vec<UiNode>, HistoryInfo, Option<ViewAnchor>) {
        let mut anchor = None;
        let read_revision = self.info.read_revision;
        let nodes = self
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let wanted = position(index, &row);
                let mut node = row.node.into_ui_node(wanted, width);
                stamp_source(&mut node, read_revision);
                if let Some(wanted) = wanted {
                    anchor = Some(ViewAnchor {
                        node_id: Some(node.node_id.clone()),
                        char_offset: wanted.offset(node.total_chars),
                    });
                }
                node
            })
            .collect();
        (nodes, self.info, anchor)
    }
}
impl<'a> HistoryReader<'a> {
    pub(super) fn new(control: &'a Control, updates: &'a dyn UpdateTarget, width: u16) -> Self {
        Self {
            control,
            updates,
            width,
        }
    }
    pub(super) async fn saved(
        control: &Control,
        width: u16,
        draft_id: String,
        revision: u64,
        generation: u64,
        value: &Value,
    ) -> Result<UiUpdate, ApiError> {
        let result = Self::saved_inner(control, width, draft_id, revision, generation, value).await;
        checked_storage(control, result).await
    }
    async fn saved_inner(
        control: &Control,
        width: u16,
        draft_id: String,
        revision: u64,
        generation: u64,
        value: &Value,
    ) -> Result<UiUpdate, ApiError> {
        let thread = value["thread_id"]
            .as_str()
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "Accepted input has no Thread"))?
            .to_owned();
        let target = thread.clone();
        let (raw, before_cursor) = control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let (mut raw, cursor) = query_page(&tx, &target, None, None)?;
                let cursor = cursor.map_err(StorageError::from)?;
                raw.info.pending = message_queue::query(&tx, &target)?;
                raw.info.read_revision = message_queue::read_revision();
                tx.commit()?;
                Ok((raw, cursor))
            })
            .await
            .map_err(storage_error)?;
        let (mut nodes, info, _) =
            presentation_work(move || raw.project(width, |_, _| None)).await?;
        message_metadata::enrich(control, Some(&thread), &mut nodes).await?;
        streams::enrich(control, &mut nodes, width).await?;
        stamp_sources(&mut nodes, info.read_revision);
        let history = UiUpdate::History {
            thread_id: thread.clone(),
            generation,
            nodes,
            pending: info.pending,
            read_revision: info.read_revision,
            before_cursor,
            append: false,
            first_ordinal: info.first,
            selection_revision: info.revision,
            total_nodes: info.total,
        };
        Ok(UiUpdate::SavedMessage {
            draft_id,
            revision,
            generation,
            thread_id: thread,
            status: if value["delivery_status"] == "held" {
                ReceiptStatus::Held
            } else {
                ReceiptStatus::Accepted
            },
            history: Box::new(history),
        })
    }
    pub(super) async fn rebase(
        &self,
        reply: &WindowReply,
        anchor: ViewAnchor,
    ) -> Result<(), ApiError> {
        let thread = reply.target.thread.clone();
        let wanted_offset = anchor.char_offset;
        let result = self
            .control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let (mut raw, ordinal) = query_rebase(&tx, &thread, &anchor)?;
                raw.info.pending = message_queue::query(&tx, &thread)?;
                raw.info.read_revision = message_queue::read_revision();
                tx.commit()?;
                Ok((raw, ordinal))
            })
            .await
            .map_err(storage_error);
        let (raw, ordinal) = checked_storage(self.control, result).await?;
        let width = self.width;
        let (mut nodes, info, anchor) = presentation_work(move || {
            raw.project(width, |index, row| {
                if Some(row.ordinal) == ordinal {
                    Some(NodePosition::At(wanted_offset))
                } else if ordinal.is_none() && index == 0 {
                    Some(NodePosition::At(0))
                } else {
                    None
                }
            })
        })
        .await?;
        message_metadata::enrich(self.control, Some(&reply.target.thread), &mut nodes).await?;
        let enriched = streams::enrich(self.control, &mut nodes, self.width).await;
        checked_storage(self.control, enriched).await?;
        stamp_sources(&mut nodes, info.read_revision);
        let selection_revision = info.revision;
        reply
            .deliver(self.updates, nodes, info, anchor.unwrap_or_default())
            .await;
        received::refresh(
            self.control,
            self.updates,
            &reply.target.thread,
            reply.target.generation,
            self.width,
            received::Scope {
                selection_revision,
                view_revision: Some(reply.view_revision),
            },
        )
        .await?;
        Ok(())
    }
    pub(super) async fn window(
        &self,
        reply: &WindowReply,
        request: WindowRequest,
    ) -> Result<(), ApiError> {
        let result = async {
            let thread = reply.target.thread.clone();
            let ordinal = request.ordinal;
            let position = request.position;
            let raw = self
                .control
                .store
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let mut raw = query_window(&tx, &thread, &request)?;
                    raw.info.pending = message_queue::query(&tx, &thread)?;
                    raw.info.read_revision = message_queue::read_revision();
                    tx.commit()?;
                    Ok(raw)
                })
                .await
                .map_err(storage_error)?;
            let width = self.width;
            let (mut nodes, info, anchor) = presentation_work(move || {
                raw.project(width, |_, row| {
                    (row.ordinal as u64 == ordinal).then_some(position)
                })
            })
            .await?;
            message_metadata::enrich(self.control, Some(&reply.target.thread), &mut nodes).await?;
            streams::enrich(self.control, &mut nodes, self.width).await?;
            stamp_sources(&mut nodes, info.read_revision);
            let anchor = anchor.ok_or_else(|| {
                ApiError::new(
                    ErrorCode::TargetUnavailable,
                    "Requested history ordinal is no longer selected",
                )
            })?;
            let selection_revision = info.revision;
            reply.deliver(self.updates, nodes, info, anchor).await;
            received::refresh(
                self.control,
                self.updates,
                &reply.target.thread,
                reply.target.generation,
                self.width,
                received::Scope {
                    selection_revision,
                    view_revision: Some(reply.view_revision),
                },
            )
            .await?;
            Ok(())
        }
        .await;
        checked_storage(self.control, result).await
    }
    pub(super) async fn page(
        &self,
        reply: &Reply,
        before: Option<String>,
        anchor: Option<ViewAnchor>,
    ) -> Result<(), ApiError> {
        let result = async {
            let thread = reply.thread.clone();
            let append = before.is_some();
            let db_anchor = anchor.clone();
            let (raw, before_cursor) = self
                .control
                .store
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let (mut raw, cursor) = query_page(&tx, &thread, before, db_anchor)?;
                    raw.info.pending = message_queue::query(&tx, &thread)?;
                    raw.info.read_revision = message_queue::read_revision();
                    tx.commit()?;
                    Ok((raw, cursor))
                })
                .await
                .map_err(storage_error)?;
            let width = self.width;
            let (mut nodes, info, _) = presentation_work(move || {
                raw.project(width, |_, row| {
                    anchor
                        .as_ref()
                        .filter(|anchor| anchor.node_id.as_deref() == Some(&row.node.id))
                        .map(|anchor| NodePosition::At(anchor.char_offset))
                })
            })
            .await?;
            message_metadata::enrich(self.control, Some(&reply.thread), &mut nodes).await?;
            streams::enrich(self.control, &mut nodes, self.width).await?;
            stamp_sources(&mut nodes, info.read_revision);
            let before_cursor = before_cursor?;
            stream_commits(
                self.updates,
                &reply.thread,
                reply.generation,
                info.committed,
            )
            .await;
            update(
                self.updates,
                UiUpdate::History {
                    thread_id: reply.thread.clone(),
                    generation: reply.generation,
                    nodes,
                    pending: info.pending,
                    read_revision: info.read_revision,
                    before_cursor,
                    append,
                    first_ordinal: info.first,
                    selection_revision: info.revision,
                    total_nodes: info.total,
                },
            )
            .await;
            received::refresh(
                self.control,
                self.updates,
                &reply.thread,
                reply.generation,
                self.width,
                received::Scope {
                    selection_revision: info.revision,
                    view_revision: None,
                },
            )
            .await?;
            Ok(())
        }
        .await;
        checked_storage(self.control, result).await
    }
    /// Item-scoped link hydration never installs off-page bodies into the
    /// conversation window and never reads shell raw output.
    pub(super) async fn links(
        &self,
        node_ids: Vec<String>,
    ) -> Result<Vec<crate::presentation::PresentationLink>, ApiError> {
        if node_ids.is_empty()
            || node_ids.len() > 2
            || node_ids.iter().any(|id| id.is_empty() || id.len() > 256)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Link source requires one or two bounded node IDs",
            ));
        }
        let result = async {
            let rows = self.control.store.database.call(move |db| {
                let tx = db.transaction()?;
                let mut rows = Vec::with_capacity(node_ids.len());
                let mut query = tx.prepare("SELECT id,kind,CASE WHEN length(CAST(payload_json AS BLOB))<=98304 THEN payload_json END,deleted_reason FROM node WHERE id=?1")?;
                for id in node_ids {
                    let row = query.query_row([id], |row|Ok(RawNode {
                        id:row.get(0)?,kind:row.get(1)?,payload:row.get(2)?,reason:row.get(3)?,received_recovery_wait:false,
                    })).optional()?;
                    let Some(row) = row.filter(|row|row.payload.is_some() && row.reason.is_none()) else {
                        return Err(StorageError::from(ApiError::new(ErrorCode::TargetUnavailable,"Link source is unavailable, deleted, or larger than the bounded preview allowance; use original access")));
                    };
                    rows.push(row);
                }
                drop(query);
                tx.commit()?;
                Ok(rows)
            }).await.map_err(storage_error)?;
            let width=self.width;
            presentation_work(move || {
                let mut links=Vec::<crate::presentation::PresentationLink>::new();
                for row in rows {
                    if row.payload.as_deref().is_none_or(|payload|serde_json::from_str::<Value>(payload).is_err()) {
                        return Err(ApiError::new(ErrorCode::TargetUnavailable,"Saved link source is malformed; use original access"));
                    }
                    let node=row.into_ui_node(Some(NodePosition::At(0)),width);
                    for link in node.presentation.links {
                        if links.len() >= 128 {return Ok(links);}
                        if !links.iter().any(|old|old.url==link.url) {links.push(link);}
                    }
                }
                Ok(links)
            }).await?
        }.await;
        checked_storage(self.control, result).await
    }

    pub(super) async fn node(
        &self,
        node_id: String,
        offset: usize,
        viewed_thread: &str,
    ) -> Result<UiNode, ApiError> {
        let result = async {
            let (row, read_revision) = self
                .control
                .store
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let row = tx.query_row(
                            "SELECT n.id,n.kind,n.payload_json,n.deleted_reason,CASE WHEN n.kind='incomplete_response' THEN EXISTS(SELECT 1 FROM turn u WHERE u.id=n.turn_id AND u.state='recovery_wait') ELSE 0 END FROM node n WHERE n.id=?1",
                            [&node_id],
                            |r| {
                                Ok(RawNode {
                                    id: r.get(0)?,
                                    kind: r.get(1)?,
                                    payload: r.get(2)?,
                                    reason: r.get(3)?,
                                    received_recovery_wait: r.get(4)?,
                                })
                            },
                        )
                        .optional()?;
                    let read_revision = message_queue::read_revision();
                    tx.commit()?;
                    Ok((row, read_revision))
                })
                .await
                .map_err(storage_error)?;
            let row = row.ok_or_else(|| {
                    ApiError::new(ErrorCode::TargetUnavailable, "History node is unavailable")
                })?;
            let width = self.width;
            let mut node =
                presentation_work(move || row.into_ui_node(Some(NodePosition::At(offset)), width))
                    .await?;
            message_metadata::enrich(self.control, Some(viewed_thread), std::slice::from_mut(&mut node)).await?;
            streams::enrich(self.control, std::slice::from_mut(&mut node), self.width).await?;
            stamp_source(&mut node, read_revision);
            Ok(node)
        }
        .await;
        checked_storage(self.control, result).await
    }
}
// Enrichment can rebuild a presentation; freshness and eviction remain source facts.
fn stamp_source(node: &mut UiNode, read_revision: u64) {
    node.presentation.read_revision = read_revision;
    node.presentation.body_evicted = false;
}
fn stamp_sources(nodes: &mut [UiNode], read_revision: u64) {
    for node in nodes {
        stamp_source(node, read_revision);
    }
}
fn live_thread_revision(db: &Connection, thread: &str) -> Result<i64, StorageError> {
    crate::history::selection_revision(db, thread, crate::history::ThreadScope::Live)
        .optional()?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::TargetUnavailable,
                "The selected Thread is no longer available",
            )
            .into()
        })
}
fn query_rebase(
    db: &Connection,
    thread: &str,
    anchor: &ViewAnchor,
) -> Result<(RawHistory, Option<i64>), StorageError> {
    let revision = live_thread_revision(db, thread)?;
    let total = history_total(db, thread)?;
    let ordinal = anchor
        .node_id
        .as_ref()
        .map(|id| {
            db.query_row(
                "SELECT ordinal FROM thread_path WHERE thread_id=?1 AND node_id=?2",
                rusqlite::params![thread, id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
        })
        .transpose()?
        .flatten();
    let first = ordinal.map_or_else(
        || total.saturating_sub(50).max(0),
        |position| position.saturating_sub(10).max(0),
    );
    let rows = history_rows(db, thread, PathRange::From(first))?;
    let committed = committed_streams(db, &rows)?;
    let first = rows.first().map_or(0, |row| row.ordinal as u64);
    Ok((
        RawHistory {
            rows,
            info: HistoryInfo {
                first,
                revision: revision as u64,
                total: total as u64,
                committed,
                pending: PendingMessages::default(),
                read_revision: 0,
            },
        },
        ordinal,
    ))
}
fn query_window(
    db: &Connection,
    thread: &str,
    request: &WindowRequest,
) -> Result<RawHistory, StorageError> {
    let revision = live_thread_revision(db, thread)? as u64;
    if revision != request.expected_revision {
        return Err(expired_history_cursor().into());
    }
    let total = history_total(db, thread)? as u64;
    let first = request.ordinal.saturating_sub(10);
    let rows = history_rows(db, thread, PathRange::From(first as i64))?;
    let committed = committed_streams(db, &rows)?;
    Ok(RawHistory {
        rows,
        info: HistoryInfo {
            first,
            revision,
            total,
            committed,
            pending: PendingMessages::default(),
            read_revision: 0,
        },
    })
}
fn query_page(
    db: &Connection,
    thread: &str,
    before: Option<String>,
    anchor: Option<ViewAnchor>,
) -> Result<(RawHistory, Result<Option<String>, ApiError>), StorageError> {
    let revision = live_thread_revision(db, thread)?;
    let total = history_total(db, thread)?;
    let upper = if let Some(cursor) = before {
        PageCursor::decode(&cursor, "ui_history", thread, revision as u64, None)
            .map_err(|_| StorageError::from(expired_history_cursor()))?
            .last_sort_key
            .parse::<i64>()
            .map_err(|_| StorageError::from(expired_history_cursor()))?
    } else if let Some(node) = anchor.as_ref().and_then(|anchor| anchor.node_id.as_ref()) {
        db.query_row(
            "SELECT ordinal+50 FROM thread_path WHERE thread_id=?1 AND node_id=?2",
            rusqlite::params![thread, node],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(i64::MAX)
    } else {
        i64::MAX
    };
    let rows = history_rows(db, thread, PathRange::Before(upper))?;
    let cursor = rows
        .first()
        .filter(|row| row.ordinal > 0)
        .map(|row| {
            PageCursor {
                version: 1,
                scope: "ui_history".into(),
                filter: thread.into(),
                last_sort_key: row.ordinal.to_string(),
                watermark: 0,
                revision: revision as u64,
                encoding: None,
            }
            .encode()
        })
        .transpose();
    let first = rows.first().map_or(0, |row| row.ordinal as u64);
    let committed = committed_streams(db, &rows)?;
    Ok((
        RawHistory {
            rows,
            info: HistoryInfo {
                first,
                revision: revision as u64,
                total: total as u64,
                committed,
                pending: PendingMessages::default(),
                read_revision: 0,
            },
        },
        cursor,
    ))
}
// Stream events carry model-attempt IDs; only the persisted output ledger can
// reconcile them with selected conversation nodes. Query only this bounded page
// and decline ambiguous attempt mappings rather than guessing from body text.
fn committed_streams(
    db: &rusqlite::Connection,
    rows: &[PathNode],
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut query = db.prepare(
        "SELECT DISTINCT o.attempt_id FROM output_item o
         WHERE o.node_id=?1 AND NOT EXISTS (
             SELECT 1 FROM output_item other
             WHERE other.attempt_id=o.attempt_id AND other.node_id IS NOT NULL
               AND other.node_id<>o.node_id
         ) ORDER BY o.attempt_id LIMIT 2",
    )?;
    let mut result = Vec::new();
    for node in rows
        .iter()
        .map(|row| &row.node)
        .filter(|node| node.kind == "assistant_response")
    {
        let attempts = query
            .query_map([&node.id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // Production commits one response node per attempt and one attempt per
        // response. An inconsistent imported ledger must not create unbounded
        // events or choose an arbitrary identity.
        if attempts.len() == 1 {
            result.push((attempts.into_iter().next().unwrap(), node.id.clone()));
        }
    }
    Ok(result)
}
struct PathNode {
    ordinal: i64,
    node: RawNode,
}
enum PathRange {
    From(i64),
    Before(i64),
}
fn history_total(db: &rusqlite::Connection, thread: &str) -> rusqlite::Result<i64> {
    db.query_row(
        "SELECT COALESCE(MAX(ordinal)+1,0) FROM thread_path WHERE thread_id=?1",
        [thread],
        |row| row.get(0),
    )
}
fn history_rows(
    db: &rusqlite::Connection,
    thread: &str,
    range: PathRange,
) -> rusqlite::Result<Vec<PathNode>> {
    let (sql, bound) = match range {
        PathRange::From(bound) => (
            "SELECT p.ordinal,n.id,n.kind,n.payload_json,n.deleted_reason,CASE WHEN n.kind='incomplete_response' THEN EXISTS(SELECT 1 FROM turn u WHERE u.id=n.turn_id AND u.state='recovery_wait') ELSE 0 END FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 ORDER BY p.ordinal LIMIT 50",
            bound,
        ),
        PathRange::Before(bound) => (
            "SELECT p.ordinal,n.id,n.kind,n.payload_json,n.deleted_reason,CASE WHEN n.kind='incomplete_response' THEN EXISTS(SELECT 1 FROM turn u WHERE u.id=n.turn_id AND u.state='recovery_wait') ELSE 0 END FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal<?2 ORDER BY p.ordinal DESC LIMIT 50",
            bound,
        ),
    };
    let mut rows = db
        .prepare(sql)?
        .query_map(rusqlite::params![thread, bound], raw_node_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if matches!(range, PathRange::Before(_)) {
        rows.reverse();
    }
    Ok(rows)
}
fn raw_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PathNode> {
    Ok(PathNode {
        ordinal: row.get(0)?,
        node: RawNode {
            id: row.get(1)?,
            kind: row.get(2)?,
            payload: row.get(3)?,
            reason: row.get(4)?,
            received_recovery_wait: row.get(5)?,
        },
    })
}

#[cfg(test)]
mod detail_link_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offwindow_sources_are_bounded_and_unavailable_is_not_an_empty_link_list() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        owner.db.call(|db| {
            let call=json!({"text":"[call source](https://example.test/call)"}).to_string();
            let result=json!({"item":{"type":"function_call_output","call_id":"call","output":json!({"status":"exited","exit_code":0,"output":"https://example.test/result"}).to_string()}}).to_string();
            for (id,kind,payload) in [("call","assistant_response",call),("result","tool_result",result),("large","assistant_response",json!({"text":"x".repeat(100_000)}).to_string())] {
                db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'now',?3,length(CAST(?3 AS BLOB)))",rusqlite::params![id,kind,payload])?;
            }
            db.execute("INSERT INTO node(id,kind,created_at,deleted_reason)VALUES('deleted','tool_result','now','gc')",[])?;
            Ok(())
        }).await.unwrap();
        let (updates, _incoming) = std::sync::mpsc::sync_channel(8);
        let reader = HistoryReader::new(&control, &updates, 80);
        let links = reader
            .links(vec!["call".into(), "result".into()])
            .await
            .unwrap();
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://example.test/call")
        );
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://example.test/result")
        );
        for ids in [vec![], vec!["call".into(); 3], vec!["x".repeat(257)]] {
            assert_eq!(
                reader.links(ids).await.unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
        for id in ["large", "deleted", "absent"] {
            assert_eq!(
                reader.links(vec![id.into()]).await.unwrap_err().code,
                ErrorCode::TargetUnavailable
            );
        }
        owner.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod read_stamping_tests {
    use super::*;
    use serde_json::json;

    fn rows() -> Vec<PathNode> {
        [
            ("text", "user", json!({"role":"user","content":[{"type":"input_text","text":"synthetic source"}]})),
            ("tool", "tool_result", json!({"item":{"type":"function_call_output","call_id":"synthetic-call","output":json!({"status":"cancelled","executed":false,"reason":"human_turn_interrupted","message":"Not executed"}).to_string()}})),
            ("tool_finished", "tool_result", json!({"item":{"type":"function_call_output","call_id":"finished-call","output":json!({"status":"exited","exit_code":0,"output":"synthetic tool output"}).to_string()}})),
            ("empty", "event", json!({"text":""})),
        ].into_iter().enumerate().map(|(ordinal,(id,kind,payload))|PathNode {
            ordinal:ordinal as i64,
            node:RawNode{id:id.into(),kind:kind.into(),payload:Some(payload.to_string()),reason:None,received_recovery_wait:false},
        }).collect()
    }

    #[test]
    fn projected_source_windows_inherit_the_paired_snapshot_and_empty_is_not_evicted() {
        let raw = RawHistory {
            rows: rows(),
            info: HistoryInfo {
                first: 0,
                revision: 1,
                total: 4,
                committed: vec![],
                pending: PendingMessages::default(),
                read_revision: 73,
            },
        };
        let (nodes, info, _) = raw.project(80, |_, _| None);
        assert_eq!(nodes.len(), 4);
        assert_eq!(info.read_revision, 73);
        for node in &nodes {
            assert_eq!(node.presentation.read_revision, 73);
            assert!(!node.presentation.body_evicted);
        }
        assert_eq!(
            nodes[3].total_chars, 0,
            "a known empty canonical body remains a successful read"
        );
    }

    #[test]
    fn ephemeral_window_facts_do_not_change_serialized_content_or_projection_identity() {
        let mut node = UiNode {
            node_id: "source".into(),
            role: "assistant".into(),
            text: "retained text".into(),
            total_chars: 13,
            presentation: crate::presentation::project(
                "assistant_response",
                &json!({"text":"retained text"}),
                "retained text",
                0,
            ),
            ..Default::default()
        };
        streams::seal_node(&mut node);
        let before = node.presentation.projection_stamp;
        let serialized = serde_json::to_value(&node).unwrap();
        node.presentation.read_revision = 901;
        node.presentation.body_evicted = true;
        streams::seal_node(&mut node);
        assert_eq!(node.presentation.projection_stamp, before);
        assert_eq!(serde_json::to_value(&node).unwrap(), serialized);
        let decoded: UiNode = serde_json::from_value(serialized).unwrap();
        assert_eq!(decoded.presentation.read_revision, 0);
        assert!(!decoded.presentation.body_evicted);
    }

    #[tokio::test]
    async fn node_hydration_and_enriched_pages_share_the_existing_serialized_read_sequence() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                for row in rows() {
                    crate::history::append_node(
                        &tx,
                        "thread",
                        &row.node.id,
                        None,
                        &row.node.kind,
                        &serde_json::from_str::<Value>(row.node.payload.as_ref().unwrap()).unwrap(),
                        "now",
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let (updates, incoming) = std::sync::mpsc::sync_channel(32);
        let reader = HistoryReader::new(&control, &updates, 80);
        reader
            .page(&Reply::new("thread", 1), None, None)
            .await
            .unwrap();
        let first = loop {
            match incoming.try_recv().unwrap() {
                UiUpdate::History {
                    nodes,
                    read_revision,
                    ..
                } => {
                    assert_eq!(nodes.len(), 4);
                    for node in nodes {
                        assert_eq!(node.presentation.read_revision, read_revision);
                        assert!(!node.presentation.body_evicted);
                    }
                    break read_revision;
                }
                _ => continue,
            }
        };
        assert!(first > 0);
        let hydrated = reader
            .node("tool_finished".into(), 0, "thread")
            .await
            .unwrap();
        assert!(hydrated.presentation.read_revision > first);
        assert!(!hydrated.presentation.body_evicted);
        let empty = reader.node("empty".into(), 0, "thread").await.unwrap();
        assert!(empty.presentation.read_revision > hydrated.presentation.read_revision);
        assert_eq!(empty.total_chars, 0);
        assert!(!empty.presentation.body_evicted);
        reader
            .page(&Reply::new("thread", 1), None, None)
            .await
            .unwrap();
        let last = loop {
            match incoming.try_recv().unwrap() {
                UiUpdate::History {
                    nodes,
                    read_revision,
                    ..
                } => {
                    for node in nodes {
                        assert_eq!(node.presentation.read_revision, read_revision);
                        assert!(!node.presentation.body_evicted);
                    }
                    break read_revision;
                }
                _ => continue,
            }
        };
        assert!(last > empty.presentation.read_revision);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
