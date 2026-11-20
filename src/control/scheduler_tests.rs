use super::*;
use crate::{app::DataOwner, protocol::{Command, Request, ScheduleAddArgs, ScheduleListArgs, ThreadCreateArgs, ThreadDeleteArgs, ThreadArchiveArgs}};
use std::time::Duration;
use chrono::{Datelike, TimeZone};

async fn create_test_thread(control:&Arc<Control>,cwd:&std::path::Path,operation:&'static str)->String {
    let response=control.request(Request::new(operation,Command::ThreadCreate(ThreadCreateArgs{
        agents:None,connection:None,cwd:Some(cwd.to_string_lossy().into_owned()),model:None,effort:None,parent_id:None,
    }))).await;
    assert!(response.ok,"{response:?}");
    response.result.unwrap()["thread_id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_at_runs_shared_shell_and_list_reports_latest_result() {
    crate::test_watchdog::watchdog("schedule add -> timer -> shell -> latest result -> list", async {
        let root = tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        std::fs::write(&config_path,"[schedule]\nmax_sleep_seconds=300\n").unwrap();
        let owner = DataOwner::start(Some(config_path), Some(root.path().join("data")), root.path().into())
            .await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let created=control.request(Request::new("thread-create",Command::ThreadCreate(ThreadCreateArgs{agents:None,connection:None,cwd:Some(root.path().to_string_lossy().into_owned()),model:None,effort:None,parent_id:None}))).await;
        assert!(created.ok,"{created:?}");
        let thread_id=created.result.unwrap()["thread_id"].as_str().unwrap().to_owned();
        let started=root.path().join("started");
        let release=root.path().join("release");
        let finished=root.path().join("finished");
        let command=format!("touch '{}' ; while [ ! -e '{}' ]; do sleep 0.01; done; printf 'scheduled-ok\\n%s\\n%s\\n' \"$SHELLBOY_THREAD_ID\" \"$PWD\"; touch '{}'",started.display(),release.display(),finished.display());
        let due_at=chrono::Utc::now().timestamp()-1;
        let add=control.request(Request::new("schedule-add",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(thread_id.clone()),command,cwd:None,cron:None,at:Some(due_at)}))).await;
        assert!(add.ok,"{add:?}");
        let schedule_id=add.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
        loop {
            if tokio::fs::try_exists(&started).await.unwrap() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let active_preview=control.request(Request::new("preview-active",Command::ThreadDeletePreview(ThreadDeleteArgs{thread_id:thread_id.clone(),descendants:false,preview_marker:None}))).await;
        assert!(active_preview.ok,"{active_preview:?}");
        assert_eq!(active_preview.result.unwrap()["blockers"]["scheduled_shell_count"],1);
        let active_delete=control.request(Request::new("delete-active",Command::ThreadDelete(ThreadDeleteArgs{thread_id:thread_id.clone(),descendants:false,preview_marker:None}))).await;
        assert!(!active_delete.ok,"active scheduled shell must block Thread deletion");
        assert!(active_delete.error.as_ref().is_some_and(|error|error.message.contains("scheduled shell")),"unexpected deletion blocker: {active_delete:?}");
        let archived=control.request(Request::new("archive-active",Command::ThreadArchive(ThreadArchiveArgs{thread_ids:vec![thread_id.clone()],descendants:false}))).await;
        assert!(archived.ok,"{archived:?}");
        let hidden=control.request(Request::new("list-archived",Command::ScheduleList(ScheduleListArgs{thread_id:Some(thread_id.clone())}))).await;
        assert!(hidden.ok,"{hidden:?}");
        assert!(hidden.result.unwrap()["schedules"].as_array().unwrap().is_empty());
        std::fs::write(&release,"").unwrap();
        loop { if tokio::fs::try_exists(&finished).await.unwrap(){break;} tokio::time::sleep(Duration::from_millis(10)).await; }
        let unarchived=control.request(Request::new("unarchive-active",Command::ThreadUnarchive(ThreadArchiveArgs{thread_ids:vec![thread_id.clone()],descendants:false}))).await;
        assert!(unarchived.ok,"{unarchived:?}");
        loop {
            let list=control.request(Request::new("schedule-list",Command::ScheduleList(ScheduleListArgs{thread_id:Some(thread_id.clone())}))).await;
            assert!(list.ok,"{list:?}");
            let schedules=list.result.unwrap()["schedules"].as_array().unwrap().clone();
            if schedules.iter().any(|row|{
                let tail=row["latest"]["output_tail"].as_str().unwrap_or("");
                row["schedule_id"]==schedule_id && row["thread_id"]==thread_id && row["cwd"].is_null()
                    && row["next_at"].is_null() && row["latest"]["status"]=="finished"
                    && row["latest"]["scheduled_at"]==due_at && tail.contains("scheduled-ok") && tail.contains(&thread_id) && tail.contains(root.path().to_str().unwrap())
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
        let deleted=control.request(Request::new("thread-delete",Command::ThreadDelete(ThreadDeleteArgs{thread_id:thread_id.clone(),descendants:false,preview_marker:Some(serde_json::from_value(marker).unwrap())}))).await;
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
async fn busy_thread_admission_retires_one_due_row_and_dispatches_another() {
    crate::test_watchdog::watchdog("busy Thread admission retires without a child and dispatcher continues",async {
        let root=tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        std::fs::write(&config_path,"[schedule]\nmax_sleep_seconds=300\n").unwrap();
        let owner=DataOwner::start(Some(config_path),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());
        let busy_thread=create_test_thread(&control,root.path(),"create-busy-thread").await;
        let free_thread=create_test_thread(&control,root.path(),"create-free-thread").await;
        let no_child=root.path().join("must-not-run");
        let future_at=chrono::Utc::now().timestamp()+86_400;
        let blocked=control.request(Request::new("blocked-schedule-add",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(busy_thread.clone()),command:format!("touch '{}'",no_child.display()),cwd:None,cron:None,at:Some(future_at)}))).await;
        assert!(blocked.ok,"{blocked:?}");
        let blocked_id=blocked.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
        let did_run=root.path().join("other-ran");
        let other=control.request(Request::new("free-schedule-add",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(free_thread.clone()),command:format!("touch '{}'",did_run.display()),cwd:None,cron:None,at:Some(future_at)}))).await;
        assert!(other.ok,"{other:?}");
        let other_id=other.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
        let boundary=control.boundary(&busy_thread).await;
        let held=boundary.lock().await;
        let due_at=chrono::Utc::now().timestamp()-1;
        let busy_id=blocked_id.clone();let free_id=other_id.clone();
        // Synthetic cursor injection deterministically makes future registrations due;
        // this exercises dispatcher settlement, not the original one-shot timestamp.
        owner.db.call(move|db|{
            db.execute("UPDATE scheduled_command SET next_at=?1 WHERE id IN (?2,?3)",rusqlite::params![due_at,busy_id,free_id])?;
            Ok(())
        }).await.unwrap();
        control.changed.notify_waiters();
        loop {
            let busy_list=control.request(Request::new("busy-thread-list",Command::ScheduleList(ScheduleListArgs{thread_id:Some(busy_thread.clone())}))).await;
            let free_list=control.request(Request::new("free-thread-list",Command::ScheduleList(ScheduleListArgs{thread_id:Some(free_thread.clone())}))).await;
            assert!(busy_list.ok && free_list.ok,"{busy_list:?} {free_list:?}");
            let busy_rows=busy_list.result.unwrap()["schedules"].as_array().unwrap().clone();
            let free_rows=free_list.result.unwrap()["schedules"].as_array().unwrap().clone();
            let busy_settled=busy_rows.iter().any(|row|row["schedule_id"]==blocked_id
                && row["next_at"].is_null() && row["latest"]["status"]=="launch_failed"
                && row["latest"]["scheduler_diagnostic"]=="Scheduler: Thread admission busy; command not started"
                && row["latest"]["output_tail"].is_null() && row["latest"]["exit_code"].is_null()
                && row["latest"]["scheduled_at"]==due_at);
            let free_settled=free_rows.iter().any(|row|row["schedule_id"]==other_id
                && row["next_at"].is_null() && row["latest"]["status"]=="finished"
                && row["latest"]["scheduled_at"]==due_at);
            if busy_settled && free_settled { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!tokio::fs::try_exists(&no_child).await.unwrap(),"busy-boundary occurrence must not launch a child");
        drop(held);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_schedule_cap_is_atomic_across_threads_and_scoped_list_is_complete() {
    crate::test_watchdog::watchdog("16 owner registrations, concurrent last slot, exact scoped lists and freed slots", async {
        let root=tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        std::fs::write(&config_path,"[schedule]\nmax_sleep_seconds=300\n").unwrap();
        let owner=DataOwner::start(Some(config_path),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());
        let first=create_test_thread(&control,root.path(),"cap-thread-a").await;
        let second=create_test_thread(&control,root.path(),"cap-thread-b").await;
        let at=chrono::Utc::now().timestamp()+86_400;
        let mut first_ids=Vec::new();
        let mut second_ids=Vec::new();
        for index in 0..15 {
            let thread=if index%2==0 {first.clone()} else {second.clone()};
            let response=control.request(Request::new(format!("fill-{index}"),Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(thread.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))).await;
            assert!(response.ok,"{response:?}");
            let id=response.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
            if thread==first { first_ids.push(id); } else { second_ids.push(id); }
        }
        let (a,b)=tokio::join!(
            control.request(Request::new("last-slot-a",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(first.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))),
            control.request(Request::new("last-slot-b",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(second.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))),
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
        let removed=control.request(Request::new("remove-frees-owner-slot",Command::ScheduleRemove(crate::protocol::ScheduleRemoveArgs{schedule_id:removed_id}))).await;
        assert!(removed.ok,"{removed:?}");
        let refilled=control.request(Request::new("refill-after-remove",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(second.clone()),command:"true".into(),cwd:None,cron:None,at:Some(at)}))).await;
        assert!(refilled.ok,"{refilled:?}");
        let preview=control.request(Request::new("preview-cascade-cap",Command::ThreadDeletePreview(ThreadDeleteArgs{thread_id:first.clone(),descendants:false,preview_marker:None}))).await;
        assert!(preview.ok,"{preview:?}");
        let marker=serde_json::from_value(preview.result.unwrap()["preview_marker"].clone()).unwrap();
        let deleted=control.request(Request::new("delete-frees-owner-slots",Command::ThreadDelete(ThreadDeleteArgs{thread_id:first,descendants:false,preview_marker:Some(marker)}))).await;
        assert!(deleted.ok,"{deleted:?}");
        let count=owner.db.call(|db|Ok(db.query_row("SELECT count(*) FROM scheduled_command",[],|row|row.get::<_,i64>(0))?)).await.unwrap();
        assert!(count<16,"Thread delete cascade must release owner-wide registrations: {count}");
        let after_cascade=control.request(Request::new("cascade-refills-owner-cap",Command::ScheduleAdd(ScheduleAddArgs{thread_id:Some(second),command:"true".into(),cwd:None,cron:None,at:Some(at)}))).await;
        assert!(after_cascade.ok,"Thread cascade must free owner-wide registration capacity: {after_cascade:?}");
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_start_resumes_an_injected_persisted_due_cron_once() {
    crate::test_watchdog::watchdog("normal Control::start resumes a synthetic persisted due cursor", async {
        let root=tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        let data_path=root.path().join("data");
        std::fs::write(&config_path,"[schedule]\nmax_sleep_seconds=300\n").unwrap();
        let first_owner=DataOwner::start(Some(config_path.clone()),Some(data_path.clone()),root.path().into()).await.unwrap();
        let first_control=Control::new(&first_owner,root.path().into());
        let thread_id=create_test_thread(&first_control,root.path(),"restart-thread").await;
        let output=root.path().join("restart-count");
        let added=first_control.request(Request::new("register-before-restart",Command::ScheduleAdd(ScheduleAddArgs{
            thread_id:Some(thread_id.clone()),command:format!("printf x >> '{}'",output.display()),cwd:None,cron:Some("0 0 1 1 *".into()),at:None,
        }))).await;
        assert!(added.ok,"{added:?}");
        let schedule_id=added.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
        first_control.shutdown().await.unwrap();
        first_owner.shutdown().await.unwrap();

        // Seed only the durable cursor while no owner is open, so ordinary restart startup sees it due.
        let year=chrono::Local::now().year();
        let jan1=|year|chrono::Local.with_ymd_and_hms(year,1,1,0,0,0).single().unwrap().timestamp();
        let expected_latest=jan1(year);
        let expected_next=jan1(year+1);
        let connection=rusqlite::Connection::open(data_path.join("shellboy.db")).unwrap();
        connection.execute("UPDATE scheduled_command SET next_at=?1 WHERE id=?2",rusqlite::params![expected_latest-86_400,schedule_id]).unwrap();
        drop(connection);

        let second_owner=DataOwner::start(Some(config_path),Some(data_path),root.path().into()).await.unwrap();
        let second_control=Control::new(&second_owner,root.path().into());
        second_control.start().await.unwrap();
        let mut poll=0_u32;
        loop {
            let list=second_control.request(Request::new(format!("restart-list-{poll}"),Command::ScheduleList(ScheduleListArgs{thread_id:Some(thread_id.clone())}))).await;
            poll+=1;
            assert!(list.ok,"{list:?}");
            let rows=list.result.unwrap()["schedules"].as_array().unwrap().clone();
            if let Some(row)=rows.iter().find(|row|row["schedule_id"]==schedule_id && row["latest"]["status"]=="finished") {
                assert_eq!(row["latest"]["scheduled_at"],expected_latest,"independent calendar oracle for latest missed annual occurrence");
                assert_eq!(row["next_at"],expected_next,"next annual occurrence is future relative to restart dispatch");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        second_control.shutdown().await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&output).await.unwrap(),"x","append witness records one execution, unlike idempotent touch");
        second_owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_owner_shutdown_settles_active_scheduled_shell() {
    crate::test_watchdog::watchdog("normal owner shutdown settles an active scheduled child", async {
        let root=tempfile::tempdir().unwrap();
        let config_path=root.path().join("config.toml");
        std::fs::write(&config_path,"[schedule]\nmax_sleep_seconds=300\n").unwrap();
        let owner=DataOwner::start(Some(config_path),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());
        let thread_id=create_test_thread(&control,root.path(),"shutdown-thread").await;
        let started=root.path().join("shutdown-child-started");
        // Result settlement is observable; this fixture does not claim an independent OS group-cleanup fact.
        let command=format!("touch '{}' ; sleep 1",started.display());
        let added=control.request(Request::new("shutdown-scheduled-add",Command::ScheduleAdd(ScheduleAddArgs{
            thread_id:Some(thread_id),command,cwd:None,cron:None,at:Some(chrono::Utc::now().timestamp()-1),
        }))).await;
        assert!(added.ok,"{added:?}");
        let id=added.result.unwrap()["schedule_id"].as_str().unwrap().to_owned();
        loop {
            if tokio::fs::try_exists(&started).await.unwrap(){break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        control.shutdown().await.unwrap();
        let key=id.clone();
        let status=owner.db.call(move|db|db.query_row("SELECT status FROM scheduled_command_result WHERE schedule_id=?1",[key],|row|row.get::<_,String>(0)).map_err(crate::storage::StorageError::from)).await.unwrap();
        assert_eq!(status,"finished");
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}
