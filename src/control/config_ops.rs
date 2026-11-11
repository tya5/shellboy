//! `config.get` and `config.set`: file work on the owner's bounded control-file pool.
//! Live connection adoption is serialized with configuration and credential writes.
use super::*;
use crate::protocol::{ConfigSetArgs, MAX_FRAME_BYTES};
use tokio::time::{Duration, Instant};

/// Keeps room for the response envelope around the largest allowed read.
const MAX_READ_RESULT_BYTES: usize = MAX_FRAME_BYTES - 1024 * 1024;
const MIN_FILE_TIMEOUT_MS: u64 = 1_000;

impl Control {
    pub(super) fn config_deadline(&self) -> Instant {
        // A tiny configured value must not make every settings call time out.
        let millis = self
            .store
            .config
            .io
            .file_prepare_timeout_ms
            .max(MIN_FILE_TIMEOUT_MS);
        Instant::now() + Duration::from_millis(millis)
    }

    pub(super) async fn config_get(
        &self,
        disconnected: CancellationToken,
    ) -> Result<Value, ApiError> {
        let config = self.store.config.clone();
        let result = self
            .attachments
            .control_files()
            .execute(self.config_deadline(), disconnected, move || {
                crate::config_edit::read(&config)
            })
            .await?;
        if serde_json::to_vec(&result).map_or(usize::MAX, |bytes| bytes.len())
            > MAX_READ_RESULT_BYTES
        {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "The configuration is too large for one response",
            ));
        }
        Ok(result)
    }

    /// Not cancellable: a started write is finished and reported by its saved outcome.
    /// The receipt holds the caller and the result, which has no sensitive value.
    pub(super) async fn config_set(
        &self,
        args: ConfigSetArgs,
        caller: Option<&Origin>,
    ) -> Result<Value, ApiError> {
        let deadline = self.config_deadline();
        let path = self
            .store
            .config
            .source
            .as_ref()
            .map(|source| source.path.clone())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "This owner was started without a configuration path",
                )
            })?;
        let guard = tokio::time::timeout_at(deadline, self.config_write.clone().lock_owned())
            .await
            .map_err(|_| {
                ApiError::new(
                    ErrorCode::IoFailed,
                    "Another configuration change is still running; nothing was written",
                )
                .details(json!({"written":false}))
            })?;
        let config = self.store.config.clone();
        let clients = self.store.clients.clone();
        let applied = self
            .attachments
            .control_files()
            .execute(deadline, CancellationToken::new(), move || {
                // A timed-out caller stops waiting, but the next change must not start
                // until this one has really ended, so the closure owns the guard.
                let _guard = guard;
                crate::config_edit::apply_live(&path, &args.expected_hash, &args.changes, &config, &clients)
            })
            .await
            .map_err(|error| {
                if error.code == ErrorCode::IoFailed && error.details.is_none() {
                    // Only the worker wait can fail without details, and it cannot say
                    // whether the closure has finished.
                    error.hint("The change may still complete. Read shellboy config get before repeating it")
                } else {
                    error
                }
            })?;
        let mut result = applied.result;
        if applied.written {
            let ids = caller.map_or_else(crate::diagnostics::DiagnosticIds::default, |origin| {
                crate::diagnostics::DiagnosticIds {
                    attempt_id: None,
                    thread_id: Some(origin.thread_id.clone()),
                    run_id: Some(origin.run_id.clone()),
                    turn_id: Some(origin.turn_id.clone()),
                    execution_id: Some(origin.execution_id.clone()),
                }
            });
            self.diagnostics.record_config_change(ids, applied.fields);
        }
        result["changed_by"] = json!(caller.map(Origin::identity));
        Ok(result)
    }
}
