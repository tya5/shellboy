//! Display-only stream evidence. Original node text/items remain immutable.
use super::presentation_work;
use crate::{
    control::{
        Control,
        output::{OutputCursor, ShellOutputReader, StreamSelection},
    },
    presentation::{
        BlockKind, OutputStream, PresentationBlock, PresentationField, PresentationKind,
    },
    protocol::*,
    ui::UiNode,
};
use tokio::time::{Duration, Instant};

pub(super) async fn enrich(
    control: &Control,
    nodes: &mut [UiNode],
    width: u16,
) -> Result<(), ApiError> {
    super::tools::enrich(control, nodes).await?;
    super::recovery::enrich(control, nodes).await?;
    super::usage::enrich(control, nodes).await?;
    // One history page has one finite I/O allowance, not 50 independent deadlines.
    let deadline = Instant::now()
        + Duration::from_millis(control.store.config.io.file_prepare_timeout_ms.min(1000));
    let reader = ShellOutputReader::new(
        control.store.database.clone(),
        control.store.instance_id.clone(),
    );
    let budget = (control.store.config.limits.tool_excerpt_bytes / 2).clamp(1, 8192) as u32;
    for node in nodes.iter_mut() {
        if node.presentation.kind != PresentationKind::ToolResult {
            continue;
        }
        if node
            .presentation
            .blocks
            .iter()
            .filter_map(|block| block.tool.as_ref())
            .any(|tool| {
                matches!(tool.status.as_str(), "cancelled" | "error") && tool.execution_id.is_none()
            })
        {
            continue;
        }
        let tool_blocks = node
            .presentation
            .blocks
            .iter()
            .filter(|block| block.kind == BlockKind::ToolResult)
            .collect::<Vec<_>>();
        if !tool_blocks.is_empty()
            && tool_blocks.iter().all(|block| {
                matches!(
                    block.stream,
                    Some(OutputStream::Stdout | OutputStream::Stderr)
                )
            })
        {
            continue;
        }
        let execution = node
            .presentation
            .fields
            .iter()
            .find(|field| field.key.rsplit('/').next() == Some("execution_id"))
            .map(|field| field.value.clone());
        let result = if let Some(execution) = execution {
            let read = async {
                let args = || ShellReadArgs {
                    execution_id: execution.clone(),
                    cursor: None,
                    encoding: None,
                    limit_bytes: budget,
                };
                let stdout = reader.read_stream(args(), StreamSelection::Stdout).await?;
                let stderr = reader.read_stream(args(), StreamSelection::Stderr).await?;
                let a = OutputCursor::decode(&stdout.cursor)?;
                let b = OutputCursor::decode(&stderr.cursor)?;
                if a.retention_generation != b.retention_generation || a.encoding != b.encoding {
                    return Err(ApiError::new(
                        ErrorCode::CursorExpired,
                        "Raw output changed during display preparation",
                    ));
                }
                Ok((stdout, stderr))
            };
            match tokio::time::timeout_at(deadline, read).await {
                Ok(result) => result,
                Err(_) => Err(ApiError::new(
                    ErrorCode::IoFailed,
                    "Stream preview deadline reached",
                )),
            }
        } else {
            Err(ApiError::new(
                ErrorCode::TargetUnavailable,
                "No execution reference",
            ))
        };
        if let Err(error) = &result
            && error.is_storage_failure()
        {
            return Err(error.clone());
        }
        let presentation = std::mem::take(&mut node.presentation);
        node.presentation=presentation_work(move || {
            let mut p=presentation;
            let note=match result {
                Ok((stdout,stderr))=>{
                    let tool=p.blocks.iter().find(|b|b.kind==BlockKind::ToolResult).and_then(|b|b.tool.clone());
                    let span=p.blocks.iter().find(|b|b.kind==BlockKind::ToolResult).map(|b|(b.source_start,b.source_end)).unwrap_or((0,0));
                    p.blocks.retain(|b|b.kind!=BlockKind::ToolResult);
                    p.body_omitted_chars=0;
                    p.body_extent_unknown=false;
                    let incomplete=!stdout.output_complete||!stderr.output_complete;
                    let decode=stdout.decode_errors||stderr.decode_errors;
                    for (output,stream) in [(stdout,OutputStream::Stdout),(stderr,OutputStream::Stderr)] {
                        let end = output.text.floor_char_boundary(output.text.len().min(budget as usize));
                        let (_,full_links)=crate::presentation::sanitize_plain(&output.text,0);
                        let (text,links)=crate::presentation::sanitize_plain(&output.text[..end],0);
                        for mut link in links {
                            // A cut bare URL must never become a different copy target. Raw
                            // offsets are evidence for clipping only, never canonical offsets.
                            let complete=full_links.iter().any(|full|full.url==link.url&&(output.output_complete||output.text.chars().skip(full.source_end).any(|c|c.is_whitespace()||c.is_control())));
                            if complete&&!p.links.iter().any(|old|old.url==link.url&&old.source_start==span.0) {
                                if p.links.len()<128 {link.source_start=span.0;link.source_end=span.1;p.links.push(link);}else{p.details_omitted=true;p.metadata_limited=true;}
                            }
                        }
                        p.body_omitted_chars=p.body_omitted_chars.saturating_add(output.text[end..].chars().count());
                        p.body_extent_unknown|=!output.output_complete;
                        p.details_omitted|=end<output.text.len()||incomplete;
                        if !text.is_empty() {p.blocks.push(PresentationBlock{kind:BlockKind::ToolResult,stream:Some(stream),tool:tool.clone(),text,source_start:span.0,source_end:span.1,source_exact:false,markdown:vec![],rendered:None});}
                    }
                    if !p.blocks.iter().any(|b|b.kind==BlockKind::ToolResult) {
                        p.blocks.push(PresentationBlock{kind:BlockKind::ToolResult,stream:Some(OutputStream::Combined),tool,text:String::new(),source_start:span.0,source_end:span.1,source_exact:false,markdown:vec![],rendered:None});
                    }
                    p.details_omitted=p.metadata_limited||p.body_omitted_chars>0||p.body_extent_unknown;
                    format!("保持中rawの表示用先頭抜粋（API保存本文と別）。{}{}",if incomplete{"続き/未回収部分あり。shell原文で取得。"}else{"stdout/stderr末尾まで取得。"},if decode{"文字変換エラーあり。"}else{""})
                }
                Err(error)=>{
                    p.fields.insert(0,PresentationField{value_omitted:false,display:None,key:"/ui_raw_stream_preview_error".into(),label:"出力の表示取得エラー".into(),value:format!("{:?}",error.code)});
                    for block in &mut p.blocks {if block.kind==BlockKind::ToolResult&&!matches!(block.stream,Some(OutputStream::Stdout|OutputStream::Stderr)) {block.stream=Some(OutputStream::Combined);}}
                    p.details_omitted=true;
                    p.body_extent_unknown=true;
                    format!("rawのstream識別を取得できません（{:?}）。保存済みstream情報を維持し、未識別部分は統合抜粋を表示。",error.code)
                }
            };
            p.fields.insert(0,PresentationField{value_omitted:false,display:None,key:"/ui_raw_stream_preview".into(),label:"出力の表示範囲".into(),value:note});
            crate::presentation::render_markdown(&mut p,width.max(1));p
        }).await?;
    }
    // Move one finite page into the existing worker; no body clone on UI or
    // per-node worker task. Seal only after every dynamic enrichment is final.
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
    use crate::{app::DataOwner, presentation::NodePresentation};
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preview_deadline_sets_a_typed_transient_error_without_losing_canonical_text() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "[io]\nfile_prepare_timeout_ms=10\n").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let db = owner.db.clone();
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let blocked = tokio::spawn(async move {
            db.call(move |_| {
                let _ = entered.send(());
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .await
        });
        entered_rx.await.unwrap();
        let canonical = "saved canonical preview";
        let mut nodes = vec![UiNode {
            node_id: "fixture-node".into(),
            role: "tool".into(),
            text: canonical.into(),
            base_char_offset: 0,
            total_chars: canonical.chars().count(),
            reasoning_ranges: vec![],
            presentation: NodePresentation {
                kind: PresentationKind::ToolResult,
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    text: canonical.into(),
                    ..Default::default()
                }],
                fields: vec![PresentationField {
                    value_omitted: false,
                    display: None,
                    key: "/execution_id".into(),
                    label: "execution".into(),
                    value: "fixture-execution".into(),
                }],
                ..Default::default()
            },
        }];
        let result =
            tokio::time::timeout(Duration::from_secs(3), enrich(&control, &mut nodes, 80)).await;
        release.send(()).unwrap();
        blocked.await.unwrap().unwrap();
        result.unwrap().unwrap();
        assert_eq!(nodes[0].text, canonical);
        assert!(
            nodes[0]
                .presentation
                .fields
                .iter()
                .any(|field| field.key == "/ui_raw_stream_preview_error"
                    && field.value == "IoFailed")
        );
        assert!(
            nodes[0]
                .presentation
                .fields
                .iter()
                .any(|field| field.key == "/ui_raw_stream_preview"
                    && field.value.contains("IoFailed"))
        );
        assert_eq!(
            nodes[0].presentation.blocks[0].stream,
            Some(OutputStream::Combined)
        );
        owner.shutdown().await.unwrap();
    }
}
