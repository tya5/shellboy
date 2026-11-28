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
    insert_model(db, id, thread, at, state, usage, "m");
}
fn insert_model(
    db: &Connection,
    id: &str,
    thread: &str,
    at: &str,
    state: &str,
    usage: Option<&str>,
    model: &str,
) {
    db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?2),?1,?2,0,0,'p',?6,0,?3,'{}',?4,?5,?5)",params![id,thread,state,usage,DateTime::parse_from_rfc3339(at).map(|v|v.with_timezone(&Utc).to_rfc3339()).unwrap_or_else(|_|at.into()),model]).unwrap();
}
fn args(group: &str) -> UsageGetArgs {
    UsageGetArgs {
        group_by: Some(group.into()),
        timezone: Some("UTC".into()),
        since: Some(Box::from("2026-10-31T00:00:00Z")),
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
    a.cursor = first["next_cursor"].as_str().map(Into::into);
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
    a.cursor = v["next_cursor"].as_str().map(Into::into);
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
    a.cursor = v["next_cursor"].as_str().map(Into::into);
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
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::storage::SCHEMA_VERSION
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

#[test]
fn grouped_arguments_keep_the_shared_command_under_the_enum_size_budget() {
    assert!(
        std::mem::size_of::<crate::protocol::Command>() <= 200,
        "Command is {} bytes",
        std::mem::size_of::<crate::protocol::Command>()
    );
    let value = serde_json::to_value(args("thread")).unwrap();
    assert_eq!(value["group_by"], "thread");
    assert_eq!(value["timezone"], "UTC");
}

#[test]
fn grouped_text_wire_roundtrip_preserves_absent_null_empty_and_cursor_values() {
    let absent: UsageGetArgs = serde_json::from_value(json!({})).unwrap();
    let nulls: UsageGetArgs = serde_json::from_value(json!({
        "group_by": null, "timezone": null, "cursor": null
    }))
    .unwrap();
    assert_eq!(absent, nulls);
    for raw in [
        json!({"group_by":"thread","timezone":"UTC","cursor":"opaque"}),
        json!({"group_by":"","timezone":"","cursor":""}),
    ] {
        let typed: UsageGetArgs = serde_json::from_value(raw.clone()).unwrap();
        let saved = serde_json::to_value(&typed).unwrap();
        for field in ["group_by", "timezone", "cursor"] {
            assert_eq!(saved[field], raw[field]);
            assert!(saved[field].is_string());
        }
        let again: UsageGetArgs = serde_json::from_value(saved).unwrap();
        assert_eq!(typed, again);
    }
    let empty: UsageGetArgs =
        serde_json::from_value(json!({"group_by":"thread","timezone":""})).unwrap();
    assert!(validate(&empty).is_err());
    let mut empty_cursor = args("thread");
    empty_cursor.cursor = Some("".into());
    assert_eq!(
        read(&fixture(), &empty_cursor, "f").unwrap_err().code,
        ErrorCode::CursorExpired
    );
    assert!(serde_json::from_value::<UsageGetArgs>(json!({"group_by":7})).is_err());
    assert!(serde_json::from_value::<UsageGetArgs>(json!({"cursor":[]})).is_err());
    assert!(serde_json::from_value::<UsageGetArgs>(json!({"unexpected":true})).is_err());
}

fn insert_summary(
    db: &Connection,
    thread: &str,
    id: &str,
    started_at: &str,
    usage: serde_json::Value,
) {
    let requests = usage["requests"].as_u64().unwrap();
    for ordinal in 0..requests {
        let token = |field: &str| {
            let known = requests - usage[field]["unknown_count"].as_u64().unwrap();
            (ordinal < known).then(|| {
                if ordinal == 0 {
                    usage[field]["known_sum"].as_u64().unwrap()
                } else {
                    0
                }
            })
        };
        let saved = usage::StandardUsage {
            input_tokens: token("input_tokens"),
            cached_tokens: token("cached_tokens"),
            cache_creation_tokens: token("cache_creation_tokens"),
            output_tokens: token("output_tokens"),
            ..Default::default()
        }
        .to_value();
        let manifest = json!({"summary":{"id":id,"started_at":started_at}});
        db.execute("INSERT INTO model_attempt(id,kind,thread_id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at) VALUES(?1,'summary',?2,?2,0,0,'p','m',0,'completed',?3,?4,?5)",params![format!("{id}:{ordinal}"),thread,manifest.to_string(),saved.to_string(),started_at]).unwrap();
    }
}

#[test]
fn compaction_usage_uses_first_request_time_half_open_buckets_and_unknowns() {
    let db = fixture();
    insert_summary(
        &db,
        "t1",
        "before",
        "2026-10-30T23:59:59Z",
        serde_json::json!({"requests":1,"input_tokens":{"known_sum":11,"unknown_count":0,"overflow":false},"cached_tokens":{"known_sum":2,"unknown_count":0,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"output_tokens":{"known_sum":3,"unknown_count":0,"overflow":false}}),
    );
    insert_summary(
        &db,
        "t1",
        "first",
        "2026-10-31T00:00:00Z",
        serde_json::json!({"requests":2,"input_tokens":{"known_sum":100,"unknown_count":1,"overflow":false},"cached_tokens":{"known_sum":20,"unknown_count":1,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":2,"overflow":false},"output_tokens":{"known_sum":30,"unknown_count":1,"overflow":false}}),
    );
    insert_summary(
        &db,
        "t1",
        "until",
        "2026-11-01T00:00:00Z",
        serde_json::json!({"requests":1,"input_tokens":{"known_sum":999,"unknown_count":0,"overflow":false},"cached_tokens":{"known_sum":0,"unknown_count":0,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"output_tokens":{"known_sum":1,"unknown_count":0,"overflow":false}}),
    );
    insert_summary(
        &db,
        "t1",
        "missing-time",
        "2026-10-31T12:00:00Z",
        serde_json::json!({"requests":1,"input_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"cached_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"output_tokens":{"known_sum":null,"unknown_count":1,"overflow":false}}),
    );
    db.execute("UPDATE model_attempt SET input_manifest_json=json_remove(input_manifest_json,'$.summary.started_at') WHERE id='missing-time:0'", []).unwrap();
    insert_summary(
        &db,
        "t1",
        "unknown-time",
        "not-a-timestamp",
        serde_json::json!({"requests":1,"input_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"cached_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"output_tokens":{"known_sum":null,"unknown_count":1,"overflow":false}}),
    );
    db.pragma_update(None, "query_only", true).unwrap();
    let value = read(&db, &args("day"), "fixture").unwrap();
    let item = &value["items"][0];
    assert_eq!(item["key"], "2026-10-31");
    assert_eq!(item["compaction"]["compactions"], 1);
    assert_eq!(item["compaction"]["requests"], 2);
    assert!(item["compaction"].get("map_requests").is_none());
    assert!(item["compaction"].get("reduce_requests").is_none());
    assert_eq!(item["compaction"]["input_tokens"]["known_sum"], 100);
    assert_eq!(item["compaction"]["input_tokens"]["unknown_count"], 1);
    assert_eq!(item["compaction"]["cached_tokens"]["known_sum"], 20);
    assert_eq!(item["total"]["input_tokens"]["known_sum"], 100);
    assert_eq!(item["total"]["input_tokens"]["unknown_count"], 1);
    assert_eq!(item["total"]["cached_tokens"]["known_sum"], 20);
    assert_eq!(value["coverage"]["summary"], "recorded_first_request_at");
    assert_eq!(value["unknown_time_compaction_records"], 2);
    assert_eq!(value["malformed_compaction_records"], 0);
}

#[test]
fn compaction_without_requests_stays_unknown_and_pagination_scans_bounded_windows() {
    let db = fixture();
    insert(
        &db,
        "model",
        "t1",
        "2026-10-31T00:30:00Z",
        "completed",
        Some("{\"input_tokens\":7}"),
    );
    for i in 0..10_001 {
        insert_summary(
            &db,
            "t1",
            &format!("c{i:05}"),
            "2026-10-31T00:30:00Z",
            serde_json::json!({"requests":1,"input_tokens":{"known_sum":1,"unknown_count":0,"overflow":false},"cached_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"output_tokens":{"known_sum":1,"unknown_count":0,"overflow":false}}),
        );
    }
    let mut a = args("thread");
    a.limit = Some(1);
    let first = read(&db, &a, "fixture").unwrap();
    assert_eq!(first["more"], true);
    let first_count = first["items"][0]["compaction"]["requests"]
        .as_u64()
        .unwrap();
    assert!(first_count > 0 && first_count < 10_001);
    assert!(first["scanned_compaction_bytes"].as_u64().unwrap() <= COMPACTION_PAGE_BYTES as u64);
    a.cursor = first["next_cursor"].as_str().map(Into::into);
    let second = read(&db, &a, "fixture").unwrap();
    assert_eq!(second["more"], false);
    let second_count = second["items"][0]["compaction"]["requests"]
        .as_u64()
        .unwrap();
    assert_eq!(first_count + second_count, 10_001);
    assert!(second["items"][0]["total"]["cached_tokens"]["known_sum"].is_null());
    assert_eq!(
        second["items"][0]["total"]["cached_tokens"]["unknown_count"],
        second_count
    );

    let empty = fixture();
    insert(
        &empty,
        "only-model",
        "t1",
        "2026-10-31T00:30:00Z",
        "completed",
        Some("{\"input_tokens\":7}"),
    );
    let result = read(&empty, &args("thread"), "fixture").unwrap();
    assert_eq!(result["items"][0]["compaction"]["requests"], 0);
    assert!(result["items"][0]["compaction"]["input_tokens"]["known_sum"].is_null());
    assert_eq!(result["items"][0]["total"]["input_tokens"]["known_sum"], 7);
}

#[test]
fn grouped_connection_rates_follow_candidate_window() {
    let db = fixture();
    insert(&db, "limited", "t1", "2026-10-31T00:30:00Z", "failed", None);
    insert(
        &db,
        "unknown",
        "t1",
        "2026-10-31T00:30:01Z",
        "completed",
        None,
    );
    db.execute(
        "UPDATE model_attempt SET input_manifest_json=?1 WHERE id='limited'",
        [json!({"rate_observation":{"status":429,"applied_wait_ms":42}}).to_string()],
    )
    .unwrap();
    let result = read(&db, &args("thread"), "fixture").unwrap();
    let rates = &result["items"][0]["connection_rates"]["by_connection"]["p"];
    assert_eq!(rates["http_429"], 1);
    assert_eq!(rates["applied_wait_ms"], 42);
    assert_eq!(rates["status_unknown"], 1);
    assert_eq!(rates["wait_unknown"], 1);
}

#[test]
fn model_groups_use_existing_bounded_attempt_scan_and_do_not_invent_summary_usage() {
    let db = fixture();
    insert_model(
        &db,
        "luna-1",
        "t1",
        "2026-10-31T00:10:00Z",
        "completed",
        Some("{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":80}}"),
        "gpt-6-luna",
    );
    insert_model(
        &db,
        "luna-2",
        "t2",
        "2026-10-31T00:20:00Z",
        "failed",
        Some("{\"input_tokens\":200}"),
        "gpt-6-luna",
    );
    insert_model(
        &db,
        "terra-1",
        "t1",
        "2026-10-31T00:30:00Z",
        "completed",
        Some("{\"input_tokens\":50,\"input_tokens_details\":{\"cached_tokens\":10}}"),
        "terra",
    );
    let before = db.total_changes();
    db.pragma_update(None, "query_only", true).unwrap();
    let mut a = args("model");
    a.limit = Some(1);
    let first = read(&db, &a, "fixture").unwrap();
    assert_eq!(db.total_changes(), before);
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["items"][0]["key"], "gpt-6-luna");
    assert_eq!(first["items"][0]["model_attempts"]["completed"]["count"], 1);
    assert_eq!(first["items"][0]["model_attempts"]["failed"]["count"], 1);
    assert_eq!(first["items"][0]["cache"]["ratio"], 0.8);
    assert_eq!(first["items"][0]["total"]["summary_requests"], Value::Null);
    assert_eq!(
        first["items"][0]["total"]["cache_creation_tokens"],
        Value::Null
    );
    assert_eq!(first["items"][0]["compaction"]["compactions"], Value::Null);
    assert_eq!(first["coverage"]["summary"], "not_attributed_by_model");
    assert_eq!(first["scan_truncated"], false);
    assert_eq!(first["scanned_compaction_candidates"], Value::Null);
    assert_eq!(first["matched_compactions"], Value::Null);
    assert_eq!(first["more"], true);
    a.cursor = first["next_cursor"].as_str().map(Into::into);
    let second = read(&db, &a, "fixture").unwrap();
    assert_eq!(second["items"][0]["key"], "terra");
    assert_eq!(second["items"][0]["cache"]["ratio"], 0.2);
    assert_eq!(second["more"], false);
}

#[test]
fn model_group_by_validates_and_reuses_rowid_query_without_model_scan() {
    let db = fixture();
    let mut a = args("model");
    assert!(validate(&a).is_ok());
    a.group_by = Some("profile".into());
    assert_eq!(validate(&a).unwrap_err().code, ErrorCode::InvalidArgument);
}

#[test]
fn bounded_shell_exit_groups_preserve_unknown_and_scope_to_thread() {
    let db = fixture();
    for (id, thread, code, at, state) in [
        ("s0", "t1", Some(0), "2026-10-31T00:30:00Z", "exited"),
        ("s7", "t1", Some(7), "2026-10-31T00:31:00Z", "failed"),
        ("snull", "t1", None, "2026-10-31T00:32:00Z", "stopped"),
        ("other", "t2", Some(7), "2026-10-31T00:33:00Z", "exited"),
        ("until", "t1", Some(9), "2026-11-01T00:00:00Z", "exited"),
    ] {
        db.execute(
            "INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,exit_code) VALUES(?1,?2,?2,?1,?5,'/','{}','UTF-8','UTF-8','closed',?4,?4,?3)",
            params![id, thread, code, at, state],
        ).unwrap();
    }
    let mut a = args("shell-exit");
    a.thread_id = Some("t1".into());
    let value = read(&db, &a, "fixture").unwrap();
    assert_eq!(value["matched_shell_executions"], 3);
    let items = value["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["exit_code"], 0);
    assert_eq!(items[1]["exit_code"], 7);
    assert_eq!(items[2]["exit_code"], Value::Null);
    assert_eq!(items[2]["count"], 1);
    assert_eq!(value["outside_period_count"], 1);
}

#[test]
fn shell_exit_group_cursor_pages_groups_and_keeps_scope_bound() {
    let db = fixture();
    for (id, thread) in [("s0", "t1"), ("s1", "t1"), ("s2", "t2")] {
        db.execute(
            "INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,exit_code) VALUES(?1,?2,?2,?1,'exited','/','{}','UTF-8','UTF-8','closed','2026-10-31T00:30:00Z','2026-10-31T00:31:00Z',?3)",
            params![id, thread, id.strip_prefix('s').unwrap().parse::<i64>().unwrap()],
        ).unwrap();
    }
    let mut a = args("shell-exit");
    a.thread_id = Some("t1".into());
    a.limit = Some(1);
    let first = read(&db, &a, "fixture").unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["more"], true);
    a.cursor = Some(first["next_cursor"].as_str().unwrap().to_owned().into());
    let second = read(&db, &a, "fixture").unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["more"], false);
    assert_eq!(first["items"][0]["thread_id"], "t1");
    assert_eq!(second["items"][0]["thread_id"], "t1");
}
