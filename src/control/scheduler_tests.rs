use super::*;
use crate::{app::DataOwner, protocol::{Command, Request, ScheduleAddArgs, ScheduleListArgs}};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_at_runs_shared_shell_and_list_reports_latest_result() {
    crate::test_watchdog::watchdog("schedule add -> timer -> shell -> latest result -> list", async {
        let root = tempfile::tempdir().unwrap();
        let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
            .await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let add = control.request(Request::new("schedule-add", Command::ScheduleAdd(ScheduleAddArgs {
            command: "printf 'scheduled-ok\\n'".into(),
            cwd: root.path().to_string_lossy().into_owned(),
            cron: None,
            at: Some(chrono::Utc::now().timestamp() - 1),
        }))).await;
        assert!(add.ok, "{add:?}");
        loop {
            let list = control.request(Request::new("schedule-list", Command::ScheduleList(ScheduleListArgs {}))).await;
            assert!(list.ok, "{list:?}");
            let schedules = list.result.unwrap()["schedules"].as_array().unwrap().clone();
            if schedules.iter().any(|row| row["latest"]["status"] == "finished"
                && row["latest"]["output_tail"].as_str().unwrap_or("").contains("scheduled-ok")) { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}
