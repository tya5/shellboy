#!/usr/bin/env python3
"""Synthetic public-release -> candidate upgrade on an owned Unix PTY.

Only supplied binaries, an empty fixture directory and loopback endpoints are
used. This never locates or modifies a user's owner/store/installed executable.
"""
import argparse
from contextlib import closing
import hashlib
import http.server
import json
import os
from pathlib import Path
import shlex
import signal
import sqlite3
import struct
import subprocess
import threading
import time


CARGO_VERSION = next(line.split('"')[1] for line in (Path(__file__).resolve().parents[1] / 'Cargo.toml').read_text(encoding='utf-8').splitlines() if line.startswith('version = '))
MIGRATIONS = Path(__file__).resolve().parents[1] / 'migrations'
DERIVED = {version: next(MIGRATIONS.glob(f'{version:04d}_*.sql'))
           for version in (15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41)}
PUBLIC_SCHEMA = {'0.3.0': 14, '0.4.0': 14, '0.5.0': 17, '0.6.0': 19, '0.6.1': 19, '0.7.0': 24, '0.7.1': 25, '0.7.2': 27, '0.8.0': 27, '0.8.1': 28, '0.8.2': 28, '0.8.3': 29, '0.9.0': 31, '0.10.0': 32, '0.11.0': 39}


def read_only_uri(path, stopped=False):
    # Only reaped writers and absent WAL permit immutable read-only access.
    immutable = stopped and not Path(str(path) + '-wal').exists()
    return path.as_uri() + '?mode=ro' + ('&immutable=1' if immutable else '')


def ledger(db):
    return db.execute('SELECT version,name FROM schema_migration ORDER BY version').fetchall()


def snapshot(db):
    # Saved originals and relationships must survive; startup may append records.
    rows = {table: db.execute(f'SELECT * FROM "{table}"').fetchall()
            for table in ('node', 'edge', 'thread_path', 'output_item', 'tool_call',
                          'tool_result', 'output_chunk', 'attachment_ref', 'managed_file')}
    rows['thread_lineage'] = db.execute('SELECT id,parent_thread_id,cwd FROM thread').fetchall()
    rows['summary_usage'] = db.execute("SELECT key,value FROM app_meta WHERE key LIKE 'summary.usage.%'").fetchall()
    return rows


def preserved(db, before):
    for table, rows in before.items():
        if table == 'thread_lineage':
            query = 'SELECT id,parent_thread_id,cwd FROM thread'
        elif table == 'summary_usage':
            query = "SELECT key,value FROM app_meta WHERE key LIKE 'summary.usage.%'"
        else:
            query = f'SELECT * FROM "{table}"'
        after = db.execute(query).fetchall()
        # Additive migrations append columns; compare all original columns.
        assert all(any(candidate[:len(row)] == row for candidate in after) for row in rows), f'saved data changed: {table}'


def check_migration(db, before, start, target):
    assert db.execute('PRAGMA user_version').fetchone() == (target,)
    expected = before + [(version, next(MIGRATIONS.glob(f'{version:04d}_*.sql')).stem)
                         for version in range(start + 1, target + 1)]
    assert ledger(db) == expected, 'migration ledger prefix or suffix changed'
    assert db.execute('PRAGMA integrity_check').fetchall() == [('ok',)]
    assert db.execute('PRAGMA foreign_key_check').fetchall() == []
    if target >= 17:
        assert [row[2] for row in db.execute('PRAGMA index_info(thread_parent)')] == [
            'parent_thread_id', 'id']
    if target >= 18:
        row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='gc_bytes_app_meta_history'").fetchone()
        expected_index = next(line for line in DERIVED[18].read_text(encoding='utf-8').splitlines()
                              if line.startswith('CREATE INDEX gc_bytes_app_meta_history '))
        assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected_index.rstrip(';').split()), \
            'history accounting index differs from migration 18'
    if target >= 19:
        row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='attempts_completed_scope'").fetchone()
        expected_index = next(line for line in DERIVED[19].read_text(encoding='utf-8').splitlines()
                              if line.startswith('CREATE INDEX attempts_completed_scope '))
        assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected_index.rstrip(';').split()), \
            'context attempt scope index differs from migration 19'
    if target >= 21:
        for name in ('node_event_retention', 'delivery_event_state'):
            row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name=?", (name,)).fetchone()
            expected_index = next(line for line in DERIVED[21].read_text(encoding='utf-8').splitlines()
                                  if line.startswith(f'CREATE INDEX {name} '))
            assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected_index.rstrip(';').split()), \
                f'{name} differs from migration 21'
    if target >= 22:
        # Counters must equal the saved rows they account (exact, #171).
        counters = dict(db.execute("SELECT category,logical_bytes FROM quota_counter WHERE category LIKE 'logical.%'").fetchall())
        assert set(counters) == {'logical.history', 'logical.raw', 'logical.managed', 'logical.index'}, \
            'logical usage counters missing after migration 22'
        raw = db.execute('SELECT COALESCE(SUM(length(bytes)),0) FROM output_chunk').fetchone()[0]
        assert counters['logical.raw'] == raw, 'raw counter differs from saved output after migration 22'
        row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='attachment_delivery_operation'").fetchone()
        assert row, 'attachment_delivery_operation index missing after migration 22'
    if target >= 23:
        for name in ('activity_attempt_turn', 'activity_turn_run', 'activity_turn_run_state',
                     'activity_group_turn_state', 'activity_shell_spawned_turn', 'activity_shell_uncollected_owner'):
            row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name=?", (name,)).fetchone()
            expected_index = next(line.replace(' IF NOT EXISTS', '')
                                  for line in DERIVED[23].read_text(encoding='utf-8').splitlines()
                                  if line.startswith(f'CREATE INDEX IF NOT EXISTS {name} '))
            assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected_index.rstrip(';').split()), \
                f'{name} differs from migration 23'

    if target >= 26:
        expected_index = DERIVED[26].read_text(encoding='utf-8').split(';')[0]
        actual = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='receipt_reply_pending'").fetchone()
        assert actual and ' '.join(actual[0].split()) == ' '.join(expected_index.split()), 'receipt_reply_pending missing or changed'
    if target >= 24:
        assert db.execute("SELECT count(*) FROM app_meta WHERE key GLOB 'revisit.*'").fetchone() == (0,), \
            'Revisit metadata remains after migration 24'
        row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='receipt_run_completion'").fetchone()
        expected_index = next(line for line in DERIVED[24].read_text(encoding='utf-8').splitlines()
                              if line.startswith('CREATE INDEX IF NOT EXISTS receipt_run_completion '))
        expected_index = expected_index.replace(' IF NOT EXISTS', '')
        assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected_index.rstrip(';').split()), \
            'receipt_run_completion differs from migration 24'

    if target >= 25:
        row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='history_tool_group_attempt'").fetchone()
        expected_index = next(line for line in DERIVED[25].read_text(encoding='utf-8').splitlines()
                              if line.startswith('CREATE INDEX IF NOT EXISTS history_tool_group_attempt '))
        expected_index = expected_index.replace(' IF NOT EXISTS', '')
        assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected_index.rstrip(';').split()), \
            'history_tool_group_attempt differs from migration 25'

    if target >= 27:
        row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='jobs_running_recovery'").fetchone()
        expected = next(line.replace(' IF NOT EXISTS', '') for line in DERIVED[27].read_text(encoding='utf-8').splitlines() if line.startswith('CREATE INDEX IF NOT EXISTS jobs_running_recovery '))
        assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected.rstrip(';').split()), 'jobs_running_recovery differs from migration 27'
    if target >= 28:
        expected_candidates = db.execute("SELECT g.id FROM tool_group g WHERE g.state<>'completed' OR EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=g.id AND (c.state IN ('pending','running') OR NOT EXISTS(SELECT 1 FROM tool_result r WHERE r.group_id=c.group_id AND r.call_id=c.call_id))) ORDER BY g.id").fetchall()
        actual_candidates = db.execute('SELECT group_id FROM tool_group_recovery_candidate ORDER BY group_id').fetchall()
        assert actual_candidates == expected_candidates, 'tool-group recovery candidates differ after migration 28'
        trigger_count = db.execute("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'tool_group_recovery_%'").fetchone()[0]
        assert trigger_count == 9, 'tool-group recovery triggers missing after migration 28'
    if target >= 29:
        row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='model_attempt_active_recovery'").fetchone()
        text = DERIVED[29].read_text(encoding='utf-8')
        start = text.index('CREATE INDEX')
        expected = text[start:text.index(';', start)].replace(' IF NOT EXISTS', '')
        assert row and ' '.join(row[0].rstrip(';').split()) == ' '.join(expected.split()), 'model_attempt_active_recovery differs from migration 29'
        plan = ' '.join(r[3] for r in db.execute("EXPLAIN QUERY PLAN UPDATE model_attempt SET state='incomplete' WHERE state IN ('preparing','inflight')"))
        assert 'model_attempt_active_recovery' in plan, plan
    if target >= 30:
        columns = {row[1]: row for row in db.execute('PRAGMA table_info(thread)')}
        assert 'auto_compact_at' in columns and columns['auto_compact_at'][2] == 'INTEGER' and not columns['auto_compact_at'][3], 'thread.auto_compact_at differs from migration 30'
        assert db.execute('SELECT count(*) FROM thread WHERE auto_compact_at IS NOT NULL').fetchone()[0] == 0, 'migration 30 must leave every Thread on the default threshold'
    if target >= 31:
        columns = {row[1]: row for row in db.execute('PRAGMA table_info(thread)')}
        for name in ('segment_chars', 'reduce_rounds'):
            assert name in columns and columns[name][2] == 'INTEGER' and not columns[name][3], f'thread.{name} differs from migration 31'
            assert db.execute(f'SELECT count(*) FROM thread WHERE {name} IS NOT NULL').fetchone()[0] == 0, 'migration 31 must leave every Thread on the default summary parameters'
    if target >= 32:
        columns = {row[1]: row for row in db.execute('PRAGMA table_info(thread)')}
        assert 'archived_at' in columns and columns['archived_at'][2] == 'TEXT' and not columns['archived_at'][3], 'thread.archived_at differs from migration 32'
        assert db.execute('SELECT count(*) FROM thread WHERE archived_at IS NOT NULL').fetchone()[0] == 0, 'migration 32 must leave every Thread unarchived'
        indexes = {row[1]: row[2] for row in db.execute("SELECT type,name,sql FROM sqlite_master WHERE type='index' AND name IN ('threads_visible_updated','threads_archived_updated')")}
        assert set(indexes) == {'threads_visible_updated', 'threads_archived_updated'}, 'migration 32 must create both partial list indexes'
        assert all('WHERE' in sql for sql in indexes.values()), 'archive list indexes must be partial'
    if target >= 33:
        columns = {row[1]: row for row in db.execute('PRAGMA table_info(index_space)')}
        assert 'int8_scale' in columns and columns['int8_scale'][2] == 'REAL' and not columns['int8_scale'][3], 'index_space.int8_scale differs from migration 33'
        assert db.execute('SELECT count(*) FROM index_space WHERE int8_scale IS NOT NULL').fetchone()[0] == 0, 'migration 33 must leave every existing space float32'
        assert db.execute("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='chunks_job'").fetchone()[0] == 1, 'migration 33 must index embedding_chunk.job_id'
    if target >= 34:
        columns = {row[1]: row for row in db.execute('PRAGMA table_info(thread_tag)')}
        assert set(columns) == {'thread_id', 'tag'} and all(row[2] == 'TEXT' and row[3] for row in columns.values()), 'thread_tag differs from migration 34'
        assert db.execute('SELECT count(*) FROM thread_tag').fetchone()[0] == 0, 'migration 34 must leave every Thread untagged'
        indexes = {row[0]: row[1] for row in db.execute("SELECT name,sql FROM sqlite_master WHERE type='index' AND name IN ('thread_tag_by_tag','threads_cwd_updated')")}
        assert set(indexes) == {'thread_tag_by_tag', 'threads_cwd_updated'}, 'migration 34 must create the tag and cwd list indexes'
        assert 'WHERE' in indexes['threads_cwd_updated'], 'the cwd list index must be partial'
    if target >= 38:
        index = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name='receipt_human_reply_pending'").fetchone()
        assert index is not None and 'target_thread_id,created_at' in index[0], 'migration 38 must index human reply targets and timestamps'
        assert "json_extract(result_json,'$.reply.state')='pending'" in index[0] and "json_extract(result_json,'$.to_human')=1" in index[0], 'migration 38 must index only pending human replies'

    if target >= 35:
        for name in ('index_reclaim_state', 'index_reclaim_group_value'):
            expected = next(line.rstrip(';') for line in DERIVED[35].read_text().splitlines() if f' {name} ' in line).replace('IF NOT EXISTS ', '')
            row = db.execute("SELECT sql FROM sqlite_master WHERE type='index' AND name=?", (name,)).fetchone()
            assert row and ' '.join(row[0].split()) == ' '.join(expected.split()), f'{name} differs from migration 35'

    if target >= 36:
        assert 'spill_tools_json' in {row[1] for row in db.execute('PRAGMA table_info(node)')}, 'migration 36 spill metadata missing'
        assert db.execute("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='node_spill_tools'").fetchone()[0] == 1, 'migration 36 index missing'
    if target >= 37:
        assert 'branch_kind' in {row[1] for row in db.execute('PRAGMA table_info(thread)')}, 'migration 37 branch kind missing'

    if target >= 39:
        assert 'owner_run_id' not in {row[1] for row in db.execute('PRAGMA table_info(run)')}, 'migration 39 must remove Run owner'
        for name in ('receipt_human_reply_pending', 'receipt_reply_run_pending'):
            assert db.execute("SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?", (name,)).fetchone()[0] == 1, f'migration 39 missing {name}'

    if target >= 41:
        assert db.execute("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='receipt_reply_node_lookup'").fetchone()[0] == 1, 'migration 41 reply lookup index missing'
    if target >= 40:
        assert 'selection_nonappend_generation' in {row[1] for row in db.execute('PRAGMA table_info(thread)')}, 'migration 40 witness missing'
        for name in ('selection_generation_combined_write', 'selection_generation_update', 'selection_thread_conflict', 'selection_generation_insert', 'selection_path_insert', 'selection_path_delete', 'selection_path_update', 'selection_revision_same_cursor'):
            assert db.execute("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name=?", (name,)).fetchone()[0] == 1, f'migration 40 missing {name}'

def apply_migration(db, version):
    if version != 39:
        sql=DERIVED[version].read_text() if version in DERIVED else next(MIGRATIONS.glob(f'{version:04d}_*.sql')).read_text()
        db.executescript('BEGIN IMMEDIATE;\n' + sql + '\nCOMMIT;')
        return
    # Mirror the approved migration contract: check FK before committing.
    db.commit()
    db.execute('PRAGMA foreign_keys=OFF')
    try:
        db.executescript('BEGIN IMMEDIATE;\n' + DERIVED[version].read_text())
        assert db.execute('PRAGMA foreign_key_check').fetchall() == [], 'migration 39 foreign key violation'
        db.commit()
    except BaseException:
        db.rollback()
        raise
    finally:
        db.execute('PRAGMA foreign_keys=ON')

def prepare_schema(db, target):
    start = db.execute('PRAGMA user_version').fetchone()[0]
    if start == target:
        return
    assert start in (14, 17, 19, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41) and target in (14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41) and start <= target, 'unexpected synthetic fixture schema'
    before, records = ledger(db), snapshot(db)
    for version in range(start + 1, target + 1):
        apply_migration(db, version)
    check_migration(db, before, start, target)
    preserved(db, records)


def parse_args(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument('--old', type=Path, required=True)
    parser.add_argument('--new', type=Path, required=True)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--old-version', choices=tuple(PUBLIC_SCHEMA), default='0.10.0')
    parser.add_argument('--start-schema', type=int, choices=(14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43), default=None,
                        help='defaults to the public old schema; 16/17 can stage derived indexes on a stopped schema 14 copy')
    parser.add_argument('--expected-version', default=CARGO_VERSION, help='defaults to the Cargo.toml package version')
    parser.add_argument('--expected-new-schema', type=int, choices=(14, 15, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43), default=43,
                        help='43 for current Thread summary settings; 42 for notification semantic metadata; 40 for the non-append selection witness; 39 for independent Runs; 38 for the human-reply pending index; 34 for the Thread tag table and cwd list index; 33 for the index space int8 scale column; 32 for the Thread archive column and partial list indexes; 31 for the per-Thread summary parameter columns; 30 for the per-Thread compaction threshold column; 29 for the active-attempt recovery index; earlier versions retain historical release checks')
    args = parser.parse_args(argv)
    public_schema = PUBLIC_SCHEMA[args.old_version]
    if args.start_schema is None:
        args.start_schema = public_schema
    if args.start_schema < public_schema or args.expected_new_schema < args.start_schema:
        parser.error('schemas must not precede the public or fixture schema')
    return args


def main():
    args = parse_args()
    public_schema = PUBLIC_SCHEMA[args.old_version]
    old, new = args.old.resolve(), args.new.resolve()
    assert subprocess.check_output([str(old), '--version'], text=True).strip() == 'shellboy ' + args.old_version
    assert subprocess.check_output([str(new), '--version'], text=True).strip() == 'shellboy ' + args.expected_version
    root = args.root.resolve()
    root.mkdir()  # Existing fixture/user paths are never overwritten.
    work = root / 'work'
    work.mkdir()
    data, upgraded = root / 'old-data', root / 'upgraded-data'
    config = root / 'fixture.toml'
    env = {key: value for key, value in os.environ.items() if not key.startswith('SHELLBOY_')}
    env.update(TERM='xterm-256color', NO_COLOR='1')
    state = {'recovery_thread': None, 'response_calls': 0, 'embedding_calls': 0}

    def response(text):
        return {'id': 'synthetic-upgrade', 'status': 'completed',
                'usage': {'input_tokens': 11, 'input_tokens_details': {'cached_tokens': 3},
                          'output_tokens': 5, 'total_tokens': 16},
                'output': [{'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': text}]}]}

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def send(self, status, body):
            raw = json.dumps(body).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if self.path.endswith('/embeddings'):
                state['embedding_calls'] += 1
                inputs = body['input'] if isinstance(body['input'], list) else [body['input']]
                return self.send(200, {'data': [{'index': i, 'embedding': [1.0, 0.0, 0.0]} for i in range(len(inputs))], 'usage': {'prompt_tokens': 3, 'total_tokens': 3}})
            state['response_calls'] += 1
            text = json.dumps(body.get('input', []))
            if 'Final answer contract' in text:
                return self.send(200, response('{"value":"upgraded mock exec"}'))
            if 'SYNTHETIC_RECOVERY' in text:
                return self.send(400, {'error': {'code': 'invalid_request_error', 'message': 'synthetic permanent failure'}})
            if 'SYNTHETIC_SENDER' in text:
                if any(item.get('type') == 'function_call_output' for item in body.get('input', [])):
                    return self.send(200, response('synthetic sender completed'))
                command = ' '.join(shlex.quote(str(item)) for item in [old, '--config', config, '--data-dir', data, 'message', 'send', '--thread', state['recovery_thread'], '--text', 'SYNTHETIC_PENDING_FROM_OLD'])
                return self.send(200, {'id': 'synthetic-tool', 'status': 'completed', 'output': [{'type': 'function_call', 'call_id': 'queue-pending', 'name': 'shell', 'arguments': json.dumps({'command': command})}]})
            return self.send(200, response('SYNTHETIC_ANSWER retained conversation text'))

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    config.write_text(f'default_connection="fixture"\ndefault_model="fixture-model"\n[connections.fixture]\nbase_url="http://127.0.0.1:{server.server_port}/v1"\nresponse_max_retries=0\n[embedding]\nconnection="fixture"\nmodel="fixture-embedding"\n[ui]\ncopy_backend="osc52"\n', encoding='utf-8')

    class Owner:
        def __init__(self, binary, store):
            import fcntl
            import pty
            import termios
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
            def setup():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            self.process = subprocess.Popen([str(binary), '--data-dir', str(store), '--config', str(config)], stdin=slave, stdout=slave, stderr=slave, cwd=work, env=env, preexec_fn=setup)
            os.close(slave)
            self.master = master
            self.output = bytearray()
            def drain():
                while True:
                    try:
                        part = os.read(master, 65536)
                        if not part:
                            break
                        if len(self.output) < 2_000_000:
                            self.output.extend(part)
                    except OSError:
                        break
            self.reader = threading.Thread(target=drain, daemon=True)
            self.reader.start()

        def close(self, hard=False):
            if self.process.poll() is None:
                if hard:
                    os.killpg(self.process.pid, signal.SIGKILL)
                else:
                    os.write(self.master, b'\x11')
                    try:
                        self.process.wait(timeout=60)
                    except subprocess.TimeoutExpired:
                        os.killpg(self.process.pid, signal.SIGKILL)
                        raise
            self.process.wait(timeout=60)
            os.close(self.master)
            self.reader.join(timeout=5)
            return self.process.returncode

    def cli(binary, store, *words):
        process = subprocess.run([str(binary), '--data-dir', str(store), '--config', str(config), *map(str, words)], cwd=work, env=env, capture_output=True, text=True, timeout=90)
        result = json.loads(process.stdout)
        assert process.returncode == 0 and result.get('ok'), (words, result, process.stderr)
        return result

    def sql(store, query, values=()):
        connection = sqlite3.connect((store / 'shellboy.db').as_uri() + '?mode=ro', uri=True)
        try:
            return connection.execute(query, values).fetchall()
        finally:
            connection.close()

    def wait_for(check, label):
        end = time.monotonic() + 90
        while True:
            if check():
                return
            if time.monotonic() >= end:
                raise AssertionError('fixture watchdog: ' + label)
            time.sleep(.03)

    def ready(binary, store):
        def probe():
            try:
                return cli(binary, store, 'status')['ok']
            except (OSError, ValueError, AssertionError):
                return False
        wait_for(probe, 'owner IPC ready')

    def thread(binary, store):
        return cli(binary, store, 'thread', 'create')['result']['thread_id']

    def settled(store, target, expected):
        wait_for(lambda: sql(store, 'SELECT state FROM turn WHERE thread_id=? ORDER BY rowid DESC LIMIT 1', (target,)) == [(expected,)], 'saved Turn ' + expected)

    owners = []
    try:
        previous = Owner(old, data)
        owners.append(previous)
        ready(old, data)
        assert sql(data, 'PRAGMA user_version') == [(public_schema,)]
        seed = thread(old, data)
        first = cli(old, data, 'message', 'send', '--thread', seed, '--text', 'SYNTHETIC_SEED')['result']['message_id']
        settled(data, seed, 'completed')
        wait_for(lambda: sql(data, "SELECT count(*) FROM embedding_job WHERE state='done'")[0][0] > 0, 'old indexing')
        assert 'SYNTHETIC_ANSWER' in json.dumps(cli(old, data, 'history', 'search', '--thread', seed, '--query', 'SYNTHETIC_ANSWER'))
        recovery = thread(old, data)
        state['recovery_thread'] = recovery
        cli(old, data, 'message', 'send', '--thread', recovery, '--text', 'SYNTHETIC_RECOVERY')
        settled(data, recovery, 'recovery_wait')
        sender = thread(old, data)
        cli(old, data, 'message', 'send', '--thread', sender, '--text', 'SYNTHETIC_SENDER')
        settled(data, sender, 'completed')
        pending = sql(data, "SELECT r.message_node_id,r.state FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=? AND n.payload_json LIKE '%SYNTHETIC_PENDING_FROM_OLD%'", (recovery,))
        assert len(pending) == 1 and pending[0][1] in ('held', 'accepted'), pending
        materials = root / 'materials'
        materials.mkdir()
        (materials / 'document.txt').write_text('SYNTHETIC_RAG_CONTENT retained extracted text', encoding='utf-8')
        cli(old, data, 'rag', 'init')
        cli(old, data, 'rag', 'source', 'add', materials, '--name', 'synthetic-documents')
        cli(old, data, 'rag', 'index', '--source', 'synthetic-documents', '--mode', 'keyword')
        original_sender = cli(old, data, 'message', 'get', '--message', first)['result']['sender']
        original_usage = cli(old, data, 'usage', 'get', '--thread', seed)['result']['model_attempts']['completed']
        before_nodes = sql(data, 'SELECT count(*) FROM node')[0][0]
        previous.close(hard=True)
        owners.remove(previous)
        upgraded.mkdir()
        for filename in ['shellboy.db', 'rag.db']:
            source_path = data / filename
            # All fixture writers are reaped. A checkpointed WAL-mode RAG file
            # can lack sidecars; immutable permits read-only backup without
            # creating them. Never ignore an existing conversation WAL.
            source = sqlite3.connect(read_only_uri(source_path, stopped=True), uri=True)
            destination = sqlite3.connect(upgraded / filename)
            try:
                source.backup(destination)
            finally:
                destination.close()
                source.close()
        with closing(sqlite3.connect(upgraded / 'shellboy.db')) as fixture:
            prepare_schema(fixture, args.start_schema)
            before_ledger, before_records = ledger(fixture), snapshot(fixture)
        current = Owner(new, upgraded)
        owners.append(current)
        ready(new, upgraded)
        with closing(sqlite3.connect((upgraded / 'shellboy.db').as_uri() + '?mode=ro', uri=True)) as fixture:
            check_migration(fixture, before_ledger, args.start_schema, args.expected_new_schema)
            preserved(fixture, before_records)
            after_ledger = ledger(fixture)
        assert 'SYNTHETIC_ANSWER' in json.dumps(cli(new, upgraded, 'history', 'search', '--thread', seed, '--query', 'SYNTHETIC_ANSWER'))
        old_sender = cli(new, upgraded, 'message', 'get', '--message', first)['result']['sender']
        assert old_sender == original_sender, (original_sender, old_sender)
        saved_pending = cli(new, upgraded, 'message', 'get', '--message', pending[0][0])['result']
        assert saved_pending['delivery_status'] in ('held', 'accepted')
        assert saved_pending['sender']['kind'] == 'model' and saved_pending['sender']['thread_id'] == sender
        usage = cli(new, upgraded, 'usage', 'get', '--thread', seed)['result']
        assert usage['model_attempts']['completed'] == original_usage
        assert usage['model_attempts']['completed']['input_tokens']['known_sum'] == 11
        # RAG schema 4 is migrated only by the writer; readers ask for `rag init`.
        cli(new, upgraded, 'rag', 'init')
        assert 'SYNTHETIC_RAG_CONTENT' in json.dumps(cli(new, upgraded, 'rag', 'search', '--scope', 'source:synthetic-documents', '--query', 'SYNTHETIC_RAG_CONTENT'))
        schema = root / 'result.schema.json'
        schema.write_text('{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}', encoding='utf-8')
        # This fixture returns an end message, not an explicit correlated reply.
        # Preserve that upgrade check even after exec's default changes to reply.
        result_args = ['--result', 'turn-end'] if args.expected_new_schema >= 26 else []
        execution = subprocess.run([str(new), '--config', str(config), '--data-dir', str(upgraded), 'exec', *result_args, '--schema', str(schema), '--thread', seed, 'UPGRADE_EXEC'], cwd=work, env=env, capture_output=True, text=True, timeout=90)
        assert execution.returncode == 0, (execution.stdout, execution.stderr)
        assert json.loads(execution.stdout) == {'value': 'upgraded mock exec'}
        execution_facts = json.loads(execution.stderr)
        added = cli(new, upgraded, 'message', 'get', '--message', execution_facts['message_id'])['result']['sender']
        assert added == {'kind': 'external_cli', 'label': 'shellboy exec'}, added
        assert cli(new, upgraded, 'usage', 'get', '--thread', seed)['result']['model_attempts']['completed']['count'] == 2
        exit_code = current.close()
        owners.remove(current)
        assert exit_code == 0
        with closing(sqlite3.connect(read_only_uri(upgraded / 'shellboy.db', stopped=True), uri=True)) as fixture:
            check_migration(fixture, before_ledger, args.start_schema, args.expected_new_schema)
            preserved(fixture, before_records)
        rag_path = upgraded / 'rag.db'
        rag = sqlite3.connect(read_only_uri(rag_path, stopped=True), uri=True)
        try:
            rag_schema = rag.execute('PRAGMA user_version').fetchone()[0]
        finally:
            rag.close()
        assert rag_schema == 4, rag_schema
        result = dict(old_public_version=args.old_version, new_version=args.expected_version,
                      old_binary_sha256=hashlib.sha256(old.read_bytes()).hexdigest(),
                      new_binary_sha256=hashlib.sha256(new.read_bytes()).hexdigest(),
                      public_schema=public_schema, schema_before=args.start_schema, schema_after=args.expected_new_schema,
                      synthetic_schema_staging=(args.start_schema != public_schema),
                      migration_ledger_before=before_ledger, migration_ledger_after=after_ledger,
                      preserved_rows={table: len(rows) for table, rows in before_records.items()},
                      migration_ledger_unchanged=(after_ledger == before_ledger),
                      rag_schema=rag_schema, original_nodes_before=before_nodes,
                      old_external_sender=old_sender, old_model_sender='model',
                      pending_preserved=saved_pending['delivery_status'], new_exec_sender=added,
                      old_and_new_usage='pass', conversation_search='pass', standalone_rag='pass',
                      mock_exec='pass', owner_shutdown_exit=exit_code,
                      mock_response_calls=state['response_calls'], mock_embedding_calls=state['embedding_calls'],
                      environment='native Unix PTY; temporary synthetic stores, public old executable, loopback only')
        (root / 'upgrade-result.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(result, indent=2))
    finally:
        for owner in owners:
            try:
                owner.close(hard=True)
            except (OSError, subprocess.SubprocessError):
                pass
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    main()
