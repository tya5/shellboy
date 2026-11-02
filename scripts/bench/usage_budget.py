"""Fail-closed accounting for an explicitly approved live usage experiment.

This module does not perform network I/O or launch agents.
"""
from dataclasses import dataclass, field
import math


class StopExperiment(RuntimeError):
    pass


@dataclass
class Budget:
    limit: int = 1_500_000
    max_output: int = 2_048
    first_input_reserve: int = 100_000
    total: int = 0
    last_input: int | None = None
    pending: bool = False
    stopped: str | None = None
    rows: list = field(default_factory=list)

    def stop(self, reason):
        self.stopped = reason
        self.pending = False
        raise StopExperiment(reason)

    def admit(self):
        if self.stopped:
            raise StopExperiment(self.stopped)
        if self.pending:
            self.stop('concurrent requests are forbidden')
        reserve = self.first_input_reserve if self.last_input is None else math.ceil(self.last_input * 1.5)
        if self.total >= self.limit or self.limit - self.total < reserve + self.max_output:
            self.stop('reported usage budget exhausted or insufficient reserve')
        self.pending = True

    def accept(self, agent, input_tokens, cached_tokens, output_tokens, headers):
        if self.stopped or not self.pending:
            self.stop('response without one admitted request')
        def valid(value):
            return isinstance(value, int) and not isinstance(value, bool) and value >= 0
        if (not valid(input_tokens) or not valid(output_tokens) or
                (cached_tokens is not None and (not valid(cached_tokens) or cached_tokens > input_tokens))):
            self.stop('invalid or missing provider usage')
        if not isinstance(headers, dict) or set(headers) != {'session', 'trace', 'shellboy'} or any(
                not isinstance(value, bool) for value in headers.values()):
            self.stop('header presence is unknown')
        self.rows.append({'agent': agent, 'input': input_tokens, 'cached': cached_tokens,
                          'output': output_tokens, 'headers': dict(headers)})
        self.total += input_tokens + output_tokens
        self.last_input = input_tokens
        self.pending = False
        if self.total >= self.limit:
            self.stop('reported usage budget reached')

    def error(self):
        self.stop('upstream error; internal retry usage unknown')


def cache_ratios(rows, agent):
    selected = [row for row in rows if row['agent'] == agent]
    def ratio(group):
        if not group or any(row['cached'] is None for row in group):
            return None
        total = sum(row['input'] for row in group)
        return sum(row['cached'] for row in group) / total if total else None
    return {'all': ratio(selected), 'after_first': ratio(selected[1:])}


def normalize_usage(wire, usage):
    """Normalize explicit counters; never assume an omitted cache counter is zero."""
    if not isinstance(usage, dict):
        raise StopExperiment('missing provider usage')
    if wire == 'responses':
        details = usage.get('input_tokens_details')
        return (usage.get('input_tokens'),
                details.get('cached_tokens') if isinstance(details, dict) else None,
                usage.get('output_tokens'))
    if wire == 'chat':
        details = usage.get('prompt_tokens_details')
        return (usage.get('prompt_tokens'),
                details.get('cached_tokens') if isinstance(details, dict) else None,
                usage.get('completion_tokens'))
    if wire == 'messages':
        # Anthropic reports uncached input separately from cache read/write.
        # Missing cache fields make the total input ambiguous, not zero.
        fields = ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens')
        values = [usage.get(key) for key in fields]
        if any(not isinstance(x, int) or isinstance(x, bool) or x < 0 for x in values):
            raise StopExperiment('ambiguous Messages input usage')
        return sum(values), values[1], usage.get('output_tokens')
    raise StopExperiment('unsupported wire format')


class UsageStream:
    """Extract only terminal usage counters from in-memory SSE events."""
    def __init__(self, wire):
        self.wire = wire
        self.usage = None
        self.failed = False
        self.terminal = False

    def event(self, event):
        if not isinstance(event, dict):
            raise StopExperiment('invalid SSE event')
        kind = event.get('type')
        if kind in ('error', 'response.failed', 'response.incomplete') or event.get('error'):
            self.failed = True
            raise StopExperiment('upstream error; internal retry usage unknown')
        if self.wire == 'responses' and kind == 'response.completed':
            self.terminal = True
            self.usage = (event.get('response') or {}).get('usage')
        elif self.wire == 'chat':
            if event.get('usage') is not None:
                self.usage = event['usage']
        elif self.wire == 'messages':
            if kind == 'message_start':
                self.usage = dict((event.get('message') or {}).get('usage') or {})
            elif kind == 'message_delta' and isinstance(event.get('usage'), dict):
                if self.usage is None:
                    self.usage = {}
                self.usage.update(event['usage'])
            elif kind == 'message_stop':
                self.terminal = True

    def end_marker(self):
        if self.wire == 'chat':
            self.terminal = True

    def done(self):
        if self.failed or not self.terminal:
            raise StopExperiment('incomplete provider stream')
        return normalize_usage(self.wire, self.usage)


def validate_request(path, body):
    """Reject unknown routes, alternate models and uncapped output before forwarding."""
    wires = {'/v1/responses': ('responses', 'max_output_tokens'),
             '/v1/chat/completions': ('chat', 'max_tokens'),
             '/v1/messages': ('messages', 'max_tokens')}
    if path not in wires or not isinstance(body, dict):
        raise StopExperiment('unknown API path or request')
    wire, output_key = wires[path]
    if body.get('model') != 'gpt-6.1-terra':
        raise StopExperiment('unexpected model')
    limit = body.get(output_key)
    if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= 2048:
        raise StopExperiment('missing or excessive output limit')
    if body.get('stream') is not True:
        raise StopExperiment('streaming usage is required')
    if body.get('models') or body.get('fallbacks') or body.get('route'):
        raise StopExperiment('fallback routing is forbidden')
    return wire
