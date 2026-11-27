//! Owner read of adopted-attempt usage for window observation.
use std::collections::HashMap;

// The indexed output_item.node_id relation identifies the adopted attempt,
// including repeated attempts in one Turn. Read at most two distinct owners
// so corrupt/ambiguous provenance never turns into a confident cache count.
pub(crate) const SOURCE: &str = "WITH owners AS (
    SELECT requested.value AS node_id,o.attempt_id,
           row_number() OVER (PARTITION BY requested.value ORDER BY o.attempt_id) AS ordinal
    FROM json_each(?1) requested
    JOIN output_item o INDEXED BY output_item_node ON o.node_id=requested.value
    GROUP BY requested.value,o.attempt_id
    ) SELECT owners.node_id,a.id,
    USAGE_SCALARS,INPUT_BYTES
    FROM owners JOIN model_attempt a ON a.id=owners.attempt_id
    WHERE owners.ordinal<=2";

/// One adopted attempt's reported tokens and measured request bytes (#186).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) attempt: String,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cached_tokens: Option<u64>,
    pub(crate) input_bytes: Option<crate::model::wire_bytes::InputBytes>,
}

pub(crate) struct Observation {
    pub(crate) index: usize,
    pub(crate) sources: Vec<Source>,
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
