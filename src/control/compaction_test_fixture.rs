//! Canonical active-Turn database fixture shared by compaction and driver tests.
use crate::{history, storage::DbWorker};
use serde_json::json;

/// #181 fixture: a running Turn with three complete tool groups whose
/// responses carry large current-epoch reasoning items.
pub(crate) async fn active_turn_fixture(db: &DbWorker) {
    active_turn_fixture_with(db, false).await;
}
pub(crate) async fn active_turn_fixture_with(db: &DbWorker, messages: bool) {
    active_turn_fixture_seed(db, messages, false, false, "r".into()).await;
}

async fn active_turn_fixture_seed(
    db: &DbWorker,
    messages: bool,
    closed_prefix_groups: bool,
    large_active_output: bool,
    run_id: String,
) {
    db.call(move |db| {
        let tx=db.transaction()?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,'t','live','now')", [&run_id])?;
        let thinking = |name: &str| crate::model::opaque::wrap("anthropic_messages", "thinking", json!({"type":"thinking","thinking":"","signature":format!("sig-{name}")}));
        if closed_prefix_groups {
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('older','t',?1,'completed','now')", [&run_id])?;
        }
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('last','t',?1,'completed','now'),('current','t',?1,'running','now')", [&run_id])?;
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','current',0,0,'fixture','fixture',0,'completed','{}','now')", [])?;
        if closed_prefix_groups {
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at,ended_at)VALUES('older-attempt','older',0,0,'fixture','fixture',0,'completed','{}','older','now')", [])?;
            history::append_node(&tx,"t","older-user",Some("older"),"message",&json!({"role":"user","content":"older eligible request"}),"now")?;
            history::append_node(&tx,"t","older-answer",Some("older"),"assistant_response",&json!({"reasoning_epoch":0,"output":[{"type":"reasoning","id":"rs-older","summary":[],"encrypted_content":"OLD"},thinking("older"),{"type":"message","role":"assistant","content":[{"type":"output_text","text":"older complete"}]}]}),"now")?;
            for group in ["older-g1", "older-g2"] {
                append_completed_tool_group(&tx, "t", "older-attempt", "older", group, 4_000, &"x".repeat(40_000))?;
            }
        }
        history::append_node(&tx,"t","last-user",Some("last"),"message",&json!({"role":"user","content":"earlier"}),"now")?;
        history::append_node(&tx,"t","last-answer",Some("last"),"assistant_response",&json!({"reasoning_epoch":0,"output":[{"type":"reasoning","id":"rs-last","summary":[],"encrypted_content":"OLD"},thinking("last"),{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]}),"now")?;
        if closed_prefix_groups {
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at,ended_at) VALUES('closed-first','last',0,0,'fixture','fixture',0,'completed','{}','first','now')", [])?;
            for group in ["closed-g1", "closed-g2"] {
                append_completed_tool_group(&tx, "t", "closed-first", "last", group, 4_000, &"x".repeat(40_000))?;
            }
        }
        history::append_node(&tx,"t","current-user",Some("current"),"message",&json!({"role":"user","content":"long task"}),"now")?;
        for group in ["g1","g2","g3"] {
            let stdout = if large_active_output {
                "x".repeat(40_000)
            } else {
                format!("out-{group}")
            };
            append_completed_tool_group(&tx, "t", "attempt", "current", group, 4_000, &stdout)?;
            if messages && group != "g3" {
                let id = format!("user-after-{group}");
                history::append_node(&tx,"t",&id,Some("current"),"message",&json!({"role":"user","content":format!("instruction after {group}")}),"now")?;
            }
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
}
/// Which completed request owns the tool groups consumed by a stage fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StagePlacement {
    ActiveTurn,
    ClosedPrefix,
}

/// Stable synthetic identities and policy facts used by direct boundary tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StageFixture {
    pub(crate) thread_id: &'static str,
    pub(crate) run_id: String,
    pub(crate) turn_id: &'static str,
    pub(crate) attempt_id: &'static str,
    pub(crate) first_attempt_id: &'static str,
    pub(crate) input_tokens: u64,
    pub(crate) a_input_tokens: u64,
    pub(crate) target_percent: u64,
    pub(crate) spill_effective: bool,
}

/// Construct the stage's real turn placement, whole groups, and policy facts.
pub(crate) async fn stage_fixture(
    db: &DbWorker,
    placement: StagePlacement,
    target_percent: u64,
    spill_effective: bool,
) -> StageFixture {
    let run_id = format!("direct-run-{}", uuid::Uuid::new_v4());
    match placement {
        StagePlacement::ActiveTurn => {
            active_turn_fixture_seed(db, false, false, true, run_id.clone()).await
        }
        StagePlacement::ClosedPrefix => {
            active_turn_fixture_seed(db, false, true, true, run_id.clone()).await
        }
    }
    let fixture = match placement {
        StagePlacement::ActiveTurn => StageFixture {
            thread_id: "t",
            run_id: run_id.clone(),
            turn_id: "current",
            attempt_id: "trigger",
            first_attempt_id: "attempt",
            input_tokens: 200,
            a_input_tokens: 100,
            target_percent,
            spill_effective,
        },
        StagePlacement::ClosedPrefix => StageFixture {
            thread_id: "t",
            run_id: run_id.clone(),
            turn_id: "current",
            attempt_id: "trigger",
            first_attempt_id: "attempt",
            input_tokens: 200,
            a_input_tokens: 100,
            target_percent,
            spill_effective,
        },
    };
    let mut a_state = crate::compaction::a_state::State::new("fixture", "fixture").unwrap();
    let started = a_state.token();
    assert!(
        a_state
            .completed(
                started,
                "fixture",
                "fixture",
                fixture.first_attempt_id,
                Some(fixture.a_input_tokens),
            )
            .unwrap()
    );
    let first_usage = serde_json::json!({"input_tokens": fixture.a_input_tokens}).to_string();
    let trigger_usage = serde_json::json!({"input_tokens": fixture.input_tokens}).to_string();
    db.call(move |db| {
        let tx = db.transaction()?;
        tx.execute("UPDATE model_attempt SET usage_json=?1,ended_at='now' WHERE id='attempt'", [&first_usage])?;
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at) VALUES('trigger','current',0,0,'fixture','fixture',0,'completed','{}',?1,'later','now')", [&trigger_usage])?;
        crate::compaction::a_state::save(&tx, "t", &a_state)?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    fixture
}

fn append_completed_tool_group(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    attempt: &str,
    turn: &str,
    group: &str,
    reasoning_chars: usize,
    stdout: &str,
) -> Result<(), crate::storage::StorageError> {
    tx.execute(
        "INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES(?1,?2,?3,'completed','now')",
        rusqlite::params![group, attempt, turn],
    )?;
    tx.execute(
        "INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state) VALUES(?1,?1,0,'shell','{}','completed')",
        [group],
    )?;
    let (call, result) = (format!("{group}-call"), format!("{group}-result"));
    let thinking = crate::model::opaque::wrap(
        "anthropic_messages",
        "thinking",
        serde_json::json!({"type":"thinking","thinking":"","signature":format!("sig-{group}")}),
    );
    history::append_node(
        tx,
        thread,
        &call,
        Some(turn),
        "assistant_response",
        &serde_json::json!({
            "group_id":group,
            "reasoning_epoch":0,
            "output":[
                {"type":"reasoning","id":format!("rs-{group}"),"summary":[],"encrypted_content":"E".repeat(reasoning_chars)},
                thinking,
                {"type":"function_call","call_id":group,"name":"shell","arguments":serde_json::json!({"command":format!("echo {group}")}).to_string()}
            ]
        }),
        "now",
    )?;
    let output = serde_json::json!({"stdout":stdout}).to_string();
    history::append_node(
        tx,
        thread,
        &result,
        Some(turn),
        "tool_result",
        &serde_json::json!({
            "group_id":group,
            "item":{"type":"function_call_output","call_id":group,"output":output}
        }),
        "now",
    )?;
    tx.execute(
        "INSERT INTO tool_result(group_id,call_id,node_id,status) VALUES(?1,?1,?2,'completed')",
        rusqlite::params![group, result],
    )?;
    Ok(())
}
