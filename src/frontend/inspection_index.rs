//! One bounded saved index snapshot. No provider or source-admission side effects.
use crate::{
    control::Control,
    index::saved_view,
    protocol::{ApiError, IndexArgs},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) async fn request(
    control: &Arc<Control>,
    args: IndexArgs,
    stop: CancellationToken,
) -> Result<Value, ApiError> {
    let index = control.index.clone();
    let request_args = args.clone();
    let prepared = super::presentation_work(move || {
        index.prepare_coverage_request(&request_args, None, saved_view::SavedReadLimits::default())
    })
    .await?;
    let index = control.index.clone();
    let (raw, coverage, at) = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop,
        move |db| {
            let raw = index.read_saved_tx(db, &args, saved_view::SavedReadLimits::default())?;
            let coverage =
                prepared.and_then(|request| index.read_prepared_coverage_tx(db, &request));
            if let Err(error) = &coverage
                && error.is_storage_failure()
            {
                return Err(error.clone().into());
            }
            Ok((raw, coverage, chrono::Utc::now().to_rfc3339()))
        },
    )
    .await?;
    super::presentation_work(move || {
        let mut management = saved_view::prepare_saved(raw);
        let mut window = match coverage.and_then(saved_view::prepare_coverage_window) {
            Ok(window) => window,
            Err(error) => json!({"available": false, "current": false, "error": error}),
        };
        let counts = window
            .as_object_mut()
            .and_then(|map| map.remove("coverage"))
            .unwrap_or(Value::Null);
        management["coverage"] = if window["window_complete"] == true {
            counts
        } else {
            window["observed_lower_bounds"] = counts.clone();
            counts
                .as_object()
                .map(|map| {
                    Value::Object(map.keys().map(|key| (key.clone(), Value::Null)).collect())
                })
                .unwrap_or(Value::Null)
        };
        management["coverage_window"] = window;
        management["coverage_scope"] = json!("current finite window only; not global totals");
        management["definitions"] = saved_view::coverage_definitions();
        Ok(json!({
            "management": management,
            "view_observation": {
                "observed_at": at,
                "scope": "saved index metadata and finite current coverage window"
            }
        }))
    })
    .await?
}
