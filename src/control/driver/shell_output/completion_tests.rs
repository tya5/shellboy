use super::*;
use crate::control::{
    output::ShellOutputReader,
    shell_lifecycle_tests::{answer, fixture, incoming, rpc, start, tool},
};
use crate::protocol::{Command, ExecutionTarget};
use rusqlite::OptionalExtension;
use tokio::time::{Duration, timeout};

fn call(command: &str) -> Value {
    let mut response = tool(command);
    // This test concerns process termination rather than racing an initial
    // stdin write against a deliberately immediate signal exit.
    response["output"][0]["arguments"] = json!({"command":command}).to_string().into();
    response
}
fn assert_facts(value: &Value, forced: bool, signal: Option<i32>, confirmed: bool) {
    assert_eq!(value["forced"], json!(forced));
    assert_eq!(value["exit_signal"], json!(signal));
    assert_eq!(value["group_cleanup_confirmed"], json!(confirmed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_signal_and_forced_completion_survive_model_event_get_and_restart() {
    for forced in [false, true] {
        let (root, owner, control, mut requests, server) = fixture().await;
        start(&control).await;
        let (_, reply) = incoming(&mut requests).await;
        reply
            .send(call(if forced {
                "trap '' TERM; printf READY; while :; do :; done"
            } else {
                "kill -TERM $$"
            }))
            .unwrap();
        if forced {
            // The saved READY bytes are an actual process/DB boundary: TERM
            // cannot arrive before its ignore handler has been installed.
            let execution=timeout(Duration::from_secs(3),async {
                loop {
                    let row=control.store.database.call(|db| {
                        Ok(db.query_row("SELECT s.id FROM shell_execution s WHERE s.state='running' AND EXISTS(SELECT 1 FROM output_chunk o WHERE o.execution_id=s.id)",[],|row|row.get::<_,String>(0)).optional()?)
                    }).await.unwrap();
                    if let Some(execution)=row {break execution;}
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            rpc(
                &control,
                Command::ShellStop(ExecutionTarget {
                    execution_id: execution,
                }),
            )
            .await;
        }
        let (request, reply) = incoming(&mut requests).await;
        let output = request["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap()["output"]
            .as_str()
            .unwrap();
        let result: Value = serde_json::from_str(output).unwrap();
        let execution = result["execution_id"].as_str().unwrap().to_owned();
        let signal = Some(if forced { libc::SIGKILL } else { libc::SIGTERM });
        assert_facts(&result, forced, signal, true);
        let details = rpc(
            &control,
            Command::ShellGet(ExecutionTarget {
                execution_id: execution.clone(),
            }),
        )
        .await;
        assert_facts(&details, forced, signal, true);
        let target = execution.clone();
        let (facts,event)=control.store.database.call(move|db| {
            let saved:String=db.query_row("SELECT json_extract(shell_json,'$.completion') FROM shell_execution WHERE id=?1",[&target],|row|row.get(0))?;
            let event:String=db.query_row("SELECT payload_ref_json FROM event WHERE id=?1",[format!("shell-ended-{target}")],|row|row.get(0))?;
            Ok((saved,event))
        }).await.unwrap();
        assert_facts(
            &serde_json::from_str::<Value>(&facts).unwrap(),
            forced,
            signal,
            true,
        );
        assert_facts(
            &serde_json::from_str::<Value>(&event).unwrap(),
            forced,
            signal,
            true,
        );
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
        let details = serde_json::to_value(reader.get(&execution).await.unwrap()).unwrap();
        assert_facts(&details, forced, signal, true);
        reopened.shutdown().await.unwrap();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_legacy_facts_stay_unknown_and_mock_unconfirmed_cleanup_stays_false() {
    let (_root, owner, control, mut requests, server) = fixture().await;
    let thread = start(&control).await;
    let (_, _held_reply) = incoming(&mut requests).await;
    let target = thread.clone();
    let run = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
                [target],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    let super::super::Next::Ready(snapshot) =
        super::super::next(&control, &thread, &run).await.unwrap()
    else {
        panic!("fixture has a real admitted Turn");
    };
    let run = snapshot.run.clone();
    let turn = snapshot.turn.clone();
    // This row is a DB/mock fixture, not a claim that an OS group was spawned.
    // Non-typed legacy extensions cannot fabricate known bool/signal facts.
    control.store.database.call(move|db| {
        db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('mock-unconfirmed',?1,?2,'mock-operation','running','/','{\"command\":\"mock\",\"legacy_note\":\"keep\",\"completion\":{\"forced\":\"unknown\",\"exit_signal\":4294967295,\"group_cleanup_confirmed\":0,\"elapsed_ms\":-1}}','UTF-8','UTF-8','closed','now')",params![run,turn])?;
        Ok(())
    }).await.unwrap();
    let reader = ShellOutputReader::new(
        control.store.database.clone(),
        control.store.instance_id.clone(),
    );
    let unknown = serde_json::to_value(reader.get("mock-unconfirmed").await.unwrap()).unwrap();
    for field in [
        "forced",
        "exit_signal",
        "group_cleanup_confirmed",
        "elapsed_ms",
    ] {
        assert!(
            !unknown.as_object().unwrap().contains_key(field),
            "unknown legacy fact must remain absent: {field}"
        );
    }
    let completion = ShellCompletion {
        execution_id: "mock-unconfirmed".into(),
        state: ShellState::Exited,
        exit_code: Some(0),
        exit_signal: None,
        reason: Some("output drain incomplete".into()),
        forced: true,
        output_complete: false,
        group_cleanup_confirmed: false,
        elapsed_ms: None,
    };
    save_completion(&control, &snapshot, "mock-unconfirmed", &completion)
        .await
        .unwrap();
    let observed = serde_json::to_value(reader.get("mock-unconfirmed").await.unwrap()).unwrap();
    assert_facts(&observed, true, None, false);
    let (body, event) = control
        .store
        .database
        .call(|db| {
            Ok((
                db.query_row(
                    "SELECT shell_json FROM shell_execution WHERE id='mock-unconfirmed'",
                    [],
                    |row| row.get::<_, String>(0),
                )?,
                db.query_row(
                    "SELECT payload_ref_json FROM event WHERE id='shell-ended-mock-unconfirmed'",
                    [],
                    |row| row.get::<_, String>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["legacy_note"], "keep");
    assert_facts(&body["completion"], true, None, false);
    assert_facts(
        &serde_json::from_str::<Value>(&event).unwrap(),
        true,
        None,
        false,
    );
    drop(snapshot);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn repeated_or_late_completion_keeps_original_facts_and_one_event() {
    use super::super::preparation_tests::{close, fixture, ready};
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let run = snapshot.run.clone();
    let turn = snapshot.turn.clone();
    control.store.database.call(move|db| {
        db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('execution',?1,?2,'execution','running','/','{}','UTF-8','UTF-8','closed','started')",params![run,turn])?;
        Ok(())
    }).await.unwrap();
    let mut result = ShellCompletion {
        execution_id: "execution".into(),
        state: ShellState::Exited,
        exit_code: Some(0),
        exit_signal: None,
        reason: None,
        forced: false,
        output_complete: true,
        group_cleanup_confirmed: true,
        elapsed_ms: Some(42),
    };
    save_completion(&control, &snapshot, "execution", &result)
        .await
        .unwrap();
    let original=control.store.database.call(|db| {
        Ok(db.query_row("SELECT json_object('state',state,'exit',exit_code,'reason',end_reason,'complete',output_complete,'ended',ended_at,'shell',json(shell_json)) FROM shell_execution",[],|r|r.get::<_,String>(0))?)
    }).await.unwrap();
    result.state = ShellState::Failed;
    result.exit_code = Some(99);
    result.output_complete = false;
    result.reason = Some("late result".into());
    save_completion(&control, &snapshot, "execution", &result)
        .await
        .unwrap();
    control.store.database.call(move|db| {
        let current=db.query_row("SELECT json_object('state',state,'exit',exit_code,'reason',end_reason,'complete',output_complete,'ended',ended_at,'shell',json(shell_json)) FROM shell_execution",[],|r|r.get::<_,String>(0))?;
        assert_eq!(current,original);
        assert_eq!(db.query_row("SELECT count(*) FROM event WHERE id='shell-ended-execution'",[],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT count(*) FROM delivery WHERE event_id='shell-ended-execution'",[],|r|r.get::<_,i64>(0))?,1);
        Ok(())
    }).await.unwrap();
    drop(snapshot);
    close(control, owner).await;
}
