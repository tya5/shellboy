//! Saved attempt usage for F6; never infer the attempt from the latest Turn.
use crate::{presentation::PresentationField, ui::UiNode};

use crate::presentation::fields::{
    ATTEMPT_CACHED_TOKENS as FIELD, ATTEMPT_ID as ATTEMPT, ATTEMPT_INPUT_BYTES as BYTES,
    ATTEMPT_INPUT_TOKENS as TOKENS, NOT_RECORDED, NOT_REPORTED, NOT_REPORTED_AMBIGUOUS,
    NOT_REPORTED_NO_REQUEST,
};

fn cached_tokens(count: Option<u64>) -> String {
    count
        .map(|n| n.to_string())
        .unwrap_or_else(|| NOT_REPORTED.into())
}

#[cfg(test)]
use crate::history::window_facts::usage::*;
pub(super) use crate::history::window_facts::usage::{Observation, Source};

/// Apply the selected window's adopted-attempt usage to the nodes it names.
pub(super) fn apply_window(nodes: &mut [UiNode], usage: &[crate::protocol::HistoryWindowUsage]) {
    let observations = usage
        .iter()
        .filter_map(|saved| {
            let index = nodes
                .iter()
                .position(|node| node.node_id == saved.node_id)?;
            let sources = saved
                .sources
                .iter()
                .map(|source| Source {
                    attempt: source.attempt_id.clone(),
                    input_tokens: source.input_tokens,
                    cached_tokens: source.cached_tokens,
                    input_bytes: source.input_bytes.as_ref().and_then(|bytes| {
                        crate::model::wire_bytes::InputBytes::from_value(&serde_json::json!({
                            "unit": "utf8_bytes",
                            "total": bytes.total,
                            "categories": bytes.categories,
                        }))
                    }),
                })
                .collect();
            Some(Observation { index, sources })
        })
        .collect();
    apply(nodes, observations);
}

fn field(key: &str, value: String) -> PresentationField {
    PresentationField {
        value_omitted: false,
        omission: None,
        display: None,
        key: key.into(),
        label: crate::presentation::fields::attempt_label(key, crate::config::UiLanguage::Ja)
            .expect("request size field")
            .into(),
        value,
    }
}

/// Values stay language-neutral (counts, saved JSON, fixed unknown markers);
/// F6 renders and localizes them. Bytes and tokens are separate facts.
pub(super) fn apply(nodes: &mut [UiNode], observations: Vec<Observation>) {
    for observation in observations {
        let index = observation.index;
        let (attempt, tokens, cached, bytes) = match observation.sources.as_slice() {
            [source] => (
                Some(source.attempt.clone()),
                cached_tokens(source.input_tokens),
                cached_tokens(source.cached_tokens),
                source
                    .input_bytes
                    .as_ref()
                    .map_or_else(|| NOT_RECORDED.into(), |bytes| bytes.to_value().to_string()),
            ),
            [] => (
                None,
                NOT_REPORTED_NO_REQUEST.into(),
                NOT_REPORTED_NO_REQUEST.into(),
                NOT_RECORDED.into(),
            ),
            _ => (
                None,
                NOT_REPORTED_AMBIGUOUS.into(),
                NOT_REPORTED_AMBIGUOUS.into(),
                NOT_RECORDED.into(),
            ),
        };
        let presentation = &mut nodes[index].presentation;
        presentation
            .fields
            .retain(|field| !matches!(field.key.as_str(), FIELD | ATTEMPT | TOKENS | BYTES));
        let mut added = vec![
            field(BYTES, bytes),
            field(TOKENS, tokens),
            field(FIELD, cached),
        ];
        if let Some(attempt) = attempt {
            added.push(field(ATTEMPT, attempt));
        }
        presentation.fields.splice(0..0, added);
        if presentation.fields.len() > crate::presentation::MAX_FIELDS {
            presentation.metadata_limited = true;
            presentation.details_omitted = true;
        }
        presentation
            .fields
            .truncate(crate::presentation::MAX_FIELDS);
        presentation.bound_metadata();
    }
}

#[cfg(test)]
mod tests;
