#!/usr/bin/env python3
"""Observe synthetic send/draft state through Windows ConPTY, never user data."""
import argparse
from contextlib import closing
import os
from pathlib import Path
import queue
import sqlite3
import sys
import tempfile
import threading
import time

import pyte
from winpty import Backend, PtyProcess


def run_case(binary, shell, mode, index):
    with tempfile.TemporaryDirectory(prefix='shellboy-conpty-162-') as scratch:
        root = Path(scratch)
        home = root / 'home'
        home.mkdir()
        (root / 'config.toml').write_text(
            'default_connection="fixture"\ndefault_model="offline-fixture"\n'
            '[connections.fixture]\nbase_url="http://127.0.0.1:9/v1"\n'
            '[ui]\ncopy_backend="osc52"\nbell=false\nlanguage="en"\n', encoding='utf-8')
        args = [str(binary), '--config', str(root / 'config.toml'), '--data-dir', str(root / 'data')]
        if shell == 'powershell':
            quote = lambda value: "'" + value.replace("'", "''") + "'"
            command = ['pwsh', '-NoLogo', '-NoProfile', '-Command',
                       '& ' + ' '.join(map(quote, args))]
        else:
            command = ['bash', '--noprofile', '--norc', '-c', 'exec "$@"', '_', *args]
        env = {k: v for k, v in os.environ.items() if k.upper() in
               {'PATH', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATHEXT', 'NUMBER_OF_PROCESSORS'}}
        env.update(HOME=str(home), USERPROFILE=str(home), APPDATA=str(home / 'config'),
                   LOCALAPPDATA=str(home / 'local'), TMP=str(root), TEMP=str(root), TERM='xterm-256color',
                   PYWINPTY_BACKEND='0')
        process = PtyProcess.spawn(command, cwd=str(root), env=env, dimensions=(36, 100),
                                   backend=Backend.ConPTY)
        chunks = queue.Queue()
        def reader():
            try:
                while process.isalive():
                    text = process.read(4096)
                    if text:
                        chunks.put(text)
            except (EOFError, OSError):
                pass
        thread = threading.Thread(target=reader, daemon=True)
        thread.start()
        screen = pyte.Screen(100, 36)
        stream = pyte.Stream(screen)
        def until(predicate, label, timeout=45):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                try:
                    stream.feed(chunks.get(timeout=.1))
                except queue.Empty:
                    pass
                frame = '\n'.join(screen.display)
                if predicate(frame):
                    return frame
            raise RuntimeError(f'{shell}/{mode}/{index}: timeout {label}\n{frame}')
        try:
            until(lambda frame: 'Ctrl+T' in frame, 'startup')
            # Start a synthetic offline run first so the next message stays queued.
            process.write('synthetic-warmup\r')
            until(lambda frame: 'Turn started' in frame or 'Retrying' in frame, 'active run')
            marker = f'synthetic-{shell}-{mode}-{index}'
            if mode.startswith('paste'):
                process.write('\x1b[200~' + marker + '\x1b[201~')
            else:
                process.write(marker)
            if not mode.endswith('-burst'):
                until(lambda frame: marker in frame, 'draft typed')
            process.write('\r\x1b[3~' if mode == 'typed-noop-delete' else '\r')
            frame = until(lambda frame: ('Pending' in frame or 'Submitting' in frame)
                          and marker in frame, 'queue')
            time.sleep(1)
            while not chunks.empty():
                stream.feed(chunks.get_nowait())
            frame = '\n'.join(screen.display)
            print(f'=== shell={shell} transport=ConPTY mode={mode} index={index} ===', flush=True)
            print(frame, flush=True)
            print(f'marker_occurrences={frame.count(marker)}', flush=True)
            process.write('\r')
            time.sleep(1)
            while not chunks.empty():
                stream.feed(chunks.get_nowait())
            print('=== after second Enter ===', flush=True)
            final = '\n'.join(screen.display)
            print(final, flush=True)
            db_path = root / 'data' / 'shellboy.db'
            with closing(sqlite3.connect(db_path.as_uri() + '?mode=ro', uri=True)) as db:
                count = db.execute(
                    'SELECT count(*) FROM receipt r JOIN node n ON n.id=r.message_node_id '
                    'WHERE instr(n.payload_json, ?) > 0', (marker,)).fetchone()[0]
                drafts = db.execute(
                    "SELECT count(*) FROM draft WHERE state='editing' AND "
                    "instr(COALESCE(json_extract(body_json, '$.text'), ''), ?) > 0",
                    (marker,)).fetchone()[0]
            print(f'db_receipts={count} editing_drafts_with_marker={drafts}', flush=True)
            assert frame.count(marker) == 1 and final.count(marker) == 1 and count == 1 and drafts == 0, (shell, mode, index, frame.count(marker), final.count(marker), count, drafts)
        finally:
            if process.isalive():
                process.write('\x11')
                deadline = time.monotonic() + 5
                while process.isalive() and time.monotonic() < deadline:
                    time.sleep(.05)
                if process.isalive():
                    process.terminate(force=True)


def main():
    sys.stdout.reconfigure(encoding='utf-8', errors='backslashreplace')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--repeat', type=int, default=3)
    args = parser.parse_args()
    # pywinpty 2.0.15 treats the numeric ConPTY value (0) as false.
    os.environ['PYWINPTY_BACKEND'] = '0'
    binary = args.binary.resolve(strict=True)
    for shell in ('git-bash', 'powershell'):
        for mode in ('typed-noop-delete', 'typed', 'paste', 'typed-burst', 'paste-burst'):
            for index in range(args.repeat):
                run_case(binary, shell, mode, index)


if __name__ == '__main__':
    main()
