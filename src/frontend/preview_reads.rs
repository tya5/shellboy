//! Untimed, bounded preview preparation in the existing frontend read workers.
use crate::{config::UiLanguage, protocol::*};

pub(super) struct Source<'a> {
    pub thread: &'a str,
    pub node: &'a str,
    pub group: Option<&'a str>,
    pub call: &'a str,
}

pub(super) async fn load(
    owner: super::native_history::Owner<'_>,
    source: Source<'_>,
    limit: usize,
    language: UiLanguage,
) -> Result<crate::ui::ToolPreview, String> {
    let result = async {
        let window = owner
            .window(HistoryWindowArgs {
                thread_id: source.thread.to_owned(),
                include_observation: true,
                observe_node_ids: vec![source.node.to_owned()],
                items: false,
                ordinal: None,
                node_id: None,
                expect_node_id: None,
                before: 0,
                after: 0,
                excerpt_chars: 0,
            })
            .await?;
        let observation = window
            .observation
            .ok_or_else(|| ApiError::new(ErrorCode::IoFailed, "Missing selected observation"))?;
        let unavailable = || {
            ApiError::new(
                ErrorCode::TargetUnavailable,
                "Saved tool output is unavailable",
            )
        };
        let mut matches = observation.tools.calls.iter().filter(|call| {
            call.node_id == source.node
                && call.call_id == source.call
                && source.group.is_none_or(|group| group == call.group_id)
        });
        let call = matches
            .next()
            .filter(|_| matches.next().is_none())
            .ok_or_else(unavailable)?;
        let execution = call.execution_id.as_deref().ok_or_else(unavailable)?;
        let output: ShellOutput = owner
            .call(Command::ShellRead(ShellReadArgs {
                execution_id: execution.to_owned(),
                expected_owner: Some(call.shell_owner.clone().ok_or_else(unavailable)?),
                tail: true,
                segments: true,
                cursor: None,
                encoding: None,
                limit_bytes: limit as u32,
            }))
            .await?;
        let terminal = matches!(
            call.shell_state.as_deref(),
            Some("exited" | "failed" | "stopped")
        );
        let page = super::streams::from_native_tail(execution, output, limit, terminal)?;
        Ok::<_, ApiError>(super::tool_preview(page, language))
    }
    .await;
    result.map_err(|error| error.to_string())
}

#[cfg(test)]
mod cancelled_reader_tests {
    use crate::ui::{ItemKey, PreviewItemState};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    use tokio::{
        sync::{Notify, Semaphore, oneshot},
        task::JoinSet,
    };
    #[tokio::test(start_paused = true)]
    async fn pending_reader_survives_virtual_delay_but_close_releases_its_slot_and_reopen_is_fresh()
    {
        let slots = Arc::new(Semaphore::new(1));
        let mut workers = JoinSet::new();
        let mut items = PreviewItemState::default();
        let key = ItemKey::Node("one".into());
        let gate = Arc::new(Notify::new());
        let done = Arc::new(AtomicBool::new(false));
        items.request(&key, "outcome");
        let token = items.preview_cancel(&key).unwrap();
        let (entered, wait) = oneshot::channel();
        let (release, completed) = (gate.clone(), done.clone());
        assert!(crate::frontend::admission::spawn_read(
            &mut workers,
            &slots,
            &token,
            async move {
                entered.send(()).unwrap();
                release.notified().await;
                completed.store(true, Ordering::SeqCst);
            }
        ));
        wait.await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(!done.load(Ordering::SeqCst));
        assert_eq!(slots.available_permits(), 0);
        items.cancel_request(&key);
        workers.join_next().await.unwrap().unwrap();
        assert_eq!(slots.available_permits(), 1);
        assert!(!done.load(Ordering::SeqCst));
        items.request(&key, "outcome");
        let token = items.preview_cancel(&key).unwrap();
        let (release, completed) = (gate.clone(), done.clone());
        assert!(crate::frontend::admission::spawn_read(
            &mut workers,
            &slots,
            &token,
            async move {
                release.notified().await;
                completed.store(true, Ordering::SeqCst);
            }
        ));
        gate.notify_one();
        workers.join_next().await.unwrap().unwrap();
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(slots.available_permits(), 1);
    }
}

#[cfg(test)]
mod terminal_refresh_tests {
    use crate::ui::UiNode;
    use serde_json::json;
    #[tokio::test]
    async fn repeated_failed_metadata_refreshes_do_not_read_raw_or_mint_observation_notices() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        owner.db.call(|db| {
            db.execute_batch("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='target-run'),'no-raw-attempt','target-run',0,0,'fixture','fixture-model',0,'completed','{}','now');
              INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('no-raw-group','no-raw-attempt','target-run','completed','now');
              INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('no-raw-node','target-run','tool_result','now','{\"group_id\":\"no-raw-group\",\"item\":{\"type\":\"function_call_output\",\"call_id\":\"one\",\"output\":\"{\\\"status\\\":\\\"exited\\\",\\\"exit_code\\\":1}\"}}');
              INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,exit_code,output_complete,started_at)VALUES('no-raw-exec','target-run','target-run','no-raw-exec','exited','/','{}','UTF-8','UTF-8','closed',1,1,'now');
              INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('no-raw-group','one',0,'shell','{}','completed','no-raw-exec');
              INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('no-raw-group','one','no-raw-node','exited');
              DROP TABLE output_chunk;")?;
            Ok(())
        }).await.unwrap();
        let payload = json!({"group_id":"no-raw-group","item":{"type":"function_call_output","call_id":"one","output":"{\"status\":\"exited\",\"exit_code\":1}"}});
        let text = crate::history::text_of_payload(&payload);
        let original = UiNode {
            node_id: "no-raw-node".into(),
            role: "tool".into(),
            text: text.clone(),
            total_chars: text.chars().count(),
            presentation: crate::presentation::project("tool_result", &payload, &text, 0),
            ..Default::default()
        };
        let mut stamp = None;
        for _ in 0..3 {
            let mut nodes = vec![original.clone()];
            crate::frontend::window_facts::apply_saved(
                &control,
                &mut nodes,
                crate::config::UiLanguage::Ja,
            )
            .await
            .unwrap();
            super::super::streams::seal_nodes(&mut nodes).await.unwrap();
            assert!(nodes[0].presentation.blocks.iter().any(|block| {
                block
                    .tool
                    .as_ref()
                    .is_some_and(|tool| tool.requires_attention())
            }));
            assert!(
                !nodes[0]
                    .presentation
                    .fields
                    .iter()
                    .any(|field| field.key.starts_with("/ui_raw_stream_preview"))
            );
            if let Some(previous) = stamp {
                assert_eq!(nodes[0].presentation.projection_stamp, Some(previous));
            }
            stamp = nodes[0].presentation.projection_stamp;
        }
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
