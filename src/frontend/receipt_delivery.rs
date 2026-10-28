//! Bounded continuation of receipt projections, owned by the frontend loop.
use super::*;

pub(super) const BATCH_LIMIT: usize = 128;
const TURN_LIMIT: usize = 32;
pub(super) struct PendingReceipt {
    pub thread: String,
    pub draft: String,
    pub revision: u64,
    // One bit per existing pending receipt; no per-thread cursor cache.
    pub checked: bool,
    pub repeat: bool,
}
pub(super) fn next_ids(pending: &mut HashMap<String, PendingReceipt>, thread: &str) -> Vec<String> {
    if !pending
        .values()
        .any(|receipt| receipt.thread == thread && !receipt.checked)
    {
        for receipt in pending
            .values_mut()
            .filter(|receipt| receipt.thread == thread)
        {
            receipt.checked = false;
        }
    }
    pending
        .iter_mut()
        .filter(|(_, receipt)| receipt.thread == thread && !receipt.checked)
        .take(BATCH_LIMIT)
        .map(|(id, receipt)| {
            receipt.checked = true;
            id.clone()
        })
        .collect()
}

// Do not restart a partial scan: that would starve IDs past its first page.
pub(super) fn changed(pending: &mut HashMap<String, PendingReceipt>, thread: &str) {
    for receipt in pending.values_mut().filter(|r| r.thread == thread) {
        receipt.repeat = true;
    }
}
pub(super) fn continue_scan(pending: &mut HashMap<String, PendingReceipt>, thread: &str) -> bool {
    if pending.values().any(|r| r.thread == thread && !r.checked) {
        return true;
    }
    if !pending.values().any(|r| r.thread == thread && r.repeat) {
        return false;
    }
    for receipt in pending.values_mut().filter(|r| r.thread == thread) {
        receipt.checked = false;
        receipt.repeat = false;
    }
    true
}

#[derive(Default)]
pub(super) struct Batch {
    receipts: std::vec::IntoIter<(String, ReceiptStatus)>,
}
impl Batch {
    pub(super) fn new(receipts: Vec<(String, ReceiptStatus)>) -> Self {
        // The query receives at most BATCH_LIMIT IDs. Never truncate results.
        assert!(
            receipts.len() <= BATCH_LIMIT,
            "receipt query exceeded its input bound"
        );
        Self {
            receipts: receipts.into_iter(),
        }
    }
    pub(super) fn active(&self) -> bool {
        !self.receipts.as_slice().is_empty()
    }
    pub(super) fn drain(
        &mut self,
        pending: &mut HashMap<String, PendingReceipt>,
        updates: &BufferedUpdates,
    ) {
        if updates.closed() {
            self.receipts = Vec::new().into_iter();
            return;
        }
        for _ in 0..TURN_LIMIT {
            if !updates.main_slot() {
                break;
            }
            let Some((message, status)) = self.receipts.next() else {
                break;
            };
            let Some(receipt) = pending.remove(&message) else {
                continue;
            };
            let hint =
                matches!(status, ReceiptStatus::Cancelled | ReceiptStatus::Failed).then(|| {
                    format!(
                        "依頼を保持しました · /restore {} で入力欄へ戻す",
                        receipt.revision
                    )
                });
            // Main is the sole producer allowed into the last 32 slots. Its
            // synchronous capacity check + insertion cannot race another main
            // handler, and workers stop at 480. No await and no raw bypass.
            let rejected = updates.defer(UiUpdate::Receipt {
                draft_id: receipt.draft,
                revision: receipt.revision,
                thread_id: Some(receipt.thread),
                status,
                message: hint,
            });
            assert!(
                rejected.is_none(),
                "reserved main capacity was consumed concurrently"
            );
        }
    }
    pub(super) async fn finish(
        &mut self,
        pending: &mut HashMap<String, PendingReceipt>,
        updates: &BufferedUpdates,
    ) {
        while self.active() && !updates.closed() {
            self.drain(pending, updates);
            if self.active() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct Startup {
    pub pending: HashMap<String, PendingReceipt>,
    pub batch: Batch,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn change_during_partial_scan_revisits_early_ids_after_fair_completion() {
        let mut pending = (0..200)
            .map(|n| {
                (
                    n.to_string(),
                    PendingReceipt {
                        thread: "thread".into(),
                        draft: "draft".into(),
                        revision: n,
                        checked: false,
                        repeat: false,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let first = next_ids(&mut pending, "thread");
        assert_eq!(first.len(), 128);
        let changed_id = first[0].clone();
        changed(&mut pending, "thread");
        assert!(continue_scan(&mut pending, "thread"));
        let tail = next_ids(&mut pending, "thread");
        assert_eq!(tail.len(), 72);
        assert!(tail.iter().all(|id| !first.contains(id)));
        assert!(continue_scan(&mut pending, "thread"));
        let mut second_pass = next_ids(&mut pending, "thread");
        assert!(continue_scan(&mut pending, "thread"));
        second_pass.extend(next_ids(&mut pending, "thread"));
        assert_eq!(second_pass.len(), 200);
        assert!(second_pass.contains(&changed_id));
        assert!(
            !continue_scan(&mut pending, "thread"),
            "one change requests exactly one further pass"
        );
    }
    #[test]
    fn receipt_queries_bound_the_continuation_without_starving_later_pending_messages() {
        let mut pending = (0..BATCH_LIMIT * 2 + 1)
            .map(|n| {
                (
                    n.to_string(),
                    PendingReceipt {
                        thread: "thread".into(),
                        draft: "draft".into(),
                        revision: n as u64,
                        checked: false,
                        repeat: false,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let mut seen = HashSet::new();
        for expected in [BATCH_LIMIT, BATCH_LIMIT, 1] {
            let ids = next_ids(&mut pending, "thread");
            assert_eq!(ids.len(), expected);
            for id in ids {
                assert!(seen.insert(id));
            }
        }
        assert_eq!(seen.len(), pending.len());
        assert_eq!(next_ids(&mut pending, "thread").len(), BATCH_LIMIT);
        assert!(next_ids(&mut pending, "other").is_empty());
    }
}
