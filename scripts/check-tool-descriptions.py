#!/usr/bin/env python3
"""Actual disposable Unix PTY checks for model-authored tool labels (loopback only)."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import tempfile
import threading
sys.path.insert(0, str(Path(__file__).parent / 'dev'))
from pty_capture import Terminal


class Mock(BaseHTTPRequestHandler):
    bodies = []

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        Mock.bodies.append(body)
        second = any(v.get('type') == 'function_call_output' for v in body['input'])
        output = [{'type': 'message', 'role': 'assistant', 'content': [
            {'type': 'output_text', 'text': 'DESCRIPTION_DONE' if second else 'DESCRIPTION_HELLO'}]}]
        if not second:
            for key, command, description in [
                    ('a', "printf CMD_DESCRIBED", 'CHECK_LABEL'),
                    ('b', "printf CMD_FALLBACK", '')]:
                output.append({'type': 'function_call', 'name': 'shell', 'call_id': key,
                               'arguments': json.dumps({'command': command, 'description': description})})
        raw = {'id': 'fixture', 'status': 'completed', 'output': output}
        data = ('data: ' + json.dumps({'type': 'response.completed', 'response': raw}) + '\n\n').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_args):
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Mock)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    results = []
    try:
        for cols in [80, 140]:
            for palette, plain in [('dark', False), ('light', False), ('dark', True)]:
                for enabled in [True, False]:
                    name = f'{cols}-{palette}-plain{int(plain)}-describe{int(enabled)}'
                    with tempfile.TemporaryDirectory(prefix='sb-description-pty-') as scratch:
                        root = Path(scratch)
                        (root / 'config.toml').write_text(
                            'default_connection="fixture"\ndefault_model="fixture"\n'
                            '[connections.fixture]\n'
                            f'base_url="http://127.0.0.1:{server.server_port}/v1"\n'
                            f'[ui]\ncopy_backend="osc52"\nbell=false\npalette="{palette}"\n'
                            f'[shell]\ndescribe_calls={str(enabled).lower()}\n')
                        before = len(Mock.bodies)
                        terminal = Terminal(args.binary.resolve(), root, cols, palette, no_color=plain)
                        frames = []
                        def scene(label, predicate):
                            terminal.send('\x0c')
                            text = terminal.expect(predicate, label)
                            (args.out / f'{name}-{label}.txt').write_text(text + '\n')
                            frames.append(label)
                            return text
                        try:
                            scene('ready', lambda s: 'F1 help' in s)
                            terminal.paste('description fixture')
                            terminal.send('\r')
                            text = scene('done', lambda s: 'DESCRIPTION_DONE' in s)
                            if enabled:
                                assert 'CHECK_LABEL' in text and 'printf CMD_DESCRIBED' not in text
                                assert 'printf CMD_FALLBACK' in text
                                def click_described_header():
                                    y = next(i + 1 for i, row in enumerate(terminal.text().splitlines()) if 'CHECK_LABEL' in row)
                                    terminal.send(f'\x1b[<0;5;{y}M\x1b[<0;5;{y}m')
                                # Notifications are separate selectable items: address the actual header,
                                # not an assumed number of Up keys from the end of history.
                                click_described_header()
                                scene('focused-expanded', lambda s: 'コマンド（プレビュー）' in s and 'printf CMD_DESCRIBED' in s)
                                terminal.send(' ')
                                terminal.send('\x1b\x1b')
                                scene('blurred-collapsed', lambda s: 'CHECK_LABEL' in s and 'printf CMD_DESCRIBED' not in s)
                                click_described_header()
                                scene('expanded', lambda s: 'コマンド原文' in s and 'printf CMD_DESCRIBED' in s)
                                terminal.send('\x1b[17~')
                                scene('details', lambda s: '選択した内容の詳細' in s and 'CMD_DESCRIBED' in s and 'O 原文' in s)
                                terminal.send('o')
                                scene('original', lambda s: 'CMD_DESCRIBED' in s and 'N 次page' in s and ('arguments' in s or 'command' in s))
                                terminal.send('\x1b\x1b')
                                terminal.send('\x1b\x1b')
                            else:
                                assert 'printf CMD_DESCRIBED' in text and 'CHECK_LABEL' not in text
                        finally:
                            terminal.close()
                        observed = Mock.bodies[before:]
                        assert len(observed) == 2
                        assert all(('description' in b['tools'][0]['parameters']['properties']) == enabled for b in observed)
                        results.append({'case': name, 'frames': frames, 'http_rows': len(observed), 'ok': True})
    finally:
        server.shutdown()
        server.server_close()
        worker.join()
    (args.out / 'result.json').write_text(json.dumps(results, indent=2))
    print(json.dumps({'cases': len(results), 'ok': True}))


if __name__ == '__main__':
    main()
