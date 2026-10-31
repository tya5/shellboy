#!/usr/bin/env python3
"""Native CLI checks of receipt, context, and deletion facts on a fresh disposable fixture."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import sqlite3
import struct
import subprocess
import termios
import threading
import time


class Owner:
    def __init__(self, binary, root, env):
        self.root = root
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 100, 0, 0))
        self.process = subprocess.Popen([binary, '--data-dir', str(root / 'data'), '--config', str(root / 'config.toml')], stdin=slave, stdout=slave, stderr=slave, env=env)
        os.close(slave)
        self.stopped = threading.Event()
        self.reader = threading.Thread(target=self.drain, daemon=True)
        self.reader.start()

    def drain(self):
        while not self.stopped.is_set():
            if select.select([self.master], [], [], .1)[0]:
                try:
                    if not os.read(self.master, 65536):
                        break
                except OSError:
                    break

    def ready(self):
        deadline = time.monotonic() + 20
        while not (self.root / 'data/endpoint.json').exists():
            assert self.process.poll() is None, 'Dedicated owner exited during startup'
            assert time.monotonic() < deadline, 'Dedicated owner startup timeout'
            time.sleep(.05)
        time.sleep(.3)

    def close(self):
        try:
            if self.process.poll() is None:
                os.write(self.master, b'\x11')
                try:
                    self.process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    self.process.terminate()
                    self.process.wait(timeout=5)
                    raise
        finally:
            self.stopped.set()
            self.reader.join(timeout=1)
            os.close(self.master)
        assert self.process.returncode == 0, self.process.returncode
        assert not (self.root / 'data/endpoint.json').exists(), 'Endpoint survived normal shutdown'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture-root', type=Path, required=True)
    parser.add_argument('--binary', type=Path, default=Path('target/release/shellboy'))
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    root = args.fixture_root.resolve()
    if not (root / '.shellboy-visual-fixture').is_file() or not (root / 'data/shellboy.db').is_file():
        parser.error('Create a dedicated visual_fixture first')
    if (root / 'data/endpoint.json').exists() or (root / '.observability-cli').exists():
        parser.error('Use a fresh, stopped visual_fixture')
    (root / '.observability-cli').write_text('synthetic-observability-v1\n')
    # All fixture writes precede owner startup. No live DB mutation is used.
    (root / 'config.toml').write_text("default_connection='visual'\ndefault_model='offline-fixture'\n[connections.visual]\nbase_url='http://127.0.0.1:9/v1'\n[ui]\ncopy_backend='osc52'\n")
    external = root / 'synthetic-external.txt'
    external.write_text('Synthetic external attachment must survive Thread deletion.\n')
    original_hash = hashlib.sha256(external.read_bytes()).hexdigest()
    with sqlite3.connect(root / 'data/shellboy.db') as db:
        db.execute('PRAGMA foreign_keys=ON')
        assert db.execute('SELECT count(*) FROM operation_receipt').fetchone()[0] == 0, 'Fixture has been used'
        for thread, parent in [('obs-root', None), ('obs-child', 'obs-root')]:
            db.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?,?,?,?,'visual','offline-fixture','2026-10-25T00:00:11Z')", (thread, parent, str(root), str(root / 'AGENTS.md')))
        db.executescript("""
            INSERT INTO run(id,thread_id,state,created_at,ended_at) VALUES('obs-owner','obs-root','completed','2026-10-25T00:00:00Z','2026-10-25T00:00:10Z');
            INSERT INTO run(id,thread_id,owner_run_id,state,created_at,ended_at,stop_reason) VALUES('obs-owned','obs-child','obs-owner','stopped','2026-10-25T00:00:01Z','2026-10-25T00:00:04.125Z','explicit_stop');
            INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at) VALUES('obs-turn','obs-root','obs-owner','completed','2026-10-25T00:00:01Z','2026-10-25T00:00:03Z');
            INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at,recovery_reason_json) VALUES('obs-failed-turn','obs-child','obs-owned','failed','2026-10-25T00:00:02Z','2026-10-25T00:00:04Z','{"code":"synthetic_failure","message":"synthetic failure"}');
        """)
        for node, kind, turn, body in [
            ('obs-user', 'user', 'obs-turn', {'role': 'user', 'content': 'Synthetic recorded message'}),
            ('obs-answer', 'assistant_response', 'obs-turn', {'output': [{'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': 'Synthetic recorded answer'}]}]}),
            ('obs-summary', 'summary', None, {'text': 'Synthetic source reference'}),
        ]:
            payload = json.dumps(body)
            db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES(?,?,?,'2026-10-25T00:00:01Z',?,?,?)", (node, turn, kind, payload, len(payload.encode()), hashlib.sha256(payload.encode()).hexdigest()))
        db.executescript("""
            INSERT INTO thread_path VALUES('obs-root',0,'obs-user'),('obs-root',1,'obs-answer');
            UPDATE thread SET cursor_node_id='obs-answer' WHERE id='obs-root';
            UPDATE turn SET answer_node_id='obs-answer' WHERE id='obs-turn';
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,result_json) VALUES('obs-receipt','synthetic-recorded-operation','{"kind":"human"}','obs-root','obs-user','obs-owner','obs-turn','applied','2026-10-25T00:00:01Z','2026-10-25T00:00:03Z','{"message_id":"obs-user","thread_id":"obs-root","delivery_status":"accepted","assignment":"new_run","run_id":"obs-owner","turn_id":"obs-turn"}');
            INSERT INTO replacement VALUES('obs-replacement','obs-summary','synthetic-key','1','1','completed','2026-10-25T00:00:04Z');
            INSERT INTO replacement_source VALUES('obs-replacement',0,'obs-user',0,'synthetic-recorded-witness');
            INSERT INTO thread_replacement VALUES('visual-summary','obs-replacement');
        """)
        db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at) VALUES('obs-attachment','obs-user',?,'file','external','2026-10-25T00:00:01Z')", (str(external),))
    binary = str(args.binary.resolve())
    env = {key: value for key, value in os.environ.items() if not key.startswith('SHELLBOY_')}
    env['TERM'] = 'xterm-256color'
    checks = []
    facts = {}

    def cli(label, *words, error=None):
        result = subprocess.run([binary, '--config', str(root / 'config.toml'), '--data-dir', str(root / 'data'), *words], env=env, capture_output=True, text=True, timeout=30)
        value = json.loads(result.stdout)
        assert value['ok'] == (error is None), (label, value)
        assert result.returncode == (0 if error is None else 1), (label, result.returncode)
        if error:
            assert value['error']['code'] == error, (label, value)
        checks.append({'check': label, 'ok': True, **({'expected_error': error} if error else {})})
        return value

    owner = Owner(binary, root, env)
    try:
        owner.ready()
        thread = cli('thread_context', 'thread', 'get', '--thread', 'obs-child')['result']['context']
        assert thread['parent_thread_id'] == 'obs-root' and thread['state'] == 'active'
        assert 'created_at' not in thread and 'started_at' not in thread and 'owner_run_id' not in thread
        run = cli('run_context', 'run', 'get', '--run', 'obs-owned')['result']['context']
        assert run['owner_run_id'] == 'obs-owner' and run['stop_reason'] == 'explicit_stop'
        assert run['lifetime_ms'] == 3125 and run['ended_at'] == '2026-10-25T00:00:04.125Z'
        assert 'started_at' not in run
        turn = cli('turn_context', 'turn', 'get', '--turn', 'obs-failed-turn')['result']['context']
        assert turn['failure_reason']['code'] == 'synthetic_failure' and turn['lifetime_ms'] == 2000
        message = cli('message_context', 'message', 'get', '--message', 'obs-user')['result']['context']
        assert message['state'] == 'applied' and message['run_id'] == 'obs-owner'
        assert message['created_at'] == '2026-10-25T00:00:01Z' and message['updated_at'] == '2026-10-25T00:00:03Z'
        node = cli('history_context', 'history', 'get', '--node', 'obs-user')['result']['context']
        assert node['thread_id'] == 'obs-root' and node['turn_id'] == 'obs-turn'
        facts['contexts'] = {'thread_parent': thread['parent_thread_id'], 'run_owner': run['owner_run_id'], 'closed_run_lifetime_ms': run['lifetime_ms'], 'turn_failure_code': turn['failure_reason']['code'], 'message_state': message['state'], 'invented_started_at': False}
        fork = cli('fork_shared_path', 'thread', 'fork', '--thread', 'obs-root', '--node', 'obs-answer')['result']['thread_id']
        preview = cli('delete_preview_references', 'thread', 'delete-preview', '--thread', 'obs-root')['result']
        assert preview['deletable_now'] and preview['snapshot_only']
        assert preview['selection']['descendant_thread_count'] == 2
        assert preview['references']['shared_path_nodes'] == 2
        assert preview['references']['surviving_replacement_source_nodes'] == 1
        assert preview['references']['external_attachment_reference_count'] == 1
        assert preview['reclaimable_bytes'] is None
        facts['preview'] = {key: preview[key] for key in ['snapshot_only', 'deletable_now', 'selection', 'references', 'reclaimable_bytes']}
        # Drop the first response at the caller boundary, then replay the same mutation.
        first = cli('mutation_first_response_discarded', 'thread', 'set', '--thread', 'visual-composer', '--title', 'Synthetic operation replay')
        operation = first['operation_id']
        original = first['result']
        del first
        replay = cli('mutation_replayed', '--operation-id', operation, 'thread', 'set', '--thread', 'visual-composer', '--title', 'Synthetic operation replay')
        assert replay['result'] == original
        cli('operation_content_conflict', '--operation-id', operation, 'thread', 'set', '--thread', 'visual-composer', '--title', 'different', error='operation_conflict')
        status = cli('operation_receipt', 'operation', 'get', '--operation', operation)['result']
        assert status['state'] == 'completed' and status['current_instance']
        assert status['replay_policy'] == 'return_saved_outcome' and not status['result_uncertain']
        assert status['outcome']['result'] == original
        # New child after preview: delete must resolve the selection again.
        before = cli('preview_before_child_created', 'thread', 'delete-preview', '--thread', 'visual-table', '--descendants')['result']
        assert before['selection']['thread_count'] == 1
        child = cli('create_child_after_preview', 'thread', 'create', '--parent', 'visual-table')['result']['thread_id']
        cli('delete_rechecks_descendants', 'thread', 'delete', '--thread', 'visual-table', '--descendants')
        cli('new_child_deleted', 'thread', 'get', '--thread', child, error='thread_not_found')
        # A single-Thread deletion preserves its child/fork references.
        cli('delete_original_thread_only', 'thread', 'delete', '--thread', 'obs-root')
        cli('seed_child_survives', 'thread', 'get', '--thread', 'obs-child')
        cli('deleted_preview_unavailable', 'thread', 'delete-preview', '--thread', 'obs-root', error='thread_not_found')
        assert cli('fork_still_available', 'thread', 'get', '--thread', fork)['result']['thread_id'] == fork
        assert cli('original_still_readable', 'history', 'get', '--node', 'obs-user')['result']['context']['thread_id'] == 'obs-root'
        assert hashlib.sha256(external.read_bytes()).hexdigest() == original_hash
    finally:
        owner.close()
    owner = Owner(binary, root, env)
    try:
        owner.ready()
        replay = cli('replay_after_owner_restart', '--operation-id', operation, 'thread', 'set', '--thread', 'visual-composer', '--title', 'Synthetic operation replay')
        assert replay['result'] == original
        status = cli('receipt_from_previous_instance', 'operation', 'get', '--operation', operation)['result']
        assert not status['current_instance'] and status['state'] == 'completed'
        assert status['outcome']['result'] == original
        cli('unknown_old_operation_expired', '--operation-id', operation.split(':', 1)[0] + ':synthetic-unknown', 'thread', 'set', '--thread', 'visual-composer', '--title', 'must not apply', error='operation_expired')
        assert hashlib.sha256(external.read_bytes()).hexdigest() == original_hash
        facts['receipt'] = {'state': status['state'], 'replay_policy': status['replay_policy'], 'current_instance_after_restart': status['current_instance'], 'original_outcome_preserved': True}
    finally:
        owner.close()
    # Only synthetic assertion evidence is saved. No cwd, raw terminal output, or private logs.
    artifact = {'mode': 'synthetic_native_release_cli', 'binary_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(), 'api_requests': 0, 'normal_owner_shutdowns': 2, 'checks': checks, 'facts': facts, 'external_attachment_preserved': True, 'limitations': ['Lost-response test discards an already received response; real observer disconnect is covered by runtime_features.', 'Live/stopping blockers and concurrent snapshot behavior are covered by deletion unit tests; this script sends no model requests.']}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(artifact, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'cli_checks': len(checks), 'api_requests': 0, 'normal_owner_shutdowns': 2}))


if __name__ == '__main__':
    main()
