//! One exact-Turn human interruption, with Run ownership left untouched.
use super::*;
use rusqlite::{OptionalExtension, params};

#[derive(Clone)]
pub(super) struct TurnBinding {
    pub(super) turn: String,
    pub(super) cancel: CancellationToken,
    pub(super) start_gate: Arc<Mutex<bool>>,
}
impl DriverSlot {
    pub(super) async fn bind_turn(
        &self,
        control: &Control,
        turn: &str,
    ) -> Result<TurnBinding, ApiError> {
        let binding = {
            let mut current = self.turn.lock().await;
            if let Some(binding) = current.as_ref().filter(|binding| binding.turn == turn) {
                binding.clone()
            } else {
                let binding = TurnBinding {
                    turn: turn.into(),
                    cancel: self.stop.child_token(),
                    start_gate: Arc::new(Mutex::new(false)),
                };
                *current = Some(binding.clone());
                binding
            }
        };
        let target = turn.to_owned();
        let requested = control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT interrupt_requested_at IS NOT NULL FROM turn WHERE id=?1",
                    [target],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .await
            .map_err(storage_error)?;
        if requested {
            *binding.start_gate.lock().await = true;
            binding.cancel.cancel();
        }
        Ok(binding)
    }
}
impl Control {
    /// Shared exact-Turn admission for TUI and runtime callers.
    pub async fn interrupt_turn(self: &Arc<Self>, turn: String) -> Result<Value, ApiError> {
        let control = self.clone();
        tokio::spawn(async move { control.interrupt_turn_inner(turn).await })
            .await
            .map_err(|_| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "Turn interruption worker failed; inspect the saved Turn",
                )
            })?
    }
    async fn interrupt_turn_inner(self: &Arc<Self>, turn: String) -> Result<Value, ApiError> {
        let _admission = self.admit(true)?;
        let id = turn.clone();
        let thread = self
            .store
            .database
            .call(move |db| {
                Ok(db
                    .query_row("SELECT thread_id FROM turn WHERE id=?1", [id], |r| {
                        r.get::<_, String>(0)
                    })
                    .optional()?)
            })
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                ApiError::new(ErrorCode::TargetUnavailable, "Turn was not found")
                    .details(json!({"reason":"turn_not_found","turn_id":turn}))
            })?;
        let outcome = self.live.admit_turn_interrupt(self, &thread, &turn).await;
        if let Err(error) = &outcome
            && error.is_storage_failure()
        {
            self.fail_storage().await;
        }
        let result = outcome?;
        if result["requested"] == true || result["already_requested"] == true {
            self.changed(&thread);
            self.kick_quiet(thread).await;
        }
        if let Some(reason @ ("already_finished" | "run_stopping")) = result["state"].as_str() {
            return Err(
                ApiError::new(ErrorCode::InvalidState, "Turn is no longer interruptible")
                    .details(json!({"reason":reason,"turn_id":turn})),
            );
        }
        Ok(result)
    }
}

/// Caller owns the existing driver-publication lock through this whole admission.
pub(super) async fn admit_locked(
    control: &Control,
    thread: &str,
    turn: &str,
    slot: Option<&DriverSlot>,
) -> Result<Value, ApiError> {
    let binding = reserve_binding(control, thread, turn, slot).await?;
    let mut gate = match &binding {
        Some(binding) => Some(binding.start_gate.lock().await),
        None => None,
    };
    let saved_thread = thread.to_owned();
    let saved_turn = turn.to_owned();
    let outcome = control.store.database.call(move |db| {
            let tx=db.transaction()?;
            let row:Option<(String,String,String,Option<String>,bool)>=tx.query_row(
                "SELECT u.run_id,u.state,r.state,u.interrupt_requested_at,t.deleted_at IS NOT NULL FROM turn u JOIN run r ON r.id=u.run_id JOIN thread t ON t.id=u.thread_id WHERE u.id=?1 AND u.thread_id=?2",
                params![saved_turn,saved_thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let Some((run,state,run_state,at,deleted))=row else{return Err(ApiError::new(ErrorCode::TargetUnavailable,"Turn does not belong to the requested Thread").into());};
            if deleted{return Err(ApiError::new(ErrorCode::ThreadNotFound,"Thread was deleted").into());}
            let active=matches!(state.as_str(),"preparing"|"running"|"waiting"|"recovery_wait");
            let refused = |state: &str| json!({"requested":false,"already_requested":false,"state":state});
            if run_state!="live" {return Ok(refused(if run_state=="stopping"{"run_stopping"}else{"already_finished"}));}
            if !active{return Ok(refused("already_finished"));}
            let (at, requested) = if let Some(at)=at { (at, false) } else {
                let at=state::now();
                tx.execute("UPDATE turn SET interrupt_requested_at=?2,generation=generation+1 WHERE id=?1 AND interrupt_requested_at IS NULL",params![saved_turn,at])?;
                tx.execute("UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",params![saved_thread,at])?;
                tx.commit()?;
                (at, true)
            };
            Ok(json!(crate::protocol::TurnInterruptResult {
                thread_id:saved_thread, run_id:run, turn_id:saved_turn,
                requested, already_requested:!requested,
                state:crate::protocol::TurnInterruptState::InterruptRequested,
                interrupt_requested_at:at,
            }))
        }).await.map_err(storage_error);
    if let Ok(result) = &outcome
        && (result["requested"] == true || result["already_requested"] == true)
    {
        if let Some(gate) = &mut gate {
            **gate = true;
        }
        if let Some(binding) = &binding {
            binding.cancel.cancel();
        }
    }
    drop(gate);
    outcome
}

async fn reserve_binding(
    control: &Control,
    thread: &str,
    turn: &str,
    slot: Option<&DriverSlot>,
) -> Result<Option<TurnBinding>, ApiError> {
    let Some(slot) = slot else {
        return Ok(None);
    };
    let mut current = slot.turn.lock().await;
    let saved_thread = thread.to_owned();
    let saved_turn = turn.to_owned();
    let run = slot.run_id.clone();
    let eligible=control.store.database.call(move|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id JOIN thread t ON t.id=u.thread_id WHERE u.id=?1 AND u.thread_id=?2 AND u.run_id=?3 AND u.state IN ('preparing','running','waiting','recovery_wait') AND r.state='live' AND t.deleted_at IS NULL)",params![saved_turn,saved_thread,run],|r|r.get::<_,bool>(0))?)).await.map_err(storage_error)?;
    if !eligible {
        return Ok(None);
    }
    if let Some(binding) = current.as_ref().filter(|binding| binding.turn == turn) {
        return Ok(Some(binding.clone()));
    }
    let binding = TurnBinding {
        turn: turn.into(),
        cancel: slot.stop.child_token(),
        start_gate: Arc::new(Mutex::new(false)),
    };
    *current = Some(binding.clone());
    Ok(Some(binding))
}

/// The driver calls this before next(), after any adopted group has all its results.
pub(super) async fn acknowledge(
    control: &Arc<Control>,
    thread: &str,
    run: &str,
) -> Result<(), ApiError> {
    let target_thread = thread.to_owned();
    let thread = thread.to_owned();
    let run = run.to_owned();
    let wait_wakes = control.waits.keyed.clone();
    let changed=control.store.database.call(move|db|{
        let tx=db.transaction()?;
        let interrupted:Option<(String,String)>=tx.query_row("SELECT u.id,u.interrupt_requested_at FROM turn u JOIN run r ON r.id=u.run_id WHERE u.thread_id=?1 AND u.run_id=?2 AND r.state='live' AND u.state IN ('preparing','running','waiting','recovery_wait') AND u.interrupt_requested_at IS NOT NULL",params![thread,run],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((turn,requested_at))=interrupted else{return Ok(false);};
        let group_running:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_group WHERE turn_id=?1 AND state<>'completed')",[&turn],|r|r.get(0))?;
        if group_running{return Ok(false);}
        let at=state::now();
        // Settle only notifications that were already in this Turn's input.
        let admitted:Vec<String>=tx.prepare("SELECT event_id FROM delivery WHERE recipient_id=?1 AND state='pending' AND (included_attempt_id IN (SELECT id FROM model_attempt WHERE turn_id=?2) OR event_id IN (SELECT json_extract(payload_json,'$.event_id') FROM node WHERE turn_id=?2 AND kind='event'))")?.query_map(params![run,turn],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for event in &admitted { message_delivery::settle_notification_tx(&tx,event,&run,None)?; }
        tx.execute("UPDATE turn SET state='stopped',ended_at=?2 WHERE id=?1",params![turn,at])?;
        message_delivery::repair_terminal_tx(&tx,&at)?;
        let carried_count:i64=tx.query_row("SELECT count(*) FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted'",params![turn,thread],|r|r.get(0))?;
        let carried_sources=tx.prepare("SELECT id,message_node_id,run_id,turn_id FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted' ORDER BY created_at,rowid LIMIT 128")?.query_map(params![turn,thread],|r|Ok(json!({"receipt_id":r.get::<_,String>(0)?,"message_id":r.get::<_,String>(1)?,"original_run_id":r.get::<_,Option<String>>(2)?,"original_turn_id":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let last_carried:Option<String>=tx.query_row("SELECT message_node_id FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted' ORDER BY created_at DESC,rowid DESC LIMIT 1",params![turn,thread],|r|r.get(0)).optional()?;
        let new_turn=if carried_count>0{Some(state::create_turn_tx(&tx,&thread,&run,&at,&at)?)}else{None};
        let adopted_group:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_group WHERE turn_id=?1)",[&turn],|r|r.get(0))?;
        let interrupted_attempt: Option<String> = tx.query_row(
            "SELECT id FROM model_attempt WHERE turn_id=?1 AND kind='conversation' AND (state='cancelled' AND ended_at>=?2 OR json_extract(input_manifest_json,'$.adoption.outcome')='not_adopted') AND generation=(SELECT generation-1 FROM turn WHERE id=?1) ORDER BY started_at DESC,rowid DESC LIMIT 1",
            params![turn,requested_at], |r|r.get(0),
        ).optional()?;
        let phase = if interrupted_attempt.is_some() {"model_attempt_interrupted"} else {"turn_interrupted"};
        let meaning = "A human interrupted this Turn. Any response not adopted before interruption is retained audit-only and is absent from model history. Saved tool-call results are authoritative; cancelled calls did not execute. Inspect continuing execution IDs before repeating work.";
        let event=format!("turn-interrupted-{turn}");
        let mut payload=json!({"kind":"turn_interrupted","actor":{"kind":"human"},"change":{"turn_id":turn,"run_id":run,"requested_at":requested_at,"phase":phase,"meaning":meaning,"had_adopted_groups":adopted_group,"interrupted_attempt_id":interrupted_attempt,"new_turn_id":new_turn,"carried_count":carried_count,"carried_sources":carried_sources,"carried_sources_truncated":carried_count>128,"first_carried_message_id":carried_sources.first().and_then(|s|s.get("message_id")),"last_carried_message_id":last_carried},"observed_at":at});
        tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES(?1,'context_changed','{\"kind\":\"human\"}',?2,?3,?4)",params![event,thread,payload.to_string(),at])?;
        let envelope=json!({"event_id":event,"kind":"context_changed","payload":payload});
        crate::history::append_node(&tx,&thread,&format!("event-{event}"),Some(&turn),"event",&json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {envelope}")}],"event_id":event}),&at)?;
        tx.execute("INSERT INTO delivery(event_id,recipient_id,state)VALUES(?1,?2,'processed')",params![event,run])?;
        let mut admitted_count=0_i64;
        let mut admitted_ids=Vec::new();
        let mut deferred_ids=Vec::new();
        if let Some(new_turn)=&new_turn {
            loop {
                let rows=tx.prepare("SELECT id,message_node_id FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted' ORDER BY created_at,rowid LIMIT 128")?.query_map(params![turn,thread],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                if rows.is_empty(){break;}
                let nodes=rows.iter().map(|r|r.1.clone()).collect::<Vec<_>>();
                let revision=crate::history::selection_revision(&tx,&thread,crate::history::ThreadScope::Live)?;
                let plan=message_delivery::append_admitted_tx(&tx,&thread,revision as u64,&nodes,&at)?;
                let mut admitted_in_batch=0;
                for (receipt,node) in &rows {
                    if plan.admitted_callers.contains(node) {
                        message_delivery::mark_interruption_carry_tx(&tx,(receipt,node),(&thread,&run,&turn,new_turn,&at))?;
                        admitted_count+=1;
                        admitted_in_batch+=1;
                        if admitted_ids.len()<128 {admitted_ids.push(node.clone());}
                    } else if deferred_ids.len()<128 {
                        deferred_ids.push(node.clone());
                    }
                }
                // A deferred FIFO suffix cannot make progress in this transaction. Leave it
                // held with its original Run/Turn so the existing resume path can recover it.
                if admitted_in_batch<rows.len(){break;}
            }
        }
        if admitted_count>carried_count{return Err(crate::storage::StorageError::Schema("carried admission count exceeds source count".into()));}
        let deferred_count=carried_count-admitted_count;
        payload["change"]["carried_admission"]=json!({"admitted_count":admitted_count,"admitted_message_ids":admitted_ids,"admitted_message_ids_truncated":admitted_count>128,"deferred_count":deferred_count,"deferred_message_ids":deferred_ids,"deferred_message_ids_truncated":deferred_count>deferred_ids.len() as i64,"deferred_state":"held_unadmitted_with_original_assignment"});
        // Finalize the new, uncommitted audit node with actual selected/deferred outcomes.
        let final_envelope=json!({"event_id":event,"kind":"context_changed","payload":payload});
        let final_payload=payload.to_string();
        let node_body=json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {final_envelope}")}],"event_id":event}).to_string();
        let body_hash=crate::digest::sha256_hex(node_body.as_bytes());
        if tx.execute(
            "UPDATE event SET payload_ref_json=?2 WHERE id=?1 AND target_thread_id=?3 AND kind='context_changed'",
            params![event,final_payload,thread],
        )? != 1 {
            return Err(crate::storage::StorageError::Schema(
                "turn interruption event disappeared before commit".into(),
            ));
        }
        if tx.execute(
            "UPDATE node SET payload_json=?2,payload_bytes=?3,body_hash=?4 WHERE id=?1 AND kind='event'",
            params![format!("event-{event}"),node_body,node_body.len() as i64,body_hash],
        )? != 1 {
            return Err(crate::storage::StorageError::Schema(
                "turn interruption audit node disappeared before commit".into(),
            ));
        }
        tx.execute("UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",params![thread,at])?;
        tx.commit()?;
        wait_wakes.publish(super::wait::wakes::Key::recovery(thread, run, turn));
        Ok(true)
    }).await.map_err(storage_error)?;
    if changed {
        control.changed(&target_thread);
    }
    Ok(())
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use crate::{config::Config, protocol::ShellToolArgs, shell::ShellSupervisor};

    async fn fixture() -> (
        tempfile::TempDir,
        DataOwner,
        Arc<Control>,
        String,
        String,
        String,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(directory.path().join("data")),
            directory.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, directory.path().into());
        let thread = control
            .store
            .execute(
                Request::new(
                    format!("{}:create", owner.operation_epoch.current()),
                    Command::ThreadCreate(ThreadCreateArgs {
                        agents: None,
                        connection: None,
                        cwd: None,
                        model: None,
                        effort: None,
                        parent_thread_id: None,
                    }),
                ),
                None,
            )
            .await
            .unwrap()
            .result["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let accepted = control
            .store
            .execute(
                Request::new(
                    format!("{}:send", owner.operation_epoch.current()),
                    Command::MessageSend(MessageSendArgs {
                        max_model_requests: None,
                        resume_recovery: false,

                        human: false,
                        sender_label: None,
                        thread_id: Some(thread.clone()),
                        text: "fixture".into(),
                        deliver: crate::protocol::Deliver::Next,
                    }),
                ),
                None,
            )
            .await
            .unwrap()
            .result;
        (
            directory,
            owner,
            control,
            thread,
            accepted["run_id"].as_str().unwrap().into(),
            accepted["turn_id"].as_str().unwrap().into(),
        )
    }

    #[tokio::test]
    async fn turn_interrupt_command_and_tui_save_the_same_turn_fence() {
        for wire in [false, true] {
            let (_root, owner, control, thread, run, turn) = fixture().await;
            let slot = control
                .live
                .register_or_nudge_driver(&thread, run.clone(), &control.stop)
                .await
                .unwrap();
            let ack = if wire {
                let request = Request::new(
                    format!("{}:turn-interrupt", owner.operation_epoch.current()),
                    Command::TurnInterrupt(crate::protocol::TurnTarget {
                        turn_id: turn.clone(),
                    }),
                );
                let response = control.request(request.clone()).await;
                assert_eq!(control.request(request).await.result, response.result);
                response.result.unwrap()
            } else {
                control.interrupt_turn(turn.clone()).await.unwrap()
            };
            assert_eq!(ack["requested"], true);
            let at = ack["interrupt_requested_at"].as_str().unwrap().to_owned();
            control.store.database.call(move |db| {
                let saved = db.query_row("SELECT u.interrupt_requested_at,u.generation,r.state FROM turn u JOIN run r ON r.id=u.run_id WHERE u.id=?1 AND r.id=?2", params![turn,run], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))?;
                assert_eq!(saved, (at, 1, "live".into()));
                Ok(())
            }).await.unwrap();
            control
                .live
                .remove_driver_if_current(&thread, &slot.id)
                .await;
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn interruption_rebinds_the_active_turn_but_never_a_stale_target() {
        let (_directory, owner, control, thread, run, old_turn) = fixture().await;
        let slot = control
            .live
            .register_or_nudge_driver(&thread, run.clone(), &control.stop)
            .await
            .unwrap();
        let old = slot.bind_turn(&control, &old_turn).await.unwrap();
        let saved_thread = thread.clone();
        let saved_run = run.clone();
        let saved_old = old_turn.clone();
        let new_turn = control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                tx.execute("UPDATE turn SET state='completed' WHERE id=?1", [saved_old])?;
                let next = state::create_turn_tx(&tx, &saved_thread, &saved_run, "now", "now")?;
                tx.commit()?;
                Ok(next)
            })
            .await
            .unwrap();
        let binding = reserve_binding(&control, &thread, &new_turn, Some(&slot))
            .await
            .unwrap()
            .unwrap();
        let driver = slot.bind_turn(&control, &new_turn).await.unwrap();
        assert!(Arc::ptr_eq(&binding.start_gate, &driver.start_gate));
        assert!(!Arc::ptr_eq(&old.start_gate, &driver.start_gate));
        let stale = control
            .live
            .admit_turn_interrupt(&control, &thread, &old_turn)
            .await
            .unwrap();
        assert_eq!(stale["requested"], false);
        assert!(!driver.cancel.is_cancelled());
        assert!(Arc::ptr_eq(
            &slot.turn.lock().await.as_ref().unwrap().start_gate,
            &driver.start_gate
        ));
        let result = control
            .live
            .admit_turn_interrupt(&control, &thread, &new_turn)
            .await
            .unwrap();
        assert_eq!(result["requested"], true);
        assert!(driver.cancel.is_cancelled());
        assert!(*driver.start_gate.lock().await);
        control
            .live
            .remove_driver_if_current(&thread, &slot.id)
            .await;
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn interruption_leaves_reply_held_when_older_held_question_blocks_its_carry() {
        let (_directory, owner, control, thread, run, turn) = fixture().await;
        let slot = control
            .live
            .register_or_nudge_driver(&thread, run.clone(), &control.stop)
            .await
            .unwrap();
        slot.bind_turn(&control, &turn).await.unwrap();
        let held = control
            .store
            .execute(
                Request::new(
                    format!("{}:held-before-question", owner.operation_epoch.current()),
                    Command::MessageSend(MessageSendArgs {
                        max_model_requests: None,
                        resume_recovery: false,
                        human: true,
                        sender_label: None,
                        thread_id: Some(thread.clone()),
                        text: "held H".into(),
                        deliver: crate::protocol::Deliver::Complete,
                    }),
                ),
                None,
            )
            .await
            .unwrap();
        let held_id = held.result["message_id"].as_str().unwrap().to_owned();
        // Seed a retained ordinary hold; waiting Revisit input is not a barrier.
        owner.db.call({
            let held_id = held_id.clone();
            move |db| {
                assert_eq!(db.execute("UPDATE receipt SET hold_reason='previous_instance_interrupted' WHERE message_node_id=?1 AND state='held' AND turn_id IS NULL", [&held_id])?, 1);
                Ok(())
            }
        }).await.unwrap();
        let origin = crate::protocol::Origin {
            instance_id: control.store.instance_id.clone(),
            thread_id: thread.clone(),
            run_id: run.clone(),
            turn_id: turn.clone(),
            execution_id: "question-producer".into(),
            token: "question-producer-token".into(),
        };
        let question = control
            .store
            .execute(
                Request::new(
                    format!("{}:held-frontier-question", owner.operation_epoch.current()),
                    Command::MessageAsk(MessageAskArgs {
                        human: false,
                        sender_label: None,
                        thread_id: None,
                        to: None,
                        deliver: Deliver::Next,
                        text: "Q behind held H".into(),
                    }),
                ),
                Some(origin),
            )
            .await
            .unwrap();
        let question_id = question.result["message_id"].as_str().unwrap().to_owned();
        let reply = control
            .store
            .execute(
                Request::new(
                    format!("{}:held-frontier-reply", owner.operation_epoch.current()),
                    Command::MessageReply(MessageReplyArgs {
                        message_id: question_id.clone(),
                        text: "R depends on Q".into(),
                        sender_label: None,
                    }),
                ),
                None,
            )
            .await
            .unwrap();
        let reply_id = reply.result["message_id"].as_str().unwrap().to_owned();

        let interrupt = control
            .live
            .admit_turn_interrupt(&control, &thread, &turn)
            .await
            .unwrap();
        assert_eq!(interrupt["requested"], true);
        acknowledge(&control, &thread, &run).await.unwrap();
        let (held_state, question_selected, reply_state, reply_admission, reply_run, reply_turn, reply_selected, audit) = owner.db.call({
            let thread = thread.clone();
            let held_id = held_id.clone();
            let question_id = question_id.clone();
            let reply_id = reply_id.clone();
            let event_id = format!("turn-interrupted-{turn}");
            move |db| {
                let held_state: String = db.query_row("SELECT state FROM receipt WHERE message_node_id=?1",[&held_id],|row|row.get(0))?;
                let question_selected: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2)",rusqlite::params![thread,question_id],|row|row.get(0))?;
                let (reply_state,reply_admission,reply_run,reply_turn,reply_selected):(String,String,Option<String>,Option<String>,bool) = db.query_row(
                    "SELECT r.state,r.path_admission,r.run_id,r.turn_id,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id) FROM receipt r WHERE r.message_node_id=?1",
                    [&reply_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
                )?;
                let audit: String = db.query_row("SELECT payload_ref_json FROM event WHERE id=?1",[event_id],|row|row.get(0))?;
                Ok((held_state,question_selected,reply_state,reply_admission,reply_run,reply_turn,reply_selected,audit))
            }
        }).await.unwrap();
        assert_eq!(held_state, "held");
        assert!(!question_selected);
        assert_eq!(reply_state, "held");
        assert_eq!(reply_admission, "unadmitted");
        assert_eq!(reply_run.as_deref(), Some(run.as_str()));
        assert_eq!(reply_turn.as_deref(), Some(turn.as_str()));
        assert!(
            !reply_selected,
            "a deferred reply stays absent from selected history"
        );
        let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
        assert_eq!(audit["change"]["carried_admission"]["deferred_count"], 1);
        assert_eq!(
            audit["change"]["carried_admission"]["deferred_message_ids"],
            json!([reply_id])
        );
        let carried = audit["change"]["carried_count"].as_i64().unwrap();
        let admitted = audit["change"]["carried_admission"]["admitted_count"]
            .as_i64()
            .unwrap();
        let deferred = audit["change"]["carried_admission"]["deferred_count"]
            .as_i64()
            .unwrap();
        assert_eq!(
            admitted + deferred,
            carried,
            "the interruption audit separates selected from still-held carried sources"
        );
        control
            .live
            .remove_driver_if_current(&thread, &slot.id)
            .await;
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn interruption_fences_replacement_registry_publication_until_durable_commit() {
        use futures_util::FutureExt;
        let (_directory, owner, control, thread, old_run, old_turn) = fixture().await;
        let old_slot = control
            .live
            .register_or_nudge_driver(&thread, old_run.clone(), &control.stop)
            .await
            .unwrap();
        let saved_thread = thread.clone();
        let saved_run = old_run.clone();
        let saved_old = old_turn.clone();
        let new_turn=control.store.database.call(move|db|{
            let tx=db.transaction()?;tx.execute("UPDATE turn SET state='completed' WHERE id=?1",[saved_old])?;
            tx.execute("UPDATE run SET state='completed' WHERE id=?1",[saved_run])?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('replacement-run',?1,'live','now')",[&saved_thread])?;
            let next=state::create_turn_tx(&tx,&saved_thread,"replacement-run","now","now")?;tx.commit()?;Ok(next)
        }).await.unwrap();
        let (entered, receive) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let db = control.store.database.clone();
        // A finite fixture-only DB barrier exposes registry ownership during await.
        let barrier = tokio::spawn(async move {
            db.call(move |_| {
                entered.send(()).unwrap();
                crate::test_watchdog::blocking_recv("release interrupt DB barrier", &blocked)
                    .unwrap();
                Ok(())
            })
            .await
            .unwrap()
        });
        receive.await.unwrap();
        let mut interrupt = Box::pin(
            control
                .live
                .admit_turn_interrupt(&control, &thread, &new_turn),
        );
        assert!(interrupt.as_mut().now_or_never().is_none());
        assert!(
            control.live.test_drivers().try_lock().is_err(),
            "driver publication must stay fenced while the DB commit is pending"
        );
        let mut replacement = Box::pin(async {
            control
                .live
                .remove_driver_if_current(&thread, &old_slot.id)
                .await;
            let slot = control
                .live
                .register_or_nudge_driver(&thread, "replacement-run".into(), &control.stop)
                .await
                .unwrap();
            let binding = slot.bind_turn(&control, &new_turn).await.unwrap();
            (slot, binding)
        });
        assert!(replacement.as_mut().now_or_never().is_none());
        release.send(()).unwrap();
        barrier.await.unwrap();
        assert_eq!(interrupt.await.unwrap()["requested"], true);
        let (slot, binding) = replacement.await;
        assert!(binding.cancel.is_cancelled());
        assert!(*binding.start_gate.lock().await);
        control
            .live
            .remove_driver_if_current(&thread, &slot.id)
            .await;
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn interrupted_native_start_gate_refuses_the_os_side_effect() {
        let directory = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(ShellSupervisor::new(&Config::default()));
        let gate = Arc::new(Mutex::new(false));
        let mut admission = gate.lock().await;
        let task_gate = gate.clone();
        let cwd = directory.path().to_path_buf();
        let task = tokio::spawn(async move {
            let args:ShellToolArgs=serde_json::from_value(json!({"command":if cfg!(windows){"[IO.File]::WriteAllText('must-not-exist','forbidden')"}else{"printf forbidden > must-not-exist"}})).unwrap();
            supervisor
                .spawn_guarded(
                    "losing-execution".into(),
                    cwd,
                    args,
                    std::collections::BTreeMap::new(),
                    task_gate,
                    None,
                )
                .await
        });
        // The interrupt owns the same gate through its commit; the spawn loses.
        *admission = true;
        drop(admission);
        let result = task.await.unwrap();
        let Err(error) = result else {
            panic!("interrupted call created an OS process");
        };
        assert_eq!(
            error.details.unwrap()["reason"],
            "turn_interrupted_before_spawn"
        );
        assert!(!directory.path().join("must-not-exist").exists());
    }
}
