//! Saved review facts only; the dedicated confirmation still requests exact review.
use crate::{
    control::Control,
    protocol::{ApiError, ThreadDeleteArgs},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) async fn request(
    control: &Arc<Control>,
    args: ThreadDeleteArgs,
    stop: CancellationToken,
) -> Result<Value, ApiError> {
    let thread = args.thread_id.clone();
    let (value, at) = control.scheduler_delete_preview(args, stop).await?;
    super::presentation_work(move || {
        let mut value = value;
        value["view_observation"] = json!({
            "thread_id": thread,
            "observed_at": at,
            "scope": "saved deletion facts; not an execution authorization"
        });
        Ok(value)
    })
    .await?
}
