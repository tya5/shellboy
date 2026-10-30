//! Reuse the Compactor's API-free maximum edit; retry admission stays in retry.rs.
use super::*;

pub(super) async fn reduce(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    failure: &mut model::http::AttemptFailure,
) -> Result<Value, ApiError> {
    if slot.stop.is_cancelled() {
        return Ok(json!({"outcome":"cancelled"}));
    }
    match control
        .compactor
        .trim_activated(&snapshot.thread, slot.stop.clone())
        .await
    {
        Ok(result) => {
            if !result.compact.no_target {
                control.changed(&snapshot.thread);
                Ok(json!({"outcome":"adopted","result":result}))
            } else {
                Ok(json!({"outcome":"no_safe_reduction","result":result}))
            }
        }
        Err(error) if error.is_storage_failure() => Err(error),
        Err(error) => {
            failure
                .error
                .message
                .push_str(&format!("; automatic context trim not adopted: {error}"));
            Ok(json!({"outcome":"rejected","error":error}))
        }
    }
}
