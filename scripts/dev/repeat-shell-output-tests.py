#!/usr/bin/env python3
"""Repeat the two shell-output integration regressions and retain exact-source evidence."""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import shutil
import sys

TESTS = (
    'runtime::slow_database_batches_binary_shell_output_without_losing_natural_exit_tail',
    'shell::leading_dash_command_is_a_script_not_a_bash_option',
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--count', type=int, default=30)
    parser.add_argument('--binary', type=Path, help='reuse an already-built integration test executable')
    parser.add_argument('--test-threads', type=int, default=4,
                        help='parallel test threads for the concurrent suite (default: 4)')
    parser.add_argument('--suite-load', action='store_true',
                        help='run the full integration suite concurrently with repetitions')
    args = parser.parse_args()
    if not 1 <= args.count <= 100:
        parser.error('--count must be 1..100')
    source = args.source.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    record = {
        'tests': list(TESTS),
        'count': args.count,
        'platform': platform.platform(),
        'machine': platform.machine(),
        'sha': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=source, text=True).strip(),
        'runner': {key: os.environ.get(key) for key in ('ImageOS', 'ImageVersion', 'RUNNER_ARCH')},
        'results': {test: [] for test in TESTS},
        'suite_load': args.suite_load,
    }

    def save():
        (args.output / 'results.json').write_text(json.dumps(record, indent=2) + '\n')

    save()
    if args.binary:
        binary = args.binary.resolve(strict=True)
    else:
        with (args.output / 'build.jsonl').open('w') as output, (args.output / 'build.log').open('w') as errors:
            built = subprocess.run(
                ['cargo', 'test', '--locked', '--test', 'integration', '--no-run', '--message-format=json'],
                cwd=source, stdout=output, stderr=errors, check=False,
            )
        if built.returncode:
            record['build_exit'] = built.returncode
            save()
            return built.returncode

        binaries = [
            entry['executable']
            for line in (args.output / 'build.jsonl').read_text().splitlines()
            if (entry := json.loads(line)).get('reason') == 'compiler-artifact'
            and entry.get('executable')
            and entry['target']['name'] == 'integration'
            and entry.get('profile', {}).get('test')
        ]
        if len(binaries) != 1:
            raise ValueError(f'Expected one integration executable, found {binaries}')
        binary = binaries[0]

    isolated_binary = args.output / 'integration'
    if Path(binary) != isolated_binary:
        shutil.copy2(binary, isolated_binary)
    binary = str(isolated_binary)

    suite = None
    if args.suite_load:
        suite_log = (args.output / 'suite-load.log').open('w')
        suite = subprocess.Popen(
            [binary, f'--test-threads={args.test_threads}'], cwd=source,
            stdout=suite_log, stderr=subprocess.STDOUT,
        )
        record['suite_load_pid'] = suite.pid
        save()

    for test in TESTS:
        listed = subprocess.check_output([binary, test, '--exact', '--list'], cwd=source, text=True)
        if f'{test}: test' not in listed:
            raise ValueError(f'Exact test {test} was not registered; refusing a zero-test repeat')
        for index in range(args.count):
            log = f'{test.replace("::", "-")}-{index + 1:03}.log'
            with (args.output / log).open('w') as output:
                try:
                    result = subprocess.run(
                        [binary, test, '--exact', '--nocapture'], cwd=source,
                        stdout=output, stderr=subprocess.STDOUT, timeout=120, check=False,
                    )
                    code = result.returncode
                except subprocess.TimeoutExpired:
                    code = 124
            record['results'][test].append({'iteration': index + 1, 'exit_code': code, 'log': log})
            save()
            failures = sum(item['exit_code'] != 0 for item in record['results'][test])
            print(f'{record["sha"]} {test} {index + 1}/{args.count}: exit={code}; failures={failures}', flush=True)

    if suite is not None:
        record['suite_load_exit'] = suite.wait()
        suite_log.close()

    record['failures'] = {
        test: sum(item['exit_code'] != 0 for item in results)
        for test, results in record['results'].items()
    }
    save()
    return int(any(record['failures'].values()))


if __name__ == '__main__':
    sys.exit(main())
