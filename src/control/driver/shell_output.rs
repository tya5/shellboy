//! Raw shell output batching, persistence, and completion publication.
use super::{Snapshot, invalid, now};
use crate::{
    app::storage_error,
    control::Control,
    protocol::{ApiError, ErrorCode, ShellState},
    shell::{OutputChunk, OutputStream, ShellCompletion, ShellEvent},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

/// Keep original chunks/sequences and their permits; only the transaction
/// boundary changes. A carry chunk prevents overshooting the byte bound.
async fn output_batch(
    events: &mut tokio::sync::mpsc::Receiver<ShellEvent>,
    pending: &mut Option<OutputChunk>,
    max_bytes: u64,
    max_delay: std::time::Duration,
) -> Option<Vec<OutputChunk>> {
    let first = match pending.take() {
        Some(chunk) => chunk,
        None => match events.recv().await? {
            ShellEvent::Output(chunk) => chunk,
        },
    };
    let deadline = tokio::time::Instant::now() + max_delay;
    let mut bytes = first.bytes.len() as u64;
    let mut batch = vec![first];
    while bytes < max_bytes {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => break,
            event = events.recv() => match event {
                None => break,
                Some(ShellEvent::Output(chunk)) => {
                    if bytes + chunk.bytes.len() as u64 > max_bytes {
                        *pending = Some(chunk);
                        break;
                    }
                    bytes += chunk.bytes.len() as u64;
                    batch.push(chunk);
                }
            },
        }
    }
    Some(batch)
}

pub(super) async fn collect_shell(
    control: &Control,
    snapshot: &Snapshot,
    execution: &str,
    mut shell: crate::shell::ShellExecution,
) -> Result<ShellCompletion, ApiError> {
    let mut stdout = 0i64;
    let mut stderr = 0i64;
    let mut save_failure = None;
    let mut pending = None;
    let batch_bytes = control.store.config.io.output_batch_bytes;
    let batch_delay = std::time::Duration::from_millis(control.store.config.io.output_batch_ms);
    while let Some(batch) =
        output_batch(&mut shell.events, &mut pending, batch_bytes, batch_delay).await
    {
        if save_failure.is_some() {
            drop(batch);
            continue;
        }
        let execution = execution.to_owned();
        let (mut next_stdout, mut next_stderr) = (stdout, stderr);
        // The owned chunks, including their byte-budget permits, stay in this
        // DB closure until the entire ordered batch commits or rolls back.
        let saved = control.store.database.call(move |db| {
            let tx = db.transaction()?;
            for chunk in &batch {
                let (stream, offset) = match chunk.stream {
                    OutputStream::Stdout => ("stdout", &mut next_stdout),
                    OutputStream::Stderr => ("stderr", &mut next_stderr),
                };
                tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES(?1,?2,?3,?4,?5,?6)", params![execution,chunk.sequence as i64,stream,*offset,&chunk.bytes,now()])?;
                *offset += chunk.bytes.len() as i64;
            }
            let saved_sequence = batch.last().expect("nonempty output batch").sequence as i64 + 1;
            tx.execute("UPDATE shell_execution SET saved_sequence=?2 WHERE id=?1", params![execution,saved_sequence])?;
            tx.commit()?;
            drop(batch); // Return the byte budget only after successful commit.
            Ok((next_stdout, next_stderr))
        }).await;
        match saved {
            Ok((out, err)) => {
                (stdout, stderr) = (out, err);
                control.changed.notify_waiters();
            }
            Err(error) => {
                shell.handle.stop();
                control.fail_storage().await;
                save_failure = Some(error.into_api_error());
            }
        }
    }
    let completion = loop {
        if let Some(value) = shell.completion.borrow().clone() {
            break value;
        }
        shell
            .completion
            .changed()
            .await
            .map_err(|_| invalid("Shell completion unavailable"))?;
    };
    // Even after a save failure, retain the shell until its supervisor has
    // terminated and reaped the process and drained its output pumps.
    if let Some(error) = save_failure {
        return Err(error);
    }
    if completion.state == ShellState::Failed || !completion.output_complete {
        control.diagnostics.record(
            crate::diagnostics::DiagnosticEvent::ShellFailure,
            crate::diagnostics::DiagnosticIds {
                thread_id: Some(snapshot.thread.clone()),
                run_id: Some(snapshot.run.clone()),
                turn_id: Some(snapshot.turn.clone()),
                execution_id: Some(execution.into()),
            },
        );
    }
    save_completion(control, snapshot, execution, &completion).await?;
    Ok(completion)
}

async fn save_completion(
    control: &Control,
    snapshot: &Snapshot,
    execution: &str,
    completion: &ShellCompletion,
) -> Result<(), ApiError> {
    let current = snapshot.clone();
    let execution = execution.to_owned();
    let completed = completion.clone();
    let index = control.index.clone();
    control.store.database.call(move|db|{let tx=db.transaction()?;let state=serde_json::to_value(completed.state).unwrap_or(Value::Null);
        if tx.execute("UPDATE shell_execution SET state=?2,exit_code=?3,end_reason=?4,output_complete=?5,ended_at=?6,shell_json=json_set(shell_json,'$.completion',json(?7)) WHERE id=?1 AND state IN ('starting','running','stopping')",params![execution,state.as_str().unwrap_or("failed"),completed.exit_code,completed.reason,completed.output_complete,now(),json!({"forced":completed.forced,"exit_signal":completed.exit_signal,"group_cleanup_confirmed":completed.group_cleanup_confirmed,"elapsed_ms":completed.elapsed_ms}).to_string()])? == 0 {
            let saved:Option<String> = tx.query_row("SELECT state FROM shell_execution WHERE id=?1",[&execution],|r|r.get(0)).optional()?;
            let saved=saved.ok_or_else(||ApiError::new(ErrorCode::TargetUnavailable,"Shell execution no longer exists"))?;
            let saved:ShellState=serde_json::from_value(Value::String(saved)).map_err(|_|ApiError::new(ErrorCode::InvalidState,"Unknown saved shell state"))?;
            if matches!(saved,ShellState::Exited|ShellState::Failed|ShellState::Stopped) {return Ok(());}
            return Err(ApiError::new(ErrorCode::OperationConflict,"Shell completion is no longer current").into());
        }
        let nodes:Vec<String>=tx.prepare("SELECT r.node_id FROM tool_result r JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id WHERE c.operation_id=?1")?.query_map([&execution],|r|r.get(0))?.collect::<Result<_,_>>()?;
        index.authorize_new_tx(&tx,&nodes).map_err(crate::storage::StorageError::from)?;
        let event=format!("shell-ended-{execution}");
        tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,target_run_id,payload_ref_json,created_at)VALUES(?1,'shell_ended',?2,?3,?4,?5,?6)",params![event,json!({"execution_id":execution}).to_string(),current.thread,current.run,json!({"execution_id":execution,"state":completed.state,"exit_code":completed.exit_code,"forced":completed.forced,"exit_signal":completed.exit_signal,"group_cleanup_confirmed":completed.group_cleanup_confirmed,"elapsed_ms":completed.elapsed_ms,"output_complete":completed.output_complete}).to_string(),now()])?;
        tx.execute("INSERT INTO delivery(event_id,recipient_id,state)VALUES(?1,?2,'pending')",params![event,current.run])?;
        tx.commit()?;Ok(())}).await.map_err(storage_error)?;
    control.index.wake();
    Ok(())
}

#[cfg(all(test, unix))]
mod output_batch_tests {
    use super::*;
    use crate::{config::Config, shell::ShellSupervisor};
    use futures_util::FutureExt;
    use std::{collections::BTreeMap, time::Duration};

    #[tokio::test]
    async fn batches_carry_original_chunks_without_crossing_the_byte_bound() {
        for (read_bound, batch_bound, expected_sizes, delay) in [
            (5, 8, vec![5, 7], Duration::from_secs(1)),
            (1, 1, vec![1; 12], Duration::from_secs(1)),
            (5, 8, vec![5, 7], Duration::from_millis(u64::MAX)),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut config = Config::default();
            config.io.output_batch_bytes = read_bound;
            config.io.output_buffer_bytes = 16;
            let args = serde_json::from_value(json!({"command":"printf abcdefghijkl"})).unwrap();
            let mut shell = ShellSupervisor::new(&config)
                .spawn(
                    "batch-bytes".into(),
                    directory.path().into(),
                    args,
                    BTreeMap::new(),
                )
                .await
                .unwrap();
            let mut pending = None;
            let mut bytes = Vec::new();
            let mut sequence = 0;
            let mut sizes = Vec::new();
            while let Some(batch) =
                output_batch(&mut shell.events, &mut pending, batch_bound, delay).await
            {
                let size: usize = batch.iter().map(|chunk| chunk.bytes.len()).sum();
                assert!(size as u64 <= batch_bound);
                sizes.push(size);
                for chunk in batch {
                    assert!(
                        chunk.bytes.len() as u64 <= read_bound,
                        "producer ignored the configured read bound"
                    );
                    assert_eq!(chunk.sequence, sequence);
                    sequence += 1;
                    bytes.extend_from_slice(&chunk.bytes);
                }
            }
            assert_eq!(bytes, b"abcdefghijkl");
            assert_eq!(sizes, expected_sizes);
            let completion = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Some(done) = shell.completion.borrow().clone() {
                        break done;
                    }
                    shell.completion.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            assert!(completion.output_complete);
        }
    }

    #[tokio::test]
    async fn sparse_live_output_flushes_at_the_configured_time_bound() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::default();
        let args = serde_json::from_value(
            json!({"command":"printf timer; read -r done","stdin_open":true}),
        )
        .unwrap();
        let mut shell = ShellSupervisor::new(&config)
            .spawn(
                "batch-time".into(),
                directory.path().into(),
                args,
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let ShellEvent::Output(first) =
            tokio::time::timeout(Duration::from_secs(1), shell.events.recv())
                .await
                .unwrap()
                .unwrap();
        let mut pending = Some(first);
        let delay = Duration::from_millis(30);
        let started = tokio::time::Instant::now();
        let mut batch = Box::pin(output_batch(
            &mut shell.events,
            &mut pending,
            64 * 1024,
            delay,
        ));
        assert!(
            batch.as_mut().now_or_never().is_none(),
            "a sparse batch flushed immediately"
        );
        let batch = tokio::time::timeout(Duration::from_secs(1), batch)
            .await
            .unwrap()
            .unwrap();
        assert!(started.elapsed() >= delay);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].bytes, b"timer");
        drop(batch);
        shell.handle.eof().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while shell.events.recv().await.is_some() {}
            while shell.completion.borrow().is_none() {
                shell.completion.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
}

#[cfg(all(test, unix))]
mod completion_tests;

#[cfg(all(test, unix))]
mod drain_tests;

#[cfg(all(test, unix))]
mod duration_tests;
