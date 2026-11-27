//! The saved Draft and view-anchor shapes with the owner's rules for decoding
//! and validating them. Owner modules and the terminal client both import this
//! module; it knows neither.
use crate::{
    attachments::DraftContent,
    protocol::{ApiError, ErrorCode},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct UiAttachment {
    pub id: String,
    pub path: PathBuf,
    pub kind: String,
    pub state: String,
    /// Byte position of the owned marker. Metadata awaiting marker placement
    /// uses explicit null; a sendable draft must contain a concrete position.
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub position: Option<usize>,
}
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PasteFold {
    pub id: u64,
    pub start: usize,
    pub end: usize,
    pub expanded: bool,
}
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct EditRequestTarget {
    pub thread_id: String,
    pub node_id: String,
    pub run_id: Option<String>,
}
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DraftSnapshot {
    pub draft_id: String,
    pub revision: u64,
    pub text: String,
    pub cursor: usize,
    pub attachments: Vec<UiAttachment>,
    pub folds: Vec<PasteFold>,
    pub literal_paste: bool,
    /// Contiguous manually entered prefix in bytes; never restored from storage.
    #[serde(skip)]
    pub typed_prefix: usize,
    #[serde(default)]
    pub edit: Option<EditRequestTarget>,
}
impl Default for DraftSnapshot {
    fn default() -> Self {
        Self {
            draft_id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            text: String::new(),
            cursor: 0,
            attachments: Vec::new(),
            folds: Vec::new(),
            literal_paste: false,
            typed_prefix: 0,
            edit: None,
        }
    }
}
#[derive(
    Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ViewAnchor {
    pub node_id: Option<String>,
    pub char_offset: usize,
}

pub(crate) const MARKER: &str = "\u{fffc}";
fn invalid() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidArgument,
        "Draft attachment positions or source elements are inconsistent; retain the draft and inspect its source",
    )
}
pub(crate) fn decode(
    text: &str,
    mut attachments: Vec<UiAttachment>,
) -> Result<(DraftContent, Vec<UiAttachment>), ApiError> {
    let mut ids = HashSet::new();
    let mut positions = HashSet::new();
    for a in &attachments {
        if !ids.insert(&a.id) {
            return Err(invalid());
        }
        let p = a.position.ok_or_else(invalid)?;
        if !text.is_char_boundary(p) || !text[p..].starts_with(MARKER) || !positions.insert(p) {
            return Err(invalid());
        }
    }
    attachments.sort_by_key(|a| a.position.expect("validated marker position"));
    let mut plain = String::new();
    let mut source = 0;
    let mut offsets = Vec::new();
    for a in &attachments {
        let position = a.position.expect("validated marker position");
        plain.push_str(&text[source..position]);
        offsets.push(plain.len());
        source = position + MARKER.len();
    }
    plain.push_str(&text[source..]);
    let content = DraftContent {
        text: plain,
        positions: offsets,
    };
    content.validate(attachments.len())?;
    Ok((content, attachments))
}

/// Rebuild from the source's ordered references, never SQL ID order or text flattening.
pub(crate) fn restore(
    payload: &Value,
    attachments: Vec<UiAttachment>,
    selected_only: bool,
) -> Result<(String, Vec<UiAttachment>), ApiError> {
    let count = attachments.len();
    let mut records: HashMap<_, _> = attachments.into_iter().map(|a| (a.id.clone(), a)).collect();
    if records.len() != count {
        return Err(invalid());
    }
    if let Some(text) = payload.get("content").and_then(Value::as_str) {
        if !records.is_empty() {
            return Err(invalid());
        }
        return Ok((text.into(), Vec::new()));
    }
    let elements = payload
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let mut text = String::new();
    let mut ordered = Vec::new();
    for element in elements {
        match element.get("type").and_then(Value::as_str) {
            Some("input_text") => text.push_str(
                element
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?,
            ),
            Some("attachment_ref") => {
                let id = element
                    .get("attachment_id")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let mut attachment = records.remove(id).ok_or_else(invalid)?;
                if selected_only && attachment.state != "active" {
                    continue;
                }
                attachment.position = Some(text.len());
                text.push_str(MARKER);
                ordered.push(attachment);
            }
            _ => return Err(invalid()),
        }
    }
    if !records.is_empty() {
        return Err(invalid());
    }
    Ok((text, ordered))
}

pub(crate) fn validate_positions(snapshot: &DraftSnapshot, limit: u64) -> Result<(), ApiError> {
    if snapshot.text.len() as u64 > limit {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Draft exceeds the current input budget",
        ));
    }
    if snapshot.cursor > snapshot.text.len()
        || !snapshot.text.is_char_boundary(snapshot.cursor)
        || snapshot.folds.iter().any(|fold| {
            fold.start > fold.end
                || fold.end > snapshot.text.len()
                || !snapshot.text.is_char_boundary(fold.start)
                || !snapshot.text.is_char_boundary(fold.end)
        })
    {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "Saved draft positions are malformed",
        ));
    }
    Ok(())
}
