use super::*;
/// Each projected item contributes its compact JSON characters plus one comma.
/// Internal event IDs, provenance and expired-epoch reasoning follow replay.
pub(in crate::compaction) fn projection_characters(payload: &Value, epoch: i64) -> u64 {
    source_projection_characters(payload, epoch, false)
}
pub(in crate::compaction) fn source_projection_characters(
    payload: &Value,
    epoch: i64,
    imported: bool,
) -> u64 {
    crate::control::replay::convert_source(payload.clone(), epoch, false, imported)
        .into_iter()
        .fold(0u64, |sum, item| {
            sum.saturating_add(item.to_string().chars().count() as u64)
                .saturating_add(1)
        })
}
pub(in crate::compaction) fn history_characters(snapshot: &Snapshot) -> u64 {
    let items = snapshot
        .groups
        .iter()
        .flat_map(|group| &group.sources)
        .map(|source| {
            source_projection_characters(&source.payload, snapshot.epoch, source.imported)
        })
        .fold(0u64, u64::saturating_add);
    if items == 0 {
        2
    } else {
        items.saturating_add(1)
    }
}
/// Exact changed replay suffix size; not a prompt-cache or token estimate.
pub(in crate::compaction) fn suffix_characters(snapshot: &Snapshot, start: usize) -> u64 {
    snapshot
        .groups
        .iter()
        .skip(start)
        .flat_map(|group| &group.sources)
        .map(|source| {
            source_projection_characters(&source.payload, snapshot.epoch, source.imported)
        })
        .fold(0u64, u64::saturating_add)
}
