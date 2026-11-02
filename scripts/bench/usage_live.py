#!/usr/bin/env python3
"""Run the one explicitly approved #151 live experiment, never a connectivity test.

Run only after offline tests and sandbox bypass checks have passed. The exclusive
ledger prevents accidental reuse of the same output path after any interruption.
"""
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import threading

import compare
import footprint
from meter_relay import Meter, Server
from usage_trial import shellboy, other, wrapper


def main():
    if sys.platform != 'darwin' or len(sys.argv) != 4 or sys.argv[1] != '--execute-approved-151':
        raise SystemExit('usage: usage_live.py --execute-approved-151 SHELLBOY OPENCODE')
    binary = {'shellboy': Path(sys.argv[2]).resolve(strict=True),
              'codex': Path(shutil.which('codex')).resolve(strict=True),
              'claude-code': Path(shutil.which('claude')).resolve(strict=True),
              'opencode': Path(sys.argv[3]).resolve(strict=True)}
    ledger = Path('.local-bin/real-usage-ledger.jsonl').resolve()
    fd = os.open(ledger, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.close(fd)
    # Only the relay sees the existing proxy key; isolated clients get dummy keys.
    meter = Meter(4000, ledger, os.environ.get('OPENAI_API_KEY'))
    relay = Server(meter)
    worker = threading.Thread(target=relay.serve_forever, daemon=True)
    worker.start()
    meter.append({'type': 'start', 'model': 'gpt-6.1-terra',
                  'note': 'LiteLLM internal retry usage unknown'})
    try:
        for name, executable in binary.items():
            meter.set_agent(name)
            with tempfile.TemporaryDirectory(prefix='live-', dir='/private/tmp') as scratch:
                root = Path(scratch).resolve()
                workspace = compare.make_workspace(root / 'workspace')
                port = compare.free_port()
                allowed_ports = [relay.server_port, port] if name == 'opencode' else [relay.server_port]
                isolated = wrapper(root, executable, allowed_ports)
                meter.append({'type': 'agent_start', 'agent': name})
                try:
                    if name == 'shellboy':
                        shellboy(isolated, root, f'http://127.0.0.1:{relay.server_port}', workspace)
                    else:
                        other(name, isolated, root, f'http://127.0.0.1:{relay.server_port}', workspace, port)
                    with meter.lock:
                        pass
                    if meter.budget.stopped:
                        break
                    meter.append({'type': 'agent_complete', 'agent': name})
                    print(json.dumps({'agent': name, 'status': 'complete', 'total': meter.budget.total}), flush=True)
                except Exception:
                    if not meter.budget.stopped:
                        meter.stop('agent failed or interrupted; in-flight usage may be unknown')
                    print(json.dumps({'agent': name, 'status': 'stopped', 'reason': meter.budget.stopped,
                                      'total': meter.budget.total}), flush=True)
                    break
    except BaseException:
        if not meter.budget.stopped:
            meter.stop('controller interrupted; in-flight usage may be unknown')
        raise
    finally:
        relay.shutdown()
        relay.server_close()
        worker.join(timeout=10)
        meter.append({'type': 'end', 'total': meter.budget.total, 'stop': meter.budget.stopped})
        print(json.dumps({'status': 'stopped' if meter.budget.stopped else 'complete',
                          'total': meter.budget.total, 'requests': len(meter.budget.rows)}), flush=True)


if __name__ == '__main__':
    main()
