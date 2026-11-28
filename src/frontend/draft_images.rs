//! Local image identities; managed pins remain with the owner.
use super::{ApiError, Command, UiAttachment, native_history};
use crate::protocol::{
    DRAFT_IMAGE_CHUNK_BYTES, DraftImageAction, DraftImageArgs, DraftImageResult, ErrorCode,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::{HashMap, HashSet};
struct Image {
    revision: u64,
    adopted: bool,
}
#[derive(Default)]
pub(super) struct DraftImages {
    images: HashMap<(String, String), Image>,
    pending: HashMap<u64, (String, Option<String>)>,
    next_ticket: u64,
    send_references: HashMap<(String, String), usize>,
    releases: Vec<(String, String)>,
}
impl DraftImages {
    pub(super) fn associate(&mut self, draft: &str, thread: &str) {
        // Once a draft targets a Thread, committed owner writes own disposal.
        self.images.retain(|(owner, _), _| owner != draft);
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
    pub(super) fn accept_image(&mut self, ticket: u64) -> Option<bool> {
        self.pending
            .remove(&ticket)
            .map(|(_, thread)| thread.is_none())
    }
    pub(super) fn forget_threads(&mut self, deleted: &HashSet<String>) {
        let drafts: HashSet<_> = self
            .pending
            .values()
            .filter(|(_, thread)| {
                thread
                    .as_ref()
                    .is_some_and(|thread| deleted.contains(thread))
            })
            .map(|(draft, _)| draft.clone())
            .collect();
        self.images.retain(|key, _| {
            if !drafts.contains(&key.0) {
                return true;
            }
            self.releases.push(key.clone());
            false
        });
        self.pending.retain(|_, (draft, _)| !drafts.contains(draft));
    }
    pub(super) fn begin_send(&mut self, draft: &str, attachments: &[UiAttachment]) {
        for attachment in attachments {
            let key = (draft.to_owned(), attachment.id.clone());
            if self.images.contains_key(&key) {
                *self.send_references.entry(key).or_default() += 1;
            }
        }
    }
    pub(super) fn end_send(&mut self, draft: &str, attachments: &HashSet<String>) {
        for id in attachments {
            let key = (draft.to_owned(), id.clone());
            if let Some(count) = self.send_references.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    self.send_references.remove(&key);
                }
            }
        }
    }
    pub(super) fn next_release(&mut self) -> Option<(String, String)> {
        let available = self
            .releases
            .iter()
            .position(|key| !self.send_references.contains_key(key))?;
        Some(self.releases.swap_remove(available))
    }
    pub(super) fn insert(&mut self, draft: String, revision: u64, attachment_id: String) {
        self.images.insert(
            (draft, attachment_id),
            Image {
                revision,
                adopted: false,
            },
        );
    }
    pub(super) fn reject(&mut self, draft: String, attachment_id: String) {
        self.releases.push((draft, attachment_id));
    }
    pub(super) fn sent(&mut self, draft: &str, attachments: &HashSet<String>) {
        self.images
            .retain(|(owner, id), _| owner != draft || !attachments.contains(id));
    }
    pub(super) fn decision(&mut self, draft: String, attachment: String, accepted: bool) {
        let key = (draft, attachment);
        if accepted {
            if let Some(image) = self.images.get_mut(&key) {
                image.adopted = true;
            }
        } else {
            self.images.remove(&key);
            self.releases.push(key);
        }
    }
    pub(super) fn release(&mut self, draft: &str, revision: u64, selected: &HashSet<String>) {
        self.images.retain(|key, image| {
            if key.0 != draft
                || !image.adopted
                || revision <= image.revision
                || selected.contains(&key.1)
            {
                return true;
            }
            self.releases.push(key.clone());
            false
        });
    }
}

async fn call(
    owner: &native_history::Owner<'_>,
    draft: &str,
    action: DraftImageAction,
) -> Result<DraftImageResult, ApiError> {
    let op = matches!(action, DraftImageAction::Commit { .. })
        .then(|| format!("{}:{}", owner.instance, uuid::Uuid::new_v4()));
    super::compose_call(
        owner,
        Command::DraftImage(DraftImageArgs {
            instance_id: owner.instance.to_owned(),
            draft_id: draft.to_owned(),
            action,
        }),
        op,
    )
    .await
}

pub(super) async fn upload(
    owner: &native_history::Owner<'_>,
    draft: &str,
    bytes: Vec<u8>,
) -> Result<UiAttachment, ApiError> {
    let invalid_reply =
        || ApiError::new(ErrorCode::ProtocolMismatch, "Invalid draft image response");
    let DraftImageResult::Begun { upload_id } = call(
        owner,
        draft,
        DraftImageAction::Begin {
            encoded_bytes: bytes.len() as u64,
        },
    )
    .await?
    else {
        return Err(invalid_reply());
    };
    let mut offset = 0;
    for chunk in bytes.chunks(DRAFT_IMAGE_CHUNK_BYTES) {
        let reply = call(
            owner,
            draft,
            DraftImageAction::Append {
                upload_id: upload_id.clone(),
                offset,
                base64: STANDARD.encode(chunk),
            },
        )
        .await?;
        offset += chunk.len() as u64;
        if reply
            != (DraftImageResult::Appended {
                received_bytes: offset,
            })
        {
            return Err(invalid_reply());
        }
    }
    let DraftImageResult::Committed { attachment } =
        call(owner, draft, DraftImageAction::Commit { upload_id }).await?
    else {
        return Err(invalid_reply());
    };
    Ok(UiAttachment {
        id: attachment.id,
        path: attachment.path,
        kind: "image".into(),
        state: "ready".into(),
        position: None,
    })
}

pub(super) async fn release(
    owner: &native_history::Owner<'_>,
    draft: &str,
    attachment_id: String,
) -> Result<(), ApiError> {
    call(owner, draft, DraftImageAction::Release { attachment_id })
        .await
        .map(|_| ())
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
        images.forget_threads(&HashSet::from(["deleted".into()]));
        assert!(images.accept_image(old).is_none());
        assert!(images.accept_image(newly_sent).is_none());
        assert_eq!(images.accept_image(unrelated), Some(false));
        assert!(images.pending.is_empty());
    }
    #[test]
    fn failed_capture_reclaims_ticket_and_owner_even_when_command_error_was_already_reported() {
        let mut images = DraftImages::default();
        let ticket = images.begin_image("draft", Some("thread"));
        assert_eq!(images.accept_image(ticket), Some(false));
        assert!(images.pending.is_empty());
        assert!(images.accept_image(ticket).is_none());
    }
}
