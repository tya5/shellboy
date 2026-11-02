"""#116 stage-2 fake model: one bounded workload over three wire formats."""
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from urllib.request import ProxyHandler, Request, build_opener

BENCH = Path(__file__).resolve().parents[1] / 'bench'
sys.path.insert(0, str(BENCH))
spec = importlib.util.spec_from_file_location('fake_model', BENCH / 'fake_model.py')
fake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fake)
open_url = build_opener(ProxyHandler({})).open

SHELL_SCHEMA = {'type': 'object', 'properties': {'command': {'type': 'string'},
                                                 'description': {'type': 'string'}}}


def request(wire, history):
    """A minimal request body in `wire` format; history items are ('user', text) or ('tool',)."""
    if wire == 'responses':
        items = [{'type': 'function_call_output', 'call_id': 'c', 'output': 'x'} if entry[0] == 'tool'
                 else {'role': 'user', 'content': [{'type': 'input_text', 'text': entry[1]}]}
                 for entry in history]
        return {'model': 'm', 'stream': True, 'input': items,
                'tools': [{'type': 'function', 'name': 'shell', 'parameters': {
                    'type': 'object', 'properties': {'command': {'type': 'array'}}}}]}
    if wire == 'chat':
        messages = [{'role': 'tool', 'tool_call_id': 'c', 'content': 'x'} if entry[0] == 'tool'
                    else {'role': 'user', 'content': entry[1]} for entry in history]
        return {'model': 'm', 'stream': True, 'stream_options': {'include_usage': True}, 'messages': messages,
                'tools': [{'type': 'function', 'function': {'name': 'bash', 'parameters': SHELL_SCHEMA}}]}
    messages = [{'role': 'user', 'content': [{'type': 'tool_result', 'tool_use_id': 't', 'content': 'x'}]}
                if entry[0] == 'tool' else
                {'role': 'user', 'content': [{'type': 'text', 'text': f'<reminder/>\n{entry[1]}'}]}
                for entry in history]
    return {'model': 'm', 'stream': True, 'messages': messages,
            'tools': [{'name': 'Bash', 'input_schema': SHELL_SCHEMA}]}


def post(origin, path, body):
    data = json.dumps(body).encode()
    with open_url(Request(origin + path, data=data, headers={'content-type': 'application/json'}),
                  timeout=10) as response:
        return response.read().decode()


class DecideTests(unittest.TestCase):
    def test_each_wire_gets_four_tools_then_answers(self):
        for wire in ('responses', 'chat', 'messages'):
            history, actions = [], []
            for index, prompt in enumerate(fake.PROMPTS):
                history.append(('user', prompt))
                action = fake.decide(*fake.normalize(wire, request(wire, history)))
                actions.append(action[0])
                if action[0] == 'tool':
                    self.assertIn(fake.COMMANDS[index], json.dumps(action[3]))
                    history.append(('tool',))
                    action = fake.decide(*fake.normalize(wire, request(wire, history)))
                    actions.append(action[0])
                self.assertEqual(action, ('answer', index, fake.ANSWERS[index]))
            self.assertEqual(actions, ['tool', 'answer'] * 4 + ['answer'], wire)

    def test_array_command_schema_and_auxiliary_requests(self):
        body = request('responses', [('user', fake.PROMPTS[0])])
        action = fake.decide(*fake.normalize('responses', body))
        self.assertEqual(action[3], {'command': ['bash', '-lc', fake.COMMANDS[0]]})
        body['tools'] = []
        self.assertEqual(fake.decide(*fake.normalize('responses', body)), ('aux', fake.AUX_TEXT))
        body = request('chat', [('user', 'Generate a title for this conversation')])
        self.assertEqual(fake.decide(*fake.normalize('chat', body)), ('aux', fake.AUX_TEXT))


class ServerTests(unittest.TestCase):
    def test_session_header_attributes_auxiliary_request(self):
        with fake.FakeModel(2) as server:
            server.traffic.set_session_ids(['first-id', 'second-id'])
            body = json.dumps(request('chat', [('user', 'title')])).encode()
            req = Request(server.origin + '/v1/chat/completions', data=body,
                          headers={'content-type': 'application/json',
                                   'x-opencode-session-id': 'second-id'})
            with open_url(req, timeout=10) as response:
                response.read()
            self.assertEqual(server.traffic.result()['requests'][0]['conversation'], 1)

    def test_body_bytes_are_recorded_without_body_content(self):
        with fake.FakeModel(1) as server:
            body = request('responses', [('user', fake.PROMPTS[4])])
            body['metadata'] = {'fixture': '\u00e5'}
            raw = json.dumps(body, ensure_ascii=False).encode('utf-8')
            with open_url(Request(server.origin + '/v1/responses', data=raw,
                                  headers={'content-type': 'application/json'}), timeout=10) as response:
                response.read()
            result = server.traffic.result()
            self.assertEqual(result['requests'], [{'path': '/v1/responses',
                                                   'bytes': len(raw), 'conversation': 0}])
            self.assertEqual(result['conversations'][0]['first_bytes'], len(raw))
            self.assertNotIn('fixture', json.dumps(result))

    def test_streams_are_wire_shaped_and_tool_budget_is_enforced(self):
        with fake.FakeModel(1, max_requests=6) as server:
            first = post(server.origin, '/v1/messages', request('messages', [('user', fake.PROMPTS[0])]))
            self.assertIn('event: message_start', first)
            self.assertIn('"stop_reason": "tool_use"', first)
            # A repeated first request exceeds one call per session and is answered instead.
            again = post(server.origin, '/v1/chat/completions', request('chat', [('user', fake.PROMPTS[0])]))
            self.assertIn('"finish_reason": "stop"', again)
            self.assertTrue(again.endswith('data: [DONE]\n\n'))
            done = post(server.origin, '/v1/responses', request('responses', [('user', fake.PROMPTS[4])]))
            self.assertIn('event: response.output_item.done', done)
            self.assertIn('event: response.completed', done)
            stats = server.stats()
            self.assertEqual(stats['tool_calls_by_round'], [1, 0, 0, 0])
            self.assertEqual(stats['tool_budget_exhausted'], 1)
            self.assertEqual(stats['shell_tools_called'], ['Bash'])
            self.assertFalse(server.workload_complete())
            for _ in range(3):
                post(server.origin, '/v1/responses', request('responses', [('user', 'x')]))
            with self.assertRaises(Exception):
                post(server.origin, '/v1/responses', request('responses', [('user', 'x')]))
            self.assertIn('fake model request cap reached', server.stats()['errors'])


if __name__ == '__main__':
    unittest.main()
