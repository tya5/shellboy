//! Control-owned stage placement and receipt facts over the shared Compactor seed.
use crate::storage::DbWorker;

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
            crate::compaction::test_fixture::active_turn_fixture_seed(
                db,
                false,
                false,
                true,
                run_id.clone(),
            )
            .await
        }
        StagePlacement::ClosedPrefix => {
            crate::compaction::test_fixture::active_turn_fixture_seed(
                db,
                false,
                true,
                true,
                run_id.clone(),
            )
            .await
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
