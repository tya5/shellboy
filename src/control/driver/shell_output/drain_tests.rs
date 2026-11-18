//! Real pipe/DB boundaries distinguish process success from missing output.
use super::*;
use crate::control::shell_lifecycle_tests::{answer, fixture_with_settings, incoming, start, tool};
use rusqlite::OptionalExtension;

fn is_recorded_initial_stdin_failure(part: &str) -> bool {
    // InitialStdin is recorded only for non-Complete writes while stop is not cancelled.
    ["Timeout", "Closed", "Failed"].iter().any(|status| {
        let prefix = format!("initial_stdin_{status}: ");
        part.strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(" bytes delivered"))
            .is_some_and(|count| !count.is_empty() && count.parse::<u64>().is_ok())
    })
}

fn expected_state_for_incomplete_output(reason: &str) -> &'static str {
    const DRAIN: &str = "output drain incomplete";
    const RAW_LIMIT: &str = "raw output limit exceeded; execution stopped and output truncated";
    assert!(!reason.is_empty(), "incomplete output needs a reason");
    assert!(
        reason.matches(DRAIN).count() <= 1,
        "Drain must be deduplicated: {reason:?}"
    );
    assert!(
        reason.matches(RAW_LIMIT).count() <= 1,
        "raw-limit reason must be deduplicated: {reason:?}"
    );
    let normalized = reason.replace(RAW_LIMIT, "<raw-limit>");
    let components = normalized.split("; ").collect::<Vec<_>>();
    assert!(
        components.iter().all(|part| !part.is_empty()),
        "empty reason component: {reason:?}"
    );
    assert_eq!(
        components.iter().filter(|part| **part == DRAIN).count(),
        1,
        "this fixture must witness exactly one Drain reason: {reason:?}"
    );
    let mut non_drain = false;
    for part in components {
        if part == DRAIN {
            continue;
        }
        let recognized = part == "<raw-limit>"
            || part == "Unix shell reap was not confirmed"
            || part == "Unix guardian reap was not confirmed"
            || part == "output receiver disconnected"
            || part == "shell process exit after Job termination is unconfirmed"
            || part.starts_with("shell wait failed: ")
            || part.starts_with("output read failed: ")
            || is_recorded_initial_stdin_failure(part)
            || part.starts_with("terminate execution Job: ")
            || part.starts_with("query owned Job processes: ")
            || part.starts_with("read shell exit code: ")
            || part.starts_with("observe shell process handle: ");
        assert!(recognized, "unclassified shell failure reason: {part:?}");
        non_drain = true;
    }
    if non_drain { "failed" } else { "exited" }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incomplete_output_preserves_native_facts_and_cancels_unexecuted_calls() {
    let (root, owner, control, mut requests, server) =
        fixture_with_settings("output_buffer_bytes=16\noutput_batch_bytes=16\noutput_batch_ms=1\n")
            .await;
    let marker = root.path().join("must-not-run");
    start(&control).await;
    let (_, reply) = incoming(&mut requests).await;
    let mut response = tool("unused");
    response["output"][0]["arguments"]=json!({"command":"printf START; read line; printf 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","stdin_open":true}).to_string().into();
    let mut second = response["output"][0].clone();
    second["call_id"] = "must-not-run".into();
    second["arguments"] = json!({"command":format!("printf unexpected > '{}'",marker.display())})
        .to_string()
        .into();
    response["output"].as_array_mut().unwrap().push(second);
    reply.send(response).unwrap();
    let execution=crate::test_watchdog::watchdog("src/control/driver/shell_output/drain_tests.rs: incomplete_output_preserves_native_facts_and_cancels_unexecuted_calls",async {
        loop {
            let row=control.store.database.call(|db|Ok(db.query_row("SELECT s.id FROM shell_execution s WHERE s.state='running' AND EXISTS(SELECT 1 FROM output_chunk o WHERE o.execution_id=s.id AND o.bytes=CAST('START' AS BLOB))",[],|r|r.get::<_,String>(0)).optional()?)).await.unwrap();
            if let Some(execution)=row {break execution;}
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let handle = control
        .live
        .test_shells()
        .lock()
        .await
        .get(&execution)
        .unwrap()
        .handle
        .clone();
    let database = control.store.database.clone();
    let (held, db_held) = tokio::sync::oneshot::channel();
    let (release, await_release) = std::sync::mpsc::channel();
    let blocking = tokio::spawn(async move {
        database
            .call(move |_| {
                let _ = held.send(());
                await_release.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    db_held.await.unwrap();
    // Writes now acknowledge durable facts. Keep the database blocked until the
    // native exit, without blocking the test on the persistence acknowledgement.
    let writer = handle.clone();
    let write = tokio::spawn(async move { writer.write("GO\n", "UTF-8").await });
    let ended = crate::test_watchdog::watchdog("src/control/driver/shell_output/drain_tests.rs: incomplete_output_preserves_native_facts_and_cancels_unexecuted_calls", async {
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    // Release the actor before assertions so a red result still drains safely.
    release.send(()).unwrap();
    blocking.await.unwrap();
    let write = write.await.unwrap().unwrap();
    assert_eq!(write.status, crate::shell::StdinWriteStatus::Complete);
    assert_eq!(write.bytes_written, 3);
    ended.unwrap();
    let (request, reply) = incoming(&mut requests).await;
    let results: Vec<Value> = request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| serde_json::from_str(item["output"].as_str().unwrap()).unwrap())
        .collect();
    let saved=control.store.database.call(move|db|Ok(db.query_row("SELECT state,exit_code,output_complete,end_reason FROM shell_execution WHERE id=?1",[execution],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<i32>>(1)?,r.get::<_,bool>(2)?,r.get::<_,Option<String>>(3)?)))?)).await.unwrap();
    reply.send(answer()).unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
    eprintln!(
        "incomplete-output tool result={:#?}; native shell row={saved:#?}",
        results[0]
    );
    assert_eq!(results.len(), 2);
    // The real pipe/DB control checks native facts and reason-conditioned state.
    // Drain-only Exited mapping is covered at ShellFailures, not by a combined
    // native Exited-only end-to-end schedule.
    assert_eq!(
        saved.1,
        Some(0),
        "known native exit code: {results:#?}; DB row: {saved:#?}"
    );
    assert_eq!(results[0]["exit_code"], json!(saved.1));
    assert_eq!(results[0]["output_complete"], saved.2);
    assert!(!saved.2, "the bounded pipe lost output: {saved:#?}");
    assert_eq!(results[0]["reason"].as_str(), saved.3.as_deref());
    let reason = saved
        .3
        .as_deref()
        .expect("incomplete output has a saved reason");
    let expected_state = expected_state_for_incomplete_output(reason);
    assert_eq!(
        saved.0, expected_state,
        "reason-conditioned ShellFailures state; tool={:#?}; DB={saved:#?}",
        results[0]
    );
    assert_eq!(
        results[0]["status"], saved.0,
        "model result retains the exact native completion class: {saved:#?}"
    );
    assert_eq!(results[1]["status"], "cancelled");
    assert!(
        results[1]["reason"]
            .as_str()
            .unwrap()
            .contains("incomplete")
    );
    assert!(
        !marker.exists(),
        "missing observation must not allow the next side effect"
    );
    assert!(!saved.2);
}
