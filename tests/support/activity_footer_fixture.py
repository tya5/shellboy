"""Trusted #85 temporary-owner fixture; shared PTY/VT helper, loopback only."""
import argparse
from contextlib import closing
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/dev'))
from pty_capture import Terminal, WATCHDOG_SECONDS


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--cols', type=int, default=80)
    parser.add_argument('--palette', choices=['dark', 'light'], default='dark')
    parser.add_argument('--term', default='xterm-256color')
    parser.add_argument('--no-color', action='store_true')
    parser.add_argument('--out', type=Path)
    args = parser.parse_args()
    if args.out and args.out.exists():
        raise ValueError('out already exists')
    headers, finish = threading.Event(), threading.Event()
    requests = []
    failures = []

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *_):
            pass

        def do_POST(self):
            try:
                assert self.path == '/v1/responses'
                requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                number = len(requests)
                if number in (2, 4):
                    error = {'error': {'code': 'fixture_error', 'message': 'fixture failure'}}
                    status = 400
                    if number == 4:
                        status = 429
                        error = {'error': {'type': 'usage_limit_reached', 'resets_in_seconds': 7200,
                                           'limit_window_minutes': 10080, 'message': 'fixture quota'}}
                    body = json.dumps(error).encode()
                    self.send_response(status)
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    self.wfile.flush()
                    return
                assert number in (1, 3)
                if number == 1:
                    assert headers.wait(WATCHDOG_SECONDS), 'header gate watchdog'
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Transfer-Encoding', 'chunked')
                self.end_headers()

                def chunk(body):
                    data = body.encode()
                    self.wfile.write(f'{len(data):x}\r\n'.encode() + data + b'\r\n')
                    self.wfile.flush()

                if number == 1:
                    chunk(': fixture transport liveness\n\n')
                    assert finish.wait(WATCHDOG_SECONDS), 'completion gate watchdog'
                text = 'fixture answer' if number == 1 else 'fixture recovered'
                response = {'type': 'response.completed', 'response': {
                    'id': f'fixture-{number}', 'status': 'completed',
                    'output': [{'type': 'message', 'role': 'assistant', 'status': 'completed',
                                'content': [{'type': 'output_text', 'text': text}]}]}}
                chunk('data: ' + json.dumps(response) + '\n\n')
                self.wfile.write(b'0\r\n\r\n')
                self.wfile.flush()
            except BaseException as error:
                failures.append(repr(error))
                raise

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    terminal = None
    screens = []
    try:
        terminal = Terminal(args.binary.resolve(strict=True), cols=args.cols, rows=36,
                            palette=args.palette, term=args.term, no_color=args.no_color,
                            connection_url=f'http://127.0.0.1:{server.server_port}/v1')
        terminal.expect(lambda text: 'F1 help' in text, 'ready')
        root = terminal.root
        env = {'PATH': os.defpath, 'HOME': str(root / 'isolated-home')}

        def cli(*words):
            run = subprocess.run([str(args.binary.resolve()), '--config', str(root / 'config.toml'),
                                  '--data-dir', str(root / 'data'), *words], env=env,
                                 stdin=subprocess.DEVNULL, capture_output=True, check=True,
                                 timeout=WATCHDOG_SECONDS)
            value = json.loads(run.stdout)
            assert value['ok'], value
            return value['result']

        def edit(statement, params=()):
            # Only the disposable store made by Terminal above; never user data.
            with closing(sqlite3.connect(root / 'data/shellboy.db')) as db:
                db.execute(statement, params)
                db.commit()

        def capture(label, predicate):
            print('PTY_STAGE ' + label, file=sys.stderr, flush=True)
            before = terminal.frames.frame_count
            terminal.send('\x0c')
            text = terminal.expect(lambda text: terminal.frames.frame_count > before and predicate(text), label)
            screens.append({'label': label, 'text': text})
            return text

        terminal.paste('fixture request')
        terminal.send('\r')
        capture('awaiting', lambda text: 'モデル応答待ち' in text)
        item = cli('thread', 'list')['items'][0]
        tid = item['thread_id']
        details = cli('thread', 'get', '--thread', tid)
        turn = details['turn_id']
        with closing(sqlite3.connect(root / 'data/shellboy.db')) as db:
            attempt = db.execute('SELECT id FROM model_attempt WHERE turn_id=? ORDER BY rowid DESC LIMIT 1', (turn,)).fetchone()[0]
        edit("UPDATE turn SET created_at='1970-01-01T00:00:00Z' WHERE id=?", (turn,))
        headers.set()
        capture('transport receiving', lambda text: '応答受信中' in text)
        # Construct the saved overlap matrix without claiming an OS shell was spawned.
        edit("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES('ui85-marker',?,?,'running',?)",
             (attempt, turn, datetime.now(timezone.utc).isoformat()))
        capture('overlapping saved facts', lambda text: 'ツール処理中' in text and '応答受信中' in text)
        edit("DELETE FROM tool_group WHERE id='ui85-marker'")
        capture('tool finished', lambda text: '応答受信中' in text and 'ツール処理中' not in text)
        finish.set()
        capture('answer', lambda text: 'fixture answer' in text)
        terminal.paste('fixture failure request')
        terminal.send('\r')
        capture('failure', lambda text: '復旧待ち' in text and 'fixture failure' in text)
        run = cli('thread', 'get', '--thread', tid)['run_id']
        cli('run', 'retry', '--run', run)
        capture('external retry', lambda text: 'fixture recovered' in text and '復旧待ち' not in text
                and 'fixture failure' not in '\n'.join(text.splitlines()[-2:]))
        terminal.paste('fixture quota request')
        terminal.send('\r')
        text = capture('quota', lambda text: '使用量上限' in text and 'UTC' in text and '/model' in text)
        assert '明示的' in text
        assert len(requests) == 4 and not failures, (len(requests), failures)
        terminal.close()
        terminal = None
        result = {'ok': True, 'cols': args.cols, 'palette': args.palette, 'term': args.term,
                  'no_color': args.no_color, 'requests': len(requests), 'screens': screens,
                  'overlap': 'synthetic saved group marker; no OS-shell execution claim'}
        if args.out:
            args.out.write_text(json.dumps(result, ensure_ascii=False), encoding='utf-8')
        print(json.dumps({k: v for k, v in result.items() if k != 'screens'}, ensure_ascii=False))
    finally:
        headers.set()
        finish.set()
        if terminal:
            terminal.close(force=True)
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == '__main__':
    main()
