#!/usr/bin/env python3
"""Deterministic loopback model for the #116 cross-agent comparison (stage 2).

One workload, three wire formats: OpenAI Responses SSE, OpenAI Chat Completions
SSE and Anthropic Messages SSE. Event shapes follow the synthetic fixtures in
tests/fixtures/{responses,chat,messages}. No credentials, no real provider.

The round is chosen by the last user message containing one of the five fixed
instructions (footprint.PROMPTS). Rounds 1-4 first answer with one shell call
(footprint.COMMANDS) and, once a tool result follows the instruction, with the
fixed answer. Round 5 only answers. Requests that carry no shell tool or no
fixed instruction (titles, summaries, quota probes) are "auxiliary" and get a
short fixed text. Tool calls are capped at one per session per round and all
requests at a fixed total, so no agent can loop without bound.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from footprint import ANSWERS, COMMANDS, PROMPTS  # noqa: E402

MODEL = 'bench-fake'
AUX_TEXT = 'ok'
SHELL_TOOLS = ('shell', 'Bash', 'bash', 'shell_command', 'exec_command')
TOOL_RESULT_ITEMS = ('function_call_output', 'custom_tool_call_output', 'local_shell_call_output')
MAX_BODY = 8 * 1024 * 1024


def _text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return '\n'.join(part.get('text', '') for part in content
                         if isinstance(part, dict) and isinstance(part.get('text'), str))
    return ''


def normalize(wire, body):
    """Return ([(role, text, is_tool_result)], {tool name: JSON schema})."""
    messages, tools = [], {}
    if wire == 'responses':
        for item in body.get('input') or []:
            if item.get('type') in TOOL_RESULT_ITEMS:
                messages.append(('tool', '', True))
            elif item.get('type', 'message') == 'message':
                messages.append((item.get('role', ''), _text(item.get('content')), False))
        for tool in body.get('tools') or []:
            if tool.get('type') == 'function':
                tools[tool.get('name')] = tool.get('parameters') or {}
            elif tool.get('type') == 'local_shell':
                tools['local_shell'] = {}
    elif wire == 'chat':
        for message in body.get('messages') or []:
            role = message.get('role', '')
            messages.append((role, _text(message.get('content')), role == 'tool'))
        for tool in body.get('tools') or []:
            function = tool.get('function') or {}
            tools[function.get('name')] = function.get('parameters') or {}
    elif wire == 'messages':
        for message in body.get('messages') or []:
            content = message.get('content')
            result = isinstance(content, list) and any(
                isinstance(part, dict) and part.get('type') == 'tool_result' for part in content)
            messages.append((message.get('role', ''), _text(content), result))
        for tool in body.get('tools') or []:
            if 'name' in tool:
                tools[tool['name']] = tool.get('input_schema') or {}
    else:
        raise ValueError(f'unknown wire {wire}')
    return messages, tools


def shell_arguments(schema, command, index):
    props = (schema or {}).get('properties') or {}
    if 'cmd' in props:
        args = {'cmd': command}
    elif 'command' in props:
        array = props['command'].get('type') == 'array'
        args = {'command': ['bash', '-lc', command] if array else command}
    else:
        raise ValueError('shell tool without a command parameter')
    if 'description' in props:
        args['description'] = f'Benchmark round {index + 1}'
    return args


def decide(messages, tools):
    """('aux', text) | ('tool', round, name, args) | ('answer', round, text)."""
    shell = next((name for name in SHELL_TOOLS if name in tools), None)
    for pos in range(len(messages) - 1, -1, -1):
        role, text, _ = messages[pos]
        if role != 'user':
            continue
        matches = [index for index, prompt in enumerate(PROMPTS) if prompt in text]
        if not matches:
            continue
        index = matches[-1]
        if shell is None:
            return ('aux', AUX_TEXT)
        followup = any(result for _, _, result in messages[pos + 1:])
        if index < len(COMMANDS) and not followup:
            return ('tool', index, shell, shell_arguments(tools[shell], COMMANDS[index], index))
        return ('answer', index, ANSWERS[index])
    return ('aux', AUX_TEXT)


def sse(events, done=False):
    chunks = []
    for name, data in events:
        prefix = f'event: {name}\n' if name else ''
        chunks.append(f'{prefix}data: {json.dumps(data)}\n\n')
    if done:
        chunks.append('data: [DONE]\n\n')
    return ''.join(chunks).encode()


def responses_body(rid, action, stream):
    if action[0] == 'tool':
        item = {'type': 'function_call', 'id': f'fc_{rid}', 'call_id': f'call_{rid}',
                'name': action[2], 'arguments': json.dumps(action[3]), 'status': 'completed'}
    else:
        item = {'type': 'message', 'id': f'msg_{rid}', 'role': 'assistant', 'status': 'completed',
                'content': [{'type': 'output_text', 'text': action[-1], 'annotations': []}]}
    usage = {'input_tokens': 1, 'input_tokens_details': {'cached_tokens': 0}, 'output_tokens': 1,
             'output_tokens_details': {'reasoning_tokens': 0}, 'total_tokens': 2}
    response = {'id': f'resp_{rid}', 'object': 'response', 'created_at': int(time.time()),
                'model': MODEL, 'status': 'completed', 'output': [item], 'usage': usage}
    if not stream:
        return 'application/json', json.dumps(response).encode()
    started = dict(response, status='in_progress', output=[], usage=None)
    events = [('response.created', {'type': 'response.created', 'response': started}),
              ('response.output_item.added', {'type': 'response.output_item.added', 'output_index': 0,
                                              'item': dict(item, status='in_progress')})]
    if item['type'] == 'message':
        events.append(('response.output_text.delta', {
            'type': 'response.output_text.delta', 'item_id': item['id'], 'output_index': 0,
            'content_index': 0, 'delta': action[-1]}))
    events += [('response.output_item.done', {'type': 'response.output_item.done', 'output_index': 0,
                                              'item': item}),
               ('response.completed', {'type': 'response.completed', 'response': response})]
    return 'text/event-stream', sse(events)


def chat_body(rid, action, stream, include_usage):
    usage = {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}
    if action[0] == 'tool':
        call = {'id': f'call_{rid}', 'type': 'function',
                'function': {'name': action[2], 'arguments': json.dumps(action[3])}}
        message, finish = {'role': 'assistant', 'content': None, 'tool_calls': [call]}, 'tool_calls'
        delta = {'role': 'assistant', 'tool_calls': [dict(call, index=0)]}
    else:
        message, finish = {'role': 'assistant', 'content': action[-1]}, 'stop'
        delta = {'role': 'assistant', 'content': action[-1]}
    base = {'id': f'chatcmpl_{rid}', 'created': int(time.time()), 'model': MODEL}
    if not stream:
        return 'application/json', json.dumps(dict(base, object='chat.completion', usage=usage, choices=[
            {'index': 0, 'message': message, 'finish_reason': finish}])).encode()
    chunk = dict(base, object='chat.completion.chunk')
    events = [(None, dict(chunk, choices=[{'index': 0, 'delta': delta, 'finish_reason': None}])),
              (None, dict(chunk, choices=[{'index': 0, 'delta': {}, 'finish_reason': finish}]))]
    if include_usage:
        events.append((None, dict(chunk, choices=[], usage=usage)))
    return 'text/event-stream', sse(events, done=True)


def messages_body(rid, action, stream):
    if action[0] == 'tool':
        block = {'type': 'tool_use', 'id': f'toolu_{rid}', 'name': action[2], 'input': action[3]}
        stop = 'tool_use'
    else:
        block, stop = {'type': 'text', 'text': action[-1]}, 'end_turn'
    usage = {'input_tokens': 1, 'output_tokens': 1, 'cache_creation_input_tokens': 0,
             'cache_read_input_tokens': 0}
    message = {'id': f'msg_{rid}', 'type': 'message', 'role': 'assistant', 'model': MODEL,
               'content': [block], 'stop_reason': stop, 'stop_sequence': None, 'usage': usage}
    if not stream:
        return 'application/json', json.dumps(message).encode()
    if block['type'] == 'tool_use':
        start = dict(block, input={})
        delta = {'type': 'input_json_delta', 'partial_json': json.dumps(block['input'])}
    else:
        start, delta = dict(block, text=''), {'type': 'text_delta', 'text': block['text']}
    events = [('message_start', {'type': 'message_start',
                                 'message': dict(message, content=[], stop_reason=None)}),
              ('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': start}),
              ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': delta}),
              ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
              ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': stop, 'stop_sequence': None},
                                 'usage': {'output_tokens': 1}}),
              ('message_stop', {'type': 'message_stop'})]
    return 'text/event-stream', sse(events)


ROUTES = {'/v1/responses': 'responses', '/responses': 'responses',
          '/v1/chat/completions': 'chat', '/chat/completions': 'chat',
          '/v1/messages': 'messages'}


class FakeModel(ThreadingHTTPServer):
    """Bounded loopback server; `stats()` reports exactly what it served."""
    daemon_threads = True
    request_queue_size = 128

    def __init__(self, sessions, max_requests=None):
        super().__init__(('127.0.0.1', 0), Handler)
        self.sessions = sessions
        self.max_requests = max_requests or 40 * sessions + 200
        self.lock = threading.Lock()
        self.requests = 0
        self.wires = {}
        self.tools = [0] * len(COMMANDS)
        self.followups = [0] * len(COMMANDS)
        self.answers = [0] * len(PROMPTS)
        self.aux = 0
        self.tool_budget_exhausted = 0
        self.models = set()
        self.shell_tools = set()
        self.unexpected = []
        self.errors = []
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_exc):
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)

    @property
    def origin(self):
        return f'http://127.0.0.1:{self.server_port}'

    def note(self, kind, value):
        with self.lock:
            target = self.unexpected if kind == 'unexpected' else self.errors
            if len(target) < 50:
                target.append(value)

    def account(self, action, wire, model):
        """Count one served request, demoting a tool call past its budget to the answer."""
        with self.lock:
            self.requests += 1
            self.wires[wire] = self.wires.get(wire, 0) + 1
            if model:
                self.models.add(str(model)[:80])
            if self.requests > self.max_requests:
                raise OverflowError('fake model request cap reached')
            if action[0] == 'tool':
                if self.tools[action[1]] >= self.sessions:
                    self.tool_budget_exhausted += 1
                    action = ('answer', action[1], ANSWERS[action[1]])
                else:
                    self.tools[action[1]] += 1
                    self.shell_tools.add(action[2])
                    return action
            if action[0] == 'aux':
                self.aux += 1
            else:
                self.answers[action[1]] += 1
                if action[1] < len(COMMANDS):
                    self.followups[action[1]] += 1
            return action

    def stats(self):
        with self.lock:
            return {'requests': self.requests, 'requests_by_wire': dict(self.wires),
                    'tool_calls_by_round': list(self.tools), 'answers_by_round': list(self.answers),
                    'auxiliary_requests': self.aux, 'tool_budget_exhausted': self.tool_budget_exhausted,
                    'requested_models': sorted(self.models), 'shell_tools_called': sorted(self.shell_tools), 'unexpected_requests': list(self.unexpected),
                    'errors': list(self.errors)}

    def workload_complete(self):
        """Every session got exactly one tool call and one answer per round."""
        with self.lock:
            n = self.sessions
            return (self.tools == [n] * len(COMMANDS) and self.answers == [n] * len(PROMPTS)
                    and not self.errors and not self.tool_budget_exhausted)


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def reply(self, status, content_type, payload):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        path = self.path.split('?', 1)[0].rstrip('/')
        if path in ('/v1/models', '/models'):
            model = {'id': MODEL, 'object': 'model', 'type': 'model', 'display_name': MODEL,
                     'created': 0, 'created_at': '2026-01-01T00:00:00Z', 'owned_by': 'bench'}
            self.reply(200, 'application/json', json.dumps(
                {'object': 'list', 'data': [model], 'models': [{'slug': MODEL}], 'has_more': False,
                 'first_id': MODEL, 'last_id': MODEL}).encode())
            return
        self.server.note('unexpected', f'GET {path}'[:120])
        self.reply(404, 'application/json', b'{"error":{"message":"not found"}}')

    def do_HEAD(self):
        self.reply(200, 'text/plain', b'')

    def do_POST(self):
        self.connection.settimeout(60)
        path = self.path.split('?', 1)[0].rstrip('/')
        try:
            size = int(self.headers.get('Content-Length', '0'))
            if not 0 < size <= MAX_BODY:
                raise ValueError('missing or oversized body')
            body = json.loads(self.rfile.read(size))
            if path == '/v1/messages/count_tokens':
                self.reply(200, 'application/json', b'{"input_tokens":1}')
                return
            wire = ROUTES.get(path)
            if wire is None:
                self.server.note('unexpected', f'POST {path}'[:120])
                self.reply(404, 'application/json', b'{"error":{"message":"not found"}}')
                return
            action = self.server.account(decide(*normalize(wire, body)), wire, body.get('model'))
            rid = f'{self.server.requests}'
            stream = bool(body.get('stream'))
            if wire == 'responses':
                content_type, payload = responses_body(rid, action, stream)
            elif wire == 'chat':
                include = bool((body.get('stream_options') or {}).get('include_usage'))
                content_type, payload = chat_body(rid, action, stream, include)
            else:
                content_type, payload = messages_body(rid, action, stream)
            self.reply(200, content_type, payload)
        except OverflowError as error:
            self.server.note('error', str(error))
            self.reply(400, 'application/json', b'{"error":{"type":"invalid_request_error","message":"cap"}}')
        except (ValueError, KeyError, TypeError, AttributeError) as error:
            self.server.note('error', f'{path}: {type(error).__name__}: {error}'[:300])
            self.reply(400, 'application/json', b'{"error":{"type":"invalid_request_error","message":"bad"}}')
        except (BrokenPipeError, ConnectionResetError):
            self.server.note('error', 'client disconnected')

    def log_message(self, *_args):
        pass


if __name__ == '__main__':
    # Manual protocol exploration only: serve until interrupted, then print stats.
    with FakeModel(int(sys.argv[1]) if len(sys.argv) > 1 else 1) as server:
        print(server.origin, flush=True)
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            print(json.dumps(server.stats(), indent=2))
