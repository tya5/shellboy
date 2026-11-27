//! Visible browser and descendant observations use independent owner read sequences.
//! Admission, task ownership and cancellation remain in the existing frontend.
#[cfg(test)]
use super::Control;
use super::{ApiError, browser, lineage_observer};
#[cfg(test)]
use {crate::storage::StorageError, rusqlite::Connection};

#[derive(Clone, Default)]
pub(super) struct Tickets {
    pub browser: Option<browser::Ticket>,
    pub descendants: Option<lineage_observer::Ticket>,
}
impl Tickets {
    pub fn is_empty(&self) -> bool {
        self.browser.is_none() && self.descendants.is_none()
    }
    async fn closed(&self) {
        async fn closed(stop: Option<&tokio_util::sync::CancellationToken>) {
            if let Some(stop) = stop {
                stop.cancelled().await;
            }
        }
        tokio::join!(
            closed(self.browser.as_ref().map(|ticket| &ticket.stop)),
            closed(self.descendants.as_ref().map(|ticket| &ticket.stop)),
        );
    }
}
#[cfg(test)]
pub(super) struct RawBatch {
    browser: Option<Result<browser::RawObservation, ApiError>>,
    #[cfg(test)]
    descendants: Option<Result<lineage_observer::RawBatch, ApiError>>,
    cold_lineage: Option<(std::time::Duration, usize, bool)>,
}
pub(super) struct Prepared {
    pub browser: Option<Result<browser::Observation, ApiError>>,
    pub descendants: Option<Result<lineage_observer::Prepared, ApiError>>,
    cold_lineage: Option<(std::time::Duration, usize, bool)>,
    descendant_sequence: Option<u64>,
}
#[cfg(test)]
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
#[cfg(test)]
fn read_tx(db: &Connection, tickets: &Tickets) -> Result<RawBatch, StorageError> {
    collect_tx(db, tickets, None)
}

#[cfg(test)]
fn collect_tx(
    db: &Connection,
    tickets: &Tickets,
    membership: Option<(bool, Vec<String>)>,
) -> Result<RawBatch, StorageError> {
    #[cfg(test)]
    let mut cold_lineage = None;
    #[cfg(not(test))]
    let cold_lineage = None;
    #[cfg(test)]
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
            let scope = membership.clone();
            #[cfg(test)]
            let scope = scope.or_else(|| {
                ticket.descendant_root.as_deref().and_then(|root| {
                    descendants
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .and_then(|raw| raw.scope(root))
                })
            });
            #[cfg(test)]
            let scope = if scope.is_none() && ticket.cached_scope.is_none() {
                if let Some(root) = &ticket.descendant_root {
                    let began = std::time::Instant::now();
                    let rows = super::lineage::read_roots_tx(db, std::slice::from_ref(root))?;
                    cold_lineage = Some((
                        began.elapsed(),
                        rows.iter().map(super::lineage::RawLineage::witnesses).sum(),
                        rows.iter().any(super::lineage::RawLineage::truncated),
                    ));
                    rows.first().map(super::lineage::membership)
                } else {
                    None
                }
            } else {
                scope
            };
            observed(browser::read_tx(db, ticket, scope))
        })
        .transpose()?;
    Ok(RawBatch {
        browser,
        #[cfg(test)]
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
                descendant_sequence: None,
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
    if let Some(ticket) = tickets.descendants {
        let result = prepared.descendants.take().unwrap_or_else(|| {
            Err(ApiError::new(
                crate::protocol::ErrorCode::InvalidState,
                "Descendant observation unavailable",
            ))
        });
        if let Ok(prepared) = &result {
            diagnostics.record_lineage_query(
                crate::diagnostics::DiagnosticIds::default(),
                prepared.elapsed,
                prepared.queried_roots,
                prepared.discovered,
                prepared.truncated,
            );
        }
        if let Some(completion) =
            descendants.complete(&ticket, prepared.descendant_sequence, result)
            && let Some(update) = completion.update
        {
            super::update(updates, update).await;
        }
    }
    if let Some(ticket) = tickets.browser {
        match prepared.browser.take() {
            Some(Ok(observation)) if browser.complete(&ticket) => {
                if let Some(patch) = observation.patch {
                    browser.adopt_patch(ticket.generation, patch.read_sequence, &patch.rows, None);
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
#[cfg(test)]
fn prepare(
    raw: RawBatch,
    sequence: u64,
    tickets: &Tickets,
    language: crate::config::UiLanguage,
) -> Prepared {
    Prepared {
        cold_lineage: raw.cold_lineage,
        descendant_sequence: None,
        browser: raw.browser.map(|result| {
            result.map(|raw| {
                browser::prepare_localized(
                    raw,
                    sequence,
                    tickets
                        .browser
                        .as_ref()
                        .expect("browser raw owns ticket")
                        .generation,
                    language,
                )
            })
        }),
        descendants: raw
            .descendants
            .map(|result| result.map(lineage_observer::RawBatch::prepare)),
    }
}
pub(super) async fn read(
    owner: &super::native_history::Owner<'_>,
    tickets: &Tickets,
    language: crate::config::UiLanguage,
    progress_slots: std::sync::Arc<tokio::sync::Semaphore>,
    stop: tokio_util::sync::CancellationToken,
) -> Result<(Prepared, u64), ApiError> {
    let _permit = tokio::select! { biased;
        _=stop.cancelled()=>return Err(ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read closed")),
        _=tickets.closed()=>return Err(ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read closed")),
        permit=progress_slots.acquire_owned()=>permit.map_err(|_|ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read admission closed"))?,
    };
    tokio::select! { biased;
        _ = stop.cancelled() => Err(ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read closed")),
        _ = tickets.closed() => Err(ApiError::new(crate::protocol::ErrorCode::InvalidState,"Visible read closed")),
        result = async {
            let (descendants, browser) = tokio::join!(async {
                match &tickets.descendants {
                    Some(ticket) => Some(lineage_observer::read_native(owner, ticket).await),
                    None => None,
                }
            }, async {
                match &tickets.browser {
                    Some(ticket) if ticket.mode == crate::ui::watch::BrowserMode::List => Some(browser::observe_list(owner, ticket).await.map(|p| (p, 0))),
                    Some(ticket) => Some(browser::observe_native(owner, ticket, language).await),
                    None => None,
                }
            });
            let (descendants, descendant_sequence) = match descendants {
                Some(Ok((counts, sequence))) => (Some(Ok(counts)), Some(sequence)),
                Some(Err(error)) => (Some(Err(error)), None),
                None => (None, None),
            };
            let (browser, sequence) = match browser {
                Some(Ok((value, sequence))) => (Some(Ok(value)), sequence),
                Some(Err(error)) => (Some(Err(error)), 0),
                None => (None, 0),
            };
            Ok((Prepared { browser, descendants, cold_lineage: None, descendant_sequence }, sequence))
        } => result,
    }
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
            let selected=super::super::selected_owner(&control);
            let slots=std::sync::Arc::new(Semaphore::new(0));
            let stop=CancellationToken::new();
            let empty=Tickets::default();
            let empty_ready={
                let mut pending=std::pin::pin!(read(&selected,&empty,crate::config::UiLanguage::En,slots.clone(),stop.clone()));
                matches!(pending.as_mut().poll(&mut Context::from_waker(Waker::noop())),Poll::Ready(Err(error)) if error.code==crate::protocol::ErrorCode::InvalidState)
            };
            let mut browser=browser::Browser::default();
            browser.observe(1,"".into(),Some("t".into()),true).unwrap();
            let mut descendants=lineage_observer::Observer::default();
            descendants.observe(1,vec!["t".into()]).unwrap();
            let tickets=Tickets { browser:Some(browser.take(tokio::time::Instant::now()).unwrap()), descendants:Some(descendants.take(tokio::time::Instant::now()).unwrap()) };
            let (single_stays_pending,all_closed_ready)={
                let mut pending=std::pin::pin!(read(&selected,&tickets,crate::config::UiLanguage::En,slots.clone(),stop));
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
        let raw = read_tx(&db, &tickets).unwrap();
        assert!(raw.browser.is_none());
        let prepared = prepare(raw, 7, &tickets, crate::config::UiLanguage::Ja);
        assert!(prepared.browser.is_none());
        let counts = prepared.descendants.unwrap().unwrap();
        assert_eq!(counts.queried_roots, 1);
        assert_eq!(counts.snapshots[0].threads, ["child"]);
        assert_eq!(counts.snapshots[0].active, 1);
    }
}
