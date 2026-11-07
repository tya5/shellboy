//! Fixed successful-summary adoption, retaining the caller's one transaction.
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
            return Err(StorageError::from(invalid(
                "Compaction cancelled before adoption",
            )));
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
