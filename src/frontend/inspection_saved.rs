//! Two saved, human inspection targets; no command replay or runtime effects.
use crate::{
    context::ExecutionContext,
    protocol::{
        ApiError, Assignment, Command, DeliveryStatus, ErrorCode, MessageDetails, MessageSender,
        ShellDetails, ShellState,
    },
    storage::StorageError,
};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{Value, json};

const JSON_BYTES: usize = 64 * 1024;
const SCALAR_BYTES: usize = 512;
#[derive(Debug)]
pub(super) enum RawSaved {
    Shell(RawShell),
    Message(RawMessage),
}
#[derive(Debug)]
pub(super) struct PreparedSaved {
    pub value: Value,
    pub thread: Option<String>,
    pub limited_fields: Vec<String>,
    pub observed_at: String,
}
#[derive(Debug)]
pub(super) struct RawShell {
    id: String,
    run: String,
    state: String,
    exit: Option<i32>,
    reason: Option<String>,
    complete: bool,
    encoding: Option<String>,
    cwd: Option<String>,
    thread: String,
    turn: String,
    started: Option<String>,
    last_output: Option<String>,
    shell_json: Option<String>,
    limited: Vec<String>,
    observed: DateTime<Utc>,
}
#[derive(Debug)]
pub(super) struct RawMessage {
    id: String,
    thread: String,
    state: String,
    run: Option<String>,
    turn: Option<String>,
    sender: Option<String>,
    assignment: Option<String>,
    created: Option<String>,
    updated: Option<String>,
    hold: Option<String>,
    limited: Vec<String>,
    observed: DateTime<Utc>,
}
pub(super) fn supports(command: &Command) -> bool {
    matches!(command, Command::ShellGet(_) | Command::MessageGet(_))
}
pub(super) async fn request(
    control: &std::sync::Arc<crate::control::Control>,
    command: Command,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Value, ApiError> {
    if !supports(&command) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Not a saved metadata inspection",
        ));
    }
    command.validate()?;
    let raw = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop,
        move |db| read_tx(db, &command, None),
    )
    .await?;
    let prepared = super::presentation_work(move || prepare(raw)).await??;
    let mut value = prepared.value;
    value["view_observation"] =
        json!({"observed_at":prepared.observed_at,"thread_id":prepared.thread});
    if !prepared.limited_fields.is_empty() {
        value["view_limits"] = json!({"limited_fields":prepared.limited_fields});
    }
    Ok(value)
}

fn domain(code: ErrorCode, message: &str) -> StorageError {
    StorageError::from(ApiError::new(code, message))
}
fn id(value: &str) -> Result<(), StorageError> {
    if value.is_empty() || value.len() > 4096 {
        return Err(domain(
            ErrorCode::InvalidArgument,
            "Inspection identity must contain1..4096 bytes",
        ));
    }
    Ok(())
}
fn fields(mask: i64, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1i64 << i) != 0)
        .map(|(_, name)| (*name).into())
        .collect()
}
fn required(value: Option<String>, name: &str) -> Result<String, StorageError> {
    value.ok_or_else(||domain(ErrorCode::InputLimit,&format!("Saved {name} exceeds its {SCALAR_BYTES}-byte scalar inspection limit; use its full CLI get command")))
}

const SHELL_SQL:&str="SELECT
 CASE WHEN octet_length(s.owner_run_id)<=512 THEN s.owner_run_id END,
 CASE WHEN octet_length(s.state)<=64 THEN s.state END,s.exit_code,
 CASE WHEN octet_length(s.end_reason)<=512 THEN s.end_reason END,COALESCE(s.output_complete,0),
 CASE WHEN octet_length(s.output_encoding)<=64 THEN s.output_encoding END,
 CASE WHEN octet_length(s.cwd)<=512 THEN s.cwd END,
 CASE WHEN octet_length(r.thread_id)<=512 THEN r.thread_id END,
 CASE WHEN octet_length(s.turn_id)<=512 THEN s.turn_id END,
 CASE WHEN octet_length(s.started_at)<=512 THEN s.started_at END,
 CASE WHEN octet_length(s.shell_json)<=65536 THEN s.shell_json END,
 CASE WHEN octet_length(c.received_at)<=512 THEN c.received_at END,
 COALESCE(octet_length(s.end_reason)>512,0)
 +2*(octet_length(s.output_encoding)>64)+4*(octet_length(s.cwd)>512)
 +8*(octet_length(s.started_at)>512)+16*(octet_length(s.shell_json)>65536)
 +32*COALESCE(octet_length(c.received_at)>512,0)
 FROM shell_execution s JOIN run r ON r.id=s.owner_run_id
 LEFT JOIN output_chunk c ON c.rowid=(SELECT rowid FROM output_chunk WHERE execution_id=s.id ORDER BY sequence DESC LIMIT 1)
 WHERE s.id=?1";
const MESSAGE_SQL:&str="SELECT
 CASE WHEN octet_length(r.target_thread_id)<=512 THEN r.target_thread_id END,
 CASE WHEN octet_length(r.state)<=64 THEN r.state END,
 CASE WHEN octet_length(r.run_id)<=512 THEN r.run_id END,
 CASE WHEN octet_length(r.turn_id)<=512 THEN r.turn_id END,
 NULL,
 CASE WHEN octet_length(r.sender_json)<=65536 THEN r.sender_json END,
 CASE WHEN octet_length(r.result_json)<=65536-CASE WHEN octet_length(r.sender_json)<=65536 THEN octet_length(r.sender_json) ELSE 0 END THEN r.result_json END,
 CASE WHEN octet_length(r.created_at)<=512 THEN r.created_at END,
 CASE WHEN octet_length(r.updated_at)<=512 THEN NULLIF(r.updated_at,'') END,
 CASE WHEN octet_length(r.hold_reason)<=512 THEN r.hold_reason END,
 COALESCE(octet_length(r.run_id)>512,0)+2*COALESCE(octet_length(r.turn_id)>512,0)
 +8*(octet_length(r.sender_json)>65536)
 +16*COALESCE(octet_length(r.result_json)>65536-CASE WHEN octet_length(r.sender_json)<=65536 THEN octet_length(r.sender_json) ELSE 0 END,0)
 +32*(octet_length(r.created_at)>512)+64*(octet_length(r.updated_at)>512)+128*COALESCE(octet_length(r.hold_reason)>512,0)
 FROM receipt r LEFT JOIN turn t ON t.id=r.turn_id WHERE r.message_node_id=?1";

/// Closed allowlist checked before touching SQLite. `default_thread` is issuing
/// context, never authority to retarget an explicit execution/message/Run.
pub(super) fn read_tx(
    db: &Connection,
    command: &Command,
    _default_thread: Option<&str>,
) -> Result<RawSaved, StorageError> {
    match command {
        Command::ShellGet(args) => {
            id(&args.execution_id)?;
            let row = db
                .query_row(SHELL_SQL, [&args.execution_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                        r.get(9)?,
                        r.get(10)?,
                        r.get(11)?,
                        r.get::<_, i64>(12)?,
                    ))
                })
                .optional()?
                .ok_or_else(|| domain(ErrorCode::TargetUnavailable, "shell execution not found"))?;
            let limited = fields(
                row.12,
                &[
                    "/reason",
                    "/output_encoding",
                    "/cwd",
                    "/started_at",
                    "/shell_json",
                    "/last_output_at",
                ],
            );
            Ok(RawSaved::Shell(RawShell {
                id: args.execution_id.clone(),
                run: required(row.0, "shell Run identity")?,
                state: required(row.1, "shell state")?,
                exit: row.2,
                reason: row.3,
                complete: row.4,
                encoding: row.5,
                cwd: row.6,
                thread: required(row.7, "shell Thread identity")?,
                turn: required(row.8, "shell Turn identity")?,
                started: row.9,
                shell_json: row.10,
                last_output: row.11,
                limited,
                observed: Utc::now(),
            }))
        }
        Command::MessageGet(args) => {
            id(&args.message_id)?;
            let row = db
                .query_row(MESSAGE_SQL, [&args.message_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get::<_, i64>(10)?,
                    ))
                })
                .optional()?
                .ok_or_else(|| domain(ErrorCode::TargetUnavailable, "message not found"))?;
            if row.10 & 7 != 0 {
                return Err(domain(
                    ErrorCode::InputLimit,
                    "Saved message assignment identity exceeds its inspection limit; inspect the full CLI result",
                ));
            }
            let limited = fields(
                row.10 >> 3,
                &[
                    "/sender",
                    "/assignment",
                    "/context/created_at",
                    "/context/updated_at",
                    "/context/hold_reason",
                ],
            );
            Ok(RawSaved::Message(RawMessage {
                id: args.message_id.clone(),
                thread: required(row.0, "message Thread identity")?,
                state: required(row.1, "message delivery state")?,
                run: row.2,
                turn: row.3,
                sender: row.5,
                assignment: row.6,
                created: row.7,
                updated: row.8,
                hold: row.9,
                limited,
                observed: Utc::now(),
            }))
        }
        _ => Err(domain(
            ErrorCode::InvalidArgument,
            "Saved inspection supports only shell get and message get",
        )),
    }
}
#[derive(Deserialize)]
struct SavedAssignment {
    assignment: Assignment,
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, message)
}
fn encode<T: serde::Serialize>(value: T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|_| invalid("Cannot encode saved inspection"))
}
pub(super) fn prepare(raw: RawSaved) -> Result<PreparedSaved, ApiError> {
    match raw {
        RawSaved::Shell(mut raw) => {
            debug_assert!(
                raw.shell_json
                    .as_ref()
                    .is_none_or(|body| body.len() <= JSON_BYTES)
            );
            let state: ShellState = serde_json::from_value(json!(raw.state))
                .map_err(|_| invalid("stored shell state is invalid"))?;
            if let Some(encoding) = &raw.encoding {
                crate::control::output::canonical_encoding(encoding)
                    .map_err(|_| invalid("stored shell encoding is unsupported"))?;
            }
            let shell = raw
                .shell_json
                .as_deref()
                .map(serde_json::from_str::<Value>)
                .transpose()
                .map_err(|_| invalid("stored shell metadata is malformed"))?;
            let completion = shell.as_ref().and_then(|v| v.get("completion"));
            let forced = completion
                .and_then(|v| v.get("forced"))
                .and_then(Value::as_bool);
            let exit_signal = completion
                .and_then(|v| v.get("exit_signal"))
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok());
            let confirmed = completion
                .and_then(|v| v.get("group_cleanup_confirmed"))
                .and_then(Value::as_bool);
            let elapsed = if matches!(
                state,
                ShellState::Starting | ShellState::Running | ShellState::Stopping
            ) {
                raw.started
                    .as_ref()
                    .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                    .map(|at| {
                        raw.observed
                            .signed_duration_since(at)
                            .num_milliseconds()
                            .max(0) as u64
                    })
            } else {
                completion
                    .and_then(|v| v.get("elapsed_ms"))
                    .and_then(Value::as_i64)
                    .and_then(|n| u64::try_from(n).ok())
            };
            let command = shell
                .as_ref()
                .and_then(|v| v.get("command"))
                .filter(|v| !v.is_null())
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("stored shell command is not a string"))
                })
                .transpose()?;
            if raw.limited.iter().any(|p| p == "/shell_json") {
                raw.limited.extend(
                    [
                        "/command",
                        "/forced",
                        "/exit_signal",
                        "/group_cleanup_confirmed",
                    ]
                    .map(str::to_owned),
                );
                if !matches!(
                    state,
                    ShellState::Starting | ShellState::Running | ShellState::Stopping
                ) {
                    raw.limited.push("/elapsed_ms".into());
                }
            }
            let details = ShellDetails {
                execution_id: raw.id,
                run_id: raw.run,
                state,
                output_complete: raw.complete,
                started_at: raw.started,
                last_output_at: raw.last_output,
                forced,
                exit_signal,
                group_cleanup_confirmed: confirmed,
                elapsed_ms: elapsed,
                exit_code: raw.exit,
                reason: raw.reason,
                command,
                cwd: raw.cwd,
                thread_id: Some(raw.thread.clone()),
                turn_id: Some(raw.turn),
            };
            Ok(PreparedSaved {
                value: encode(details)?,
                thread: Some(raw.thread),
                limited_fields: raw.limited,
                observed_at: raw.observed.to_rfc3339(),
            })
        }
        RawSaved::Message(raw) => {
            debug_assert!(
                raw.sender.as_ref().map_or(0, String::len)
                    + raw.assignment.as_ref().map_or(0, String::len)
                    <= JSON_BYTES
            );
            let status: DeliveryStatus = serde_json::from_value(json!(raw.state))
                .map_err(|_| invalid("unknown saved delivery state"))?;
            let assignment = if raw.run.is_none() {
                Assignment::Pending
            } else {
                raw.assignment
                    .as_deref()
                    .and_then(|b| serde_json::from_str::<SavedAssignment>(b).ok())
                    .map(|saved| saved.assignment)
                    .filter(|a| *a != Assignment::Pending)
                    .unwrap_or(Assignment::ExistingTurn)
            };
            let assignment_limited =
                raw.run.is_some() && raw.limited.iter().any(|p| p == "/assignment");
            let details = MessageDetails {
                sender: MessageSender::from_saved(raw.sender.as_deref()),
                context: ExecutionContext {
                    thread_id: Some(raw.thread.clone()),
                    run_id: raw.run.clone(),
                    turn_id: raw.turn.clone(),
                    state: raw.state,
                    created_at: raw.created,
                    updated_at: raw.updated,
                    hold_reason: raw.hold,
                    node_id: Some(raw.id.clone()),
                    ..Default::default()
                },
                message_id: raw.id,
                thread_id: raw.thread.clone(),
                delivery_status: status,
                assignment,
                run_id: raw.run,
                turn_id: raw.turn,
                expect_reply: raw
                    .assignment
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .is_some_and(|v| v["expect_reply"] == true),
                reply: raw
                    .assignment
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .and_then(|v| v.get("reply").cloned()),
                reply_to: raw
                    .assignment
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .and_then(|v| v["reply_to"].as_str().map(str::to_owned)),
            };
            let mut value = encode(details)?;
            if assignment_limited {
                value["assignment"] = Value::Null;
            }
            Ok(PreparedSaved {
                value,
                thread: Some(raw.thread),
                limited_fields: raw.limited,
                observed_at: raw.observed.to_rfc3339(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use crate::protocol::{ExecutionTarget, MessageTarget, RunTarget};
    use rusqlite::{StatementStatus, params};
    fn seed(db: &Connection) -> rusqlite::Result<()> {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('target','/','','fixture','synthetic','2026-01-01T00:00:00Z'),('issuing','/','','fixture','synthetic','2026-01-01T00:00:00Z');
         INSERT INTO run(id,thread_id,state,created_at)VALUES('run','target','live','2026-01-01T00:00:00Z');
         INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','target','run','completed','2026-01-01T00:00:00Z');
         INSERT INTO node(id,kind,created_at,payload_json)VALUES('message','user','2026-01-01T00:00:00Z','{}'),('answer','assistant_response','2026-01-01T00:00:00Z','{}');
         UPDATE turn SET answer_node_id='answer' WHERE id='turn';
         INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,result_json)VALUES('receipt','fixture:message','{\"kind\":\"human\",\"label\":\"Author\"}','target','message','run','turn','applied','2026-01-01T00:00:00Z','{\"assignment\":\"new_turn\"}');
         INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,exit_code,output_complete)VALUES('shell','run','turn','shell','exited','/','{\"command\":\"printf done\",\"completion\":{\"elapsed_ms\":42,\"forced\":false,\"exit_signal\":15,\"group_cleanup_confirmed\":true}}','UTF-8','cp932','closed','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z',0,1);
         INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('attachment','message','/SHELLBOY_SYNTHETIC_NO_FILE_PROBE','file','external','2026-01-01T00:00:00Z');
         INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES('target','attachment','active','2026-01-01T00:00:00Z',0);
         INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('saved:operation','fixture','null','hash','completed','{\"result\":{\"message_id\":\"message\"}}','2026-01-01T00:00:00Z');
         INSERT INTO output_chunk VALUES('shell',1,'stdout',0,X'61','2026-01-01T00:00:00.5Z');")?;
        Ok(())
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        seed(&db).unwrap();
        db
    }
    fn message() -> Command {
        Command::MessageGet(MessageTarget {
            message_id: "message".into(),
        })
    }
    fn shell() -> Command {
        Command::ShellGet(ExecutionTarget {
            execution_id: "shell".into(),
        })
    }
    #[test]
    fn closed_allowlist_and_absent_human_run_reject_before_sql() {
        let db = fixture();
        let trace = Statements::new(&db);
        let bad = Command::RunGet(RunTarget {
            run_id: "run".into(),
        });
        assert_eq!(
            read_tx(&db, &bad, Some("issuing"))
                .unwrap_err()
                .into_api_error()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(trace.count(), 0);
    }
    #[test]
    fn message_current_assignment_answer_sender_and_context_match_canonical_even_after_tombstone() {
        let db = fixture();
        for deleted in [false, true] {
            if deleted {
                db.execute_batch("UPDATE run SET state='completed',ended_at='2026-01-01T00:00:01Z';UPDATE thread SET deleted_at='2026-01-01T00:00:02Z' WHERE id='target';UPDATE node SET payload_json=NULL,deleted_reason='explicit GC' WHERE id='message';").unwrap();
            }
            let trace = Statements::new(&db);
            let raw = read_tx(&db, &message(), Some("issuing")).unwrap();
            assert_eq!(trace.count(), 1);
            drop(trace);
            let expected =
                encode(crate::control::db::message_details(&db, "message").unwrap()).unwrap();
            let prepared = prepare(raw).unwrap();
            assert_eq!(prepared.value, expected);
            assert_eq!(prepared.thread.as_deref(), Some("target"));
            assert!(prepared.limited_fields.is_empty());
        }
    }
    #[tokio::test]
    async fn terminal_shell_matches_real_output_reader_and_keeps_exact_foreign_owner() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::storage::DbWorker::open(root.path().join("shell.db"))
            .await
            .unwrap();
        db.call(|db| seed(db).map_err(Into::into)).await.unwrap();
        let canonical = crate::control::output::ShellOutputReader::new(db.clone(), "fixture")
            .get("shell")
            .await
            .unwrap();
        let raw = db
            .call(|db| read_tx(db, &shell(), Some("issuing")))
            .await
            .unwrap();
        let prepared = prepare(raw).unwrap();
        assert_eq!(prepared.value, encode(canonical).unwrap());
        assert_eq!(prepared.thread.as_deref(), Some("target"));
        assert!(prepared.limited_fields.is_empty());
        assert!(!prepared.observed_at.is_empty());
        db.shutdown().await.unwrap();
    }
    #[test]
    fn shared_json_budget_preserves_known_message_state_without_inventing_assignment() {
        let db = fixture();
        let body = json!({"assignment":"new_turn","padding":"x".repeat(JSON_BYTES)}).to_string();
        db.execute("UPDATE receipt SET result_json=?1", [body])
            .unwrap();
        let raw = read_tx(&db, &message(), None).unwrap();
        let RawSaved::Message(row) = &raw else {
            panic!()
        };
        assert!(row.assignment.is_none());
        assert!(row.sender.as_ref().unwrap().len() <= JSON_BYTES);
        let prepared = prepare(raw).unwrap();
        assert!(prepared.value["assignment"].is_null());
        assert_eq!(prepared.value["delivery_status"], "applied");
        assert_eq!(prepared.value["answer_node_id"], "answer");
        assert!(prepared.limited_fields.iter().any(|p| p == "/assignment"));
        let sender =
            json!({"kind":"human","label":"Author","padding":"x".repeat(JSON_BYTES)}).to_string();
        db.execute(
            "UPDATE receipt SET sender_json=?1,result_json='{\"assignment\":\"new_turn\"}'",
            [sender],
        )
        .unwrap();
        let raw = read_tx(&db, &message(), None).unwrap();
        let RawSaved::Message(row) = &raw else {
            panic!()
        };
        assert!(row.sender.is_none());
        assert!(row.assignment.as_ref().unwrap().len() <= JSON_BYTES);
        let prepared = prepare(raw).unwrap();
        assert_eq!(prepared.value["sender"]["kind"], "unknown");
        assert!(prepared.limited_fields.iter().any(|p| p == "/sender"));
    }
    #[test]
    fn shell_metadata_overflow_is_limited_while_primitive_completion_stays_visible() {
        let db = fixture();
        let body=json!({"command":"printf done","completion":{"elapsed_ms":42},"padding":"x".repeat(JSON_BYTES)}).to_string();
        db.execute(
            "UPDATE shell_execution SET shell_json=?1,end_reason=?2 WHERE id='shell'",
            params![body, "r".repeat(600)],
        )
        .unwrap();
        let raw = read_tx(&db, &shell(), None).unwrap();
        let RawSaved::Shell(row) = &raw else { panic!() };
        assert!(row.shell_json.is_none());
        assert!(row.reason.is_none());
        let prepared = prepare(raw).unwrap();
        assert_eq!(prepared.value["exit_code"], 0);
        assert_eq!(prepared.value["state"], "exited");
        assert!(prepared.value.get("elapsed_ms").is_none());
        assert!(prepared.limited_fields.iter().any(|p| p == "/shell_json"));
        assert!(prepared.limited_fields.iter().any(|p| p == "/elapsed_ms"));
    }
    #[test]
    fn shell_latest_output_lookup_work_does_not_grow_with_retained_output() {
        let mut db = fixture();
        db.execute(
            "INSERT INTO output_chunk VALUES('shell',2,'stdout',2,X'61','2025-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let received: String = db.query_row(SHELL_SQL, ["shell"], |r| r.get(11)).unwrap();
        assert_eq!(received, "2025-01-01T00:00:00Z");
        db.execute(
            "DELETE FROM output_chunk WHERE execution_id='shell' AND sequence=2",
            [],
        )
        .unwrap();
        let mut previous = 0;
        for total in [1000, 10000] {
            let tx = db.transaction().unwrap();
            for sequence in previous + 2..=total + 1 {
                tx.execute("INSERT INTO output_chunk VALUES('shell',?1,'stdout',?1,X'61','2026-01-01T00:00:00Z')",[sequence]).unwrap();
            }
            tx.commit().unwrap();
            previous = total;
            let trace = Statements::new(&db);
            assert!(matches!(
                read_tx(&db, &shell(), None).unwrap(),
                RawSaved::Shell(_)
            ));
            assert_eq!(trace.count(), 1);
            drop(trace);
            let mut q = db.prepare(SHELL_SQL).unwrap();
            {
                let mut rows = q.query(["shell"]).unwrap();
                assert!(rows.next().unwrap().is_some());
            }
            let steps = q.get_status(StatementStatus::VmStep);
            if total == 1000 {
                db.execute(
                    "INSERT INTO app_meta(key,value)VALUES('test.shell.vm',?1)",
                    [steps.to_string()],
                )
                .unwrap();
            } else {
                let first: i32 = db
                    .query_row(
                        "SELECT CAST(value AS INTEGER)FROM app_meta WHERE key='test.shell.vm'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert!(
                    steps <= first + 100,
                    "PK suffix lookup became linear: {first}->{steps}"
                );
            }
        }
        let mut q = db
            .prepare(&format!("EXPLAIN QUERY PLAN {SHELL_SQL}"))
            .unwrap();
        let plan = q
            .query_map(["shell"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|line| line.contains("output_chunk") && line.contains("INDEX")),
            "{plan:?}"
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_saved_gets_finish_while_writer_is_blocked_and_create_no_receipts() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n[embedding]\nconnection='fixture'\nmodel='synthetic'\n").unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = crate::control::Control::new(&owner, root.path().into());
        owner
            .db
            .call(|db| seed(db).map_err(Into::into))
            .await
            .unwrap();
        let before: i64 = owner
            .db
            .call(
                |db| Ok(db.query_row("SELECT count(*) FROM operation_receipt", [], |r| r.get(0))?),
            )
            .await
            .unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let blocked = owner
            .db
            .try_call(move |_| {
                let _ = entered.send(());
                let _ = gate.recv();
                Ok(())
            })
            .unwrap();
        crate::test_watchdog::watchdog("saved inspector blocked writer entered", ready)
            .await
            .unwrap()
            .unwrap();
        let observed =
            crate::test_watchdog::watchdog("saved gets independent of blocked writer", async {
                let mut values = Vec::new();
                for command in [
                    shell(),
                    message(),
                    Command::RunGet(crate::protocol::RunTarget {
                        run_id: "run".into(),
                    }),
                    Command::TurnGet(crate::protocol::TurnTarget {
                        turn_id: "turn".into(),
                    }),
                    Command::OperationGet(crate::protocol::OperationTarget {
                        operation_id: "saved:operation".into(),
                    }),
                    Command::AttachmentList(crate::protocol::ThreadTarget {
                        thread_id: Some("target".into()),
                    }),
                ] {
                    values.push(
                        super::super::management_request(
                            &control,
                            command,
                            tokio_util::sync::CancellationToken::new(),
                        )
                        .await
                        .unwrap(),
                    );
                }
                values.push(
                    super::super::inspection_attachments::request(
                        &control,
                        "target".into(),
                        Some("attachment".into()),
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                    .unwrap(),
                );
                let storage = super::super::management_request(
                    &control,
                    Command::StorageStatus(crate::protocol::StorageArgs { category: None }),
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
                .unwrap();
                let deletion = super::super::management_request(
                    &control,
                    Command::ThreadDeletePreview(crate::protocol::ThreadDeleteArgs {
                        preview_marker: None,
                        thread_id: "target".into(),
                        descendants: false,
                    }),
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
                .unwrap();
                let index = super::super::management_request(
                    &control,
                    Command::IndexStatus(crate::protocol::IndexArgs::default()),
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
                .unwrap();
                (values, storage, deletion, index)
            })
            .await;
        // Always release before asserting so the intentional red cannot strand
        // a fixture writer. The watchdog is a safety limit, not a latency gate.
        let _ = release.send(());
        blocked.receive().await.unwrap();
        let after: i64 = owner
            .db
            .call(
                |db| Ok(db.query_row("SELECT count(*) FROM operation_receipt", [], |r| r.get(0))?),
            )
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        let (values, storage, deletion, index) = observed.unwrap();
        assert_eq!(index["management"]["kind"], "saved_index_observation");
        assert!(index["management"]["spaces"].is_array());
        assert!(index["management"]["definitions"]["not_indexed_sources"].is_string());
        assert!(index["view_observation"]["observed_at"].is_string());
        assert!(index.get("request_operation_id").is_none());
        assert_eq!(deletion["thread_id"], "target");
        assert_eq!(deletion["snapshot_only"], true);
        assert_eq!(deletion["deletable_now"], false);
        assert_eq!(deletion["blockers"]["live_run_count"], 1);
        assert_eq!(deletion["view_observation"]["thread_id"], "target");
        assert!(storage["logical"]["bytes"].as_u64().is_some());
        assert_eq!(storage["physical_observation"]["available"], true);
        assert!(storage["view_observation"].get("thread_id").is_none());
        assert_eq!(before, after);
        assert_eq!(values[0]["execution_id"], "shell");
        assert_eq!(values[0]["command"], "printf done");
        assert_eq!(values[1]["message_id"], "message");
        assert_eq!(values[2]["run_id"], "run");
        assert_eq!(values[2]["usage_totals"]["scope"], "run");
        assert_eq!(values[2]["usage_totals"]["scope_id"], "run");
        assert_eq!(values[3]["turn_id"], "turn");
        assert_eq!(values[3]["usage_totals"]["scope"], "turn");
        assert_eq!(values[3]["usage_totals"]["scope_id"], "turn");
        assert_eq!(values[4]["operation_id"], "saved:operation");
        assert_eq!(values[4]["downstream"]["state"], "applied");
        assert_eq!(values[4]["current_instance"], false);
        assert_eq!(values[5]["attachments"][0]["attachment_id"], "attachment");
        assert_eq!(values[5]["scope"]["kind"], "thread");
        assert_eq!(values[6]["scope"]["kind"], "attachment");
        assert_eq!(values[6]["scope"]["selection_found"], true);
        assert!(
            values[6]["notice"]
                .as_str()
                .unwrap()
                .contains("not been checked")
        );
        for value in values {
            assert_eq!(value["view_observation"]["thread_id"], "target");
            assert!(
                value["view_observation"]["observed_at"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
            assert!(value.get("request_operation_id").is_none());
        }
    }
}
