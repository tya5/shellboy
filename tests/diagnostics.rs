use shellboy::{
    config::Logging,
    diagnostics::{DiagnosticEvent, DiagnosticIds, Diagnostics},
};

#[tokio::test]
async fn diagnostic_rotation_is_bounded_and_records_only_structured_context() {
    let root = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::start(
        root.path().join("logs"),
        Logging {
            file_bytes: 512,
            files: 3,
        },
    );
    for i in 0..100 {
        diagnostics.record(
            DiagnosticEvent::ModelFailure,
            DiagnosticIds {
                thread_id: Some(format!("thread-{i}")),
                ..Default::default()
            },
        );
    }
    diagnostics.shutdown().await;
    assert!(!diagnostics.has_failed());
    let mut files = 0;
    for entry in std::fs::read_dir(root.path().join("logs")).unwrap() {
        let entry = entry.unwrap();
        files += 1;
        assert!(entry.metadata().unwrap().len() <= 512);
        for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["event"], "model_failure");
            assert!(value.get("message").is_none());
            assert!(value.get("body").is_none());
        }
    }
    assert!(files <= 3);
}

#[tokio::test]
async fn log_save_failure_and_saturated_queue_do_not_block_application_admission() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("not-a-directory");
    std::fs::write(&path, "keep").unwrap();
    let diagnostics = Diagnostics::start(
        path,
        Logging {
            file_bytes: 1024,
            files: 1,
        },
    );
    for _ in 0..1000 {
        diagnostics.record(DiagnosticEvent::Startup, DiagnosticIds::default());
    }
    diagnostics.shutdown().await;
    assert!(diagnostics.has_failed());
    assert!(diagnostics.pending_dropped() > 0);
    assert_eq!(
        std::fs::read_to_string(root.path().join("not-a-directory")).unwrap(),
        "keep"
    );
}
