//! Observe eventual history results without assuming one bounded pass finishes GC.
use shellboy::{
    gc::{CollectionReport, GcService},
    protocol::{ApiError, StorageCategory},
};

pub async fn collect_history(
    gc: &GcService,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<CollectionReport, ApiError> {
    let mut total = CollectionReport::default();
    for _ in 0..4096 {
        let pass = gc.collect(StorageCategory::History, at).await?;
        total.nodes_tombstoned += pass.nodes_tombstoned;
        total.history_bytes_collected += pass.history_bytes_collected;
        total.summary_cache_parts_collected += pass.summary_cache_parts_collected;
        total.summary_cache_bytes_collected += pass.summary_cache_bytes_collected;
        if !pass.deferred {
            return Ok(total);
        }
    }
    panic!("history collection did not finish within the fixture's bounded pass allowance");
}
