#!/usr/bin/env python3
"""Native title rename/reset/replay check on a stopped synthetic fixture; no API calls."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import sqlite3
import subprocess
import struct
import termios
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture-root', type=Path, required=True)
    parser.add_argument('--binary', type=Path, default=Path('target/release/shellboy'))
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    root = args.fixture_root.resolve()
    if not (root / '.shellboy-visual-fixture').is_file() or not (root / 'data/shellboy.db').is_file():
        parser.error('Create a dedicated visual_fixture first')
    endpoint = root / 'data/endpoint.json'
    if endpoint.exists():
        parser.error('Fixture owner must be stopped')
    binary = str(args.binary.resolve())
    env = {key: value for key, value in os.environ.items() if not key.startswith('SHELLBOY_')}
    env['TERM'] = 'xterm-256color'
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 100, 0, 0))
    process = subprocess.Popen([binary, '--data-dir', str(root / 'data'), '--config', str(root / 'config.toml')], stdin=slave, stdout=slave, stderr=slave, env=env)
    os.close(slave)
    stopped = threading.Event()

    def drain():
        while not stopped.is_set():
            if select.select([master], [], [], .1)[0]:
                try:
                    if not os.read(master, 65536):
                        break
                except OSError:
                    break

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    evidence = {}

    def cli(label, *words, success=True):
        result = subprocess.run([binary, '--config', str(root / 'config.toml'), '--data-dir', str(root / 'data'), *words], env=env, capture_output=True, text=True, timeout=25)
        value = json.loads(result.stdout)
        assert value['ok'] == success, value
        assert result.returncode == (0 if success else 1), result.returncode
        evidence[label] = value
        return value

    try:
        deadline = time.monotonic() + 15
        while not endpoint.exists():
            assert process.poll() is None, 'Dedicated owner exited'
            assert time.monotonic() < deadline, 'Owner startup timed out'
            time.sleep(.05)
        # endpoint publication can precede socket readiness by a scheduling turn.
        time.sleep(.3)
        thread = 'visual-composer'
        before = cli('before', 'thread', 'get', '--thread', thread)['result']
        def revisions():
            with sqlite3.connect(root / 'data/shellboy.db') as db:
                return db.execute('SELECT revision,profile_revision,reasoning_epoch FROM thread WHERE id=?', (thread,)).fetchone()
        baseline = revisions()
        renamed = cli('rename', 'thread', 'set', '--thread', thread, '--title', '  タイトル変更の実確認  ')
        assert renamed['result']['title'] == 'タイトル変更の実確認'
        assert cli('get_renamed', 'thread', 'get', '--thread', thread)['result']['title'] == 'タイトル変更の実確認'
        filtered = cli('search', 'thread', 'list', '--query', 'タイトル変更の実確認')['result']['items']
        assert [item['thread_id'] for item in filtered] == [thread]
        after = revisions()
        assert after == (baseline[0] + 1, baseline[1], baseline[2]), (baseline, after)
        replay = cli('replay', '--operation-id', renamed['operation_id'], 'thread', 'set', '--thread', thread, '--title', '  タイトル変更の実確認  ')
        assert replay == renamed and revisions() == after
        reset = cli('reset', 'thread', 'set', '--thread', thread, '--title', '')['result']
        assert reset['title'].strip() and reset['title'] != renamed['result']['title']
        for key in ('connection', 'model', 'effort', 'cwd'):
            assert reset.get(key) == before.get(key), key
        cli('invalid', 'thread', 'set', '--thread', thread, '--title', 'bad\ntitle', success=False)
    finally:
        if process.poll() is None:
            os.write(master, b'\x11')
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=5)
                raise
        stopped.set()
        reader.join(timeout=1)
        os.close(master)
    assert process.returncode == 0, process.returncode
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({'api_requests': 0, 'normal_shutdown': True, 'operations': evidence}, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'api_requests': 0, 'cli_checks': len(evidence), 'normal_shutdown': True}))


if __name__ == '__main__':
    main()
