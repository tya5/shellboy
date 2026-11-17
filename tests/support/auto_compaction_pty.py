"""Exercise auto-compaction in a disposable TUI and gated loopback provider."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/dev'))
from pty_capture import Terminal, WATCHDOG_SECONDS


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--baseline', action='store_true')
    parser.add_argument('--summary-error', action='store_true')
    parser.add_argument('--cancel', action='store_true')
    parser.add_argument('--language', choices=('en', 'ja'), default='en')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if args.out.exists():
        raise ValueError('output already exists')
    entered, summary_release, foreground_release, summary_served = (threading.Event() for _ in range(4))
    requests, failures = [], []

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'
        def log_message(self, *_):
            pass
        def do_POST(self):
            try:
                assert self.path == '/v1/responses'
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                summary = request.get('tools') == []
                requests.append('summary' if summary else 'ordinary')
                if summary:
                    entered.set()
                    assert summary_release.wait(WATCHDOG_SECONDS), 'summary gate timed out'
                    if args.cancel:
                        summary_served.set()
                        return
                    if args.summary_error:
                        payload = json.dumps({'error': {'code': 'authentication_error', 'message': 'synthetic summary authorization rejection'}}).encode()
                        # 401 is vetoed from both prefix halving and transient retry.
                        self.send_response(401)
                        self.send_header('Content-Type', 'application/json')
                        self.send_header('Content-Length', str(len(payload)))
                        self.end_headers()
                        self.wfile.write(payload)
                        self.wfile.flush()
                        summary_served.set()
                        return
                    text, tokens = 'Synthetic prior conversation summary.', 120
                else:
                    index = requests.count('ordinary')
                    # #303 #A: the first known report (50000) exceeds the default
                    # target (200000 x 20%), so the third task opens the gated Summary.
                    text, tokens = 'a' * 1024 + f' fixture answer {index}', (50000 if index == 1 else 203000 if index == 2 else 10)
                    if index == 3:
                        # Keep the real current Turn live while the Summary HTTP
                        # response is gated; release this final answer with the test.
                        assert foreground_release.wait(WATCHDOG_SECONDS), 'foreground request gate timed out'
                response = {'id': f'fixture-{len(requests)}', 'status': 'completed',
                            'output': [{'type': 'message', 'role': 'assistant', 'status': 'completed',
                                        'content': [{'type': 'output_text', 'text': text}]}],
                            'usage': {'input_tokens': tokens, 'output_tokens': 1, 'total_tokens': tokens + 1}}
                if request.get('stream'):
                    payload = ('data: ' + json.dumps({'type': 'response.completed', 'response': response}) + '\n\n').encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                else:
                    payload = json.dumps(response).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                self.wfile.flush()
                if summary:
                    summary_served.set()
            except BaseException as error:
                failures.append(repr(error))
                raise

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    terminal = None
    try:
        terminal = Terminal(args.binary.resolve(strict=True), cols=80, rows=28,
                            connection_url=f'http://127.0.0.1:{server.server_port}/v1', language=args.language)
        ready, completed = ('Ready for input', 'Completed') if args.language == 'en' else ('入力できます', '完了')
        label = 'Auto-compacting' if args.language == 'en' else '自動圧縮中'
        terminal.expect(lambda text: ready in text, 'ready')
        for number in (1, 2):
            terminal.paste('synthetic context ' + str(number) + ' ' + 'context ' * 120)
            terminal.send('\r')
            terminal.expect(lambda text: f'fixture answer {number}' in text and completed in text, 'completed turn')
        terminal.paste('synthetic third task')
        terminal.send('\r')
        assert entered.wait(WATCHDOG_SECONDS), ('no summary request', requests, failures)
        if args.baseline:
            screen = terminal.expect(lambda text: 'synthetic third task' in text and ('Preparing' in text or 'Running' in text or '準備中' in text or '処理中' in text), 'gated baseline summary')
            assert label not in screen, screen
        else:
            screen = terminal.expect(lambda text: label in text and '203000' in text and '200000' in text, 'gated summary status')
        assert requests[:2] == ['ordinary', 'ordinary'], requests
        assert requests.count('summary') == 1, requests
        assert 2 <= requests.count('ordinary') <= 3, requests
        assert 'Waiting for conversation summary' not in screen and '会話要約の完了待ち' not in screen, screen
        if args.cancel:
            terminal.send('\x03')
            stopped = 'Stopped' if args.language == 'en' else '中断済み'
            ended = terminal.expect(lambda text: stopped in text and label not in text, 'stopped during summary')
            foreground_release.set()
            summary_release.set()
        else:
            # Complete the already-admitted Ordinary response before returning
            # the held Summary so FINISH can reacquire its normal boundary.
            foreground_release.set()
            ended = terminal.expect(lambda text: 'fixture answer 3' in text and completed in text and label not in text, 'foreground complete before Summary release')
            summary_release.set()
            assert summary_served.wait(WATCHDOG_SECONDS), ('summary response was not serviced', requests, failures)
            if args.summary_error:
                # Count HTTP requests only after Core records this original operation's terminal event.
                log_path = terminal.root / 'data/logs/shellboy.log'
                deadline = time.monotonic() + WATCHDOG_SECONDS
                while True:
                    entries = [json.loads(line) for line in log_path.read_text().splitlines()] if log_path.exists() else []
                    started = [entry for entry in entries if entry['event'] == 'automatic_compaction_started']
                    finished = [entry for entry in entries if entry['event'] == 'automatic_compaction_finished']
                    if finished:
                        assert len(started) == len(finished) == 1, (started, finished)
                        assert started[0]['ids'] == finished[0]['ids'], (started, finished)
                        break
                    assert time.monotonic() < deadline, ('Core did not finish the one-attempt 401 operation', requests, failures)
                    time.sleep(0.02)
        assert requests[:2] == ['ordinary', 'ordinary'], requests
        assert requests.count('summary') == 1, requests
        if args.cancel:
            assert 2 <= requests.count('ordinary') <= 3, requests
        else:
            assert requests.count('ordinary') == 3, requests
        root = terminal.root
        # Keep the isolated directory until diagnostics have been flushed by shutdown.
        scratch = terminal.scratch
        terminal.scratch = None
        terminal.close()
        terminal = None
        try:
            entries = [json.loads(line) for line in (root / 'data/logs/shellboy.log').read_text().splitlines()]
            recorded = [entry for entry in entries if entry['event'].startswith('automatic_compaction_')]
            events = [entry['event'] for entry in recorded]
            if not args.baseline:
                # #303: the "below threshold" diagnostic and the guard-reason
                # rejected/skipped diagnostics are retired with the old guard.
                assert events[:2] == ['automatic_compaction_started', 'automatic_compaction_finished'], events
                assert events[2:] == [], events
                assert recorded[0]['ids'] == recorded[1]['ids'], recorded
                assert all(recorded[0]['ids'].get(key) for key in ('thread_id', 'run_id', 'turn_id')), recorded
                assert all('synthetic context' not in json.dumps(entry) for entry in recorded), recorded
            args.out.write_text(json.dumps({'requests': requests, 'gated_screen': screen, 'ended_screen': ended, 'events': events, 'failures': failures}, ensure_ascii=False, indent=2))
        finally:
            scratch.cleanup()
        print(json.dumps({'ok': True, 'out': str(args.out), 'requests': requests}))
    finally:
        summary_release.set()
        foreground_release.set()
        if terminal:
            terminal.close(force=True)
        server.shutdown()
        server.server_close()

if __name__ == '__main__':
    main()
