//! Fixed collection of visible browser/count facts in one caller-owned snapshot.
//! Admission, task ownership and cancellation remain in the existing frontend.
use super::{ApiError, Control, browser, checked_storage, lineage_observer, presentation_work};
use crate::storage::StorageError;
use rusqlite::Connection;

#[derive(Clone, Default)]
pub(super) struct Tickets {
    pub browser: Option<browser::Ticket>,
    pub descendants: Option<lineage_observer::Ticket>,
}
impl Tickets {
    pub fn is_empty(&self) -> bool {
        self.browser.is_none() && self.descendants.is_none()
    }
    fn active(&self) -> bool {
        self.browser
            .as_ref()
            .is_some_and(|ticket| !ticket.stop.is_cancelled())
            || self
                .descendants
                .as_ref()
                .is_some_and(|ticket| !ticket.stop.is_cancelled())
    }
}
pub(super) struct RawBatch {
    browser: Option<Result<browser::RawObservation, ApiError>>,
    descendants: Option<Result<lineage_observer::RawBatch, ApiError>>,
    cold_lineage: Option<(std::time::Duration, usize, bool)>,
}
pub(super) struct Prepared {
    pub browser: Option<Result<browser::Observation, ApiError>>,
    pub descendants: Option<Result<lineage_observer::Prepared, ApiError>>,
    cold_lineage: Option<(std::time::Duration, usize, bool)>,
}
fn observed<T>(result: Result<T, StorageError>) -> Result<Result<T, ApiError>, StorageError> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(StorageError::Operation(error))
            if error.code != crate::protocol::ErrorCode::SaveFailed =>
        {
            Ok(Err(*error))
        }
        Err(error) => Err(error),
    }
}
pub(super) fn read_tx(db: &Connection, tickets: &Tickets) -> Result<RawBatch, StorageError> {
    let mut cold_lineage = None;
    let descendants = tickets
        .descendants
        .as_ref()
        .filter(|ticket| !ticket.stop.is_cancelled())
        .map(|ticket| observed(lineage_observer::read_tx(db, ticket)))
        .transpose()?;
    let browser = tickets
        .browser
        .as_ref()
        .filter(|ticket| !ticket.stop.is_cancelled())
        .map(|ticket| {
            let mut scope = ticket.descendant_root.as_deref().and_then(|root| {
                descendants
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .and_then(|raw| raw.scope(root))
            });
            if scope.is_none()
                && ticket.cached_scope.is_none()
                && let Some(root) = &ticket.descendant_root
            {
                // Cold user-open only; steady reads reuse the paired count or page
                // scope. This does not start a second count worker or timer.
                let began = std::time::Instant::now();
                let rows = super::lineage::read_roots_tx(db, std::slice::from_ref(root))?;
                cold_lineage = Some((
                    began.elapsed(),
                    rows.iter().map(super::lineage::RawLineage::witnesses).sum(),
                    rows.iter().any(super::lineage::RawLineage::truncated),
                ));
                scope = rows.first().map(super::lineage::membership);
            }
            observed(browser::read_tx(db, ticket, scope))
        })
        .transpose()?;
    Ok(RawBatch {
        browser,
        descendants,
        cold_lineage,
    })
}

pub(super) async fn complete(
    diagnostics: &crate::diagnostics::Diagnostics,
    browser: &mut browser::Browser,
    descendants: &mut lineage_observer::Observer,
    updates: &(impl super::UpdateTarget + ?Sized),
    tickets: Tickets,
    result: Result<(Prepared, u64), ApiError>,
) {
    let (mut prepared, sequence) = match result {
        Ok(value) => value,
        Err(error) => (
            Prepared {
                cold_lineage: None,
                browser: tickets.browser.as_ref().map(|_| Err(error.clone())),
                descendants: tickets.descendants.as_ref().map(|_| Err(error)),
            },
            super::message_queue::read_revision(),
        ),
    };
    if let Some((elapsed, discovered, truncated)) = prepared.cold_lineage {
        diagnostics.record_lineage_query(
            crate::diagnostics::DiagnosticIds::default(),
            elapsed,
            1,
            discovered,
            truncated,
        );
    }
    let already_read = tickets
        .browser
        .as_ref()
        .filter(|ticket| !ticket.stop.is_cancelled())
        .filter(|_| {
            prepared.browser.as_ref().is_some_and(|result| {
                result
                    .as_ref()
                    .is_ok_and(|observation| observation.patch.is_some())
            })
        })
        .map(|ticket| {
            ticket
                .ids
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    if let Some(ticket) = tickets.descendants {
        let result = prepared.descendants.take().unwrap_or_else(|| {
            Err(ApiError::new(
                crate::protocol::ErrorCode::InvalidState,
                "Descendant observation unavailable",
            ))
        });
        let success = result.is_ok();
        if let Ok(prepared) = &result {
            diagnostics.record_lineage_query(
                crate::diagnostics::DiagnosticIds::default(),
                prepared.elapsed,
                prepared.queried_roots,
                prepared.discovered,
                prepared.truncated,
            );
        }
        if let Some(completion) = descendants.complete(&ticket, sequence, result) {
            if success && let Some(generation) = browser.generation() {
                for scope in descendants.snapshots() {
                    browser.adopt_scope(generation, sequence, scope);
                }
                for id in completion.changed_members {
                    if !already_read.contains(&id) {
                        browser.changed(&id);
                    }
                }
            }
            if let Some(update) = completion.update {
                super::update(updates, update).await;
            }
        }
    }
    if let Some(ticket) = tickets.browser {
        match prepared.browser.take() {
            Some(Ok(observation)) if browser.complete(&ticket) => {
                if let Some(patch) = observation.patch {
                    browser.adopt_patch(ticket.generation, sequence, &patch.rows, None);
                    super::update(
                        updates,
                        crate::ui::UiUpdate::BrowserPatch {
                            patch: Box::new(patch),
                        },
                    )
                    .await;
                }
                if browser.details_current(&ticket)
                    && let Some(details) = observation.details
                {
                    browser.accept_details(&ticket, &details.details);
                    super::update(
                        updates,
                        crate::ui::UiUpdate::WatchDetails {
                            generation: ticket.generation,
                            read_sequence: sequence,
                            details: details.details,
                            missing_ids: details.missing_ids,
                        },
                    )
                    .await;
                }
            }
            Some(Err(error)) if browser.complete(&ticket) => {
                browser.failed(&ticket);
                super::update(
                    updates,
                    crate::ui::UiUpdate::BrowserPatchFailed {
                        generation: ticket.generation,
                        error: error.to_string(),
                    },
                )
                .await;
            }
            _ => {}
        }
    }
}
pub(super) fn prepare(raw: RawBatch, sequence: u64, tickets: &Tickets) -> Prepared {
    Prepared {
        cold_lineage: raw.cold_lineage,
        browser: raw.browser.map(|result| {
            result.map(|raw| {
                browser::prepare(
                    raw,
                    sequence,
                    tickets
                        .browser
                        .as_ref()
                        .expect("browser raw owns ticket")
                        .generation,
                )
            })
        }),
        descendants: raw
            .descendants
            .map(|result| result.map(lineage_observer::RawBatch::prepare)),
    }
}
pub(super) async fn read(
    control: &Control,
    tickets: &Tickets,
    progress_slots: std::sync::Arc<tokio::sync::Semaphore>,
    stop: tokio_util::sync::CancellationToken,
) -> Result<(Prepared, u64), ApiError> {
    let _permit = tokio::select! { biased;
        _=stop.cancelled()=>return Err(ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read closed")),
        permit=progress_slots.acquire_owned()=>permit.map_err(|_|ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read admission closed"))?,
    };
    let collect = tickets.clone();
    let result = super::snapshot::read_sequenced(
        control.store.database.path().to_path_buf(),
        stop,
        move |db, _sequence| {
            if !collect.active() {
                return Err(ApiError::new(
                    crate::protocol::ErrorCode::InvalidState,
                    "Visible read closed",
                )
                .into());
            }
            read_tx(db, &collect)
        },
    )
    .await;
    drop(_permit);
    let (raw, sequence) = checked_storage(control, result).await?;
    let project = tickets.clone();
    let prepared = presentation_work(move || prepare(raw, sequence, &project)).await?;
    Ok((prepared, sequence))
}
