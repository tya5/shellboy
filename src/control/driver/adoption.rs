//! Completed response adoption and its existing post-commit effects.
use super::*;
pub(super) async fn adopt_response(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    attempt: &str,
    response: &model::ModelResponse,
    prepared: Arc<crate::attachments::PreparedAttachments>,
    group: Option<String>,
) -> Result<bool, ApiError> {
    let current = snapshot.clone();
    let raw = response.raw.clone();
    let stream_observation = response.stream_observation.as_ref().map(|observation| {
        json!({
            "classification":"stream_difference",
            "attempt_id":attempt,
            "differences":observation.differences,
            "delta_differences":observation.delta_differences,
            "total_differences":observation.total_differences,
            "partial_text":observation.partial_text,
            "partial_reasoning":observation.partial_reasoning,
            "completed_items":observation.completed_items,
            "raw_response":response.raw,
        })
    });
    let output = response.output.clone();
    let group_id = group;
    let attempt_id = attempt.to_owned();
    let index = control.index.clone();
    let successful_attachments = prepared;
    let may_execute=control.store.database.call(move|db|{
            let tx=db.transaction()?;
            let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id WHERE u.id=?1 AND u.generation=?2 AND r.state='live' AND u.state='running')",params![current.turn,current.generation],|r|r.get(0))?;
            if !live{return Ok(false);}
            if crate::control::state::complete_attempt(&tx, &attempt_id, &raw, &now())?
                != crate::control::state::AttemptTransition::Applied {
                return Ok(false);
            }
            let node=id();
            history::append_node(&tx,&current.thread,&node,Some(&current.turn),"assistant_response",&json!({"output":output,"reasoning_epoch":current.epoch,"group_id":group_id}),&now())?;
            index.authorize_new_tx(&tx,std::slice::from_ref(&node)).map_err(crate::storage::StorageError::from)?;
            // Same commit/root-revision boundary as the selected answer. This
            // received source remains outside thread_path and model replay.
            if let Some(body) = &stream_observation {
                let body = body.to_string();
                tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'model_stream_observation',?3,?4,?5)",params![id(),current.turn,now(),body,body.len() as i64])?;
            }
            for (ordinal,item)in output.iter().enumerate(){tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_id,item_type,phase,status,payload_json,node_id)VALUES(?1,?2,?3,?4,?5,'completed',?6,?7)",params![attempt_id,ordinal as i64,item.get("id").and_then(Value::as_str),item.get("type").and_then(Value::as_str).unwrap_or("unknown"),item.get("phase").and_then(Value::as_str),item.to_string(),node])?;}
            for receipt in &current.receipts{tx.execute("UPDATE receipt SET state='applied',updated_at=?2 WHERE id=?1 AND state='accepted'",params![receipt,now()])?;}
            for event in &current.events{tx.execute("UPDATE delivery SET state='processed',processed_attempt_id=?2 WHERE event_id=?1 AND recipient_id=?3",params![event,attempt_id,current.run])?;tx.execute("UPDATE event SET target_run_id=?2 WHERE id=?1 AND kind='context_changed' AND target_run_id IS NULL",params![event,current.run])?;}
            if let Some(group)=&group_id{
                tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,?3,'running',?4)",params![group,attempt_id,current.turn,now()])?;
                for (ordinal,item)in output.iter().filter(|v|v.get("type").and_then(Value::as_str)==Some("function_call")).enumerate(){tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES(?1,?2,?3,'shell',?4,'pending')",params![group,item["call_id"].as_str().unwrap_or(""),ordinal as i64,item["arguments"].as_str().unwrap_or("{}")])?;}
            }else{
                crate::control::state::complete_answer_turn(&tx, &current.thread, &current.run, &current.turn, &node)?;
            }
            crate::attachments::mark_success_tx(&tx,&successful_attachments,&attempt_id).map_err(crate::storage::StorageError::from)?;
            tx.commit()?;Ok(true)
        }).await.map_err(storage_error)?;
    control.index.wake();
    control.changed(&snapshot.thread);
    control.nudge_owner(&snapshot.run).await;
    Ok(may_execute)
}
