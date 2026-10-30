//! Bounded continuation of receipt projections, owned by the frontend loop.
use super::*;

/// A terminal Turn is not evidence that its original input was cancelled or
/// answered. Shared recovery owns that distinction in the saved receipt.
pub(super) fn project_state(receipt: &str, turn: Option<&str>) -> Option<ReceiptStatus> {
    match (receipt, turn) {
        ("cancelled", _) => Some(ReceiptStatus::Cancelled),
        ("applied", Some("completed")) => Some(ReceiptStatus::Completed),
        ("applied", _) => Some(ReceiptStatus::Applied),
        ("held", _) => Some(ReceiptStatus::Held),
        _ => None,
    }
}
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
    let any_checked = pending
        .values()
        .any(|receipt| receipt.thread == thread && receipt.checked);
    let any_unchecked = pending
        .values()
        .any(|receipt| receipt.thread == thread && !receipt.checked);
    if !any_checked || !any_unchecked {
        // A fresh query observes all changes so far. Only a mixed partial pass
        // must retain a request to revisit already checked IDs.
        for receipt in pending
            .values_mut()
            .filter(|receipt| receipt.thread == thread)
        {
            receipt.checked = false;
            receipt.repeat = false;
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
    let repeat = pending.values().any(|r| r.thread == thread && r.repeat);
    for receipt in pending.values_mut().filter(|r| r.thread == thread) {
        receipt.checked = false;
        receipt.repeat = false;
    }
    repeat
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
            let Some(receipt) = pending.get(&message) else {
                continue;
            };
            let draft = receipt.draft.clone();
            let revision = receipt.revision;
            let thread = receipt.thread.clone();
            // Held is an observed durable outcome, not the end of tracking.
            // A later explicit resume may admit this same original receipt.
            if status != ReceiptStatus::Held {
                pending.remove(&message);
            }
            let hint = matches!(status, ReceiptStatus::Cancelled | ReceiptStatus::Failed)
                .then(|| format!("依頼を保持しました · /restore {} で入力欄へ戻す", revision));
            // Main is the sole producer allowed into the last 32 slots. Its
            // synchronous capacity check + insertion cannot race another main
            // handler, and workers stop at 480. No await and no raw bypass.
            let rejected = updates.defer(UiUpdate::Receipt {
                draft_id: draft,
                revision,
                thread_id: Some(thread),
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
    fn saved_delivery_fact_precedes_terminal_turn_labels() {
        for turn in [
            None,
            Some("preparing"),
            Some("running"),
            Some("stopped"),
            Some("failed"),
        ] {
            assert_eq!(project_state("applied", turn), Some(ReceiptStatus::Applied));
            assert_eq!(project_state("held", turn), Some(ReceiptStatus::Held));
            assert_eq!(
                project_state("cancelled", turn),
                Some(ReceiptStatus::Cancelled)
            );
            assert_eq!(project_state("accepted", turn), None);
        }
        assert_eq!(
            project_state("applied", Some("completed")),
            Some(ReceiptStatus::Completed)
        );
    }
    #[test]
    fn held_is_tracked_until_its_saved_admission_without_restore_hint() {
        let (send, receive) = std::sync::mpsc::sync_channel(8);
        let updates = BufferedUpdates::new(send);
        let mut pending = HashMap::from([(
            "message".into(),
            PendingReceipt {
                thread: "thread".into(),
                draft: "draft".into(),
                revision: 1,
                checked: true,
                repeat: false,
            },
        )]);
        Batch::new(vec![("message".into(), ReceiptStatus::Held)]).drain(&mut pending, &updates);
        assert!(pending.contains_key("message"));
        assert!(matches!(
            receive.try_recv().unwrap(),
            UiUpdate::Receipt {
                status: ReceiptStatus::Held,
                message: None,
                ..
            }
        ));
        Batch::new(vec![("message".into(), ReceiptStatus::Applied)]).drain(&mut pending, &updates);
        assert!(!pending.contains_key("message"));
        assert!(matches!(
            receive.try_recv().unwrap(),
            UiUpdate::Receipt {
                status: ReceiptStatus::Applied,
                message: None,
                ..
            }
        ));
    }
    #[test]
    fn change_before_a_fresh_scan_is_observed_without_an_extra_pass() {
        for count in [1, 128, 200] {
            for previously_checked in [false, true] {
                let mut pending = (0..count)
                    .map(|n| {
                        (
                            n.to_string(),
                            PendingReceipt {
                                thread: "thread".into(),
                                draft: "draft".into(),
                                revision: n,
                                checked: previously_checked,
                                repeat: false,
                            },
                        )
                    })
                    .collect::<HashMap<_, _>>();
                changed(&mut pending, "thread");
                let mut seen = HashSet::new();
                let mut batches = 0;
                loop {
                    let ids = next_ids(&mut pending, "thread");
                    assert!(!ids.is_empty());
                    for id in ids {
                        assert!(
                            seen.insert(id),
                            "a pre-scan change must not request a duplicate pass"
                        );
                    }
                    batches += 1;
                    if !continue_scan(&mut pending, "thread") {
                        break;
                    }
                    assert!(batches < 3, "finite two-page fixture unexpectedly repeated");
                }
                assert_eq!(seen.len(), count as usize);
                assert_eq!(batches, (count as usize).div_ceil(BATCH_LIMIT));
                assert!(pending.values().all(|receipt| !receipt.repeat));
            }
        }
    }
    #[test]
    fn send_after_completed_scan_does_not_create_a_false_partial_pass() {
        let mut pending = HashMap::from([(
            "old".into(),
            PendingReceipt {
                thread: "thread".into(),
                draft: "old".into(),
                revision: 1,
                checked: false,
                repeat: false,
            },
        )]);
        assert_eq!(next_ids(&mut pending, "thread"), vec!["old"]);
        assert!(!continue_scan(&mut pending, "thread"));
        pending.insert(
            "new".into(),
            PendingReceipt {
                thread: "thread".into(),
                draft: "new".into(),
                revision: 2,
                checked: false,
                repeat: false,
            },
        );
        changed(&mut pending, "thread");
        let ids = next_ids(&mut pending, "thread");
        assert_eq!(ids.len(), 2);
        assert!(
            !continue_scan(&mut pending, "thread"),
            "a completed pass plus later send must require only one query"
        );
        assert!(pending.values().all(|r| !r.checked && !r.repeat));
    }
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
