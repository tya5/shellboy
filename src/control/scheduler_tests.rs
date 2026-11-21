use super::*;
use crate::{
    app::DataOwner,
    protocol::{
        Command, Request, ScheduleAddArgs, ScheduleListArgs, ThreadArchiveArgs, ThreadCreateArgs,
        ThreadDeleteArgs, ThreadForkArgs, ThreadRewindArgs,
    },
};
use chrono::{Datelike, TimeZone};
use std::time::Duration;

fn current_request(control: &Control, operation: impl AsRef<str>, command: Command) -> Request {
    Request::new(
        format!("{}:{}", control.store.instance_id, operation.as_ref()),
        command,
    )
}

fn scheduler_thread_config() -> &'static str {
    "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n"
}

async fn create_test_thread(
    control: &Arc<Control>,
    cwd: &std::path::Path,
    operation: &'static str,
) -> String {
    let response = control
        .request(current_request(
            control,
            operation,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: Some(cwd.to_string_lossy().into_owned()),
                model: None,
                effort: None,
                parent_id: None,
            }),
        ))
        .await;
    assert!(response.ok, "{response:?}");
    response.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_request_cancels_a_due_launch_waiting_on_thread_edit() {
    crate::test_watchdog::watchdog(
        "scheduler request order removes a due pending launch before the Thread edit releases",
        async {
            let root = tempfile::tempdir().unwrap();
            let config_path = root.path().join("config.toml");
            std::fs::write(&config_path, scheduler_thread_config()).unwrap();
            let owner = DataOwner::start(
                Some(config_path),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            let thread_id =
                create_test_thread(&control, root.path(), "remove-pending-thread").await;
            let marker = root.path().join("removed-schedule-launched");
            let added = control
                .request(current_request(
                    &control,
                    "remove-pending-add",
                    Command::ScheduleAdd(ScheduleAddArgs {
                        thread_id: Some(thread_id.clone()),
                        command: format!("touch '{}'", marker.display()),
                        cwd: None,
                        cron: None,
                        at: Some(chrono::Utc::now().timestamp() + 3600),
                    }),
                ))
                .await;
            assert!(added.ok, "{added:?}");
            let schedule_id = added.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();

            let boundary = control.boundary(&thread_id).await;
            let edit_guard = boundary.lock_owned().await;
            let id_for_due = schedule_id.clone();
            owner
                .db
                .call(move |db| {
                    db.execute(
                        "UPDATE scheduled_command SET next_at=?1 WHERE id=?2",
                        rusqlite::params![chrono::Utc::now().timestamp() - 1, id_for_due],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            control.changed.notify_waiters();

            let id_for_wait = schedule_id.clone();
            loop {
                let id = id_for_wait.clone();
                let due = owner
                    .db
                    .call(move |db| {
                        Ok(db.query_row(
                            "SELECT next_at IS NULL FROM scheduled_command WHERE id=?1",
                            [id],
                            |row| row.get::<_, bool>(0),
                        )?)
                    })
                    .await
                    .unwrap();
                if due {
                    break;
                }
                tokio::task::yield_now().await;
            }

            let removed = control
                .request(current_request(
                    &control,
                    "remove-pending-remove",
                    Command::ScheduleRemove(crate::protocol::ScheduleRemoveArgs {
                        schedule_id: schedule_id.clone(),
                    }),
                ))
                .await;
            assert!(removed.ok, "{removed:?}");
            assert_eq!(removed.result.unwrap()["removed"], true);
            drop(edit_guard);
            let rows = crate::storage::schedule::list(&owner.db, thread_id)
                .await
                .unwrap();
            assert!(rows.is_empty());
            control.shutdown().await.unwrap();
            assert!(
                !marker.exists(),
                "a removed pending occurrence must not launch later"
            );
            owner.shutdown().await.unwrap();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_at_runs_shared_shell_and_list_reports_latest_result() {
    crate::test_watchdog::watchdog("schedule add -> timer -> shell -> latest result -> list", async {
        let root = tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        std::fs::write(&config_path, scheduler_thread_config()).unwrap();
        let owner = DataOwner::start(Some(config_path), Some(root.path().join("data")), root.path().into())
            .await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let created=control.request(current_request(&control, "thread-create",Command::ThreadCreate(ThreadCreateArgs{agents:None,connection:None,cwd:Some(root.path().to_string_lossy().into_owned()),model:None,effort:None,parent_id:None}))).await;
        assert!(created.ok,"{created:?}");
        let thread_id=created.result.unwrap()["thread_id"].as_str().unwrap().to_owned();
        let started=root.path().join("started");
        let release=root.path().join("release");
        let finished=root.path().join("finished");
        let command=format!("touch '{}' ; while [ ! -e '{}' ]; do sleep 0.01; done; printf 'scheduled-ok\\n%s\\n%s\\n' \"$SHELLBOY_THREAD_ID\" \"$PWD\"; printf 'scheduled-stderr-final\\n' >&2; touch '{}'",started.display(),release.display(),finished.display());
        let due_at=chrono::Utc::now().timestamp()-1;
        let add=control.request(current_request(&control, "schedule-add",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(thread_id.clone()),command,cwd:None,cron:None,at:Some(due_at)}))).await;
        assert!(add.ok,"{add:?}");
        let schedule_id=add.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
        loop {
            if tokio::fs::try_exists(&started).await.unwrap() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let active_preview=control.request(Request::new("preview-active",Command::ThreadDeletePreview(ThreadDeleteArgs{thread_id:thread_id.clone(),descendants:false,preview_marker:None}))).await;
        assert!(active_preview.ok,"{active_preview:?}");
        assert_eq!(active_preview.result.unwrap()["blockers"]["scheduled_shell_count"],1);
        // The row-action blocker is derived from the canonical bounded observation;
        // when its descendant scope is incomplete, it must stay unknown.
        owner.db.call({
            let thread_id = thread_id.clone();
            move |db| {
                for index in 0..257 {
                    db.execute(
                        "INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?2,'/','agents','profile','model','now')",
                        rusqlite::params![format!("active-preview-child-{index:03}"), thread_id],
                    )?;
                }
                Ok(())
            }
        }).await.unwrap();
        let bounded_preview = control.request(Request::new(
            "preview-active-bounded-scope",
            Command::ThreadDeletePreview(ThreadDeleteArgs {
                thread_id: thread_id.clone(),
                descendants: true,
                preview_marker: None,
            }),
        )).await;
        assert!(bounded_preview.ok, "bounded preview must remain a response: {bounded_preview:?}");
        let bounded_preview = bounded_preview.result.unwrap();
        assert_eq!(bounded_preview["preview_marker_unavailable_reason"], "input_limit");
        assert_eq!(bounded_preview["blockers"]["scheduled_shell_count"], serde_json::Value::Null);
        assert_eq!(bounded_preview["deletable_now"], serde_json::Value::Null);
        let active_delete=control.request(current_request(&control, "delete-active",Command::ThreadDelete(ThreadDeleteArgs{thread_id:thread_id.clone(),descendants:false,preview_marker:None}))).await;
        assert!(!active_delete.ok,"active scheduled shell must block Thread deletion");
        assert!(active_delete.error.as_ref().is_some_and(|error|error.message.contains("scheduled shell")),"unexpected deletion blocker: {active_delete:?}");
        let archived=control.request(current_request(&control, "archive-active",Command::ThreadArchive(ThreadArchiveArgs{thread_ids:vec![thread_id.clone()],descendants:false}))).await;
        assert!(archived.ok,"{archived:?}");
        let hidden=control.request(Request::new("list-archived",Command::ScheduleList(ScheduleListArgs{thread_id:Some(thread_id.clone())}))).await;
        assert!(hidden.ok,"{hidden:?}");
        assert!(hidden.result.unwrap()["schedules"].as_array().unwrap().is_empty());
        std::fs::write(&release,"").unwrap();
        loop { if tokio::fs::try_exists(&finished).await.unwrap(){break;} tokio::time::sleep(Duration::from_millis(10)).await; }
        let unarchived=control.request(current_request(&control, "unarchive-active",Command::ThreadUnarchive(ThreadArchiveArgs{thread_ids:vec![thread_id.clone()],descendants:false}))).await;
        assert!(unarchived.ok,"{unarchived:?}");
        loop {
            let list=control.request(Request::new("schedule-list",Command::ScheduleList(ScheduleListArgs{thread_id:Some(thread_id.clone())}))).await;
            assert!(list.ok,"{list:?}");
            let schedules=list.result.unwrap()["schedules"].as_array().unwrap().clone();
            if schedules.iter().any(|row|{
                let tail=row["latest"]["output_tail"].as_str().unwrap_or("");
                row["schedule_id"]==schedule_id && row["thread_id"]==thread_id && row["cwd"].is_null()
                    && row["next_at"].is_null() && row["latest"]["status"]=="finished"
                    && row["latest"]["scheduled_at"]==due_at && tail.contains("scheduled-ok") && tail.contains("scheduled-stderr-final")
                    && tail.contains(&thread_id) && tail.contains(root.path().to_str().unwrap())
                    && row["latest"]["output_truncated"]==false
            }) { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut preview_attempt=0;
        let preview=loop {
            let operation=format!("delete-preview-after-shell-settlement-{preview_attempt}");
            preview_attempt+=1;
            let response=control.request(Request::new(operation,Command::ThreadDeletePreview(ThreadDeleteArgs{thread_id:thread_id.clone(),descendants:false,preview_marker:None}))).await;
            assert!(response.ok,"{response:?}");
            let preview=response.result.unwrap();
            if preview["blockers"]["scheduled_shell_count"]==0 { break preview; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(preview["scheduled_command_count"],1);
        let marker=preview["preview_marker"].clone();
        let deleted=control.request(current_request(&control, "thread-delete",Command::ThreadDelete(ThreadDeleteArgs{thread_id:thread_id.clone(),descendants:false,preview_marker:Some(serde_json::from_value(marker).unwrap())}))).await;
        assert!(deleted.ok,"{deleted:?}");
        let check_id=schedule_id.clone();
        let (registrations,results)=owner.db.call(move|db|Ok((
            db.query_row("SELECT count(*) FROM scheduled_command WHERE id=?1",[&check_id],|row|row.get::<_,i64>(0))?,
            db.query_row("SELECT count(*) FROM scheduled_command_result WHERE schedule_id=?1",[&check_id],|row|row.get::<_,i64>(0))?
        ))).await.unwrap();
        assert_eq!(registrations,0,"registration row must be physically cascaded");
        assert_eq!(results,0,"latest-result row must be physically cascaded");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_marks_shell_producer_incompleteness_as_truncated() {
    crate::test_watchdog::watchdog(
        "scheduled latest result reflects shared shell output completeness",
        async {
            let root = tempfile::tempdir().unwrap();
            let config_path = root.path().join("config.toml");
            let config = format!(
                "{}\n[limits]\nshell_raw_bytes=256\n",
                scheduler_thread_config()
            );
            std::fs::write(&config_path, config).unwrap();
            let owner = DataOwner::start(
                Some(config_path),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            let thread_id =
                create_test_thread(&control, root.path(), "create-incomplete-output-thread").await;
            let command = "printf '%0257d' 0".to_string();
            let due_at = chrono::Utc::now().timestamp() - 1;
            let added = control
                .request(current_request(
                    &control,
                    "schedule-incomplete-output",
                    Command::ScheduleAdd(ScheduleAddArgs {
                        thread_id: Some(thread_id.clone()),
                        command,
                        cwd: None,
                        cron: None,
                        at: Some(due_at),
                    }),
                ))
                .await;
            assert!(added.ok, "{added:?}");
            let schedule_id = added.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();
            loop {
                let listed = control
                    .request(Request::new(
                        "list-incomplete-output",
                        Command::ScheduleList(ScheduleListArgs {
                            thread_id: Some(thread_id.clone()),
                        }),
                    ))
                    .await;
                assert!(listed.ok, "{listed:?}");
                let rows = listed.result.unwrap()["schedules"]
                    .as_array()
                    .unwrap()
                    .clone();
                if let Some(row) = rows.iter().find(|row| {
                    row["schedule_id"] == schedule_id && row["latest"]["status"] == "finished"
                }) {
                    let tail = row["latest"]["output_tail"]
                        .as_str()
                        .expect("retained bounded producer bytes");
                    assert_eq!(tail.len(), 256);
                    assert!(tail.bytes().all(|byte| byte == b'0'));
                    assert_eq!(row["latest"]["output_truncated"], true);
                    assert_eq!(row["latest"]["scheduled_at"], due_at);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_thread_admission_waits_then_runs_due_row_and_dispatches_another() {
    crate::test_watchdog::watchdog(
        "ordinary Thread edit delays, but does not drop, a due scheduled command",
        async {
            let root = tempfile::tempdir().unwrap();
            let config_path = root.path().join("config.toml");
            std::fs::write(&config_path, scheduler_thread_config()).unwrap();
            let owner = DataOwner::start(
                Some(config_path),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            let delayed_root =
                create_test_thread(&control, root.path(), "create-delayed-root").await;
            let delayed_response = control
                .request(current_request(
                    &control,
                    "create-delayed-child",
                    Command::ThreadCreate(ThreadCreateArgs {
                        agents: None,
                        connection: None,
                        cwd: Some(root.path().to_string_lossy().into_owned()),
                        model: None,
                        effort: None,
                        parent_id: Some(delayed_root.clone()),
                    }),
                ))
                .await;
            assert!(delayed_response.ok, "{delayed_response:?}");
            let delayed_thread = delayed_response.result.unwrap()["thread_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let free_thread = create_test_thread(&control, root.path(), "create-free-thread").await;
            let delayed_child = root.path().join("delayed-child-ran");
            let free_child = root.path().join("free-child-ran");
            let future_at = chrono::Utc::now().timestamp() + 86_400;
            let delayed = control
                .request(current_request(
                    &control,
                    "delayed-schedule-add",
                    Command::ScheduleAdd(ScheduleAddArgs {
                        thread_id: Some(delayed_thread.clone()),
                        command: format!("touch '{}'", delayed_child.display()),
                        cwd: None,
                        cron: None,
                        at: Some(future_at),
                    }),
                ))
                .await;
            assert!(delayed.ok, "{delayed:?}");
            let delayed_id = delayed.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let free = control
                .request(current_request(
                    &control,
                    "free-schedule-add",
                    Command::ScheduleAdd(ScheduleAddArgs {
                        thread_id: Some(free_thread.clone()),
                        command: format!("touch '{}'", free_child.display()),
                        cwd: None,
                        cron: None,
                        at: Some(future_at),
                    }),
                ))
                .await;
            assert!(free.ok, "{free:?}");
            let free_id = free.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let boundary = control.boundary(&delayed_thread).await;
            let held = boundary.lock().await;
            let due_at = chrono::Utc::now().timestamp() - 1;
            let delayed_id_for_db = delayed_id.clone();
            let free_id_for_db = free_id.clone();
            owner
                .db
                .call(move |db| {
                    db.execute(
                        "UPDATE scheduled_command SET next_at=?1 WHERE id IN (?2,?3)",
                        rusqlite::params![due_at, delayed_id_for_db, free_id_for_db],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            control.changed.notify_waiters();
            loop {
                let response = control
                    .request(Request::new(
                        "list-free-delayed-schedules",
                        Command::ScheduleList(ScheduleListArgs {
                            thread_id: Some(free_thread.clone()),
                        }),
                    ))
                    .await;
                assert!(response.ok, "{response:?}");
                let rows = response.result.unwrap()["schedules"]
                    .as_array()
                    .unwrap()
                    .clone();
                if rows.iter().any(|row| {
                    row["schedule_id"] == free_id
                        && row["latest"]["status"] == "finished"
                        && row["latest"]["scheduled_at"] == due_at
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(tokio::fs::try_exists(&free_child).await.unwrap());
            assert!(
                !tokio::fs::try_exists(&delayed_child).await.unwrap(),
                "ordinary Thread edit should delay the scheduled shell, not launch it concurrently"
            );
            let preview = control
                .request(Request::new(
                    "preview-delayed-schedule",
                    Command::ThreadDeletePreview(ThreadDeleteArgs {
                        thread_id: delayed_root.clone(),
                        descendants: true,
                        preview_marker: None,
                    }),
                ))
                .await;
            assert!(preview.ok, "{preview:?}");
            assert_eq!(
                preview.result.unwrap()["blockers"]["scheduled_shell_count"],
                0,
                "queued admission is not an active shell and must not block deletion",
            );
            drop(held);
            loop {
                let response = control
                    .request(Request::new(
                        "list-delayed-schedule-result",
                        Command::ScheduleList(ScheduleListArgs {
                            thread_id: Some(delayed_thread.clone()),
                        }),
                    ))
                    .await;
                assert!(response.ok, "{response:?}");
                let rows = response.result.unwrap()["schedules"]
                    .as_array()
                    .unwrap()
                    .clone();
                if rows.iter().any(|row| {
                    row["schedule_id"] == delayed_id
                        && row["latest"]["status"] == "finished"
                        && row["latest"]["scheduled_at"] == due_at
                        && row["latest"]["exit_code"] == 0
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(tokio::fs::try_exists(&delayed_child).await.unwrap());
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_schedule_overlap_is_skipped_while_shell_is_active() {
    crate::test_watchdog::watchdog(
        "same-ID overlap uses the single owner active registry",
        async {
            let root = tempfile::tempdir().unwrap();
            let config_path = root.path().join("config.toml");
            std::fs::write(&config_path, scheduler_thread_config()).unwrap();
            let owner = DataOwner::start(
                Some(config_path),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            let thread_id = create_test_thread(&control, root.path(), "overlap-thread").await;
            let started = root.path().join("overlap-started");
            let release = root.path().join("overlap-release");
            let executions = root.path().join("overlap-executions");
            let added = control.request(current_request(
            &control,
            "overlap-schedule-add",
            Command::ScheduleAdd(ScheduleAddArgs {
                thread_id: Some(thread_id.clone()),
                command: format!(
                    "printf x >> '{}'; touch '{}'; while [ ! -e '{}' ]; do sleep 0.01; done",
                    executions.display(), started.display(), release.display()
                ),
                cwd: None,
                cron: Some("* * * * *".into()),
                at: None,
            }),
        )).await;
            assert!(added.ok, "{added:?}");
            let schedule_id = added.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let first_due = chrono::Utc::now().timestamp() - 1;
            let id_for_db = schedule_id.clone();
            owner
                .db
                .call(move |db| {
                    db.execute(
                        "UPDATE scheduled_command SET next_at=?1 WHERE id=?2",
                        rusqlite::params![first_due, id_for_db],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            control.changed.notify_waiters();
            loop {
                if tokio::fs::try_exists(&started).await.unwrap() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let overlap_due = chrono::Utc::now().timestamp() + 1;
            let id_for_db = schedule_id.clone();
            owner
                .db
                .call(move |db| {
                    db.execute(
                        "UPDATE scheduled_command SET next_at=?1 WHERE id=?2",
                        rusqlite::params![overlap_due, id_for_db],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            control.changed.notify_waiters();
            loop {
                let listed = control
                    .request(Request::new(
                        "overlap-schedule-list",
                        Command::ScheduleList(ScheduleListArgs {
                            thread_id: Some(thread_id.clone()),
                        }),
                    ))
                    .await;
                assert!(listed.ok, "{listed:?}");
                let rows = listed.result.unwrap()["schedules"]
                    .as_array()
                    .unwrap()
                    .clone();
                if rows.iter().any(|row| {
                    row["schedule_id"] == schedule_id
                        && row["latest"]["status"] == "overlap_skipped"
                        && row["latest"]["scheduled_at"] == overlap_due
                        && row["latest"]["exit_code"].is_null()
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(tokio::fs::read_to_string(&executions).await.unwrap(), "x");
            std::fs::write(&release, "").unwrap();
            loop {
                let listed = control
                    .request(Request::new(
                        "overlap-final-list",
                        Command::ScheduleList(ScheduleListArgs {
                            thread_id: Some(thread_id.clone()),
                        }),
                    ))
                    .await;
                let rows = listed.result.unwrap()["schedules"]
                    .as_array()
                    .unwrap()
                    .clone();
                if rows.iter().any(|row| {
                    row["schedule_id"] == schedule_id
                        && row["latest"]["status"] == "overlap_skipped"
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(tokio::fs::read_to_string(&executions).await.unwrap(), "x");
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_schedule_cap_is_atomic_across_threads_and_scoped_list_is_complete() {
    crate::test_watchdog::watchdog("16 owner registrations, concurrent last slot, exact scoped lists and freed slots", async {
        let root=tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        std::fs::write(&config_path, scheduler_thread_config()).unwrap();
        let owner=DataOwner::start(Some(config_path),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());
        let first=create_test_thread(&control,root.path(),"cap-thread-a").await;
        let second=create_test_thread(&control,root.path(),"cap-thread-b").await;
        let at=chrono::Utc::now().timestamp()+86_400;
        let mut first_ids=Vec::new();
        let mut second_ids=Vec::new();
        for index in 0..15 {
            let thread=if index%2==0 {first.clone()} else {second.clone()};
            let response=control.request(current_request(&control, format!("fill-{index}"),Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(thread.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))).await;
            assert!(response.ok,"{response:?}");
            let id=response.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
            if thread==first { first_ids.push(id); } else { second_ids.push(id); }
        }
        let (a,b)=tokio::join!(
            control.request(current_request(&control, "last-slot-a",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(first.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))),
            control.request(current_request(&control, "last-slot-b",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(second.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))),
        );
        assert_ne!(a.ok,b.ok,"exactly one concurrent registration may occupy the owner-wide last slot: {a:?} {b:?}");
        let (accepted_thread,accepted_id,rejected_error)=if a.ok {
            (first.clone(),a.result.unwrap()["schedule_id"].as_str().unwrap().to_owned(),b.error.unwrap())
        } else {
            (second.clone(),b.result.unwrap()["schedule_id"].as_str().unwrap().to_owned(),a.error.unwrap())
        };
        if accepted_thread==first { first_ids.push(accepted_id); } else { second_ids.push(accepted_id); }
        assert_eq!(rejected_error.message,"Owner schedule limit of 16 reached");
        let list_a=control.request(Request::new("list-a-at-cap",Command::ScheduleList(ScheduleListArgs{thread_id:Some(first.clone())}))).await;
        let list_b=control.request(Request::new("list-b-at-cap",Command::ScheduleList(ScheduleListArgs{thread_id:Some(second.clone())}))).await;
        assert!(list_a.ok && list_b.ok);
        let ids_a:std::collections::HashSet<String>=list_a.result.unwrap()["schedules"].as_array().unwrap().iter().map(|row|row["schedule_id"].as_str().unwrap().to_owned()).collect();
        let ids_b:std::collections::HashSet<String>=list_b.result.unwrap()["schedules"].as_array().unwrap().iter().map(|row|row["schedule_id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(ids_a,first_ids.iter().cloned().collect());
        assert_eq!(ids_b,second_ids.iter().cloned().collect());
        assert_eq!(ids_a.len()+ids_b.len(),16);
        let owner_count=owner.db.call(|db|Ok(db.query_row("SELECT count(*) FROM scheduled_command",[],|row|row.get::<_,i64>(0))?)).await.unwrap();
        assert_eq!(owner_count,16);
        let removed_id=ids_a.iter().next().unwrap().clone();
        let removed=control.request(current_request(&control, "remove-frees-owner-slot",Command::ScheduleRemove(crate::protocol::ScheduleRemoveArgs{schedule_id:removed_id}))).await;
        assert!(removed.ok,"{removed:?}");
        let refilled=control.request(current_request(&control, "refill-after-remove",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(second.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))).await;
        assert!(refilled.ok,"{refilled:?}");
        let preview=control.request(Request::new("preview-cascade-cap",Command::ThreadDeletePreview(ThreadDeleteArgs{thread_id:first.clone(),descendants:false,preview_marker:None}))).await;
        assert!(preview.ok,"{preview:?}");
        let preview_result = preview.result.unwrap();
        assert_eq!(preview_result["scheduled_command_count"], first_ids.len() - 1);
        let marker=serde_json::from_value(preview_result["preview_marker"].clone()).unwrap();
        let deleted=control.request(current_request(&control, "delete-frees-owner-slots",Command::ThreadDelete(ThreadDeleteArgs{thread_id:first,descendants:false,preview_marker:Some(marker)}))).await;
        assert!(deleted.ok,"{deleted:?}");
        let count=owner.db.call(|db|Ok(db.query_row("SELECT count(*) FROM scheduled_command",[],|row|row.get::<_,i64>(0))?)).await.unwrap();
        assert!(count<16,"Thread delete cascade must release owner-wide registrations: {count}");
        let after_cascade=control.request(current_request(&control, "cascade-refills-owner-cap",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(second),command:"true".into(),cwd:None,cron:None,at:Some(at)}))).await;
        assert!(after_cascade.ok,"Thread cascade must free owner-wide registration capacity: {after_cascade:?}");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_delete_preview_stays_partial_and_unmarked_delete_keeps_full_scope() {
    crate::test_watchdog::watchdog("delete preview retains partial scope while unmarked delete uses canonical targets", async {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        std::fs::write(&config_path, scheduler_thread_config()).unwrap();
        let owner = DataOwner::start(
            Some(config_path),
            Some(root.path().join("data")),
            root.path().into(),
        ).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let thread_id = create_test_thread(&control, root.path(), "large-delete-root").await;
        let schedule = control.request(current_request(
            &control,
            "schedule-on-large-delete-root",
            Command::ScheduleAdd(ScheduleAddArgs {
                thread_id: Some(thread_id.clone()),
                command: "true".into(),
                cwd: None,
                cron: None,
                at: Some(chrono::Utc::now().timestamp() + 86_400),
            }),
        )).await;
        assert!(schedule.ok, "{schedule:?}");
        owner.db.call({
            let thread_id = thread_id.clone();
            move |db| {
                for index in 0..257 {
                    db.execute(
                        "INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?2,'/','agents','profile','model','now')",
                        rusqlite::params![format!("delete-child-{index:03}"), thread_id],
                    )?;
                }
                Ok(())
            }
        }).await.unwrap();
        let preview = control.request(Request::new(
            "preview-large-delete-scope",
            Command::ThreadDeletePreview(ThreadDeleteArgs {
                thread_id: thread_id.clone(),
                descendants: true,
                preview_marker: None,
            }),
        )).await;
        assert!(preview.ok, "bounded preview must remain a response: {preview:?}");
        let preview = preview.result.unwrap();
        assert_eq!(preview["preview_marker"], serde_json::Value::Null);
        assert_eq!(preview["preview_marker_unavailable_reason"], "input_limit");
        assert_eq!(preview["scheduled_command_count"], serde_json::Value::Null);
        let deleted = control.request(current_request(
            &control,
            "unmarked-delete-keeps-canonical-full-target-scope",
            Command::ThreadDelete(ThreadDeleteArgs {
                thread_id: thread_id.clone(),
                descendants: true,
                preview_marker: None,
            }),
        )).await;
        assert!(deleted.ok, "unmarked deletion must retain canonical unbounded scope: {deleted:?}");
        let remaining = owner.db.call(move |db| Ok(db.query_row(
            "SELECT count(*) FROM thread WHERE deleted_at IS NULL AND (id=?1 OR parent_thread_id=?1)",
            [&thread_id],
            |row| row.get::<_, i64>(0),
        )?)).await.unwrap();
        assert_eq!(remaining, 0);
        let schedules_remaining = owner.db.call(|db| Ok(db.query_row(
            "SELECT count(*) FROM scheduled_command",
            [],
            |row| row.get::<_, i64>(0),
        )?)).await.unwrap();
        assert_eq!(schedules_remaining, 0);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_start_resumes_an_injected_persisted_due_cron_once() {
    crate::test_watchdog::watchdog(
        "normal Control::start resumes a synthetic persisted due cursor",
        async {
            let root = tempfile::tempdir().unwrap();
            let config_path = root.path().join("config.toml");
            let data_path = root.path().join("data");
            std::fs::write(&config_path, scheduler_thread_config()).unwrap();
            let first_owner = DataOwner::start(
                Some(config_path.clone()),
                Some(data_path.clone()),
                root.path().into(),
            )
            .await
            .unwrap();
            let first_control = Control::new(&first_owner, root.path().into());
            let thread_id = create_test_thread(&first_control, root.path(), "restart-thread").await;
            let output = root.path().join("restart-count");
            let added = first_control
                .request(current_request(
                    &first_control,
                    "register-before-restart",
                    Command::ScheduleAdd(ScheduleAddArgs {
                        thread_id: Some(thread_id.clone()),
                        command: format!("printf x >> '{}'", output.display()),
                        cwd: None,
                        cron: Some("0 0 1 1 *".into()),
                        at: None,
                    }),
                ))
                .await;
            assert!(added.ok, "{added:?}");
            let schedule_id = added.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();
            first_control.shutdown().await.unwrap();
            first_owner.shutdown().await.unwrap();

            // Seed only the durable cursor while no owner is open, so ordinary restart startup sees it due.
            let year = chrono::Local::now().year();
            let jan1 = |year| {
                chrono::Local
                    .with_ymd_and_hms(year, 1, 1, 0, 0, 0)
                    .single()
                    .unwrap()
                    .timestamp()
            };
            let expected_latest = jan1(year);
            let expected_next = jan1(year + 1);
            let connection = rusqlite::Connection::open(data_path.join("shellboy.db")).unwrap();
            connection
                .execute(
                    "UPDATE scheduled_command SET next_at=?1 WHERE id=?2",
                    rusqlite::params![expected_latest - 86_400, schedule_id],
                )
                .unwrap();
            drop(connection);

            let second_owner =
                DataOwner::start(Some(config_path), Some(data_path), root.path().into())
                    .await
                    .unwrap();
            let second_control = Control::new(&second_owner, root.path().into());
            second_control.start().await.unwrap();
            let mut poll = 0_u32;
            loop {
                let list = second_control
                    .request(Request::new(
                        format!("restart-list-{poll}"),
                        Command::ScheduleList(ScheduleListArgs {
                            thread_id: Some(thread_id.clone()),
                        }),
                    ))
                    .await;
                poll += 1;
                assert!(list.ok, "{list:?}");
                let rows = list.result.unwrap()["schedules"]
                    .as_array()
                    .unwrap()
                    .clone();
                if let Some(row) = rows.iter().find(|row| {
                    row["schedule_id"] == schedule_id && row["latest"]["status"] == "finished"
                }) {
                    assert_eq!(
                        row["latest"]["scheduled_at"], expected_latest,
                        "independent calendar oracle for latest missed annual occurrence"
                    );
                    assert_eq!(
                        row["next_at"], expected_next,
                        "next annual occurrence is future relative to restart dispatch"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            second_control.shutdown().await.unwrap();
            assert_eq!(
                tokio::fs::read_to_string(&output).await.unwrap(),
                "x",
                "append witness records one execution, unlike idempotent touch"
            );
            second_owner.shutdown().await.unwrap();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_owner_shutdown_settles_active_scheduled_shell() {
    crate::test_watchdog::watchdog(
        "normal owner shutdown settles an active scheduled child",
        async {
            let root = tempfile::tempdir().unwrap();
            let config_path = root.path().join("config.toml");
            std::fs::write(&config_path, scheduler_thread_config()).unwrap();
            let owner = DataOwner::start(
                Some(config_path),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            let thread_id = create_test_thread(&control, root.path(), "shutdown-thread").await;
            let started = root.path().join("shutdown-child-started");
            // Result settlement is observable; this fixture does not claim an independent OS group-cleanup fact.
            let command = format!("touch '{}' ; sleep 1", started.display());
            let added = control
                .request(current_request(
                    &control,
                    "shutdown-scheduled-add",
                    Command::ScheduleAdd(ScheduleAddArgs {
                        thread_id: Some(thread_id),
                        command,
                        cwd: None,
                        cron: None,
                        at: Some(chrono::Utc::now().timestamp() - 1),
                    }),
                ))
                .await;
            assert!(added.ok, "{added:?}");
            let id = added.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();
            loop {
                if tokio::fs::try_exists(&started).await.unwrap() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            control.shutdown().await.unwrap();
            let key = id.clone();
            let status = owner
                .db
                .call(move |db| {
                    db.query_row(
                        "SELECT status FROM scheduled_command_result WHERE schedule_id=?1",
                        [key],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(crate::storage::StorageError::from)
                })
                .await
                .unwrap();
            assert_eq!(status, "finished");
            owner.shutdown().await.unwrap();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_and_rewind_do_not_copy_or_change_thread_owned_schedule() {
    crate::test_watchdog::watchdog("fork child remains empty and applied rewind preserves parent schedule", async {
        let root=tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        std::fs::write(&config_path, scheduler_thread_config()).unwrap();
        let owner=DataOwner::start(Some(config_path),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());
        let parent=create_test_thread(&control,root.path(),"scheduler-branch-parent").await;
        owner.db.call({let parent=parent.clone();move|db|{
            db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('sched-branch-run',?1,'completed','2026-01-01T00:00:00')",[&parent])?;
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('sched-branch-turn',?1,'sched-branch-run','completed','2026-01-01T00:00:00')",[&parent])?;
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) VALUES('sched-branch-n1','sched-branch-turn','user','2026-01-01T00:00:01','{\"role\":\"user\",\"content\":[]}',20)",[])?;
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) VALUES('sched-branch-n2','sched-branch-turn','assistant','2026-01-01T00:00:02','{\"role\":\"assistant\",\"content\":[]}',20)",[])?;
            db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,0,'sched-branch-n1'),(?1,1,'sched-branch-n2')",[&parent])?;
            db.execute("UPDATE thread SET cursor_node_id='sched-branch-n2' WHERE id=?1",[&parent])?;
            Ok(())
        }}).await.unwrap();
        let command="printf scheduler-branch-proof".to_owned();
        let cwd=root.path().canonicalize().unwrap().to_string_lossy().into_owned();
        let added=control.request(current_request(&control, "register-parent-schedule",Command::ScheduleAdd(ScheduleAddArgs{
            thread_id:Some(parent.clone()),command:command.clone(),cwd:Some(cwd.clone()),cron:None,at:Some(chrono::Utc::now().timestamp()+86_400),
        }))).await;
        assert!(added.ok,"{added:?}");
        let schedule_id=added.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
        let initial=control.request(Request::new("list-parent-before-branch",Command::ScheduleList(ScheduleListArgs{thread_id:Some(parent.clone())}))).await;
        assert!(initial.ok,"{initial:?}");
        let initial_rows=initial.result.unwrap()["schedules"].as_array().unwrap().clone();
        assert_eq!(initial_rows.len(),1);
        let initial_next=initial_rows[0]["next_at"].clone();

        let forked=control.request(current_request(&control, "fork-parent-with-schedule",Command::ThreadFork(ThreadForkArgs{
            agents:None,thread_id:parent.clone(),node_id:"sched-branch-n1".into(),cwd:None,
        }))).await;
        assert!(forked.ok,"{forked:?}");
        let fork_id=forked.result.unwrap()["thread_id"].as_str().unwrap().to_owned();
        let empty_before=control.request(Request::new("list-fork-before-unrelated-change",Command::ScheduleList(ScheduleListArgs{thread_id:Some(fork_id.clone())}))).await;
        assert!(empty_before.ok,"{empty_before:?}");
        assert!(empty_before.result.unwrap()["schedules"].as_array().unwrap().is_empty());

        let unrelated=create_test_thread(&control,root.path(),"unrelated-thread").await;
        let unrelated_add=control.request(current_request(&control,"register-unrelated-schedule",Command::ScheduleAdd(ScheduleAddArgs{
            thread_id:Some(unrelated.clone()),command:"true".into(),cwd:None,cron:None,at:Some(chrono::Utc::now().timestamp()+86_400),
        }))).await;
        assert!(unrelated_add.ok,"{unrelated_add:?}");
        let empty_after=control.request(Request::new("list-fork-after-unrelated-change",Command::ScheduleList(ScheduleListArgs{thread_id:Some(fork_id.clone())}))).await;
        assert!(empty_after.ok,"{empty_after:?}");
        assert!(empty_after.result.unwrap()["schedules"].as_array().unwrap().is_empty());

        let rewound=control.request(current_request(&control,"rewind-parent-with-schedule",Command::ThreadRewind(ThreadRewindArgs{
            thread_id:Some(parent.clone()),node_id:"sched-branch-n1".into(),exclude_node:false,dry_run:false,recovery_run_id:None,
        }))).await;
        assert!(rewound.ok,"{rewound:?}");
        let rewind_branch=rewound.result.unwrap()["branch_thread_id"].as_str().unwrap().to_owned();
        let parent_after=control.request(Request::new("list-parent-after-rewind",Command::ScheduleList(ScheduleListArgs{thread_id:Some(parent.clone())}))).await;
        assert!(parent_after.ok,"{parent_after:?}");
        let rows=parent_after.result.unwrap()["schedules"].as_array().unwrap().clone();
        assert_eq!(rows.len(),1);
        assert_eq!(rows[0]["schedule_id"],schedule_id);
        assert_eq!(rows[0]["next_at"],initial_next);
        assert!(rows[0]["latest"].is_null());
        let check_id=schedule_id.clone();
        let stored=owner.db.call(move|db|db.query_row("SELECT thread_id,command,cwd,next_at FROM scheduled_command WHERE id=?1",[check_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,i64>(3)?))).map_err(crate::storage::StorageError::from)).await.unwrap();
        assert_eq!(stored.0,parent);
        assert_eq!(stored.1,command);
        assert_eq!(stored.2,cwd);
        assert_eq!(rows[0]["next_at"],stored.3);
        let rewind_list=control.request(Request::new("list-rewind-branch",Command::ScheduleList(ScheduleListArgs{thread_id:Some(rewind_branch)}))).await;
        assert!(rewind_list.ok,"{rewind_list:?}");
        assert!(rewind_list.result.unwrap()["schedules"].as_array().unwrap().is_empty());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}
