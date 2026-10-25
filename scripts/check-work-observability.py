#!/usr/bin/env python3
"""Native release CLI/TUI smoke on a fresh, synthetic, API-free owner."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import sqlite3
import struct
import subprocess
import termios
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/release/shellboy'))
    parser.add_argument('--root', type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    binary = str(args.binary.resolve())
    config = root / 'config.toml'
    config.write_text('default_connection="fixture"\ndefault_model="current-model"\n'
                      '[connections.fixture]\nbase_url="http://127.0.0.1:9/v1"\n')
    data = root / 'data'
    endpoint = data / 'endpoint.json'
    env = {key: value for key, value in os.environ.items() if not key.startswith('SHELLBOY_')}
    env['TERM'] = 'xterm-256color'
    evidence = {'external_api_calls': 0, 'fixture': 'synthetic', 'checks': {}}

    def start():
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 120, 0, 0))
        process = subprocess.Popen([binary, '--data-dir', str(data), '--config', str(config)],
                                   stdin=slave, stdout=slave, stderr=slave, env=env)
        os.close(slave)
        output = bytearray()
        stopped = threading.Event()

        def drain():
            while not stopped.is_set():
                if select.select([master], [], [], .05)[0]:
                    try:
                        block = os.read(master, 65536)
                        if not block:
                            break
                        # Bounded capture; the fixture is small.
                        if len(output) < 4 * 1024 * 1024:
                            output.extend(block)
                    except OSError:
                        break

        reader = threading.Thread(target=drain, daemon=True)
        reader.start()
        deadline = time.monotonic() + 20
        while not endpoint.exists():
            assert process.poll() is None, 'Fixture owner exited during startup'
            assert time.monotonic() < deadline, 'Fixture owner startup timeout'
            time.sleep(.05)
        time.sleep(.3)
        return process, master, output, stopped, reader

    def stop(owner):
        process, master, output, stopped, reader = owner
        try:
            os.write(master, b'\x11')  # Ctrl+Q, graceful shutdown.
            assert process.wait(timeout=20) == 0, 'Fixture owner failed to stop'
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            stopped.set()
            reader.join(timeout=2)
            os.close(master)

    def cli(label, *words, ok=True):
        result = subprocess.run([binary, '--data-dir', str(data), *words],
                                env=env, capture_output=True, text=True, timeout=25)
        value = json.loads(result.stdout)
        assert value['ok'] is ok, (label, value)
        assert result.returncode == (0 if ok else 1), (label, result.returncode)
        evidence['checks'][label] = value
        return value.get('result', value.get('error'))

    owner = start()
    stop(owner)
    # Seeding is allowed only while this dedicated owner is stopped.
    assert not endpoint.exists()
    stamp = '2026-10-25T00:00:00Z'
    ended = '2026-10-25T00:01:00Z'
    payload = json.dumps({'text': 'Synthetic request evidence'})
    command = 'printf WORK_STATUS_NATIVE_V1'
    with sqlite3.connect(data / 'shellboy.db') as db:
        db.execute('PRAGMA foreign_keys=ON')
        for thread, parent, title in [('parent', None, 'Native parent'),
                                      ('child', 'parent', 'Native child')]:
            db.execute('INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?,?,?,?,?,?,?,?)',
                       (thread, parent, title, str(root), str(root / 'AGENTS.md'), 'fixture', 'current-model', stamp))
        for run, thread, parent, state in [('root-run', 'parent', None, 'completed'),
                                            ('child-run', 'child', 'root-run', 'failed')]:
            db.execute('INSERT INTO run(id,thread_id,owner_run_id,state,created_at,ended_at) VALUES(?,?,?,?,?,?)',
                       (run, thread, parent, state, stamp, ended))
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at) VALUES('turn','parent','root-run','completed',?,?)", (stamp, ended))
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) VALUES('request','turn','assistant',?,?,?)", (stamp, payload, len(payload)))
        db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('parent',0,'request')")
        db.execute("UPDATE thread SET cursor_node_id='request' WHERE id='parent'")
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at,ended_at) VALUES('attempt','turn',0,0,'fixture','actual-model',0,'completed','{}',?,?)", (stamp, ended))
        db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id) VALUES('attempt',0,'function_call','completed','{}','request')")
        db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES('group','attempt','turn','completed',?)", (stamp,))
        db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id) VALUES('group','call',0,'shell',?,'completed','execution')", (json.dumps({'command': command}),))
        db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at) VALUES('execution','root-run','turn','execution','exited',?,?,'UTF-8','UTF-8','closed',1,0,1,?,?)",
                   (str(root), json.dumps({'command': command, 'spawned': True}), stamp, ended))
        db.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at) VALUES('execution',0,'stdout',0,?,?)", (b'WORK_STATUS_NATIVE_V1', ended))

    owner = start()
    try:
        assert cli('current', 'status')['items'] == []
        status = cli('all', 'status', '--all')
        assert len(status['items']) == 5
        shell = next(item for item in status['items'] if item['kind'] == 'shell')
        assert shell['execution_id'] == 'execution'
        assert not any(target['command'] == 'operation.get' for target in shell['targets'])
        attempt = next(item for item in status['items'] if item['kind'] == 'attempt')
        assert attempt['attempt_model'] == 'actual-model'
        assert attempt['current_model'] == 'current-model'
        child = cli('child', 'status', '--thread', 'child', '--all')['items']
        assert len(child) == 1 and child[0]['owner_run_id'] == 'root-run'
        assert len(cli('failed', 'status', '--state', 'failed')['items']) == 1
        assert len(cli('since', 'status', '--all', '--since', '2026-10-25T09:01:00+09:00')['items']) == 5
        first = cli('page-1', 'status', '--all', '--limit', '2')
        second = cli('page-2', 'status', '--all', '--limit', '2', '--cursor', first['next_cursor'])
        third = cli('page-3', 'status', '--all', '--limit', '2', '--cursor', second['next_cursor'])
        assert len({item['id'] for page in [first, second, third] for item in page['items']}) == 5
        error = cli('wrong-cursor', 'status', '--thread', 'child', '--all', '--cursor', first['next_cursor'], ok=False)
        assert error['code'] == 'cursor_expired' and error['hint']
        history = cli('history', 'history', 'list', '--thread', 'parent', '--run', 'root-run', '--status', 'exited')
        link = history['items'][0]['executions'][0]
        assert link['execution_id'] == 'execution' and link['cwd'] == str(root)
        assert link['request_node_id'] == 'request'
        assert cli('shell-get', 'shell', 'get', '--execution', link['execution_id'])['state'] == 'exited'
        assert cli('shell-read', 'shell', 'read', '--execution', link['execution_id'])['text'] == 'WORK_STATUS_NATIVE_V1'
        cli('original', 'history', 'get', '--node', link['request_node_id'])
        os.write(owner[1], b'/status all\r')
        deadline = time.monotonic() + 10
        def rendered_text():
            text = bytes(owner[2]).decode('utf-8', errors='replace')
            text = re.sub(r'\x1b\][^\x1b\x07]*(?:\x07|\x1b\\)', '', text)
            return re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text)

        while not ('┌作業状態' in rendered_text() and 'run failed: Native child' in rendered_text()):
            assert owner[0].poll() is None, 'Fixture owner exited'
            assert time.monotonic() < deadline, 'TUI work view did not render'
            time.sleep(.05)
        # A real PTY supplies this display; read/navigation itself is covered by Rust bridge tests.
        evidence['tui_status_rendered'] = True
        os.write(owner[1], b'\x1b')
        time.sleep(.1)
        for words in [('shell', 'get'), ('shell', 'read'), ('wait',), ('thread', 'create'), ('status',)]:
            result = subprocess.run([binary, *words, '--help'], env=env, capture_output=True, text=True, timeout=15)
            assert result.returncode == 0 and 'shellboy ' in result.stdout
        evidence['help_examples'] = 5
    finally:
        stop(owner)
        (root / 'terminal.ansi').write_bytes(owner[2])
    evidence['owner_exit'] = 0
    (root / 'evidence.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'ok': True, 'cli_checks': len(evidence['checks']), 'help_checks': 5,
                      'tui': True, 'external_api_calls': 0, 'evidence': str(root / 'evidence.json')}))


if __name__ == '__main__':
    main()
