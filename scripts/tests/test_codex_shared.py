"""A shared Codex transport must not mix concurrent thread observations."""
import os
import queue
from pathlib import Path
import sys
import threading
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'bench'))
if os.name != 'nt':
    from compare import CodexSharedServer, CodexSharedSession


class FakeIO:
    def __init__(self, events):
        self.values = events

    def events(self, timeout):
        yield from self.values


@unittest.skipIf(os.name == 'nt', 'benchmark PTY dependencies are Unix-only')
class SharedTests(unittest.TestCase):
    def test_dispatch_separates_concurrent_responses_and_notifications(self):
        server = CodexSharedServer.__new__(CodexSharedServer)
        a, b, response = queue.Queue(), queue.Queue(), queue.Queue()
        server.lock = threading.Lock()
        server.pending = {7: response}
        server.threads = {'a': a, 'b': b}
        server.failure = None
        server.io = FakeIO([
            {'method': 'item/completed', 'params': {'threadId': 'b', 'value': 'B'}},
            {'id': 7, 'result': {'ok': True}},
            {'method': 'item/completed', 'params': {'threadId': 'a', 'value': 'A'}},
        ])
        server._dispatch()
        self.assertEqual(response.get_nowait()['result'], {'ok': True})
        self.assertEqual(a.get_nowait()['params']['value'], 'A')
        self.assertEqual(b.get_nowait()['params']['value'], 'B')
        self.assertTrue(a.empty() and b.empty())

    def test_unexpected_server_request_fails_all_waiters(self):
        server = CodexSharedServer.__new__(CodexSharedServer)
        waiting = queue.Queue()
        server.lock = threading.Lock()
        server.pending = {}
        server.threads = {'a': waiting}
        server.failure = None
        server.io = FakeIO([{'id': 1, 'method': 'approval', 'params': {}}])
        server._dispatch()
        self.assertIn('unexpected codex server request', waiting.get_nowait()['_failure'])

    def test_rejects_cross_turn_completion(self):
        class Server:
            def request(self, method, params):
                return {'turn': {'id': 'expected'}}
        events = queue.Queue()
        events.put({'method': 'turn/completed', 'params': {
            'threadId': 'a', 'turn': {'id': 'other', 'status': 'completed'}}})
        session = CodexSharedSession(Server(), 'a', events)
        with self.assertRaisesRegex(RuntimeError, 'mismatched'):
            session.turn('fixture')


if __name__ == '__main__':
    unittest.main()
