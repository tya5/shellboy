use super::*;
use crate::frontend::sql_trace::Statements;
use rusqlite::{Connection, params};
use serde_json::json;

fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'),('other','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','2026-10-25T00:00:00Z'),('other-run','other','completed','2026-10-25T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2026-10-25T00:00:00Z'),('new','t','r','completed','2026-10-25T00:00:10Z'),('future-turn','t','r','completed','2026-10-25T00:00:30Z'),('other-u','other','other-run','completed','2026-10-25T00:00:00Z');").unwrap();
    let error = |message: &str| {
        json!({"error":{"kind":"provider","message":message,"status":500}}).to_string()
    };
    let tied = error("tied latest🦀");
    for (id, turn, at, body) in [
        ("first", "u", "01", error("first")),
        ("tie-first", "u", "02", error("tie first")),
        ("tie-last", "u", "02", tied.clone()),
        ("future", "u", "20", error("future")),
        ("new-first", "new", "11", error("new turn")),
        ("new-future", "new", "30", error("new future")),
        ("unrelated", "other-u", "02", tied.clone()),
        ("invalid", "u", "02", "malformed json".into()),
        ("no-object", "u", "02", json!({"error":null}).to_string()),
    ] {
        attempt(&db, id, turn, at, &body);
    }
    for (id, turn, body) in [
        ("source-first", "u", tied.clone()),
        ("source-last", "u", tied.clone()),
        ("wrong-turn-source", "other-u", tied.clone()),
        ("different-bytes", "u", format!("{tied} ")),
    ] {
        source(&db, id, turn, &body);
    }
    let large = error(&"🦀".repeat(40_000));
    assert!(large.chars().count() < 131072 && large.len() > 131072);
    attempt(&db, "large", "u", "04", &large);
    source(&db, "large-source", "u", &large);
    db
}

fn attempt(db: &Connection, id: &str, turn: &str, second: &str, body: &str) {
    db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES(?1,?2,0,0,'p','m',0,'failed','{}',?3,'2026-10-25T00:00:00Z',?4)", params![id,turn,body,format!("2026-10-25T00:00:{second}Z")]).unwrap();
}

fn source(db: &Connection, id: &str, turn: &str, body: &str) {
    db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,?2,'incomplete_response','now',?3)", params![id,turn,body]).unwrap();
}

fn resume(db: &Connection, id: &str, second: &str, turn: Option<&str>) {
    let mut change = json!({"run_id":"r"});
    if let Some(turn) = turn {
        change["turn_id"] = json!(turn);
    }
    db.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES(?1,'context_changed','{}','t',?2,?3)", params![id,json!({"kind":"recovery_resumed","change":change}).to_string(),format!("2026-10-25T00:00:{second}Z")]).unwrap();
    db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'event','2099-01-01T00:00:00Z',?2)",params![id,json!({"event_id":id}).to_string()]).unwrap();
}

#[test]
fn fifty_eligible_events_share_one_statement_and_saved_observation_evidence() {
    let db = fixture();
    let requests = (0..50)
        .map(|index| {
            let id = format!("resume-{index}");
            resume(&db, &id, "03", Some("u"));
            (index * 2, id)
        })
        .collect::<Vec<_>>();
    let reference = read_tx_reference(&db, &requests).unwrap();
    let statements = Statements::new(&db);
    let batch = read_tx(&db, &requests).unwrap();
    assert_eq!(statements.count(), 1);
    assert_eq!(batch, reference);
    assert_eq!(batch.len(), 50);
    for (index, evidence) in batch.iter().enumerate() {
        assert_eq!(evidence.index, index * 2);
        assert_eq!(evidence.count, 3);
        assert_eq!(evidence.attempt, "tie-last");
        assert_eq!(evidence.source.as_deref(), Some("source-last"));
        assert!(
            evidence
                .error_json
                .as_deref()
                .unwrap()
                .contains("tied latest🦀")
        );
    }
}

#[test]
fn mixed_observations_match_reference_with_saved_turn_fallback_and_byte_caps() {
    let db = fixture();
    resume(&db, "explicit", "03", Some("u"));
    resume(&db, "fallback", "12", None);
    resume(&db, "large-observation", "05", None);
    resume(&db, "wrong-turn", "03", Some("new"));
    resume(&db, "not-resume", "03", Some("u"));
    db.execute(
        "UPDATE event SET payload_ref_json='{}' WHERE id='not-resume'",
        [],
    )
    .unwrap();
    resume(&db, "null-turn", "03", Some("u"));
    db.execute(
        "UPDATE event SET payload_ref_json=?1 WHERE id='null-turn'",
        [json!({"kind":"recovery_resumed","change":{"run_id":"r","turn_id":null}}).to_string()],
    )
    .unwrap();
    db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('missing-event','event','now','{}')",[]).unwrap();
    let requests = [
        (9, "fallback"),
        (3, "explicit"),
        (7, "large-observation"),
        (1, "explicit"),
        (4, "wrong-turn"),
        (5, "not-resume"),
        (6, "missing-event"),
        (8, "absent"),
        (11, "null-turn"),
    ]
    .map(|(index, id)| (index, id.into()));
    let reference = read_tx_reference(&db, &requests).unwrap();
    let statements = Statements::new(&db);
    let batch = read_tx(&db, &requests).unwrap();
    assert_eq!(statements.count(), 1);
    assert_eq!(batch, reference);
    assert_eq!(
        batch.iter().map(|row| row.index).collect::<Vec<_>>(),
        [9, 3, 7, 1, 11]
    );
    assert_eq!(batch[0].attempt, "new-first");
    assert_eq!(batch[0].count, 1);
    assert_eq!(batch[2].attempt, "large");
    assert_eq!(batch[2].count, 4);
    assert!(
        batch[2].error_json.is_none(),
        "limit is raw UTF8 bytes, not characters"
    );
    assert_eq!(
        batch[2].source.as_deref(),
        Some("large-source"),
        "omitting a large body must retain its exact saved source"
    );
}

#[test]
fn empty_and_oversized_requests_do_not_execute_sql() {
    let db = fixture();
    let statements = Statements::new(&db);
    assert!(read_tx(&db, &[]).unwrap().is_empty());
    let too_many = (0..129)
        .map(|index| (index, "absent".into()))
        .collect::<Vec<_>>();
    assert_eq!(
        read_tx(&db, &too_many).unwrap_err().into_api_error().code,
        ErrorCode::InputLimit
    );
    assert_eq!(statements.count(), 0);
}
