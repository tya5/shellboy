//! Fixed successful-summary adoption, retaining the caller's one transaction.
use super::discard::{DiscardReason, rejected};
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
                DiscardReason::Cancelled,
                "Compaction cancelled before adoption",
            )));
        }
        if cancel.is_cancelled() {
            return Err(StorageError::from(rejected(
                DiscardReason::Cancelled,
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
                Some(DiscardReason::NonappendGenerationChanged)
            } else if !current.starts_with(selected) {
                Some(DiscardReason::SelectedPrefixChanged)
            } else if current.last() != cursor.as_ref() {
                Some(DiscardReason::CurrentTailChanged)
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
        let mut path = self.snapshot.groups[..self.start]
            .iter()
            .flat_map(|group| group.sources.iter().map(|source| source.id.clone()))
            .collect::<Vec<_>>();
        path.push(node.clone());
        path.extend(
            self.snapshot.groups[self.count..]
                .iter()
                .flat_map(|g| g.sources.iter().map(|s| s.id.clone())),
        );
        path.extend(appended);
        if cancel.is_cancelled() {
            return Err(StorageError::from(rejected(
                DiscardReason::Cancelled,
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
        let snapshot = Compactor::new(db.clone(), Arc::new(Config::default()))
            .snapshot("t", true)
            .await
            .unwrap();
        db.call(move |db| { let tx=db.transaction()?;
            match mode {
                "batch" => {
                    for id in ["tail-a","tail-b"] { tx.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,'user','now','{}')",[id])?; }
                    history::append_existing_nodes(&tx,"t",snapshot_revision(&tx)?,&["tail-a".into(),"tail-b".into()],"now")?;
                }
                "multi" => {
                    for id in ["tail-a","tail-b"] { history::append_node(&tx,"t",id,None,"user",&json!({"role":"user","content":id}),"now")?; }
                }
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
        if accepted.is_err() {
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
