//! Owner-wide saved Thread hints; live text is not part of this subscription.
use crate::protocol::{
    ApiError, Command, DEFAULT_WAIT_TIMEOUT_MS, ErrorCode, EventsWaitArgs, EventsWaitResult,
};
use std::{path::Path, time::Duration};

pub(super) async fn wait(
    data_dir: &Path,
    instance: &str,
    after: Option<String>,
    more: bool,
) -> Result<EventsWaitResult, ApiError> {
    let timeout_ms = if after.is_none() || more {
        0
    } else {
        DEFAULT_WAIT_TIMEOUT_MS
    };
    loop {
        let response = crate::ipc::call_selected_operation(
            data_dir,
            instance,
            Command::EventsWait(EventsWaitArgs {
                after: after.clone(),
                thread_ids: vec![],
                timeout_ms,
                include_text: false,
                include_reasoning_activity: false,
            }),
            None,
        )
        .await?;
        let malformed = || ApiError::new(ErrorCode::ProtocolMismatch, "Invalid events response");
        if response.ok {
            return serde_json::from_value(response.result.ok_or_else(malformed)?)
                .map_err(|_| malformed());
        }
        let error = response.error.unwrap_or_else(malformed);
        // The wait class is shared with other local clients. Retry only a definite
        // admission refusal, keeping the same owner and cursor; dropping this future stops it.
        if error.code != ErrorCode::InvalidState
            || !error.details.as_ref().is_some_and(|details| {
                details["phase"] == "admission" && details["request_may_have_been_applied"] == false
            })
        {
            return Err(error);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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

#[tokio::test]
async fn native_gap_retires_the_old_browser_page_and_reloads_one_bounded_page() {
    use crate::protocol::{DEFAULT_LIST_LIMIT, EventsWaitStatus, ThreadListArgs};
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    let (_root, owner, control, server) = super::control_tests::fixture().await;
    let directory = &owner.loaded.data_dir;
    let instance = &owner.instance_id;
    let before = wait(directory, instance, None, false).await.unwrap();
    let mut browser = super::browser::Browser::default();
    let retired = browser.begin_page(1, String::new()).unwrap().unwrap();
    owner.db.call(|db| {
        for i in 0..=DEFAULT_LIST_LIMIT {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES (?1,'gap fixture','/','','p','m','2026-01-01T00:00:00Z')", [format!("reload-{i:03}")])?;
        }
        Ok(())
    }).await.unwrap();
    control.expire_saved_event_cursor_for_test();
    let page = wait(directory, instance, Some(before.cursor), true)
        .await
        .unwrap();
    assert_eq!(page.status, EventsWaitStatus::ResyncRequired);
    assert!(page.changes.is_empty());
    assert!(schedule_reload(&page, &mut browser));
    assert!(retired.is_cancelled());
    assert_eq!(browser.take_resync().unwrap().0, 1);
    assert!(browser.take_resync().is_none());
    assert!(browser.begin_page(1, String::new()).unwrap().is_none());
    let stop = browser.begin_page(2, String::new()).unwrap().unwrap();
    let (fresh, _, _) = super::browser::page(
        &control,
        ThreadListArgs::default(),
        stop,
        Arc::new(Semaphore::new(1)),
    )
    .await
    .unwrap();
    assert_eq!(fresh.items.len(), DEFAULT_LIST_LIMIT as usize);
    assert!(fresh.items.iter().any(|row| row.title == "gap fixture"));
    assert!(
        fresh.next_cursor.is_some(),
        "gap must not load the entire list"
    );
    server.shutdown().await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
