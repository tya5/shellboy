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
    assert_eq!(all["coverage"]["scope_complete"], true);
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
                    assert!(v["coverage"]["scope_complete"].as_bool().unwrap());
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

#[test]
#[ignore = "controlled varied-usage and per-query cause measurement; synthetic data only"]
fn measure_varied_query_costs() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("varied.sqlite");
    let mut writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .unwrap();
    writer
        .execute_batch(crate::storage::CURRENT_SCHEMA)
        .unwrap();
    seed(&mut writer, 1000, false);
    writer.execute("UPDATE model_attempt SET usage_json=json_set(usage_json,'$.input_tokens',100+rowid%97,'$.output_tokens',10+rowid%13)",[]).unwrap();
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
            db.execute("UPDATE model_attempt SET usage_json=json_set(usage_json,'$.attribution',?1||CAST(rowid AS TEXT)) WHERE rowid<=256",["x".repeat(128*1024)]).unwrap();
        }
        for (label, scope) in &scopes {
            let mut samples = Vec::new();
            let mut first = 0.;
            for i in 0..22 {
                let db = Connection::open(&path).unwrap();
                let t = Instant::now();
                let v = read(&db, scope).unwrap();
                let ms = t.elapsed().as_secs_f64() * 1000.;
                assert_eq!(
                    v["model_attempts"]["completed"]["count"],
                    if *label == "thread" || *label == "children" {
                        60_000
                    } else {
                        60
                    }
                );
                if i == 0 { first = ms } else { samples.push(ms) }
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "VARIED scope={label} legacy_distinct256={legacy} n=21 first_ms={first:.3} median_ms={:.3} p95_ms={:.3}",
                samples[10], samples[19]
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
                "VARIED_WARM scope={label} legacy_distinct256={legacy} median_ms={:.3} p95_ms={:.3}",
                warm[10], warm[19]
            );
        }
        let db = Connection::open(&path).unwrap();
        let scope = Scope::Thread {
            id: "t0".into(),
            children: false,
        };
        let mut trace = Trace::default();
        inspect(&db, &scope, &mut trace).unwrap();
        let selected = selection(&db, &scope, &mut Trace::default()).unwrap();
        for (sql, _) in trace.statements {
            let kind = if sql.contains("FROM model_attempt a") {
                if sql.contains("'completed'") {
                    "model-terminal"
                } else {
                    "model-active"
                }
            } else if sql.contains("shell_terminal_time") {
                "shell-terminal"
            } else if sql.contains("shell_active_owner") {
                "shell-active"
            } else if sql.contains("FROM run r") {
                "run"
            } else {
                continue;
            };
            let mut variants = vec![("actual", sql.clone())];
            if kind == "model-terminal" {
                let mut no_json = sql.replace(standard_values_sql(), "NULL");
                for (i, value) in [100, 75, 10, 2, 110].into_iter().enumerate() {
                    no_json = no_json.replace(
                        &usage::sql_valid_integer("standard_values", &format!("$[{i}]")),
                        &value.to_string(),
                    );
                }
                assert_ne!(no_json, sql, "diagnostic must remove actual JSON work");
                variants.push(("diagnostic-no-json", no_json.clone()));
                variants.push((
                    "diagnostic-no-duration-parse",
                    sql.replace("usage_saved_ms(a.started_at,a.ended_at)", "10"),
                ));
                variants.push((
                    "diagnostic-neither",
                    no_json.replace("usage_saved_ms(a.started_at,a.ended_at)", "10"),
                ));
            }
            for (variant, sql) in variants {
                let mut samples = Vec::new();
                let mut vm = 0;
                for _ in 0..21 {
                    let t = Instant::now();
                    let mut st = db.prepare(&sql).unwrap();
                    st.raw_bind_parameter(1, &selected.parameter).unwrap();
                    if st.parameter_count() == 2 {
                        st.raw_bind_parameter(2, chrono::Utc::now().to_rfc3339())
                            .unwrap();
                    }
                    {
                        let mut rows = st.raw_query();
                        while rows.next().unwrap().is_some() {}
                    }
                    samples.push(t.elapsed().as_secs_f64() * 1000.);
                    vm = st.get_status(StatementStatus::VmStep);
                }
                samples.sort_by(f64::total_cmp);
                println!(
                    "CAUSE scope=thread legacy_distinct256={legacy} kind={kind} variant={variant} median_ms={:.3} p95_ms={:.3} vmsteps={vm}",
                    samples[10], samples[19]
                );
            }
        }
    }
}

#[test]
fn compact_jsonb_projection_preserves_all_49_numeric_read_rules() {
    let db = db();
    let projected = (0..5)
        .map(|i| usage::sql_valid_integer("standard_values", &format!("$[{i}]")))
        .collect::<Vec<_>>()
        .join(",");
    let optimized = format!(
        "WITH a AS (SELECT 'completed' state,?1 usage_json),docs AS MATERIALIZED (SELECT {} standard_values FROM a) SELECT {projected} FROM docs",
        standard_values_sql()
    );
    let original = format!("SELECT {}", usage::sql_projection("?1"));
    for literal in [
        "0",
        "-0",
        "123",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "-1",
        "1.0",
        "1e0",
        "true",
        "false",
        "\"123\"",
        "null",
        "{}",
        "[]",
    ] {
        let raw = format!(
            r#"{{"input_tokens":{literal},"input_tokens_details":{{"cached_tokens":{literal}}},"output_tokens":{literal},"output_tokens_details":{{"reasoning_tokens":{literal}}},"total_tokens":{literal}}}"#
        );
        let expected = db
            .query_row(&original, [&raw], |r| usage::StandardUsage::read(r, 0))
            .unwrap();
        let actual = db
            .query_row(&optimized, [&raw], |r| usage::StandardUsage::read(r, 0))
            .unwrap();
        assert_eq!(actual, expected, "{literal}");
    }
    for raw in [
        "{",
        "not JSON",
        "null",
        "{}",
        r#"{"input_tokens_details":false}"#,
    ] {
        assert_eq!(
            db.query_row(&optimized, [raw], |r| usage::StandardUsage::read(r, 0))
                .unwrap(),
            usage::StandardUsage::default()
        );
    }
}

#[test]
fn active_saved_records_and_cancelled_rows_do_not_pollute_terminal_totals() {
    let mut db = db();
    seed(&mut db, 1, false);
    db.execute(
        "INSERT INTO run(id,thread_id,state,created_at)VALUES('live','t0','live',?1)",
        [START],
    )
    .unwrap();
    db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('live','t0','live','waiting',?1)",[START]).unwrap();
    db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES('live','live',0,0,'fixture','fixture',0,'inflight','{}','malformed',?1)",[START]).unwrap();
    db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('cancelled','u0',0,0,'fixture','fixture',0,'cancelled','{}','{\"input_tokens\":999}',?1,?2)",params![START,END]).unwrap();
    db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('live','live','live','live','running','.','{}','UTF-8','UTF-8','closed',?1)",[START]).unwrap();
    let v = read(
        &db,
        &Scope::Thread {
            id: "t0".into(),
            children: false,
        },
    )
    .unwrap();
    assert_eq!(v["model_attempts"]["completed"]["count"], 60);
    assert!(v["model_attempts"].get("inflight").is_none());
    assert!(v["model_attempts"].get("cancelled").is_none());
    assert_eq!(v["active"]["model_attempts"][0]["count"], 1);
    assert_eq!(v["active"]["run"]["count"], 1);
    assert_eq!(v["run"]["count"], 1);
    assert_eq!(v["active"]["shell"]["count"], 1);
    assert_eq!(v["shell"]["count"], 10);
    let expected = saved_ms(Some(START), v["active"]["observed_at"].as_str());
    assert_eq!(
        v["active"]["shell"]["duration_ms"]["known_sum"],
        json!(expected)
    );
    assert_eq!(
        v["active"]["shell"]["duration_ms"]["unknown_count"],
        u64::from(expected.is_none())
    );
}

#[test]
fn descendant_limit_deleted_bridge_and_cycles_are_honest() {
    let mut db = db();
    seed(&mut db, 1, false);
    db.execute("UPDATE thread SET deleted_at='now' WHERE id='t1'", [])
        .unwrap();
    db.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('hidden','t1','.','AGENTS.md','fixture','fixture','now')",[]).unwrap();
    db.execute("UPDATE thread SET parent_thread_id='t2' WHERE id='t0'", [])
        .unwrap();
    let scope = Scope::Thread {
        id: "t0".into(),
        children: true,
    };
    let selected = selection(&db, &scope, &mut Trace::default()).unwrap();
    assert!(selected.complete);
    assert!(!selected.threads.contains(&"hidden".into()));
    assert!(!selected.threads.contains(&"t1".into()));
    for i in 101..250 {
        db.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'t0','.','AGENTS.md','fixture','fixture','now')",[format!("t{i}")]).unwrap();
    }
    let v = read(&db, &scope).unwrap();
    assert_eq!(v["selected_threads"], MAX_LIST_LIMIT);
    assert_eq!(v["coverage"]["scope_complete"], false);
    assert_eq!(v["coverage"]["descendants"], "thread_limit");
}

#[test]
fn saved_shell_sum_can_exceed_i64_without_wrapping_or_failing() {
    let mut db = db();
    seed(&mut db, 1, false);
    db.execute(
        "DELETE FROM shell_execution WHERE id NOT IN ('s0-0','s0-1','s0-2')",
        [],
    )
    .unwrap();
    db.execute("UPDATE shell_execution SET shell_json='{\"completion\":{\"elapsed_ms\":9223372036854775807}}'",[]).unwrap();
    let scope = Scope::Run("r0".into());
    let v = read(&db, &scope).unwrap();
    assert_eq!(v["shell"]["duration_ms"]["overflow"], true);
    db.execute("DELETE FROM shell_execution WHERE id='s0-2'", [])
        .unwrap();
    let v = read(&db, &scope).unwrap();
    assert_eq!(v["shell"]["duration_ms"]["known_sum"], json!(u64::MAX - 1));
    assert_eq!(v["shell"]["duration_ms"]["overflow"], false);
}

#[test]
fn first_and_last_known_start_preserve_offsets_subseconds_and_negative_epoch_order() {
    let mut db = db();
    seed(&mut db, 1, false);
    db.execute(
        "DELETE FROM model_attempt WHERE id NOT IN ('a0-2','a0-3','a0-4')",
        [],
    )
    .unwrap();
    db.execute("UPDATE model_attempt SET started_at='1970-01-01T01:00:00.000000001+01:00',ended_at='1970-01-01T00:00:01Z' WHERE id='a0-2'",[]).unwrap();
    db.execute("UPDATE model_attempt SET started_at='1969-12-31T23:59:59.999999999Z',ended_at='1970-01-01T00:00:01Z' WHERE id='a0-3'",[]).unwrap();
    db.execute(
        "UPDATE model_attempt SET started_at='bad' WHERE id='a0-4'",
        [],
    )
    .unwrap();
    let v = read(&db, &Scope::Turn("u0".into())).unwrap();
    assert_eq!(
        v["attempt_period"]["first_known_started_at"],
        "1969-12-31T23:59:59.999999999Z"
    );
    assert_eq!(
        v["attempt_period"]["last_known_started_at"],
        "1970-01-01T00:00:00.000000001Z"
    );
    assert_eq!(v["attempt_period"]["unknown_start_count"], 1);
    assert_eq!(v["turn_lifetime_ms"], 10);
    assert!(
        !v.to_string().contains('|'),
        "internal sort keys are not public metadata"
    );
}

#[test]
fn requested_periods_use_saved_start_boundaries_and_keep_whole_lifetimes() {
    crate::readback::tests::watchdog(|| {
        let mut c = db();
        seed(&mut c, 1, false);
        c.execute("UPDATE model_attempt SET started_at='unknown'", [])
            .unwrap();
        for (id, at) in [
            ("a0-0", "2026-10-30T00:00:00Z"),
            ("a0-1", "2026-10-30T00:00:00.000000001Z"),
            ("a0-2", "2026-10-30T09:00:00.000000001+09:00"),
            ("a0-3", "2026-10-30T00:00:00.000000002Z"),
            ("a0-4", "2026-10-30T00:00:00.000000001Z"),
        ] {
            c.execute(
                "UPDATE model_attempt SET started_at=?2 WHERE id=?1",
                params![id, at],
            )
            .unwrap();
        }
        c.execute(
            "UPDATE model_attempt SET state='inflight',ended_at=NULL WHERE id='a0-4'",
            [],
        )
        .unwrap();
        c.execute("UPDATE shell_execution SET started_at='unknown'", [])
            .unwrap();
        c.execute("UPDATE shell_execution SET started_at='2026-10-30T00:00:00.000000001Z' WHERE id IN ('s0-0','s0-2')",[]).unwrap();
        c.execute("UPDATE shell_execution SET started_at='2026-10-30T00:00:00.000000002Z' WHERE id='s0-1'",[]).unwrap();
        c.execute(
            "UPDATE shell_execution SET state='running',ended_at=NULL WHERE id='s0-2'",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE run SET created_at='2026-10-30T00:00:00.000000001Z' WHERE id='r0'",
            [],
        )
        .unwrap();
        let w = crate::readback::Window::new(
            Some("2026-10-30T00:00:00.000000001Z"),
            Some("2026-10-30T00:00:00.000000002Z"),
        )
        .unwrap();
        let scope = Scope::Thread {
            id: "t0".into(),
            children: false,
        };
        let before = c.total_changes();
        let v = read_period_logged(&c, &scope, ReadContext::default(), None, &w).unwrap();
        assert_eq!(v["model_attempts"]["completed"]["count"], 1);
        assert_eq!(v["model_attempts"]["incomplete"]["count"], 1);
        assert!(v["model_attempts"].get("failed").is_none());
        assert_eq!(v["shell"]["count"], 1);
        assert_eq!(v["shell"]["duration_ms"]["known_sum"], 5);
        assert_eq!(v["active"]["shell"]["count"], 1);
        assert_eq!(v["run"]["count"], 1);
        assert_eq!(v["run"]["duration_ms"]["known_sum"], 9);
        assert_eq!(v["attempt_period"]["unknown_start_count"], 0);
        assert_eq!(c.total_changes(), before);
        let mut trace = Trace::default();
        inspect_period(&c, &scope, &mut trace, &w).unwrap();
        for (sql, _) in trace.statements {
            let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let parameter = if sql.contains("json_each(?1)") {
                r#"["t0"]"#
            } else {
                "t0"
            };
            let p = if st.parameter_count() == 2 {
                st.query_map(params![parameter, END], |r| r.get::<_, String>(3))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap()
            } else {
                st.query_map([parameter], |r| r.get::<_, String>(3))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap()
            };
            eprintln!("period usage: {p:?}");
            for d in p {
                let words = d.split_whitespace().collect::<Vec<_>>();
                assert!(
                    !(words.first() == Some(&"SCAN")
                        && words.get(1).is_some_and(|s| [
                            "a",
                            "s",
                            "r",
                            "u",
                            "t",
                            "thread",
                            "turn",
                            "model_attempt",
                            "shell_execution",
                            "run"
                        ]
                        .contains(s))),
                    "{d}"
                );
            }
        }
        let empty = crate::readback::Window::new(Some(START), Some(START)).unwrap();
        let z = read_period_logged(&c, &scope, ReadContext::default(), None, &empty).unwrap();
        assert_eq!(z["shell"]["count"], 0);
        assert_eq!(z["run"]["count"], 0);
        assert_eq!(z["model_attempts"], json!({}));
    });
}
