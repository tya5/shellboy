#!/usr/bin/env python3
"""Record Linux Shellboy owner-tree PSS and cold disposable-TUI PTY startup.

No credentials, paid APIs, other agents, or performance thresholds are used.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import re
import select
import signal
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from dev.pty_capture import Frames  # noqa: E402

PROMPTS = (
    'Use a tool to list the files in the current directory, then reply with just the number of entries.',
    'Use a tool to create hello.txt containing exactly "hello world".',
    "Search the repository for the string 'fn main' with a tool and reply with how many files matched.",
    'Use a tool to count the lines in README.md.',
    'Reply with exactly the word: done',
)
COMMANDS = (
    'find . -maxdepth 1 -type f | sort',
    "printf 'hello world' > hello.txt",
    "grep -rl -- 'fn main' src",
    'wc -l README.md',
)
ANSWERS = ('2', 'created hello.txt', '1', '2', 'done')
TIMEOUT = 60


def isolated_env(root):
    env = {key: os.environ[key] for key in ('PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'TZ')
           if key in os.environ}
    home = root / 'home'
    (home / 'tmp').mkdir(parents=True)
    env.update(HOME=str(home), USERPROFILE=str(home), TERM='xterm-256color',
               XDG_CONFIG_HOME=str(home / 'config'), XDG_DATA_HOME=str(home / 'data'),
               TMPDIR=str(home / 'tmp'), TMP=str(home / 'tmp'), TEMP=str(home / 'tmp'))
    return env


def configure(root, url):
    config = root / 'config.toml'
    config.write_text('default_connection="fixture"\ndefault_model="offline-bench"\n'
                      '[connections.fixture]\nbase_url=' + json.dumps(url) + '\n'
                      'response_max_retries=0\n[ui]\ncopy_backend="osc52"\nbell=false\n')
    return config


def plan(body):
    """Only the last user instruction determines this request's fixed response."""
    items = body['input']
    for pos in range(len(items) - 1, -1, -1):
        item = items[pos]
        if item.get('role') != 'user':
            continue
        content = item.get('content', '')
        text = content if isinstance(content, str) else ''.join(
            part.get('text', '') for part in content if isinstance(part, dict))
        if text not in PROMPTS:
            raise ValueError('Unexpected synthetic instruction')
        index = PROMPTS.index(text)
        call_id = f'bench-round-{index}'
        followup = any(entry.get('type') == 'function_call_output'
                       and entry.get('call_id') == call_id for entry in items[pos + 1:])
        tool = index < len(COMMANDS) and not followup
        if tool:
            output = [{'type': 'function_call', 'name': 'shell', 'call_id': call_id,
                       'arguments': json.dumps({'command': COMMANDS[index],
                                                'description': f'Benchmark round {index + 1}'})}]
        else:
            output = [{'type': 'message', 'role': 'assistant',
                       'content': [{'type': 'output_text', 'text': ANSWERS[index]}]}]
        return index, tool, output
    raise ValueError('No synthetic user instruction')


class FakeModel(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 64

    def __init__(self, sessions, traffic=None):
        super().__init__(('127.0.0.1', 0), Handler)
        self.traffic = traffic
        self.sessions = sessions
        self.requests = 0
        self.tools = [0] * 5
        self.answers = [0] * 5
        self.errors = []
        self.allowed_threads = set()
        self.traffic_threads = {}
        self.seen = set()
        self.lock = threading.Lock()

    @property
    def url(self):
        return f'http://127.0.0.1:{self.server_port}/v1'


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        self.connection.settimeout(TIMEOUT)
        try:
            size = int(self.headers.get('Content-Length', '0'))
            if not 0 < size <= 4 * 1024 * 1024 or self.path != '/v1/responses':
                raise ValueError('Unexpected request path or size')
            raw = self.rfile.read(size)
            if len(raw) != size:
                raise ValueError('Incomplete synthetic request')
            body = json.loads(raw)
            index, tool, output = plan(body)
            contexts = [part.get('text', '') for item in body['input']
                        if item.get('role') == 'developer' for part in item.get('content', [])]
            identities = [match.group(1) for text in contexts
                          if (match := re.match(r'Shellboy context v1: thread=([a-zA-Z0-9-]+) ', text))]
            if not identities:
                raise ValueError('Missing synthetic Thread identity')
            thread = identities[-1]
            if self.server.traffic is not None:
                conversation = self.server.traffic_threads.get(thread)
                self.server.traffic.record(self.path, raw, conversation)
            with self.server.lock:
                key = (thread, index, tool)
                if thread not in self.server.allowed_threads or key in self.server.seen:
                    raise ValueError('Unknown Thread or repeated per-Thread round/tool request')
                if not tool and index < 4 and (thread, index, True) not in self.server.seen:
                    raise ValueError('Tool followup without issued call')
                self.server.seen.add(key)
                self.server.requests += 1
                counter = self.server.tools if tool else self.server.answers
                if self.server.requests > self.server.sessions * 9 or counter[index] >= self.server.sessions:
                    raise ValueError('Fixed model request/tool budget exceeded')
                counter[index] += 1
            response = {'id': f'bench-{index}', 'status': 'completed', 'output': output}
            payload = ('data: ' + json.dumps({'type': 'response.completed',
                                             'response': response}) + '\n\n').encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except (ValueError, KeyError, TypeError) as error:
            with self.server.lock:
                self.server.errors.append(str(error))
            self.send_error(400, 'Invalid synthetic request')
        except (BrokenPipeError, ConnectionResetError):
            with self.server.lock:
                self.server.errors.append('Disconnected synthetic request')

    def log_message(self, *_args):
        pass


def proc_identity(path):
    # comm may contain spaces or parentheses; fields after the final ')' are fixed.
    fields = (path / 'stat').read_text().rsplit(')', 1)[1].split()
    return int(fields[1]), int(fields[19])  # parent PID, starttime


def process_tree(owner, proc=Path('/proc')):
    identities = {}
    for path in proc.iterdir():
        if path.name.isdigit():
            try:
                identities[int(path.name)] = proc_identity(path)
            except (FileNotFoundError, ProcessLookupError):
                pass
    if owner not in identities:
        raise RuntimeError('Owner disappeared during PSS measurement')
    selected = {owner: identities[owner]}
    while True:
        children = {pid: identity for pid, identity in identities.items()
                    if identity[0] in selected and pid not in selected}
        if not children:
            return selected
        selected.update(children)


def sample_pss(owner, proc=Path('/proc')):
    for _ in range(3):
        before = process_tree(owner, proc)
        rows = []
        try:
            for pid, identity in sorted(before.items()):
                lines = (proc / str(pid) / 'smaps_rollup').read_text().splitlines()
                pss = [line.split() for line in lines if line.startswith('Pss:')]
                if len(pss) != 1 or pss[0][2] != 'kB':
                    raise RuntimeError('Missing or invalid smaps_rollup Pss')
                rows.append({'pid': pid, 'starttime_ticks': identity[1], 'pss_kib': int(pss[0][1])})
            if before == process_tree(owner, proc):
                return {'pss_kib': sum(row['pss_kib'] for row in rows), 'processes': rows,
                        'coverage': 'stable_owner_and_descendants'}
        except (FileNotFoundError, ProcessLookupError):
            continue
    raise RuntimeError('Process tree changed during all three PSS snapshots')


def cli(binary, root, env, *args):
    command = [str(binary), '--data-dir', str(root / 'data'), *args]
    for attempt in range(30):
        result = subprocess.run(command, cwd=root, env=env, text=True,
                                capture_output=True, timeout=TIMEOUT + 10)
        value = json.loads(result.stdout)
        if result.returncode == 0 and value.get('ok') is True:
            return value['result']
        error = value.get('error', {})
        details = error.get('details', {})
        # Retry only a proven admission rejection, retaining its exact operation ID.
        if (error.get('code') == 'invalid_state' and details.get('phase') == 'admission'
                and details.get('request_may_have_been_applied') is False):
            command = [str(binary), '--data-dir', str(root / 'data'),
                       '--operation-id', value['operation_id'], *args]
            time.sleep(.02 * (attempt + 1))
            continue
        raise RuntimeError(f'CLI {args[:2]} failed: {result.stderr[:1000]} {result.stdout[:1000]}')
    raise RuntimeError('Bounded IPC admission retries exhausted')


def read_json_line(process):
    if not select.select([process.stdout], [], [], TIMEOUT)[0]:
        raise TimeoutError('Owner readiness/stop watchdog')
    line = process.stdout.readline()
    if not line:
        raise RuntimeError('Owner exited without lifecycle JSON')
    return json.loads(line)


def finish_round(binary, root, env, thread, sent, prompt, expected):
    waited = cli(binary, root, env, 'wait', '--conditions-json', json.dumps([
        {'type': 'run_ended', 'run_id': sent['run_id']}]), '--timeout-ms', '60000')
    if waited['status'] != 'satisfied':
        raise RuntimeError('Run did not finish')
    turn = cli(binary, root, env, 'turn', 'get', '--turn', sent['turn_id'])
    if turn['state'] != 'completed':
        raise RuntimeError(f'Non-completed turn: {turn["state"]}')
    message = cli(binary, root, env, 'message', 'get', '--message', sent['message_id'])
    node = cli(binary, root, env, 'history', 'get', '--node', turn['end_message_node_id'])
    if node.get('text') != expected:
        raise RuntimeError('Missing exact fixed synthetic answer')
    shells = cli(binary, root, env, 'shell', 'list', '--thread', thread, '--limit', '20')
    if shells.get('next_cursor') or shells.get('scan_budget_reached'):
        raise RuntimeError('Incomplete synthetic shell listing')
    current = [row for row in shells['items'] if row['run_id'] == sent['run_id']]
    if len(current) != (0 if prompt == PROMPTS[-1] else 1):
        raise RuntimeError('Unexpected per-round shell count')
    for row in current:
        shell = cli(binary, root, env, 'shell', 'get', '--execution', row['execution_id'])
        if (shell.get('state') != 'exited' or shell.get('exit_code') != 0
                or shell.get('forced') is not False or shell.get('output_complete') is not True
                or shell.get('group_cleanup_confirmed') is not True):
            raise RuntimeError('Synthetic shell did not exit successfully with complete cleanup')
    return {'run_id': sent['run_id'], 'turn_id': sent['turn_id'], 'state': turn['state']}


def measure_case(binary, sessions, observe_io=False):
    if observe_io:
        from observations import RequestLog, storage
    with tempfile.TemporaryDirectory(prefix='shellboy-bench-') as scratch:
        root = Path(scratch)
        env = isolated_env(root)
        traffic = RequestLog(sessions) if observe_io else None
        model = FakeModel(sessions, traffic=traffic)
        worker = threading.Thread(target=model.serve_forever, daemon=True)
        worker.start()
        owner = None
        try:
            config = configure(root, model.url)
            owner = subprocess.Popen([str(binary), 'serve', '--config', str(config),
                                      '--data-dir', str(root / 'data')], cwd=root, env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                     start_new_session=True)
            if read_json_line(owner).get('event') != 'ready':
                raise RuntimeError('Owner not ready')
            threads = []
            for index in range(sessions):
                cwd = root / f'workspace-{index}'
                (cwd / 'src').mkdir(parents=True)
                (cwd / 'README.md').write_text('# Synthetic repository\nBenchmark fixture.\n')
                (cwd / 'Cargo.toml').write_text('[package]\nname="synthetic"\n')
                (cwd / 'src/main.rs').write_text('fn main() {}\n')
                thread = cli(binary, root, env, 'thread', 'create', '--cwd', str(cwd))
                threads.append(thread['thread_id'])
                model.allowed_threads.add(thread['thread_id'])
                model.traffic_threads[thread['thread_id']] = index
            result = {'n': sessions, 'baseline': sample_pss(owner.pid), 'rounds': []}
            with ThreadPoolExecutor(max_workers=sessions) as pool:
                for index, prompt in enumerate(PROMPTS):
                    barrier = threading.Barrier(sessions)

                    def send(thread):
                        barrier.wait(timeout=TIMEOUT)
                        return cli(binary, root, env, 'message', 'send', '--thread', thread, '--text', prompt)

                    sent = list(pool.map(send, threads))
                    # Concurrent sends, sequential observers: 20 long waits would consume
                    # the owner's bounded IPC admission slots, not add model concurrency.
                    completed = [finish_round(binary, root, env, thread, message, prompt, ANSWERS[index])
                                 for thread, message in zip(threads, sent)]
                    result['rounds'].append({'round': index + 1, 'prompt': prompt,
                                             **sample_pss(owner.pid), 'completed': completed})
            if model.errors or model.requests != sessions * 9 or model.tools != [sessions] * 4 + [0]:
                raise RuntimeError(f'Unexpected fake model work: {model.errors}, {model.tools}')
            result['model_requests'] = model.requests
            result['tool_calls'] = sum(model.tools)
            if observe_io:
                result['traffic'] = traffic.result()
                result['storage'] = storage([Path(env['HOME']), root / 'data'])
            cli(binary, root, env, 'shutdown')
            stopped = read_json_line(owner)
            owner.wait(timeout=TIMEOUT)
            if owner.returncode or stopped.get('event') != 'stopped' or stopped.get('ok') is not True:
                raise RuntimeError('Owner cleanup failed')
            return result
        finally:
            if owner is not None:
                if owner.poll() is None:
                    os.killpg(owner.pid, signal.SIGKILL)
                    owner.wait(timeout=10)
                owner.stdout.close()
            model.shutdown()
            model.server_close()
            worker.join(timeout=5)


def input_ready(text):
    return '\u5165\u529b\u3067\u304d\u307e\u3059' in text or 'Ready for input' in text


def measure_startup(binary):
    import fcntl
    import pty
    import struct
    import termios
    with tempfile.TemporaryDirectory(prefix='shellboy-startup-') as scratch:
        root = Path(scratch)
        env = isolated_env(root)
        config = configure(root, 'http://127.0.0.1:9/v1')
        master, slave = pty.openpty()
        process = None
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 100, 0, 0))
            frames = Frames(100, 36)
            start = time.perf_counter_ns()
            process = subprocess.Popen([str(binary), '--config', str(config), '--data-dir', str(root / 'data')],
                                       cwd=root, env=env, stdin=slave, stdout=slave, stderr=slave,
                                       start_new_session=True)
            os.close(slave)
            slave = None
            first_screen = None
            probe_sent = False
            probe = 'BENCH_INPUT_READY'
            deadline = time.monotonic() + TIMEOUT
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError('TUI exited before input probe')
                if not select.select([master], [], [], .01)[0]:
                    continue
                for text in frames.feed(os.read(master, 65536)):
                    elapsed = (time.perf_counter_ns() - start) / 1e6
                    if first_screen is None and text.strip():
                        first_screen = elapsed
                    if not probe_sent and input_ready(text):
                        os.write(master, ('\x1b[200~' + probe + '\x1b[201~').encode())
                        probe_sent = True
                    if probe in text:
                        return {'first_screen_ms': first_screen, 'input_echo_ms': elapsed,
                                'method': 'spawn_to_first_nonempty_completed_frame_and_pasted_composer_echo',
                                'cols': 100, 'rows': 36, 'cold_data_dir': True}
            raise TimeoutError('PTY startup/input watchdog: ' + frames.completed_text)
        finally:
            if slave is not None:
                os.close(slave)
            if process is not None and process.poll() is None:
                os.write(master, b'\x11')
                deadline = time.monotonic() + 10
                while process.poll() is None and time.monotonic() < deadline:
                    if select.select([master], [], [], .05)[0]:
                        try:
                            os.read(master, 65536)
                        except OSError:
                            break
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=10)
            os.close(master)
            if process is not None and process.returncode != 0:
                raise RuntimeError('TUI did not shut down cleanly')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True, help='Release binary to measure (never rebuilt by this script)')
    parser.add_argument('--sessions', type=int, nargs='+', default=[1, 5, 10, 20], help='Independent fresh owners; 1..20 each')
    parser.add_argument('--out', type=Path, required=True, help='New JSON file; existing files are never overwritten')
    args = parser.parse_args()
    if sys.platform != 'linux':
        parser.error('PSS measurement requires Linux /proc/<pid>/smaps_rollup')
    if any(not 1 <= n <= 20 for n in args.sessions) or len(args.sessions) > 4:
        parser.error('Use at most four session counts, each in 1..20')
    binary = args.binary.resolve(strict=True)
    if args.out.exists():
        parser.error('Output already exists')
    cpuinfo = Path('/proc/cpuinfo').read_text()
    cpu_model = next((line.split(':', 1)[1].strip() for line in cpuinfo.splitlines()
                      if line.startswith('model name')), platform.processor())
    memory_total = next(line for line in Path('/proc/meminfo').read_text().splitlines()
                        if line.startswith('MemTotal:'))
    result = {'schema_version': 1, 'status': 'running', 'started_at': datetime.now(timezone.utc).isoformat(),
              'machine': {'system': platform.system(), 'release': platform.release(),
                          'machine': platform.machine(), 'cpu_count': os.cpu_count(),
                          'cpu_model': cpu_model, 'memory_total': memory_total,
                          'distribution': platform.freedesktop_os_release(),
                          'platform': platform.platform()},
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'ci': {key: os.environ[key] for key in ('GITHUB_SHA', 'GITHUB_RUN_ID', 'GITHUB_REPOSITORY')
                     if key in os.environ},
              'model': 'literal-loopback deterministic Responses SSE; no credentials',
              'pss_unit': 'KiB', 'sampling': 'after all N completed runs, not peak or simultaneous OS snapshot',
              'max_tool_calls_per_conversation': 4, 'cases': []}
    try:
        result['startup'] = measure_startup(binary)
        for sessions in args.sessions:
            result['cases'].append(measure_case(binary, sessions))
        result['status'] = 'complete'
    except Exception as error:
        result.update(status='error', error=f'{type(error).__name__}: {error}')
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open('x') as output:
        json.dump(result, output, indent=2)
        output.write('\n')
    print(json.dumps({'status': result['status'], 'out': str(args.out)}))
    return 0 if result['status'] == 'complete' else 1


if __name__ == '__main__':
    raise SystemExit(main())
