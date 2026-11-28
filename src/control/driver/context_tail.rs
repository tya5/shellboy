//! Request-local context commands, owned until native cleanup completes.
use super::*;
use crate::shell::{OutputStream, ShellEvent};
use serde::Serialize;
use std::time::Duration;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Serialize)]
struct ResultEntry {
    name: String,
    outcome: Outcome,
    stdout: String,
    truncated: bool,
}

pub(super) async fn collect(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    cancel: &CancellationToken,
    start_gate: Arc<Mutex<bool>>,
) -> Result<Option<Value>, ApiError> {
    let thread = snapshot.thread.clone();
    let definitions = control
        .store
        .database
        .call(move |db| {
            super::super::context_command::definitions(db, &thread).map_err(StorageError::from)
        })
        .await
        .map_err(storage_error)?;
    if definitions.is_empty() || cancel.is_cancelled() {
        return Ok(None);
    }
    let limit =
        usize::try_from(control.store.config.limits.tool_excerpt_bytes).unwrap_or(usize::MAX);
    use futures_util::{StreamExt, stream::FuturesOrdered};
    let mut commands: FuturesOrdered<_> = definitions
        .into_iter()
        .map(|definition| {
            run(
                control,
                snapshot,
                cancel,
                start_gate.clone(),
                definition,
                limit,
            )
        })
        .collect();
    let envelope = item(ENVELOPE.to_owned() + SUFFIX);
    let base = serde_json::to_vec(&envelope)
        .expect("context item serializes")
        .len();
    let mut remaining = limit.saturating_sub(base);
    let mut entries = Vec::new();
    let mut omitted = false;
    let mut shortened = false;
    while let Some(mut entry) = commands.next().await {
        if cancel.is_cancelled() || omitted {
            continue;
        }
        let comma = usize::from(!entries.is_empty());
        match fit_entry(&mut entry, remaining.saturating_sub(comma)) {
            Some(encoded) => {
                shortened |= entry.truncated;
                remaining -= escaped_size(&encoded) + comma;
                entries.push(encoded);
            }
            None => omitted = true,
        }
    }
    if cancel.is_cancelled() || base > limit {
        return Ok(None);
    }
    let fact = if omitted || shortened {
        "truncated"
    } else {
        "complete"
    };
    Ok(Some(item(format!(
        "{ENVELOPE}{}],\"outcome\":\"{fact}\"}}",
        entries.join(",")
    ))))
}

fn item(text: String) -> Value {
    json!({"type":"message","role":"developer","content":[{"type":"input_text","text":text}]})
}

async fn run(
    control: &Control,
    snapshot: &Snapshot,
    cancel: &CancellationToken,
    start_gate: Arc<Mutex<bool>>,
    definition: super::super::context_command::Definition,
    limit: usize,
) -> ResultEntry {
    let mut env = shell_output::cli_environment(control, &snapshot.thread);
    env.insert("SHELLBOY_RUN_ID".into(), snapshot.run.clone());
    env.insert("SHELLBOY_TURN_ID".into(), snapshot.turn.clone());
    let args = crate::protocol::ShellToolArgs {
        command: definition.command,
        description: None,
        background: false,
        stdin_open: false,
        initial_stdin: None,
        input_encoding: "utf-8".into(),
        output_encoding: "utf-8".into(),
    };
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(definition.timeout_ms.into());
    let execution = control
        .shell
        .spawn_guarded(
            uuid::Uuid::new_v4().to_string(),
            snapshot.cwd.clone(),
            args,
            env,
            start_gate,
        )
        .await;
    let mut bytes = Vec::new();
    let mut truncated = false;
    let outcome = match execution {
        Ok(mut child) => {
            let stopped = loop {
                tokio::select! { biased;
                    _ = cancel.cancelled() => break Some(Outcome::Cancelled),
                    _ = tokio::time::sleep_until(deadline) => break Some(Outcome::TimedOut),
                    event = child.events.recv() => match event {
                        Some(ShellEvent::Output(chunk)) if chunk.stream == OutputStream::Stdout => {
                            let keep = chunk.bytes.len().min(limit - bytes.len());
                            bytes.extend_from_slice(&chunk.bytes[..keep]);
                            truncated |= keep != chunk.bytes.len();
                        }
                        Some(_) => {},
                        None => break None,
                    }
                }
            };
            if stopped.is_some() {
                child.handle.stop();
            }
            // Release buffered output permits even when a command is stopping.
            drop(child.events);
            let completion = child
                .completion
                .wait_for(|done| done.is_some())
                .await
                .ok()
                .and_then(|done| done.clone());
            stopped.unwrap_or_else(|| match completion {
                Some(done)
                    if done.exit_code == Some(0)
                        && done.output_complete
                        && done.group_cleanup_confirmed =>
                {
                    Outcome::Ok
                }
                _ => Outcome::Failed,
            })
        }
        Err(_) if cancel.is_cancelled() => Outcome::Cancelled,
        Err(_) => Outcome::Failed,
    };
    ResultEntry {
        name: definition.name,
        outcome,
        stdout: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    }
}

// Reserve the larger outcome spelling, so truncation never grows the envelope.
const ENVELOPE: &str = "{\"type\":\"context_commands\",\"results\":[";
const SUFFIX: &str = "],\"outcome\":\"truncated\"}";

fn escaped_size(text: &str) -> usize {
    serde_json::to_vec(text)
        .expect("context text serializes")
        .len()
        - 2
}

fn fit_entry(entry: &mut ResultEntry, limit: usize) -> Option<String> {
    let encode =
        |entry: &ResultEntry| serde_json::to_string(entry).expect("context result serializes");
    let encoded = encode(entry);
    if escaped_size(&encoded) <= limit {
        return Some(encoded);
    }
    entry.truncated = true;
    let stdout = std::mem::take(&mut entry.stdout);
    let mut best = encode(entry);
    if escaped_size(&best) > limit {
        return None;
    }
    let (mut low, mut high) = (1, stdout.len());
    while low <= high {
        let mid = low + (high - low) / 2;
        entry.stdout = stdout[..stdout.floor_char_boundary(mid)].to_owned();
        let encoded = encode(entry);
        if escaped_size(&encoded) <= limit {
            best = encoded;
            low = mid + 1;
        } else {
            high = mid - 1;
        }
    }
    Some(best)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::control::driver::preparation_tests::{close, fixture, ready};

    #[tokio::test]
    async fn context_definitions_supply_fresh_ordered_stdout_and_failure() {
        let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1").await;
        let snapshot = ready(&control, &accepted).await;
        let cancel = CancellationToken::new();
        assert!(
            collect(&control, &snapshot, &cancel, Arc::default())
                .await
                .unwrap()
                .is_none()
        );
        let thread = accepted.thread_id.clone();
        control
            .store
            .database
            .call(move |db| {
                for (name, command) in [("z", "printf '%s' \"$SHELLBOY_TURN_ID\""), ("a", "exit 1")]
                {
                    super::super::super::context_command::add(
                        db,
                        &thread,
                        &crate::protocol::ContextAddArgs {
                            thread_id: None,
                            name: name.into(),
                            command: command.into(),
                            timeout_ms: 10_000,
                        },
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let item = collect(&control, &snapshot, &cancel, Arc::default())
            .await
            .unwrap()
            .unwrap();
        let text = item["content"][0]["text"].as_str().unwrap();
        let result: Value = serde_json::from_str(text).unwrap();
        assert_eq!(result["results"][0]["name"], "z");
        assert_eq!(result["results"][0]["outcome"], "ok");
        assert_eq!(result["results"][0]["stdout"], snapshot.turn);
        assert_eq!(result["results"][1]["outcome"], "failed");
        close(control, owner).await;
    }
}

#[cfg(all(test, unix))]
mod request_tests;
