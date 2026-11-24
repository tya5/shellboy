use super::{UiAction, UiState};
use crate::{config::Config, protocol::*};

fn assert_dispatch(text: &str, expected: Command) {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    let actions = ui.slash(text);
    let [UiAction::Manage(command)] = actions.as_slice() else {
        panic!("{text} did not dispatch: {}", ui.view().status);
    };
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        serde_json::to_value(expected).unwrap(),
        "{text}"
    );
}

macro_rules! routes {
    ($($name:ident: $text:literal => $expected:expr;)*) => {$(
        #[test]
        fn $name() { assert_dispatch($text, $expected); }
    )*};
}

routes! {
    schedule_list: "/schedule list --thread thread-a" => Command::ScheduleList(ThreadTarget { thread_id: Some("thread-a".into()) });
    schedule_add: "/schedule add --thread thread-a --in 10m --cwd '/a path' --description 'later today' -- printf '%s' 'hello world'" => Command::ScheduleAdd(ScheduleAddArgs {
        thread_id: Some("thread-a".into()), cron: None, at: None, in_seconds: Some(600), cwd: Some("/a path".into()), description: Some("later today".into()), command: "printf %s hello world".into(),
    });
    schedule_set: "/schedule set --schedule sched-a --description 'updated text'" => Command::ScheduleSet(ScheduleSetArgs { schedule_id: "sched-a".into(), description: "updated text".into() });
    schedule_enable: "/schedule enable --schedule sched-a" => Command::ScheduleEnable(ScheduleTarget { schedule_id: "sched-a".into() });
    schedule_disable: "/schedule disable --schedule sched-a" => Command::ScheduleDisable(ScheduleTarget { schedule_id: "sched-a".into() });
    schedule_remove: "/schedule remove --schedule sched-a" => Command::ScheduleRemove(ScheduleTarget { schedule_id: "sched-a".into() });
    service_list: "/service list" => Command::ServiceList(ThreadTarget { thread_id: None });
    service_add: "/service add --thread thread-a --cwd '/a path' --description 'example service' --command 'echo hello'" => Command::ServiceAdd(ServiceAddArgs {
        thread_id: Some("thread-a".into()), cwd: Some("/a path".into()), description: Some("example service".into()), command: "echo hello".into(),
    });
    service_start: "/service start --service svc-a" => Command::ServiceStart(ServiceTarget { service_id: "svc-a".into() });
    service_stop: "/service stop --service svc-a" => Command::ServiceStop(ServiceTarget { service_id: "svc-a".into() });
    service_remove: "/service remove --service svc-a" => Command::ServiceRemove(ServiceTarget { service_id: "svc-a".into() });
}

#[test]
fn rejects_non_cli_verbs_flags_and_unclosed_quotes() {
    for text in [
        "/service enable --service svc-a",
        "/schedule list --data-dir other-owner",
        "/schedule set --schedule sched-a --description 'unfinished",
    ] {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        assert!(ui.slash(text).is_empty(), "{text}");
        assert!(ui.command_rejected, "{text}");
    }
}
