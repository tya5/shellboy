//! Saved inspection composition; domain readers retain their own limits/policy.
use super::header;
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::InspectionTarget,
};
use rusqlite::Connection;
use serde_json::Value;
use std::sync::Arc;
pub(super) struct Dependencies {
    pub thread: String,
    pub parent_thread: Option<String>,
}
pub(super) struct PreparedInspection {
    pub target: InspectionTarget,
    pub dependencies: Dependencies,
    pub value: Value,
    pub observed_at: String,
    pub limited_fields: Vec<String>,
}
pub(super) struct RawInspection {
    target: InspectionTarget,
    body: Body,
    at: String,
}
enum Body {
    Thread(
        Arc<header::RawHeader>,
        Box<crate::thread_observation::RawLatest>,
        Option<Box<crate::readback::RawCompactions>>,
    ),
    Lifecycle(Box<super::inspection_lifecycle::Raw>),
    Saved(super::inspection_saved::RawSaved, Option<u64>),
    Attachments(super::inspection_attachments::Raw),
    Operation(super::inspection_operation::RawOperation),
    Storage(crate::gc::observation::RawStorageObservation),
    Deletion(crate::deletion::observation::RawObservation),
    Index(
        Box<(
            crate::index::saved_view::RawIndexSaved,
            Result<crate::index::saved_view::RawCoverageWindow, ApiError>,
        )>,
    ),
}
#[derive(Clone)]
pub(super) struct Services {
    pub instance_id: String,
    pub config: Arc<crate::config::Config>,
    pub index: Arc<crate::index::IndexService>,
}
impl Services {
    pub fn new(control: &crate::control::Control) -> Self {
        Self {
            instance_id: control.store.instance_id.clone(),
            config: control.store.config.clone(),
            index: control.index.clone(),
        }
    }
}
fn unavailable() -> StorageError {
    ApiError::new(
        ErrorCode::InvalidState,
        "Saved inspection services are unavailable",
    )
    .into()
}
pub(super) fn read_scoped_tx(
    db: &Connection,
    target: &InspectionTarget,
    known_header: Option<&Arc<header::RawHeader>>,
    services: Option<&Services>,
    coverage: Option<&Result<Arc<crate::index::saved_view::PreparedCoverageRequest>, ApiError>>,
) -> Result<RawInspection, StorageError> {
    use crate::protocol::*;
    let services = services.ok_or_else(unavailable)?;
    let body =
        match target {
            InspectionTarget::Usage(_) => {
                return Err(ApiError::new(
                    ErrorCode::InvalidState,
                    "Usage has a separate saved-read ticket",
                )
                .into());
            }
            InspectionTarget::Thread(id) => {
                let header = match known_header.filter(|header| header.thread_id() == id) {
                    Some(header) => Arc::clone(header),
                    None => Arc::new(header::read_tx(db, id)?),
                };
                let compact = crate::readback::read_last_compaction_display_tx(db, id)?;
                let latest = crate::thread_observation::read_latest_display_tx(db, id)?;
                Body::Thread(header, Box::new(latest), compact.map(Box::new))
            }
            InspectionTarget::Run(id) => {
                Body::Lifecycle(Box::new(super::inspection_lifecycle::read_tx(
                    db,
                    &Command::RunGet(RunTarget { run_id: id.clone() }),
                )?))
            }
            InspectionTarget::Turn(id) => {
                Body::Lifecycle(Box::new(super::inspection_lifecycle::read_tx(
                    db,
                    &Command::TurnGet(TurnTarget {
                        turn_id: id.clone(),
                    }),
                )?))
            }
            InspectionTarget::Shell(id) => Body::Saved(
                super::inspection_saved::read_tx(
                    db,
                    &Command::ShellGet(ExecutionTarget {
                        execution_id: id.clone(),
                    }),
                    None,
                )?,
                services.config.limits.revisit_max_per_run,
            ),
            InspectionTarget::Message(id) => Body::Saved(
                super::inspection_saved::read_tx(
                    db,
                    &Command::MessageGet(MessageTarget {
                        message_id: id.clone(),
                    }),
                    None,
                )?,
                services.config.limits.revisit_max_per_run,
            ),
            InspectionTarget::Revisit(id) => Body::Saved(
                super::inspection_saved::read_tx(
                    db,
                    &Command::RevisitGet(RevisitArgs {
                        run_id: Some(id.clone()),
                    }),
                    None,
                )?,
                services.config.limits.revisit_max_per_run,
            ),
            InspectionTarget::Attachments {
                thread_id,
                attachment_id,
            } => Body::Attachments(match attachment_id {
                Some(id) => super::inspection_attachments::read_selected_tx(db, thread_id, id)?,
                None => super::inspection_attachments::read_tx(db, thread_id)?,
            }),
            InspectionTarget::Operation(id) => Body::Operation(
                super::inspection_operation::read_tx(db, id, &services.instance_id)?,
            ),
            InspectionTarget::Storage => Body::Storage(crate::gc::observation::read_tx(
                db,
                crate::gc::observation::Limits::default(),
                &services.config.retention,
                chrono::Utc::now(),
            )?),
            InspectionTarget::Deletion(args) => {
                Body::Deletion(crate::deletion::observation::read_tx(db, args)?)
            }
            InspectionTarget::Index(args) => {
                let saved = services.index.read_saved_tx(
                    db,
                    args,
                    crate::index::saved_view::SavedReadLimits::default(),
                )?;
                let window = coverage
                    .ok_or_else(|| {
                        ApiError::new(ErrorCode::InvalidState, "Coverage request is not prepared")
                    })
                    .and_then(|request| request.clone())
                    .and_then(|request| services.index.read_prepared_coverage_tx(db, &request));
                if let Err(error) = &window
                    && error.is_storage_failure()
                {
                    return Err(error.clone().into());
                }
                Body::Index(Box::new((saved, window)))
            }
        };
    Ok(RawInspection {
        target: target.clone(),
        body,
        at: chrono::Utc::now().to_rfc3339(),
    })
}
pub(super) fn prepare(raw: RawInspection) -> Result<PreparedInspection, ApiError> {
    let mut limited_fields = Vec::new();
    let mut observed_at = raw.at;
    let target = raw.target;
    let (thread, value) = match raw.body {
        Body::Thread(header, latest, compactions) => {
            let (mut details, limited) = header.prepare_inspection();
            limited_fields = limited;
            let latest = crate::thread_observation::prepare_latest_display(*latest);
            limited_fields.extend(latest.limited_fields);
            details.last_answer = latest.last_answer;
            details.last_input = latest.last_input;
            if let Some(compactions) = compactions {
                let compactions = crate::readback::prepare_compactions(*compactions)?;
                details.last_compaction = compactions["items"]
                    .as_array()
                    .and_then(|rows| rows.first())
                    .cloned();
            } else {
                limited_fields.push("last_compaction".into());
            }
            (
                details.thread_id.clone(),
                serde_json::to_value(details).map_err(|_| {
                    ApiError::new(ErrorCode::InvalidState, "Cannot prepare Thread facts")
                })?,
            )
        }
        Body::Lifecycle(raw) => {
            let prepared = super::inspection_lifecycle::prepare(*raw)?;
            limited_fields = prepared.limited_fields;
            observed_at = prepared.observed_at;
            (prepared.thread.unwrap_or_default(), prepared.value)
        }
        Body::Saved(raw, limit) => {
            let prepared = super::inspection_saved::prepare(raw, limit)?;
            limited_fields = prepared.limited_fields;
            observed_at = prepared.observed_at;
            (prepared.thread.unwrap_or_default(), prepared.value)
        }
        Body::Operation(raw) => {
            let prepared = super::inspection_operation::prepare(raw)?;
            limited_fields = prepared.limited_fields;
            observed_at = prepared.observed_at;
            (prepared.thread.unwrap_or_default(), prepared.value)
        }
        Body::Attachments(raw) => {
            let (value, limited) = super::inspection_attachments::prepare(raw);
            if limited {
                limited_fields.push("attachments".into());
            }
            (
                value["thread_id"].as_str().unwrap_or_default().into(),
                value,
            )
        }
        Body::Storage(raw) => (
            String::new(),
            serde_json::to_value(crate::gc::observation::prepare(raw)).map_err(|_| {
                ApiError::new(ErrorCode::InvalidState, "Cannot prepare storage facts")
            })?,
        ),
        Body::Deletion(raw) => (String::new(), crate::deletion::observation::prepare(raw)),
        Body::Index(raw) => {
            let (saved, coverage) = *raw;
            (
                String::new(),
                super::inspection_index::prepare(saved, coverage, observed_at.clone())?,
            )
        }
    };
    let parent_thread = value["parent_id"].as_str().map(str::to_owned);
    Ok(PreparedInspection {
        target,
        dependencies: Dependencies {
            thread,
            parent_thread,
        },
        value,
        observed_at,
        limited_fields,
    })
}
