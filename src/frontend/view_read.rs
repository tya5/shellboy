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
    async fn closed(&self) {
        match (&self.browser, &self.descendants) {
            (None, None) => {}
            (Some(browser), None) => browser.stop.cancelled().await,
            (None, Some(descendants)) => descendants.stop.cancelled().await,
            (Some(browser), Some(descendants)) => {
                tokio::join!(browser.stop.cancelled(), descendants.stop.cancelled());
            }
        }
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
        _=tickets.closed()=>return Err(ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read closed")),
        permit=progress_slots.acquire_owned()=>permit.map_err(|_|ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read admission closed"))?,
    };
    let collect = tickets.clone();
    let active = tickets.clone();
    let result = super::snapshot::read_sequenced_guarded(
        control.store.database.path().to_path_buf(),
        stop,
        move || active.active(),
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

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    use tokio::sync::Semaphore;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn closed_visible_consumers_do_not_wait_for_read_admission() {
        crate::test_watchdog::watchdog("closed visible read admission", async {
            let root=tempfile::tempdir().unwrap();
            let owner=crate::app::DataOwner::start(None,Some(root.path().join("data")),root.path().into()).await.unwrap();
            let control=Control::new(&owner,root.path().into());
            let slots=std::sync::Arc::new(Semaphore::new(0));
            let stop=CancellationToken::new();
            let empty=Tickets::default();
            let empty_ready={
                let mut pending=std::pin::pin!(read(&control,&empty,slots.clone(),stop.clone()));
                matches!(pending.as_mut().poll(&mut Context::from_waker(Waker::noop())),Poll::Ready(Err(error)) if error.code==crate::protocol::ErrorCode::InvalidState)
            };
            let mut browser=browser::Browser::default();
            browser.observe(1,"".into(),Some("t".into()),true).unwrap();
            let mut descendants=lineage_observer::Observer::default();
            descendants.observe(1,vec!["t".into()]).unwrap();
            let tickets=Tickets { browser:Some(browser.take(tokio::time::Instant::now()).unwrap()), descendants:Some(descendants.take(tokio::time::Instant::now()).unwrap()) };
            let (single_stays_pending,all_closed_ready)={
                let mut pending=std::pin::pin!(read(&control,&tickets,slots.clone(),stop));
                assert!(pending.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
                tickets.browser.as_ref().unwrap().stop.cancel();
                let single=pending.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending();
                tickets.descendants.as_ref().unwrap().stop.cancel();
                let closed=matches!(pending.as_mut().poll(&mut Context::from_waker(Waker::noop())),Poll::Ready(Err(error)) if error.code==crate::protocol::ErrorCode::InvalidState);
                (single,closed)
            };
            assert_eq!(slots.available_permits(),0);
            drop(control);owner.shutdown().await.unwrap();
            assert!(empty_ready,"an empty visible plan must not wait for the shared read slot");
            assert!(single_stays_pending,"a remaining visible consumer still owns the shared read");
            assert!(all_closed_ready,"closing all consumers must retire a slot waiter immediately");
        }).await.unwrap();
    }
    #[test]
    fn closed_watch_does_not_discard_a_visible_descendant_count() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('root','parent','/','','p','m','now'); INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('child','root','child','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','child','live','now');").unwrap();
        let mut browser = browser::Browser::default();
        browser
            .observe_view(
                1,
                "".into(),
                Some("child".into()),
                true,
                crate::ui::watch::BrowserMode::Watch,
                Some("root".into()),
                vec!["child".into()],
                vec!["root".into()],
            )
            .unwrap();
        let mut descendants = lineage_observer::Observer::default();
        descendants.observe(1, vec!["root".into()]).unwrap();
        let tickets = Tickets {
            browser: Some(browser.take(tokio::time::Instant::now()).unwrap()),
            descendants: Some(descendants.take(tokio::time::Instant::now()).unwrap()),
        };
        tickets.browser.as_ref().unwrap().stop.cancel();
        let statements = super::super::sql_trace::Statements::new(&db);
        let raw = read_tx(&db, &tickets).unwrap();
        assert!(raw.browser.is_none());
        assert_eq!(
            statements.count(),
            1,
            "only the still-visible root count may query SQLite"
        );
        let prepared = prepare(raw, 7, &tickets);
        assert!(prepared.browser.is_none());
        let counts = prepared.descendants.unwrap().unwrap();
        assert_eq!(counts.queried_roots, 1);
        assert_eq!(counts.snapshots[0].threads, ["child"]);
        assert_eq!(counts.snapshots[0].active, 1);
    }
}
