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
    #[serde(skip_serializing_if = "Option::is_none")]
    started: Option<bool>,
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
    let turn = snapshot.turn.clone();
    let (definitions, request_index) = control
        .store
        .database
        .call(move |db| {
            let definitions = super::super::context_command::definitions(db, &thread)?;
            // Every adopted tool group starts one continuation; retry attempts do not.
            let request_index = if definitions.is_empty() {
                0
            } else {
                db.query_row(
                    "SELECT count(*) FROM tool_group WHERE turn_id=?1",
                    [&turn],
                    |row| row.get::<_, i64>(0),
                )?
            };
            Ok::<_, StorageError>((definitions, request_index))
        })
        .await
        .map_err(storage_error)?;
    if definitions.is_empty() || cancel.is_cancelled() {
        return Ok(None);
    }
    let limit =
        usize::try_from(control.store.config.limits.tool_excerpt_bytes).unwrap_or(usize::MAX);
    let reserved = serde_json::to_vec(&tail(
        definitions.iter().map(|def| ResultEntry {
            name: def.name.clone(),
            outcome: Outcome::Cancelled,
            started: Some(false),
            stdout: String::new(),
            truncated: false,
        }),
        true,
    ))
    .expect("context metadata serializes")
    .len();
    let Some(available) = limit.checked_sub(reserved) else {
        return Ok(Some(item(SKIPPED.to_owned())));
    };
    let share = available / definitions.len();
    let results = futures_util::future::join_all(definitions.into_iter().map(|definition| {
        run(
            control,
            snapshot,
            cancel,
            start_gate.clone(),
            definition,
            share,
            request_index,
        )
    }))
    .await;
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let truncated = results.iter().any(|entry| entry.truncated);
    Ok(Some(tail(results.into_iter(), truncated)))
}

fn tail(entries: impl Iterator<Item = ResultEntry>, truncated: bool) -> Value {
    item(
        json!({"type":"context_commands", "results": entries.collect::<Vec<_>>(),
        "outcome": if truncated { "truncated" } else { "complete" }})
        .to_string(),
    )
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
    share: usize,
    request_index: i64,
) -> ResultEntry {
    let mut env = shell_output::cli_environment(control, &snapshot.thread);
    env.insert("SHELLBOY_RUN_ID".into(), snapshot.run.clone());
    env.insert("SHELLBOY_TURN_ID".into(), snapshot.turn.clone());
    env.insert("SHELLBOY_REQUEST_INDEX".into(), request_index.to_string());
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
            Some(deadline),
        )
        .await;
    let started = execution.is_ok();
    let mut stdout = Excerpt::new(share);
    let outcome = match execution {
        Ok(mut child) => {
            let stopped = loop {
                tokio::select! { biased;
                    _ = cancel.cancelled() => break Some(Outcome::Cancelled),
                    _ = tokio::time::sleep_until(deadline) => break Some(Outcome::TimedOut),
                    event = child.events.recv() => match event {
                        Some(ShellEvent::Output(chunk)) if chunk.stream == OutputStream::Stdout => {
                            stdout.push(&chunk.bytes);
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
        Err(crate::shell::SpawnRefusal::Deadline) => Outcome::TimedOut,
        Err(_) => Outcome::Failed,
    };
    stdout.finish();
    ResultEntry {
        name: definition.name,
        started: (started || matches!(outcome, Outcome::TimedOut)).then_some(started),
        outcome,
        stdout: stdout.text,
        truncated: stdout.truncated,
    }
}

const SKIPPED: &str = r#"{"context":"skipped","reason":"budget_too_small"}"#;

pub(crate) fn minimum_budget() -> u64 {
    serde_json::to_vec(&item(SKIPPED.to_owned()))
        .expect("skip item serializes")
        .len() as u64
}

struct Excerpt {
    text: String,
    pending: Vec<u8>,
    remaining: usize,
    truncated: bool,
}

impl Excerpt {
    fn new(remaining: usize) -> Self {
        Self {
            text: String::new(),
            pending: Vec::new(),
            remaining,
            truncated: false,
        }
    }

    fn append(&mut self, text: &str) {
        for ch in text.chars() {
            // Excerpt text is a JSON string inside another JSON string.
            let cost = match ch {
                '"' | '\\' => 4,
                '\x08' | '\x0c' | '\n' | '\r' | '\t' => 3,
                '\x00'..='\x1f' => 7,
                _ => ch.len_utf8(),
            };
            let Some(remaining) = self.remaining.checked_sub(cost) else {
                self.truncated = true;
                break;
            };
            self.remaining = remaining;
            self.text.push(ch);
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.truncated {
            return;
        }
        let mut pending = std::mem::take(&mut self.pending);
        let mut bytes = if pending.is_empty() {
            bytes
        } else {
            pending.extend_from_slice(bytes);
            &pending
        };
        while !self.truncated {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    self.append(text);
                    break;
                }
                Err(error) => {
                    let (valid, rest) = bytes.split_at(error.valid_up_to());
                    self.append(std::str::from_utf8(valid).expect("valid UTF-8 prefix"));
                    if self.truncated {
                        break;
                    }
                    if let Some(invalid) = error.error_len() {
                        self.append("\u{fffd}");
                        bytes = &rest[invalid..];
                    } else {
                        self.pending.extend_from_slice(rest);
                        break;
                    }
                }
            }
        }
    }

    fn finish(&mut self) {
        if !self.pending.is_empty() && !self.truncated {
            self.append("\u{fffd}");
        }
    }
}

#[cfg(all(test, unix))]
mod request_tests;
