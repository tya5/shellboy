"""Fail-closed streaming relay for the approved #151 experiment.

No network listener is started by importing this module. The caller owns the
server and isolates clients so that only its loopback port is reachable.
"""
import http.client
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

from usage_budget import Budget, StopExperiment, UsageStream, normalize_usage, validate_request

MODEL = 'gpt-6.1-terra'
MAX_BODY = 16 * 1024 * 1024
SESSION_HEADERS = ('x-litellm-session-id', 'x-litellm-trace-id', 'x-shellboy-session-id')


class Meter:
    def __init__(self, upstream_port, ledger, api_key=None):
        self.upstream_port = upstream_port
        self.ledger = Path(ledger)
        self.api_key = api_key
        self.budget = Budget()
        self.agent = None
        self.lock = threading.Lock()

    def append(self, row):
        # This exclusive file is created by the controller in its private root.
        with self.ledger.open('a', encoding='utf-8') as out:
            out.write(json.dumps(row, separators=(',', ':')) + '\n')
            out.flush()
            import os
            os.fsync(out.fileno())

    def stop(self, reason):
        self.budget.stopped = reason
        self.budget.pending = False
        self.append({'type': 'stop', 'reason': reason})

    def set_agent(self, agent):
        if self.budget.stopped or self.budget.pending:
            raise StopExperiment('cannot start agent while stopped or pending')
        self.agent = agent


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, meter):
        self.meter = meter
        super().__init__(('127.0.0.1', 0), Handler)


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, *_args):
        pass

    def reject(self):
        try:
            self.send_response(502)
            self.send_header('Content-Length', '0')
            self.end_headers()
        except OSError:
            pass

    def do_GET(self):
        self.fail('unexpected HTTP method')

    do_HEAD = do_PUT = do_DELETE = do_PATCH = do_OPTIONS = do_GET

    def fail(self, reason):
        meter = self.server.meter
        if not meter.budget.stopped:
            meter.stop(reason)
        self.reject()

    def do_POST(self):
        meter = self.server.meter
        meter.lock.acquire()
        conn = None
        sent_headers = False
        try:
            if meter.budget.stopped or not meter.agent:
                raise StopExperiment(meter.budget.stopped or 'agent is not selected')
            length = self.headers.get('content-length')
            if not length or not length.isdigit() or not 0 < int(length) <= MAX_BODY:
                raise StopExperiment('invalid request length')
            raw = self.rfile.read(int(length))
            if len(raw) != int(length):
                raise StopExperiment('truncated request')
            body = json.loads(raw)
            # Fixed output cap, not a client-dependent default. The requested
            # model and route are never rewritten.
            parsed = urlsplit(self.path)
            if parsed.query not in ('', 'beta=true') or parsed.fragment:
                raise StopExperiment('unexpected query')
            path = parsed.path
            output_keys = {'/v1/responses': 'max_output_tokens', '/v1/chat/completions': 'max_tokens',
                           '/v1/messages': 'max_tokens'}
            key = output_keys.get(path)
            if not key:
                raise StopExperiment('unexpected endpoint: ' + path[:80])
            if key and isinstance(body, dict):
                for other in ('max_tokens', 'max_output_tokens', 'max_completion_tokens'):
                    if other != key and other in body:
                        raise StopExperiment('conflicting output cap')
                body[key] = 2048
                if path == '/v1/chat/completions':
                    options = body.get('stream_options') or {}
                    if not isinstance(options, dict):
                        raise StopExperiment('invalid stream options')
                    body['stream_options'] = {**options, 'include_usage': True}
            wire = validate_request(path, body)
            expected = {'shellboy': 'responses', 'codex': 'responses',
                        'claude-code': 'messages', 'opencode': 'chat'}
            if meter.agent in expected and wire != expected[meter.agent]:
                raise StopExperiment('unexpected agent route')
            if any(len(self.headers.get_all(name, [])) > 1 for name in SESSION_HEADERS):
                raise StopExperiment('duplicate session header')
            presence = {key: self.headers.get(name) is not None for key, name in
                        zip(('session', 'trace', 'shellboy'), SESSION_HEADERS)}
            outgoing = {'content-type': 'application/json', 'accept': 'text/event-stream'}
            for name in (*SESSION_HEADERS, 'x-opencode-session-id', 'anthropic-version', 'anthropic-beta', 'user-agent'):
                value = self.headers.get(name)
                if value is not None:
                    outgoing[name] = value
            if meter.api_key:
                outgoing['authorization'] = 'Bearer ' + meter.api_key
                if wire == 'messages':
                    outgoing['x-api-key'] = meter.api_key
            payload = json.dumps(body, separators=(',', ':')).encode()
            meter.budget.admit()
            meter.append({'type': 'request', 'agent': meter.agent, 'wire': wire,
                          'headers': presence, 'body_bytes': len(raw)})
            conn = http.client.HTTPConnection('127.0.0.1', meter.upstream_port, timeout=180)
            conn.request('POST', self.path, payload, outgoing)
            response = conn.getresponse()
            if response.status != 200:
                # Retain only usage counters, never upstream error text or headers.
                error_body = response.read(MAX_BODY + 1)
                try:
                    error = json.loads(error_body)
                    usage = normalize_usage(wire, error.get('usage'))
                    meter.append({'type': 'error_usage', 'agent': meter.agent,
                                  'input': usage[0], 'cached': usage[1], 'output': usage[2]})
                except (ValueError, AttributeError, StopExperiment):
                    pass
                raise StopExperiment('upstream error; internal retry usage unknown')
            if not response.getheader('content-type', '').lower().startswith('text/event-stream'):
                raise StopExperiment('unexpected response content type')
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            sent_headers = True
            stream = UsageStream(wire)
            data = []
            seen_model = False
            committed = False
            while True:
                line = response.readline(MAX_BODY + 1)
                if len(line) > MAX_BODY:
                    raise StopExperiment('response event too large')
                if not line:
                    break
                if line.startswith(b'data:'):
                    data.append(line[5:].strip())
                    if sum(map(len, data)) > MAX_BODY:
                        raise StopExperiment('response event too large')
                if line in (b'\n', b'\r\n') and data:
                    content = b'\n'.join(data)
                    data.clear()
                    if content == b'[DONE]':
                        stream.end_marker()
                    else:
                        event = json.loads(content)
                        if not isinstance(event, dict):
                            raise StopExperiment('invalid stream event')
                        candidate = event.get('model')
                        if wire == 'responses' and isinstance(event.get('response'), dict):
                            candidate = event['response'].get('model', candidate)
                        if wire == 'messages' and isinstance(event.get('message'), dict):
                            candidate = event['message'].get('model', candidate)
                        if candidate is not None:
                            if candidate != MODEL:
                                raise StopExperiment('unexpected response model')
                            seen_model = True
                        stream.event(event)
                    if stream.terminal and not committed:
                        if not seen_model:
                            raise StopExperiment('missing response model')
                        input_tokens, cached_tokens, output_tokens = stream.done()
                        before = len(meter.budget.rows)
                        try:
                            meter.budget.accept(meter.agent, input_tokens, cached_tokens, output_tokens, presence)
                        finally:
                            if len(meter.budget.rows) > before:
                                write_tokens = stream.usage.get('cache_creation_input_tokens') if wire == 'messages' else None
                                meter.append({'type': 'usage', 'agent': meter.agent, 'input': input_tokens,
                                              'cached': cached_tokens, 'cache_write': write_tokens,
                                              'output': output_tokens, 'total': meter.budget.total})
                        committed = True
                        if output_tokens > meter.budget.max_output:
                            raise StopExperiment('reported output exceeds fixed limit')
                self.wfile.write(line)
                self.wfile.flush()
            if data or not seen_model or not committed:
                raise StopExperiment('incomplete stream or missing response model')
        except (StopExperiment, OSError, ValueError, TypeError, http.client.HTTPException) as error:
            reason = str(error) if isinstance(error, StopExperiment) else 'request failed or incomplete; internal retry usage unknown'
            meter.stop(meter.budget.stopped or reason)
            if not sent_headers:
                self.reject()
        finally:
            if conn:
                conn.close()
            meter.lock.release()
