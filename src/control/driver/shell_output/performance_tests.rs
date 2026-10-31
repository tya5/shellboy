//! Opt-in real shell collector + wait observer contention, Unix synthetic data.
use super::*;
use crate::{
    app::DataOwner,
    control::{driver::Snapshot, wait},
    protocol::{WaitArgs, WaitCondition},
    shell::ShellSupervisor,
};
use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

fn summary(samples: &[u128]) -> Value {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let percentile = |n: usize| {
        samples[(samples.len() * n)
            .div_ceil(100)
            .saturating_sub(1)
            .min(samples.len() - 1)]
    };
    json!({"samples":samples.len(),"p50_us":percentile(50),"p95_us":percentile(95),"max_us":samples.last()})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Ten native shells and ten production wait observers; run serially"]
async fn performance_ten_shells_ten_lifecycle_waits() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db| {
        let tx=db.transaction()?;
        for i in 0..10 {
            let thread=format!("t{i}"); let run=format!("r{i}"); let turn=format!("u{i}"); let shell=format!("s{i}");
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/a','fixture','synthetic','now')",[&thread])?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,'live','now')",params![run,thread])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'running','now')",params![turn,thread,run])?;
            tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,?3,?1,'running','/','{}','UTF-8','UTF-8','open','now')",params![shell,run,turn])?;
        }
        tx.commit()?; Ok(())
    }).await.unwrap();
    let mut collectors = Vec::new();
    let mut handles = Vec::new();
    for i in 0..10 {
        let execution = format!("s{i}");
        let args=serde_json::from_value(json!({"command":"read -r start; i=0; while [ \"$i\" -lt 200 ]; do printf 'synthetic-output\\n'; i=$((i+1)); sleep 0.01; done; read -r finish", "stdin_open":true})).unwrap();
        let shell = ShellSupervisor::new(&owner.loaded.config)
            .spawn(execution.clone(), root.path().into(), args, BTreeMap::new())
            .await
            .unwrap();
        handles.push(shell.handle.clone());
        let snapshot = Snapshot {
            thread: format!("t{i}"),
            run: format!("r{i}"),
            turn: format!("u{i}"),
            generation: 0,
            selection: 0,
            profile_revision: 0,
            profile: "fixture".into(),
            model: "synthetic".into(),
            effort: None,
            epoch: 0,
            cwd: root.path().into(),
            agents: root.path().join("AGENTS.md"),
            input: None,
            receipts: vec![],
            events: vec![],
        };
        let control = control.clone();
        collectors.push(tokio::spawn(async move {
            collect_shell(&control, &snapshot, &execution, shell)
                .await
                .unwrap();
        }));
    }
    let mut observers = Vec::new();
    let mut cancellation = Vec::new();
    for i in 0..10 {
        let control = control.clone();
        let token = CancellationToken::new();
        cancellation.push(token.clone());
        observers.push(tokio::spawn(async move {
            wait::observe(
                control,
                format!("benchmark-wait-{i}"),
                WaitArgs {
                    after: None,
                    conditions: vec![WaitCondition::ShellEnded {
                        execution_id: format!("s{i}"),
                    }],
                    timeout_ms: 30_000,
                },
                None,
                token,
            )
            .await
            .unwrap()
        }));
    }
    crate::test_watchdog::watchdog("src/control/driver/shell_output/performance_tests.rs: performance_ten_shells_ten_lifecycle_waits",async {
        loop {
            let count=owner.db.call(|db| Ok(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.benchmark-wait-%'",[],|r|r.get::<_,i64>(0))?)).await.unwrap();
            if count==10 { break; } tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let before = control.waits.observations.load(Ordering::Relaxed);
    let commits_before = control.waits.output_commits.load(Ordering::Relaxed);
    control.waits.output_save_micros.lock().unwrap().clear();
    for handle in &handles {
        handle.write("begin\n", "UTF-8").await.unwrap();
    }
    let started = Instant::now();
    let mut read_samples = Vec::new();
    while started.elapsed() < Duration::from_millis(1200) {
        let probe = Instant::now();
        owner
            .db
            .call(|db| {
                db.query_row(
                    "SELECT id FROM thread WHERE id='t0' AND deleted_at IS NULL",
                    [],
                    |r| r.get::<_, String>(0),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        read_samples.push(probe.elapsed().as_micros());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let observations = control.waits.observations.load(Ordering::Relaxed) - before;
    let commits = control.waits.output_commits.load(Ordering::Relaxed) - commits_before;
    let output_samples = control.waits.output_save_micros.lock().unwrap().clone();
    assert!(
        commits >= 10 && !output_samples.is_empty(),
        "Native shells must actually produce persisted batches"
    );
    println!(
        "PERF {}",
        json!({"name":"ten_shells_ten_waits","shells":10,"lifecycle_waiters":10,"window_ms":started.elapsed().as_secs_f64()*1000.0,"wait_observation_db_jobs":observations,"shell_output_commits":commits,"output_commit":summary(&output_samples),"thread_read":summary(&read_samples),"scope":"Real ShellSupervisor/collect_shell commits and wait::observe; direct Thread identity SQL probe, no full ThreadGet projection or API"})
    );
    for token in cancellation {
        token.cancel();
    }
    for observer in observers {
        observer.await.unwrap();
    }
    for handle in handles {
        handle.stop();
    }
    for collector in collectors {
        collector.await.unwrap();
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
