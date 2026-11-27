//! Saved attempt usage for F6; never infer the attempt from the latest Turn.
use crate::{presentation::PresentationField, ui::UiNode};
use std::collections::HashMap;

use crate::presentation::fields::{
    ATTEMPT_CACHED_TOKENS as FIELD, ATTEMPT_ID as ATTEMPT, ATTEMPT_INPUT_BYTES as BYTES,
    ATTEMPT_INPUT_TOKENS as TOKENS, NOT_RECORDED, NOT_REPORTED, NOT_REPORTED_AMBIGUOUS,
    NOT_REPORTED_NO_REQUEST,
};

// The indexed output_item.node_id relation identifies the adopted attempt,
// including repeated attempts in one Turn. Read at most two distinct owners
// so corrupt/ambiguous provenance never turns into a confident cache count.
const SOURCE: &str = "WITH owners AS (
    SELECT requested.value AS node_id,o.attempt_id,
           row_number() OVER (PARTITION BY requested.value ORDER BY o.attempt_id) AS ordinal
    FROM json_each(?1) requested
    JOIN output_item o INDEXED BY output_item_node ON o.node_id=requested.value
    GROUP BY requested.value,o.attempt_id
    ) SELECT owners.node_id,a.id,
    USAGE_SCALARS,INPUT_BYTES
    FROM owners JOIN model_attempt a ON a.id=owners.attempt_id
    WHERE owners.ordinal<=2";

fn cached_tokens(count: Option<u64>) -> String {
    count
        .map(|n| n.to_string())
        .unwrap_or_else(|| NOT_REPORTED.into())
}

/// One adopted attempt's reported tokens and measured request bytes (#186).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    pub(super) attempt: String,
    pub(super) input_tokens: Option<u64>,
    pub(super) cached_tokens: Option<u64>,
    pub(super) input_bytes: Option<crate::model::wire_bytes::InputBytes>,
}

pub(crate) struct Observation {
    index: usize,
    sources: Vec<Source>,
}

impl Observation {
    pub(crate) fn into_window(
        self,
        items: &[(String, String)],
    ) -> crate::protocol::HistoryWindowUsage {
        use crate::model::wire_bytes::Category;
        use crate::protocol::{
            HistoryWindowInputBytes, HistoryWindowUsage, HistoryWindowUsageSource,
        };
        HistoryWindowUsage {
            node_id: items[self.index].0.clone(),
            sources: self
                .sources
                .into_iter()
                .map(|source| HistoryWindowUsageSource {
                    attempt_id: source.attempt,
                    input_tokens: source.input_tokens,
                    cached_tokens: source.cached_tokens,
                    input_bytes: source.input_bytes.map(|bytes| HistoryWindowInputBytes {
                        total: bytes.total,
                        categories: Category::ALL
                            .into_iter()
                            .map(|kind| (kind.name().to_owned(), bytes.get(kind)))
                            .collect(),
                    }),
                })
                .collect(),
        }
    }
}

pub(crate) fn read_tx(
    db: &rusqlite::Connection,
    requests: &[(usize, String)],
) -> Result<Vec<Observation>, crate::storage::StorageError> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(&requests.iter().map(|(_, id)| id).collect::<Vec<_>>())
        .expect("string IDs serialize");
    let sql = SOURCE
        .replace(
            "USAGE_SCALARS",
            &crate::usage::sql_projection("a.usage_json"),
        )
        .replace("INPUT_BYTES", crate::usage_totals::INPUT_BYTES_SQL);
    let mut statement = db.prepare(&sql)?;
    let mut sources = HashMap::<String, Vec<Source>>::new();
    let rows = statement.query_map(rusqlite::params![ids], |row| {
        let usage = crate::usage::StandardUsage::read(row, 2)?;
        Ok((
            row.get::<_, String>(0)?,
            Source {
                attempt: row.get(1)?,
                input_tokens: usage.input_tokens,
                cached_tokens: usage.cached_tokens,
                input_bytes: crate::usage_totals::parse_input_bytes(row.get(7)?),
            },
        ))
    })?;
    for row in rows {
        let (node, source) = row?;
        sources.entry(node).or_default().push(source);
    }
    Ok(requests
        .iter()
        .map(|(index, node)| Observation {
            index: *index,
            sources: sources.get(node).cloned().unwrap_or_default(),
        })
        .collect())
}

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
