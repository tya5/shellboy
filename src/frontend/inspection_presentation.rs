//! Off-worker projection of one current metadata/usage pair and stable field anchors.
use super::{ApiError, inspection_state::Presentation, management};
use crate::{
    presentation::PresentationField,
    ui::{InspectionFieldAnchor, InspectionSnapshot},
};
use serde_json::Value;

pub(super) struct Prepared {
    pub report: management::Report,
    pub snapshot: InspectionSnapshot,
}
#[cfg(test)]
pub(super) fn prepare(ticket: &Presentation, budget: usize) -> Result<Prepared, ApiError> {
    prepare_in(crate::config::UiLanguage::Ja, ticket, budget)
}
pub(super) fn prepare_in(
    language: crate::config::UiLanguage,
    ticket: &Presentation,
    budget: usize,
) -> Result<Prepared, ApiError> {
    let mut value = (*ticket.metadata.value).clone();
    if let Some(physical) = &ticket.physical {
        value["physical_observation"] = (**physical).clone();
    } else if matches!(ticket.target, crate::ui::InspectionTarget::Storage) {
        value["physical_observation"] = serde_json::json!({"available":false,"sample":null,"notice":"Physical storage sample has not been read; saved DB facts have a separate observation time"});
    }
    if let Some(usage) = &ticket.usage
        && !matches!(ticket.target, crate::ui::InspectionTarget::Usage(_))
    {
        value["usage_totals"] = (*usage.value).clone();
        if let Some(context) = &usage.context_usage {
            let same_scope =
                usage
                    .context_scope
                    .as_ref()
                    .is_some_and(|(thread, connection, model)| {
                        value["thread_id"].as_str() == Some(thread.as_str())
                            && value["connection"].as_str() == Some(connection.as_str())
                            && value["model"].as_str() == Some(model.as_str())
                    });
            value["context_usage"] = if same_scope {
                (**context).clone()
            } else {
                serde_json::json!({"observation":"previous_configuration","last_observation":**context,"read_at":usage.observed_at})
            };
        }
    }
    if matches!(ticket.target, crate::ui::InspectionTarget::Thread(_))
        && value["context_usage"].is_null()
    {
        value["context_usage"] = serde_json::json!({"observation":"not_yet_read","detail":"保存済み使用量の取得待ち","inspect":"shellboy thread get --thread <thread_id>"});
    }
    if !ticket.limited_fields.is_empty() {
        value["view_limits"] = serde_json::json!({"limited_fields":ticket.limited_fields});
    }
    if matches!(ticket.target, crate::ui::InspectionTarget::Index(_)) {
        merge_previous_coverage(&mut value, ticket.last_coverage.as_ref());
    }
    let current_cursor = value
        .pointer("/management/coverage_window/next_cursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if ticket.target.retains_history() {
        value = serde_json::json!({"current_source":value});
    }
    let mut source = crate::presentation::safe_detail_value(value);
    // A private JSON-encoded cursor is an opaque navigation token, not a field
    // tree to expand or rewrite. Its list row never displays the token.
    if let Some(cursor) = &current_cursor {
        source["management"]["coverage_window"]["next_cursor"] = Value::String(cursor.clone());
    }
    let mut report = management::report_in(language, source.clone(), budget);
    if let Some(detail) = &mut report.detail
        && let Some(cursor) = &current_cursor
    {
        detail["management"]["coverage_window"]["next_cursor"] = Value::String(cursor.clone());
    }
    let mut fields = std::mem::take(&mut report.fields);
    if matches!(ticket.target, crate::ui::InspectionTarget::Index(_)) {
        fields = prioritize_index(&source, fields, &mut report.omitted);
    }
    if ticket.target.retains_history() {
        report.omitted |= fields.len() > 128;
        fields.truncate(128);
        for field in &mut fields {
            if field.key.is_empty() {
                field.key = "/current_source".into();
            }
            field.label =
                crate::ui::text_width::glyphs::formatted!("現在 · {}", "現在 | {}", field.label);
        }
    }
    // report.detail may be omitted by a tiny budget. Anchors still derive from
    // exactly the current values, before the bounded retention decision.
    let anchors = anchors(&source, &fields);
    let snapshot = InspectionSnapshot {
        target: ticket.target.clone(),
        metadata_sequence: ticket.metadata.sequence,
        usage_sequence: ticket.usage.as_ref().map(|facts| facts.sequence),
        metadata_observed_at: ticket.metadata.observed_at.clone(),
        usage_observed_at: ticket.usage.as_ref().map(|facts| facts.observed_at.clone()),
        fields,
        anchors,
        omitted: report.omitted,
    };
    Ok(Prepared { report, snapshot })
}
/// A failed current window must not turn yesterday's cursor into a current action.
fn merge_previous_coverage(value: &mut Value, last: Option<&super::inspection_state::Facts>) {
    if value.pointer("/management/coverage_window/available") != Some(&Value::Bool(false)) {
        return;
    }
    if let Some(current) = value
        .pointer_mut("/management/coverage_window")
        .and_then(Value::as_object_mut)
    {
        current.remove("next_cursor");
        current.insert("current".into(), Value::Bool(false));
        if let Some(last) = last
            && let Some(previous) = last.value.pointer("/management/coverage_window")
        {
            let mut previous = previous.clone();
            if let Some(object) = previous.as_object_mut() {
                object.remove("next_cursor");
                object.insert(
                    "coverage".into(),
                    last.value
                        .pointer("/management/coverage")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
            current.insert("last_observation".into(), previous);
            current.insert(
                "last_observed_at".into(),
                Value::String(last.observed_at.clone()),
            );
            current.insert(
                "last_read_sequence".into(),
                serde_json::json!(last.sequence),
            );
            current.insert("stale".into(), Value::Bool(true));
        }
    }
}
fn atomic_count(value: &Value) -> String {
    let number = value.get("value").and_then(Value::as_u64);
    match (
        number,
        value.get("complete").and_then(Value::as_bool),
        value.get("relation").and_then(Value::as_str),
    ) {
        (Some(n), Some(true), Some("exact")) => format!("{n}（確認済み）"),
        (Some(n), _, Some("lower_bound")) => format!("{n}以上（下限・未確認あり）"),
        _ => "不明（未確認）".into(),
    }
}
fn compact_value(value: &Value) -> (String, bool) {
    let message = value
        .get("message")
        .or_else(|| value.pointer("/error/message"))
        .and_then(Value::as_str);
    let text = if let Some(message) = message {
        message.to_owned()
    } else {
        match value {
            Value::String(text) => {
                let decoded = serde_json::from_str::<Value>(text).ok();
                decoded
                    .as_ref()
                    .and_then(|v| v.get("message").or_else(|| v.pointer("/error/message")))
                    .and_then(Value::as_str)
                    .unwrap_or(text)
                    .to_owned()
            }
            Value::Null => "未確認".into(),
            other => other.to_string(),
        }
    };
    let omitted = value.is_object() || value.is_array() || text.chars().count() > 256;
    let text = text.chars().take(256).collect::<String>();
    (crate::presentation::sanitize_plain(&text, 0).0, omitted)
}
fn prioritize_index(
    source: &Value,
    old: Vec<PresentationField>,
    omitted: &mut bool,
) -> Vec<PresentationField> {
    const CURRENT: &[(&str, &str)] = &[
        ("/management/auto_history", "索引 › 履歴の自動索引"),
        (
            "/management/automatic_history_reason",
            "索引 › 自動索引の方針",
        ),
        ("/management/status/paused", "索引 › 一時停止"),
        ("/management/status/pause_known", "索引 › 停止状態の確認"),
        ("/management/status/reason", "索引 › 現在の理由"),
        ("/management/status/paused_at", "索引 › 停止日時"),
        ("/management/status/pause_id", "索引 › 停止記録"),
        ("/management/complete", "観測 › 全体の確認"),
        (
            "/management/metadata_limited",
            "観測 › 保存メタデータの制限",
        ),
        ("/unknown_work_records", "観測 › 未確認の処理記録"),
        (
            "/management/coverage_window/available",
            "coverage › 今回の取得",
        ),
        (
            "/management/coverage_window/current",
            "coverage › 現在の値か",
        ),
        ("/management/coverage_window/kind", "coverage › 観測種別"),
        ("/management/coverage_window/error", "coverage › 更新結果"),
        (
            "/management/coverage_window/evaluated_sources",
            "coverage › 今回確認した資料数",
        ),
        (
            "/management/coverage_window/inspected_memberships",
            "coverage › 今回調べた対象数",
        ),
        (
            "/management/coverage_window/window_complete",
            "coverage › このwindowの確認",
        ),
        (
            "/management/coverage_window/scope_exhausted",
            "coverage › 対象範囲の末尾",
        ),
        (
            "/management/coverage_window/read_at",
            "coverage › 今回の観測日時",
        ),
        ("/management/coverage_window/stale", "coverage › 以前の観測"),
        (
            "/management/coverage_window/last_observed_at",
            "coverage › 以前の観測日時",
        ),
        (
            "/management/coverage_window/last_read_sequence",
            "coverage › 以前の読取番号",
        ),
        (
            "/management/coverage_window/last_observation",
            "coverage › 以前の内容（現在ではない）",
        ),
    ];
    let mut fields = Vec::new();
    if let Some(full) = old
        .iter()
        .find(|field| field.key.is_empty() && field.label == "すべての詳細を見る")
    {
        fields.push(full.clone());
    }
    fields.extend(
        old.iter()
            .filter(|field| field.key.is_empty() && field.label != "すべての詳細を見る")
            .cloned(),
    );
    for (state, label) in [
        ("held_auto_history", "索引 › 自動索引無効による保留"),
        ("pending", "索引 › 順番待ち"),
        ("running", "索引 › 実行中"),
        ("done", "索引 › 完了"),
        ("failed", "索引 › 失敗"),
    ] {
        let key = format!("/management/status/counts/{state}");
        if let Some(value) = source.pointer(&key) {
            fields.push(PresentationField {
                key,
                label: label.into(),
                value: atomic_count(value),
                value_omitted: true,
                display: None,
            });
        }
    }
    if let Some(value) = source.pointer("/management/spaces_count") {
        fields.push(PresentationField {
            key: "/management/spaces_count".into(),
            label: "索引空間の確認数".into(),
            value: atomic_count(value),
            value_omitted: true,
            display: None,
        });
    }
    for (key, label) in CURRENT {
        if let Some(value) = source.pointer(key) {
            let (text, short) = if *key == "/management/coverage_window/last_observation" {
                ("以前の観測の詳細（現在の値ではありません）".into(), true)
            } else {
                compact_value(value)
            };
            let display = key
                .ends_with("_at")
                .then(|| crate::presentation::fields::local_time(&text))
                .flatten();
            fields.push(PresentationField {
                key: (*key).into(),
                label: (*label).into(),
                value: text,
                value_omitted: short,
                display,
            });
        }
    }
    if source
        .pointer("/management/coverage_window/next_cursor")
        .and_then(Value::as_str)
        .is_some()
    {
        fields.push(PresentationField {
            key: "/management/coverage_window/next_cursor".into(),
            label: "次のcoverage window".into(),
            value: "次のwindowあり（n）".into(),
            value_omitted: true,
            display: None,
        });
    }
    let cap = crate::presentation::MAX_FIELDS + 1;
    if fields.len() > cap {
        fields.truncate(cap);
        *omitted = true;
    }
    let mandatory = fields
        .iter()
        .map(|field| field.key.clone())
        .collect::<std::collections::HashSet<_>>();
    for field in old {
        if mandatory.contains(&field.key)
            || field.key.starts_with("/management/status/counts/")
            || field.key.starts_with("/management/spaces_count/")
            || field
                .key
                .starts_with("/management/coverage_window/next_cursor/")
        {
            continue;
        }
        if fields.len() == cap {
            *omitted = true;
            break;
        }
        fields.push(field);
    }
    fields
}
fn scalar_id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256)
}
fn identity(value: &Value, array: &str) -> Option<(String, String)> {
    if let Some(id) = scalar_id(value) {
        let kind = match array {
            "owned_run_ids" => "run",
            "owned_execution_ids" => "execution",
            _ => return None,
        };
        return Some((kind.into(), id.into()));
    }
    let object = value.as_object()?;
    if array == "attachments" {
        let thread = object.get("thread_id").and_then(scalar_id)?;
        let id = object.get("attachment_id").and_then(scalar_id)?;
        return Some(("attachment_selection".into(), format!("{thread}/{id}")));
    }
    if array == "spaces"
        && let Some(id) = object.get("id").and_then(scalar_id)
    {
        if [
            "thread_id",
            "run_id",
            "turn_id",
            "execution_id",
            "node_id",
            "message_id",
            "attachment_id",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
        {
            return None;
        }
        if object
            .get("space_id")
            .and_then(scalar_id)
            .is_some_and(|other| other != id)
        {
            return None;
        }
        return Some(("space".into(), id.into()));
    }
    if array == "probes" {
        // Connection/model/dimensions omit endpoint-family identity; old and
        // current endpoints can share that tuple. Only a saved key proves it.
        if let Some(key) = object.get("record_key").and_then(scalar_id) {
            return Some(("index_probe".into(), key.into()));
        }
    }
    if let (Some(kind), Some(id)) = (
        object.get("kind").and_then(Value::as_str),
        object.get("id").and_then(scalar_id),
    ) && matches!(
        kind,
        "thread" | "run" | "turn" | "shell" | "execution" | "message" | "node" | "attachment"
    ) {
        return Some((kind.into(), id.into()));
    }
    let mut keys = [
        ("thread_id", "thread"),
        ("run_id", "run"),
        ("turn_id", "turn"),
        ("execution_id", "execution"),
        ("node_id", "node"),
        ("message_id", "message"),
        ("attachment_id", "attachment"),
        ("space_id", "space"),
    ]
    .into_iter()
    .filter_map(|(key, kind)| {
        object
            .get(key)
            .and_then(scalar_id)
            .map(|id| (kind.to_owned(), id.to_owned()))
    });
    let candidate = keys.next()?;
    if keys.next().is_some() {
        None
    } else {
        Some(candidate)
    }
}
pub(super) fn anchors(source: &Value, fields: &[PresentationField]) -> Vec<InspectionFieldAnchor> {
    fields
        .iter()
        .map(|field| {
            let pointer = field.key.clone();
            let tokens = pointer
                .strip_prefix('/')
                .map(|rest| rest.split('/').collect::<Vec<_>>())
                .unwrap_or_default();
            let mut current = source;
            let mut entity = None;
            let mut relative = 0;
            let mut stable = true;
            let mut array = String::new();
            for (position, escaped) in tokens.iter().enumerate() {
                let token = escaped.replace("~1", "/").replace("~0", "~");
                if let Some(items) = current.as_array() {
                    let Some(value) = token
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| items.get(index))
                    else {
                        stable = false;
                        break;
                    };
                    if let Some(id) = identity(value, &array).filter(|candidate| {
                        items
                            .iter()
                            .filter(|item| identity(item, &array).as_ref() == Some(candidate))
                            .count()
                            == 1
                    }) {
                        entity = Some(id);
                        relative = position + 1;
                        stable = true;
                    } else {
                        stable = false;
                    }
                    current = value;
                } else if let Some(value) =
                    current.as_object().and_then(|object| object.get(&token))
                {
                    array = token;
                    current = value;
                } else {
                    stable = false;
                    break;
                }
            }
            let relative_pointer = if entity.is_some() && relative < tokens.len() {
                format!("/{}", tokens[relative..].join("/"))
            } else if entity.is_some() {
                String::new()
            } else {
                pointer.clone()
            };
            InspectionFieldAnchor {
                pointer,
                entity,
                relative_pointer,
                stable,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn field(key: &str) -> PresentationField {
        PresentationField {
            key: key.into(),
            label: key.into(),
            value: String::new(),
            value_omitted: false,
            display: None,
        }
    }
    #[test]
    fn multiple_context_ids_do_not_guess_the_identity_of_an_untyped_array_row() {
        let source = serde_json::json!({"items":[{"thread_id":"same-thread", "run_id":"first-run", "state":"live"}]});
        let result = anchors(&source, &[field("/items/0/state")]);
        assert!(!result[0].stable);
        assert!(result[0].entity.is_none());
    }

    #[test]
    fn semantic_entities_survive_reorder_and_ambiguous_indices_do_not() {
        let source = serde_json::json!({"items":[{"kind":"run","id":"r1","state":"live"},{"kind":"run","id":"r2","state":"stopped"}],"owned_run_ids":["child"],"unknown":["value"],"numeric":{"0":"object property"}});
        let fields = [
            field("/items/1/state"),
            field("/owned_run_ids/0"),
            field("/unknown/0"),
            field("/numeric/0"),
        ];
        let result = anchors(&source, &fields);
        assert_eq!(result[0].entity, Some(("run".into(), "r2".into())));
        assert_eq!(result[0].relative_pointer, "/state");
        assert!(result[0].stable);
        assert_eq!(result[1].entity, Some(("run".into(), "child".into())));
        assert!(result[1].stable);
        assert!(!result[2].stable);
        assert!(result[3].stable);
        let moved = serde_json::json!({"items":[{"kind":"run","id":"r2","state":"live"}]});
        let moved = anchors(&moved, &[field("/items/0/state")]);
        assert_eq!(moved[0].entity, result[0].entity);
        assert_eq!(moved[0].relative_pointer, result[0].relative_pointer);
    }
    #[test]
    fn escaped_object_keys_and_nested_unidentified_arrays_keep_their_real_semantics() {
        let source = serde_json::json!({"a/b":{"~key":"value"},"items":[{"kind":"run","id":"r","values":[1,2]}]});
        let result = anchors(&source, &[field("/a~1b/~0key"), field("/items/0/values/1")]);
        assert!(result[0].stable);
        assert_eq!(result[0].pointer, "/a~1b/~0key");
        assert!(!result[1].stable);
    }
    #[test]
    fn index_current_facts_and_atomic_counts_survive_large_collections() {
        let probes=(0..32).map(|n|serde_json::json!({"connection":"c","model":"m","requested_dimensions":n,"dimension":n,"checked_at":"now","success":true,"error":null})).collect::<Vec<_>>();
        let spaces=(0..200).map(|n|serde_json::json!({"id":format!("space-{n}"),"model":"m","state":"active","segments":{"value":10,"complete":true,"relation":"exact"}})).collect::<Vec<_>>();
        let source = serde_json::json!({"probes":probes,"spaces":spaces,"complete":false,"metadata_limited":true,"status":{"paused":true,"pause_known":true,"reason":"Saved pause","counts":{"pending":{"value":1024,"complete":false,"relation":"lower_bound"},"running":{"value":null,"complete":false,"relation":"unknown"},"done":{"value":2,"complete":true,"relation":"exact"}}},"coverage":{"evaluated_sources":32,"window_complete":false,"scope_exhausted":false,"next_cursor":"DO_NOT_DISPLAY_TOKEN"}});
        let mut management = source;
        let coverage = management
            .as_object_mut()
            .unwrap()
            .remove("coverage")
            .unwrap();
        management["coverage_window"] = coverage;
        let source = serde_json::json!({"management":management});
        let mut report = management::report(source.clone(), usize::MAX);
        let fields = prioritize_index(
            &source,
            std::mem::take(&mut report.fields),
            &mut report.omitted,
        );
        assert!(fields.len() <= crate::presentation::MAX_FIELDS + 1);
        assert_eq!(
            fields
                .iter()
                .filter(|f| f.key.is_empty() && f.label == "すべての詳細を見る")
                .count(),
            1
        );
        for key in [
            "/management/status/paused",
            "/management/status/reason",
            "/management/complete",
            "/management/metadata_limited",
            "/management/coverage_window/evaluated_sources",
            "/management/coverage_window/window_complete",
            "/management/coverage_window/scope_exhausted",
            "/management/coverage_window/next_cursor",
        ] {
            assert!(
                fields.iter().any(|f| f.key == key),
                "missing current fact {key}"
            );
        }
        assert!(
            fields
                .iter()
                .find(|f| f.key == "/management/status/counts/pending")
                .unwrap()
                .value
                .contains("1024以上")
        );
        assert_eq!(
            fields
                .iter()
                .find(|f| f.key == "/management/status/counts/running")
                .unwrap()
                .value,
            "不明（未確認）"
        );
        assert!(
            fields
                .iter()
                .find(|f| f.key == "/management/status/counts/done")
                .unwrap()
                .value
                .contains("確認済み")
        );
        assert!(
            !fields
                .iter()
                .any(|f| f.key.starts_with("/management/status/counts/pending/"))
        );
        assert!(
            !fields
                .iter()
                .find(|f| f.key == "/management/coverage_window/next_cursor")
                .unwrap()
                .value
                .contains("DO_NOT_DISPLAY_TOKEN")
        );
        assert_eq!(
            report.detail.as_ref().unwrap()["management"]["status"]["counts"]["pending"],
            source["management"]["status"]["counts"]["pending"]
        );
    }
    #[test]
    fn stale_coverage_keeps_previous_observation_separate_from_current_status_and_navigation() {
        let last = super::super::inspection_state::Facts {
            value: std::sync::Arc::new(
                serde_json::json!({"management":{"coverage":{"fully_indexed_sources":9},"coverage_window":{"next_cursor":"STALE_TOKEN","window_complete":true}}}),
            ),
            sequence: 7,
            observed_at: "old-time".into(),
            context_usage: None,
            context_scope: None,
        };
        let mut source = serde_json::json!({"management":{"status":{"paused":true,"reason":"Current pause"},"coverage_window":{"available":false,"next_cursor":"BAD_CURRENT_TOKEN","error":"scope changed"}}});
        merge_previous_coverage(&mut source, Some(&last));
        assert_eq!(source["management"]["status"]["reason"], "Current pause");
        assert_eq!(source["management"]["coverage_window"]["current"], false);
        assert_eq!(source["management"]["coverage_window"]["stale"], true);
        assert_eq!(
            source["management"]["coverage_window"]["last_observed_at"],
            "old-time"
        );
        assert_eq!(
            source["management"]["coverage_window"]["last_read_sequence"],
            7
        );
        assert_eq!(
            source["management"]["coverage_window"]["last_observation"]["coverage"]["fully_indexed_sources"],
            9
        );
        assert!(
            source
                .pointer("/management/coverage_window/next_cursor")
                .is_none()
        );
        assert!(
            source
                .pointer("/management/coverage_window/last_observation/next_cursor")
                .is_none()
        );
        let mut report = management::report(source.clone(), usize::MAX);
        let fields = prioritize_index(
            &source,
            std::mem::take(&mut report.fields),
            &mut report.omitted,
        );
        assert!(
            fields
                .iter()
                .any(|f| f.key == "/management/status/reason" && f.value == "Current pause")
        );
        assert!(
            !fields
                .iter()
                .any(|f| f.key == "/management/coverage_window/next_cursor")
        );
        assert!(
            fields
                .iter()
                .find(|f| f.key == "/management/coverage_window/last_observation")
                .unwrap()
                .value
                .contains("現在の値ではありません")
        );
    }
    #[test]
    fn space_plain_id_is_collection_scoped_and_probe_tuple_without_saved_key_is_unproven() {
        let first = serde_json::json!({"spaces":[{"id":"one","state":"active"},{"id":"two","state":"paused"}],"probes":[{"connection":"c","model":"m","requested_dimensions":2,"success":true},{"record_key":"index.probe.exact","success":false}]});
        let old = anchors(
            &first,
            &[
                field("/spaces/1/state"),
                field("/probes/0/success"),
                field("/probes/1/success"),
            ],
        );
        assert_eq!(old[0].entity, Some(("space".into(), "two".into())));
        assert!(old[0].stable);
        assert!(!old[1].stable);
        assert_eq!(
            old[2].entity,
            Some(("index_probe".into(), "index.probe.exact".into()))
        );
        let moved = serde_json::json!({"spaces":[{"id":"two","state":"active"}]});
        let new = anchors(&moved, &[field("/spaces/0/state")]);
        assert_eq!(old[0].entity, new[0].entity);
        assert_eq!(old[0].relative_pointer, new[0].relative_pointer);
        for source in [
            serde_json::json!({"spaces":[{"id":"same","state":"active"},{"id":"same","state":"paused"}]}),
            serde_json::json!({"spaces":[{"id":"space","thread_id":"thread","state":"active"}]}),
        ] {
            assert!(!anchors(&source, &[field("/spaces/0/state")])[0].stable);
        }
    }
}
