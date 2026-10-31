//! Human Watch actions retain exact saved target identities through dispatch.
use super::*;
use crate::ui::watch::WatchActionKind;

pub(super) async fn execute(
    control: &Arc<Control>,
    target: WatchActionTarget,
) -> Result<Value, ApiError> {
    if target.kind == WatchActionKind::Output {
        let execution = target.execution_id.ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidArgument,
                "Watch output target has no execution",
            )
        })?;
        let expected_execution = execution.clone();
        let expected_run = target.run_id;
        let expected_thread = target.thread_id;
        let owned=super::snapshot::read(control.store.database.path().to_path_buf(),CancellationToken::new(),move|db| {
            Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE s.id=?1 AND r.id=?2 AND r.thread_id=?3)",rusqlite::params![expected_execution,expected_run,expected_thread],|row|row.get::<_,bool>(0))?)
        }).await;
        if !checked_storage(control, owned).await? {
            return Err(ApiError::new(
                ErrorCode::TargetUnavailable,
                "Watch output no longer belongs to the selected saved Run",
            ));
        }
        return request(
            control,
            Command::ShellRead(ShellReadArgs {
                execution_id: execution,
                cursor: None,
                encoding: None,
                limit_bytes: 64 * 1024,
            }),
        )
        .await;
    }
    let action = match target.kind {
        WatchActionKind::Stop => crate::control::run_action::RunAction::Stop,
        WatchActionKind::Retry => crate::control::run_action::RunAction::Retry,
        WatchActionKind::Output => unreachable!(),
    };
    let command = match target.kind {
        WatchActionKind::Stop => Command::RunStop(RunTarget {
            run_id: target.run_id.clone(),
        }),
        _ => Command::RunRetry(RunTarget {
            run_id: target.run_id.clone(),
        }),
    };
    let expected = crate::control::run_action::ExpectedRun {
        thread_id: target.thread_id,
        run_id: target.run_id,
        turn_id: target.turn_id,
        turn_generation: target.turn_generation,
    };
    control
        .expected_run_action(operation(control, command).operation_id, expected, action)
        .await
}
