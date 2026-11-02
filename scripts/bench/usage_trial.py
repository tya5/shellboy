#!/usr/bin/env python3
"""Offline agent/relay integration preflight for #151. Never connects to LiteLLM."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading

import compare
import fake_model
import footprint
from meter_relay import Meter, Server, MODEL

compare.MODEL = MODEL
fake_model.MODEL = MODEL
_original_normalize = fake_model.normalize


def inspect_normalize(wire, body):
    messages, tools = _original_normalize(wire, body)
    if wire == 'responses' and any(item.get('type') == 'additional_tools' for item in body.get('input', [])):
        tools['exec_command'] = {'properties': {'cmd': {'type': 'string'}}}
    return messages, tools


fake_model.normalize = inspect_normalize
_original_response = fake_model.responses_body


def custom_response(rid, action, stream):
    if action[0] != 'tool' or action[2] != 'exec_command':
        return _original_response(rid, action, stream)
    command = action[3]['cmd']
    item = {'type': 'custom_tool_call', 'id': f'fc_{rid}', 'call_id': f'call_{rid}',
            'name': 'exec', 'namespace': 'functions',
            'input': 'text((await tools.exec_command(' + json.dumps({'cmd': command}) + ')).output)',
            'status': 'completed'}
    usage = {'input_tokens': 1, 'input_tokens_details': {'cached_tokens': 0}, 'output_tokens': 1,
             'output_tokens_details': {'reasoning_tokens': 0}, 'total_tokens': 2}
    response = {'id': f'resp_{rid}', 'object': 'response', 'model': MODEL, 'status': 'completed',
                'output': [item], 'usage': usage}
    events = [('response.created', {'type': 'response.created', 'response': dict(response, output=[], usage=None)}),
              ('response.output_item.added', {'type': 'response.output_item.added', 'output_index': 0, 'item': item}),
              ('response.output_item.done', {'type': 'response.output_item.done', 'output_index': 0, 'item': item}),
              ('response.completed', {'type': 'response.completed', 'response': response})]
    return 'text/event-stream', fake_model.sse(events)


fake_model.responses_body = custom_response


def profile(root, binary, ports):
    root, binary = root.resolve(), binary.resolve()
    parts = ['(version 1)', '(allow default)', '(deny network-outbound)', '(deny file-write*)']
    for path in (Path.home().resolve(), Path('/Volumes'), Path('/private/var/folders')):
        parts.append(f'(deny file-read* (subpath {json.dumps(str(path))}))')
    parts += [f'(allow network-outbound (remote ip "localhost:{port}"))' for port in ports]
    parts.append(f'(allow network-outbound (remote unix-socket (subpath {json.dumps(str(root))})))')
    parts += [f'(allow file-read-metadata (literal {json.dumps(str(parent))}))'
              for target in (root, binary) for parent in target.parents]
    parts += [f'(allow file-read* (subpath {json.dumps(str(root))}))',
              f'(allow file-read* (literal {json.dumps(str(binary))}))',
              f'(allow file-write* (subpath {json.dumps(str(root))}))',
              '(allow file-write* (literal "/dev/null"))']
    return ' '.join(parts)


def wrapper(root, binary, ports):
    path = root / 'sandboxed-agent'
    command = ['/usr/bin/sandbox-exec', '-p', profile(root, binary, ports), str(binary)]
    path.write_text('#!/bin/sh\nexec ' + ' '.join(shlex.quote(x) for x in command) + ' "$@"\n')
    path.chmod(0o700)
    return path


def shellboy(agent, root, origin, workspace):
    env = footprint.isolated_env(root)
    runtime = root / 'rt'
    runtime.mkdir(mode=0o700)
    env['XDG_RUNTIME_DIR'] = str(runtime)
    config = root / 'config.toml'
    config.write_text('default_connection="fixture"\ndefault_model="' + MODEL + '"\n'
                      '[connections.fixture]\nbase_url=' + json.dumps(origin + '/v1') + '\n'
                      'response_max_retries=0\n[ui]\ncopy_backend="osc52"\nbell=false\n')
    owner = subprocess.Popen([str(agent), 'serve', '--config', str(config), '--data-dir', str(root / 'data')],
                             cwd=root, env=env, stdout=subprocess.PIPE, stderr=(root / 'shellboy.stderr').open('w'),
                             start_new_session=True)
    try:
        if footprint.read_json_line(owner).get('event') != 'ready':
            raise RuntimeError('owner readiness failed')
        thread = footprint.cli(agent, root, env, 'thread', 'create', '--cwd', str(workspace))['thread_id']
        for prompt in footprint.PROMPTS:
            message = footprint.cli(agent, root, env, 'message', 'send', '--thread', thread, '--text', prompt)
            waited = footprint.cli(agent, root, env, 'wait', '--conditions-json', json.dumps([
                {'type': 'run_ended', 'run_id': message['run_id']}]), '--timeout-ms', '180000')
            if waited['status'] != 'satisfied':
                raise RuntimeError('turn did not finish')
            turn = footprint.cli(agent, root, env, 'turn', 'get', '--turn', message['turn_id'])
            if turn['state'] != 'completed':
                raise RuntimeError('turn did not complete')
        footprint.cli(agent, root, env, 'shutdown')
        owner.wait(timeout=20)
        if owner.returncode:
            raise RuntimeError('owner shutdown failed')
    finally:
        compare.kill_group(owner)
        owner.stdout.close()


def other(name, binary, root, origin, workspace, port):
    cls = compare.ADAPTERS[name]
    agent = cls(binary)
    env = agent.prepare(root, origin, [workspace])
    if name == 'claude-code':
        env['CLAUDE_CODE_MAX_RETRIES'] = '0'
    if name == 'codex':
        with (Path(env['CODEX_HOME']) / 'config.toml').open('a') as config:
            config.write('\n[features]\nincremental_tools = false\ncode_mode_host = false\n')
    # OpenCode's server port is fixed before sandbox launch, so it can be the
    # only permitted peer other than the relay.
    old_port = compare.free_port
    compare.free_port = lambda: port
    sessions = []
    try:
        sessions = agent.start(root, env, [workspace])
        for prompt in footprint.PROMPTS:
            sessions[0].turn(prompt)
    finally:
        for session in sessions:
            session.close()
        compare.free_port = old_port
        agent.clean()


def main():
    if sys.platform != 'darwin':
        raise SystemExit('This disposable preflight needs macOS sandbox-exec')
    binaries = {'shellboy': Path(sys.argv[1]).resolve(strict=True),
                'codex': Path(shutil.which('codex')).resolve(strict=True),
                'claude-code': Path(shutil.which('claude')).resolve(strict=True),
                'opencode': Path(sys.argv[2]).resolve(strict=True)}
    results = {}
    for name, binary in binaries.items():
        with tempfile.TemporaryDirectory(prefix='ut-', dir='/private/tmp') as scratch, fake_model.FakeModel(1) as fake:
            root = Path(scratch).resolve()
            workspace = compare.make_workspace(root / 'workspace')
            fake.traffic.set_workspaces([workspace])
            ledger = root / 'ledger'
            ledger.touch(mode=0o600, exist_ok=False)
            meter = Meter(fake.server_port, ledger)
            meter.set_agent(name)
            relay = Server(meter)
            worker = threading.Thread(target=relay.serve_forever, daemon=True)
            worker.start()
            opencode_port = compare.free_port()
            isolated = wrapper(root, binary, [relay.server_port, opencode_port] if name == 'opencode'
                               else [relay.server_port])
            try:
                if name == 'shellboy':
                    shellboy(isolated, root, f'http://127.0.0.1:{relay.server_port}', workspace)
                else:
                    other(name, isolated, root, f'http://127.0.0.1:{relay.server_port}', workspace, opencode_port)
                with meter.lock:
                    pass
                if meter.budget.stopped or not fake.workload_complete():
                    raise RuntimeError('relay stopped or fake workload incomplete')
                results[name] = {'status': 'passed', 'requests': len(meter.budget.rows)}
            except Exception as error:
                logs = {}
                for path in root.glob('*.stderr'):
                    logs[path.name] = path.read_text(errors='replace')[-1500:]
                results[name] = {'status': 'failed', 'reason': f'{type(error).__name__}: {error}',
                                 'relay_stop': meter.budget.stopped, 'fake_stats': fake.stats(), 'logs': logs}
            finally:
                relay.shutdown()
                relay.server_close()
                worker.join()
            print(json.dumps({name: results[name]}), flush=True)
    if any(result['status'] != 'passed' for result in results.values()):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
