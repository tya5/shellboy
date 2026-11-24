//! Capture only immutable page identities; body retention stays with management.
use crate::{
    protocol::{
        ApiError, Command, ErrorCode, HistoryBody, HistoryGetArgs, HistorySnippet, HistoryWindow,
        Page,
    },
    ui::{HistorySourceScope, InspectionTarget},
};
use serde_json::Value;
pub(super) enum CapturedCommand {
    Full(Box<Command>),
    Limited(super::inspection_retained::RetainedRoute),
}
pub(super) fn capture_command(command: &Command) -> Option<CapturedCommand> {
    if !(matches!(command, Command::HistoryGet(_) | Command::HistoryList(_))
        || super::inspection_retained::supports(command))
    {
        return None;
    }
    // Count borrowed JSON before cloning the request's query/filter strings.
    Some(if super::inspection_retained::request_fits(command) {
        CapturedCommand::Full(Box::new(command.clone()))
    } else {
        CapturedCommand::Limited(super::inspection_retained::route(command))
    })
}
pub(super) fn capture_saved(command: &CapturedCommand, value: &Value) -> Option<InspectionTarget> {
    match command {
        CapturedCommand::Full(command) => capture(command, value),
        CapturedCommand::Limited(route) if value.get("error").is_none() => {
            Some(super::inspection_retained::limited_scope(*route))
        }
        _ => None,
    }
}
pub(super) async fn native_body(
    data_dir: &std::path::Path,
    args: HistoryGetArgs,
) -> Result<HistoryBody, ApiError> {
    native_read(data_dir, Command::HistoryGet(args)).await
}

pub(super) async fn native_read<T: serde::de::DeserializeOwned>(
    data_dir: &std::path::Path,
    command: Command,
) -> Result<T, ApiError> {
    let response = crate::ipc::call_selected(data_dir, command).await?;
    let malformed = || {
        ApiError::new(
            ErrorCode::ProtocolMismatch,
            "Invalid native saved-read response",
        )
    };
    if !response.ok {
        return Err(response.error.unwrap_or_else(malformed));
    }
    serde_json::from_value(response.result.ok_or_else(malformed)?).map_err(|_| malformed())
}

// The report preserves owner-produced Summary parts; Copy only needs the body.
#[derive(serde::Deserialize, serde::Serialize)]
struct HistoryReport {
    #[serde(flatten)]
    body: HistoryBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parts: Option<SummaryParts>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SummaryParts {
    reduce_keys_recorded: bool,
    items: Vec<SummaryPart>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SummaryPart {
    key: String,
    listed_as: String,
    #[serde(flatten)]
    state: SummaryPartState,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum SummaryPartState {
    Retained {
        stage: Option<String>,
        round: Option<u32>,
        sources: Option<SummaryPartSources>,
        characters: usize,
        usage: Option<Value>,
    },
    NotRetained,
    Unreadable,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SummaryPartSources {
    first: String,
    last: String,
    count: usize,
}

pub(super) async fn request(
    data_dir: &std::path::Path,
    command: Command,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Value, crate::protocol::ApiError> {
    tokio::select! { biased;
        _ = stop.cancelled() => Err(ApiError::new(ErrorCode::InvalidState, "Display snapshot observation closed")),
        result = async {
            match command {
                command @ Command::HistoryGet(_) => {
                    let report: HistoryReport = native_read(data_dir, command).await?;
                    super::presentation_work(move || serialize(report)).await?
                }
                command @ Command::HistoryList(_) => {
                    let page: Page<HistorySnippet> = native_read(data_dir, command).await?;
                    super::presentation_work(move || serialize(page)).await?
                }
                command @ Command::HistoryWindow(_) => {
                    let window: HistoryWindow = native_read(data_dir, command).await?;
                    super::presentation_work(move || serialize(window)).await?
                }
                _ => Err(ApiError::new(ErrorCode::InvalidArgument, "Not a retained history read")),
            }
        } => result,
    }
}

fn serialize(value: impl serde::Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|_| {
        ApiError::new(
            ErrorCode::IoFailed,
            "Cannot prepare retained history result",
        )
    })
}
pub(super) fn capture(command: &Command, value: &Value) -> Option<InspectionTarget> {
    if let Some(target) = super::inspection_retained::capture_response(command, value) {
        return Some(target);
    }
    let (rows, thread) = match command {
        Command::HistoryGet(_) => (vec![value], None),
        Command::HistoryList(args) => (
            value.get("items")?.as_array()?.iter().take(201).collect(),
            Some(args.thread_id.as_str()),
        ),
        _ => return None,
    };
    let mut limited = rows.len() > 200 || thread.is_some_and(|id| id.is_empty() || id.len() > 512);
    let mut nodes = Vec::new();
    for row in rows {
        let id = row.get("node_id")?.as_str()?;
        if id.is_empty() || id.len() > 512 {
            limited = true;
            break;
        }
        nodes.push((
            id.to_owned(),
            row.pointer("/provenance/source_revision")
                .and_then(Value::as_i64),
        ));
    }
    if limited {
        nodes.clear();
    }
    Some(InspectionTarget::HistorySources(HistorySourceScope {
        nodes,
        thread_id: thread.filter(|_| !limited).map(str::to_owned),
        captured_at: chrono::Utc::now().to_rfc3339(),
        scope_limited: limited,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::HistoryListArgs;
    use serde_json::json;
    #[tokio::test]
    async fn native_list_inspection_continues_and_rejects_changed_path() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                for node in ["list-a", "list-b", "list-c"] {
                    crate::history::append_node(
                        &tx,
                        "thread",
                        node,
                        None,
                        "user",
                        &json!({"role":"user","content":node}),
                        "2026-11-24T00:00:00Z",
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let read = |cursor| {
            request(
                &owner.loaded.data_dir,
                Command::HistoryList(HistoryListArgs {
                    thread_id: "thread".into(),
                    limit: 1,
                    cursor,
                    excerpt_chars: 32,
                    run_id: None,
                    since: None,
                    state: None,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
        };
        let mut cursor = None;
        let mut first = None;
        for expected in ["list-a", "list-b", "list-c"] {
            let page = read(cursor).await.unwrap();
            let items = page["items"].as_array().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["node_id"], expected);
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if expected == "list-a" {
                first = Some(cursor.clone().unwrap());
            }
        }
        assert!(cursor.is_none());
        owner
            .db
            .call(|db| {
                db.execute(
                    "UPDATE thread SET selection_revision=selection_revision+1 WHERE id='thread'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            read(first).await.unwrap_err().code,
            ErrorCode::CursorExpired
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_summary_report_preserves_part_states_and_body_cursor() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        owner.db.call(|db| {
            let tx = db.transaction()?;
            crate::history::append_node(&tx, "thread", "summary", None, "summary",
                &json!({"summary_part_keys":["kept-part","missing-part","unreadable-part"]}), "now")?;
            tx.execute("INSERT INTO app_meta(key,value) VALUES('kept-part',?1),('unreadable-part','invalid')",
                [json!({"text":"retained body beyond this page", "sources":["source-a","source-b"], "stage":"map"}).to_string()])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let args = HistoryGetArgs {
            node_id: "summary".into(),
            cursor: None,
            limit_bytes: 32,
            encoding: "UTF-8".into(),
            end_message_only: false,
            parts: true,
        };
        let read = |args| {
            request(
                &owner.loaded.data_dir,
                Command::HistoryGet(args),
                tokio_util::sync::CancellationToken::new(),
            )
        };
        let report = read(args.clone()).await.unwrap();
        assert_eq!(
            report["parts"],
            json!({"reduce_keys_recorded":false,"items":[
                {"key":"kept-part","listed_as":"map","state":"retained","stage":"map","round":null,
                 "sources":{"first":"source-a","last":"source-b","count":2},"characters":30,"usage":null},
                {"key":"missing-part","listed_as":"map","state":"not_retained"},
                {"key":"unreadable-part","listed_as":"map","state":"unreadable"}
            ]})
        );
        let cursor = report["next_cursor"].as_str().unwrap().to_owned();
        let next = read(HistoryGetArgs {
            cursor: Some(cursor),
            ..args.clone()
        })
        .await
        .unwrap();
        assert_eq!(next["parts"], report["parts"]);
        assert_ne!(next["text"], report["text"]);
        assert!(
            read(HistoryGetArgs {
                parts: false,
                ..args
            })
            .await
            .unwrap()
            .get("parts")
            .is_none()
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_body_inspection_continues_and_rejects_changed_revision() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                crate::history::append_node(
                    &tx,
                    "thread",
                    "inspection-source",
                    None,
                    "user",
                    &json!({"role":"user","content":"abcdefgh"}),
                    "2026-11-24T00:00:00Z",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let read = |cursor| {
            request(
                &owner.loaded.data_dir,
                Command::HistoryGet(HistoryGetArgs {
                    node_id: "inspection-source".into(),
                    cursor,
                    limit_bytes: 4,
                    encoding: "UTF-8".into(),
                    end_message_only: false,
                    parts: false,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
        };
        let first = read(None).await.unwrap();
        assert_eq!(first["text"], "abcd");
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        let second = read(Some(cursor.clone())).await.unwrap();
        assert_eq!(second["text"], "efgh");
        assert!(second.get("next_cursor").is_none());
        owner.db.call(|db| {
            db.execute("UPDATE node SET payload_revision=payload_revision+1 WHERE id='inspection-source'", [])?;
            Ok(())
        }).await.unwrap();
        assert_eq!(
            read(Some(cursor)).await.unwrap_err().code,
            ErrorCode::CursorExpired
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_window_report_keeps_source_budget_omission() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                crate::history::append_node(
                    &tx,
                    "thread",
                    "omitted-source",
                    None,
                    "user",
                    &json!({"role":"user","content":"x".repeat(1024 * 1024)}),
                    "2026-11-24T00:00:00Z",
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let report = request(
            &owner.loaded.data_dir,
            Command::HistoryWindow(crate::protocol::HistoryWindowArgs {
                thread_id: "thread".into(),
                ordinal: None,
                node_id: Some("omitted-source".into()),
                expect_node_id: None,
                before: 0,
                after: 0,
                excerpt_chars: 0,
                items: true,
            }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let items = report["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["node_id"], "omitted-source");
        assert_eq!(items[0]["items"][0]["reason"], "source_budget");
        assert!(items[0]["payload_chars"].is_null());
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn body_identity_never_infers_selection_from_creation_or_viewer_and_limits_do_not_copy_bad_ids()
    {
        let command = Command::HistoryGet(HistoryGetArgs {
            node_id: "n".into(),
            cursor: None,
            limit_bytes: 65536,
            encoding: "UTF-8".into(),
            end_message_only: false,
            parts: false,
        });
        let InspectionTarget::HistorySources(scope)=capture(&command,&json!({"node_id":"n","provenance":{"source_revision":7,"origin_thread_id":"creation"},"text":"retained"})).unwrap() else {panic!()};
        assert_eq!(scope.nodes, vec![("n".into(), Some(7))]);
        assert!(scope.thread_id.is_none());
        assert!(!scope.scope_limited);
        let InspectionTarget::HistorySources(scope) =
            capture(&command, &json!({"node_id":"x".repeat(513)})).unwrap()
        else {
            panic!()
        };
        assert!(scope.scope_limited && scope.nodes.is_empty());
        assert!(capture(&command, &json!({"error":{"message":"absent"}})).is_none());
    }
    #[test]
    fn empty_list_still_captures_its_literal_thread_without_a_fabricated_revision() {
        let command = Command::HistoryList(HistoryListArgs {
            thread_id: "foreign".into(),
            excerpt_chars: 512,
            limit: 50,
            cursor: None,
            run_id: None,
            since: None,
            state: None,
        });
        let InspectionTarget::HistorySources(scope) =
            capture(&command, &json!({"items":[]})).unwrap()
        else {
            panic!()
        };
        assert_eq!(scope.thread_id.as_deref(), Some("foreign"));
        assert!(scope.nodes.is_empty());
    }
}
