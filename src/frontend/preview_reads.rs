//! Untimed, bounded preview preparation in the existing frontend read workers.
pub(super) async fn load(
    control: &crate::control::Control,
    source: &str,
    group: Option<&str>,
    call: &str,
    limit: usize,
) -> Result<crate::ui::ToolPreview, String> {
    super::streams::preview(control, source, group, call, limit)
        .await
        .map(|page| super::tool_preview(page, control.store.config.ui.language))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod blocked_database_tests {
    use super::*;
    #[tokio::test]
    async fn a_blocked_database_read_stays_pending_after_the_old_deadline_and_can_be_retired() {
        use std::time::Duration;
        use tokio::sync::oneshot;
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let (entered, waiting) = oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        let db = owner.db.clone();
        let blocker = tokio::spawn(async move {
            db.call(move |_| {
                let _ = entered.send(());
                crate::test_watchdog::blocking_recv("release preview database gate", &held)
                    .unwrap();
                Ok(())
            })
            .await
            .unwrap()
        });
        crate::test_watchdog::watchdog("database gate entered", waiting)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        let mut request = Box::pin(load(&control, "missing", None, "call", 16384));
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(
            futures_util::poll!(request.as_mut()).is_pending(),
            "elapsed virtual time is not an I/O failure"
        );
        // Retiring the future leaves the queued read harmless; it exposes no late error.
        drop(request);
        tokio::time::resume();
        release.send(()).unwrap();
        crate::test_watchdog::watchdog("database gate released", blocker)
            .await
            .unwrap()
            .unwrap();
        let genuine = load(&control, "missing", None, "call", 16384).await;
        assert!(genuine.is_err());
        assert!(!genuine.unwrap_err().contains("時間内"));
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
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
            db.execute_batch("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('no-raw-attempt','target-run',0,0,'fixture','fixture-model',0,'completed','{}','now');
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
            super::super::streams::enrich(&control, &mut nodes, 80)
                .await
                .unwrap();
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
