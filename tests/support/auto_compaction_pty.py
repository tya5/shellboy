"""Exercise auto-compaction in a disposable TUI and gated loopback provider."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import threading

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
    entered, release = threading.Event(), threading.Event()
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
                    assert release.wait(WATCHDOG_SECONDS), 'summary gate timed out'
                    if args.cancel:
                        return
                    if args.summary_error:
                        payload = json.dumps({'error': {'message': 'synthetic summary rejection'}}).encode()
                        self.send_response(400)
                        self.send_header('Content-Type', 'application/json')
                        self.send_header('Content-Length', str(len(payload)))
                        self.end_headers()
                        self.wfile.write(payload)
                        self.wfile.flush()
                        return
                    text, tokens = 'Synthetic prior conversation summary.', 120
                else:
                    index = requests.count('ordinary')
                    text, tokens = 'a' * 1024 + f' fixture answer {index}', (50 if index == 1 else 203000 if index == 2 else 10)
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
        assert requests[:3] == ['ordinary', 'ordinary', 'summary'], requests
        assert 'Waiting for conversation summary' not in screen and '会話要約の完了待ち' not in screen, screen
        if args.cancel:
            terminal.send('\x03')
            stopped = 'Stopped' if args.language == 'en' else '中断済み'
            ended = terminal.expect(lambda text: stopped in text and label not in text, 'stopped during summary')
            release.set()
        else:
            release.set()
            ended = terminal.expect(lambda text: 'fixture answer 3' in text and completed in text and label not in text, 'after summary')
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
                assert events[:3] == ['automatic_compaction_below_threshold', 'automatic_compaction_started', 'automatic_compaction_finished'], events
                if args.summary_error:
                    assert events[3:] == ['automatic_compaction_rejected_other', 'automatic_compaction_skipped_no_new_target'], events
                elif args.cancel:
                    # Cancellation can be observed by the model request or
                    # by a pre-adoption fence; both leave the source path intact.
                    assert events[3:] in (["automatic_compaction_rejected_other"],
                                          ["automatic_compaction_rejected_invalid_state"]), events
                else:
                    assert events[3:] == [], events
                assert recorded[1]['ids'] == recorded[2]['ids'], recorded
                assert all(recorded[1]['ids'].get(key) for key in ('thread_id', 'run_id', 'turn_id')), recorded
                assert all('synthetic context' not in json.dumps(entry) for entry in recorded), recorded
            args.out.write_text(json.dumps({'requests': requests, 'gated_screen': screen, 'ended_screen': ended, 'events': events, 'failures': failures}, ensure_ascii=False, indent=2))
        finally:
            scratch.cleanup()
        print(json.dumps({'ok': True, 'out': str(args.out), 'requests': requests}))
    finally:
        release.set()
        if terminal:
            terminal.close(force=True)
        server.shutdown()
        server.server_close()

if __name__ == '__main__':
    main()
