"""Offline loopback tests: no request reaches a real provider."""
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'bench'))
from meter_relay import Meter, Server

MODEL = 'gpt-6.1-terra'


class Fake(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        server = self.server
        server.requests.append((self.path, json.loads(self.rfile.read(int(self.headers['content-length']))),
                                dict(self.headers)))
        self.send_response(server.status)
        self.send_header('Content-Type', 'text/event-stream' if server.status == 200 else 'application/json')
        self.end_headers()
        self.wfile.write(server.response)


def event(data):
    return b'data: ' + json.dumps(data).encode() + b'\n\n'


def fixture(wire, *, model=MODEL, missing=False):
    if wire == 'responses':
        usage = {} if missing else {'input_tokens': 10, 'input_tokens_details': {'cached_tokens': 5}, 'output_tokens': 3}
        return event({'type': 'response.completed', 'response': {'model': model, 'usage': usage}})
    if wire == 'chat':
        usage = None if missing else {'prompt_tokens': 10, 'prompt_tokens_details': {'cached_tokens': 5}, 'completion_tokens': 3}
        return event({'model': model, 'usage': usage}) + b'data: [DONE]\n\n'
    usage = {} if missing else {'input_tokens': 4, 'cache_read_input_tokens': 5, 'cache_creation_input_tokens': 1}
    return (event({'type': 'message_start', 'message': {'model': model, 'usage': usage}}) +
            event({'type': 'message_delta', 'usage': {'output_tokens': 3}}) + event({'type': 'message_stop'}))


@contextmanager
def servers(response, status=200):
    upstream = ThreadingHTTPServer(('127.0.0.1', 0), Fake)
    upstream.requests, upstream.response, upstream.status = [], response, status
    with tempfile.TemporaryDirectory() as root:
        ledger = Path(root) / 'ledger'
        ledger.touch(mode=0o600, exist_ok=False)
        meter = Meter(upstream.server_port, ledger)
        meter.set_agent('test')
        relay = Server(meter)
        threads = [threading.Thread(target=s.serve_forever, daemon=True) for s in (upstream, relay)]
        for thread in threads:
            thread.start()
        try:
            yield upstream, relay, meter, ledger
        finally:
            for server in (relay, upstream):
                server.shutdown()
                server.server_close()
            for thread in threads:
                thread.join()


def post(port, path='/v1/responses', body=None, headers=None):
    body = body if body is not None else {'model': MODEL, 'stream': True}
    conn = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
    conn.request('POST', path, json.dumps(body), {'Content-Type': 'application/json', **(headers or {})})
    response = conn.getresponse()
    result = response.status, response.read()
    conn.close()
    return result


class RelayTests(unittest.TestCase):
    def test_each_wire_usage_caps_and_header_privacy(self):
        for wire, path in (('responses', '/v1/responses'), ('chat', '/v1/chat/completions'),
                           ('messages', '/v1/messages')):
            with self.subTest(wire=wire), servers(fixture(wire)) as (up, relay, meter, ledger):
                status, _ = post(relay.server_port, path, headers={'X-Litellm-Session-Id': 'private-value'})
                self.assertEqual(status, 200)
                # Handler may still be flushing the usage record after client EOF.
                with meter.lock:
                    pass
                self.assertEqual(meter.budget.total, 13)
                self.assertEqual(meter.budget.rows[0]['cached'], 5)
                self.assertTrue(meter.budget.rows[0]['headers']['session'])
                self.assertEqual(up.requests[0][1]['max_output_tokens' if wire == 'responses' else 'max_tokens'], 2048)
                self.assertEqual(up.requests[0][2]['x-litellm-session-id'], 'private-value')
                self.assertNotIn('private-value', ledger.read_text())
                if wire == 'chat':
                    self.assertTrue(up.requests[0][1]['stream_options']['include_usage'])

    def test_model_and_path_rejected_without_forwarding(self):
        for path, body in [('/v1/responses', {'model': 'other', 'stream': True}),
                           ('/v1/other', {'model': MODEL, 'stream': True})]:
            with self.subTest(path=path), servers(b'') as (up, relay, meter, _):
                self.assertEqual(post(relay.server_port, path, body)[0], 502)
                self.assertTrue(meter.budget.stopped)
                self.assertEqual(up.requests, [])

    def test_errors_or_missing_usage_prevent_future_forwarding(self):
        cases = [(500, b'{"error":"secret"}'), (200, fixture('responses', missing=True)),
                 (200, fixture('responses', model='different')), (200, b'data: {"type":"response.created"}\n\n')]
        for status, data in cases:
            with self.subTest(status=status, data=data[:40]), servers(data, status) as (up, relay, meter, ledger):
                post(relay.server_port)
                with meter.lock:
                    pass
                self.assertTrue(meter.budget.stopped)
                self.assertEqual(post(relay.server_port)[0], 502)
                self.assertEqual(len(up.requests), 1)
                self.assertNotIn('secret', ledger.read_text())

    def test_budget_overshoot_is_retained(self):
        data = event({'type': 'response.completed', 'response': {'model': MODEL, 'usage': {
            'input_tokens': 1500000, 'output_tokens': 12}}})
        with servers(data) as (up, relay, meter, ledger):
            post(relay.server_port)
            with meter.lock:
                pass
            self.assertTrue(meter.budget.stopped)
            self.assertIn('1500012', ledger.read_text())
            self.assertEqual(post(relay.server_port)[0], 502)
            self.assertEqual(len(up.requests), 1)


if __name__ == '__main__':
    unittest.main()
