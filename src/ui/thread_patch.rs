//! Bounded frontend browser observations, not a public CLI result.
use super::ThreadDisplayTime;
use crate::protocol::ThreadSummary;

#[derive(Clone, Debug)]
pub struct BrowserPatch {
    pub generation: u64,
    pub read_sequence: u64,
    pub rows: Vec<ThreadSummary>,
    pub watch_summaries: Vec<super::watch::WatchSummary>,
    pub display_times: Vec<ThreadDisplayTime>,
    pub missing_ids: Vec<String>,
    pub limited_titles: Vec<String>,
    pub excerpt: Option<(String, Result<String, String>)>,
}
