//! Human Watch actions retain exact saved target identities through dispatch.
use super::*;
use crate::ui::watch::WatchActionKind;

pub(super) async fn execute(
    owner: &native_history::Owner<'_>,
    target: WatchActionTarget,
) -> Result<Value, ApiError> {
    if target.kind == WatchActionKind::Output {
        let execution = target.execution_id.ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidArgument,
                "Watch output target has no execution",
            )
        })?;
        return owner
            .call(Command::ShellRead(ShellReadArgs {
                expected_owner: Some(crate::protocol::ShellReadOwner {
                    thread_id: target.thread_id,
                    run_id: target.run_id,
                }),
                tail: false,
                segments: false,
                execution_id: execution,
                cursor: None,
                encoding: None,
                limit_bytes: 64 * 1024,
            }))
            .await;
    }
    let turn = match (target.turn_id, target.turn_generation) {
        (Some(turn_id), Some(generation)) => Some(ExpectedTurn {
            turn_id,
            generation,
        }),
        (None, None) if target.kind == WatchActionKind::Stop => None,
        _ => {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Invalid Watch Turn identity",
            ));
        }
    };
    let expected_selection = Some(ExpectedRunSelection {
        thread_id: target.thread_id,
        turn,
    });
    let command = match target.kind {
        WatchActionKind::Stop => Command::RunStop(RunStopTarget::Run {
            run_id: target.run_id,
            expected_selection,
        }),
        WatchActionKind::Retry => Command::RunRetry(RunRetryArgs {
            run_id: target.run_id,
            expected_turn: None,
            expected_selection,
        }),
        WatchActionKind::Output => unreachable!(),
    };
    owner.call(command).await
}
