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
    /// Dedicated human frontend action. It is deliberately absent from model CLI/wire commands.
    pub async fn interrupt_turn(
        self: &Arc<Self>,
        thread: String,
        turn: String,
    ) -> Result<Value, ApiError> {
        let control = self.clone();
        tokio::spawn(async move { control.interrupt_turn_inner(thread, turn).await })
            .await
            .map_err(|_| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "Turn interruption worker failed; inspect the saved Turn",
                )
            })?
    }
    async fn interrupt_turn_inner(
        self: &Arc<Self>,
        thread: String,
        turn: String,
    ) -> Result<Value, ApiError> {
        let _admission = self.admit(true)?;
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
            let mut result=json!({"thread_id":saved_thread,"run_id":run,"turn_id":saved_turn,"requested":false,"already_requested":false,"state":"already_finished"});
            if run_state!="live" {result["state"]=json!(if run_state=="stopping"{"run_stopping"}else{"already_finished"});return Ok(result);}
            if !active{return Ok(result);}
            if let Some(at)=at {result["already_requested"]=json!(true);result["interrupt_requested_at"]=json!(at);result["state"]=json!("interrupt_requested");return Ok(result);}
            let at=state::now();
            tx.execute("UPDATE turn SET interrupt_requested_at=?2,generation=generation+1 WHERE id=?1 AND interrupt_requested_at IS NULL",params![saved_turn,at])?;
            tx.execute("UPDATE model_attempt SET state='cancelled',ended_at=?2,error_json=?3 WHERE turn_id=?1 AND state IN ('preparing','inflight')",params![saved_turn,at,json!({"kind":"human_turn_interrupted","message":"A human interrupted this Turn"}).to_string()])?;
            revisit::drop_pending(&tx,&run).map_err(crate::storage::StorageError::from)?;
            tx.execute("UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",params![saved_thread,at])?;
            result["requested"]=json!(true);result["state"]=json!("interrupt_requested");result["interrupt_requested_at"]=json!(at);
            tx.commit()?;Ok(result)
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
    let changed=control.store.database.call(move|db|{
        let tx=db.transaction()?;
        let interrupted:Option<(String,String)>=tx.query_row("SELECT u.id,u.interrupt_requested_at FROM turn u JOIN run r ON r.id=u.run_id WHERE u.thread_id=?1 AND u.run_id=?2 AND r.state='live' AND u.state IN ('preparing','running','waiting','recovery_wait') AND u.interrupt_requested_at IS NOT NULL",params![thread,run],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((turn,requested_at))=interrupted else{return Ok(false);};
        let group_running:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_group WHERE turn_id=?1 AND state<>'completed')",[&turn],|r|r.get(0))?;
        if group_running{return Ok(false);}
        let at=state::now();
        // Settle only notifications that were already in this Turn's input.
        tx.execute("UPDATE delivery SET state='processed' WHERE recipient_id=?1 AND state='pending' AND (included_attempt_id IN (SELECT id FROM model_attempt WHERE turn_id=?2) OR event_id IN (SELECT json_extract(payload_json,'$.event_id') FROM node WHERE turn_id=?2 AND kind='event'))",params![run,turn])?;
        tx.execute("UPDATE event SET target_run_id=?1 WHERE kind='context_changed' AND target_run_id IS NULL AND id IN (SELECT event_id FROM delivery WHERE recipient_id=?1 AND state='processed')",[&run])?;
        tx.execute("UPDATE turn SET state='stopped',ended_at=?2 WHERE id=?1",params![turn,at])?;
        message_delivery::repair_terminal_tx(&tx,&at)?;
        let carried_count:i64=tx.query_row("SELECT count(*) FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted'",params![turn,thread],|r|r.get(0))?;
        let carried_sources=tx.prepare("SELECT id,message_node_id,run_id,turn_id FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted' ORDER BY created_at,rowid LIMIT 128")?.query_map(params![turn,thread],|r|Ok(json!({"receipt_id":r.get::<_,String>(0)?,"message_id":r.get::<_,String>(1)?,"original_run_id":r.get::<_,Option<String>>(2)?,"original_turn_id":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let last_carried:Option<String>=tx.query_row("SELECT message_node_id FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted' ORDER BY created_at DESC,rowid DESC LIMIT 1",params![turn,thread],|r|r.get(0)).optional()?;
        let new_turn=if carried_count>0{Some(state::create_turn_tx(&tx,&thread,&run,&at,&at)?)}else{None};
        let adopted_group:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tool_group WHERE turn_id=?1)",[&turn],|r|r.get(0))?;
        let interrupted_attempt: Option<String> = tx.query_row(
            "SELECT id FROM model_attempt WHERE turn_id=?1 AND state='cancelled' AND json_extract(error_json,'$.kind')='human_turn_interrupted' ORDER BY started_at DESC,rowid DESC LIMIT 1",
            [&turn], |r|r.get(0),
        ).optional()?;
        let phase = if interrupted_attempt.is_some() {"model_attempt_interrupted"} else {"turn_interrupted"};
        let meaning = "A human interrupted this Turn. Any response not adopted before interruption is retained audit-only and is absent from model history. Saved tool-call results are authoritative; cancelled calls did not execute. Inspect continuing execution IDs before repeating work.";
        let event=format!("turn-interrupted-{turn}");
        let payload=json!({"kind":"turn_interrupted","actor":{"kind":"human"},"change":{"turn_id":turn,"run_id":run,"requested_at":requested_at,"phase":phase,"meaning":meaning,"had_adopted_groups":adopted_group,"interrupted_attempt_id":interrupted_attempt,"new_turn_id":new_turn,"carried_count":carried_count,"carried_sources":carried_sources,"carried_sources_truncated":carried_count>128,"first_carried_message_id":carried_sources.first().and_then(|s|s.get("message_id")),"last_carried_message_id":last_carried},"observed_at":at});
        tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES(?1,'context_changed','{\"kind\":\"human\"}',?2,?3,?4)",params![event,thread,payload.to_string(),at])?;
        let envelope=json!({"event_id":event,"kind":"context_changed","payload":payload});
        crate::history::append_node(&tx,&thread,&format!("event-{event}"),Some(&turn),"event",&json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {envelope}")}],"event_id":event}),&at)?;
        tx.execute("INSERT INTO delivery(event_id,recipient_id,state)VALUES(?1,?2,'processed')",params![event,run])?;
        if let Some(new_turn)=&new_turn {
            loop {
                let rows=tx.prepare("SELECT id,message_node_id FROM receipt WHERE turn_id=?1 AND target_thread_id=?2 AND state='held' AND path_admission='unadmitted' ORDER BY created_at,rowid LIMIT 128")?.query_map(params![turn,thread],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                if rows.is_empty(){break;}
                let nodes=rows.iter().map(|r|r.1.clone()).collect::<Vec<_>>();
                let revision=crate::history::selection_revision(&tx,&thread,crate::history::ThreadScope::Live)?;
                crate::history::append_existing_nodes(&tx,&thread,revision as u64,&nodes,&at)?;
                message_delivery::mark_admitted_tx(&tx,&thread,&nodes)?;
                for(receipt,_)in rows {tx.execute("UPDATE receipt SET state='applied',hold_reason='carried_after_turn_interruption',updated_at=?2,result_json=json_set(COALESCE(result_json,'{}'),'$.delivery_context',json(?3)) WHERE id=?1",params![receipt,at,json!({"run_id":run,"turn_id":new_turn,"reason":"carried_after_turn_interruption","interrupted_turn_id":turn,"admitted_at":at}).to_string()])?;}
            }
        }
        tx.execute("UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",params![thread,at])?;
        tx.commit()?;Ok(true)
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
                    format!("{}:create", owner.instance_id),
                    Command::ThreadCreate(ThreadCreateArgs {
                        agents: None,
                        connection: None,
                        cwd: None,
                        model: None,
                        effort: None,
                        parent_id: None,
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
                    format!("{}:send", owner.instance_id),
                    Command::MessageSend(MessageSendArgs {
                        thread_id: Some(thread.clone()),
                        text: "fixture".into(),
                        now: false,
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
    async fn interruption_rebinds_the_active_turn_but_never_a_stale_target() {
        let (_directory, owner, control, thread, run, old_turn) = fixture().await;
        let slot = control
            .live
            .register_or_nudge_driver(&thread, run.clone(), &control.stop, false)
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
    async fn interruption_fences_replacement_registry_publication_until_durable_commit() {
        use futures_util::FutureExt;
        let (_directory, owner, control, thread, old_run, old_turn) = fixture().await;
        let old_slot = control
            .live
            .register_or_nudge_driver(&thread, old_run.clone(), &control.stop, false)
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
                blocked
                    .recv_timeout(std::time::Duration::from_secs(15))
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
                .register_or_nudge_driver(&thread, "replacement-run".into(), &control.stop, false)
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
