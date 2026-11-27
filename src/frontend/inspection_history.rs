//! Native saved reads over the selected endpoint.
use crate::protocol::{ApiError, Command, ErrorCode, HistoryBody, HistoryReadArgs};
pub(super) async fn native_body(
    data_dir: &std::path::Path,
    args: HistoryReadArgs,
) -> Result<HistoryBody, ApiError> {
    native_read(data_dir, Command::HistoryRead(args)).await
}

pub(super) async fn native_read<T: serde::de::DeserializeOwned>(
    data_dir: &std::path::Path,
    command: Command,
) -> Result<T, ApiError> {
    let response = crate::ipc::call_selected(data_dir, command).await?;
    let malformed = || {
        ApiError::new(
            ErrorCode::ProtocolMismatch,
            "Invalid native saved-read response",
        )
    };
    if !response.ok {
        return Err(response.error.unwrap_or_else(malformed));
    }
    serde_json::from_value(response.result.ok_or_else(malformed)?).map_err(|_| malformed())
}
