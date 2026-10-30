//! Display-only tails. Original node text/items and CLI raw reads remain immutable.
use super::presentation_work;
use crate::{
    control::{
        Control,
        output::preview::{PreviewLimits, PreviewPage, preview_tail},
    },
    presentation::{BlockKind, OutputStream, PresentationBlock, PresentationField},
    protocol::*,
    ui::UiNode,
};
use tokio::time::{Duration, Instant};

/// Preview-only terminal current-line overwrite. The input has already been
/// byte-bounded; no terminal execution or unbounded scrollback is involved.
fn normalize(text: &str, budget: usize) -> String {
    let mut out = String::new();
    let mut line = Vec::new();
    let mut column = 0;
    // Strip terminal control sequences per CR-delimited segment so invisible
    // ANSI bytes cannot consume display columns during overwrite handling.
    for (part, raw) in text.split('\r').enumerate() {
        if part != 0 {
            column = 0;
        }
        let (safe, _) = crate::presentation::sanitize_plain(raw, 0);
        for ch in safe.chars() {
            match ch {
                '\n' => {
                    out.extend(line.drain(..));
                    out.push('\n');
                    column = 0;
                }
                '\u{00ad}'
                | '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}' => {}
                _ => {
                    if column < line.len() {
                        line[column] = ch;
                    } else {
                        line.push(ch);
                    }
                    column += 1;
                }
            }
        }
    }
    out.extend(line);
    let (mut text, _) = crate::presentation::sanitize_plain(&out, 0);
    if text.len() > budget {
        let start = text.ceil_char_boundary(text.len() - budget);
        text.drain(..start);
    }
    text
}

fn normalize_page(page: &mut PreviewPage, budget: usize) {
    // Reserve stderr first, with all decoded text sharing one total allowance.
    page.stderr = normalize(&page.stderr, budget);
    page.stdout = normalize(&page.stdout, budget.saturating_sub(page.stderr.len()));
}

async fn execution_preview(
    control: &Control,
    execution: String,
    limit: usize,
) -> Result<PreviewPage, ApiError> {
    let path = control.store.database.path().to_path_buf();
    let limit = limit.clamp(
        1,
        control
            .store
            .config
            .limits
            .tool_excerpt_bytes
            .min(16 * 1024) as usize,
    );
    presentation_work(move || {
        let db =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|error| {
                    crate::storage::save_error(format!("preview database open failed: {error}"))
                })?;
        db.busy_timeout(std::time::Duration::from_millis(250))
            .and_then(|_| db.pragma_update(None, "query_only", true))
            .map_err(|error| {
                crate::storage::save_error(format!("preview read-only setup failed: {error}"))
            })?;
        let mut page = preview_tail(&db, &execution, PreviewLimits { bytes: limit })?;
        if page.raw_bytes > limit || page.inspected_rows > 128 {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Preview exceeded its bounded read allowance",
            ));
        }
        normalize_page(&mut page, limit);
        Ok(page)
    })
    .await?
}

/// Called only by the existing admitted frontend reader, never by the UI loop.
pub(super) async fn preview(
    control: &Control,
    source_node: &str,
    group: Option<&str>,
    call: &str,
    limit: usize,
) -> Result<PreviewPage, ApiError> {
    let execution = super::tools::preview_source(control, source_node, group, call)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::TargetUnavailable,
                "Saved tool has no shell execution",
            )
        })?;
    execution_preview(control, execution, limit).await
}

pub(super) async fn enrich(
    control: &Control,
    nodes: &mut [UiNode],
    width: u16,
) -> Result<(), ApiError> {
    super::tools::enrich(control, nodes).await?;
    super::recovery::enrich(control, nodes).await?;
    super::usage::enrich(control, nodes).await?;
    enrich_previews(control, nodes, width).await
}

/// Existing paused-tool refresh shares the same finite raw-preview work without
/// reloading message bodies or unrelated recovery/usage metadata.
pub(super) async fn enrich_previews(
    control: &Control,
    nodes: &mut [UiNode],
    width: u16,
) -> Result<(), ApiError> {
    enrich_previews_inner(control, nodes, width, None).await
}

/// Facts and source projection belong to the same admitted read transaction.
/// Reuse that exact correlation without another DB-worker source lookup.
pub(super) async fn enrich_previews_observed(
    control: &Control,
    nodes: &mut [UiNode],
    width: u16,
    facts: &super::tools::ToolFacts,
) -> Result<(), ApiError> {
    enrich_previews_inner(control, nodes, width, Some(facts)).await
}

async fn enrich_previews_inner(
    control: &Control,
    nodes: &mut [UiNode],
    width: u16,
    observed: Option<&super::tools::ToolFacts>,
) -> Result<(), ApiError> {
    let deadline = Instant::now()
        + Duration::from_millis(control.store.config.io.file_prepare_timeout_ms.min(1000));
    for node in nodes.iter_mut() {
        let mut seen = std::collections::HashSet::new();
        let failed = node
            .presentation
            .blocks
            .iter()
            .filter_map(|block| block.tool.as_ref())
            .filter(|tool| tool.execution_id.is_some() && tool.requires_attention())
            .filter(|tool| seen.insert((tool.group_id.clone(), tool.call_id.clone())))
            .map(|tool| {
                (
                    tool.group_id.clone(),
                    tool.call_id.clone(),
                    tool.execution_id.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (group, call, expected_execution) in failed {
            let result = match tokio::time::timeout_at(deadline, async {
                match observed {
                    None => preview(control, &node.node_id, group.as_deref(), &call, 2048).await,
                    Some(facts) => {
                        let execution = observed_execution(
                            facts,
                            &node.node_id,
                            group.as_deref(),
                            &call,
                            expected_execution.as_deref(),
                        )?;
                        execution_preview(control, execution, 2048).await
                    }
                }
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Err(ApiError::new(
                    ErrorCode::IoFailed,
                    "Stream preview deadline reached",
                )),
            };
            if let Err(error) = &result
                && error.is_storage_failure()
            {
                return Err(error.clone());
            }
            let presentation = std::mem::take(&mut node.presentation);
            node.presentation = presentation_work(move || {
                let mut p = presentation;
                apply_preview(&mut p, group.as_deref(), &call, result);
                crate::presentation::render_markdown(&mut p, width.max(1));
                p
            })
            .await?;
        }
    }
    let owned = nodes.iter_mut().map(std::mem::take).collect::<Vec<_>>();
    let sealed = presentation_work(move || {
        let mut owned = owned;
        for node in &mut owned {
            seal_node(node);
        }
        owned
    })
    .await?;
    for (target, node) in nodes.iter_mut().zip(sealed) {
        *target = node;
    }
    Ok(())
}

fn observed_execution(
    facts: &super::tools::ToolFacts,
    node: &str,
    group: Option<&str>,
    call: &str,
    expected_execution: Option<&str>,
) -> Result<String, ApiError> {
    let unavailable = || {
        ApiError::new(
            ErrorCode::TargetUnavailable,
            "Saved tool output source is unavailable or does not match this read",
        )
    };
    let mut matched = facts.sources().iter().filter(|source| {
        source.node_id == node && Some(source.group_id.as_str()) == group && source.call_id == call
    });
    let source = matched.next().ok_or_else(unavailable)?;
    if matched.next().is_some()
        || source.shell_state.is_none()
        || source.execution_id.is_none()
        || source.execution_id.as_deref() != expected_execution
    {
        return Err(unavailable());
    }
    source.execution_id.clone().ok_or_else(unavailable)
}

fn apply_preview(
    p: &mut crate::presentation::NodePresentation,
    group: Option<&str>,
    call: &str,
    result: Result<PreviewPage, ApiError>,
) {
    let matches = |block: &PresentationBlock| {
        block
            .tool
            .as_ref()
            .is_some_and(|tool| tool.group_id.as_deref() == group && tool.call_id == call)
    };
    let marker = format!("/ui_raw_stream_preview/{call}");
    let error_marker = format!("/ui_raw_stream_preview_error/{call}");
    p.fields
        .retain(|field| field.key != marker && field.key != error_marker);
    match result {
        Ok(page) => {
            let Some(source) = p.blocks.iter().find(|block| matches(block)) else {
                return;
            };
            let tool = source.tool.clone();
            let span = (source.source_start, source.source_end);
            let mut replacements = Vec::new();
            for (text, stream) in [
                (page.stdout, OutputStream::Stdout),
                (page.stderr, OutputStream::Stderr),
            ] {
                if !text.is_empty() {
                    replacements.push(PresentationBlock {
                        kind: BlockKind::ToolResult,
                        stream: Some(stream),
                        tool: tool.clone(),
                        text,
                        source_start: span.0,
                        source_end: span.1,
                        source_exact: false,
                        markdown: vec![],
                        rendered: None,
                    });
                }
            }
            if replacements.is_empty() {
                replacements.push(PresentationBlock {
                    kind: BlockKind::ToolResult,
                    stream: Some(OutputStream::Combined),
                    tool,
                    text: String::new(),
                    source_start: span.0,
                    source_end: span.1,
                    source_exact: false,
                    markdown: vec![],
                    rendered: None,
                });
            }
            let old = std::mem::take(&mut p.blocks);
            let mut inserted = false;
            for block in old {
                let matching = matches(&block);
                if !(matching && block.kind == BlockKind::ToolResult) {
                    p.blocks.push(block);
                }
                if matching && !inserted {
                    p.blocks.append(&mut replacements);
                    inserted = true;
                }
            }
            p.body_extent_unknown |= page.skipped || !page.output_complete;
            p.details_omitted |= p.body_extent_unknown;
            p.fields.insert(
                0,
                PresentationField {
                    key: marker,
                    label: "出力の表示範囲".into(),
                    value: format!(
                        "取得 {} UTC · 閉じて開くと再取得 · 保持中rawの末尾抜粋。{}{}{}",
                        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                        if page.skipped {
                            "前方省略あり。"
                        } else {
                            ""
                        },
                        if page.decode_uncertain {
                            "文字境界/状態は不確実。原文を参照。"
                        } else {
                            ""
                        },
                        if page.decode_errors {
                            "文字変換エラーあり。"
                        } else {
                            ""
                        }
                    ),
                    display: None,
                    value_omitted: false,
                },
            );
        }
        Err(error) => {
            p.fields.insert(
                0,
                PresentationField {
                    key: error_marker,
                    label: "出力の表示取得エラー".into(),
                    value: format!(
                        "出力を取得できません（{:?}）。原文を参照するか、項目を開き直してください",
                        error.code
                    ),
                    display: None,
                    value_omitted: false,
                },
            );
            p.details_omitted = true;
        }
    }
}

/// A display cache witness, not persisted identity or provenance. Streaming
/// serialization hashes borrowed strings without allocating a JSON body copy.
pub(super) fn seal_node(node: &mut UiNode) {
    use sha2::{Digest, Sha256};
    struct Hasher(Sha256);
    impl std::io::Write for Hasher {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    node.presentation.projection_stamp = None;
    let mut hasher = Hasher(Sha256::new());
    if serde_json::to_writer(&mut hasher, &*node).is_err() {
        return;
    }
    if serde_json::to_writer(
        &mut hasher,
        &(
            node.presentation.created_at.as_deref(),
            node.presentation.input_source,
        ),
    )
    .is_err()
    {
        return;
    }
    for block in &node.presentation.blocks {
        if let Some(tool) = &block.tool
            && serde_json::to_writer(
                &mut hasher,
                &(&tool.group_id, &tool.call_node_id, &tool.result_node_id),
            )
            .is_err()
        {
            return;
        }
    }
    // Rendered lines are derived deterministically from the serialized blocks,
    // but serde intentionally omits them: width/omission are additional inputs.
    for block in &node.presentation.blocks {
        if serde_json::to_writer(
            &mut hasher,
            &block
                .rendered
                .as_ref()
                .map(|rendered| (rendered.width, rendered.omitted)),
        )
        .is_err()
        {
            return;
        }
    }
    if serde_json::to_writer(&mut hasher, &node.presentation.provisional_stream).is_err() {
        return;
    }
    node.presentation.projection_stamp = Some(hasher.0.finalize().into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::DataOwner,
        presentation::{NodePresentation, PresentationKind, ToolPresentation},
    };

    #[test]
    fn call_only_previews_keep_prose_and_all_failed_call_identities_separate() {
        let call = |id: &str| PresentationBlock {
            kind: BlockKind::ToolCall,
            tool: Some(ToolPresentation {
                group_id: Some("g".into()),
                call_id: id.into(),
                status: "failed".into(),
                execution_id: Some(format!("e-{id}")),
                ..Default::default()
            }),
            ..Default::default()
        };
        let prose = PresentationBlock {
            text: "intermediate prose must stay".into(),
            ..Default::default()
        };
        let mut p = NodePresentation {
            kind: PresentationKind::Assistant,
            blocks: vec![call("a"), prose.clone(), call("b")],
            ..Default::default()
        };
        for id in ["a", "b"] {
            apply_preview(
                &mut p,
                Some("g"),
                id,
                Ok(PreviewPage {
                    stdout: format!("tail-{id}"),
                    stderr: String::new(),
                    raw_bytes: 6,
                    inspected_rows: 1,
                    skipped: true,
                    decode_uncertain: false,
                    decode_errors: false,
                    terminal: true,
                    output_complete: true,
                }),
            );
        }
        assert_eq!(p.kind, PresentationKind::Assistant);
        assert_eq!(
            p.blocks
                .iter()
                .filter(|block| block.kind == BlockKind::ToolCall)
                .count(),
            2
        );
        assert_eq!(p.blocks[2], prose);
        let results = p
            .blocks
            .iter()
            .filter(|block| block.kind == BlockKind::ToolResult)
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool.as_ref().unwrap().call_id, "a");
        assert_eq!(results[0].text, "tail-a");
        assert_eq!(results[1].tool.as_ref().unwrap().call_id, "b");
        assert_eq!(results[1].text, "tail-b");
        for note in p
            .fields
            .iter()
            .filter(|field| field.key.starts_with("/ui_raw_stream_preview/"))
        {
            assert!(
                note.value.starts_with("取得 ") && note.value.contains(" UTC · 閉じて開くと再取得")
            );
        }

        assert!(
            p.fields
                .iter()
                .any(|field| field.key == "/ui_raw_stream_preview/a")
        );
        assert!(
            p.fields
                .iter()
                .any(|field| field.key == "/ui_raw_stream_preview/b")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paused_call_only_refresh_observes_real_failed_execution_tails() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        owner.db.call(|db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now');
                INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');
                INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','u',0,0,'p','m',0,'completed','{}','now');
                INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','attempt','u','completed','now');
                INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('calls','u','assistant_response','now','{\"group_id\":\"g\"}');")?;
            for (ordinal,call) in [(0,"a"),(1,"b")] {
                let execution=format!("exec-{call}");
                db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('g',?1,?2,'shell','{}','completed',?3)",rusqlite::params![call,ordinal,execution])?;
                db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,exit_code,output_complete)VALUES(?1,'r','u',?1,'failed','/','{}','UTF-8','UTF-8','closed','now',7,1)",[&execution])?;
                let output=format!("{}TAIL-{call}","x".repeat(12_000));
                db.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES(?1,0,'stderr',0,?2,'now')",rusqlite::params![execution,output.as_bytes()])?;
            }
            Ok(())
        }).await.unwrap();
        let mut nodes = vec![UiNode {
            node_id: "calls".into(),
            presentation: NodePresentation {
                kind: PresentationKind::Assistant,
                blocks: ["a", "b"]
                    .into_iter()
                    .map(|call| PresentationBlock {
                        kind: BlockKind::ToolCall,
                        tool: Some(ToolPresentation {
                            call_id: call.into(),
                            status: "completed".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
            ..Default::default()
        }];
        super::super::tools::enrich(&control, &mut nodes)
            .await
            .unwrap();
        enrich_previews(&control, &mut nodes, 80).await.unwrap();
        let facts=control.store.database.call(|db| {
            let payload=serde_json::json!({"group_id":"g","output":[{"type":"function_call","call_id":"a"},{"type":"function_call","call_id":"b"}]}).to_string();
            db.execute("UPDATE node SET payload_json=?1 WHERE id='calls'",[payload])?;
            super::super::tools::read_nodes_tx(db,&["calls".into()])
        }).await.unwrap();
        assert_eq!(
            observed_execution(&facts, "calls", Some("g"), "a", Some("exec-a")).unwrap(),
            "exec-a"
        );
        for (node, group, call, execution) in [
            ("unrelated", Some("g"), "a", Some("exec-a")),
            ("calls", Some("other"), "a", Some("exec-a")),
            ("calls", None, "a", Some("exec-a")),
            ("calls", Some("g"), "missing", Some("exec-a")),
            ("calls", Some("g"), "a", Some("exec-b")),
        ] {
            assert_eq!(
                observed_execution(&facts, node, group, call, execution)
                    .unwrap_err()
                    .code,
                ErrorCode::TargetUnavailable
            );
        }
        let ambiguous = control
            .store
            .database
            .call(|db| {
                super::super::tools::read_tx(
                    db,
                    &[("calls".into(), "a".into()), ("calls".into(), "a".into())],
                )
            })
            .await
            .unwrap();
        assert_eq!(
            observed_execution(&ambiguous, "calls", Some("g"), "a", Some("exec-a"))
                .unwrap_err()
                .code,
            ErrorCode::TargetUnavailable
        );
        let missing_shell=control.store.database.call(|db| {
            db.execute("UPDATE tool_call SET operation_id='unavailable-execution' WHERE group_id='g' AND call_id='b'",[])?;
            let facts=super::super::tools::read_tx(db,&[("calls".into(),"b".into())])?;
            db.execute("UPDATE tool_call SET operation_id='exec-b' WHERE group_id='g' AND call_id='b'",[])?;
            Ok(facts)
        }).await.unwrap();
        assert_eq!(
            observed_execution(
                &missing_shell,
                "calls",
                Some("g"),
                "b",
                Some("unavailable-execution")
            )
            .unwrap_err()
            .code,
            ErrorCode::TargetUnavailable
        );
        // Hold the actual DB worker. The observed path must complete through
        // its read-only connection, without a second source-validation job.
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let db = control.store.database.clone();
        let blocked = tokio::spawn(async move {
            db.call(move |_| {
                let _ = entered.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
                Ok(())
            })
            .await
        });
        entered_rx.await.unwrap();
        let observed = enrich_previews_observed(&control, &mut nodes, 80, &facts).await;
        let _ = release.send(());
        blocked.await.unwrap().unwrap();
        observed.unwrap();
        for call in ["a", "b"] {
            assert!(
                nodes[0]
                    .presentation
                    .fields
                    .iter()
                    .any(|field| field.key == format!("/ui_raw_stream_preview/{call}"))
            );
            assert!(
                nodes[0]
                    .presentation
                    .blocks
                    .iter()
                    .any(|block| block.kind == BlockKind::ToolResult
                        && block.text.ends_with(&format!("TAIL-{call}"))
                        && block.tool.as_ref().is_some_and(|tool| tool.call_id == call))
            );
        }
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn carriage_return_preview_keeps_current_line_and_removes_invisible_controls() {
        assert_eq!(
            normalize("10%\r20%\r100%\nnext\rLAST\u{202e}", 2048),
            "100%\nLAST"
        );
        assert_eq!(normalize("prefix😀tail", 5), "tail");
        assert_eq!(
            normalize("\u{1b}[31m10%\r\u{1b}[32m20%\u{1b}[0m", 2048),
            "20%"
        );
        assert_eq!(normalize("long\nsecond", 0), "");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closed_success_and_executionless_cancellation_never_query_raw_output() {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.path().into());
        // A raw query would fail. All metadata enrichment remains available.
        owner
            .db
            .call(|db| {
                db.execute_batch("DROP TABLE output_chunk")?;
                Ok(())
            })
            .await
            .unwrap();
        let mut nodes = vec![];
        for (status, exit_code, execution) in [
            ("exited", Some(0), Some("unread-execution")),
            ("cancelled", None, None),
        ] {
            nodes.push(UiNode {
                node_id: format!("fixture-{status}"),
                role: "tool".into(),
                text: "saved canonical text".into(),
                total_chars: 20,
                presentation: NodePresentation {
                    kind: PresentationKind::ToolResult,
                    read_revision: 19,
                    blocks: vec![PresentationBlock {
                        kind: BlockKind::ToolResult,
                        text: "saved result".into(),
                        tool: Some(ToolPresentation {
                            call_id: "call".into(),
                            status: status.into(),
                            exit_code,
                            execution_id: execution.map(str::to_owned),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        enrich(&control, &mut nodes, 80).await.unwrap();
        enrich_previews_observed(
            &control,
            &mut nodes,
            80,
            &super::super::tools::ToolFacts::default(),
        )
        .await
        .unwrap();
        for node in nodes {
            assert_eq!(node.text, "saved canonical text");
            assert_eq!(node.presentation.read_revision, 19);
            assert!(!node.presentation.body_evicted);
            assert!(
                !node
                    .presentation
                    .fields
                    .iter()
                    .any(|field| field.key.starts_with("/ui_raw_stream_preview"))
            );
        }
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn display_identity_metadata_changes_projection_stamp_without_changing_source() {
        let mut node = UiNode {
            node_id: "n".into(),
            ..Default::default()
        };
        seal_node(&mut node);
        let before = node.presentation.projection_stamp;
        node.presentation.created_at = Some("now".into());
        seal_node(&mut node);
        assert_ne!(before, node.presentation.projection_stamp);
    }
}
