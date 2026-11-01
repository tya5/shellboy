#!/usr/bin/env python3
"""#116 stage 2: Linux PSS and PTY startup of several coding agents on one fake model.

Every agent is pointed at the same deterministic loopback model (fake_model.py)
and runs the stage-1 workload (footprint.PROMPTS) in N concurrent sessions for
five rounds. After each round the PSS of each agent's whole process tree is
recorded. A separate PTY run records time to first visible text and to the
echo of a typed probe. No credentials, paid APIs or performance thresholds.

Each adapter drives the agent's own documented headless shape (the same shapes
as jcode's scripts/bench_headless_memory.py): Shellboy and opencode run one
server hosting N sessions; Claude Code and Codex CLI run one process per session.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import pty
import queue
import re
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from urllib.request import ProxyHandler, Request, build_opener

sys.path.insert(0, str(Path(__file__).resolve().parent))
import footprint  # noqa: E402
from fake_model import MODEL, FakeModel  # noqa: E402

PROMPTS, ANSWERS = footprint.PROMPTS, footprint.ANSWERS
TURN_TIMEOUT = 180
DUMMY_KEY = 'bench-dummy-key-not-a-credential-0000'
COLS, ROWS = 100, 36
http = build_opener(ProxyHandler({}))


def log(message):
    print(f'[{time.strftime("%H:%M:%S")}] {message}', file=sys.stderr, flush=True)


def make_workspace(path):
    (path / 'src').mkdir(parents=True)
    (path / 'README.md').write_text('# Synthetic repository\nBenchmark fixture.\n')
    (path / 'Cargo.toml').write_text('[package]\nname="synthetic"\n')
    (path / 'src/main.rs').write_text('fn main() {}\n')
    return path


def sample_trees(roots, proc=Path('/proc')):
    """PSS of the union of each root's transitive descendants (stage-1 rules).

    Some agents keep short-lived helpers running between turns, so up to ten
    snapshots (50 ms apart) are tried instead of stage 1's three; the number
    used is recorded, and persistent churn is still an error, never a sample.
    """
    changed = set()
    for attempt in range(10):
        if attempt:
            time.sleep(.05)
        before = {}
        for root in roots:
            before.update(footprint.process_tree(root, proc))
        rows = []
        try:
            for pid, identity in sorted(before.items()):
                lines = (proc / str(pid) / 'smaps_rollup').read_text().splitlines()
                pss = [line.split() for line in lines if line.startswith('Pss:')]
                if len(pss) != 1 or pss[0][2] != 'kB':
                    raise RuntimeError('Missing or invalid smaps_rollup Pss')
                comm = (proc / str(pid) / 'comm').read_text().strip()
                rows.append({'pid': pid, 'comm': comm, 'starttime_ticks': identity[1],
                             'pss_kib': int(pss[0][1])})
            after = {}
            for root in roots:
                after.update(footprint.process_tree(root, proc))
            if before == after:
                return {'pss_kib': sum(row['pss_kib'] for row in rows), 'process_count': len(rows),
                        'snapshot_attempts': attempt + 1, 'processes': rows}
            changed |= set(before) ^ set(after)
        except (FileNotFoundError, ProcessLookupError) as error:
            changed.add(str(error)[:80])
            continue
    names = sorted({row['comm'] for row in rows if row['pid'] in changed})
    raise RuntimeError(f'Process trees changed during all ten PSS snapshots: {sorted(map(str, changed))[:10]} {names}')


def no_pss(_roots):
    return {'pss_kib': None, 'process_count': None, 'processes': [], 'skipped': 'smoke run'}


class LineProc:
    """A child speaking newline-delimited JSON over stdio, read by one thread."""

    def __init__(self, argv, cwd, env, stderr):
        self.proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=stderr, text=True,
                                     bufsize=1, start_new_session=True)
        self.lines = queue.Queue()
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send(self, value):
        self.proc.stdin.write(json.dumps(value) + '\n')
        self.proc.stdin.flush()

    def events(self, timeout=TURN_TIMEOUT):
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('agent turn watchdog')
            try:
                line = self.lines.get(timeout=remaining)
            except queue.Empty:
                raise TimeoutError('agent turn watchdog') from None
            if line is None:
                raise RuntimeError(f'agent exited with {self.proc.wait()}')
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        kill_group(self.proc)


def kill_group(proc):
    if proc.poll() is None:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(timeout=10)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            pass
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass


def base_env(root):
    env = footprint.isolated_env(root)
    env['NO_COLOR'] = '1'
    return env


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def http_json(method, url, body=None, timeout=TURN_TIMEOUT):
    data = json.dumps(body).encode() if body is not None else None
    request = Request(url, data=data, method=method, headers={'content-type': 'application/json'})
    with http.open(request, timeout=timeout) as response:
        raw = response.read()
    return json.loads(raw) if raw else None


# ---------------------------------------------------------------------------
# Adapters. `config` documents every non-default setting; `prepare` writes it.
# ---------------------------------------------------------------------------

class Agent:
    name = ''
    wire = ''
    shape = ''
    config = {}

    def __init__(self, binary):
        self.binary = binary
        self.cleanup = []

    def clean(self):
        while self.cleanup:
            shutil.rmtree(self.cleanup.pop(), ignore_errors=True)

    def prepare(self, root, origin, workspaces):
        return base_env(root)

    def version(self, env):
        result = subprocess.run([str(self.binary), '--version'], env=env, capture_output=True,
                                text=True, timeout=60)
        return (result.stdout + result.stderr).strip()[:300]

    def tui_argv(self, root):
        return [str(self.binary)]


class ClaudeCode(Agent):
    name, wire = 'claude-code', 'messages'
    shape = 'one `claude -p --input-format stream-json` process per session'
    config = {'env': {'ANTHROPIC_BASE_URL': '<fake origin>', 'ANTHROPIC_API_KEY': '<dummy>',
                      'ANTHROPIC_MODEL': MODEL, 'ANTHROPIC_SMALL_FAST_MODEL': MODEL,
                      'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1', 'DISABLE_AUTOUPDATER': '1'},
              'flags': ['--permission-mode', 'bypassPermissions'],
              '~/.claude.json': 'onboarding done, dummy key approved, workspaces trusted (TUI only)'}

    def prepare(self, root, origin, workspaces):
        env = base_env(root)
        env.update(ANTHROPIC_BASE_URL=origin, ANTHROPIC_API_KEY=DUMMY_KEY, ANTHROPIC_MODEL=MODEL,
                   ANTHROPIC_SMALL_FAST_MODEL=MODEL, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1',
                   DISABLE_AUTOUPDATER='1')
        state = {'hasCompletedOnboarding': True, 'theme': 'dark',
                 'customApiKeyResponses': {'approved': [DUMMY_KEY[-20:]], 'rejected': []},
                 'projects': {str(path): {'hasTrustDialogAccepted': True} for path in workspaces}}
        (Path(env['HOME']) / '.claude.json').write_text(json.dumps(state))
        return env

    def start(self, root, env, workspaces):
        return [ClaudeSession(self.binary, path, env, root / f'claude-{i}.stderr')
                for i, path in enumerate(workspaces)]


class ClaudeSession:
    def __init__(self, binary, cwd, env, stderr):
        self.io = LineProc([str(binary), '-p', '--input-format', 'stream-json', '--output-format',
                            'stream-json', '--verbose', '--permission-mode', 'bypassPermissions'],
                           cwd, env, stderr.open('w'))
        self.pids = [self.io.proc.pid]

    def turn(self, prompt):
        self.io.send({'type': 'user', 'message': {'role': 'user',
                                                  'content': [{'type': 'text', 'text': prompt}]}})
        for event in self.io.events():
            if event.get('type') == 'result':
                if event.get('is_error'):
                    raise RuntimeError(f'claude result error: {str(event.get("result"))[:200]}')
                return str(event.get('result'))

    def close(self):
        self.io.close()


class Codex(Agent):
    name, wire = 'codex', 'responses'
    shape = 'one `codex app-server` (JSON-RPC over stdio) per session, one thread each'
    config = {'CODEX_HOME/config.toml': {
        'model': MODEL, 'model_provider': 'bench', 'approval_policy': 'never',
        'sandbox_mode': 'danger-full-access',
        'model_providers.bench': {'base_url': '<fake origin>/v1', 'wire_api': 'responses',
                                  'env_key': 'BENCH_DUMMY_API_KEY', 'request_max_retries': 0,
                                  'stream_max_retries': 0},
        'projects.<workspace>.trust_level': 'trusted (TUI only)'}}

    def prepare(self, root, origin, workspaces):
        env = base_env(root)
        home = root / 'codex-home'
        home.mkdir()
        env.update(CODEX_HOME=str(home), BENCH_DUMMY_API_KEY=DUMMY_KEY)
        lines = [f'model = "{MODEL}"', 'model_provider = "bench"', 'approval_policy = "never"',
                 'sandbox_mode = "danger-full-access"', '',
                 '[model_providers.bench]', 'name = "bench"', f'base_url = "{origin}/v1"',
                 'wire_api = "responses"', 'env_key = "BENCH_DUMMY_API_KEY"',
                 'request_max_retries = 0', 'stream_max_retries = 0', '']
        for path in workspaces:
            lines += [f'[projects.{json.dumps(str(path))}]', 'trust_level = "trusted"', '']
        (home / 'config.toml').write_text('\n'.join(lines))
        return env

    def start(self, root, env, workspaces):
        return [CodexSession(self.binary, path, env, root / f'codex-{i}.stderr')
                for i, path in enumerate(workspaces)]


class CodexSession:
    def __init__(self, binary, cwd, env, stderr):
        self.io = LineProc([str(binary), 'app-server'], cwd, env, stderr.open('w'))
        self.pids = [self.io.proc.pid]
        self.next_id = 1
        self.request('initialize', {'clientInfo': {'name': 'shellboy-bench', 'version': '1'}})
        self.io.send({'jsonrpc': '2.0', 'method': 'initialized'})
        result = self.request('thread/start', {'cwd': str(cwd), 'model': MODEL,
                                               'approvalPolicy': 'never',
                                               'sandbox': 'danger-full-access'})
        self.thread_id = result['thread']['id']

    def request(self, method, params):
        rid = self.next_id
        self.next_id += 1
        self.io.send({'jsonrpc': '2.0', 'id': rid, 'method': method, 'params': params})
        for event in self.io.events(timeout=120):
            if event.get('id') == rid and 'method' not in event:
                if 'error' in event:
                    raise RuntimeError(f'codex {method}: {event["error"]}')
                return event.get('result') or {}

    def turn(self, prompt):
        rid = self.next_id
        self.next_id += 1
        self.io.send({'jsonrpc': '2.0', 'id': rid, 'method': 'turn/start', 'params': {
            'threadId': self.thread_id, 'input': [{'type': 'text', 'text': prompt}]}})
        text = ''
        for event in self.io.events():
            method = event.get('method', '')
            if 'id' in event and method:
                raise RuntimeError(f'unexpected codex server request {method}')
            if event.get('id') == rid and 'error' in event:
                raise RuntimeError(f'codex turn/start: {event["error"]}')
            params = event.get('params') or {}
            if method == 'item/completed' and (params.get('item') or {}).get('type') == 'agentMessage':
                text = str(params['item'].get('text'))
            if method == 'error':
                raise RuntimeError(f'codex error: {str(params)[:300]}')
            if method == 'turn/completed':
                turn = params.get('turn') or {}
                if turn.get('status') not in (None, 'completed'):
                    raise RuntimeError(f'codex turn {turn.get("status")}: {str(turn.get("error"))[:200]}')
                return text

    def close(self):
        self.io.close()


class OpenCode(Agent):
    name, wire = 'opencode', 'chat'
    shape = 'one `opencode serve` hosting N sessions over HTTP (directory per session)'
    config = {'XDG_CONFIG_HOME/opencode/opencode.json': {
        'model': f'bench/{MODEL}', 'small_model': f'bench/{MODEL}', 'autoupdate': False,
        'share': 'disabled', 'permission': 'allow (edit, bash, webfetch)',
        'provider.bench': {'npm': '@ai-sdk/openai-compatible', 'options.baseURL': '<fake origin>/v1',
                           'options.apiKey': '<dummy>'}},
        'env': {'OPENCODE_DISABLE_AUTOUPDATE': '1', 'OPENCODE_DISABLE_MODELS_FETCH': '1',
                'OPENCODE_DISABLE_LSP_DOWNLOAD': '1'}}

    def prepare(self, root, origin, workspaces):
        env = base_env(root)
        env.update(OPENCODE_DISABLE_AUTOUPDATE='1', OPENCODE_DISABLE_MODELS_FETCH='1',
                   OPENCODE_DISABLE_LSP_DOWNLOAD='1', XDG_STATE_HOME=str(root / 'home/state'),
                   XDG_CACHE_HOME=str(root / 'home/cache'))
        config = {'$schema': 'https://opencode.ai/config.json', 'model': f'bench/{MODEL}',
                  'small_model': f'bench/{MODEL}', 'autoupdate': False, 'share': 'disabled',
                  'permission': {'edit': 'allow', 'bash': 'allow', 'webfetch': 'allow'},
                  'provider': {'bench': {'npm': '@ai-sdk/openai-compatible', 'name': 'bench',
                                         'options': {'baseURL': f'{origin}/v1', 'apiKey': DUMMY_KEY},
                                         'models': {MODEL: {'name': MODEL}}}}}
        path = Path(env['XDG_CONFIG_HOME']) / 'opencode'
        path.mkdir(parents=True)
        (path / 'opencode.json').write_text(json.dumps(config, indent=2))
        return env

    def start(self, root, env, workspaces):
        return OpenCodeServer(self.binary, root, env, workspaces).sessions


class OpenCodeServer:
    def __init__(self, binary, root, env, workspaces):
        port = free_port()
        self.base = f'http://127.0.0.1:{port}'
        self.proc = subprocess.Popen([str(binary), 'serve', '--port', str(port), '--hostname', '127.0.0.1'],
                                     cwd=root, env=env, stdin=subprocess.DEVNULL,
                                     stdout=(root / 'opencode.stdout').open('w'),
                                     stderr=(root / 'opencode.stderr').open('w'), start_new_session=True)
        deadline = time.monotonic() + 120
        while True:
            try:
                http_json('GET', f'{self.base}/session', timeout=5)
                break
            except OSError:
                if self.proc.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('opencode server not ready') from None
                time.sleep(.25)
        self.sessions = []
        for path in workspaces:
            query = f'directory={path}'
            info = http_json('POST', f'{self.base}/session?{query}', {})
            self.sessions.append(OpenCodeSession(self, info['id'], query))
        self.sessions[0].pids = [self.proc.pid]


class OpenCodeSession:
    def __init__(self, server, sid, query):
        self.server, self.sid, self.query, self.pids = server, sid, query, []

    def turn(self, prompt):
        reply = http_json('POST', f'{self.server.base}/session/{self.sid}/message?{self.query}',
                          {'parts': [{'type': 'text', 'text': prompt}],
                           'model': {'providerID': 'bench', 'modelID': MODEL}})
        info = (reply or {}).get('info') or {}
        if info.get('error'):
            raise RuntimeError(f'opencode error: {str(info["error"])[:300]}')
        texts = [part.get('text', '') for part in (reply or {}).get('parts') or [] if part.get('type') == 'text']
        return texts[-1] if texts else ''

    def close(self):
        if self.pids:
            kill_group(self.server.proc)


class JCode(Agent):
    name, wire = 'jcode', 'chat'
    shape = 'one `jcode serve` hosting N sessions over its debug socket'
    config = {'env': {'JCODE_HOME': '<temp>', 'JCODE_RUNTIME_DIR': '<temp>', 'JCODE_TEMP_SERVER': '1',
                      'JCODE_NO_TELEMETRY': '1', 'JCODE_DEBUG_CONTROL': '1', 'JCODE_MEMORY_ENABLED': '0',
                      'JCODE_OPENAI_COMPAT_API_BASE': '<fake origin>/v1',
                      'OPENAI_COMPAT_API_KEY': '<dummy>', 'JCODE_OPENROUTER_MODEL': MODEL},
              'flags': ['--no-update', '--no-selfdev', '--provider', 'openai-compatible',
                        '--model', MODEL]}

    def prepare(self, root, origin, workspaces):
        env = base_env(root)
        # Unix socket paths are short (SUN_LEN); macOS temp roots are too long.
        run = Path(tempfile.mkdtemp(prefix='jc-', dir='/tmp'))
        self.cleanup.append(run)
        env.update(JCODE_HOME=str(root / 'jcode-home'), JCODE_RUNTIME_DIR=str(run),
                   JCODE_TEMP_SERVER='1', JCODE_SERVER_OWNER_PID=str(os.getpid()),
                   JCODE_NO_TELEMETRY='1', JCODE_DEBUG_CONTROL='1', JCODE_MEMORY_ENABLED='0',
                   JCODE_OPENAI_COMPAT_API_BASE=f'{origin}/v1', OPENAI_COMPAT_API_KEY=DUMMY_KEY,
                   JCODE_OPENROUTER_MODEL=MODEL)
        (root / 'jcode-home').mkdir()
        return env

    def flags(self):
        return ['--no-update', '--no-selfdev', '--provider', 'openai-compatible', '--model', MODEL]

    def tui_argv(self, root):
        return [str(self.binary), *self.flags()]

    def start(self, root, env, workspaces):
        return JCodeServer(self, root, env, workspaces).sessions


class JCodeServer:
    def __init__(self, agent, root, env, workspaces):
        run = Path(env['JCODE_RUNTIME_DIR'])
        self.debug = str(run / 'bench-debug.sock')
        self.proc = subprocess.Popen([str(agent.binary), *agent.flags(), 'serve', '--socket',
                                      str(run / 'bench.sock')], cwd=root, env=env,
                                     stdin=subprocess.DEVNULL, stdout=(root / 'jcode.stdout').open('w'),
                                     stderr=(root / 'jcode.stderr').open('w'), start_new_session=True)
        deadline = time.monotonic() + 60
        while not os.path.exists(self.debug):
            if self.proc.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('jcode debug socket not ready')
            time.sleep(.1)
        self.sessions = []
        for path in workspaces:
            sid = json.loads(self.command(f'create_session:{path}', timeout=120))['session_id']
            self.sessions.append(JCodeSession(self, sid))
        self.sessions[0].pids = [self.proc.pid]

    def command(self, command, session=None, timeout=TURN_TIMEOUT):
        request = {'type': 'debug_command', 'id': 1, 'command': command}
        if session:
            request['session_id'] = session
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(timeout)
            sock.connect(self.debug)
            sock.sendall((json.dumps(request) + '\n').encode())
            data = b''
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                chunk = sock.recv(1 << 20)
                if not chunk:
                    break
                data += chunk
                while b'\n' in data:
                    line, data = data.split(b'\n', 1)
                    if not line.strip():
                        continue
                    response = json.loads(line)
                    if response.get('type') in (None, 'debug_response') or 'ok' in response:
                        if not response.get('ok', False):
                            raise RuntimeError(f'jcode {command[:30]}: {response.get("error") or response.get("output")}')
                        return response.get('output', '')
        raise TimeoutError(f'jcode {command[:30]}')


class JCodeSession:
    def __init__(self, server, sid):
        self.server, self.sid, self.pids = server, sid, []

    def turn(self, prompt):
        self.server.command(f'message:{prompt}', self.sid)
        return self.server.command('last_response', self.sid, timeout=30)

    def close(self):
        if self.pids:
            kill_group(self.server.proc)


ADAPTERS = {cls.name: cls for cls in (ClaudeCode, Codex, OpenCode, JCode)}


# ---------------------------------------------------------------------------
# Measurements
# ---------------------------------------------------------------------------

def measure_case(agent, sessions, sampler):
    with tempfile.TemporaryDirectory(prefix=f'bench-{agent.name}-') as scratch:
        root = Path(scratch).resolve()
        with FakeModel(sessions) as model:
            workspaces = [make_workspace(root / f'workspace-{i}') for i in range(sessions)]
            env = agent.prepare(root, model.origin, workspaces)
            runtime = []
            try:
                runtime = agent.start(root, env, workspaces)
                roots = [pid for session in runtime for pid in session.pids]
                # Process-per-session agents give no readiness signal before the
                # first turn; let every agent settle for the same 3 s (as jcode's script).
                time.sleep(3)
                result = {'n': sessions, 'baseline': sampler(roots), 'rounds': []}
                with ThreadPoolExecutor(max_workers=sessions) as pool:
                    for index, prompt in enumerate(PROMPTS):
                        barrier = threading.Barrier(sessions)

                        def run(session):
                            barrier.wait(timeout=TURN_TIMEOUT)
                            return session.turn(prompt)

                        replies = list(pool.map(run, runtime))
                        wrong = [reply[:120] for reply in replies if ANSWERS[index] not in (reply or '')]
                        if wrong:
                            raise RuntimeError(f'round {index + 1}: missing fixed answer: {wrong[:3]}')
                        result['rounds'].append({'round': index + 1, **sampler(roots)})
                        log(f'{agent.name} n={sessions} round {index + 1} '
                            f'pss_kib={result["rounds"][-1]["pss_kib"]}')
                files = [(path / 'hello.txt').read_text() if (path / 'hello.txt').exists() else None
                         for path in workspaces]
                result['model'] = model.stats()
                if files != ['hello world'] * sessions:
                    raise RuntimeError(f'hello.txt not created in every workspace: {files[:3]}')
                if not model.workload_complete():
                    raise RuntimeError(f'unexpected fake model work: {result["model"]}')
                return result
            except Exception as error:
                stats = model.stats()
                tails = {}
                for path in sorted(root.glob('*.std*'))[:3]:
                    tails[path.name] = path.read_text(errors='replace')[-1500:]
                raise RuntimeError(f'{type(error).__name__}: {error}; model={json.dumps(stats)[:1500]}; '
                                   f'logs={json.dumps(tails)[:3000]}') from error
            finally:
                for session in runtime:
                    session.close()
                agent.clean()


ESCAPES = re.compile(r'\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[P_^X][^\x1b]*\x1b\\|[()][0-9A-Za-z]|.)')
QUERIES = ((b'\x1b[6n', b'\x1b[1;1R'), (b'\x1b[c', b'\x1b[?62;22c'), (b'\x1b[0c', b'\x1b[?62;22c'),
           (b'\x1b]10;?\x1b\\', b'\x1b]10;rgb:ffff/ffff/ffff\x1b\\'),
           (b'\x1b]11;?\x1b\\', b'\x1b]11;rgb:0000/0000/0000\x1b\\'),
           (b'\x1b]10;?\x07', b'\x1b]10;rgb:ffff/ffff/ffff\x07'),
           (b'\x1b]11;?\x07', b'\x1b]11;rgb:0000/0000/0000\x07'),
           (b'\x1b[?u', b'\x1b[?0u'), (b'\x1b[18t', f'\x1b[8;{ROWS};{COLS}t'.encode()),
           (b'\x1b[14t', b'\x1b[4;720;1000t'), (b'\x1b[16t', b'\x1b[6;20;10t'))
PROBE = 'zqxjvk'


def measure_startup(argv, cwd, env, timeout=60):
    """Spawn->first visible text, and spawn->echo of typed probe text (no submission)."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', ROWS, COLS, 0, 0))
    # No kernel echo: only text the agent itself renders can satisfy the probe.
    attrs = termios.tcgetattr(slave)
    attrs[3] &= ~(termios.ECHO | termios.ICANON)
    termios.tcsetattr(slave, termios.TCSANOW, attrs)
    start = time.perf_counter_ns()
    process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=slave, stdout=slave, stderr=slave,
                               start_new_session=True)
    os.close(slave)
    raw, text, since_probe = b'', '', ''
    first, ready, probes, last_probe = None, None, 0, 0.0
    excerpt = ''
    try:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and ready is None:
            if select.select([master], [], [], .01)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    chunk = b''
                if not chunk:
                    raise RuntimeError('TUI exited before input probe: ' + text[-500:])
                raw = raw[-4096:] + chunk
                for query, answer in QUERIES:
                    while query in raw:
                        os.write(master, answer)
                        raw = raw.replace(query, b'', 1)
                visible = ESCAPES.sub('', chunk.decode('utf-8', 'replace'))
                text = (text + visible)[-20000:]
                elapsed = (time.perf_counter_ns() - start) / 1e6
                if first is None and re.search(r'[A-Za-z0-9]{3}', text):
                    first = elapsed
                    excerpt = ' '.join(text.split())[:120]
                if probes:
                    since_probe = (since_probe + visible)[-20000:]
                    if PROBE in since_probe:
                        ready = elapsed
                        break
            if process.poll() is not None:
                raise RuntimeError('TUI exited before input probe: ' + text[-500:])
            now = time.monotonic()
            # Type the probe at first visible text; retype once per second if never echoed.
            if first is not None and (not probes or now - last_probe >= 1.0) and probes < 20:
                os.write(master, PROBE.encode())
                probes += 1
                last_probe = now
        if ready is None:
            raise TimeoutError('PTY startup/input watchdog: ' + text[-800:])
        return {'first_visible_ms': first, 'input_echo_ms': ready, 'probe_writes': probes,
                'first_visible_excerpt': excerpt}
    finally:
        kill_group(process)
        os.close(master)


def measure_startups(agent, runs):
    samples = []
    for _ in range(runs):
        with tempfile.TemporaryDirectory(prefix=f'startup-{agent.name}-') as scratch:
            root = Path(scratch).resolve()
            workspace = make_workspace(root / 'workspace')
            env = agent.prepare(root, 'http://127.0.0.1:9', [workspace])
            try:
                samples.append(measure_startup(agent.tui_argv(root), workspace, env))
            finally:
                agent.clean()
    return samples


class Shellboy(Agent):
    """Shellboy keeps its stage-1 runner (one `serve` owner, N threads) unchanged."""
    name, wire = 'shellboy', 'responses'
    shape = 'one `shellboy serve` owner hosting N threads (stage-1 runner, footprint.py)'
    config = {'config.toml': 'default connection to fake /v1, response_max_retries=0, '
                             'ui.copy_backend=osc52, ui.bell=false'}

    def prepare(self, root, origin, workspaces):
        env = base_env(root)
        footprint.configure(root, origin + '/v1')
        return env

    def tui_argv(self, root):
        return [str(self.binary), '--config', str(root / 'config.toml'), '--data-dir', str(root / 'data')]


def shellboy_case(agent, sessions, sampler):
    if sampler is no_pss:
        raise RuntimeError('Shellboy smoke runs need Linux /proc (use footprint.py tests instead)')
    case = footprint.measure_case(agent.binary, sessions)
    rounds = [{'round': row['round'], 'pss_kib': row['pss_kib'], 'process_count': len(row['processes']),
               'processes': row['processes']} for row in case['rounds']]
    return {'n': sessions, 'baseline': case['baseline'], 'rounds': rounds,
            'model': {'requests': case['model_requests'], 'tool_calls': case['tool_calls'],
                      'requests_by_wire': {'responses': case['model_requests']}, 'auxiliary_requests': 0}}


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def run_agent(agent, sessions, startup_runs, sampler):
    with tempfile.TemporaryDirectory(prefix='bench-version-') as scratch:
        env = agent.prepare(Path(scratch), 'http://127.0.0.1:9', [])
        resolved = Path(agent.binary).resolve(strict=True)
        version = agent.version(env)
        agent.clean()
        entry = {'agent': agent.name, 'version': version, 'executable': str(resolved),
                 'executable_sha256': sha256(resolved), 'wire': agent.wire, 'shape': agent.shape,
                 'config': agent.config, 'startup': [], 'cases': [], 'status': 'running'}
    try:
        log(f'{agent.name}: {entry["version"]}')
        entry['startup'] = measure_startups(agent, startup_runs)
        for n in sessions:
            measure = shellboy_case if isinstance(agent, Shellboy) else measure_case
            entry['cases'].append(measure(agent, n, sampler))
        entry['status'] = 'complete'
    except Exception as error:  # one agent's failure must not hide the others
        entry.update(status='error', error=f'{type(error).__name__}: {error}'[:6000])
        log(f'{agent.name}: ERROR {entry["error"][:2000]}')
    return entry


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--agent', action='append', default=[], metavar='NAME=PATH',
                        help=f'Agent executable; NAME in shellboy, {", ".join(ADAPTERS)}')
    parser.add_argument('--sessions', type=int, nargs='+', default=[1, 5, 10, 20])
    parser.add_argument('--startup-runs', type=int, default=5)
    parser.add_argument('--provenance', type=Path, help='JSON describing how each agent was installed')
    parser.add_argument('--smoke', action='store_true',
                        help='Protocol check without PSS (any OS); output is not a measurement')
    parser.add_argument('--out', type=Path, required=True, help='New JSON file (never overwritten)')
    args = parser.parse_args()
    if not args.smoke and sys.platform != 'linux':
        parser.error('PSS measurement requires Linux /proc/<pid>/smaps_rollup (or pass --smoke)')
    if any(not 1 <= n <= 20 for n in args.sessions) or len(args.sessions) > 4:
        parser.error('Use at most four session counts, each in 1..20')
    if not 1 <= args.startup_runs <= 20:
        parser.error('--startup-runs must be 1..20')
    if args.out.exists():
        parser.error('Output already exists')
    agents = []
    for spec in args.agent:
        name, _, path = spec.partition('=')
        cls = Shellboy if name == 'shellboy' else ADAPTERS.get(name)
        if cls is None or not path:
            parser.error(f'Unknown agent spec {spec!r}')
        if cls is JCode and sys.platform != 'linux':
            # Observed 2026-11-01: jcode's first macOS TUI start registers a launchd
            # hotkey agent that escapes the temporary HOME. Linux only.
            parser.error('jcode is only driven on Linux by this script')
        agents.append(cls(Path(path).resolve(strict=True)))
    if not agents:
        parser.error('Pass at least one --agent NAME=PATH')
    machine = {'system': platform.system(), 'release': platform.release(), 'machine': platform.machine(),
               'cpu_count': os.cpu_count(), 'platform': platform.platform()}
    if sys.platform == 'linux':
        cpuinfo = Path('/proc/cpuinfo').read_text()
        machine['cpu_model'] = next((line.split(':', 1)[1].strip() for line in cpuinfo.splitlines()
                                     if line.startswith('model name')), platform.processor())
        machine['memory_total'] = next(line for line in Path('/proc/meminfo').read_text().splitlines()
                                       if line.startswith('MemTotal:'))
        machine['distribution'] = platform.freedesktop_os_release()
    result = {'schema_version': 1, 'kind': 'cross-agent fake-model comparison (#116 stage 2)',
              'status': 'running', 'smoke': args.smoke,
              'started_at': datetime.now(timezone.utc).isoformat(), 'machine': machine,
              'ci': {key: os.environ[key] for key in ('GITHUB_SHA', 'GITHUB_RUN_ID', 'GITHUB_REPOSITORY',
                                                      'RUNNER_NAME', 'RUNNER_OS', 'RUNNER_ARCH',
                                                      'ImageOS', 'ImageVersion') if key in os.environ},
              'model': f'loopback fake model ({MODEL}); deterministic; no credentials',
              'workload': {'prompts': list(PROMPTS), 'commands': list(footprint.COMMANDS),
                           'answers': list(ANSWERS)},
              'pss_unit': 'KiB', 'startup': {'cols': COLS, 'rows': ROWS, 'probe': PROBE,
                                             'runs_per_agent': args.startup_runs},
              'sessions': args.sessions,
              'provenance': json.loads(args.provenance.read_text()) if args.provenance else None,
              'agents': []}
    sampler = no_pss if args.smoke else sample_trees
    for agent in agents:
        result['agents'].append(run_agent(agent, args.sessions, args.startup_runs, sampler))
    result['finished_at'] = datetime.now(timezone.utc).isoformat()
    failed = [entry['agent'] for entry in result['agents'] if entry['status'] != 'complete']
    result['status'] = 'error' if failed else 'complete'
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open('x') as output:
        json.dump(result, output, indent=2)
        output.write('\n')
    print(json.dumps({'status': result['status'], 'failed': failed, 'out': str(args.out)}))
    return 0 if not failed else 1


if __name__ == '__main__':
    raise SystemExit(main())
