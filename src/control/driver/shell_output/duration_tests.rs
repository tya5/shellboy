//! A live process boundary separates monotonic duration from saved wall time.
use super::*;
use crate::{
    control::{
        output::ShellOutputReader,
        shell_lifecycle_tests::{answer, fixture, incoming, rpc, start, tool},
    },
    protocol::{Command, ExecutionTarget, ShellWriteArgs},
};
use rusqlite::OptionalExtension;
use tokio::time::Instant;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn monotonic_duration_ignores_wall_clock_changes_and_survives_restart() {
    let (root, owner, control, mut requests, server) = fixture().await;
    let before = Instant::now();
    start(&control).await;
    let (_, reply) = incoming(&mut requests).await;
    let mut response = tool("unused");
    response["output"][0]["arguments"] =
        json!({"command":"printf START; read line; exit 0","stdin_open":true})
            .to_string()
            .into();
    reply.send(response).unwrap();
    let execution = crate::test_watchdog::watchdog("src/control/driver/shell_output/duration_tests.rs: monotonic_duration_ignores_wall_clock_changes_and_survives_restart", async {
        loop {
            let row=control.store.database.call(|db|Ok(db.query_row("SELECT s.id FROM shell_execution s WHERE s.state='running' AND EXISTS(SELECT 1 FROM output_chunk o WHERE o.execution_id=s.id AND o.bytes=CAST('START' AS BLOB))",[],|r|r.get::<_,String>(0)).optional()?)).await.unwrap();
            if let Some(execution) = row { break execution; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let target = execution.clone();
    control
        .store
        .database
        .call(move |db| {
            // Simulated wall-clock discontinuity, not a change to the host clock.
            db.execute(
                "UPDATE shell_execution SET started_at='2000-01-01T00:00:00Z' WHERE id=?1",
                [target],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    rpc(
        &control,
        Command::ShellWrite(ShellWriteArgs {
            execution_id: execution.clone(),
            text: "GO\n".into(),
            encoding: "UTF-8".into(),
        }),
    )
    .await;
    let (request, reply) = incoming(&mut requests).await;
    let result: Value = serde_json::from_str(
        request["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap()["output"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let bound = before.elapsed().as_millis() as u64;
    let details = rpc(
        &control,
        Command::ShellGet(ExecutionTarget {
            execution_id: execution.clone(),
        }),
    )
    .await;
    let target = execution.clone();
    let (facts, event) = control.store.database.call(move |db| {
        Ok((db.query_row("SELECT json_extract(shell_json,'$.completion') FROM shell_execution WHERE id=?1", [&target], |r|r.get::<_,String>(0))?, db.query_row("SELECT payload_ref_json FROM event WHERE id=?1", [format!("shell-ended-{target}")], |r|r.get::<_,String>(0))?))
    }).await.unwrap();
    reply.send(answer()).unwrap();
    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
    let reopened = crate::app::DataOwner::start(
        Some(root.path().join("config.toml")),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let reader = ShellOutputReader::new(reopened.db.clone(), reopened.instance_id.clone());
    let replay = serde_json::to_value(reader.get(&execution).await.unwrap()).unwrap();
    reopened.shutdown().await.unwrap();
    server.abort();
    let elapsed = result["elapsed_ms"].as_u64().unwrap();
    assert!(
        elapsed <= bound,
        "elapsed={elapsed} exceeds monotonic outer bound={bound}"
    );
    assert_eq!(result["started_at"], "2000-01-01T00:00:00Z");
    assert!(result["ended_at"].is_string());
    for value in [
        details,
        serde_json::from_str(&facts).unwrap(),
        serde_json::from_str(&event).unwrap(),
        replay,
    ] {
        assert_eq!(value["elapsed_ms"], elapsed);
    }
}
