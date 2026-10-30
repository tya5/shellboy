#!/usr/bin/env python3
"""Synthetic public-0.3.0 -> candidate upgrade on an owned Unix PTY.

Only supplied binaries, an empty fixture directory and loopback endpoints are
used. This never locates or modifies a user's owner/store/installed executable.
"""
import argparse
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import pty
import shlex
import signal
import sqlite3
import struct
import subprocess
import termios
import threading
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--old', type=Path, required=True)
    parser.add_argument('--new', type=Path, required=True)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--expected-version', default='0.4.0')
    args = parser.parse_args()
    old, new = args.old.resolve(), args.new.resolve()
    assert subprocess.check_output([str(old), '--version'], text=True).strip() == 'shellboy 0.3.0'
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
                command = ' '.join(shlex.quote(str(item)) for item in [old, '--data-dir', data, 'message', 'send', '--thread', state['recovery_thread'], '--text', 'SYNTHETIC_PENDING_FROM_030'])
                return self.send(200, {'id': 'synthetic-tool', 'status': 'completed', 'output': [{'type': 'function_call', 'call_id': 'queue-pending', 'name': 'shell', 'arguments': json.dumps({'command': command})}]})
            return self.send(200, response('SYNTHETIC_ANSWER retained conversation text'))

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    config.write_text(f'default_connection="fixture"\ndefault_model="fixture-model"\n[connections.fixture]\nbase_url="http://127.0.0.1:{server.server_port}/v1"\nresponse_max_retries=0\n[embedding]\nconnection="fixture"\nmodel="fixture-embedding"\n', encoding='utf-8')

    class Owner:
        def __init__(self, binary, store):
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
        assert sql(data, 'PRAGMA user_version') == [(14,)]
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
        pending = sql(data, "SELECT r.message_node_id,r.state FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=? AND n.payload_json LIKE '%SYNTHETIC_PENDING_FROM_030%'", (recovery,))
        assert len(pending) == 1 and pending[0][1] in ('held', 'accepted'), pending
        materials = root / 'materials'
        materials.mkdir()
        (materials / 'document.txt').write_text('SYNTHETIC_RAG_CONTENT retained extracted text', encoding='utf-8')
        cli(old, data, 'rag', 'init')
        cli(old, data, 'rag', 'source', 'add', materials, '--name', 'synthetic-documents')
        cli(old, data, 'rag', 'index', '--source', 'synthetic-documents', '--mode', 'keyword')
        before_ledger = sql(data, 'SELECT * FROM schema_migration ORDER BY version')
        before_nodes = sql(data, 'SELECT count(*) FROM node')[0][0]
        previous.close(hard=True)
        owners.remove(previous)
        upgraded.mkdir()
        for filename in ['shellboy.db', 'rag.db']:
            source_path = data / filename
            # All fixture writers are reaped. A checkpointed WAL-mode RAG file
            # can lack sidecars; immutable permits read-only backup without
            # creating them. Never ignore an existing conversation WAL.
            immutable = '' if Path(str(source_path) + '-wal').exists() else '&immutable=1'
            source = sqlite3.connect(source_path.as_uri() + '?mode=ro' + immutable, uri=True)
            destination = sqlite3.connect(upgraded / filename)
            try:
                source.backup(destination)
            finally:
                destination.close()
                source.close()
        current = Owner(new, upgraded)
        owners.append(current)
        ready(new, upgraded)
        assert sql(upgraded, 'PRAGMA user_version') == [(14,)]
        assert sql(upgraded, 'SELECT * FROM schema_migration ORDER BY version') == before_ledger
        assert sql(upgraded, 'PRAGMA integrity_check') == [('ok',)]
        assert sql(upgraded, 'PRAGMA foreign_key_check') == []
        assert 'SYNTHETIC_ANSWER' in json.dumps(cli(new, upgraded, 'history', 'search', '--thread', seed, '--query', 'SYNTHETIC_ANSWER'))
        old_sender = cli(new, upgraded, 'message', 'get', '--message', first)['result']['sender']
        assert old_sender['kind'] == 'unknown', old_sender
        saved_pending = cli(new, upgraded, 'message', 'get', '--message', pending[0][0])['result']
        assert saved_pending['delivery_status'] in ('held', 'accepted')
        assert saved_pending['sender']['kind'] == 'model' and saved_pending['sender']['thread_id'] == sender
        usage = cli(new, upgraded, 'usage', 'get', '--thread', seed)['result']
        assert usage['model_attempts']['completed']['input_tokens']['known_sum'] == 11
        assert 'SYNTHETIC_RAG_CONTENT' in json.dumps(cli(new, upgraded, 'rag', 'search', '--scope', 'source:synthetic-documents', '--query', 'SYNTHETIC_RAG_CONTENT'))
        schema = root / 'result.schema.json'
        schema.write_text('{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}', encoding='utf-8')
        execution = subprocess.run([str(new), '--data-dir', str(upgraded), 'exec', '--schema', str(schema), '--thread', seed, 'UPGRADE_EXEC'], cwd=work, env=env, capture_output=True, text=True, timeout=90)
        assert execution.returncode == 0, (execution.stdout, execution.stderr)
        assert json.loads(execution.stdout) == {'value': 'upgraded mock exec'}
        execution_facts = json.loads(execution.stderr)
        added = cli(new, upgraded, 'message', 'get', '--message', execution_facts['message_id'])['result']['sender']
        assert added == {'kind': 'external_cli', 'label': 'shellboy exec'}, added
        assert cli(new, upgraded, 'usage', 'get', '--thread', seed)['result']['model_attempts']['completed']['count'] == 2
        exit_code = current.close()
        owners.remove(current)
        assert exit_code == 0
        rag_path = upgraded / 'rag.db'
        rag_immutable = '' if Path(str(rag_path) + '-wal').exists() else '&immutable=1'
        rag = sqlite3.connect(rag_path.as_uri() + '?mode=ro' + rag_immutable, uri=True)
        try:
            rag_schema = rag.execute('PRAGMA user_version').fetchone()[0]
        finally:
            rag.close()
        result = dict(old_public_version='0.3.0', new_version=args.expected_version,
                      old_binary_sha256=hashlib.sha256(old.read_bytes()).hexdigest(),
                      new_binary_sha256=hashlib.sha256(new.read_bytes()).hexdigest(),
                      schema_before=14, schema_after=14, migration_ledger_unchanged=True,
                      rag_schema=rag_schema, original_nodes_before=before_nodes,
                      old_external_sender='unknown', old_model_sender='model',
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
