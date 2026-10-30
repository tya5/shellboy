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
pub(super) struct HistoryInfo {
    pub(super) first: u64,
    pub(super) revision: u64,
    pub(super) total: u64,
    pub(super) committed: Vec<(String, String)>,
    pub(super) pending: PendingMessages,
    pub(super) read_revision: u64,
}
pub(super) struct RawHistory {
    rows: Vec<PathNode>,
    info: HistoryInfo,
}
impl RawHistory {
    /// Borrow only the identities in this bounded page, without cloning payloads.
    #[cfg(test)]
    pub(super) fn source_ids(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().map(|row| row.node.id.as_str())
    }

    /// Project on the existing finite presentation worker, never the DB worker.
    pub(super) fn project_page(
        self,
        width: u16,
        anchor: Option<ViewAnchor>,
    ) -> (Vec<UiNode>, HistoryInfo, Option<ViewAnchor>) {
        self.project(width, |_, row| {
            anchor
                .as_ref()
                .filter(|anchor| anchor.node_id.as_deref() == Some(&row.node.id))
                .map(|anchor| NodePosition::At(anchor.char_offset))
        })
    }

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
                let (raw, cursor) = query_page_tx(&tx, &target, None, None)?;
                let cursor = cursor.map_err(StorageError::from)?;
                tx.commit()?;
                Ok((raw, cursor))
            })
            .await
            .map_err(storage_error)?;
        let (mut nodes, info, _) = presentation_work(move || raw.project_page(width, None)).await?;
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
                    let (raw, cursor) = query_page_tx(&tx, &thread, before, db_anchor)?;
                    tx.commit()?;
                    Ok((raw, cursor))
                })
                .await
                .map_err(storage_error)?;
            let width = self.width;
            let (mut nodes, info, _) =
                presentation_work(move || raw.project_page(width, anchor)).await?;
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
    /// Bounded selected-item facts. No off-page body is installed into history
    /// and no shell output bytes are read. All saved/current facts share one job.
    pub(super) async fn links(
        &self,
        node_ids: Vec<String>,
    ) -> Result<DetailSourceSnapshot, ApiError> {
        if node_ids.is_empty()
            || node_ids.len() > 2
            || node_ids.iter().any(|id| id.is_empty() || id.len() > 256)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Detail source requires one or two bounded node IDs",
            ));
        }
        let result = async {
            let (rows, read_revision, observed_at) = self.control.store.database.call(move |db| {
                let tx = db.transaction()?;
                let mut rows = Vec::with_capacity(node_ids.len());
                let mut query = tx.prepare("SELECT id,kind,CASE WHEN length(CAST(payload_json AS BLOB))<=98304 THEN payload_json END,deleted_reason FROM node WHERE id=?1")?;
                // Node/group identity constrains every call; a matching call ID
                // elsewhere can never supply current lifecycle facts.
                let mut tools = tx.prepare(r"
                    SELECT CASE WHEN length(CAST(c.call_id AS BLOB))<=512 THEN c.call_id END,s.id,s.state,s.exit_code,s.output_complete,
                           s.retention_generation,
                           EXISTS(SELECT 1 FROM output_chunk o WHERE o.execution_id=s.id)
                    FROM tool_call c JOIN tool_group g ON g.id=c.group_id
                    LEFT JOIN shell_execution s ON s.id=c.operation_id AND s.turn_id=g.turn_id
                    WHERE c.group_id IN (
                      SELECT CASE WHEN json_valid(n.payload_json) THEN json_extract(n.payload_json,'$.group_id') END
                      FROM node n WHERE n.id=?1 AND n.kind='assistant_response'
                      UNION SELECT r.group_id FROM tool_result r WHERE r.node_id=?1)
                    AND (EXISTS(SELECT 1 FROM node n WHERE n.id=?1 AND n.kind='assistant_response'
                         AND json_valid(n.payload_json) AND json_extract(n.payload_json,'$.group_id')=c.group_id)
                         OR EXISTS(SELECT 1 FROM tool_result r WHERE r.node_id=?1
                            AND r.group_id=c.group_id AND r.call_id=c.call_id))
                    ORDER BY c.ordinal,c.call_id LIMIT 17")?;
                for id in node_ids {
                    let row = query.query_row([&id], |row|Ok(RawNode {
                        id:row.get(0)?,kind:row.get(1)?,payload:row.get(2)?,reason:row.get(3)?,received_recovery_wait:false,
                    })).optional()?;
                    let current = if row.as_ref().is_some_and(|row|row.payload.is_some() && row.reason.is_none()) { tools.query_map([&id], |row| Ok((
                        row.get::<_,Option<String>>(0)?,row.get::<_,Option<String>>(1)?,
                        row.get::<_,Option<String>>(2)?,row.get::<_,Option<i64>>(3)?,
                        row.get::<_,Option<bool>>(4)?,row.get::<_,Option<i64>>(5)?,
                        row.get::<_,bool>(6)?,
                    )))?.collect::<Result<Vec<_>,_>>()? } else {Vec::new()};
                    rows.push((id,row,current));
                }
                drop(query);
                drop(tools);
                let read_revision=message_queue::read_revision();
                let observed_at=chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
                tx.commit()?;
                Ok((rows,read_revision,observed_at))
            }).await.map_err(storage_error)?;
            let width=self.width;
            presentation_work(move || {
                let mut sources=Vec::with_capacity(rows.len());
                for (node_id,row,current) in rows {
                    let unavailable = |reason: &str| DetailSourceFacts {
                        node_id:node_id.clone(),kind:Default::default(),fields:vec![],links:vec![],
                        unavailable_reason:Some(reason.into()),
                    };
                    let Some(row)=row.filter(|row| row.payload.is_some() && row.reason.is_none()) else {
                        sources.push(unavailable("出典が削除・回収済み、または取得上限を超えています。O 原文で確認"));
                        continue;
                    };
                    if row.payload.as_deref().is_none_or(|payload|serde_json::from_str::<Value>(payload).is_err()) {
                        sources.push(unavailable("出典の形式を確認できません。O 原文で確認"));
                        continue;
                    }
                    let mut node=row.into_ui_node(Some(NodePosition::At(0)),width);
                    let mut fields=Vec::new();
                    let field=|key:String,label:&str,value:String| crate::presentation::PresentationField {
                        key,label:label.into(),value,display:None,value_omitted:false,
                    };
                    fields.push(field("/ui/detail_preview_bounds".into(),"詳細の取得範囲","最大16呼出し・128項目・128リンク、既存metadata上限内 · O原文で確認".into()));
                    if current.len()>16 {
                        fields.push(field("/current_tools/truncated".into(),"現在の実行情報","先頭16呼出しまで · 詳細はCLIで確認".into()));
                    }
                    for (index,(call,execution,state,code,complete,retention,has_output)) in current.into_iter().take(16).enumerate() {
                        let prefix=format!("/current_tools/{index}");
                        let mut call_field=field(format!("{prefix}/call_id"),"呼出しID",call.clone().unwrap_or_default());
                        if call.is_none() {call_field.display=Some("IDが表示上限を超えています · O原文".into());call_field.value_omitted=true;}
                        fields.push(call_field);
                        fields.push(field(format!("{prefix}/execution_id"),"実行ID",execution.unwrap_or_else(||"未取得".into())));
                        fields.push(field(format!("{prefix}/state"),"現在の実行状態",state.unwrap_or_else(||"利用不可・未実行".into())));
                        fields.push(field(format!("{prefix}/exit_code"),"現在の終了コード",code.map_or("未確定".into(),|v|v.to_string())));
                        fields.push(field(format!("{prefix}/output_complete"),"出力回収完了",complete.map_or("未取得".into(),|v|v.to_string())));
                        fields.push(field(format!("{prefix}/retention_generation"),"出力保持世代",retention.map_or("未取得".into(),|v|v.to_string())));
                        fields.push(field(format!("{prefix}/output_available"),"保存出力あり",has_output.to_string()));
                    }
                    fields.extend(node.presentation.fields.into_iter().take(128usize.saturating_sub(fields.len())));
                    node.presentation.blocks=Vec::new();
                    node.presentation.fields=fields;
                    node.presentation.links.truncate(128);
                    node.presentation.bound_metadata();
                    sources.push(DetailSourceFacts {node_id,kind:node.presentation.kind,fields:node.presentation.fields,
                        links:node.presentation.links,unavailable_reason:None});
                }
                Ok(DetailSourceSnapshot {read_revision,observed_at,sources})
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
/// Read one bounded history page and its pending-message projection in the
/// caller's existing transaction. The caller owns commit and presentation work.
pub(super) fn query_page_tx(
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
                pending: message_queue::query(db, thread)?,
                read_revision: message_queue::read_revision(),
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
    async fn detail_current_execution_and_availability_follow_saved_source_identity() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        owner.db.call(|db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');")?;
            for group in ["g","other"] {
                let attempt=format!("attempt-{group}");
                let node=format!("call-{group}");
                db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,'u',0,0,'p','m',0,'completed','{}','now')",[&attempt])?;
                db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,'u','completed','now')",rusqlite::params![group,attempt])?;
                db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','assistant_response','now',?2)",rusqlite::params![node,json!({"group_id":group,"text":"[source](https://example.test/source)"}).to_string()])?;
                db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,'same',0,'shell','{}','completed')",[group])?;
            }
            db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('other',?1,1,'shell','{}','completed')",["x".repeat(200_000)])?;
            db.execute_batch("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('exec','r','u','exec','running','/','{}','UTF-8','UTF-8','closed','now'); UPDATE tool_call SET operation_id='exec' WHERE group_id='g'; INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('exec',0,'stdout',0,X'616263','now');")?;
            Ok(())
        }).await.unwrap();
        let (updates, _) = std::sync::mpsc::sync_channel(8);
        let reader = HistoryReader::new(&control, &updates, 80);
        let first = reader
            .links(vec!["call-g".into(), "call-other".into()])
            .await
            .unwrap();
        let get = |source: &DetailSourceFacts, key: &str| {
            source
                .fields
                .iter()
                .find(|f| f.key == key)
                .map(|f| f.value.clone())
        };
        assert_eq!(
            first.sources[0].kind,
            crate::presentation::PresentationKind::Assistant
        );
        let bounded_id = first.sources[1]
            .fields
            .iter()
            .find(|field| field.key == "/current_tools/1/call_id")
            .unwrap();
        assert!(bounded_id.value_omitted && bounded_id.value.is_empty());
        assert!(bounded_id.display.as_deref().unwrap().contains("上限"));
        assert_eq!(
            get(&first.sources[0], "/current_tools/0/state").as_deref(),
            Some("running")
        );
        assert_eq!(
            get(&first.sources[0], "/current_tools/0/output_available").as_deref(),
            Some("true")
        );
        assert_eq!(
            get(&first.sources[1], "/current_tools/0/execution_id").as_deref(),
            Some("未取得")
        );
        owner.db.call(|db| {
            db.execute_batch("UPDATE shell_execution SET state='exited',exit_code=7,output_complete=1,retention_generation=1 WHERE id='exec'; DELETE FROM output_chunk WHERE execution_id='exec'; UPDATE node SET deleted_reason='gc',payload_json=NULL WHERE id='call-other';")?; Ok(())
        }).await.unwrap();
        let second = reader
            .links(vec!["call-g".into(), "call-other".into()])
            .await
            .unwrap();
        assert!(second.read_revision > first.read_revision);
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/state").as_deref(),
            Some("exited")
        );
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/exit_code").as_deref(),
            Some("7")
        );
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/output_available").as_deref(),
            Some("false")
        );
        assert_eq!(
            get(&second.sources[0], "/current_tools/0/retention_generation").as_deref(),
            Some("1")
        );
        assert!(second.sources[1].unavailable_reason.is_some());
        assert!(second.sources[1].links.is_empty());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

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
        let snapshot = reader
            .links(vec!["call".into(), "result".into()])
            .await
            .unwrap();
        assert!(snapshot.observed_at.ends_with(" UTC"));
        assert_eq!(snapshot.sources.len(), 2);
        let links = snapshot
            .sources
            .iter()
            .flat_map(|source| &source.links)
            .collect::<Vec<_>>();
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
            let snapshot = reader.links(vec!["call".into(), id.into()]).await.unwrap();
            assert_eq!(snapshot.sources[0].node_id, "call");
            assert!(snapshot.sources[0].unavailable_reason.is_none());
            let unavailable = &snapshot.sources[1];
            assert_eq!(unavailable.node_id, id);
            assert!(unavailable.unavailable_reason.is_some());
            assert!(unavailable.fields.is_empty() && unavailable.links.is_empty());
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

    #[tokio::test]
    async fn shared_page_query_pairs_pending_and_keeps_bounded_borrowed_source_identities() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let (raw, cursor) = owner.db.call(|db| {
            let tx = db.transaction()?;
            for ordinal in 0..51 {
                crate::history::append_node(
                    &tx, "thread", &format!("source-{ordinal}"), None, "user",
                    &json!({"role":"user","content":[{"type":"input_text","text":format!("body-{ordinal}")}]}), "now",
                )?;
            }
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('pending','user','now',?1)", [json!({"role":"user","content":[{"type":"input_text","text":"waiting"}]}).to_string()])?;
            tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,updated_at,path_admission)VALUES('receipt','operation','null','thread','pending','held','now','now','unadmitted')", [])?;
            let page = query_page_tx(&tx, "thread", None, None)?;
            tx.commit()?;
            Ok(page)
        }).await.unwrap();
        assert!(cursor.unwrap().is_some());
        assert_eq!(raw.info.first, 1);
        assert_eq!(raw.info.total, 51);
        assert_eq!(raw.info.pending.total, 1);
        assert_eq!(raw.info.pending.items[0].message_id, "pending");
        assert!(raw.info.read_revision > 0);
        let ids = raw.source_ids().collect::<Vec<_>>();
        assert_eq!(ids.len(), 50);
        assert_eq!(ids.first().copied(), Some("source-1"));
        assert_eq!(ids.last().copied(), Some("source-50"));
        assert!(std::ptr::eq(ids[0].as_ptr(), raw.rows[0].node.id.as_ptr()));
        let revision = raw.info.read_revision;
        let (nodes, info, anchor) = presentation_work(move || {
            raw.project_page(
                80,
                Some(ViewAnchor {
                    node_id: Some("source-25".into()),
                    char_offset: 2,
                }),
            )
        })
        .await
        .unwrap();
        assert_eq!(nodes.len(), 50);
        assert_eq!(anchor.unwrap().node_id.as_deref(), Some("source-25"));
        assert_eq!(info.read_revision, revision);
        assert_eq!(info.pending.items[0].message_id, "pending");
        assert!(
            nodes
                .iter()
                .all(|node| node.presentation.read_revision == revision)
        );
        control.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
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
