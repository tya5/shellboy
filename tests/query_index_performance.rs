//! Native opt-in Control measurements. This compares index presence on the same
//! synthetic fixture and current code, not the old binary or #28 notification
//! strategy. No external service, user database, or timing acceptance threshold.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const CREATE_INDEXES: &str = "CREATE INDEX shell_active_owner ON shell_execution(owner_run_id,state) WHERE state IN ('starting','running','stopping'); CREATE INDEX run_thread_created ON run(thread_id,created_at DESC,id DESC); CREATE INDEX shell_terminal_time ON shell_execution(COALESCE(ended_at,started_at)) WHERE state IN ('exited','failed','stopped');";
const DROP_INDEXES: &str =
    "DROP INDEX shell_active_owner; DROP INDEX run_thread_created; DROP INDEX shell_terminal_time;";

async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}

fn report(name: &str, phase: &str, samples: &[Duration]) {
    let mut micros = samples
        .iter()
        .map(|sample| sample.as_secs_f64() * 1_000_000.0)
        .collect::<Vec<_>>();
    micros.sort_by(f64::total_cmp);
    println!(
        "PERF {}",
        json!({
            "name": name,"phase":phase,"samples":micros.len(),"p50_us":micros[4],"p95_us":micros[8],
            "min_us":micros[0],"max_us":micros[8],"sqlite_version":rusqlite::version(),
            "fixture":{"threads":1000,"runs":10000,"shell_executions":50000,"live_runs":100,"active_shells":100},
            "scope":"native current-code Control::request, dispatch, admission, durable request bookkeeping, DbWorker queue, inspection and response projection; wait uses already-terminal shell, includes ownership query and cleanup but no suspension; thread list includes title/excerpt projection; no IPC serialization, UI, HTTP or external API",
            "comparison":"same synthetic DB and code; only three index objects dropped/recreated after Control startup; schema ledger/user_version remain unchanged; not old/new binary or notification-policy comparison"
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "synthetic 50k shell/10k Run native Control timing; run explicitly in isolation"]
async fn performance_native_wait_and_thread_page_indexes() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    control.store.database.call(|db| {
        let tx = db.transaction()?;
        {
            let mut insert = tx.prepare("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,'/','/AGENTS.md','fixture','fixture',?3)")?;
            for i in 0..1000 {
                insert.execute(rusqlite::params![format!("t{i:04}"), format!("synthetic thread {i}"), format!("{i:08}")])?;
            }
        }
        {
            let mut run = tx.prepare("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,?3,?4)")?;
            let mut turn = tx.prepare("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,?4,?5)")?;
            for i in 0..10000 {
                let live = i >= 9900;
                let thread = format!("t{:04}", i % 1000);
                let run_id = format!("r{i:05}");
                let created = format!("{i:08}");
                run.execute(rusqlite::params![run_id,thread,if live {"live"} else {"completed"},created])?;
                turn.execute(rusqlite::params![format!("u{i:05}"),thread,run_id,if live {"running"} else {"completed"},created])?;
            }
        }
        {
            let mut shell = tx.prepare("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,output_complete)VALUES(?1,?2,?3,?1,?4,'/','{}','UTF-8','UTF-8','closed',?5,?6,?7)")?;
            for i in 0..50000 {
                let active = i >= 49900;
                let time = format!("{i:08}");
                shell.execute(rusqlite::params![format!("s{i:05}"),format!("r{:05}",i%10000),format!("u{:05}",i%10000),if active {"running"} else {"exited"},time,if active {None} else {Some(time.clone())},!active])?;
            }
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let mut baseline_wait = None;
    let mut baseline_page = None;
    for phase in ["indexes_absent", "indexes_present"] {
        control
            .store
            .database
            .call(move |db| {
                db.execute_batch(if phase == "indexes_absent" {
                    DROP_INDEXES
                } else {
                    CREATE_INDEXES
                })?;
                assert_eq!(
                    db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
                    shellboy::storage::SCHEMA_VERSION
                );
                Ok(())
            })
            .await
            .unwrap();
        let mut waits = Vec::new();
        let mut pages = Vec::new();
        for _ in 0..9 {
            let started = Instant::now();
            let wait = request(
                &control,
                Command::Wait(WaitArgs {
                    after: None,
                    conditions: vec![WaitCondition::ShellEnded {
                        execution_id: "s00000".into(),
                    }],
                    timeout_ms: 1000,
                }),
            )
            .await;
            waits.push(started.elapsed());
            assert_eq!(
                wait["matched"],
                json!([{"type":"shell_ended","execution_id":"s00000"}])
            );
            if let Some(expected) = &baseline_wait {
                assert_eq!(&wait, expected);
            } else {
                baseline_wait = Some(wait);
            }
            let started = Instant::now();
            let page = request(
                &control,
                Command::ThreadList(ThreadListArgs {
                    limit: 50,
                    cursor: None,
                    query: None,
                }),
            )
            .await;
            pages.push(started.elapsed());
            let items = page["items"].as_array().unwrap();
            assert_eq!(items.len(), 50);
            assert_eq!(items[0]["thread_id"], "t0999");
            assert_eq!(items[49]["thread_id"], "t0950");
            assert!(page["next_cursor"].is_string());
            if let Some(expected) = &baseline_page {
                assert_eq!(items, expected);
            } else {
                baseline_page = Some(items.clone());
            }
        }
        report("native_control_wait_terminal_shell", phase, &waits);
        report("native_control_thread_list_page", phase, &pages);
    }
    // Synthetic active rows have no actual processes or drivers. Settle them
    // before shutdown so shutdown measures/drains only real admitted work.
    control.store.database.call(|db| {
        db.execute_batch("UPDATE shell_execution SET state='exited',ended_at=started_at,output_complete=1 WHERE state='running'; UPDATE turn SET state='completed' WHERE state='running'; UPDATE run SET state='completed' WHERE state='live';")?;
        assert_eq!(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'", [], |r| r.get::<_, i64>(0))?, 0);
        Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
