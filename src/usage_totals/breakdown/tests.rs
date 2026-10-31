use super::*;
fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    for id in ["t1", "t2", "deleted"] {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','','p','m','now')", [id]).unwrap();
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?1,'live','now')",
            [id],
        )
        .unwrap();
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?1,?1,'running','now')", [id]).unwrap();
    }
    db.execute("UPDATE thread SET deleted_at='now' WHERE id='deleted'", [])
        .unwrap();
    db
}
fn insert(db: &Connection, id: &str, thread: &str, at: &str, state: &str, usage: Option<&str>) {
    db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES(?1,?2,0,0,'p','m',0,?3,'{}',?4,?5,?5)",params![id,thread,state,usage,DateTime::parse_from_rfc3339(at).map(|v|v.with_timezone(&Utc).to_rfc3339()).unwrap_or_else(|_|at.into())]).unwrap();
}
fn args(group: &str) -> UsageGetArgs {
    UsageGetArgs {
        group_by: Some(group.into()),
        timezone: Some("UTC".into()),
        since: Some("2026-10-31T00:00:00Z".into()),
        until: Some("2026-11-01T00:00:00Z".into()),
        ..Default::default()
    }
}
#[test]
fn exact_statistics_periods_states_unknowns_and_read_only_counts() {
    let db = fixture();
    for i in 1..=20 {
        insert(&db,&format!("a{i:02}"),"t1","2026-10-31T00:30:00Z","completed",Some(&json!({"input_tokens":i*10,"input_tokens_details":{"cached_tokens":i*5},"output_tokens":i,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":i*11}).to_string()));
    }
    insert(
        &db,
        "other",
        "t2",
        "2026-10-31T01:00:00Z",
        "failed",
        Some("{\"input_tokens\":300,\"output_tokens\":4}"),
    );
    insert(
        &db,
        "until",
        "t1",
        "2026-11-01T00:00:00Z",
        "completed",
        Some("{\"input_tokens\":999}"),
    );
    insert(
        &db,
        "before",
        "t1",
        "2026-10-30T23:59:59.999999999Z",
        "completed",
        None,
    );
    insert(&db, "unknown-time", "t1", "bad", "completed", None);
    insert(
        &db,
        "deleted",
        "deleted",
        "2026-10-31T00:00:00Z",
        "completed",
        Some("{\"input_tokens\":999}"),
    );
    let changes = db.total_changes();
    db.pragma_update(None, "query_only", true).unwrap();
    let value = read(&db, &args("thread"), "fixture").unwrap();
    assert_eq!(db.total_changes(), changes);
    assert_eq!(value["items"].as_array().unwrap().len(), 2);
    let g = &value["items"][0];
    assert_eq!(g["key"], "t1");
    assert_eq!(g["model_attempts"]["completed"]["count"], 20);
    for (field, sum) in [
        ("input_tokens", 2100),
        ("cached_tokens", 1050),
        ("output_tokens", 210),
        ("reasoning_tokens", 20),
        ("total_tokens", 2310),
        ("duration_ms", 0),
    ] {
        assert_eq!(
            g["model_attempts"]["completed"][field]["known_sum"], sum,
            "{field}"
        );
        assert_eq!(g["model_attempts"]["completed"][field]["unknown_count"], 0);
    }
    let stats = &g["input_per_request"];
    assert_eq!(stats["known_count"], 20);
    assert_eq!(stats["unknown_count"], 0);
    assert_eq!(stats["mean"], 105.0);
    assert_eq!(stats["max"], 200);
    assert_eq!(stats["p50"], 100);
    assert_eq!(stats["p95"], 190);
    assert_eq!(
        stats["max_attempt"],
        json!({"attempt_id":"a20","turn_id":"t1","run_id":"t1","thread_id":"t1"})
    );
    assert_eq!(g["cache"]["ratio"], 0.5);
    assert_eq!(g["cache"]["paired_count"], 20);
    assert_eq!(
        value["items"][1]["model_attempts"]["failed"]["input_tokens"]["known_sum"],
        300
    );
    assert!(value["items"][1]["cache"]["ratio"].is_null());
    assert_eq!(value["more"], false);
    assert_eq!(value["excluded_time_count"], 1);
    assert_eq!(value["outside_period_count"], 2);
    eprintln!(
        "usage101 synthetic exact stats elapsed_ms={}",
        value["elapsed_ms"]
    );
}
#[test]
fn malformed_legacy_usage_zero_and_independent_numeric_validation() {
    let db = fixture();
    for (id, raw) in [
        ("bad", "garbage"),
        ("null", "null"),
        ("negative", "{\"input_tokens\":-1}"),
        ("real", "{\"input_tokens\":1e0}"),
        ("big", "{\"input_tokens\":9223372036854775808}"),
        ("str", "{\"input_tokens\":\"3\"}"),
        (
            "zero",
            "{\"input_tokens\":0,\"input_tokens_details\":{\"cached_tokens\":0}}",
        ),
    ] {
        insert(
            &db,
            id,
            "t1",
            "2026-10-31T00:00:00Z",
            "completed",
            Some(raw),
        );
    }
    insert(
        &db,
        "cancelled",
        "t1",
        "2026-10-31T00:00:00Z",
        "cancelled",
        Some("{\"input_tokens\":999}"),
    );
    insert(
        &db,
        "active",
        "t1",
        "2026-10-31T00:00:00Z",
        "inflight",
        Some("{\"input_tokens\":999}"),
    );
    let v = read(&db, &args("thread"), "fixture").unwrap();
    let g = &v["items"][0];
    assert_eq!(g["input_per_request"]["known_count"], 1);
    assert_eq!(g["input_per_request"]["unknown_count"], 6);
    assert_eq!(g["input_per_request"]["mean"], 0.0);
    assert_eq!(g["input_per_request"]["p95"], 0);
    assert_eq!(g["cache"]["paired_count"], 1);
    assert!(g["cache"]["ratio"].is_null());
    assert_eq!(
        g["model_attempts"]["completed"]["input_tokens"]["unknown_count"],
        6
    );
}
#[test]
fn offsets_hour_day_buckets_subseconds_and_tie_identity() {
    let db = fixture();
    insert(
        &db,
        "z",
        "t1",
        "2026-10-31T00:00:00Z",
        "completed",
        Some("{\"input_tokens\":10}"),
    );
    insert(
        &db,
        "a",
        "t1",
        "2026-10-31T01:00:00+01:00",
        "incomplete",
        Some("{\"input_tokens\":10}"),
    );
    insert(
        &db,
        "b",
        "t1",
        "2026-10-31T01:00:00.000000001Z",
        "completed",
        None,
    );
    let mut a = args("hour");
    a.timezone = Some("-01:00".into());
    let v = read(&db, &a, "f").unwrap();
    assert_eq!(v["items"][0]["key"], "2026-10-30T23:00:00-01:00");
    assert_eq!(
        v["items"][0]["input_per_request"]["max_attempt"]["attempt_id"],
        "a"
    );
    assert_eq!(v["items"][1]["key"], "2026-10-31T00:00:00-01:00");
    a.group_by = Some("day".into());
    let v = read(&db, &a, "f").unwrap();
    assert_eq!(v["items"][0]["key"], "2026-10-30");
    a.timezone = None;
    assert!(read(&db, &a, "f").is_ok());
}
#[test]
fn group_cursor_bounds_scope_owner_filters_and_watermark() {
    let db = fixture();
    for thread in ["t1", "t2"] {
        insert(
            &db,
            thread,
            thread,
            "2026-10-31T00:00:00Z",
            "completed",
            Some("{\"input_tokens\":1}"),
        );
    }
    let mut a = args("thread");
    a.limit = Some(1);
    let first = read(&db, &a, "f").unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["more"], true);
    a.cursor = first["next_cursor"].as_str().map(str::to_owned);
    assert_eq!(
        read(&db, &a, "other-owner").unwrap_err().code,
        ErrorCode::CursorExpired
    );
    let mut altered = a.clone();
    altered.timezone = Some("+01:00".into());
    assert!(read(&db, &altered, "f").is_err());
    insert(
        &db,
        "new",
        "t2",
        "2026-10-31T00:00:00Z",
        "completed",
        Some("{\"input_tokens\":999}"),
    );
    let second = read(&db, &a, "f").unwrap();
    assert_eq!(second["items"][0]["key"], "t2");
    assert_eq!(second["items"][0]["input_per_request"]["max"], 1);
    assert_eq!(second["more"], false);
    let mut scoped = args("thread");
    scoped.thread_id = Some("t1".into());
    assert_eq!(
        read(&db, &scoped, "f").unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    scoped.thread_id = None;
    scoped.run_id = Some("t2".into());
    assert_eq!(
        read(&db, &scoped, "f").unwrap()["items"][0]["model_attempts"]["completed"]["count"],
        2
    );
    scoped.run_id = None;
    scoped.turn_id = Some("t1".into());
    assert_eq!(read(&db, &scoped, "f").unwrap()["items"][0]["key"], "t1");
}
#[test]
fn row_cap_continuation_and_empty_filtered_pages_are_honest() {
    let db = fixture();
    db.execute_batch("BEGIN").unwrap();
    for i in 0..=ROW_LIMIT {
        insert(
            &db,
            &format!("{i:05}"),
            "t1",
            "2026-10-31T00:00:00Z",
            "completed",
            Some("{\"input_tokens\":1}"),
        );
    }
    db.execute_batch("COMMIT").unwrap();
    let mut a = args("thread");
    let v = read(&db, &a, "f").unwrap();
    assert_eq!(v["scanned_candidates"], ROW_LIMIT);
    assert_eq!(v["matched_attempts"], ROW_LIMIT);
    assert_eq!(
        v["items"][0]["model_attempts"]["completed"]["count"],
        ROW_LIMIT
    );
    assert_eq!(v["scan_truncated"], true);
    assert_eq!(v["more"], true);
    a.cursor = v["next_cursor"].as_str().map(str::to_owned);
    let v = read(&db, &a, "f").unwrap();
    assert_eq!(v["scanned_candidates"], 1);
    assert_eq!(v["more"], false);
    assert_eq!(v["scan_truncated"], true);
    a.cursor = None;
    a.thread_id = Some("t2".into());
    let v = read(&db, &a, "f").unwrap();
    assert!(v["items"].as_array().unwrap().is_empty());
    assert_eq!(v["more"], true);
    assert!(v["next_cursor"].is_string());
    eprintln!("usage101 capped synthetic elapsed_ms={}", v["elapsed_ms"]);
}
#[test]
fn maximum_groups_and_validation_bounds() {
    let db = fixture();
    let mut a = args("hour");
    a.until = Some("2026-11-10T00:00:00Z".into());
    let start = DateTime::parse_from_rfc3339(a.since.as_deref().unwrap()).unwrap();
    for h in 0..169 {
        insert(
            &db,
            &format!("a{h}"),
            "t1",
            &(start + Duration::hours(h)).to_rfc3339(),
            "completed",
            None,
        );
    }
    let v = read(&db, &a, "f").unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 168);
    assert_eq!(v["more"], true);
    a.cursor = v["next_cursor"].as_str().map(str::to_owned);
    assert_eq!(
        read(&db, &a, "f").unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for (key, value) in [
        ("limit", json!(0)),
        ("limit", json!(169)),
        ("timezone", json!("bad")),
        ("timezone", json!("+24:00")),
        ("group_by", json!("month")),
        ("until", json!("2027-01-01T00:00:00Z")),
    ] {
        let mut raw = serde_json::to_value(args("hour")).unwrap();
        raw[key] = value;
        let a: UsageGetArgs = serde_json::from_value(raw).unwrap();
        assert!(validate(&a).is_err(), "{key}");
    }
    let mut a = args("thread");
    a.group_by = None;
    assert!(validate(&a).is_err());
}
#[test]
fn existing_rowid_query_plan_and_bounded_scan_counts() {
    let db = fixture();
    let plan = db
        .prepare(&format!("EXPLAIN QUERY PLAN {}", query()))
        .unwrap()
        .query_map(
            params!["2026-10-31T00:00:00", "2026-11-01T00:00:01", 0, i64::MAX],
            |r| r.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(
        plan.contains("SEARCH model_attempt USING INTEGER PRIMARY KEY"),
        "{plan}"
    );
    assert!(!plan.contains("SCAN model_attempt"), "{plan}");
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        17
    );
}

#[test]
fn descendants_overflow_and_unreported_input_are_not_inferred() {
    let db = fixture();
    db.execute("UPDATE thread SET parent_thread_id='t1' WHERE id='t2'", [])
        .unwrap();
    for id in ["a", "b", "c"] {
        insert(
            &db,
            id,
            "t2",
            "2026-10-31T00:00:00Z",
            "completed",
            Some(
                "{\"input_tokens\":9223372036854775807,\"input_tokens_details\":{\"cached_tokens\":9223372036854775807}}",
            ),
        );
    }
    insert(
        &db,
        "unknown",
        "t2",
        "2026-10-31T00:00:00Z",
        "incomplete",
        None,
    );
    let mut a = args("thread");
    a.thread_id = Some("t1".into());
    assert!(
        read(&db, &a, "f").unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    a.include_children = true;
    let v = read(&db, &a, "f").unwrap();
    let g = &v["items"][0];
    assert_eq!(
        g["model_attempts"]["completed"]["input_tokens"]["overflow"],
        true
    );
    assert!(g["model_attempts"]["completed"]["input_tokens"]["known_sum"].is_null());
    assert_eq!(g["input_per_request"]["max"], i64::MAX);
    assert_eq!(g["input_per_request"]["unknown_count"], 1);
    assert_eq!(g["cache"]["ratio"], 1.0);
}

#[test]
fn legacy_non_utc_and_missing_times_are_counted_not_guessed() {
    let db = fixture();
    for id in ["offset", "z", "empty"] {
        insert(&db, id, "t1", "2026-10-31T00:00:00Z", "completed", None);
    }
    db.execute(
        "UPDATE model_attempt SET started_at='2026-10-31T09:00:00+09:00' WHERE id='offset'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE model_attempt SET started_at='2026-10-31T00:00:00Z' WHERE id='z'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE model_attempt SET started_at='' WHERE id='empty'",
        [],
    )
    .unwrap();
    let v = read(&db, &args("thread"), "f").unwrap();
    assert_eq!(v["excluded_time_count"], 3);
    assert_eq!(v["scanned_candidates"], 3);
    assert!(v["items"].as_array().unwrap().is_empty());
}

#[test]
fn fractional_boundaries_are_exact_after_coarse_utc_string_comparison() {
    let db = fixture();
    for (id, at) in [
        ("whole", "2026-10-31T00:00:00+00:00"),
        ("low", "2026-10-31T00:00:00.1+00:00"),
        ("inside", "2026-10-31T00:00:00.100000001+00:00"),
        ("high", "2026-10-31T00:00:00.2+00:00"),
    ] {
        insert(
            &db,
            id,
            "t1",
            at,
            "completed",
            Some("{\"input_tokens\":10}"),
        );
    }
    let mut a = args("thread");
    a.since = Some("2026-10-31T00:00:00.1Z".into());
    a.until = Some("2026-10-31T00:00:00.2Z".into());
    let v = read(&db, &a, "f").unwrap();
    assert_eq!(v["matched_attempts"], 2);
    assert_eq!(v["outside_period_count"], 2);
    assert_eq!(
        v["items"][0]["model_attempts"]["completed"]["input_tokens"]["known_sum"],
        20
    );
}
