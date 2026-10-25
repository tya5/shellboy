//! Bounded raw-output retrieval with per-stream incremental decoding. Decoder
//! state is rebuilt from retained raw pages, never serialized or guessed.
use crate::{
    protocol::{
        ApiError, ErrorCode, MAX_BODY_BYTES, ShellDetails, ShellOutput, ShellReadArgs, ShellState,
    },
    storage::{DbWorker, StorageError},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use encoding_rs::{Decoder, Encoding};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

type Result<T> = std::result::Result<T, ApiError>;
const RAW_PAGE: usize = 64 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamSelection {
    Both,
    Stdout,
    Stderr,
}
impl StreamSelection {
    fn includes(self, stream: &str) -> bool {
        match self {
            Self::Both => true,
            Self::Stdout => stream == "stdout",
            Self::Stderr => stream == "stderr",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputCursor {
    pub version: u32,
    pub execution_id: String,
    pub sequence: u64,
    pub byte_offset: u64,
    pub retention_generation: u64,
    pub encoding: String,
    pub streams: StreamSelection,
    pub eof_flushed: bool,
}
impl OutputCursor {
    pub fn encode(&self) -> Result<String> {
        serde_json::to_vec(self)
            .map(|v| URL_SAFE_NO_PAD.encode(v))
            .map_err(|_| error(ErrorCode::CursorExpired, "cannot encode output cursor"))
    }
    pub fn decode(token: &str) -> Result<Self> {
        if token.len() > 8192 {
            return Err(expired_cursor());
        }
        let value: Self = URL_SAFE_NO_PAD
            .decode(token)
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .ok_or_else(expired_cursor)?;
        if value.version != 1
            || value.sequence > i64::MAX as u64
            || value.byte_offset > i64::MAX as u64
        {
            return Err(expired_cursor());
        }
        Ok(value)
    }
}
#[derive(Clone)]
pub struct ShellOutputReader {
    database: DbWorker,
    instance: Arc<str>,
}
#[derive(Clone)]
struct Snapshot {
    details: ShellDetails,
    saved: u64,
    generation: u64,
    encoding: String,
}
impl Snapshot {
    fn terminal(&self) -> bool {
        matches!(
            self.details.state,
            ShellState::Exited | ShellState::Failed | ShellState::Stopped
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    sequence: u64,
    offset: u64,
}
struct RawPage {
    stream: String,
    bytes: Vec<u8>,
    next: Position,
}
pub(crate) struct Decoders {
    stdout: Decoder,
    stderr: Decoder,
}
impl Decoders {
    pub(crate) fn new(encoding: &'static Encoding) -> Self {
        Self {
            stdout: encoding.new_decoder_without_bom_handling(),
            stderr: encoding.new_decoder_without_bom_handling(),
        }
    }
    pub(crate) fn decode(&mut self, stream: &str, bytes: &[u8], last: bool) -> (String, bool) {
        let decoder = if stream == "stdout" {
            &mut self.stdout
        } else {
            &mut self.stderr
        };
        let mut text = String::with_capacity(
            decoder
                .max_utf8_buffer_length(bytes.len())
                .unwrap_or(bytes.len() * 4 + 32),
        );
        let mut input = bytes;
        let mut errors = false;
        loop {
            let (result, read, bad) = decoder.decode_to_string(input, &mut text, last);
            errors |= bad;
            input = &input[read..];
            if result == encoding_rs::CoderResult::InputEmpty {
                break;
            }
            text.reserve(RAW_PAGE * 4 + 32);
        }
        (text, errors)
    }
    pub(crate) fn finish(&mut self, streams: StreamSelection) -> (String, bool) {
        let mut text = String::new();
        let mut errors = false;
        for stream in ["stdout", "stderr"] {
            if streams.includes(stream) {
                let (part, bad) = self.decode(stream, &[], true);
                text.push_str(&part);
                errors |= bad;
            }
        }
        (text, errors)
    }
}
#[derive(Debug, Clone)]
pub struct DecodedOutputPage {
    pub text: String,
    pub raw_bytes: u64,
    pub decode_errors: bool,
    pub done: bool,
}
pub struct OutputStreamReader {
    reader: ShellOutputReader,
    snapshot: Snapshot,
    pin: Option<ReadPin>,
    position: Position,
    streams: StreamSelection,
    decoders: Decoders,
    done: bool,
}
impl OutputStreamReader {
    pub fn retention_generation(&self) -> u64 {
        self.snapshot.generation
    }
    pub fn saved_sequence(&self) -> u64 {
        self.snapshot.saved
    }
    pub fn encoding(&self) -> &str {
        &self.snapshot.encoding
    }
    pub async fn next_page(&mut self, limit_bytes: u32) -> Result<Option<DecodedOutputPage>> {
        if limit_bytes == 0 || limit_bytes > MAX_BODY_BYTES {
            return Err(error(
                ErrorCode::InvalidArgument,
                "raw stream page limit must be in 1..=65536",
            ));
        }
        if self.done {
            return Ok(None);
        }
        let mut text = String::new();
        let mut raw_bytes = 0;
        let mut decode_errors = false;
        while self.position.sequence < self.snapshot.saved && raw_bytes < limit_bytes as u64 {
            let page = self
                .reader
                .page(
                    &self.snapshot.details.execution_id,
                    &self.snapshot,
                    self.position,
                    (limit_bytes as u64 - raw_bytes) as usize,
                )
                .await?;
            if self.streams.includes(&page.stream) {
                let (part, bad) = self.decoders.decode(&page.stream, &page.bytes, false);
                text.push_str(&part);
                decode_errors |= bad;
                raw_bytes += page.bytes.len() as u64;
            }
            self.position = page.next;
            tokio::task::yield_now().await;
        }
        let done = self.position.sequence == self.snapshot.saved;
        if done {
            if self.snapshot.terminal() {
                let (part, bad) = self.decoders.finish(self.streams);
                text.push_str(&part);
                decode_errors |= bad;
            }
            self.done = true;
            if let Some(mut pin) = self.pin.take() {
                pin.release().await?;
            }
        }
        Ok(Some(DecodedOutputPage {
            text,
            raw_bytes,
            decode_errors,
            done,
        }))
    }
}
impl ShellOutputReader {
    pub fn new(database: DbWorker, instance_id: impl Into<String>) -> Self {
        Self {
            database,
            instance: Arc::from(instance_id.into()),
        }
    }
    async fn db<T: Send + 'static, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        self.database
            .call(move |db| Ok(f(db)))
            .await
            .map_err(storage_error)?
    }
    pub async fn stream(&self, execution_id: &str) -> Result<OutputStreamReader> {
        self.stream_selected(execution_id, StreamSelection::Both)
            .await
    }
    pub async fn stream_selected(
        &self,
        execution_id: &str,
        streams: StreamSelection,
    ) -> Result<OutputStreamReader> {
        let pin = ReadPin::new(self.database.clone(), self.instance.to_string());
        let execution = execution_id.to_owned();
        let pin_id = pin.id.clone();
        let instance = pin.instance.clone();
        let state = self
            .db(move |db| {
                let tx = db.transaction().map_err(sql_error)?;
                let state = snapshot(&tx, &execution)?;
                ensure_raw_start(&tx, &state, &execution)?;
                ReadPin::insert_tx(&tx, &pin_id, &instance, &execution, state.generation)?;
                tx.commit().map_err(sql_error)?;
                Ok(state)
            })
            .await?;
        let decoders = Decoders::new(canonical_encoding(&state.encoding)?);
        Ok(OutputStreamReader {
            reader: self.clone(),
            snapshot: state,
            pin: Some(pin),
            position: Position {
                sequence: 0,
                offset: 0,
            },
            streams,
            decoders,
            done: false,
        })
    }
    pub async fn get(&self, execution_id: &str) -> Result<ShellDetails> {
        let execution = execution_id.to_owned();
        self.db(move |db| {
            let mut details = snapshot(db, &execution)?.details;
            let (command, cwd, thread, turn, forced, signal, confirmed, elapsed) = db.query_row(
                "SELECT json_extract(s.shell_json,'$.command'),s.cwd,r.thread_id,s.turn_id,CASE json_type(s.shell_json,'$.completion.forced') WHEN 'true' THEN 1 WHEN 'false' THEN 0 END,CASE WHEN json_type(s.shell_json,'$.completion.exit_signal')='integer' AND json_extract(s.shell_json,'$.completion.exit_signal') BETWEEN -2147483648 AND 2147483647 THEN json_extract(s.shell_json,'$.completion.exit_signal') END,CASE json_type(s.shell_json,'$.completion.group_cleanup_confirmed') WHEN 'true' THEN 1 WHEN 'false' THEN 0 END,CASE WHEN json_type(s.shell_json,'$.completion.elapsed_ms')='integer' AND json_extract(s.shell_json,'$.completion.elapsed_ms') BETWEEN 0 AND 9223372036854775807 THEN json_extract(s.shell_json,'$.completion.elapsed_ms') END FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE s.id=?1",
                [&execution], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get::<_,Option<i64>>(7)?)),
            ).map_err(sql_error)?;
            details.forced = forced;
            details.exit_signal = signal;
            details.group_cleanup_confirmed = confirmed;
            details.elapsed_ms = elapsed.map(|value| value as u64);
            details.command = command;
            details.cwd = Some(cwd);
            details.thread_id = Some(thread);
            details.turn_id = Some(turn);
            Ok(details)
        }).await
    }
    pub async fn read(&self, args: ShellReadArgs) -> Result<ShellOutput> {
        self.read_stream(args, StreamSelection::Both).await
    }
    pub async fn read_stream(
        &self,
        args: ShellReadArgs,
        streams: StreamSelection,
    ) -> Result<ShellOutput> {
        if args.limit_bytes == 0 || args.limit_bytes > MAX_BODY_BYTES {
            return Err(error(
                ErrorCode::InvalidArgument,
                "raw read limit must be in 1..=65536",
            ));
        }
        let canonical = args
            .encoding
            .as_deref()
            .map(canonical_encoding)
            .transpose()?
            .map(|encoding| encoding.name().to_owned());
        let old = args
            .cursor
            .as_deref()
            .map(OutputCursor::decode)
            .transpose()?;
        let mut pin = ReadPin::new(self.database.clone(), self.instance.to_string());
        let execution = args.execution_id.clone();
        let pin_id = pin.id.clone();
        let instance = pin.instance.clone();
        let expected = old.clone();
        let requested = canonical.clone();
        let prepared=self.db(move|db|{
            let tx=db.transaction().map_err(sql_error)?;
            let mut state=snapshot(&tx,&execution)?;
            if let Some(cursor)=&expected{validate_cursor(&tx,&state,&execution,cursor,requested.as_deref(),Some(streams))?;}
            ensure_raw_start(&tx,&state,&execution)?;
            if let Some(requested)=requested
                && canonical_encoding(&state.encoding)?.name()!=requested
            {
                // The retained bytes and cursor generation are unchanged. Derived
                // text vectors must be rebuilt explicitly in the new decoding.
                let mut statement=tx.prepare("SELECT DISTINCT r.node_id FROM tool_result r JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id JOIN shell_execution e ON e.id=c.operation_id OR e.operation_id=c.operation_id WHERE e.id=?1").map_err(sql_error)?;
                let nodes=statement.query_map([&execution],|row|row.get::<_,String>(0)).map_err(sql_error)?.collect::<std::result::Result<Vec<_>,_>>().map_err(sql_error)?;
                drop(statement);
                for node in nodes {crate::index::delete_raw_vectors_tx(&tx,&node)?;}
                tx.execute("UPDATE shell_execution SET output_encoding=?2 WHERE id=?1",params![execution,requested]).map_err(sql_error)?;
                state.encoding=requested;
            }
            ReadPin::insert_tx(&tx, &pin_id, &instance, &execution, state.generation)?;
            tx.commit().map_err(sql_error)?;Ok(state)
        }).await;
        let result = match prepared {
            Ok(state) => {
                let encoding = canonical_encoding(&state.encoding)?;
                self.read_pinned(&args, streams, old, state, encoding).await
            }
            Err(error) => Err(error),
        };
        let released = pin.release().await;
        match result {
            Ok(value) => {
                released?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
    async fn read_pinned(
        &self,
        args: &ShellReadArgs,
        streams: StreamSelection,
        old: Option<OutputCursor>,
        state: Snapshot,
        encoding: &'static Encoding,
    ) -> Result<ShellOutput> {
        let mut decoders = Decoders::new(encoding);
        let mut position = Position {
            sequence: 0,
            offset: 0,
        };
        let target = old.as_ref().map_or(position, |c| Position {
            sequence: c.sequence,
            offset: c.byte_offset,
        });
        while position < target {
            let max = if position.sequence == target.sequence {
                (target.offset - position.offset) as usize
            } else {
                RAW_PAGE
            };
            let page = self
                .page(&args.execution_id, &state, position, max.min(RAW_PAGE))
                .await?;
            if page.next > target {
                return Err(expired_cursor());
            }
            if streams.includes(&page.stream) {
                decoders.decode(&page.stream, &page.bytes, false);
            }
            position = page.next;
            tokio::task::yield_now().await;
        }
        let mut eof_flushed = old.as_ref().is_some_and(|c| c.eof_flushed);
        let mut text = String::new();
        let mut raw_bytes = 0;
        let mut decode_errors = false;
        while position.sequence < state.saved && raw_bytes < args.limit_bytes as u64 {
            let page = self
                .page(
                    &args.execution_id,
                    &state,
                    position,
                    (args.limit_bytes as u64 - raw_bytes).min(RAW_PAGE as u64) as usize,
                )
                .await?;
            if streams.includes(&page.stream) {
                let (part, bad) = decoders.decode(&page.stream, &page.bytes, false);
                text.push_str(&part);
                decode_errors |= bad;
                raw_bytes += page.bytes.len() as u64;
            }
            position = page.next;
            tokio::task::yield_now().await;
        }
        if state.terminal() && position.sequence == state.saved && !eof_flushed {
            let (part, bad) = decoders.finish(streams);
            text.push_str(&part);
            decode_errors |= bad;
            eof_flushed = true;
        }
        // An already-flushed cursor must not flush reconstructed pending bytes
        // again. Terminal executions cannot legitimately append new raw chunks.
        let cursor = OutputCursor {
            version: 1,
            execution_id: args.execution_id.clone(),
            sequence: position.sequence,
            byte_offset: position.offset,
            retention_generation: state.generation,
            encoding: encoding.name().into(),
            streams,
            eof_flushed,
        }
        .encode()?;
        Ok(ShellOutput {
            execution_id: args.execution_id.clone(),
            text,
            encoding: encoding.name().into(),
            cursor,
            raw_bytes,
            output_complete: state.details.output_complete && position.sequence == state.saved,
            decode_errors,
        })
    }
    async fn page(
        &self,
        execution: &str,
        state: &Snapshot,
        position: Position,
        max: usize,
    ) -> Result<RawPage> {
        let execution = execution.to_owned();
        let generation = state.generation;
        let saved = state.saved;
        self.db(move|db|{
            if let Some(error)=raw_tombstone(db,&execution)? {return Err(error);}
            let current_generation:Option<i64>=db.query_row("SELECT retention_generation FROM shell_execution WHERE id=?1",[&execution],|r|r.get(0)).optional().map_err(sql_error)?;
            if current_generation!=Some(generation as i64){return Err(expired_output_for(db,&execution,generation)?)}
            if position.sequence>=saved{return Err(expired_cursor())}
            let row:Option<(String,i64,Vec<u8>)>=db.query_row("SELECT stream,length(bytes),substr(bytes,?3+1,?4) FROM output_chunk WHERE execution_id=?1 AND sequence=?2",params![execution,position.sequence as i64,position.offset as i64,max as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql_error)?;
            let (stream,length,bytes)=match row {Some(row)=>row,None=>return Err(expired_output_for(db,&execution,generation)?)};
            let length=length as u64;
            if position.offset>length{return Err(expired_cursor())}
            let offset=position.offset+bytes.len() as u64;
            let next=if offset==length{Position{sequence:position.sequence+1,offset:0}}else{Position{sequence:position.sequence,offset}};
            if next==position{return Err(expired_cursor())}
            Ok(RawPage{stream,bytes,next})
        }).await
    }
    /// One forward pass for model excerpts: bounded head/tail text and decoder
    /// state, with short raw-page DB jobs and scheduler yields between them.
    pub async fn excerpt(&self, execution_id: &str, limit_bytes: usize) -> Result<String> {
        if limit_bytes == 0 {
            return Ok(String::new());
        }
        let mut pin = ReadPin::new(self.database.clone(), self.instance.to_string());
        let execution = execution_id.to_owned();
        let pin_id = pin.id.clone();
        let instance = pin.instance.clone();
        let prepared = self
            .db(move |db| {
                let tx = db.transaction().map_err(sql_error)?;
                let state = snapshot(&tx, &execution)?;
                ensure_raw_start(&tx, &state, &execution)?;
                ReadPin::insert_tx(&tx, &pin_id, &instance, &execution, state.generation)?;
                tx.commit().map_err(sql_error)?;
                Ok(state)
            })
            .await;
        let result = match prepared {
            Ok(state) => self.excerpt_pinned(execution_id, state, limit_bytes).await,
            Err(error) => Err(error),
        };
        let released = pin.release().await;
        match result {
            Ok(value) => {
                released?;
                Ok(value)
            }
            Err(error) if error.code == ErrorCode::OutputExpired => {
                let reason = error
                    .details
                    .as_ref()
                    .and_then(|details| details.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("raw_retention");
                Ok(prefix(
                    &format!("[original shell output expired: {reason}; use shell get for state]"),
                    limit_bytes,
                )
                .to_owned())
            }
            Err(error) => Err(error),
        }
    }
    async fn excerpt_pinned(
        &self,
        execution: &str,
        state: Snapshot,
        limit: usize,
    ) -> Result<String> {
        let mut decoders = Decoders::new(canonical_encoding(&state.encoding)?);
        let mut position = Position {
            sequence: 0,
            offset: 0,
        };
        let mut head = String::new();
        let mut tail = String::new();
        let mut total = 0usize;
        while position.sequence < state.saved {
            let page = self.page(execution, &state, position, RAW_PAGE).await?;
            let (text, _) = decoders.decode(&page.stream, &page.bytes, false);
            keep_excerpt(&text, limit, &mut head, &mut tail, &mut total);
            position = page.next;
            tokio::task::yield_now().await;
        }
        if state.terminal() {
            let (text, _) = decoders.finish(StreamSelection::Both);
            keep_excerpt(&text, limit, &mut head, &mut tail, &mut total);
        }
        if total <= limit {
            return Ok(head);
        }
        let marker = "\n[output omitted; retrieve original by execution ID]\n";
        if limit < marker.len() {
            return Ok(prefix(&head, limit).to_owned());
        }
        let available = limit - marker.len();
        let head_bytes = available / 2;
        let tail_bytes = available - head_bytes;
        Ok(format!(
            "{}{}{}",
            prefix(&head, head_bytes),
            marker,
            suffix(&tail, tail_bytes)
        ))
    }
}
fn keep_excerpt(text: &str, limit: usize, head: &mut String, tail: &mut String, total: &mut usize) {
    let previous = *total;
    *total = total.saturating_add(text.len());
    if previous < limit {
        head.push_str(prefix(text, limit - previous));
    }
    tail.push_str(text);
    if tail.len() > limit {
        let keep = suffix(tail, limit).to_owned();
        *tail = keep;
    }
}
fn prefix(text: &str, max: usize) -> &str {
    let end = text.floor_char_boundary(max.min(text.len()));
    &text[..end]
}
fn suffix(text: &str, max: usize) -> &str {
    let start = text.ceil_char_boundary(text.len().saturating_sub(max));
    &text[start..]
}

/// Must run in the same DB/Control ordering as waiter registration.
pub fn wait_position(db: &Connection, execution_id: &str, token: &str) -> Result<bool> {
    let cursor = OutputCursor::decode(token)?;
    let state = snapshot(db, execution_id)?;
    validate_cursor(db, &state, execution_id, &cursor, None, None)?;
    ensure_raw_start(db, &state, execution_id)?;
    if state.terminal() {
        return Ok(true);
    }
    let streams = match cursor.streams {
        StreamSelection::Both => "both",
        StreamSelection::Stdout => "stdout",
        StreamSelection::Stderr => "stderr",
    };
    db.query_row("SELECT EXISTS(SELECT 1 FROM output_chunk WHERE execution_id=?1 AND sequence<?2 AND (sequence>?3 OR (sequence=?3 AND length(bytes)>?4)) AND (?5='both' OR stream=?5))",params![execution_id,state.saved as i64,cursor.sequence as i64,cursor.byte_offset as i64,streams],|r|r.get(0)).map_err(sql_error)
}
fn snapshot(db: &Connection, execution: &str) -> Result<Snapshot> {
    type StoredShell = (
        String,
        String,
        Option<i32>,
        Option<String>,
        Option<bool>,
        i64,
        i64,
        String,
    );
    let row:Option<StoredShell>=db.query_row("SELECT owner_run_id,state,exit_code,end_reason,output_complete,saved_sequence,retention_generation,output_encoding FROM shell_execution WHERE id=?1",[execution],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional().map_err(sql_error)?;
    let (run_id, state, exit_code, reason, output_complete, saved, generation, encoding) =
        row.ok_or_else(|| error(ErrorCode::TargetUnavailable, "shell execution not found"))?;
    let state = serde_json::from_value(serde_json::Value::String(state))
        .map_err(|_| error(ErrorCode::InvalidState, "stored shell state is invalid"))?;
    let encoding = canonical_encoding(&encoding)
        .map_err(|_| {
            error(
                ErrorCode::InvalidState,
                "stored shell encoding is unsupported",
            )
        })?
        .name()
        .to_owned();
    Ok(Snapshot {
        details: ShellDetails {
            execution_id: execution.into(),
            run_id,
            state,
            output_complete: output_complete.unwrap_or(false),
            exit_code,
            reason,
            // Completion/provenance is fetched only by get, not every raw page.
            forced: None,
            exit_signal: None,
            group_cleanup_confirmed: None,
            elapsed_ms: None,
            command: None,
            cwd: None,
            thread_id: None,
            turn_id: None,
        },
        saved: saved as u64,
        generation: generation as u64,
        encoding,
    })
}
fn ensure_raw_start(db: &Connection, state: &Snapshot, execution: &str) -> Result<()> {
    if let Some(error) = raw_tombstone(db, execution)? {
        return Err(error);
    }
    if state.saved > 0 {
        let first: Option<i64> = db
            .query_row(
                "SELECT MIN(sequence) FROM output_chunk WHERE execution_id=?1 AND sequence<?2",
                params![execution, state.saved as i64],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if first != Some(0) {
            return Err(expired_output_for(db, execution, state.generation)?);
        }
    }
    Ok(())
}
fn validate_cursor(
    db: &Connection,
    state: &Snapshot,
    execution: &str,
    cursor: &OutputCursor,
    requested: Option<&str>,
    streams: Option<StreamSelection>,
) -> Result<()> {
    if cursor.execution_id != execution
        || cursor.sequence > state.saved
        || streams.is_some_and(|s| s != cursor.streams)
    {
        return Err(expired_cursor());
    }
    if let Some(error) = raw_tombstone(db, execution)? {
        return Err(error);
    }
    if cursor.retention_generation != state.generation {
        return Err(expired_output_for(db, execution, state.generation)?);
    }
    if cursor.encoding != state.encoding || requested.is_some_and(|s| s != cursor.encoding) {
        return Err(error(
            ErrorCode::CursorEncodingMismatch,
            "output cursor encoding changed",
        )
        .hint("restart shell read without the old cursor and specify the desired encoding"));
    }
    if cursor.sequence == state.saved {
        if cursor.byte_offset != 0 {
            return Err(expired_cursor());
        }
    } else {
        let length: Option<i64> = db
            .query_row(
                "SELECT length(bytes) FROM output_chunk WHERE execution_id=?1 AND sequence=?2",
                params![execution, cursor.sequence as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        if length.is_none() {
            return Err(expired_output_for(db, execution, state.generation)?);
        }
        if cursor.byte_offset >= length.unwrap_or(0) as u64 {
            return Err(expired_cursor());
        }
    }
    if cursor.eof_flushed && (!state.terminal() || cursor.sequence != state.saved) {
        return Err(expired_cursor());
    }
    Ok(())
}
fn canonical_encoding(label: &str) -> Result<&'static Encoding> {
    let normalized = label.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "utf8" | "utf-8" => Ok(encoding_rs::UTF_8),
        "utf16le" | "utf-16le" => Ok(encoding_rs::UTF_16LE),
        "utf16be" | "utf-16be" => Ok(encoding_rs::UTF_16BE),
        "utf16" | "utf-16" => Err(error(
            ErrorCode::InvalidArgument,
            "UTF-16 requires an explicit LE or BE byte order",
        )),
        "cp932" | "windows-31j" | "ms932" => Ok(encoding_rs::SHIFT_JIS),
        _ => Encoding::for_label(normalized.as_bytes())
            .ok_or_else(|| error(ErrorCode::InvalidArgument, "unknown output encoding")),
    }
}
struct ReadPin {
    database: DbWorker,
    id: String,
    instance: String,
    armed: bool,
}
impl ReadPin {
    /// Register only after the caller's snapshot/raw/cursor checks, in that
    /// same transaction. The existing RAII owner stays outside the DB closure.
    fn insert_tx(
        tx: &rusqlite::Transaction<'_>,
        id: &str,
        instance: &str,
        execution: &str,
        generation: u64,
    ) -> Result<usize> {
        tx.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES(?1,?2,'shell_output',?3,?4,?5)",params![id,instance,execution,generation as i64,chrono::Utc::now().to_rfc3339()]).map_err(sql_error)
    }
    fn new(database: DbWorker, instance: String) -> Self {
        Self {
            database,
            id: uuid::Uuid::new_v4().to_string(),
            instance,
            armed: true,
        }
    }
    async fn release(&mut self) -> Result<()> {
        let id = self.id.clone();
        self.database
            .call(move |db| {
                db.execute("DELETE FROM runtime_pin WHERE id=?1", [id])?;
                Ok(())
            })
            .await
            .map_err(storage_error)?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for ReadPin {
    fn drop(&mut self) {
        if self.armed
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let database = self.database.clone();
            let id = self.id.clone();
            runtime.spawn(async move {
                let _ = database
                    .call(move |db| {
                        db.execute("DELETE FROM runtime_pin WHERE id=?1", [id])?;
                        Ok(())
                    })
                    .await;
            });
        }
    }
}
fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}
fn expired_cursor() -> ApiError {
    error(
        ErrorCode::CursorExpired,
        "output cursor is invalid for this execution",
    )
    .hint("restart shell read without the previous cursor")
}
fn expired_output() -> ApiError {
    error(
        ErrorCode::OutputExpired,
        "original shell output is no longer fully retained",
    )
    .hint("inspect shell state; the unavailable raw prefix cannot be reconstructed")
}
fn raw_tombstone(db: &Connection, execution: &str) -> Result<Option<ApiError>> {
    let marker: Option<String> = db
        .query_row(
            "SELECT value FROM app_meta WHERE key=?1",
            [format!("raw_tombstone.{execution}")],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    marker
        .map(|marker| {
            let marker: serde_json::Value = serde_json::from_str(&marker)
                .map_err(|_| error(ErrorCode::InvalidState, "raw retention marker is invalid"))?;
            let reason = marker
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("raw_retention");
            Ok(expired_output().details(
                serde_json::json!({"execution_id":execution,"reason":reason,"tombstone":marker}),
            ))
        })
        .transpose()
}
fn expired_output_for(db: &Connection, execution: &str, generation: u64) -> Result<ApiError> {
    let oldest: Option<i64> = db
        .query_row(
            "SELECT MIN(sequence) FROM output_chunk WHERE execution_id=?1",
            [execution],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    Ok(expired_output().details(serde_json::json!({"execution_id":execution,"retention_generation":generation,"oldest_sequence":oldest})))
}
fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("output database operation failed: {error}"))
}
fn storage_error(error: StorageError) -> ApiError {
    error.into_api_error()
}
