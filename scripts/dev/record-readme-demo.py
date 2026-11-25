#!/usr/bin/env python3
"""Record the README demo from live Shellboy PTY frames and a scripted loopback model."""
import argparse
from contextlib import suppress
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
from threading import Event, Thread

from pty_capture import Terminal, WATCHDOG_SECONDS

ROOT = Path(__file__).resolve().parents[2]
QUESTION = 'Run the tests and investigate the failure.'
CHILD_ASK = 'Why is the total wrong?'
REASON = 'The total adds the shipping fee twice.'
TEST = 'python3 -m unittest -v'
CREATE = ('''./shellboy thread create --parent-thread "$SHELLBOY_THREAD_ID"'''
          ''' | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["thread_id"])' > c''')
ASK = f'''./shellboy message ask --thread "$(cat c)" --text '{CHILD_ASK}' > a'''
DUTY = './shellboy thread get --thread "$(cat c)" | python3 show-duty.py'
ANSWER = ('''cat checkout.py && ./shellboy message reply --message "$(cat ask-id)"'''
          f" --text '{REASON}' > reply")
COLS, ROWS = 88, 24
MAX_BYTES = 1_000_000

# Decode real CLI results for the recorded duty; no fake terminal output.
SHOW_DUTY = '''import json
from pathlib import Path
import sys

ask = json.loads(Path('a').read_text())
assert ask['ok'], ask
Path('ask-id').write_text(ask['result']['message_id'])
result = json.load(sys.stdin)
assert result['ok'], result
count = result['result']['pending_replies']['owed_by']['count']
assert count == 1, count
print(f'Child Thread: {count} reply owed')
'''


def model(release_test, release_child, child_answered):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            if self.path != '/v1/models':
                self.send_error(404)
                return
            self.respond({'object': 'list', 'data': [{'id': 'offline-fixture', 'object': 'model'}]})

        def do_POST(self):
            if self.path != '/v1/responses':
                self.send_error(404)
                return
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            items = body['input']
            outputs = [item for item in items if item.get('type') == 'function_call_output']
            if outputs:
                last = outputs[-1]['call_id']
                if last == 'test' and release_test.wait(WATCHDOG_SECONDS):
                    self.tool('create', CREATE)
                elif last == 'create':
                    self.tool('ask', ASK)
                elif last == 'ask':
                    self.tool('duty', DUTY)
                elif last == 'duty' and child_answered.wait(WATCHDOG_SECONDS):
                    self.say('The tests fail: expected 15, got 20.\n\n' + REASON)
                elif last == 'answer':
                    self.say('I checked checkout.py and sent the cause back to the parent.')
                else:
                    self.send_error(500)
            elif any(CHILD_ASK in json.dumps(item) for item in items):
                if release_child.wait(WATCHDOG_SECONDS):
                    self.tool('answer', ANSWER)
                else:
                    self.send_error(500)
            elif any(QUESTION in json.dumps(item) for item in items):
                self.tool('test', TEST)
            else:
                self.send_error(400)

        def tool(self, name, command):
            self.complete(name, {'type': 'function_call', 'call_id': name, 'name': 'shell',
                                 'arguments': json.dumps({'command': command})})

        def say(self, text):
            self.complete('message', {'type': 'message', 'role': 'assistant',
                                      'content': [{'type': 'output_text', 'text': text}]})

        def complete(self, name, item):
            self.respond({'type': 'response.completed', 'response': {
                'id': 'demo-' + name, 'status': 'completed', 'output': [item]}}, stream=True)

        def respond(self, body, stream=False):
            data = json.dumps(body).encode()
            if stream:
                data = b'data: ' + data + b'\n\n'
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            # The fixture owner can close an in-flight response on Thread input or quit.
            with suppress(BrokenPipeError, ConnectionResetError):
                self.wfile.write(data)

    return ThreadingHTTPServer(('127.0.0.1', 0), Handler)


def capture(binary, project):
    frames = []
    test, child, answered = Event(), Event(), Event()
    project.mkdir()
    (project / 'shellboy').symlink_to(binary)
    (project / 'checkout.py').write_text('def total(price, shipping):\n    return price + shipping + shipping\n')
    (project / 'test_checkout.py').write_text(
        'import unittest\nfrom checkout import total\n\n'
        'class CheckoutTest(unittest.TestCase):\n'
        '    def test_shipping_is_charged_once(self):\n'
        '        self.assertEqual(total(10, 5), 15)\n')
    (project / 'show-duty.py').write_text(SHOW_DUTY)
    with model(test, child, answered) as server:
        worker = Thread(target=server.serve_forever)
        worker.start()
        url = f'http://127.0.0.1:{server.server_port}/v1'
        (project / 'config.toml').write_text(
            'default_connection="fixture"\ndefault_model="offline-fixture"\n'
            f'[connections.fixture]\nbase_url="{url}"\n'
            '[ui]\ncopy_backend="osc52"\nbell=false\npalette="light"\nlanguage="en"\nsend_key="enter"\n')

        def launch():
            return Terminal(binary, root=project, cols=COLS, rows=ROWS, palette='light')

        def ready(terminal):
            terminal.expect(lambda text: 'Ctrl+T Threads' in text, 'startup')
            before = terminal.frames.frame_count
            terminal.send('\x0c')
            terminal.expect(lambda text: terminal.frames.frame_count > before
                            and 'Ctrl+T Threads' in text, 'interactive redraw')

        def still(terminal, caption, seconds, *markers):
            terminal.send('\x0c')
            terminal.expect(lambda screen: all(marker in screen for marker in markers)
                            and 'Loading output' not in screen, caption)
            # Later PTY frames mutate the decoder's rows.
            frames.append({'caption': caption, 'seconds': seconds,
                           'cells': [row.copy() for row in terminal.screen.buffer]})
            print(caption, flush=True)

        terminal = None
        try:
            terminal = launch()
            ready(terminal)
            terminal.paste(QUESTION)
            still(terminal, '01   Ask Shellboy to investigate', 3, QUESTION)
            terminal.send('\r')
            still(terminal, '02   Run the failing test in the shell', 5, 'FAILED (failures=1)')
            test.set()
            text = terminal.expect(lambda screen: 'python3 show-duty.py' in screen
                                   and 'Descendants | Active 1' in screen, 'active child and duty read')
            row = next(i for i, line in enumerate(text.splitlines(), 1) if 'python3 show-duty.py' in line)
            terminal.send(f'\x1b[<0;5;{row}M\x1b[<0;5;{row}m')
            still(terminal, '03   Ask a child Thread to investigate', 5,
                  './shellboy message ask --thread', CHILD_ASK, '1 reply owed')
            terminal.send('\x1b\x1b')
            child.set()
            terminal.send('\x1b[1;5F')
            still(terminal, '04   The child answers the Ask', 6, f'Reply: "{CHILD_ASK}"', REASON)
            answered.set()
            terminal.expect(lambda screen: 'The tests fail: expected 15, got 20.' in screen
                            and 'Completed' in screen, 'completed investigation')
            terminal.close()
            terminal = launch()
            ready(terminal)
            still(terminal, '05   Quit and start Shellboy again', 3, 'Ready for input')
            terminal.send('/threads\r')
            still(terminal, '06   Find the same conversation with /threads', 4, 'Run the tests and investig')
            terminal.send('\r')
            terminal.expect(lambda screen: 'Conversations |' not in screen
                            and 'Tab History' in screen, 'parent conversation opened')
            still(terminal, '07   Reopen the saved diagnosis', 6, 'Completed', REASON,
                  'The tests fail: expected 15, got 20.')
            terminal.close()
        finally:
            child.set()
            test.set()
            answered.set()
            if terminal:
                terminal.close(force=True)
            server.shutdown()
            worker.join()
    return frames


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True, help='trusted Core binary built from this checkout')
    parser.add_argument('--out', type=Path, default=ROOT / 'docs/assets/shellboy-demo.gif')
    args = parser.parse_args()
    with TemporaryDirectory(prefix='shellboy-demo-') as scratch:
        scratch = Path(scratch)
        frames = capture(args.binary.resolve(strict=True), scratch / 'checkout-demo')
        (scratch / 'frames.json').write_text(json.dumps(frames))
        subprocess.run(['swift', str(Path(__file__).with_name('render-readme-demo.swift')), str(scratch)], check=True)
        manifest = ''.join(f"file '{i}.png'\nduration {frame['seconds']}\n" for i, frame in enumerate(frames))
        # The concat demuxer needs a final timestamp to apply the final hold.
        (scratch / 'frames.txt').write_text(manifest + f"file '{len(frames) - 1}.png'\n")
        seconds = sum(frame['seconds'] for frame in frames)
        if not 20 <= seconds <= 40:
            raise ValueError(f'GIF must run 20-40 seconds, got {seconds}')
        output = scratch / 'demo.gif'
        subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-f', 'concat', '-safe', '1',
                        '-i', 'frames.txt', '-filter_complex',
                        'fps=5,split[a][b];[a]palettegen=max_colors=96[p];[b][p]paletteuse=dither=none',
                        '-t', str(seconds), '-loop', '0', str(output)], cwd=scratch, check=True)
        data = output.read_bytes()
        if len(data) > MAX_BYTES:
            raise ValueError(f'GIF is {len(data)} bytes; maximum is {MAX_BYTES}')
        args.out.write_bytes(data)
        print(f'{args.out}: {len(data)} bytes, {seconds} seconds')


if __name__ == '__main__':
    main()
