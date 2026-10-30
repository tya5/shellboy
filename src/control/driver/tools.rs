//! Preflight and serial execution of one already-adopted tool group.
use super::*;
pub(super) async fn run_tool_group(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    group: &str,
    calls: Vec<model::ToolCall>,
) -> Result<(), ApiError> {
    let turn_binding = slot.bind_turn(control, &snapshot.turn).await?;
    let mut cancelled: Option<Value> = None;
    let mut validation_errors = std::collections::HashMap::new();
    // Validate every encoding and initial input before the first OS spawn.
    for call in &calls {
        let args = match &call.args {
            Ok(args) => args,
            Err(error) => {
                validation_errors.insert(call.call_id.clone(), error.clone());
                continue;
            }
        };
        for encoding in [&args.input_encoding, &args.output_encoding] {
            if let Err(error) = crate::shell::encode_input(
                "",
                encoding,
                control.store.config.limits.shell_stdin_bytes,
            ) {
                validation_errors.insert(call.call_id.clone(), error.to_string());
            }
        }
        if let Some(text) = &args.initial_stdin
            && let Err(error) = crate::shell::encode_input(
                text,
                &args.input_encoding,
                control.store.config.limits.shell_stdin_bytes,
            )
        {
            validation_errors.insert(call.call_id.clone(), error.to_string());
        }
    }
    if !validation_errors.is_empty() {
        cancelled = Some(cancelled_call(
            "tool_group_validation_failed",
            "Tool group validation failed before any side effect",
            None,
        ));
    }
    let mut cancelled_count = 0usize;
    let mut foreground_release = None;
    for call in calls {
        if control.shell.starts_closed() {
            cancelled = Some(cancelled_call(
                "owner_shutdown_before_call",
                "Owner shutdown started before this call",
                None,
            ));
        } else if slot.stop.is_cancelled() {
            cancelled = Some(cancelled_call(
                "run_stopped_before_call",
                "Run stopped before this call",
                None,
            ));
        } else if turn_binding.cancel.is_cancelled() && cancelled.is_none() {
            cancelled = Some(cancelled_call(
                "human_turn_interrupted",
                "A human interrupted this Turn before this call started",
                None,
            ));
        }
        let result = if let Some(error) = validation_errors.get(&call.call_id) {
            json!({"status":"error","error":error})
        } else if let Some(reason) = &cancelled {
            reason.clone()
        } else {
            match run_shell(
                control.clone(),
                snapshot,
                slot,
                group,
                &call.call_id,
                call.args.expect("all arguments were preflighted"),
            )
            .await
            {
                Ok((result, halt)) => {
                    if halt {
                        cancelled = Some(cancelled_after(&call.call_id, &result));
                    }
                    result
                }
                Err(error) => {
                    if error.is_storage_failure() {
                        control.fail_storage().await;
                        return Err(error);
                    }
                    // Admission conflicts concern a pre-existing call. They are
                    // not a new tool failure that may complete that call.
                    if error.code == ErrorCode::OperationConflict {
                        return Err(error);
                    }
                    cancelled = Some(cancelled_call(
                        "previous_call_failed",
                        &error.to_string(),
                        Some(json!({"call_id":call.call_id,"state":"error"})),
                    ));
                    json!({"status":"error","error":error})
                }
            }
        };
        let release = match result.get("reason").and_then(Value::as_str) {
            Some("human_backgrounded") => Some(ForegroundRelease::Background),
            Some("additional_input") => Some(ForegroundRelease::AdditionalInput),
            _ => None,
        };
        if let Some(reason) = release
            && let Some(execution) = result.get("execution_id").and_then(Value::as_str)
        {
            foreground_release = Some((execution.to_owned(), reason));
        }
        if result.get("status").and_then(Value::as_str) == Some("cancelled") {
            cancelled_count += 1;
        }
        save_tool_result(control, snapshot, group, &call.call_id, result).await?;
    }
    control.index.wake();
    let group_copy = group.to_owned();
    let attachment_targets = control.store.database.call(move |db| {
            let tx=db.transaction()?;
            if tx.execute("UPDATE tool_group SET state='completed' WHERE id=?1 AND state='running'",[&group_copy])? == 0 {
                let state:Option<String> = tx.query_row("SELECT state FROM tool_group WHERE id=?1",[&group_copy],|r|r.get(0)).optional()?;
                match state.as_deref() {
                    Some("completed") => return Ok(Vec::new()),
                    Some("running") => return Err(ApiError::new(ErrorCode::OperationConflict,"Tool group did not finish at its expected boundary").into()),
                    None => return Err(ApiError::new(ErrorCode::TargetUnavailable,"Tool group no longer exists").into()),
                    _ => return Err(ApiError::new(ErrorCode::InvalidState,"Unknown saved tool group state").into()),
                }
            }
            let targets=tx.prepare("SELECT DISTINCT d.thread_id FROM attachment_delivery d JOIN shell_execution s ON s.id=json_extract(d.origin_json,'$.execution_id') JOIN tool_call c ON c.operation_id=s.id WHERE c.group_id=?1 AND d.status IN ('accepted','held')")?.query_map([&group_copy],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            // Idle target Threads need no synthetic Run merely to adopt refs.
            for target in &targets {
                let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run WHERE thread_id=?1 AND state IN ('live','stopping'))",[target],|r|r.get(0))?;
                if !active { crate::control::attachment_delivery::apply_pending_tx(&tx,target,&now()).map_err(crate::storage::StorageError::from)?; }
            }
            tx.commit()?;
            Ok(targets)
        }).await.map_err(storage_error)?;
    for target in attachment_targets {
        control.changed(&target);
        control.kick_quiet(target).await;
    }
    if foreground_release.is_some() || cancelled_count > 0 {
        control.publish_event(EngineEvent::Notice {
            thread_id: snapshot.thread.clone(),
            foreground_release,
            cancelled_calls: cancelled_count,
        });
    }
    Ok(())
}

fn cancelled_call(reason: &str, message: &str, prerequisite: Option<Value>) -> Value {
    let mut result =
        json!({"status":"cancelled","executed":false,"reason":reason,"message":message});
    if let Some(prerequisite) = prerequisite {
        result["prerequisite"] = prerequisite;
    }
    result
}

fn cancelled_after(call_id: &str, result: &Value) -> Value {
    let mut prerequisite = json!({"call_id":call_id});
    for field in [
        "execution_id",
        "exit_code",
        "output_complete",
        "wait_release_outcome",
    ] {
        if let Some(value) = result.get(field).filter(|value| !value.is_null()) {
            prerequisite[field] = value.clone();
        }
    }
    if let Some(state) = result.get("status").filter(|value| value.is_string()) {
        prerequisite["state"] = state.clone();
    }
    let (reason, message) = match result.get("reason").and_then(Value::as_str) {
        Some("human_turn_interrupted") => (
            "previous_turn_interrupted",
            "A human interrupted the previous shell's foreground wait; inspect its execution before repeating work",
        ),
        Some("additional_input") => (
            "previous_wait_released_by_new_input",
            "New input released the previous shell's foreground wait; inspect its execution before repeating work",
        ),
        Some("human_backgrounded") => (
            "previous_wait_released_by_human_background",
            "A human released the previous shell's foreground wait; inspect its execution before repeating work",
        ),
        _ if result.get("output_complete").and_then(Value::as_bool) == Some(false) => (
            "previous_output_incomplete",
            "Previous shell output is incomplete",
        ),
        _ if result
            .get("wait_release_outcome")
            .is_some_and(|value| !value.is_null()) =>
        {
            (
                "previous_wait_released",
                "The previous shell recorded a wait release outcome; inspect that outcome before continuing",
            )
        }
        _ => (
            "previous_call_failed",
            "Previous shell did not complete successfully",
        ),
    };
    cancelled_call(reason, message, Some(prerequisite))
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn incomplete_output_has_its_own_reason_and_keeps_known_zero_exit() {
        let result = cancelled_after(
            "prerequisite",
            &json!({
                "status":"exited", "execution_id":"execution", "exit_code":0,
                "output_complete":false,
            }),
        );
        assert_eq!(result["reason"], "previous_output_incomplete");
        assert_eq!(result["executed"], false);
        assert_eq!(result["prerequisite"]["exit_code"], 0);
        assert_eq!(result["prerequisite"]["state"], "exited");
        assert_eq!(result["prerequisite"]["output_complete"], false);
        assert!(result["prerequisite"].get("wait_release_outcome").is_none());
    }

    #[test]
    fn saved_cli_wait_outcome_is_not_fabricated_as_a_process_failure() {
        let result = cancelled_after(
            "wait-call",
            &json!({
                "status":"exited", "execution_id":"wait-execution", "exit_code":0,
                "output_complete":true, "wait_release_outcome":"timeout",
            }),
        );
        assert_eq!(result["reason"], "previous_wait_released");
        assert_eq!(result["prerequisite"]["wait_release_outcome"], "timeout");
        assert_eq!(result["prerequisite"]["exit_code"], 0);
    }
}
