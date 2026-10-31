//! Owner memory observation uses existing UI delivery and control-file admission.
use super::*;
impl Control {
    pub(super) async fn memory_snapshot(
        self: &Arc<Self>,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        let ui = self.memory_ui_snapshot(disconnected.clone()).await;
        if disconnected.is_cancelled() || self.stop.is_cancelled() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Memory observation cancelled",
            ));
        }
        let control = self.clone();
        self.instructions.file_work(move || {
            #[cfg(test)]
            control.memory_samples.fetch_add(1, Ordering::SeqCst);
            let config = &control.store.config.cache;
            let attachment = control.attachments.cache_stats();
            let replay = control.replay_cache_stats();
            let mut caches = ui.unwrap_or_else(|| json!({"body":crate::memory::unknown_cache(config.body_bytes),"layout":crate::memory::unknown_cache(config.layout_bytes)}));
            caches["attachment"] = crate::memory::cache(attachment.retained_bytes, config.attachment_base64_bytes, attachment.entries, "path");
            caches["attachment"]["live_encoded_bytes"] = json!(attachment.live_encoded_bytes);
            caches["attachment"]["pinned_bytes"] = json!(attachment.pinned_bytes);
            caches["replay"] = crate::memory::cache(replay.retained_bytes, config.history_bytes, replay.threads, "thread");
            caches["replay"]["live_bytes"] = json!(replay.live_bytes);
            caches["replay"]["pinned_bytes"] = json!(replay.pinned_bytes);
            Ok(json!({"process":crate::memory::process_snapshot(),"caches":caches,"accounting":"Existing cache budget accounting, not total heap; snapshots are not atomic or additive"}))
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn memory_probe_replies_from_current_ui_only_on_request() {
        crate::test_watchdog::watchdog("UI memory reply", async {
            let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
            let mut subscription = control.subscribe_ui();
            let source = control.clone();
            let pending = tokio::spawn(async move {
                source
                    .request(Request::new(
                        uuid::Uuid::new_v4().to_string(),
                        Command::Status(StatusArgs {
                            memory: true,
                            ..Default::default()
                        }),
                    ))
                    .await
            });
            let EngineEvent::MemoryProbe {
                owner: generation,
                reply,
            } = subscription.receiver.recv().await.unwrap()
            else {
                panic!("expected memory probe")
            };
            assert!(subscription.belongs_to(generation));
            assert_eq!(control.memory_samples.load(Ordering::SeqCst), 0);
            let mut ui = crate::ui::UiState::new(&control.store.config, std::env::temp_dir());
            ui.apply(crate::ui::UiUpdate::MemoryProbe(reply));
            let result = pending.await.unwrap();
            assert!(result.ok, "{result:?}");
            let result = result.result.unwrap();
            for key in ["body", "layout"] {
                assert!(result["caches"][key]["retained_bytes"].as_u64().is_some());
            }
            assert_eq!(control.memory_samples.load(Ordering::SeqCst), 1);
            drop(subscription);
            control.shutdown().await.unwrap();
            server.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn memory_reads_existing_caches_only_on_request_and_ignores_writer_lane() {
        crate::test_watchdog::watchdog("on-demand owner memory", async {
            use std::io::Write;
            let (root, owner, control, server) = crate::frontend::control_tests::fixture().await;
            assert_eq!(control.memory_samples.load(Ordering::SeqCst),0);
            let command = || Request::new(uuid::Uuid::new_v4().to_string(),Command::Status(StatusArgs {memory:true,..Default::default()}));
            let first = control.request(command()).await;
            assert!(first.ok, "{first:?}");
            let first = first.result.unwrap();
            assert!(first["caches"]["body"]["retained_bytes"].is_null());
            let file = root.path().join("attachment.txt");
            std::fs::write(&file,vec![b'x';65536]).unwrap();
            let path = file.to_string_lossy().into_owned();
            owner.db.call(move |db| {
                let payload = json!({"role":"user","content":[{"type":"input_text","text":"memory fixture"},{"type":"attachment_ref","attachment_id":"memory-attachment"}]}).to_string();
                db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('memory-node','user','now',?1,?2)",rusqlite::params![payload,payload.len() as i64])?;
                db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',0,'memory-node')",[])?;
                db.execute("UPDATE thread SET cursor_node_id='memory-node' WHERE id='thread'",[])?;
                db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('memory-attachment','memory-node',?1,'file','external','now')",[path])?;
                db.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('thread','memory-attachment','active','now',0)",[])?;
                Ok(())
            }).await.unwrap();
            let prepared = control.attachments.prepare("thread",CancellationToken::new(),false).await.unwrap();
            let replay = control.replay.clone();
            let lease = owner.db.call(move |db| replay.load(db,"thread")).await.unwrap();
            assert_eq!(control.memory_samples.load(Ordering::SeqCst),1);
            let (ready, entered) = tokio::sync::oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let db = owner.db.clone();
            let held = tokio::spawn(async move { db.call(move |_| {ready.send(()).unwrap();gate.recv().unwrap();Ok(())}).await.unwrap() });
            entered.await.unwrap();
            let observed = control.request(command()).await;
            release.send(()).unwrap(); held.await.unwrap();
            assert!(observed.ok,"{observed:?}");
            let observed = observed.result.unwrap();
            assert_eq!(observed["process"]["pid"],std::process::id());
            for name in ["attachment","replay"] {
                let value=&observed["caches"][name];
                assert!(value["retained_bytes"].as_u64().unwrap()>first["caches"][name]["retained_bytes"].as_u64().unwrap());
                assert!(value["retained_bytes"].as_u64().unwrap()<=value["limit_bytes"].as_u64().unwrap());
                assert!(value["entries"].as_u64().unwrap()>0);
            }
            assert_eq!(control.memory_samples.load(Ordering::SeqCst),2);
            let mut log=std::fs::File::create(root.path().join("memory-observations.jsonl")).unwrap();
            writeln!(log,"{first}").unwrap();writeln!(log,"{observed}").unwrap();log.flush().unwrap();
            assert_eq!(std::fs::read_to_string(root.path().join("memory-observations.jsonl")).unwrap().lines().count(),2);
            owner.db.call(|db| { for table in ["operation_receipt","model_attempt"] { assert_eq!(db.query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get::<_,i64>(0))?,0); } Ok(()) }).await.unwrap();
            drop(lease); drop(prepared);
            control.shutdown().await.unwrap();server.shutdown().await.unwrap();owner.shutdown().await.unwrap();
        }).await.unwrap();
    }
}
