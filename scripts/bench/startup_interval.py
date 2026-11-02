#!/usr/bin/env python3
"""Compare retry-interval sensitivity of spawn-to-echo, no model requests."""
import argparse
import json
from pathlib import Path
import statistics
import tempfile
import compare


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--runs', type=int, default=5)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    results = {}
    for interval in (.01, .05, .1):
        samples = []
        for _ in range(args.runs):
            with tempfile.TemporaryDirectory(prefix='sb-startup-') as scratch:
                root = Path(scratch).resolve()
                workspace = compare.make_workspace(root / 'workspace')
                agent = compare.Shellboy(binary)
                env = agent.prepare(root, 'http://127.0.0.1:9', [workspace])
                try:
                    samples.append(compare.measure_startup(agent.tui_argv(root), workspace, env,
                                                           probe_interval=interval))
                finally:
                    agent.clean()
        values = [x['input_echo_ms'] for x in samples]
        results[str(interval)] = {'median_ms': statistics.median(values), 'min_ms': min(values),
                                  'max_ms': max(values), 'samples': samples}
    print(json.dumps(results, indent=2))


if __name__ == '__main__':
    main()
