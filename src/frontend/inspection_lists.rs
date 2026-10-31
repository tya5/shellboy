//! Thin list adapter: no admission, task, cache, notification or adoption owner.
use crate::{
    protocol::*, readback::list_observation as retained, status::observation as status,
    storage::StorageError,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    Status,
    Thread,
    Shell,
    Compact,
}
#[derive(Clone, Debug)]
pub(super) struct CapturedScope {
    pub command: Command,
    pub original: Arc<Value>,
    pub ids: Vec<String>,
    pub work: Vec<status::Key>,
    pub compact: Vec<retained::RetainedSource>,
    pub threads: Vec<String>,
    pub route: Route,
    text_bytes: usize,
}
fn command_bytes(command: &Command) -> Result<usize, ApiError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > status::TEXT_BYTES {
                return Err(std::io::Error::other("query metadata limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, command)
        .map_err(|_| bad("Captured query metadata exceeds channel"))?;
    Ok(counter.0)
}
fn bad(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InputLimit, message)
}
/// Counts strings by reference before any scope/result copy. Structural bounds
/// prevent a tiny-text but arbitrarily nested/large object from bypassing it.
fn metadata_size(value: &Value, depth: usize, nodes: &mut usize) -> Result<usize, ApiError> {
    *nodes += 1;
    if depth > 16 || *nodes > 16384 {
        return Err(bad("Captured list metadata structure exceeds limit"));
    }
    let total = match value {
        Value::String(s) => s.len(),
        Value::Array(values) => values.iter().try_fold(0usize, |n, v| {
            Ok::<_, ApiError>(n.saturating_add(metadata_size(v, depth + 1, nodes)?))
        })?,
        Value::Object(values) => values.iter().try_fold(0usize, |n, (k, v)| {
            Ok::<_, ApiError>(n.saturating_add(k.len()).saturating_add(metadata_size(
                v,
                depth + 1,
                nodes,
            )?))
        })?,
        _ => 0,
    };
    if total > status::TEXT_BYTES {
        return Err(bad("Captured list metadata text exceeds256KiB"));
    }
    Ok(total)
}
/// Only explicit command/result identities. UI delivery owner is not a target.
pub(super) fn capture_list_scope(
    command: &Command,
    original: Arc<Value>,
    changed_threads: &[String],
) -> Result<CapturedScope, ApiError> {
    command.validate()?;
    status::validate_ids(changed_threads)?;
    let route = match command {
        Command::Status(args) if !args.memory => Route::Status,
        Command::ThreadList(_) => Route::Thread,
        Command::ShellList(_) => Route::Shell,
        Command::CompactList(_) => Route::Compact,
        _ => return Err(bad("Command is not a retained list route")),
    };
    let mut text_bytes =
        metadata_size(&original, 0, &mut 0)?.saturating_add(command_bytes(command)?);
    let rows = original
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("Captured list requires canonical items array"))?;
    if rows.len() > status::MAX_IDS {
        return Err(bad("Captured list exceeds200 retained rows"));
    }
    let mut ids = Vec::new();
    let mut work = Vec::new();
    let mut compact = Vec::new();
    for row in rows {
        let key = match route {
            Route::Status => "id",
            Route::Thread => "thread_id",
            Route::Shell => "execution_id",
            Route::Compact => "event_id",
        };
        let id = row
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| bad("Captured row has no canonical identity"))?;
        if id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control) {
            return Err(bad("Captured row identity exceeds scalar limit"));
        }
        ids.push(id.into());
        if route == Route::Status {
            let kind = row["kind"]
                .as_str()
                .and_then(status::Kind::parse)
                .ok_or_else(|| bad("Captured Status kind is not native"))?;
            work.push(status::Key {
                kind,
                id: id.into(),
            });
        }
        if route == Route::Compact {
            if let Some(operations) = row["operation_ids"].as_array() {
                for operation in operations {
                    if let Some(source) = operation.as_str() {
                        if source.is_empty()
                            || source.len() > 1024
                            || source.chars().any(char::is_control)
                        {
                            return Err(bad("Compact source identity exceeds scalar limit"));
                        }
                        compact.push(retained::RetainedSource {
                            event_id: id.into(),
                            source_id: source.into(),
                            source: retained::Source::Operation,
                        });
                    }
                }
            }
            if let Some(source) = row["attempt_id"].as_str() {
                if source.is_empty() || source.len() > 1024 || source.chars().any(char::is_control)
                {
                    return Err(bad("Compact attempt identity exceeds scalar limit"));
                }
                compact.push(retained::RetainedSource {
                    event_id: id.into(),
                    source_id: source.into(),
                    source: retained::Source::Attempt,
                });
            }
            if row["record_kind"] == "retained_replacement" {
                compact.push(retained::RetainedSource {
                    event_id: id.into(),
                    source_id: id.into(),
                    source: retained::Source::Node,
                });
            }
        }
    }
    status::validate_ids(&ids)?;
    if compact.len() > status::MAX_IDS {
        return Err(bad("Captured compact source associations exceed200"));
    }
    let mut threads = changed_threads.iter().cloned().collect::<BTreeSet<_>>();
    let explicit = match command {
        Command::Status(a) => a.thread_id.as_ref(),
        Command::ThreadList(_) => None,
        Command::ShellList(a) => a.thread_id.as_ref(),
        Command::CompactList(a) => a.thread_id.as_ref(),
        _ => None,
    };
    if let Some(thread) = explicit {
        threads.insert(thread.clone());
    }
    let threads = threads.into_iter().collect::<Vec<_>>();
    status::validate_ids(&threads)?;
    text_bytes = text_bytes.saturating_add(threads.iter().map(String::len).sum::<usize>());
    if text_bytes > status::TEXT_BYTES {
        return Err(bad(
            "Captured list/query/dependency metadata exceeds channel",
        ));
    }
    Ok(CapturedScope {
        command: command.clone(),
        original,
        ids,
        work,
        compact,
        threads,
        route,
        text_bytes,
    })
}

pub(super) struct RawLists {
    scope: CapturedScope,
    current: Body,
    membership: Option<retained::Raw>,
    status_membership: Option<status::Membership>,
    text_bytes: usize,
}
enum Body {
    Status(status::Raw),
    Shell(retained::Raw),
    Compact(retained::Raw),
    Threads(Vec<super::browser_read::RawPatch>, Vec<String>),
}
/// This is a preflight for the opaque F reader, not another summary projection.
/// Identity/optional F scalar allowance is conservative; rejected IDs stay
/// explicitly unknown and are never passed to the copying F reader.
const THREAD_PREFLIGHT: &str = "SELECT t.id,octet_length(t.title)+octet_length(t.cwd)+octet_length(t.id)+COALESCE(octet_length(t.parent_thread_id),0)+octet_length(t.updated_at) FROM json_each(?1)j CROSS JOIN thread t ON t.id=j.value WHERE t.deleted_at IS NULL";
pub(super) fn read_list_facts_tx(
    db: &Connection,
    scope: CapturedScope,
) -> Result<RawLists, StorageError> {
    let mut budget = status::TextBudget {
        used: scope
            .text_bytes
            .saturating_add(64 * 1024)
            .min(status::TEXT_BYTES),
    };
    let mut membership = None;
    let mut status_membership = None;
    let current = match scope.route {
        Route::Status => {
            let raw = status::read_status_tx(db, &scope.work, &mut budget)?;
            status_membership = Some(status::read_membership_tx(db, &scope.threads, &mut budget)?);
            Body::Status(raw)
        }
        Route::Shell => {
            let raw = retained::read_shells_tx(db, &scope.ids, &mut budget)?;
            // Same bounded native active-owner prefix as Status, only one SELECT.
            if !scope.threads.is_empty() {
                let sql =
                    status::prefix_sql(status::Kind::Shell, status::PREFIX / scope.threads.len());
                let mut statement = db.prepare(&sql)?;
                let mut rows = statement
                    .query([serde_json::to_string(&scope.threads).expect("bounded Threads")])?;
                let mut candidate = retained::Raw {
                    unconfirmed: true,
                    statements: 1,
                    ..Default::default()
                };
                while let Some(row) = rows.next()? {
                    if let Some(id) = row
                        .get_ref(1)?
                        .as_str_or_null()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?
                    {
                        if id.len() <= 1024 && budget.take(id.len()) {
                            candidate.rows.push(json!({"execution_id":id}).to_string());
                        } else {
                            candidate.limited = true;
                        }
                    } else {
                        candidate.limited = true;
                    }
                }
                membership = Some(candidate);
            }
            Body::Shell(raw)
        }
        Route::Compact => {
            let raw = retained::read_compactions_tx(db, &scope.compact, &mut budget)?;
            membership = Some(retained::read_compact_membership_tx(
                db,
                &scope.threads,
                &mut budget,
            )?);
            Body::Compact(raw)
        }
        Route::Thread => {
            let mut allowed = Vec::new();
            let mut preflight_found = BTreeSet::new();
            let mut unconfirmed = Vec::new();
            let mut st = db.prepare(THREAD_PREFLIGHT)?;
            let mut rows = st.query([serde_json::to_string(&scope.ids).expect("bounded IDs")])?;
            while let Some(row) = rows.next()? {
                let id = row
                    .get_ref(0)?
                    .as_str()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                preflight_found.insert(id.to_owned());
                let bytes = row.get::<_, i64>(1)?.max(0) as usize;
                if bytes > 64 * 1024 || !budget.take(bytes.saturating_add(8192)) {
                    unconfirmed.push(id.into());
                } else {
                    allowed.push(id.to_owned());
                }
            }
            unconfirmed.extend(
                scope
                    .ids
                    .iter()
                    .filter(|id| !preflight_found.contains(id.as_str()))
                    .cloned(),
            );
            let mut chunks = Vec::new();
            for chunk in allowed.chunks(128) {
                let summaries = crate::control::db::thread_summary::read_ids_tx(db, chunk)?;
                let found = summaries
                    .iter()
                    .map(|r| r.thread_id.as_str())
                    .collect::<BTreeSet<_>>();
                let missing = chunk
                    .iter()
                    .filter(|id| !found.contains(id.as_str()))
                    .cloned()
                    .collect();
                chunks.push(super::browser_read::read_shared_tx(
                    db, summaries, missing, None,
                )?);
            }
            Body::Threads(chunks, unconfirmed)
        }
    };
    let text_bytes = budget.used;
    Ok(RawLists {
        scope,
        current,
        membership,
        status_membership,
        text_bytes,
    })
}
/// Pure output. G maps this to PreparedInspection and owns all adoption fences.
pub(super) fn prepare_list_facts(raw: RawLists) -> Result<Value, ApiError> {
    let current = match raw.current {
        Body::Status(value) => status::prepare(value),
        Body::Shell(value) | Body::Compact(value) => retained::prepare(value)?,
        Body::Threads(chunks, unconfirmed) => {
            let mut rows = Vec::new();
            let mut missing = Vec::new();
            let mut limited = unconfirmed;
            for chunk in chunks {
                let prepared = super::browser_read::prepare_with_watch(chunk, false);
                rows.extend(prepared.rows);
                missing.extend(prepared.missing_ids);
                limited.extend(prepared.limited_title_ids);
            }
            json!({"items":rows,"missing_ids":missing,"unconfirmed_ids":limited,"new_results_unconfirmed":true})
        }
    };
    let membership = raw.membership.map(retained::prepare).transpose()?;
    let status_membership=raw.status_membership.map(|m|json!({"potential_ids":m.candidates.into_iter().map(|k|json!({"kind":k.kind.name(),"id":k.id})).collect::<Vec<_>>(),"unconfirmed":m.unconfirmed,"limited":m.limited,"sql_statements":m.statements,"prefix_rows":m.prefix_rows}));
    Ok(
        json!({"selection_scope":{"query":raw.scope.command,"retained_ids":raw.scope.ids,"notification_threads":raw.scope.threads},"captured_next_cursor":raw.scope.original.get("next_cursor"),"current":current,"membership":membership,"status_membership":status_membership,"metadata_text_bytes":raw.text_bytes,"notice":"Captured filters/page/window/cursor and selection remain fixed; missing or unconfirmed source is not a deletion or an empty global result"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_preserves_exact_cursor_filters_and_rejects_unknown_shapes() {
        let args: ShellListArgs =
            serde_json::from_value(json!({"thread_id":"scope","cursor":"opaque","limit":17}))
                .unwrap();
        let command = Command::ShellList(args);
        let value = Arc::new(json!({"items":[{"execution_id":"s"}],"next_cursor":"next"}));
        let scope = capture_list_scope(&command, value.clone(), &[]).unwrap();
        assert_eq!(scope.command, command);
        assert!(Arc::ptr_eq(&scope.original, &value));
        assert_eq!(scope.ids, vec!["s"]);
        assert!(capture_list_scope(&command, Arc::new(json!({"not_items":[]})), &[]).is_err());
    }
    #[test]
    fn all_four_routes_keep_canonical_row_identity_and_original_window() {
        let cases = [
            (
                Command::Status(StatusArgs::default()),
                json!({"items":[{"kind":"run","id":"r"}],"next_cursor":"status-next"}),
                Route::Status,
                "r",
            ),
            (
                Command::ThreadList(
                    serde_json::from_value(json!({"query":"filter","limit":17})).unwrap(),
                ),
                json!({"items":[{"thread_id":"t"}],"next_cursor":"thread-next"}),
                Route::Thread,
                "t",
            ),
            (
                Command::ShellList(
                    serde_json::from_value(json!({"state":"running","limit":17})).unwrap(),
                ),
                json!({"items":[{"execution_id":"s"}],"next_cursor":"shell-next"}),
                Route::Shell,
                "s",
            ),
            (
                Command::CompactList(
                    serde_json::from_value(json!({"thread_id":"t","limit":17})).unwrap(),
                ),
                json!({"items":[{"event_id":"n","record_kind":"retained_replacement"}]}),
                Route::Compact,
                "n",
            ),
        ];
        for (command, value, route, id) in cases {
            let value = Arc::new(value);
            let scope = capture_list_scope(&command, value.clone(), &[]).unwrap();
            assert_eq!(scope.route, route);
            assert_eq!(scope.ids, [id]);
            assert_eq!(scope.command, command);
            assert!(Arc::ptr_eq(&scope.original, &value));
        }
    }
    #[test]
    fn global_scope_does_not_invent_a_viewer_thread_or_hide_invalid_kinds() {
        let command = Command::Status(StatusArgs::default());
        let scope = capture_list_scope(&command, Arc::new(json!({"items":[]})), &[]).unwrap();
        assert!(scope.threads.is_empty());
        assert!(
            capture_list_scope(
                &command,
                Arc::new(json!({"items":[{"kind":"owned-work-widget","id":"r"}]})),
                &[]
            )
            .is_err()
        );
    }
    #[test]
    fn capture_checks_text_and_structure_before_cloning() {
        let command = Command::ThreadList(serde_json::from_value(json!({})).unwrap());
        let value =
            Arc::new(json!({"items":[{"thread_id":"one","title":"x".repeat(status::TEXT_BYTES)}]}));
        assert!(capture_list_scope(&command, value, &[]).is_err());
    }
}
