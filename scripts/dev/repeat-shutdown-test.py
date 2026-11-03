#!/usr/bin/env python3
"""Repeat only the disposable blocked-worker shutdown PTY test; retain every result."""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys

TEST = 'tui_shutdown::explicit_force_exits_a_blocked_db_worker_and_restores_terminal_modes'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path.cwd())
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--count', type=int, default=50)
    args = parser.parse_args()
    if not 1 <= args.count <= 100:
        parser.error('--count must be 1..100')
    source = args.source.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    record = {'test': TEST, 'count': args.count, 'platform': platform.platform(),
              'machine': platform.machine(), 'results': [],
              'sha': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=source,
                                             text=True).strip(),
              'runner': {key: os.environ.get(key) for key in ('ImageOS', 'ImageVersion', 'RUNNER_ARCH')}}

    def save():
        (args.output / 'results.json').write_text(json.dumps(record, indent=2) + '\n')

    save()
    with (args.output / 'build.jsonl').open('w') as output, (args.output / 'build.log').open('w') as errors:
        built = subprocess.run(['cargo', 'test', '--locked', '--test', 'integration', '--no-run',
                                '--message-format=json'], cwd=source, stdout=output, stderr=errors,
                               check=False)
    if built.returncode:
        record['build_exit'] = built.returncode
        save()
        return built.returncode
    binaries = [entry['executable'] for line in (args.output / 'build.jsonl').read_text().splitlines()
                if (entry := json.loads(line)).get('reason') == 'compiler-artifact'
                and entry.get('executable') and entry['target']['name'] == 'integration'
                and entry.get('profile', {}).get('test')]
    if len(binaries) != 1:
        raise ValueError(f'Expected one integration executable, found {binaries}')
    binary = binaries[0]
    listed = subprocess.check_output([binary, TEST, '--exact', '--list'], cwd=source, text=True)
    if f'{TEST}: test' not in listed:
        raise ValueError('Exact test was not registered; refusing a zero-test repeat')
    for index in range(args.count):
        log = f'repeat-{index + 1:03}.log'
        with (args.output / log).open('w') as output:
            try:
                result = subprocess.run([binary, TEST, '--exact', '--nocapture'], cwd=source,
                                        stdout=output, stderr=subprocess.STDOUT, timeout=120,
                                        check=False)
                code = result.returncode
            except subprocess.TimeoutExpired:
                code = 124
        record['results'].append({'iteration': index + 1, 'exit_code': code, 'log': log})
        record['failures'] = sum(item['exit_code'] != 0 for item in record['results'])
        save()
        print(f"{record['sha']} repeat {index + 1}/{args.count}: exit={code}; failures={record['failures']}",
              flush=True)
    return int(record['failures'] != 0)


if __name__ == '__main__':
    sys.exit(main())
