//! Paired real-validator guards: classification cannot relax adoption or reorder checks.
use super::*;
use discard::{Outcome, RejectionReason as R};

#[tokio::test]
async fn validator_preserves_first_rejection_and_does_not_write() {
    for (reason, mutation) in [
        (
            R::ThreadUnavailable,
            "UPDATE thread SET deleted_at='now' WHERE id='t'",
        ),
        (
            R::SelectionChanged,
            "UPDATE thread SET selection_revision=selection_revision+1,profile_revision=profile_revision+1,reasoning_epoch=reasoning_epoch+1 WHERE id='t'",
        ),
        (
            R::ProfileChanged,
            "UPDATE thread SET profile_revision=profile_revision+1,reasoning_epoch=reasoning_epoch+1 WHERE id='t'",
        ),
        (
            R::EpochChanged,
            "UPDATE thread SET reasoning_epoch=reasoning_epoch+1,summary_max_chars=9000 WHERE id='t'",
        ),
        (
            R::SummarySettingsChanged,
            "UPDATE thread SET summary_max_chars=9000 WHERE id='t'",
        ),
        (
            R::RunStarted,
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now')",
        ),
        (
            R::RunStopped,
            "UPDATE run SET state='stopping' WHERE id='r'",
        ),
        (
            R::TurnStarted,
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now')",
        ),
        (
            R::TurnChanged,
            "UPDATE turn SET generation=generation+1 WHERE id='u'",
        ),
        (
            R::SourceChanged,
            "UPDATE node SET payload_revision=payload_revision+1 WHERE id='n'",
        ),
    ] {
        let (_root, db) = tests::database().await;
        db.call(move |db| {
            let tx = db.transaction()?;
            history::append_node(&tx, "t", "n", None, "message", &json!({"role":"user","content":"original"}), "now")?;
            if matches!(reason, R::RunStopped | R::TurnChanged) {
                tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now')")?;
            }
            if reason == R::TurnChanged {
                tx.execute_batch("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now')")?;
            }
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let compactor = Compactor::new(db.clone(), Arc::new(Config::default()));
        let snapshot = compactor.snapshot("t", true).await.unwrap();
        db.call(move |db| {
            db.execute_batch(mutation)?;
            // Capture after the injected edit: validation itself must be read-only.
            let before = db.total_changes();
            let tx = db.transaction()?;
            let error = validate(&tx, &snapshot).unwrap_err().into_api_error();
            assert_eq!(error.code, ErrorCode::InvalidState);
            assert_eq!(discard::cause(&error), Outcome::Rejected { reason });
            assert!(!discard::cause(&error).deterministic());
            assert_eq!(tx.total_changes(), before);
            assert_eq!(tx.query_row("SELECT count(*) FROM replacement", [], |r| r.get::<_, i64>(0))?, 0);
            assert_eq!(tx.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'compact.deterministic_range_failure.%'", [], |r| r.get::<_, i64>(0))?, 0);
            Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
    }
}
