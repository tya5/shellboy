//! Owner-wide saved Thread hints; live text is not part of this subscription.
use crate::protocol::{
    ApiError, Command, DEFAULT_WAIT_TIMEOUT_MS, ErrorCode, EventsWaitArgs, EventsWaitResult,
};
use std::path::Path;

pub(super) async fn wait(
    data_dir: &Path,
    instance: &str,
    after: Option<String>,
    more: bool,
) -> Result<EventsWaitResult, ApiError> {
    wait_scoped(data_dir, instance, after, more, None).await
}

pub(super) async fn wait_scoped(
    data_dir: &Path,
    instance: &str,
    after: Option<String>,
    more: bool,
    thread: Option<String>,
) -> Result<EventsWaitResult, ApiError> {
    let include_text = thread.is_some();
    let timeout_ms = if after.is_none() || more {
        0
    } else {
        DEFAULT_WAIT_TIMEOUT_MS
    };
    let response = crate::ipc::call_selected_operation(
        data_dir,
        instance,
        Command::EventsWait(EventsWaitArgs {
            after,
            thread_ids: thread.into_iter().collect(),
            timeout_ms,
            include_text,
            include_reasoning_activity: false,
        }),
        None,
    )
    .await?;
    let malformed = || ApiError::new(ErrorCode::ProtocolMismatch, "Invalid events response");
    if !response.ok {
        return Err(response.error.unwrap_or_else(malformed));
    }
    serde_json::from_value(response.result.ok_or_else(malformed)?).map_err(|_| malformed())
}

/// Retire the old page ticket. Its normal first-page path supplies the bounded read.
pub(super) fn schedule_reload(
    page: &EventsWaitResult,
    browser: &mut super::browser::Browser,
) -> bool {
    use crate::protocol::EventsWaitStatus;
    match page.status {
        EventsWaitStatus::ResyncRequired => browser.request_resync_localized(
            "会話一覧の変更通知を取りこぼしました。最新のページから再取得します",
            "Conversation-list changes were missed. Reloading from the newest page",
        ),
        EventsWaitStatus::Baseline => browser
            .request_resync_localized("会話一覧を再取得します", "Reloading the conversation list"),
        _ => return false,
    }
    true
}

#[cfg(test)]
pub(super) mod tests;
