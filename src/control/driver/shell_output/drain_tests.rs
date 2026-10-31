//! Real pipe/DB boundaries distinguish process success from missing output.
use super::*;
use crate::control::shell_lifecycle_tests::{answer, fixture_with_settings, incoming, start, tool};
use rusqlite::OptionalExtension;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn successful_incomplete_output_is_saved_and_cancels_unexecuted_calls() {
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
    let execution=crate::test_watchdog::watchdog("src/control/driver/shell_output/drain_tests.rs: successful_incomplete_output_is_saved_and_cancels_unexecuted_calls",async {
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
    let write = handle.write("GO\n", "UTF-8").await;
    let ended = crate::test_watchdog::watchdog("src/control/driver/shell_output/drain_tests.rs: successful_incomplete_output_is_saved_and_cancels_unexecuted_calls", async {
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    // Release the actor before assertions so a red result still drains safely.
    release.send(()).unwrap();
    blocking.await.unwrap();
    let write = write.unwrap();
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
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["status"], "exited");
    assert_eq!(results[0]["exit_code"], 0);
    assert_eq!(results[0]["output_complete"], false);
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
    assert_eq!((&*saved.0, saved.1, saved.2), ("exited", Some(0), false));
    assert!(saved.3.unwrap().contains("drain"));
}
