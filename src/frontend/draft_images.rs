//! Managed-image leases owned by live drafts and accepted send preparations.
use super::{ManagedImage, UiAttachment};
use std::collections::{HashMap, HashSet};
struct Image {
    lease: ManagedImage,
    revision: u64,
    adopted: bool,
}
#[derive(Default)]
pub(super) struct DraftImages {
    images: HashMap<(String, String), Image>,
    owners: HashMap<String, String>,
    pending: HashMap<u64, (String, Option<String>)>,
    next_ticket: u64,
}
impl DraftImages {
    pub(super) fn associate(&mut self, draft: &str, thread: &str) {
        if !self.images.keys().any(|(owner, _)| owner == draft)
            && !self.pending.values().any(|(owner, _)| owner == draft)
        {
            return;
        }
        self.owners.insert(draft.to_owned(), thread.to_owned());
        for (owner, target) in self.pending.values_mut() {
            if owner == draft {
                *target = Some(thread.to_owned());
            }
        }
    }
    pub(super) fn begin_image(&mut self, draft: &str, thread: Option<&str>) -> u64 {
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.pending.insert(
            self.next_ticket,
            (draft.to_owned(), thread.map(str::to_owned)),
        );
        if let Some(thread) = thread {
            self.associate(draft, thread);
        }
        self.next_ticket
    }
    pub(super) fn accept_image(&mut self, ticket: u64) -> bool {
        self.pending.remove(&ticket).is_some()
    }
    pub(super) fn threads(&self) -> impl Iterator<Item = &String> {
        self.owners.values()
    }
    pub(super) fn forget_threads(&mut self, deleted: &HashSet<String>, drafts: &[String]) {
        let mut drafts: HashSet<_> = drafts.iter().cloned().collect();
        drafts.extend(
            self.owners
                .iter()
                .filter(|(_, thread)| deleted.contains(*thread))
                .map(|(draft, _)| draft.clone()),
        );
        drafts.extend(
            self.pending
                .values()
                .filter(|(_, thread)| {
                    thread
                        .as_ref()
                        .is_some_and(|thread| deleted.contains(thread))
                })
                .map(|(draft, _)| draft.clone()),
        );
        self.images.retain(|(draft, _), _| !drafts.contains(draft));
        self.owners.retain(|draft, _| !drafts.contains(draft));
        self.pending.retain(|_, (draft, _)| !drafts.contains(draft));
    }
    pub(super) fn prune_owners(&mut self) {
        self.owners.retain(|draft, _| {
            self.images.keys().any(|(owner, _)| owner == draft)
                || self.pending.values().any(|(owner, _)| owner == draft)
        });
    }
    pub(super) fn leases(&self, draft: &str, attachments: &[UiAttachment]) -> Vec<ManagedImage> {
        attachments
            .iter()
            .filter_map(|attachment| {
                self.images
                    .get(&(draft.to_owned(), attachment.id.clone()))
                    .map(|image| image.lease.clone())
            })
            .collect()
    }
    pub(super) fn insert(&mut self, draft: String, revision: u64, lease: ManagedImage) {
        self.images.insert(
            (draft, lease.id.clone()),
            Image {
                lease,
                revision,
                adopted: false,
            },
        );
    }
    pub(super) fn sent(&mut self, draft: &str, attachments: &HashSet<String>) {
        self.images
            .retain(|(owner, id), _| owner != draft || !attachments.contains(id));
        self.prune_owners();
    }
    pub(super) fn decision(&mut self, draft: String, attachment: String, accepted: bool) {
        let key = (draft, attachment);
        if accepted {
            if let Some(image) = self.images.get_mut(&key) {
                image.adopted = true;
            }
        } else {
            self.images.remove(&key);
        }
        self.prune_owners();
    }
    pub(super) fn release(&mut self, draft: &str, revision: u64, selected: &HashSet<String>) {
        self.images.retain(|(owner, id), image| {
            owner != draft || !image.adopted || revision <= image.revision || selected.contains(id)
        });
        self.prune_owners();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_invalidates_pending_captures_including_a_draft_that_was_sent_later() {
        let mut images = DraftImages::default();
        let old = images.begin_image("draft-old", Some("deleted"));
        let newly_sent = images.begin_image("draft-new", None);
        images.associate("draft-new", "deleted");
        let unrelated = images.begin_image("draft-other", Some("kept"));
        images.forget_threads(&HashSet::from(["deleted".into()]), &[]);
        assert!(!images.accept_image(old));
        assert!(!images.accept_image(newly_sent));
        assert!(images.accept_image(unrelated));
        assert!(images.threads().all(|thread| thread == "kept"));
    }
    #[tokio::test]
    async fn forgetting_a_deleted_thread_releases_its_managed_image_pin() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let image = control
            .attachments
            .save_clipboard_pixels(
                crate::attachments::clipboard::ClipboardPixels {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 255],
                },
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        let id = image.id.clone();
        let mut images = DraftImages::default();
        let ticket = images.begin_image("draft", Some("thread"));
        assert!(images.accept_image(ticket));
        images.insert("draft".into(), 1, image);
        images.forget_threads(&HashSet::from(["thread".into()]), &[]);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let resource = id.clone();
                let pinned: bool = control.store.database.call(move |db| Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='managed_file' AND resource_id=?1)",
                    [resource], |row| row.get(0))?)).await.unwrap();
                if !pinned { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.expect("discarded draft must release its managed image lease");
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[test]
    fn failed_capture_reclaims_ticket_and_owner_even_when_command_error_was_already_reported() {
        let mut images = DraftImages::default();
        let ticket = images.begin_image("draft", Some("thread"));
        assert!(images.accept_image(ticket));
        images.prune_owners();
        assert!(images.pending.is_empty());
        assert_eq!(images.threads().count(), 0);
        assert!(!images.accept_image(ticket));
    }
}
