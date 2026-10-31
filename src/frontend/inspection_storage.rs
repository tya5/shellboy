//! Initial explicit storage view; reactive DB and physical sampling stay separate.
use crate::{
    control::Control,
    gc::observation,
    protocol::{ApiError, ErrorCode},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) async fn request(
    control: &Arc<Control>,
    stop: CancellationToken,
) -> Result<Value, ApiError> {
    let retention = control.store.config.retention.clone();
    let raw = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop.clone(),
        move |db| {
            observation::read_tx(
                db,
                observation::Limits::default(),
                &retention,
                chrono::Utc::now(),
            )
            .map_err(Into::into)
        },
    )
    .await?;
    let saved = super::presentation_work(move || serde_json::to_value(observation::prepare(raw)))
        .await?
        .map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Cannot prepare saved storage facts",
            )
        })?;
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_millis(control.store.config.io.file_prepare_timeout_ms);
    let physical = control.gc.sample_observation_physical(deadline, stop).await;
    super::presentation_work(move|| {
        let mut value=saved;
        value["physical_observation"]=match physical {
            Ok(sample)=>json!({"sample":sample,"available":true,"stale":false}),
            Err(error)=>json!({"sample":null,"available":false,"stale":true,"refresh_error":error}),
        };
        let observed=value["observed_at"].clone();
        value["view_observation"]=json!({"observed_at":observed,"scope":"saved storage facts; physical sample has its own time"});
        Ok(value)
    }).await?
}
