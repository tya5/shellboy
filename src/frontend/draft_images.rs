//! Managed-image leases owned by live drafts and accepted send preparations.
use super::{ManagedImage, UiAttachment};
use std::collections::{HashMap, HashSet};
struct Image {
    lease: ManagedImage,
    revision: u64,
    adopted: bool,
}
#[derive(Default)]
pub(super) struct DraftImages(HashMap<(String, String), Image>);
impl DraftImages {
    pub(super) fn leases(&self, draft: &str, attachments: &[UiAttachment]) -> Vec<ManagedImage> {
        attachments
            .iter()
            .filter_map(|attachment| {
                self.0
                    .get(&(draft.to_owned(), attachment.id.clone()))
                    .map(|image| image.lease.clone())
            })
            .collect()
    }
    pub(super) fn insert(&mut self, draft: String, revision: u64, lease: ManagedImage) {
        self.0.insert(
            (draft, lease.id.clone()),
            Image {
                lease,
                revision,
                adopted: false,
            },
        );
    }
    pub(super) fn sent(&mut self, draft: &str, attachments: &HashSet<String>) {
        self.0
            .retain(|(owner, id), _| owner != draft || !attachments.contains(id));
    }
    pub(super) fn decision(&mut self, draft: String, attachment: String, accepted: bool) {
        let key = (draft, attachment);
        if accepted {
            if let Some(image) = self.0.get_mut(&key) {
                image.adopted = true;
            }
        } else {
            self.0.remove(&key);
        }
    }
    pub(super) fn release(&mut self, draft: &str, revision: u64, selected: &HashSet<String>) {
        self.0.retain(|(owner, id), image| {
            owner != draft || !image.adopted || revision <= image.revision || selected.contains(id)
        });
    }
}
