#!/usr/bin/env python3
"""Measure two disposable fake-terminal starts; no provider calls are made."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts/bench'))
from footprint import Frames, configure, input_ready, isolated_env  # noqa: E402


def sample(binary, root, config, env, respond):
    master, slave = pty.openpty()
    process = None
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 100, 0, 0))
        frames = Frames(100, 36)
        start = time.perf_counter()
        process = subprocess.Popen([str(binary), '--config', str(config), '--data-dir', str(root / 'data')],
                                   cwd=root, env=env, stdin=slave, stdout=slave, stderr=slave,
                                   start_new_session=True)
        os.close(slave)
        slave = None
        pending = b''
        queries = 0
        first = None
        while time.perf_counter() - start < 60:
            if process.poll() is not None:
                raise RuntimeError('process exited before ready')
            if not select.select([master], [], [], 0.02)[0]:
                continue
            data = os.read(master, 65536)
            pending += data
            while b'\x1b[6n' in pending:
                _, pending = pending.split(b'\x1b[6n', 1)
                queries += 1
                if respond:
                    os.write(master, f'\x1b[11;{1 + queries * respond}R'.encode())
            pending = pending[-4:]
            for text in frames.feed(data):
                now = (time.perf_counter() - start) * 1000
                if first is None and text.strip():
                    first = now
                if input_ready(text):
                    return {'first_screen_ms': first, 'ready_ms': now, 'dsr_queries': queries}
        raise TimeoutError('startup watchdog')
    finally:
        if slave is not None:
            os.close(slave)
        if process is not None and process.poll() is None:
            os.write(master, b'\x11')
            deadline = time.monotonic() + 15
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


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='shellboy-width-bench-') as scratch:
        root = Path(scratch)
        env = isolated_env(root)
        env['TERM_PROGRAM'] = 'shellboy-fake-terminal'
        env['TERM_PROGRAM_VERSION'] = '1'
        config = configure(root, 'http://127.0.0.1:9/v1')
        first = sample(binary, root, config, env, True)
        assert first['dsr_queries'] == 2, first
        caches = list((root / 'data').glob('.terminal-width-*'))
        assert len(caches) == 1 and caches[0].read_bytes() == b'1\n', caches
        second = sample(binary, root, config, env, False)
        assert second['dsr_queries'] == 0, second
        changed = dict(env, TERM_PROGRAM_VERSION='2')
        new_version = sample(binary, root, config, changed, 1)
        assert new_version['dsr_queries'] == 2, new_version
        print(json.dumps({'first': first, 'cached': second,
                          'new_version': new_version,
                          'method': 'fake terminal completed frame; legacy probe pending replacement'}))


if __name__ == '__main__':
    main()
