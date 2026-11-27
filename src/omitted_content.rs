//! Provider-independent description of content omitted from a request.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    Spilled,
    AttachmentChanged,
    AttachmentUnavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct OmittedContent<R> {
    pub reason: OmissionReason,
    #[serde(rename = "ref")]
    pub reference: R,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub retained: bool,
    pub read: &'static str,
}
