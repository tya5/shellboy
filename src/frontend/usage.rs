//! Saved attempt usage for F6; never infer the attempt from the latest Turn.
use crate::{
    control::Control,
    presentation::{PresentationField, PresentationKind},
    protocol::ApiError,
    ui::UiNode,
};
use serde_json::Value;
use std::collections::HashMap;

const MAX_USAGE_BYTES: usize = 64 * 1024;
const FIELD: &str = "/ui_model_attempt/cached_tokens";
const ATTEMPT: &str = "/ui_model_attempt/attempt_id";

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
    CASE WHEN octet_length(a.usage_json)<=?2 THEN a.usage_json END,
    COALESCE(octet_length(a.usage_json)>?2,0)
    FROM owners JOIN model_attempt a ON a.id=owners.attempt_id
    WHERE owners.ordinal<=2";

fn cached_tokens(raw: Option<&str>, oversized: bool) -> String {
    if oversized {
        return "未報告（使用量が表示上限を超過）".into();
    }
    let Some(raw) = raw else {
        return "未報告".into();
    };
    if raw.len() > MAX_USAGE_BYTES {
        return "未報告（使用量が表示上限を超過）".into();
    }
    let Ok(usage) = serde_json::from_str::<Value>(raw) else {
        return "未報告（使用量の形式不正）".into();
    };
    match usage.pointer("/input_tokens_details/cached_tokens") {
        None | Some(Value::Null) => "未報告".into(),
        Some(value) => value
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "未報告（値が不正）".into()),
    }
}

pub(super) async fn enrich(control: &Control, nodes: &mut [UiNode]) -> Result<(), ApiError> {
    let requests = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.presentation.kind == PresentationKind::Assistant
                && !node.presentation.provisional_stream
        })
        .map(|(index, node)| (index, node.node_id.clone()))
        .collect::<Vec<_>>();
    if requests.is_empty() {
        return Ok(());
    }
    let ids = serde_json::to_string(&requests.iter().map(|(_, id)| id).collect::<Vec<_>>())
        .expect("string IDs serialize");
    let observations = control
        .store
        .database
        .call(move |db| {
            let mut statement = db.prepare(SOURCE)?;
            let mut sources = HashMap::<String, Vec<(String, Option<String>, bool)>>::new();
            let rows =
                statement.query_map(rusqlite::params![ids, MAX_USAGE_BYTES as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                })?;
            for row in rows {
                let (node, id, raw, oversized) = row?;
                sources.entry(node).or_default().push((id, raw, oversized));
            }
            let mut observations = Vec::with_capacity(requests.len());
            for (index, node) in requests {
                let (attempt, display) = match sources.get(&node).map(Vec::as_slice).unwrap_or(&[])
                {
                    [(id, raw, oversized)] => {
                        (Some(id.clone()), cached_tokens(raw.as_deref(), *oversized))
                    }
                    [] => (None, "未報告（要求の記録なし）".into()),
                    _ => (None, "未報告（要求との対応が一意でない）".into()),
                };
                observations.push((index, attempt, display));
            }
            Ok(observations)
        })
        .await
        .map_err(|error| error.into_api_error())?;
    for (index, attempt, display) in observations {
        let presentation = &mut nodes[index].presentation;
        presentation
            .fields
            .retain(|field| !matches!(field.key.as_str(), FIELD | ATTEMPT));
        if let Some(attempt) = attempt {
            presentation.fields.insert(
                0,
                PresentationField {
                    value_omitted: false,
                    display: None,
                    key: ATTEMPT.into(),
                    label: "モデル要求ID（使用量の記録元）".into(),
                    value: attempt,
                },
            );
        }
        presentation.fields.insert(
            0,
            PresentationField {
                value_omitted: false,
                display: None,
                key: FIELD.into(),
                label: "入力キャッシュ（cached_tokens）".into(),
                value: display,
            },
        );
        if presentation.fields.len() > crate::presentation::MAX_FIELDS {
            presentation.metadata_limited = true;
            presentation.details_omitted = true;
        }
        presentation
            .fields
            .truncate(crate::presentation::MAX_FIELDS);
        presentation.bound_metadata();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
