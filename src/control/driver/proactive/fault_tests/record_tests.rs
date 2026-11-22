//! #226: the automatic attempt record is final or `unknown` with the exact set
//! of committed stages, whatever fails after a stage commits.
use super::*;

const ORDER: [&str; 4] = [
    "closed_spill",
    "closed_summary",
    "active_spill",
    "active_summary",
];

macro_rules! record_test {
    ($name:ident, $fault:expr) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            record_boundaries($fault).await;
        }
    };
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn record_after_storage_fault() {
    record_boundaries(Fault::Storage).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_finish_rejection_and_postcommit_storage_share_the_active_summary_cell() {
    let (rejected, committed) =
        crate::test_watchdog::watchdog("pre-FINISH rollback versus postcommit adoption", async {
            let rejected = record_boundary(Fault::RejectFact, Scenario::ActiveSummary).await;
            let committed = record_boundary(Fault::Storage, Scenario::ActiveSummary).await;
            (rejected, committed)
        })
        .await
        .unwrap();

    assert_eq!(
        rejected["outcome"], "failed",
        "pre-FINISH rollback: {rejected}"
    );
    assert!(rejected["committed_stages"]["active_summary"].is_null());
    assert_eq!(
        committed["outcome"], "adopted",
        "postcommit FINISH remains durable: {committed}"
    );
    assert!(committed["committed_stages"]["active_summary"].is_object());
    assert_eq!(
        trigger_identity(&rejected["trigger"]),
        trigger_identity(&committed["trigger"]),
        "same scenario uses the exact immutable trigger tuple"
    );
    assert_ne!(
        rejected["trigger"]["run_id"], committed["trigger"]["run_id"],
        "each independent cell retains its own run provenance"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_target_percent_is_inherited_when_thread_is_unset() {
    direct_boundary_case(Fault::Storage, Scenario::ClosedSpill, None).await;
}
record_test!(record_after_cancellation, Fault::Cancellation);
record_test!(record_after_remeasurement_fault, Fault::Remeasurement);
// The two longest faults run each boundary as its own test with its own watchdog.
macro_rules! boundary_tests {
    ($fault:expr, $closed_spill:ident, $closed_summary:ident, $active_spill:ident, $active_summary:ident) => {
        boundary_test!($closed_spill, $fault, Scenario::ClosedSpill);
        boundary_test!($closed_summary, $fault, Scenario::ClosedSummary);
        boundary_test!($active_spill, $fault, Scenario::ActiveSpill);
        boundary_test!($active_summary, $fault, Scenario::ActiveSummary);
    };
}
macro_rules! boundary_test {
    ($name:ident, $fault:expr, $scenario:expr) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            crate::test_watchdog::watchdog(
                "automatic attempt record boundary",
                record_boundary($fault, $scenario),
            )
            .await
            .unwrap();
        }
    };
}
boundary_tests!(
    Fault::Persistent,
    record_after_persistent_fault_closed_spill,
    record_after_persistent_fault_closed_summary,
    record_after_persistent_fault_active_spill,
    record_after_persistent_fault_active_summary
);
record_test!(
    retired_automatic_writes_are_absent,
    Fault::RetiredWritesAbsent
);
record_test!(record_after_rejected_final_write, Fault::RejectFinal);
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_stage_fact_prevents_adoption() {
    record_boundaries(Fault::RejectFact).await;
}

async fn record_boundaries(fault: Fault) {
    crate::test_watchdog::watchdog("automatic attempt record", async {
        for scenario in Scenario::ALL {
            if matches!(fault, Fault::Storage | Fault::RejectFact)
                && scenario == Scenario::ActiveSummary
            {
                continue;
            }
            record_boundary(fault, scenario).await;
        }
    })
    .await
    .unwrap();
}

fn trigger_identity(trigger: &Value) -> Value {
    json!({
        "input_tokens": trigger["input_tokens"],
        "auto_at_tokens": trigger["auto_at_tokens"],
        "target_percent": trigger["target_percent"],
        "target_tokens": trigger["target_tokens"],
        "a_input_tokens": trigger["a_input_tokens"],
    })
}

async fn record_boundary(fault: Fault, scenario: Scenario) -> Value {
    let receipt = direct_boundary_case(fault, scenario, Some(scenario.target_percent())).await;
    assert_eq!(
        receipt.reopened_witness.selection_revision,
        receipt.after_fault_witness.selection_revision
            + receipt.reopen_audit_appends.as_array().unwrap().len() as i64,
        "selection revision tracks only the separately witnessed audit append"
    );
    assert_eq!(
        receipt.reopened_witness.reasoning_epoch,
        receipt.after_fault_witness.reasoning_epoch
    );
    assert_eq!(
        receipt.reopened_witness.cursor,
        receipt
            .reopened_witness
            .full_path
            .as_array()
            .unwrap()
            .last()
            .map(|row| (
                row[0].as_i64().unwrap(),
                row[1].as_str().unwrap().to_owned()
            )),
        "reopen cursor follows the actual full selected-path tail, including appended audit nodes"
    );
    assert_eq!(
        receipt.reopened_witness.selected_path_source_bytes,
        receipt.after_fault_witness.selected_path_source_bytes
    );
    assert!(receipt.reopened_witness.audit_path.is_array());
    assert!(receipt.reopen_audit_appends.as_array().is_some());
    assert!(receipt.checkpoint_witness.selection_revision > 0);
    assert!(receipt.checkpoint_witness.reasoning_epoch >= 0);
    assert!(receipt.checkpoint_witness.cursor.is_some());
    assert!(
        !receipt
            .checkpoint_witness
            .selected_path_source_bytes
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        receipt
            .checkpoint_witness
            .replacement_provenance_and_source_bytes
            .is_array()
    );
    assert!(receipt.checkpoint_witness.whole_tool_group_facts.is_array());
    assert!(
        receipt
            .checkpoint_witness
            .protected_request_receipts
            .is_array()
    );
    assert!(receipt.checkpoint_witness.audit_path.is_array());
    let checkpoint_phase = if fault == Fault::RejectFact {
        ObservationPhase::AfterRejectedStageTransaction
    } else {
        ObservationPhase::AtCommittedCheckpoint
    };
    assert_eq!(
        receipt.checkpoint["outcome"],
        expected_outcome(fault, scenario, checkpoint_phase),
        "direct checkpoint receipt preserves per-fault outcome"
    );
    assert_eq!(
        receipt.checkpoint["committed_stages"][scenario.stage()].is_object(),
        fault != Fault::RejectFact,
        "the selected stage fact exists only if its adoption transaction commits"
    );
    assert_eq!(
        receipt.terminal["outcome"],
        expected_outcome(fault, scenario, ObservationPhase::AfterShutdownAndRestart),
        "normal shutdown/reopen preserves the per-fault terminal state"
    );
    assert_eq!(
        receipt.terminal["unfinished"].as_bool().unwrap_or(false),
        receipt.terminal["outcome"] == "unknown",
        "unknown remains provisional; successful FINISH is terminal"
    );
    assert_eq!(
        trigger_identity(&receipt.checkpoint["trigger"]),
        trigger_identity(&receipt.terminal["trigger"]),
        "the captured input/A/target facts are immutable across FINISH/reopen"
    );
    let committed = receipt.terminal["committed_stages"]
        .as_object()
        .expect("stage facts");
    assert!(
        committed.keys().all(|name| ORDER.contains(&name.as_str())),
        "only known stage names are persisted"
    );
    if fault == Fault::RejectFact {
        assert!(receipt.terminal["committed_stages"][scenario.stage()].is_null());
    }
    if fault == Fault::RejectFinal && scenario == Scenario::ActiveSpill {
        assert!(receipt.terminal["committed_stages"]["active_spill"].is_object());
        assert!(
            receipt.terminal["committed_stages"]["active_summary"].is_null(),
            "rejected strict FINISH rolls back the follow-on summary transaction"
        );
    }
    if scenario == Scenario::ActiveSpill && fault == Fault::Remeasurement {
        assert!(receipt.terminal["committed_stages"]["active_summary"].is_object());
        let mut prior = receipt.terminal["committed_stages"].clone();
        prior.as_object_mut().unwrap().remove("active_summary");
        assert_eq!(
            prior, receipt.checkpoint["committed_stages"],
            "the continued stage adds only its own fact"
        );
    } else {
        assert_eq!(
            receipt.terminal["committed_stages"], receipt.checkpoint["committed_stages"],
            "FINISH/reopen preserves the exact committed-stage set"
        );
    }
    receipt.terminal
}

fn bounded_stage_fact(fact: &Value, name: &str) -> bool {
    let Some(object) = fact.as_object() else {
        return false;
    };
    let keys = object
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let gc_fact = fact.get("drop_all_reasoning").is_some();
    let expected = if gc_fact {
        [
            "stage",
            "selection_revision",
            "reasoning_epoch",
            "summary_node_id",
            "source_nodes",
            "spilled_groups",
            "drop_all_reasoning",
            "captured_reasoning_epoch",
            "source_characters",
            "summary_characters",
            "summary_limit",
            "model_requests",
            "summary_usage",
        ]
        .into_iter()
        .collect()
    } else {
        [
            "stage",
            "selection_revision",
            "reasoning_epoch",
            "summary_node_id",
            "source_nodes",
            "spilled_groups",
            "removed_characters",
            "source_characters",
            "summary_characters",
            "summary_limit",
            "model_requests",
            "summary_usage",
        ]
        .into_iter()
        .collect()
    };
    if keys != expected || fact["stage"] != name {
        return false;
    }
    if object.iter().any(|(key, value)| match key.as_str() {
        "stage" => !value.as_str().is_some_and(|s| s.len() <= 32),
        "summary_node_id" => {
            !value.is_null()
                && !value
                    .as_str()
                    .is_some_and(|id| id.len() <= 128 && uuid::Uuid::parse_str(id).is_ok())
        }
        "drop_all_reasoning" => !value.is_boolean(),
        "summary_usage" => !value.is_null() && !bounded_summary_usage(value),
        "source_nodes" | "spilled_groups" => value.as_u64().is_none(),
        "selection_revision" | "reasoning_epoch" | "captured_reasoning_epoch" => {
            value.as_i64().is_none()
        }
        _ => !value.is_null() && value.as_u64().is_none() && value.as_i64().is_none(),
    }) {
        return false;
    }
    fact.to_string().len() < 1024
}
fn bounded_summary_usage(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let keys = object
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if keys
        != [
            "requests",
            "input_tokens",
            "cached_tokens",
            "cache_creation_tokens",
            "output_tokens",
        ]
        .into_iter()
        .collect()
        || !value["requests"].is_u64()
    {
        return false;
    }
    [
        "input_tokens",
        "cached_tokens",
        "cache_creation_tokens",
        "output_tokens",
    ]
    .into_iter()
    .all(|name| {
        let n = &value[name];
        let Some(fields) = n.as_object() else {
            return false;
        };
        fields
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            == ["known_sum", "unknown_count", "overflow"]
                .into_iter()
                .collect()
            && (n["known_sum"].is_null() || n["known_sum"].is_u64())
            && n["unknown_count"].is_u64()
            && n["overflow"].is_boolean()
    })
}

#[test]
fn bounded_stage_schema_rejects_numeric_strings_content_and_untyped_usage() {
    let valid = json!({"stage":"active_summary","selection_revision":2,"reasoning_epoch":4,"summary_node_id":"550e8400-e29b-41d4-a716-446655440000","source_nodes":2,"spilled_groups":0,"drop_all_reasoning":true,"captured_reasoning_epoch":3,"source_characters":120,"summary_characters":24,"summary_limit":4000,"model_requests":1,"summary_usage":{"requests":1,"input_tokens":{"known_sum":4,"unknown_count":0,"overflow":false},"cached_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"cache_creation_tokens":{"known_sum":null,"unknown_count":1,"overflow":false},"output_tokens":{"known_sum":2,"unknown_count":0,"overflow":false}}});
    assert!(bounded_stage_fact(&valid, "active_summary"));
    let mut bad = valid.clone();
    bad["source_nodes"] = json!("2");
    assert!(!bounded_stage_fact(&bad, "active_summary"));
    let mut bad = valid.clone();
    bad["summary_node_id"] = json!("secret provider text");
    assert!(!bounded_stage_fact(&bad, "active_summary"));
    let mut bad = valid;
    bad["summary_usage"]["input_tokens"]["known_sum"] = json!("secret");
    assert!(!bounded_stage_fact(&bad, "active_summary"));
}

// The two uncalibrated call sites have no target plan, so the driver fixture
// above never reaches them. They are exercised through `summarize_once` itself
// on a real mid-Turn or next-Turn history that a held provider request leaves
// in place; no automatic attempt triggers because every reported usage stays
// below the threshold.
macro_rules! record_site_test {
    ($name:ident, $active:expr) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            crate::test_watchdog::watchdog("uncalibrated site", async {
                for fault in [Fault::Storage, Fault::Persistent] {
                    uncalibrated_site(fault, $active).await;
                }
            })
            .await
            .unwrap();
        }
    };
}

record_site_test!(
    uncalibrated_active_summary_is_recorded_and_checkpointed,
    true
);
record_site_test!(
    uncalibrated_late_prefix_summary_is_recorded_and_checkpointed,
    false
);

async fn uncalibrated_site(fault: Fault, active: bool) {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, _heads, server) = proxy().await;
    let limits =
        "[limits]\ndraft_bytes=2097152\n[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n";
    let owner = proactive_owner(root.path(), &url, limits).await;
    let control = Control::new(&owner, root.path().into());
    let target = thread(&control).await;
    // Use persisted, completed tool/source history with no live Driver task.
    // The old fixture held an in-flight Driver response while calling the
    // internal summary stage with a different local boundary Mutex; native GC
    // correctly waited for Control's real per-Thread boundary, deadlocking the
    // test itself. This seed uses production schema/path/tool rows and the same
    // real Control boundary below, without an invented driver snapshot.
    let target_for_seed = target.clone();
    let(active_run,request_turn,attempt)=control.store.database.call(move|db|{
        let tx=db.transaction()?;
        let(profile,model,effort):(String,String,Option<String>)=tx.query_row("SELECT profile_id,model_id,effort FROM thread WHERE id=?1",[&target_for_seed],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let run=uuid::Uuid::new_v4().to_string();
        let request_turn=uuid::Uuid::new_v4().to_string();
        let old_run=(!active).then(||uuid::Uuid::new_v4().to_string());
        let old_turn=(!active).then(||uuid::Uuid::new_v4().to_string());
        if !active{
            let earlier_run=uuid::Uuid::new_v4().to_string();let earlier_turn=uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed','fixture','fixture')",rusqlite::params![earlier_run,target_for_seed])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,generation,created_at,ended_at)VALUES(?1,?2,?3,'completed',0,'fixture','fixture')",rusqlite::params![earlier_turn,target_for_seed,earlier_run])?;
            for(kind,role,text)in[("user","user","earlier request ".repeat(20_000)),("assistant_response","assistant","earlier answer ".repeat(20_000))]{let node=uuid::Uuid::new_v4().to_string();crate::history::append_node(&tx,&target_for_seed,&node,Some(&earlier_turn),kind,&json!({"role":role,"content":[{"type":"input_text","text":text}]}),"fixture")?;}
        }
        if let (Some(old_run),Some(old_turn))=(&old_run,&old_turn){
            tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed','fixture','fixture')",rusqlite::params![old_run,target_for_seed])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,generation,created_at,ended_at)VALUES(?1,?2,?3,'completed',0,'fixture','fixture')",rusqlite::params![old_turn,target_for_seed,old_run])?;
            for(kind,role,text)in[("user","user","prior request ".repeat(20_000)),("assistant_response","assistant","prior answer ".repeat(20_000))]{let node=uuid::Uuid::new_v4().to_string();crate::history::append_node(&tx,&target_for_seed,&node,Some(old_turn),kind,&json!({"role":role,"content":[{"type":"input_text","text":text}]}),"fixture")?;}
        }
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,'live','fixture')",rusqlite::params![run,target_for_seed])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES(?1,?2,?3,'running',0,'fixture')",rusqlite::params![request_turn,target_for_seed,run])?;
        let mut attempt=uuid::Uuid::new_v4().to_string();
        let insert_attempt=|tx:&rusqlite::Transaction<'_>,attempt:&str,turn:&str|->Result<(),crate::storage::StorageError>{
            let revision:i64=tx.query_row("SELECT selection_revision FROM thread WHERE id=?1",[&target_for_seed],|r|r.get(0))?;
            tx.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,effort,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?2),?1,?2,0,?3,?4,?5,?6,0,'completed',json_object('adoption',json_object('outcome','adopted')),'fixture')",rusqlite::params![attempt,turn,revision,profile,model,effort])?;Ok(())
        };
        if active{
            let user=uuid::Uuid::new_v4().to_string();
            crate::history::append_node(&tx,&target_for_seed,&user,Some(&request_turn),"user",&json!({"role":"user","content":[{"type":"input_text","text":"current request"}]}),"fixture")?;
            insert_attempt(&tx,&attempt,&request_turn)?;
            for index in 1..=2{
                let group=format!("g{index}");let call=format!("c{index}");let call_node=uuid::Uuid::new_v4().to_string();let result_node=uuid::Uuid::new_v4().to_string();
                let group_attempt=if index==1{attempt.clone()}else{let next=uuid::Uuid::new_v4().to_string();insert_attempt(&tx,&next,&request_turn)?;attempt=next.clone();next};
                tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,?3,'completed','fixture')",rusqlite::params![group,group_attempt,request_turn])?;
                let item=json!({"type":"function_call","call_id":call,"name":"shell","arguments":"{}"});
                let call_payload=json!({"role":"assistant","group_id":group,"output":[item.clone()]});
                crate::history::append_node(&tx,&target_for_seed,&call_node,Some(&request_turn),"assistant_response",&call_payload,"fixture")?;
                tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_id,item_type,status,payload_json,node_id)VALUES(?1,0,?2,'function_call','completed',?3,?4)",rusqlite::params![group_attempt,call,item.to_string(),call_node])?;
                tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,?2,0,'shell','{}','completed')",rusqlite::params![group,call])?;
                let result_payload=json!({"role":"tool","group_id":group,"call_id":call,"content":"tool output ".repeat(10_000)});
                crate::history::append_node(&tx,&target_for_seed,&result_node,Some(&request_turn),"tool_result",&result_payload,"fixture")?;
                tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?2,?3,'completed')",rusqlite::params![group,call,result_node])?;
            }
        }else{
            insert_attempt(&tx,&attempt,&request_turn)?;
            let node=uuid::Uuid::new_v4().to_string();
            crate::history::append_node(&tx,&target_for_seed,&node,Some(&request_turn),"user",&json!({"role":"user","content":[{"type":"input_text","text":"current request"}]}),"fixture")?;
        }
        tx.commit()?;Ok((run,request_turn,attempt))
    }).await.unwrap();
    let thread_id = target.clone();
    let automatic = Automatic {
        attempt: attempt.clone(),
        token: "site-token".into(),
        thread: thread_id.clone(),
        turn: request_turn,
    };
    let begin = (attempt.clone(), automatic.token.clone());
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            crate::compaction::attempt_begin_tx(
                &tx,
                &begin.0,
                &begin.1,
                REASON,
                crate::compaction::ABoundaryObservation::Observe,
                &json!({}),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let stage = if active {
        "active_summary"
    } else {
        "closed_summary"
    };
    let reached = Arc::new(AtomicBool::new(false));
    *control.automatic_fault.lock().unwrap() = Some(crate::control::AutomaticFault {
        stage,
        kind: if fault == Fault::Persistent { 4 } else { 1 },
        reached: reached.clone(),
        checkpoint_reached: Arc::new(AtomicBool::new(false)),
        release_checkpoint: Arc::new({
            let notify = tokio::sync::Notify::new();
            // This standalone retry-stage test consumes its own post-commit
            // failure before the test reads the attempt record below.
            notify.notify_one();
            notify
        }),
    });
    let slot = DriverSlot {
        id: "site".into(),
        run_id: active_run,
        stop: tokio_util::sync::CancellationToken::new(),
        nudge: Arc::new(tokio::sync::Notify::new()),
        foreground: Arc::new(crate::control::foreground::ForegroundSlot::default()),
        turn: Arc::new(tokio::sync::Mutex::new(None)),
        driver_active: Arc::new(AtomicBool::new(true)),
        proactive_job: Arc::new(tokio::sync::Mutex::new(None)),
        proactive_joining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        proactive_join_done: Arc::new(tokio::sync::Notify::new()),
    };
    let mutex = control.boundary(&thread_id).await;
    let mut boundary = crate::compaction::boundary::Boundary::new(&mutex, mutex.lock().await);
    let result=async{
        let mode = if active {
            SummaryStageMode::Active {
                spill_after_no_target: false,
            }
        } else {
            SummaryStageMode::Closed
        };
        let work = async {
            summary_stage(
            SummaryStageOwner {
                control: &control,
                compactor: &control.compactor,
                thread: &thread_id,
                boundary: &mut boundary,
                automatic: &automatic,
                run_stop: slot.stop.clone(),
            },
            mode,
            &slot,
            ).await.summary
        };
        tokio::pin!(work);
        let request=tokio::select!{
            result=&mut work=>panic!("uncalibrated summary ended before provider admission: active={active} fault={fault:?} no_target={} err={:?}",result.as_ref().is_ok_and(|value|value.no_target),result.as_ref().err().map(|error|(error.code,error.message.clone()))),
            request=incoming(&control,&mut receive)=>request,
        };
        assert_eq!(request.body["tools"],json!([]));
        request.reply.send((200,answer("Short site summary."))).unwrap();
        work.await
    }.await;
    let error = result.unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::SaveFailed,
        "the checkpoint fault follows the commit: {error:?}"
    );
    assert!(reached.load(Ordering::SeqCst));
    drop(boundary);
    let facts: Value = control
        .store
        .database
        .call({
            let attempt = attempt.clone();
            move |db| {
                Ok(serde_json::from_str(&db.query_row(
                    &format!(
                        "SELECT {SAVED_RECORD} FROM automatic_compaction c WHERE attempt_id=?1"
                    ),
                    [attempt],
                    |r| r.get::<_, String>(0),
                )?)
                .unwrap())
            }
        })
        .await
        .unwrap();
    assert_eq!(
        facts["outcome"], "unknown",
        "only the GC stage committed; proactive FINISH has not run"
    );
    assert_eq!(facts["unfinished"], true);
    let recorded = facts["committed_stages"].as_object().unwrap();
    assert_eq!(
        recorded.keys().map(String::as_str).collect::<Vec<_>>(),
        [stage]
    );
    let selected: Vec<String> = control.store.database.call({
        let target = target.clone();
        move |db| Ok(db.prepare("SELECT n.id FROM thread_replacement tr JOIN replacement r ON r.id=tr.replacement_id JOIN node n ON n.id=r.output_node_id WHERE tr.thread_id=?1 AND n.kind='summary'")?.query_map([target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }).await.unwrap();
    assert_eq!(
        selected,
        [recorded[stage]["summary_node_id"]
            .as_str()
            .unwrap()
            .to_owned()]
    );
    assert!(recorded[stage]["source_nodes"].as_u64().unwrap() > 0);
    // Uncalibrated: there is no target plan, so no provisional target either.
    let pending: i64 = control
        .store
        .database
        .call({
            let target = target.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key='compact.automatic_target.'||?1",
                    [target],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(pending, 0);
    let _ = control.shutdown().await;
    owner.shutdown().await.unwrap();
    server.abort();
}
