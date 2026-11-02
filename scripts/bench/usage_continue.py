#!/usr/bin/env python3
"""One explicitly approved continuation; never run live as part of CI."""
import json
import os
from pathlib import Path
import sys
import tempfile
import threading

import compare
import fake_model
from meter_relay import Meter, Server
from usage_trial import other, wrapper


def prior_usage():
    prior = Path('.local-bin/real-usage-ledger.jsonl')
    rows = [json.loads(line) for line in prior.read_text().splitlines()]
    usage = [row for row in rows if row['type'] == 'usage']
    end = [row for row in rows if row['type'] == 'end']
    total = sum(row['input'] + row['output'] for row in usage)
    if len(usage) != 19 or total != 156411 or len(end) != 1 or end[0]['total'] != total:
        raise RuntimeError('unexpected previous ledger; cannot restore budget')
    if usage[-1]['input'] != 12829:
        raise RuntimeError('unexpected prior input; cannot restore reserve')
    return total, usage[-1]['input']


def run(binary, upstream, ledger, api_key, fake=None):
    previous, last = prior_usage()
    meter = Meter(upstream, ledger, api_key)
    meter.budget.total = previous
    meter.budget.last_input = last
    meter.set_agent('opencode')
    server = Server(meter)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    meter.append({'type': 'start', 'agent': 'opencode', 'prior_total': previous,
                  'last_input': last, 'note': 'previous failed request and internal retry usage unknown'})
    try:
        with tempfile.TemporaryDirectory(prefix='live-opencode-', dir='/private/tmp') as scratch:
            root = Path(scratch).resolve()
            workspace = compare.make_workspace(root / 'workspace')
            if fake is not None:
                fake.traffic.set_workspaces([workspace])
            port = compare.free_port()
            binary = wrapper(root, binary, [server.server_port, port])
            try:
                other('opencode', binary, root, f'http://127.0.0.1:{server.server_port}', workspace, port)
                with meter.lock:
                    pass
                if meter.budget.stopped:
                    raise RuntimeError('relay stopped')
                if fake is not None and not fake.workload_complete():
                    raise RuntimeError('fake workload incomplete')
                meter.append({'type': 'agent_complete', 'agent': 'opencode'})
            except Exception:
                if not meter.budget.stopped:
                    meter.stop('agent failed or interrupted; in-flight usage may be unknown')
    except BaseException:
        if not meter.budget.stopped:
            meter.stop('controller interrupted; in-flight usage may be unknown')
        raise
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=10)
        meter.append({'type': 'end', 'total': meter.budget.total, 'stop': meter.budget.stopped})
    print(json.dumps({'agent': 'opencode', 'status': 'stopped' if meter.budget.stopped else 'complete',
                      'reason': meter.budget.stopped, 'total': meter.budget.total,
                      'requests': len(meter.budget.rows)}), flush=True)
    return meter.budget.stopped is None


def main():
    if sys.platform != 'darwin' or len(sys.argv) != 3 or sys.argv[1] not in ('--offline', '--execute-approved-151'):
        raise SystemExit('usage: usage_continue.py --offline|--execute-approved-151 OPENCODE')
    binary = Path(sys.argv[2]).resolve(strict=True)
    if sys.argv[1] == '--offline':
        with tempfile.TemporaryDirectory(prefix='continue-test-', dir='/private/tmp') as scratch, fake_model.FakeModel(1) as fake:
            ledger = Path(scratch) / 'ledger'
            ledger.touch(mode=0o600, exist_ok=False)
            if not run(binary, fake.server_port, ledger, None, fake):
                raise SystemExit(1)
    else:
        ledger = Path('.local-bin/real-usage-continuation.jsonl').resolve()
        fd = os.open(ledger, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
        if not run(binary, 4000, ledger, os.environ.get('OPENAI_API_KEY')):
            raise SystemExit(1)


if __name__ == '__main__':
    main()
