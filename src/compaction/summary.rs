//! Fixed successful-summary adoption, retaining the caller's one transaction.
use super::*;
pub(super) struct SummaryAdoption {
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
        cancel: &CancellationToken,
    ) -> Result<CompactResult, StorageError> {
        if cancel.is_cancelled() {
            return Err(StorageError::from(invalid(
                "Compaction cancelled before adoption",
            )));
        }
        validate(tx, &self.snapshot)?;
        let node = store_replacement(tx, &self.snapshot, &self.sources, &self.payload, FORMAT)?;
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
        self.result.summary_node_id = Some(node);
        Ok(self.result)
    }
}
