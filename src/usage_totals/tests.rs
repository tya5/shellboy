use super::*;
use std::time::Instant;

const START: &str = "2026-10-30T00:00:00.000000000Z";
const END: &str = "2026-10-30T00:00:00.010000000Z";
fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    db
}
fn seed(db: &mut Connection, runs: usize, spread: bool) {
    let tx = db.transaction().unwrap();
    for i in 0..=100 {
        tx.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,'.','AGENTS.md','fixture','fixture',?3)",params![format!("t{i}"),if i==0{None}else{Some("t0")},START]).unwrap();
    }
    {
        let mut r=tx.prepare("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed',?3,?4)").unwrap();
        let mut t=tx.prepare("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES(?1,?2,?3,'completed',?4,?5)").unwrap();
        let mut a=tx.prepare("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES(?1,?2,0,0,'fixture','fixture',0,?3,'{}',?4,?5,?6)").unwrap();
        let mut shell=tx.prepare("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at)VALUES(?1,?2,?3,?1,'exited','.','{\"completion\":{\"elapsed_ms\":5}}','UTF-8','UTF-8','closed',?4,?5)").unwrap();
        for i in 0..runs {
            let owner = format!("t{}", if spread { i % 101 } else { 0 });
            let run = format!("r{i}");
            let turn = format!("u{i}");
            r.execute(params![run, owner, START, END]).unwrap();
            t.execute(params![turn, owner, run, START, END]).unwrap();
            for j in 0..100 {
                let state = match j % 5 {
                    0 => "failed",
                    1 => "incomplete",
                    _ => "completed",
                };
                a.execute(params![format!("a{i}-{j}"),turn,state,r#"{"input_tokens":100,"input_tokens_details":{"cached_tokens":75},"output_tokens":10,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":110}"#,START,END]).unwrap();
            }
            for j in 0..10 {
                shell
                    .execute(params![format!("s{i}-{j}"), run, turn, START, END])
                    .unwrap();
            }
        }
    }
    tx.commit().unwrap();
}
#[test]
fn saved_state_totals_are_exact_read_only_and_missing_is_not_zero() {
    let mut db = db();
    seed(&mut db, 1, false);
    db.execute(
        "UPDATE model_attempt SET usage_json=NULL WHERE id='a0-0'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE model_attempt SET usage_json='not JSON' WHERE id='a0-1'",
        [],
    )
    .unwrap();
    db.execute("UPDATE model_attempt SET usage_json='{\"input_tokens\":-1,\"input_tokens_details\":{\"cached_tokens\":0}}' WHERE id='a0-2'",[]).unwrap();
    let before = db.total_changes();
    let v = read(&db, &Scope::Turn("u0".into())).unwrap();
    assert_eq!(v["model_attempts"]["failed"]["count"], 20);
    assert_eq!(
        v["model_attempts"]["failed"]["input_tokens"]["known_sum"],
        1900
    );
    assert_eq!(
        v["model_attempts"]["failed"]["input_tokens"]["unknown_count"],
        1
    );
    assert_eq!(
        v["model_attempts"]["completed"]["cached_tokens"]["known_sum"],
        59 * 75
    );
    assert_eq!(
        v["model_attempts"]["completed"]["input_tokens"]["unknown_count"],
        1
    );
    assert_eq!(v["model_duration_ms"]["known_sum"], 1000);
    assert_eq!(v["shell"]["duration_ms"]["known_sum"], 50);
    assert_eq!(v["run"]["status"], "not_applicable");
    assert_eq!(v["coverage"]["summary"], "not_recorded");
    assert_eq!(db.total_changes(), before);
    let all = read(
        &db,
        &Scope::Thread {
            id: "t0".into(),
            children: true,
        },
    )
    .unwrap();
    assert_eq!(all["selected_threads"], 101);
    assert_eq!(all["coverage"]["complete"], true);
}
#[test]
fn overflow_falls_back_to_exact_integer_chunks() {
    let mut db = db();
    seed(&mut db, 1, false);
    db.execute(
        "DELETE FROM model_attempt WHERE id NOT IN ('a0-2','a0-3','a0-4')",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE model_attempt SET usage_json='{\"input_tokens\":9223372036854775807}'",
        [],
    )
    .unwrap();
    let v = read(&db, &Scope::Run("r0".into())).unwrap();
    assert_eq!(
        v["model_attempts"]["completed"]["input_tokens"]["overflow"],
        true
    );
    assert!(v["model_attempts"]["completed"]["input_tokens"]["known_sum"].is_null());
}
#[test]
fn saved_duration_matches_context_fraction_and_unknown_rules() {
    assert_eq!(
        saved_ms(
            Some("1969-12-31T23:59:59.999100000Z"),
            Some("1970-01-01T00:00:00.000900000Z")
        ),
        Some(1)
    );
    assert_eq!(
        saved_ms(
            Some("2026-10-30T00:00:00.000100000Z"),
            Some("2026-10-30T00:00:00.000900000Z")
        ),
        Some(0)
    );
    assert_eq!(saved_ms(Some(END), Some(START)), None);
    assert_eq!(saved_ms(Some("bad"), Some(END)), None);
}
#[test]
#[ignore = "controlled saved-usage scale measurement; synthetic temporary DB only"]
fn measure_public_saved_totals() {
    let root = tempfile::tempdir().unwrap();
    for spread in [false, true] {
        let path = root.path().join(if spread {
            "spread.sqlite"
        } else {
            "hot.sqlite"
        });
        let mut writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
            )
            .unwrap();
        writer
            .execute_batch(crate::storage::CURRENT_SCHEMA)
            .unwrap();
        seed(&mut writer, 1000, spread);
        drop(writer);
        let scopes = [
            ("turn", Scope::Turn("u0".into())),
            ("run", Scope::Run("r0".into())),
            (
                "thread",
                Scope::Thread {
                    id: "t0".into(),
                    children: false,
                },
            ),
            (
                "children",
                Scope::Thread {
                    id: "t0".into(),
                    children: true,
                },
            ),
        ];
        for legacy in [false, true] {
            if legacy {
                let db = Connection::open(&path).unwrap();
                let large=json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":75},"output_tokens":10,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":110,"attribution":"x".repeat(128*1024)}).to_string();
                db.execute(
                    "UPDATE model_attempt SET usage_json=?1 WHERE rowid<=256",
                    [large],
                )
                .unwrap();
            }
            for (label, scope) in &scopes {
                let mut samples = Vec::new();
                let mut first = 0.;
                let mut vm = 0;
                for iteration in 0..22 {
                    let db = Connection::open(&path).unwrap();
                    let started = Instant::now();
                    let mut trace = Trace::default();
                    let tx = db.unchecked_transaction().unwrap();
                    let v = inspect(&tx, scope, &mut trace).unwrap();
                    let elapsed = started.elapsed().as_secs_f64() * 1000.;
                    assert!(v["coverage"]["complete"].as_bool().unwrap());
                    let n = if *label == "children" || (*label == "thread" && !spread) {
                        100_000
                    } else if *label == "thread" {
                        1000
                    } else {
                        100
                    };
                    assert_eq!(v["model_attempts"]["completed"]["count"], n * 3 / 5);
                    assert_eq!(v["model_attempts"]["failed"]["count"], n / 5);
                    assert_eq!(v["model_duration_ms"]["known_sum"], n * 10);
                    assert_eq!(v["shell"]["count"], n / 10);
                    if iteration == 0 {
                        first = elapsed;
                        vm = trace.statements.iter().map(|(_, n)| *n).sum();
                        for (sql, _) in &trace.statements {
                            let mut plan =
                                db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                            let mut rows = plan.raw_query();
                            let mut lines = Vec::new();
                            while let Some(row) = rows.next().unwrap() {
                                lines.push(row.get::<_, String>(3).unwrap());
                            }
                            println!("PLAN {label}: {}", lines.join(" | "));
                        }
                    } else {
                        samples.push(elapsed);
                    }
                }
                samples.sort_by(f64::total_cmp);
                println!(
                    "MEASURE sqlite={} build_debug={} spread={spread} legacy256={legacy} scope={label} n={} fresh_conn_first_ms={first:.3} median_ms={:.3} p95_ms={:.3} vmsteps={vm} db_bytes={}",
                    rusqlite::version(),
                    cfg!(debug_assertions),
                    samples.len(),
                    samples[samples.len() / 2],
                    samples[(samples.len() * 95 / 100).min(samples.len() - 1)],
                    std::fs::metadata(&path).unwrap().len()
                );
                let db = Connection::open(&path).unwrap();
                let mut warm = Vec::new();
                for _ in 0..21 {
                    let t = Instant::now();
                    read(&db, scope).unwrap();
                    warm.push(t.elapsed().as_secs_f64() * 1000.);
                }
                warm.sort_by(f64::total_cmp);
                println!(
                    "WARM scope={label} median_ms={:.3} p95_ms={:.3}",
                    warm[10], warm[19]
                );
            }
        }
    }
}
