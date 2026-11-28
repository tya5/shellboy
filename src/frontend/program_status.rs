//! Bounded terminal metadata on existing finite read admission.
use super::native_history::Owner;
use crate::program_status::{CHILD_LIMIT, Counts, Record, Snapshot, State, clean, thread_id};
use crate::protocol::{ApiError, Command, ErrorCode, MAX_LIST_LIMIT, StatusArgs};
use crate::status::{ActivityCounts, StatusSnapshot, WorkItem};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Session {
    pub started: chrono::DateTime<chrono::Utc>,
    pub seen: BTreeSet<(String, String)>,
}
impl Session {
    pub fn new() -> Self {
        Self {
            started: chrono::Utc::now(),
            seen: BTreeSet::new(),
        }
    }

    pub fn accept(result: Result<Snapshot, ApiError>, dirty: &mut bool) -> Option<Snapshot> {
        match result {
            Ok(snapshot) => Some(snapshot),
            Err(_) => {
                *dirty = true;
                None
            }
        }
    }
}

pub(super) async fn read(
    owner: Owner<'_>,
    selected: Option<String>,
    started: chrono::DateTime<chrono::Utc>,
    seen: BTreeSet<(String, String)>,
) -> Result<Snapshot, ApiError> {
    let active: StatusSnapshot = owner
        .call(Command::Status(StatusArgs {
            limit: MAX_LIST_LIMIT,
            ..Default::default()
        }))
        .await?;
    let counts = active.activity_counts.ok_or_else(|| {
        ApiError::new(ErrorCode::IoFailed, "Owner status omitted activity counts")
    })?;
    let cutoff = started.to_rfc3339();
    let mut ended = Vec::new();
    let mut cursor = None;
    let capped = loop {
        let remaining = MAX_LIST_LIMIT - ended.len() as u32;
        let page: StatusSnapshot = owner
            .call(Command::Status(StatusArgs {
                ended_after: Some(cutoff.clone()),
                limit: remaining,
                cursor,
                ..Default::default()
            }))
            .await?;
        if page.page.items.len() > remaining as usize
            || page.page.items.iter().any(|item| item.kind != "run")
        {
            return Err(ApiError::new(ErrorCode::IoFailed, "Invalid ended Run page"));
        }
        let more = page.page.next_cursor.is_some();
        if more && page.page.items.is_empty() {
            return Err(ApiError::new(
                ErrorCode::IoFailed,
                "Empty ended Run page has a continuation",
            ));
        }
        ended.extend(page.page.items);
        if !more || ended.len() == MAX_LIST_LIMIT as usize {
            break more;
        }
        cursor = page.page.next_cursor;
    };
    Ok(project(
        counts,
        active.page.items,
        ended,
        capped,
        selected,
        &seen,
    ))
}

fn project(
    activity: ActivityCounts,
    active: Vec<WorkItem>,
    ended: Vec<WorkItem>,
    ended_capped: bool,
    selected: Option<String>,
    seen: &BTreeSet<(String, String)>,
) -> Snapshot {
    let mut counts = Counts {
        working: activity.running.count.into(),
        working_capped: activity.running.capped,
        awaiting_you: activity.awaiting_you.count.into(),
        awaiting_you_capped: activity.awaiting_you.capped,
        done_capped: ended_capped,
        ..Default::default()
    };
    let mut children = BTreeMap::<String, Record>::new();
    let mut add = |item: &WorkItem, state, auth, msg: &str| {
        if children
            .get(&item.thread_id)
            .is_some_and(|record| record.state >= state)
        {
            return;
        }
        children.insert(
            item.thread_id.clone(),
            Record {
                id: Some(thread_id(&item.thread_id)),
                state,
                auth,
                title: clean(&item.thread_title, crate::program_status::TITLE_BYTES),
                msg: clean(msg, crate::program_status::MSG_BYTES),
            },
        );
    };
    for item in &active {
        if item.kind == "turn" && item.state == "recovery_wait" {
            add(
                item,
                State::Blocked,
                item.authentication_required,
                item.message.as_deref().unwrap_or("Recovery required"),
            );
        } else if item.kind == "run" {
            add(item, State::Working, false, &item.state);
        }
    }
    let mut viewed = None;
    for item in &ended {
        if item.state == "failed" {
            add(item, State::Error, false, "Failed");
        } else if item.state == "completed" {
            let identity = (item.thread_id.clone(), item.id.clone());
            if seen.contains(&identity) {
                continue;
            }
            if selected.as_ref() == Some(&item.thread_id) && viewed.is_none() {
                viewed = Some(identity);
                continue;
            }
            counts.done += 1;
            add(item, State::Done, false, "Completed");
        }
    }
    let mut children = children.into_values().collect::<Vec<_>>();
    for record in &children {
        match record.state {
            State::Blocked => {
                counts.blocked += 1;
                counts.auth += u64::from(record.auth);
            }
            State::Error => counts.error += 1,
            _ => {}
        }
    }
    children.sort_by_key(|record| std::cmp::Reverse(record.state));
    children.truncate(CHILD_LIMIT);
    Snapshot {
        selected,
        viewed,
        counts,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_runs_are_acknowledged_by_exact_identity_without_sending_seen() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch(
            "INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)
            VALUES('t','Title','/','','p','m','2026-11-24T00:00:00Z');
            INSERT INTO run(id,thread_id,state,created_at,ended_at) VALUES
              ('old','t','completed','2026-11-24T00:00:00Z','2026-11-24T00:00:01Z'),
              ('new','t','completed','2026-11-24T00:00:01Z','2026-11-24T00:00:02Z');",
        )
        .unwrap();
        let page = crate::status::snapshot(
            &db,
            &StatusArgs {
                ended_after: Some("2026-11-24T00:00:00Z".into()),
                ..Default::default()
            },
            "fixture",
            "fixture.0",
        )
        .unwrap();
        let counts = page.activity_counts.unwrap();
        let items = page.page.items;
        let mut seen = BTreeSet::from([("t".to_owned(), "old".to_owned())]);
        let selected = project(
            counts.clone(),
            Vec::new(),
            items.clone(),
            true,
            Some("t".into()),
            &seen,
        );
        let new = ("t".to_owned(), "new".to_owned());
        assert_eq!(selected.viewed.as_ref(), Some(&new));
        let away = project(counts.clone(), Vec::new(), items.clone(), true, None, &seen);
        assert_eq!(away.counts.done, 1);
        assert!(away.counts.done_capped);
        seen.insert(new);
        let after = project(counts, Vec::new(), items, false, None, &seen);
        assert_eq!(after.counts.done, 0);
        assert!(!after.counts.done_capped);
    }

    #[test]
    fn failed_observation_retries_at_existing_admission() {
        let mut dirty = false;
        let result = Err(ApiError::new(ErrorCode::IoFailed, "fixture"));
        assert!(Session::accept(result, &mut dirty).is_none());
        assert!(dirty);
    }
}
