"""Synthetic current-schema fixtures; never opens a non-temporary data-dir."""
import datetime as dt
import json
from pathlib import Path
import sqlite3
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def connect(root):
    root = Path(root).resolve()
    if not root.is_relative_to(Path(tempfile.gettempdir()).resolve()):
        raise ValueError('Synthetic fixtures require a temporary directory')
    db = sqlite3.connect(root / 'shellboy.db')
    db.execute('PRAGMA foreign_keys=ON')
    db.executescript((ROOT / 'src/storage/current.sql').read_text())
    return db


def time_at(seconds):
    return (dt.datetime(2026, 10, 31, tzinfo=dt.timezone.utc) + dt.timedelta(seconds=seconds)).isoformat()


def add(db, execution, command='cargo test broken', code=1, thread='thread', turn='turn',
        start=0, end=None, cwd='/repo', stderr='error: missing /tmp/build-123/file.rs line 42',
        state='exited', reason=None, encoding='UTF-8', node=True):
    run, attempt, group = 'run-' + turn, 'attempt-' + turn, 'group-' + turn
    db.execute("INSERT OR IGNORE INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?,'/repo','','p','m',?)", (thread, time_at(0)))
    db.execute("INSERT OR IGNORE INTO run(id,thread_id,state,created_at) VALUES(?,?,'completed',?)", (run, thread, time_at(0)))
    db.execute("INSERT OR IGNORE INTO turn(id,thread_id,run_id,state,created_at) VALUES(?,?,?,'completed',?)", (turn, thread, run, time_at(0)))
    db.execute("INSERT OR IGNORE INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES(?,?,0,0,'p','m',0,'completed','{}',?)", (attempt, turn, time_at(0)))
    db.execute("INSERT OR IGNORE INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES(?,?,?,'completed',?)", (group, attempt, turn, time_at(0)))
    ordinal = db.execute('SELECT count(*) FROM tool_call WHERE group_id=?', (group,)).fetchone()[0]
    call = 'call-' + str(ordinal)
    db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,exit_code,end_reason,started_at,ended_at) VALUES(?,?,?,?,?,?,?,'UTF-8',?,'closed',?,?,?,?)", (execution, run, turn, execution, state, cwd, json.dumps({'command': command}), encoding, code, reason, time_at(start), time_at(start+1 if end is None else end)))
    db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id) VALUES(?,?,?,'shell','{}','completed',?)", (group, call, ordinal, execution))
    if node:
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json) VALUES(?,?,'tool_result',?,'{}')", ('node-' + execution, turn, time_at(start)))
        db.execute("INSERT INTO tool_result VALUES(?,?,?,'completed')", (group, call, 'node-' + execution))
    db.execute("INSERT INTO output_chunk VALUES(?,0,'stdout',0,?,?)", (execution, b'RAW_STDOUT_MUST_NOT_BE_COPIED', time_at(start)))
    if stderr is not None:
        db.execute("INSERT INTO output_chunk VALUES(?,1,'stderr',0,?,?)", (execution, stderr.encode(), time_at(start)))
