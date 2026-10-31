//! Saved review facts only; the dedicated confirmation still requests exact review.
use crate::{
    control::Control,
    deletion::observation,
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
    let (raw, at) = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop,
        move |db| {
            let raw = observation::read_tx(db, &args)?;
            Ok((raw, chrono::Utc::now().to_rfc3339()))
        },
    )
    .await?;
    super::presentation_work(move|| {
        let mut value=observation::prepare(raw);
        value["view_observation"]=json!({"thread_id":thread,"observed_at":at,"scope":"saved deletion facts; not an execution authorization"});
        Ok(value)
    }).await?
}
