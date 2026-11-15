//! Fixed successful-summary adoption, retaining the caller's one transaction.
use super::discard::{RejectionReason, rejected};
use super::*;
pub(super) struct SummaryAdoption {
    pub(super) automatic: Option<AutomaticStage>,
    pub(super) removed_characters: u64,
    pub(super) drop_all_reasoning: bool,
    pub(super) snapshot: Snapshot,
    pub(super) sources: Vec<Source>,
    pub(super) payload: Value,
    pub(super) start: usize,
    pub(super) count: usize,
    pub(super) result: CompactResult,
}
impl SummaryAdoption {
    pub(super) fn adopt_tx(
        mut self,
        tx: &rusqlite::Transaction<'_>,
        config: &Config,
        cancel: &CancellationToken,
    ) -> Result<CompactResult, StorageError> {
        if cancel.is_cancelled() {
            return Err(StorageError::from(rejected(
                RejectionReason::Cancelled,
                "Compaction cancelled before adoption",
            )));
        }
        if cancel.is_cancelled() {
            return Err(StorageError::from(rejected(
                RejectionReason::Cancelled,
                "Compaction cancelled",
            )));
        }
        // Only captured, non-expanded selected paths may adopt a newer append tail.
        // All other snapshot kinds retain strict revision validation.
        let mut appended = Vec::new();
        if let Some((generation, selected)) = &self.snapshot.append_witness {
            let (current_generation, revision, cursor): (i64, i64, Option<String>) = tx.query_row(
                "SELECT selection_nonappend_generation,selection_revision,cursor_node_id FROM thread WHERE id=?1 AND deleted_at IS NULL",
                [&self.snapshot.thread], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            let current = tx
                .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
                .query_map([&self.snapshot.thread], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let reason = if current_generation != *generation {
                Some(RejectionReason::NonappendGenerationChanged)
            } else if !current.starts_with(selected) {
                Some(RejectionReason::SelectedPrefixChanged)
            } else if current.last() != cursor.as_ref() {
                Some(RejectionReason::CurrentTailChanged)
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(rejected(
                    reason,
                    "Compaction selected path changed; late result discarded",
                )
                .into());
            }
            let original_path = self
                .snapshot
                .groups
                .iter()
                .flat_map(|g| g.sources.iter())
                .map(|s| s.id.clone())
                .collect::<Vec<_>>();
            if original_path == *selected && revision >= self.snapshot.revision as i64 {
                appended.extend_from_slice(&current[selected.len()..]);
                self.snapshot.revision = revision as u64;
            }
        }
        validate(tx, &self.snapshot)?;
        let node = store_replacement(
            tx,
            config,
            &self.snapshot,
            &self.sources,
            &self.payload,
            FORMAT,
        )?;
        let snapshot_path = self
            .snapshot
            .groups
            .iter()
            .flat_map(|group| group.sources.iter().map(|source| source.id.clone()))
            .collect::<Vec<_>>();
        let start_node = self.snapshot.groups[..self.start]
            .iter()
            .try_fold(0usize, |sum, group| sum.checked_add(group.sources.len()))
            .ok_or_else(|| {
                StorageError::InvalidInput("manual selected path offset overflow".into())
            })?;
        let source_count = self.snapshot.groups[self.start..self.count]
            .iter()
            .try_fold(0usize, |sum, group| sum.checked_add(group.sources.len()))
            .ok_or_else(|| {
                StorageError::InvalidInput("manual selected path length overflow".into())
            })?;
        let path = super::replace_selected_path(
            &snapshot_path,
            start_node,
            source_count,
            &node,
            &appended,
        )?;
        if cancel.is_cancelled() {
            return Err(StorageError::from(rejected(
                RejectionReason::Cancelled,
                "Compaction cancelled",
            )));
        }
        history::select_path(
            tx,
            &self.snapshot.thread,
            self.snapshot.revision,
            &path,
            &timestamp(),
        )?;
        if self.drop_all_reasoning {
            tx.execute(
                "UPDATE thread SET reasoning_epoch=?2 WHERE id=?1",
                params![self.snapshot.thread, self.snapshot.epoch + 1],
            )?;
            tx.execute(
                "DELETE FROM app_meta WHERE key=?1",
                [history::reasoning_keep_key(&self.snapshot.thread)],
            )?;
        }
        if self.automatic.is_none() {
            let mut state = super::a_state::scope_tx(
                tx,
                &self.snapshot.thread,
                &self.snapshot.profile,
                &self.snapshot.model,
            )?;
            let expected = state.token();
            state.finish_boundary(expected, &node)?;
            super::a_state::save(tx, &self.snapshot.thread, &state)?;
        }
        // A partial active summary drops all earlier reasoning regardless of suffix.
        self.result.summary_node_id = Some(node);
        if let Some(stage) = &self.automatic {
            super::attempt::record_tx(
                tx,
                &self.snapshot.thread,
                stage,
                &self.result,
                &super::attempt::StageMeasure {
                    removed_characters: Some(self.removed_characters),
                },
            )?;
        }
        Ok(self.result)
    }
}

#[cfg(test)]
mod append_adoption_tests {
    use super::*;
    async fn adoption_case(mode: &'static str) {
        let (_root, db) = super::super::tests::database().await;
        db.call(|db| {
            let tx = db.transaction()?;
            history::append_node(
                &tx,
                "t",
                "source",
                None,
                "user",
                &json!({"role":"user","content":"source"}),
                "now",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        let mut snapshot = Compactor::new(db.clone(), Arc::new(Config::default()))
            .snapshot("t", true)
            .await
            .unwrap();
        if mode == "prefix" {
            // Inject a mismatched captured prefix without bypassing monotonic
            // generation triggers. Real path edits reject on generation first.
            snapshot
                .append_witness
                .as_mut()
                .unwrap()
                .1
                .push("missing".into());
        }
        db.call(move |db| { let tx=db.transaction()?;
            match mode {
                "batch" => {
                    for id in ["tail-a","tail-b"] { tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'user','now','{}')",[id])?; }
                    history::append_existing_nodes(&tx,"t",snapshot_revision(&tx)?,&["tail-a".into(),"tail-b".into()],"now")?;
                }
                "multi" => {
                    for id in ["tail-a","tail-b"] { history::append_node(&tx,"t",id,None,"user",&json!({"role":"user","content":id}),"now")?; }
                }
                "prefix" => {}
                "cursor" => { tx.execute("UPDATE thread SET cursor_node_id=NULL WHERE id='t'",[])?; }
                "hash" => { tx.execute("UPDATE node SET payload_revision=payload_revision+1 WHERE id='source'",[])?; }
                "profile" => { tx.execute("UPDATE thread SET profile_revision=profile_revision+1 WHERE id='t'",[])?; }
                "epoch" => { tx.execute("UPDATE thread SET reasoning_epoch=reasoning_epoch+1 WHERE id='t'",[])?; }
                "metadata" => { tx.execute("UPDATE thread SET selection_revision=selection_revision+1 WHERE id='t'",[])?; }
                "aba" => {
                    history::select_path(&tx,"t",snapshot_revision(&tx)?,&[],"now")?;
                    history::select_path(&tx,"t",snapshot_revision(&tx)?,&["source".into()],"now")?;
                    history::append_node(&tx,"t","tail",None,"user",&json!({"role":"user","content":"tail"}),"now")?;
                }
                "range" => { history::select_path(&tx,"t",snapshot_revision(&tx)?,&[],"now")?; }
                _ => { history::append_node(&tx,"t","tail",None,"user",&json!({"role":"user","content":"tail"}),"now")?; }
            }
            tx.commit()?; Ok(()) }).await.unwrap();
        let accepted = db
            .call(move |db| {
                let tx = db.transaction()?;
                let cancel = CancellationToken::new();
                if mode == "cancel" {
                    cancel.cancel();
                }
                let adoption = SummaryAdoption {
                    automatic: None,
                    removed_characters: 0,
                    drop_all_reasoning: false,
                    sources: snapshot.groups[0].sources.clone(),
                    snapshot,
                    payload: json!({"text":"summary"}),
                    start: 0,
                    count: 1,
                    result: CompactResult::default(),
                };
                let result = adoption.adopt_tx(&tx, &Config::default(), &cancel)?;
                let path = tx
                    .prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal",
                    )?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                assert_eq!(path[0], result.summary_node_id.unwrap());
                if matches!(mode, "batch" | "multi") {
                    assert_eq!(&path[1..], &["tail-a", "tail-b"]);
                } else {
                    assert_eq!(&path[1..], &["tail"]);
                }
                tx.commit()?;
                Ok(())
            })
            .await;
        assert_eq!(
            accepted.is_ok(),
            matches!(mode, "append" | "batch" | "multi"),
            "{mode}: {accepted:?}"
        );
        if let Err(error) = accepted {
            let reason = match mode {
                "prefix" => RejectionReason::SelectedPrefixChanged,
                "cursor" => RejectionReason::CurrentTailChanged,
                "hash" => RejectionReason::SourceChanged,
                "profile" => RejectionReason::ProfileChanged,
                "epoch" => RejectionReason::EpochChanged,
                "metadata" | "aba" | "range" => RejectionReason::NonappendGenerationChanged,
                "cancel" => RejectionReason::Cancelled,
                _ => panic!("unexpected rejection {mode}: {error:?}"),
            };
            let error = error.into_api_error();
            assert_eq!(
                discard::cause(&error),
                discard::Outcome::Rejected { reason }
            );
            assert_eq!(error.code, ErrorCode::InvalidState);
            let replacements = db
                .call(|db| {
                    Ok(db.query_row("SELECT count(*) FROM replacement", [], |r| {
                        r.get::<_, i64>(0)
                    })?)
                })
                .await
                .unwrap();
            assert_eq!(replacements, 0);
        }
        db.shutdown().await.unwrap();
    }
    fn snapshot_revision(tx: &rusqlite::Transaction<'_>) -> Result<u64, StorageError> {
        Ok(tx.query_row(
            "SELECT selection_revision FROM thread WHERE id='t'",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }
    #[tokio::test]
    async fn prefix_change_has_exact_guard() {
        adoption_case("prefix").await;
    }
    #[tokio::test]
    async fn cursor_change_has_exact_guard() {
        adoption_case("cursor").await;
    }
    #[tokio::test]
    async fn multiple_appends_preserve_tail() {
        adoption_case("multi").await;
    }
    #[tokio::test]
    async fn source_hash_edit_rejects() {
        adoption_case("hash").await;
    }
    #[tokio::test]
    async fn profile_edit_rejects() {
        adoption_case("profile").await;
    }
    #[tokio::test]
    async fn reasoning_epoch_edit_rejects() {
        adoption_case("epoch").await;
    }
    #[tokio::test]
    async fn same_cursor_metadata_edit_rejects() {
        adoption_case("metadata").await;
    }
    #[tokio::test]
    async fn append_tail_survives_summary_adoption() {
        adoption_case("append").await;
    }
    #[tokio::test]
    async fn batch_append_tail_survives_summary_adoption() {
        adoption_case("batch").await;
    }
    #[tokio::test]
    async fn changed_range_rejects_summary_adoption() {
        adoption_case("range").await;
    }
    #[tokio::test]
    async fn rewind_restore_aba_rejects_summary_adoption() {
        adoption_case("aba").await;
    }
    #[tokio::test]
    async fn cancelled_append_rejects_summary_adoption() {
        adoption_case("cancel").await;
    }
}

/// Native GC adoption over only the selected captured R. The caller runs this
/// inside the coordinator's strict original-token FINISH transaction.
pub(super) fn adopt_gc_result_tx(
    tx: &rusqlite::Transaction<'_>,
    config: &Config,
    captured: &super::gc_capture::CapturedSummaryInput,
    generated: &single_request::Dispatched,
    usage: &SummaryUsage,
) -> Result<String, StorageError> {
    let selected = generated.selected.clone();
    if selected.is_empty()
        || selected.end > captured.groups.len()
        || generated.response.text.trim().is_empty()
    {
        return Err(StorageError::InvalidInput(
            "GC summary has empty or invalid selected R".into(),
        ));
    }
    let stamps = captured.groups[selected.clone()]
        .iter()
        .flat_map(|group| group.stamps.iter())
        .collect::<Vec<_>>();
    let originals = stamps
        .iter()
        .map(|stamp| stamp.id.clone())
        .collect::<Vec<_>>();
    let source_characters = captured.groups[selected.clone()]
        .iter()
        .try_fold(0u64, |sum, group| {
            sum.checked_add(u64::try_from(group.projection_weight).ok()?)
        })
        .ok_or_else(|| StorageError::InvalidInput("GC source projection overflow".into()))?;
    let text = if captured.active_turn {
        let originals = originals.clone();
        super::single_request::active_turn_summary_text(
            &originals,
            captured.active_user_messages.len(),
            false,
            &generated.response.text,
            None,
        )
    } else {
        generated.response.text.clone()
    };
    let mut payload = json!({"item":{"role":"developer","content":[{"type":"input_text","text":text}]},"source_node_ids":originals,"summary_format":FORMAT,"summary_usage":usage});
    if captured.active_turn {
        payload["summary_scope"] = json!("active_turn_middle");
    }
    let projected = edit::projection_characters(&payload, captured.epoch);
    if projected >= source_characters {
        return Err(StorageError::InvalidInput(
            "GC summary does not shrink selected source projection".into(),
        ));
    }
    let thread = &captured.thread;
    let (selection,profile_revision,generation,epoch,profile,model,effort):(i64,i64,i64,i64,String,String,Option<String>)=tx.query_row("SELECT selection_revision,profile_revision,selection_nonappend_generation,reasoning_epoch,profile_id,model_id,effort FROM thread WHERE id=?1 AND deleted_at IS NULL",[thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    if selection < captured.selection_revision
        || profile_revision != captured.profile_revision
        || generation != captured.nonappend_generation
        || epoch != captured.epoch
        || profile != captured.profile
        || model != captured.model
        || effort != captured.effort
    {
        return Err(StorageError::InvalidInput(
            "GC captured source context changed before FINISH".into(),
        ));
    }
    let path = tx
        .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
        .query_map([thread], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let start = usize::try_from(
        stamps
            .first()
            .ok_or_else(|| StorageError::InvalidInput("GC selected R is empty".into()))?
            .ordinal,
    )
    .map_err(|_| StorageError::InvalidInput("negative GC source ordinal".into()))?;
    for (offset, stamp) in stamps.iter().enumerate() {
        let index = start
            .checked_add(offset)
            .ok_or_else(|| StorageError::InvalidInput("GC source ordinal overflow".into()))?;
        if usize::try_from(stamp.ordinal).ok() != Some(index)
            || path.get(index).map(String::as_str) != Some(stamp.id.as_str())
        {
            return Err(StorageError::InvalidInput(
                "GC selected R path changed".into(),
            ));
        }
        let (revision,turn,protected):(i64,Option<String>,bool)=tx.query_row("SELECT n.payload_revision,n.turn_id,EXISTS(SELECT 1 FROM receipt q WHERE q.target_thread_id=?2 AND q.message_node_id=n.id AND q.state IN ('accepted','held')) FROM node n WHERE n.id=?1",params![stamp.id,thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if revision != stamp.revision || turn != stamp.turn || protected != stamp.protected {
            return Err(StorageError::InvalidInput(
                "GC source revision, turn, or protection changed".into(),
            ));
        }
        if let Some(group) = &stamp.group_id {
            let complete:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_group g WHERE g.id=?1 AND g.state='completed' AND NOT EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state!='completed' OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))))",[group],|r|r.get(0))?;
            if !complete {
                return Err(StorageError::InvalidInput(
                    "GC tool group changed before FINISH".into(),
                ));
            }
        }
    }
    let adapter = if stamps.iter().any(|stamp| stamp.marker_adapter) {
        MARKER_ADAPTER
    } else {
        ADAPTER
    };
    let input_key = key(&(
        FORMAT,
        PROMPT,
        adapter,
        &captured.profile,
        &captured.model,
        &captured.effort,
        captured.epoch,
        stamps
            .iter()
            .map(|stamp| (&stamp.id, stamp.revision, &stamp.hash, &stamp.attachments))
            .collect::<Vec<_>>(),
    ));
    let selected_sources = stamps
        .iter()
        .map(|stamp| super::ReplacementSource {
            id: &stamp.id,
            revision: stamp.revision,
            hash: &stamp.hash,
        })
        .collect::<Vec<_>>();
    let node = super::store_replacement_material_tx(
        tx,
        config,
        super::ReplacementMaterial {
            thread,
            input_key: &input_key,
            format: FORMAT,
            sources: &selected_sources,
            payload: &payload,
        },
    )?;
    let new_path = super::replace_selected_path(&path, start, stamps.len(), &node, &[])?;
    history::select_path(tx, thread, selection as u64, &new_path, &timestamp())?;
    Ok(node)
}
