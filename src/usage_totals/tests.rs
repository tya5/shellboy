use super::*;

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
        let mut a=tx.prepare("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?2),?1,?2,0,0,'fixture','fixture',0,?3,'{}',?4,?5,?6)").unwrap();
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
    assert_eq!(v["coverage"]["summary"], "recorded");
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
    db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='live'),'live','live',0,0,'fixture','fixture',0,'inflight','{}','malformed',?1)",[START]).unwrap();
    db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u0'),'cancelled','u0',0,0,'fixture','fixture',0,'cancelled','{}','{\"input_tokens\":999}',?1,?2)",params![START,END]).unwrap();
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
        inspect_period(&c, &scope, &mut trace, &w, false).unwrap();
        for sql in trace.statements {
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
                            "run",
                            "m",
                            "app_meta"
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

#[test]
fn attempt_diagnostics_read_is_bounded_exact_read_only_and_not_an_aggregate() {
    let mut db = db();
    seed(&mut db, 1, false);
    let diagnostic = json!({"type":"cache_miss","reason":"fixture","cache_missed_tokens":0});
    let manifest =
        json!({"prompt_cache_diagnostics":diagnostic,"unrelated":"x".repeat(100_000)}).to_string();
    db.execute(
        "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='a0-0'",
        [manifest],
    )
    .unwrap();
    let before = db.total_changes();
    let result = read_attempt(&db, "a0-0").unwrap();
    assert_eq!(result["prompt_cache_diagnostics"], diagnostic);
    assert_eq!(result["usage"]["input_tokens"], 100);
    assert!(result.to_string().len() < 2048);
    assert_eq!(db.total_changes(), before);
    assert!(read_attempt(&db, "missing").is_err());
    assert!(read_attempt(&db, "a0-1").unwrap()["prompt_cache_diagnostics"].is_null());
    let huge =
        json!({"prompt_cache_diagnostics":{"type":"cache_miss","reason":"x".repeat(100_000)}})
            .to_string();
    db.execute(
        "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='a0-0'",
        [huge],
    )
    .unwrap();
    assert!(read_attempt(&db, "a0-0").unwrap()["prompt_cache_diagnostics"].is_null());
}

#[test]
fn saved_request_bytes_are_read_beside_reported_tokens_and_invalid_is_unknown() {
    use crate::model::wire_bytes::{Category, Label, measure};
    let mut db = db();
    seed(&mut db, 2, false);
    let wire = json!({"input":[{"role":"user","content":"日本語"}],"model":"m"});
    let bytes = measure(&wire, &[("/input/0".into(), Label::Whole(Category::User))]).unwrap();
    let manifest = json!({"input_bytes":bytes.to_value(),"unrelated":"x".repeat(100_000)});
    db.execute(
        "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='a1-99'",
        [manifest.to_string()],
    )
    .unwrap();
    let before = db.total_changes();
    let attempt = read_attempt(&db, "a1-99").unwrap();
    assert_eq!(attempt["input_bytes"], bytes.to_value());
    assert_eq!(
        attempt["input_bytes"]["total"],
        serde_json::to_vec(&wire).unwrap().len()
    );
    assert_eq!(attempt["usage"]["input_tokens"], 100);
    // Recorded before #186 (or not measured): unknown, not zero.
    assert!(read_attempt(&db, "a1-98").unwrap()["input_bytes"].is_null());
    assert_eq!(db.total_changes(), before);
    let mut wrong = bytes.to_value();
    wrong["categories"]["user"] = json!(1);
    db.execute(
        "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='a1-99'",
        [json!({"input_bytes":wrong}).to_string()],
    )
    .unwrap();
    assert!(read_attempt(&db, "a1-99").unwrap()["input_bytes"].is_null());
}

#[test]
fn connection_rates_keep_legacy_and_missing_waits_unknown() {
    let mut db = db();
    seed(&mut db, 1, false);
    db.execute(
        "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='a0-0'",
        [json!({"rate_observation":{"status":429,"applied_wait_ms":123}}).to_string()],
    )
    .unwrap();
    db.execute(
        "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='a0-1'",
        [json!({"rate_observation":{"status":503}}).to_string()],
    )
    .unwrap();
    let result = read_period_logged(
        &db,
        &Scope::Turn("u0".into()),
        ReadContext::default(),
        None,
        &Default::default(),
    )
    .unwrap();
    let rates = &result["connection_rates"]["by_connection"]["fixture"];
    assert_eq!(rates["http_429"], 1);
    assert_eq!(rates["http_5xx"], 1);
    assert_eq!(rates["applied_wait_ms"], 123);
    assert_eq!(rates["status_unknown"], 98);
    assert_eq!(rates["wait_unknown"], 99);
}

#[test]
fn connection_rates_survive_database_reopen_and_group_summary_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    {
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        seed(&mut db, 2, true);
        db.execute(
            "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='a0-0'",
            [json!({"rate_observation":{"status":429,"applied_wait_ms":12}}).to_string()],
        )
        .unwrap();
        for (at, rate) in [
            (0, json!({"status":429,"applied_wait_ms":0})),
            (1, Value::Null),
        ] {
            let manifest =
                json!({"summary":{"id":"summary","started_at":START},"rate_observation":rate});
            db.execute("INSERT INTO model_attempt(id,kind,thread_id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES(?1,'summary','t0','u0',0,0,'fixture','fixture',0,'completed',?2,?3)", params![format!("summary-{at}"),manifest.to_string(),START]).unwrap();
        }
    }
    let db = Connection::open(&path).unwrap();
    let result = read_period_logged(
        &db,
        &Scope::Thread {
            id: "t0".into(),
            children: true,
        },
        ReadContext::default(),
        None,
        &Default::default(),
    )
    .unwrap();
    let rates = &result["connection_rates"]["by_connection"]["fixture"];
    assert_eq!(rates["http_429"], 2);
    assert_eq!(rates["applied_wait_ms"], 12);
    assert_eq!(rates["status_unknown"], 200);
    assert_eq!(rates["wait_unknown"], 200);
}

#[test]
fn connection_rate_wait_saturation_reports_overflow() {
    let mut rates = RateCounts::default();
    rates.add(Some(429), Some(u64::MAX));
    rates.add(Some(503), Some(1));
    let value = json!(rates);
    assert_eq!(value["applied_wait_ms"], u64::MAX);
    assert_eq!(value["overflow"], true);
    assert_eq!(value["http_429"], 1);
    assert_eq!(value["http_5xx"], 1);
}

#[test]
fn bounded_usage_plan_uses_indexes_for_node_lower_bound() {
    let mut c = db();
    seed(&mut c, 2, false);
    c.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) VALUES('n1','u0','user','2026-10-29T00:00:00Z','{}',2)", []).unwrap();
    c.execute(
        "INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('t0',0,'n1')",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE model_attempt SET started_at='2026-10-29T00:00:00Z' WHERE turn_id='u0'",
        [],
    )
    .unwrap();
    let scope = Scope::Thread {
        id: "t0".into(),
        children: false,
    };
    let all_window =
        crate::readback::Window::new(Some("2026-10-28T00:00:00Z"), Some("2026-11-02T00:00:00Z"))
            .unwrap();
    let after_window =
        crate::readback::Window::new(Some("2026-10-30T00:00:00Z"), Some("2026-11-02T00:00:00Z"))
            .unwrap();
    let mut all = Trace::default();
    inspect_period(&c, &scope, &mut all, &all_window, false).unwrap();
    let mut after = Trace::default();
    inspect_period(&c, &scope, &mut after, &after_window, false).unwrap();
    let all_model = all
        .statements
        .iter()
        .find(|sql| sql.contains("FROM model_attempt a INDEXED BY attempts_by_turn"))
        .unwrap();
    let after_model = after
        .statements
        .iter()
        .find(|sql| sql.contains("FROM model_attempt a INDEXED BY attempts_by_turn"))
        .unwrap();
    assert!(all_model.contains("attempts_by_turn") && after_model.contains("attempts_by_turn"));
    let mut all_model_plan = c
        .prepare(&format!("EXPLAIN QUERY PLAN {all_model}"))
        .unwrap();
    let all_model_plan_details = all_model_plan
        .query_map(rusqlite::params!["[\"t0\"]", "2026-11-01T00:00:00Z"], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        all_model_plan_details
            .iter()
            .any(|line| line.contains("attempts_by_turn")),
        "{all_model_plan_details:?}"
    );
    let mut after_model_plan = c
        .prepare(&format!("EXPLAIN QUERY PLAN {after_model}"))
        .unwrap();
    let after_model_plan_details = after_model_plan
        .query_map(rusqlite::params!["[\"t0\"]", "2026-11-01T00:00:00Z"], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        after_model_plan_details
            .iter()
            .any(|line| line.contains("attempts_by_turn")),
        "{after_model_plan_details:?}"
    );
    let node_sql = "SELECT n.created_at FROM node n JOIN thread_path p ON p.node_id=n.id AND p.thread_id=?2 WHERE n.id=?1 AND n.deleted_reason IS NULL";
    let mut plan = c
        .prepare(&format!("EXPLAIN QUERY PLAN {node_sql}"))
        .unwrap();
    let details = plan
        .query_map(["n1", "t0"], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        details
            .iter()
            .any(|line| line.contains("SEARCH n USING INDEX sqlite_autoindex_node_1")),
        "{details:?}"
    );
    assert!(
        details
            .iter()
            .any(|line| line
                .contains("SEARCH p USING COVERING INDEX sqlite_autoindex_thread_path_2")),
        "{details:?}"
    );
}
