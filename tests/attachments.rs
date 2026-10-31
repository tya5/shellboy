use rusqlite::params;
use shellboy::{
    attachments::{
        clipboard::{ClipboardPixels, osc52},
        *,
    },
    config::Config,
    protocol::{AttachmentKind, ErrorCode},
    storage::DbWorker,
};
use std::{path::PathBuf, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
struct Fixture {
    _temp: tempfile::TempDir,
    path: PathBuf,
    db: DbWorker,
    service: AttachmentService,
}
impl Fixture {
    async fn new(mut config: Config) -> Self {
        config.workers.attachment = 1;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().to_owned();
        let db = DbWorker::open(path.join("shellboy.db")).await.unwrap();
        let cwd = path.to_string_lossy().into_owned();
        db.call(move|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread',?1,?1,'profile','model','2026-01-01T00:00:00Z')",[cwd])?;db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','live','now')",[])?;db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','preparing','now')",[])?;Ok(())}).await.unwrap();
        db.call(|db|{db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('source','user','now','{\"role\":\"user\",\"content\":[]}')",[])?;db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('thread',0,'source')",[])?;Ok(())}).await.unwrap();
        let service =
            AttachmentService::new(db.clone(), &config, path.clone(), "instance-test".into());
        Self {
            _temp: temp,
            path,
            db,
            service,
        }
    }
    async fn add(&self, name: &str, bytes: &[u8]) -> String {
        std::fs::write(self.path.join(name), bytes).unwrap();
        let plan = self
            .service
            .plan_add(
                "thread",
                vec![AttachmentInput {
                    path: name.into(),
                    kind: AttachmentKind::File,
                }],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        self.register(plan).await.unwrap()[0].attachment_id.clone()
    }
    /// Production accepts a new, final source and metadata in one transaction.
    async fn register(
        &self,
        plan: AddPlan,
    ) -> Result<Vec<shellboy::protocol::AttachmentDetails>, shellboy::protocol::ApiError> {
        self.db.call(move |db| {
            let tx=db.transaction()?;
            let source=uuid::Uuid::new_v4().to_string();
            let mut content=vec![serde_json::json!({"type":"input_text","text":"before"})];
            content.extend(plan.files.iter().map(|_|serde_json::json!({"type":"attachment_ref","attachment_id":uuid::Uuid::new_v4().to_string()})));
            let payload=serde_json::json!({"role":"user","content":content}).to_string();
            tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'turn','user','now',?2,?3)",params![source,payload,payload.len() as i64])?;
            tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)SELECT 'thread',COALESCE(MAX(ordinal),-1)+1,?1 FROM thread_path WHERE thread_id='thread'",[&source])?;
            match add_tx(&tx,&plan,Some(&source)) {
                Ok(attachments)=>{tx.commit()?;Ok(Ok(attachments))},
                Err(error)=>Ok(Err(error)),
            }
        }).await.unwrap()
    }
    async fn adopt(&self, mut prepared: PreparedAttachments, success: bool) -> PreparedAttachments {
        for m in &mut prepared.manifest {
            m.api_item_index = Some(0);
            m.api_content_index = Some(m.source_element_index);
        }
        let attempt = uuid::Uuid::new_v4().to_string();
        let id = attempt.clone();
        let prepared=self.db.call(move|db|{let tx=db.transaction()?;tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,'turn',0,?2,'profile','model',0,'inflight','{}',?3)",params![id,prepared.selection_revision,chrono::Utc::now().to_rfc3339()])?;adopt_prepared_tx(&tx,&prepared,&id).unwrap();tx.execute("UPDATE model_attempt SET state=?2 WHERE id=?1",params![id,if success{"completed"}else{"failed"}])?;tx.commit()?;Ok(prepared)}).await.unwrap();
        if success {
            self.service
                .mark_success(&prepared, &attempt)
                .await
                .unwrap();
        } else {
            assert!(
                self.service
                    .mark_success(&prepared, &attempt)
                    .await
                    .is_err()
            );
        }
        prepared
    }
    async fn prepare(&self) -> PreparedAttachments {
        self.service
            .prepare("thread", CancellationToken::new(), false)
            .await
            .unwrap()
    }
    async fn remove(&self, attachment: String) {
        self.db
            .call(move |db| {
                let tx = db.transaction()?;
                remove_tx(&tx, "thread", &attachment).unwrap();
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn wait_pins(&self, count: i64) {
        crate::test_watchdog::watchdog("tests/attachments.rs: wait_pins", async {
            loop {
                let n = self
                    .db
                    .call(|db| {
                        Ok(db.query_row("SELECT COUNT(*) FROM runtime_pin", [], |r| {
                            r.get::<_, i64>(0)
                        })?)
                    })
                    .await
                    .unwrap();
                if n == count {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
#[tokio::test]
async fn prepare_reports_missing_or_deleted_thread_without_hiding_database_failures() {
    let f = Fixture::new(Config::default()).await;
    let error = f
        .service
        .prepare("missing", CancellationToken::new(), true)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::ThreadNotFound);
    f.db.call(|db| {
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='thread'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let error = f
        .service
        .prepare("thread", CancellationToken::new(), true)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::ThreadNotFound);
    f.db.call(|db| {
        db.execute("UPDATE thread SET deleted_at=NULL WHERE id='thread'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        f.service
            .prepare("thread", CancellationToken::new(), false)
            .await
            .is_ok()
    );
    f.db.call(|db| {
        db.execute_batch("ALTER TABLE thread RENAME TO unavailable_thread")?;
        Ok(())
    })
    .await
    .unwrap();
    let error = f
        .service
        .prepare("thread", CancellationToken::new(), true)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::SaveFailed);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn external_reference_observes_changes_deletion_and_force_reload_without_copy() {
    let f = Fixture::new(Config::default()).await;
    f.add("outside.txt", b"first").await;
    let initial = f.prepare().await;
    let hash = initial.manifest[0].content_hash.clone();
    assert_eq!(initial.manifest[0].state, "ready");
    assert!(!initial.manifest[0].changed);
    drop(f.adopt(initial, true).await);
    std::fs::write(f.path.join("outside.txt"), b"changed-longer").unwrap();
    let changed = f
        .service
        .prepare("thread", CancellationToken::new(), true)
        .await
        .unwrap();
    assert!(changed.manifest[0].changed);
    assert_ne!(changed.manifest[0].content_hash, hash);
    assert!(changed.notices[0].contains("changed"));
    drop(changed);
    std::fs::remove_file(f.path.join("outside.txt")).unwrap();
    let missing = f.prepare().await;
    assert_eq!(missing.manifest[0].state, "unavailable");
    assert!(
        missing
            .placements
            .iter()
            .flat_map(|p| &p.content)
            .all(|v| v["type"] == "input_text")
    );
    assert!(!f.path.join("managed").exists());
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn failed_attempt_does_not_change_hash_baseline_or_protection() {
    let f = Fixture::new(Config::default()).await;
    f.add("a.txt", b"one").await;
    drop(f.adopt(f.prepare().await, true).await);
    std::fs::write(f.path.join("a.txt"), b"second-version").unwrap();
    let changed = f.prepare().await;
    assert!(changed.manifest[0].changed);
    drop(f.adopt(changed, false).await);
    let retry = f.prepare().await;
    assert!(retry.manifest[0].changed);
    drop(retry);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn duplicate_paths_are_distinct_elements_but_share_encoded_cache() {
    let f = Fixture::new(Config::default()).await;
    f.add("same.txt", b"body").await;
    let plan = f
        .service
        .plan_add(
            "thread",
            vec![AttachmentInput {
                path: "same.txt".into(),
                kind: AttachmentKind::File,
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let second = f.register(plan).await.unwrap();
    let prepared = f.prepare().await;
    assert_eq!(prepared.manifest.len(), 2);
    assert_ne!(prepared.manifest[0].attachment_id, second[0].attachment_id);
    assert_eq!(prepared.placements.len(), 2);
    assert_eq!(
        prepared.placements[0].content[1]["file_data"],
        prepared.placements[1].content[1]["file_data"]
    );
    let stats = f.service.cache_stats();
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.live_encoded_bytes, stats.retained_bytes);
    assert!(stats.pinned_bytes > 0);
    drop(prepared);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn protected_totals_reject_add_and_later_growth_holds_request() {
    let mut config = Config::default();
    config.limits.attachment_file_bytes = 10;
    config.limits.attachment_request_bytes = 10;
    let f = Fixture::new(config).await;
    f.add("first", b"123456").await;
    std::fs::write(f.path.join("second"), b"12345").unwrap();
    let error = f
        .service
        .plan_add(
            "thread",
            vec![AttachmentInput {
                path: "second".into(),
                kind: AttachmentKind::File,
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    std::fs::write(f.path.join("first"), b"1234").unwrap();
    let plan = f
        .service
        .plan_add(
            "thread",
            vec![AttachmentInput {
                path: "second".into(),
                kind: AttachmentKind::File,
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    f.register(plan).await.unwrap();
    std::fs::write(f.path.join("first"), b"123456").unwrap();
    let error = f
        .service
        .prepare("thread", CancellationToken::new(), true)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn oldest_past_attachments_excluded_only_at_admission_never_resurrect() {
    let mut config = Config::default();
    config.limits.attachment_file_bytes = 10;
    config.limits.attachment_request_bytes = 10;
    let f = Fixture::new(config).await;
    let old = f.add("old", b"123456").await;
    drop(f.adopt(f.prepare().await, true).await);
    f.add("new", b"123456").await;
    let prepared = f.prepare().await;
    assert_eq!(prepared.excluded_ids, vec![old.clone()]);
    let before =
        f.db.call(|db| Ok(list_tx(db, "thread").unwrap()))
            .await
            .unwrap();
    assert!(before[0].selected);
    drop(f.adopt(prepared, true).await);
    std::fs::write(f.path.join("new"), b"x").unwrap();
    let next = f.prepare().await;
    assert_eq!(next.manifest[0].state, "excluded");
    assert_eq!(next.manifest[1].state, "ready");
    drop(next);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn stale_selection_cas_rejects_manifest_and_transaction_rolls_back() {
    let f = Fixture::new(Config::default()).await;
    f.add("a", b"a").await;
    let prepared = f.prepare().await;
    f.add("b", b"b").await;
    let result =
        f.db.call(move |db| {
            let tx = db.transaction()?;
            let result = adopt_prepared_tx(&tx, &prepared, "missing-attempt");
            assert_eq!(result.unwrap_err().code, ErrorCode::OperationConflict);
            drop(tx);
            Ok(
                db.query_row("SELECT COUNT(*) FROM attachment_send", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(result, 0);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn per_file_failure_continues_conversation_and_zero_cache_does_not_reject() {
    let mut config = Config::default();
    config.cache.attachment_base64_bytes = 0;
    config.limits.attachment_file_bytes = 4;
    config.limits.attachment_request_bytes = 8;
    let f = Fixture::new(config).await;
    f.add("valid", b"1234").await;
    f.add("huge", b"1234567890").await;
    let result = f.prepare().await;
    assert_eq!(result.manifest[0].state, "ready");
    assert_eq!(result.manifest[1].state, "unavailable");
    assert_eq!(f.service.cache_stats().retained_bytes, 0);
    assert!(f.service.cache_stats().pinned_bytes > 0);
    drop(result);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn timeout_keeps_real_blocking_worker_occupied_until_exit() {
    let worker = BlockingFiles::new(1);
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let w = worker.clone();
    let task = tokio::spawn(async move {
        w.execute(
            Instant::now() + Duration::from_secs(3600),
            CancellationToken::new(),
            move || {
                entered.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            },
        )
        .await
    });
    entered_rx.await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3601)).await;
    let error = task.await.unwrap().unwrap_err();
    assert!(error.message.contains("timed out"));
    assert_eq!(worker.available(), 0);
    let next = worker.execute(
        Instant::now() + Duration::from_millis(20),
        CancellationToken::new(),
        || Ok(1),
    );
    tokio::pin!(next);
    assert!(futures_util::poll!(&mut next).is_pending());
    tokio::time::advance(Duration::from_millis(21)).await;
    assert!(next.await.is_err());
    tokio::time::resume();
    release.send(()).unwrap();
    crate::test_watchdog::watchdog(
        "tests/attachments.rs: timeout_keeps_real_blocking_worker_occupied_until_exit",
        async {
            while worker.available() == 0 {
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn managed_png_preserves_alpha_active_and_send_pins_protect_gc() {
    let mut config = Config::default();
    config.retention.clipboard_bytes = 1;
    let f = Fixture::new(config).await;
    let image = f
        .service
        .save_clipboard_pixels(
            ClipboardPixels {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 4],
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(std::fs::read(&image.path).unwrap().starts_with(b"\x89PNG"));
    let mut reader = png::Decoder::new(std::io::Cursor::new(std::fs::read(&image.path).unwrap()))
        .read_info()
        .unwrap();
    let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut decoded).unwrap();
    assert_eq!(&decoded[..frame.buffer_size()], &[1, 2, 3, 4]);
    let mut plan = f
        .service
        .plan_add("thread", vec![], CancellationToken::new())
        .await
        .unwrap();
    plan.files.push(image.planned());
    let attachment = f.register(plan).await.unwrap()[0].attachment_id.clone();
    let path = image.path.clone();
    drop(image);
    f.wait_pins(0).await;
    let report = f.service.collect_managed(chrono::Utc::now()).await.unwrap();
    assert!(report.deleted.is_empty());
    assert!(path.exists());
    assert!(report.protected_bytes > 0);
    let prepared = f.prepare().await;
    f.remove(attachment).await;
    let report = f.service.collect_managed(chrono::Utc::now()).await.unwrap();
    assert!(report.deleted.is_empty());
    assert!(path.exists());
    drop(prepared);
    f.wait_pins(0).await;
    let report = f.service.collect_managed(chrono::Utc::now()).await.unwrap();
    assert_eq!(report.deleted.len(), 1);
    assert!(!path.exists());
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn external_files_are_never_unlinked_and_deleted_managed_paths_cannot_reactivate() {
    let mut config = Config::default();
    config.retention.clipboard_bytes = 1;
    let f = Fixture::new(config).await;
    let external = f.add("external.txt", b"keep").await;
    f.remove(external).await;
    f.service.collect_managed(chrono::Utc::now()).await.unwrap();
    assert_eq!(std::fs::read(f.path.join("external.txt")).unwrap(), b"keep");
    let image = f
        .service
        .save_clipboard_pixels(
            ClipboardPixels {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 0],
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let path = image.path.clone();
    drop(image);
    f.wait_pins(0).await;
    f.service.collect_managed(chrono::Utc::now()).await.unwrap();
    let plan = f
        .service
        .plan_add(
            "thread",
            vec![AttachmentInput {
                path,
                kind: AttachmentKind::Image,
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        f.register(plan).await.unwrap_err().code,
        ErrorCode::InvalidState
    );
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_delete_crash_and_creation_restart_cleanup_never_unlink_external_paths() {
    let f = Fixture::new(Config::default()).await;
    let image = f
        .service
        .save_clipboard_pixels(
            ClipboardPixels {
                width: 1,
                height: 1,
                rgba: vec![0, 1, 2, 3],
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let managed = image.id.clone();
    let path = image.path.clone();
    drop(image);
    f.wait_pins(0).await;
    let external = f.path.join("external");
    std::fs::write(&external, b"keep").unwrap();
    let unsafe_path = external.to_string_lossy().into_owned();
    f.db.call(move|db|{db.execute("UPDATE managed_file SET state='deleting' WHERE id=?1",[managed])?;db.execute("INSERT INTO managed_file(id,absolute_path,state,created_at)VALUES('unsafe',?1,'deleting','old')",[unsafe_path])?;let creating=uuid::Uuid::new_v4().to_string();db.execute("INSERT INTO managed_file(id,absolute_path,state,created_at)VALUES(?1,?2,'creating','old')",params![creating,"/never-owned-missing-file"])?;Ok(())}).await.unwrap();
    let report = f.service.restart_cleanup().await.unwrap();
    assert!(!path.exists());
    assert!(external.exists());
    assert!(report.failed.iter().any(|(id, _)| id == "unsafe"));
    f.db.shutdown().await.unwrap();
}
#[test]
fn osc52_is_one_bounded_request_and_clipboard_rgba_is_validated() {
    let request = osc52("日本語", 100).unwrap();
    assert!(request.starts_with(b"\x1b]52;c;"));
    assert!(request.ends_with(b"\x07"));
    assert!(osc52("日本語", 8).is_err());
    assert!(
        ClipboardPixels {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0]
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn explicit_reload_detects_an_attribute_preserving_rewrite() {
    let f = Fixture::new(Config::default()).await;
    f.add("stable.txt", b"alpha").await;
    let initial = f.prepare().await;
    let original_hash = initial.manifest[0].content_hash.clone();
    drop(f.adopt(initial, true).await);
    let path = f.path.join("stable.txt");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, b"omega").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let cached = f.prepare().await;
    assert_eq!(cached.manifest[0].content_hash, original_hash);
    drop(cached);
    let reloaded = f
        .service
        .prepare("thread", CancellationToken::new(), true)
        .await
        .unwrap();
    assert!(reloaded.manifest[0].changed);
    assert_ne!(reloaded.manifest[0].content_hash, original_hash);
    use base64::Engine;
    let data = reloaded
        .placements
        .iter()
        .flat_map(|p| &p.content)
        .find_map(|item| item.get("file_data").and_then(serde_json::Value::as_str))
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(data.split_once(',').unwrap().1)
            .unwrap(),
        b"omega"
    );
    drop(reloaded);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn byte_lru_evicts_retention_but_observes_pinned_encoded_memory() {
    let mut config = Config::default();
    config.cache.attachment_base64_bytes = 128;
    let f = Fixture::new(config).await;
    f.add("a", b"abc").await;
    f.add("b", b"def").await;
    let prepared = f.prepare().await;
    let stats = f.service.cache_stats();
    assert!(stats.retained_bytes <= 128);
    assert_eq!(stats.entries, 1);
    assert!(stats.live_encoded_bytes > stats.retained_bytes);
    assert!(stats.pinned_bytes >= stats.live_encoded_bytes);
    drop(prepared);
    let stats = f.service.cache_stats();
    assert_eq!(stats.live_encoded_bytes, stats.retained_bytes);
    assert_eq!(stats.pinned_bytes, 0);
    f.db.shutdown().await.unwrap();
}
#[tokio::test]
async fn fork_selection_sharing_and_path_reattachment_keep_managed_image_active() {
    let mut config = Config::default();
    config.retention.clipboard_bytes = 1;
    let f = Fixture::new(config).await;
    let image = f
        .service
        .save_clipboard_pixels(
            ClipboardPixels {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 1],
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut plan = f
        .service
        .plan_add("thread", vec![], CancellationToken::new())
        .await
        .unwrap();
    plan.files.push(image.planned());
    let original = f.register(plan).await.unwrap()[0].attachment_id.clone();
    let location = image.path.clone();
    drop(image);
    f.wait_pins(0).await;
    f.db.call(|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) SELECT 'fork',cwd,agents_path,profile_id,model_id,updated_at FROM thread WHERE id='thread'",[])?;db.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order,revision,protected)SELECT 'fork',attachment_id,state,selected_at,selected_order,revision,protected FROM attachment_selection WHERE thread_id='thread'",[])?;Ok(())}).await.unwrap();
    f.remove(original.clone()).await;
    assert!(
        f.service
            .collect_managed(chrono::Utc::now())
            .await
            .unwrap()
            .deleted
            .is_empty()
    );
    let plan = f
        .service
        .plan_add(
            "thread",
            vec![AttachmentInput {
                path: location.clone(),
                kind: AttachmentKind::Image,
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let reattached = f.register(plan).await.unwrap();
    assert!(reattached[0].managed);
    f.db.call(move |db| {
        let tx = db.transaction()?;
        remove_tx(&tx, "fork", &original).unwrap();
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        f.service
            .collect_managed(chrono::Utc::now())
            .await
            .unwrap()
            .deleted
            .is_empty()
    );
    assert!(location.exists());
    f.remove(reattached[0].attachment_id.clone()).await;
    assert_eq!(
        f.service
            .collect_managed(chrono::Utc::now())
            .await
            .unwrap()
            .deleted
            .len(),
        1
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn saved_unsent_draft_protects_managed_image_after_runtime_pins_are_cleared() {
    let fixture = Fixture::new(Config::default()).await;
    let image = fixture
        .service
        .save_clipboard_pixels(
            ClipboardPixels {
                width: 1,
                height: 1,
                rgba: vec![255, 0, 0, 255],
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let path = image.path.clone();
    let id = image.id.clone();
    let body =
        serde_json::json!({"attachments":[{"id":id,"path":path,"kind":"image","state":"ready"}]})
            .to_string();
    fixture.db.call(move|db|{
        db.execute("INSERT INTO draft(id,revision,body_json,state,updated_at) VALUES('unsent',0,?1,'editing','now')",[body])?;
        db.execute("UPDATE managed_file SET created_at='2000-01-01T00:00:00Z' WHERE id=?1",[id])?;
        Ok(())
    }).await.unwrap();
    drop(image);
    fixture
        .db
        .call(|db| {
            db.execute("DELETE FROM runtime_pin", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let protected = fixture.service.restart_cleanup().await.unwrap();
    assert!(protected.deleted.is_empty());
    assert!(protected.protected_bytes > 0);
    assert!(path.exists());
    fixture.db.call(|db|{db.execute("UPDATE draft SET body_json='{\"attachments\":[]}',revision=revision+1 WHERE id='unsent'",[])?;Ok(())}).await.unwrap();
    let released = fixture
        .service
        .collect_managed(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(released.deleted.len(), 1);
    assert!(!path.exists());
    fixture.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn references_require_provenance_and_preserve_order_before_preparation() {
    let f = Fixture::new(Config::default()).await;
    std::fs::write(f.path.join("image.txt"), b"current").unwrap();
    let plan = f
        .service
        .plan_add(
            "thread",
            vec![AttachmentInput {
                path: "image.txt".into(),
                kind: AttachmentKind::File,
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        f.service.add(plan.clone(), None).await.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let added = f.register(plan).await.unwrap();
    assert_ne!(added[0].source_node_id, "source");
    assert_eq!(added[0].delivery_status, "applied");
    let first = f.prepare().await;
    assert_eq!(first.placements[0].source_element_index, 1);
    assert!(first.manifest[0].first_inclusion);
    f.adopt(first, true).await;
    let replay = f.prepare().await;
    assert!(!replay.manifest[0].first_inclusion);
    assert!(!replay.manifest[0].changed);
    let source = added[0].source_node_id.clone();
    f.db.call(move |db| {
        db.execute(
            "UPDATE node SET payload_json='{\"role\":\"user\",\"content\":[]}' WHERE id=?1",
            [source],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        f.service
            .prepare("thread", CancellationToken::new(), false)
            .await
            .is_err()
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_path_new_id_has_no_other_references_hash_baseline() {
    let f = Fixture::new(Config::default()).await;
    f.add("same.txt", b"first").await;
    f.adopt(f.prepare().await, true).await;
    let new = f.add("same.txt", b"second-version").await;
    let prepared = f.prepare().await;
    let old = &prepared.manifest[0];
    let new = prepared
        .manifest
        .iter()
        .find(|m| m.attachment_id == new)
        .unwrap();
    assert!(old.changed);
    assert!(!new.changed);
    assert!(new.first_inclusion);
    assert!(new.previous_hash.is_none());
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn committed_source_cannot_receive_payload_appends_or_unstamped_attachment_changes() {
    let f = Fixture::new(Config::default()).await;
    std::fs::write(f.path.join("new.txt"), b"new data").unwrap();
    let plan = f
        .service
        .plan_add(
            "thread",
            vec![AttachmentInput {
                path: "new.txt".into(),
                kind: AttachmentKind::File,
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let before=f.db.call(|db|Ok(db.query_row("SELECT payload_json,payload_revision,body_hash,payload_bytes FROM node WHERE id='source'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,i64>(3)?)))?)).await.unwrap();
    assert_eq!(
        f.service
            .add(plan.clone(), Some("source".into()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    f.db.call(move |db|{
        let after=db.query_row("SELECT payload_json,payload_revision,body_hash,payload_bytes FROM node WHERE id='source'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,i64>(3)?)))?;
        assert_eq!(after,before);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM attachment_ref",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT selection_revision FROM thread WHERE id='thread'",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    // A finalized, predeclared source can also be registered through the Rust
    // helper after commit; the helper never needs to guess its commit history.
    let attachment = uuid::Uuid::new_v4().to_string();
    let stored = attachment.clone();
    let final_hash=f.db.call(move |db|{
        use sha2::Digest;
        let body=serde_json::json!({"role":"user","content":[{"type":"input_text","text":"final"},{"type":"attachment_ref","attachment_id":stored}]}).to_string();
        let body_hash=sha2::Sha256::digest(body.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('final','user','now',?1,?2,?3)",params![body,body.len() as i64,body_hash])?;
        db.execute_batch("CREATE TRIGGER immutable_payload_guard BEFORE UPDATE OF payload_json,payload_bytes,body_hash ON node BEGIN SELECT RAISE(ABORT,'no source rewrites');END;")?;
        Ok(body_hash)
    }).await.unwrap();
    let registered = f.service.add(plan, Some("final".into())).await.unwrap();
    assert_eq!(registered[0].attachment_id, attachment);
    f.db.call(move |db| {
        assert_eq!(
            db.query_row(
                "SELECT payload_revision,body_hash FROM node WHERE id='final'",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            )?,
            (0, final_hash)
        );
        Ok(())
    })
    .await
    .unwrap();
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_designations_become_past_only_after_a_successful_request() {
    for missing in [true, false] {
        let mut config = Config::default();
        config.limits.attachment_request_bytes = 10;
        config.limits.attachment_file_bytes = 10;
        let f = Fixture::new(config).await;
        let old = f
            .add("old", if missing { b"123456" } else { b"123456789012" })
            .await;
        if missing {
            std::fs::remove_file(f.path.join("old")).unwrap();
        }
        let unavailable = f.prepare().await;
        assert_eq!(unavailable.manifest[0].state, "unavailable");
        assert!(unavailable.manifest[0].content_hash.is_none());
        f.adopt(unavailable, false).await;
        let copy = old.clone();
        f.db.call(move |db| {
            assert!(db.query_row(
                "SELECT protected FROM attachment_selection WHERE attachment_id=?1",
                [copy],
                |r| r.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .await
        .unwrap();
        let unavailable = f.prepare().await;
        f.adopt(unavailable, true).await;
        let copy = old.clone();
        f.db.call(move|db|{
            assert!(!db.query_row("SELECT protected FROM attachment_selection WHERE attachment_id=?1",[&copy],|r|r.get::<_,bool>(0))?);
            assert_eq!(db.query_row("SELECT COUNT(*) FROM attachment_send WHERE attachment_id=?1 AND (state='sent' OR content_hash IS NOT NULL)",[copy],|r|r.get::<_,i64>(0))?,0,"missing bytes are never recorded as sent or hashed");
            Ok(())
        }).await.unwrap();
        std::fs::write(f.path.join("old"), b"123456").unwrap();
        f.add("new", b"123456").await;
        let restored = f.prepare().await;
        assert_eq!(restored.excluded_ids, vec![old]);
        assert_eq!(restored.manifest[0].state, "excluded");
        assert_eq!(restored.manifest[1].state, "ready");
        f.db.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn detached_excluded_tombstones_do_not_hide_active_or_live_source_errors() {
    for keep_payload in [false, true] {
        let f = Fixture::new(Config::default()).await;
        let id = f.add("retired.txt", b"retained file").await;
        let saved_id = id.clone();
        f.db.call(move |db| {
            let tx = db.transaction()?;
            let source: String = tx.query_row("SELECT source_node_id FROM attachment_ref WHERE id=?1", [&saved_id], |r| r.get(0))?;
            exclude_replaced_sources_tx(&tx, "thread", std::slice::from_ref(&source))?;
            tx.execute("DELETE FROM thread_path WHERE thread_id='thread' AND node_id=?1", [&source])?;
            tx.execute("UPDATE node SET deleted_reason='thread_deleted_unreachable',payload_json=CASE WHEN ?2 THEN payload_json ELSE NULL END WHERE id=?1", params![source, keep_payload])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let prepared = f.prepare().await;
        assert!(prepared.manifest.is_empty() && prepared.placements.is_empty());
        f.db.call(move |db| {
            let state: String = db.query_row("SELECT state FROM attachment_selection WHERE attachment_id=?1", [&id], |r| r.get(0))?;
            assert_eq!(state, "excluded");
            db.execute("UPDATE attachment_selection SET state='active',reason=NULL WHERE attachment_id=?1", [&id])?;
            db.execute("UPDATE node SET payload_json=NULL WHERE id=(SELECT source_node_id FROM attachment_ref WHERE id=?1)", [&id])?;
            Ok(())
        }).await.unwrap();
        let error = f
            .service
            .prepare("thread", CancellationToken::new(), false)
            .await
            .unwrap_err();
        assert!(
            error.message.contains("ordered source reference"),
            "{error:?}"
        );
        f.db.call(|db| {
            let tx = db.transaction()?;
            tx.execute("UPDATE attachment_selection SET state='excluded',reason='context_replacement' WHERE thread_id='thread'", [])?;
            tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT 'thread',(SELECT COALESCE(MAX(ordinal),-1)+1 FROM thread_path WHERE thread_id='thread'),source_node_id FROM attachment_ref LIMIT 1", [])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let error = f
            .service
            .prepare("thread", CancellationToken::new(), false)
            .await
            .unwrap_err();
        assert!(
            error.message.contains("ordered source reference"),
            "{error:?}"
        );
        f.db.shutdown().await.unwrap();
    }
    let f = Fixture::new(Config::default()).await;
    let id = f.add("broken.txt", b"file").await;
    f.db.call(move |db| {
        let tx = db.transaction()?;
        let source: String = tx.query_row(
            "SELECT source_node_id FROM attachment_ref WHERE id=?1",
            [&id],
            |r| r.get(0),
        )?;
        exclude_replaced_sources_tx(&tx, "thread", std::slice::from_ref(&source))?;
        tx.execute(
            "DELETE FROM thread_path WHERE thread_id='thread' AND node_id=?1",
            [&source],
        )?;
        tx.execute("UPDATE node SET payload_json='{}' WHERE id=?1", [&source])?;
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    let error = f
        .service
        .prepare("thread", CancellationToken::new(), false)
        .await
        .unwrap_err();
    assert!(
        error.message.contains("ordered source reference"),
        "{error:?}"
    );
    f.db.shutdown().await.unwrap();
}

#[tokio::test]
async fn context_exclusion_notices_are_bounded_retryable_and_make_progress() {
    for long_paths in [false, true] {
        let f = Fixture::new(Config::default()).await;
        let count = if long_paths { 12 } else { 101 };
        let details = f
            .register(AddPlan {
                thread_id: "thread".into(),
                selection_revision: 0,
                files: (0..count)
                    .map(|i| PlannedAttachment {
                        // These excluded paths must never be opened. One path exceeds the
                        // entire notice budget; escaped UTF8 previews also exercise JSON bytes.
                        path: f.path.join(if long_paths {
                            format!("{}-{i}", "界\n".repeat(20000))
                        } else {
                            format!("missing-{i}")
                        }),
                        kind: AttachmentKind::File,
                        observed_bytes: None,
                        managed_id: None,
                    })
                    .collect(),
            })
            .await
            .unwrap();
        let source = details[0].source_node_id.clone();
        f.db.call(move |db| {
            let tx = db.transaction()?;
            exclude_replaced_sources_tx(&tx, "thread", std::slice::from_ref(&source))?;
            tx.execute(
                "DELETE FROM thread_path WHERE thread_id='thread' AND node_id=?1",
                [source],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut batches = 0;
        loop {
            let prepared = f.prepare().await;
            if prepared.manifest.is_empty() {
                break;
            }
            assert!(prepared.manifest.len() <= shellboy::protocol::DEFAULT_LIST_LIMIT as usize);
            let content: Vec<_> = prepared
                .placements
                .iter()
                .flat_map(|p| p.content.clone())
                .collect();
            let body = serde_json::json!({"role":"developer","content":content});
            assert!(
                serde_json::to_vec(&body).unwrap().len()
                    <= shellboy::protocol::MAX_BODY_BYTES as usize
            );
            let ids: Vec<_> = prepared
                .manifest
                .iter()
                .map(|m| m.attachment_id.clone())
                .collect();
            assert!(
                prepared
                    .manifest
                    .iter()
                    .all(|m| m.state == "excluded" && m.content_hash.is_none())
            );
            if long_paths {
                for notice in &prepared.notices {
                    let value: serde_json::Value = serde_json::from_str(
                        notice
                            .strip_prefix("Shellboy attachment exclusion v1: ")
                            .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(value["path_truncated"], true);
                    assert!(value["path"].as_str().unwrap().len() <= 4096);
                    assert!(value["path_utf8_bytes"].as_u64().unwrap() > 65536);
                    assert_eq!(value["path_utf8_sha256"].as_str().unwrap().len(), 64);
                }
            }
            if batches == 0 {
                if !long_paths {
                    assert_eq!(ids.len(), 50);
                }
                f.adopt(prepared, false).await;
                let retry = f.prepare().await;
                assert_eq!(
                    retry
                        .manifest
                        .iter()
                        .map(|m| m.attachment_id.clone())
                        .collect::<Vec<_>>(),
                    ids
                );
                f.adopt(retry, true).await;
            } else {
                f.adopt(prepared, true).await;
            }
            for id in ids {
                assert!(seen.insert(id));
            }
            batches += 1;
            assert!(batches <= count);
        }
        assert_eq!(seen.len(), count);
        assert!(batches > 1);
        f.db.call(move|db|{
            let excluded:i64=db.query_row("SELECT count(*) FROM attachment_selection WHERE state='excluded' AND reason='context_replacement'",[],|r|r.get(0))?;
            assert_eq!(excluded,count as i64); Ok(())
        }).await.unwrap();
    }
}

async fn observed_list(f: &Fixture) -> Vec<shellboy::protocol::AttachmentDetails> {
    use shellboy::{control::db::Store, protocol::*};
    let store = Store::new(
        f.db.clone(),
        std::sync::Arc::new(Config::default()),
        f.path.clone(),
        "instance-test".into(),
    );
    let result = store
        .execute(
            Request::new(
                uuid::Uuid::new_v4().to_string(),
                Command::AttachmentList(ThreadTarget {
                    thread_id: Some("thread".into()),
                }),
            ),
            None,
        )
        .await
        .unwrap();
    serde_json::from_value(result.result).unwrap()
}

#[tokio::test]
async fn managed_path_uses_ordinary_hash_baselines_missing_facts_and_gc() {
    crate::test_watchdog::watchdog("managed attachment observations", async {
        use std::io::Write;
        let mut config = Config::default();
        config.retention.clipboard_bytes = 1;
        let f = Fixture::new(config).await;
        let image = f
            .service
            .save_clipboard_pixels(
                ClipboardPixels {
                    width: 1,
                    height: 1,
                    rgba: vec![2, 4, 6, 255],
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let path = image.path.clone();
        assert!(path.is_absolute());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut plan = f
            .service
            .plan_add("thread", vec![], CancellationToken::new())
            .await
            .unwrap();
        plan.files.push(image.planned());
        let added = f.register(plan).await.unwrap();
        assert_eq!(added[0].path, path.to_str().unwrap());
        assert!(added[0].managed && added[0].unavailable_reason.is_none());
        let shown = PathBuf::from(observed_list(&f).await[0].path.clone());
        #[cfg(windows)]
        let script = {
            let script = f.path.join("read-image.ps1");
            std::fs::write(&script, "$ErrorActionPreference='Stop'; [Console]::Write([Convert]::ToBase64String([IO.File]::ReadAllBytes($args[0])))").unwrap();
            script
        };
        let output = tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            let mut command = {
                let mut command = std::process::Command::new("/bin/sh");
                command.args(["-c", "cat \"$1\"", "read-managed-image"]);
                command
            };
            #[cfg(windows)]
            let mut command = {
                let mut command = std::process::Command::new("powershell.exe");
                command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"]).arg(script);
                command
            };
            command.arg(shown).output().unwrap()
        }).await.unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        #[cfg(unix)]
        assert_eq!(output.stdout, std::fs::read(&path).unwrap());
        #[cfg(windows)] {
            use base64::Engine;
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(String::from_utf8(output.stdout).unwrap().trim()).unwrap(), std::fs::read(&path).unwrap());
        }

        let attachment = added[0].attachment_id.clone();
        drop(image);
        let log_path = f.path.join("observations.jsonl");
        let mut log = std::fs::File::create(&log_path).unwrap();
        let initial = f.prepare().await;
        let first_hash = initial.manifest[0].content_hash.clone();
        let metadata: serde_json::Value = serde_json::from_str(
            initial.placements[0].content[0]["text"]
                .as_str()
                .unwrap()
                .strip_prefix("[Shellboy attachment metadata] ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["path"], path.to_str().unwrap());
        assert!(metadata["unavailable_reason"].is_null());
        writeln!(
            log,
            "{}",
            serde_json::to_string(&initial.manifest[0]).unwrap()
        )
        .unwrap();
        drop(f.adopt(initial, true).await);
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[12, 34, 56, 255, 1, 2, 3, 255])
                .unwrap();
        }
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            std::fs::read(&observed_list(&f).await[0].path).unwrap(),
            bytes
        );
        let changed = f.prepare().await;
        assert!(changed.manifest[0].changed);
        assert_eq!(changed.manifest[0].previous_hash, first_hash);
        assert_ne!(changed.manifest[0].content_hash, first_hash);
        assert!(
            changed
                .notices
                .iter()
                .any(|s| s.contains("attachment changed update"))
        );
        writeln!(
            log,
            "{}",
            serde_json::to_string(&changed.manifest[0]).unwrap()
        )
        .unwrap();
        drop(f.adopt(changed, true).await);
        std::fs::remove_file(&path).unwrap();
        assert!(observed_list(&f).await[0].unavailable_reason.is_some());
        let missing = f.prepare().await;
        assert_eq!(missing.manifest[0].state, "unavailable");
        let metadata: serde_json::Value = serde_json::from_str(
            missing.placements[0].content[0]["text"]
                .as_str()
                .unwrap()
                .strip_prefix("[Shellboy attachment metadata] ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["path"], path.to_str().unwrap());
        assert!(metadata["unavailable_reason"].is_string());
        writeln!(
            log,
            "{}",
            serde_json::to_string(&missing.manifest[0]).unwrap()
        )
        .unwrap();
        drop(missing);
        std::fs::write(&path, &bytes).unwrap();
        f.remove(attachment).await;
        loop {
            let n =
                f.db.call(|db| {
                    Ok(db.query_row("SELECT count(*) FROM runtime_pin", [], |r| {
                        r.get::<_, i64>(0)
                    })?)
                })
                .await
                .unwrap();
            if n == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let report = f.service.collect_managed(chrono::Utc::now()).await.unwrap();
        assert_eq!(report.deleted.len(), 1);
        let deleted = observed_list(&f).await;
        assert_eq!(deleted[0].path, path.to_str().unwrap());
        assert!(
            deleted[0]
                .unavailable_reason
                .as_deref()
                .unwrap()
                .contains("clipboard retention")
        );
        assert!(!path.exists());
        log.flush().unwrap();
        assert_eq!(
            std::fs::read_to_string(&log_path).unwrap().lines().count(),
            3
        );
        f.db.call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM operation_receipt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
        f.db.shutdown().await.unwrap();
    })
    .await
    .expect("managed attachment facts watchdog");
}

#[tokio::test]
async fn attachment_list_seeks_existing_indexes_and_does_not_wait_for_writer() {
    crate::test_watchdog::watchdog("indexed attachment list while writer is held", async {
        let f = Fixture::new(Config::default()).await;
        f.add("regular.txt", b"ordinary").await;
        f.db.call(|db| {
            let mut plan = db.prepare(&format!("EXPLAIN QUERY PLAN {}", ATTACHMENT_LIST_SQL))?;
            let details = plan
                .query_map(["thread"], |r| r.get::<_, String>(3))?
                .collect::<Result<Vec<_>, _>>()?;
            for alias in ["s", "a", "m"] {
                assert!(
                    details
                        .iter()
                        .any(|s| s.starts_with(&format!("SEARCH {alias} "))),
                    "{details:?}"
                );
                assert!(
                    !details
                        .iter()
                        .any(|s| s.starts_with(&format!("SCAN {alias} "))),
                    "{details:?}"
                );
            }
            Ok(())
        })
        .await
        .unwrap();
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let db = f.db.clone();
        let blocked = tokio::spawn(async move {
            db.call(move |_| {
                ready.send(()).unwrap();
                gate.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap()
        });
        waiting.await.unwrap();
        let items = observed_list(&f).await;
        assert_eq!(items.len(), 1);
        assert!(items[0].unavailable_reason.is_none());
        release.send(()).unwrap();
        blocked.await.unwrap();
        std::fs::remove_file(f.path.join("regular.txt")).unwrap();
        assert!(observed_list(&f).await[0].unavailable_reason.is_some());
        f.db.shutdown().await.unwrap();
    })
    .await
    .expect("attachment observation watchdog");
}
