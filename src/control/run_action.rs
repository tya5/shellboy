//! Private Watch actions retain the exact saved target shown to the human.
//! No wire command or separate mutation lifecycle is introduced.
use super::*;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExpectedRun {
    pub thread_id: String,
    pub run_id: String,
    pub turn_id: Option<String>,
    pub turn_generation: Option<u64>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum RunAction {
    Stop,
    Retry,
}
impl RunAction {
    pub(super) fn command(self, expected: &ExpectedRun) -> Command {
        let target = RunTarget {
            run_id: expected.run_id.clone(),
        };
        match self {
            Self::Stop => Command::RunStop(target),
            Self::Retry => Command::RunRetry(target),
        }
    }
}
impl ExpectedRun {
    pub(super) fn validate(&self) -> Result<(), ApiError> {
        if self.thread_id.is_empty()
            || self.thread_id.len() > 256
            || self.run_id.is_empty()
            || self.run_id.len() > 256
            || self
                .turn_id
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 256)
            || self.turn_id.is_some() != self.turn_generation.is_some()
            || self.turn_generation.is_some_and(|n| n > i64::MAX as u64)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Invalid expected Run/Turn identity",
            ));
        }
        Ok(())
    }
}
impl Control {
    /// Accepted work owns its admission even if the browser disappears.
    pub(crate) async fn expected_run_action(
        self: &Arc<Self>,
        operation_id: String,
        expected: ExpectedRun,
        action: RunAction,
    ) -> Result<Value, ApiError> {
        expected.validate()?;
        let control = self.clone();
        tokio::spawn(async move {
            let _guard = control.admit(true)?;
            control.start().await?;
            if control.stop.is_cancelled() {
                return Err(ApiError::new(
                    ErrorCode::InvalidState,
                    "Owner is shutting down",
                ));
            }
            match control
                .store
                .execute_expected_run(operation_id, expected, action)
                .await
            {
                Ok(effects) => {
                    let result = effects.result.clone();
                    control.apply_effects(Some(effects)).await;
                    Ok(result)
                }
                Err(error) => {
                    if error.is_storage_failure() {
                        control.fail_storage().await;
                    }
                    Err(error)
                }
            }
        })
        .await
        .map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Run action worker failed; inspect its operation receipt",
            )
        })?
    }
}
