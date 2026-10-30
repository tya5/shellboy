#!/usr/bin/env python3
"""Stable Cargo/libtest timing and coverage evidence; no retry or test selection loss."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


def run(command, log):
    started = time.monotonic()
    with log.open('w', encoding='utf-8') as output:
        result = subprocess.run(command, stdout=output, stderr=subprocess.STDOUT, check=False)
    return {'command': command, 'seconds': time.monotonic() - started,
            'exit_code': result.returncode, 'log': log.name}


def identities(target, names):
    """Only the integration module prefix is structural; preserve all other names."""
    result = []
    for name in names:
        if target == 'test:integration':
            module, separator, rest = name.partition('::')
            if not separator:
                raise ValueError(f'unregistered aggregate test: {name}')
            result.append(f'test:{module}::{rest}')
        else:
            result.append(f'{target}::{name}')
    return result


def listing(binary, ignored=False):
    command = [binary, '--list', '--format=terse']
    if ignored:
        command.append('--ignored')
    result = subprocess.run(command, check=True, text=True, encoding='utf-8', capture_output=True)
    return sorted(line[:-6] for line in result.stdout.splitlines() if line.endswith(': test'))


def main():
    # CI consoles may default to a code page unable to represent failure logs.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, 'reconfigure'):
            stream.reconfigure(encoding='utf-8', errors='replace')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--artifacts', type=Path, help='existing Cargo JSON from an all-target no-run build')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--inventory-only', action='store_true')
    mode.add_argument('--profile-only', action='store_true', help='inventory plus isolated profiles, without full suite execution')
    parser.add_argument('--compare', type=Path, help='same-OS baseline results.json; require identical tests/ignored sets')
    parser.add_argument('--per-test', default='', help='comma-separated suite names for extra isolated-process profiles')
    args = parser.parse_args()
    if args.inventory_only and args.per_test:
        parser.error('--inventory-only cannot execute --per-test; use --profile-only')
    if not Path('Cargo.toml').is_file():
        parser.error('run from the repository root')
    args.output.mkdir(parents=True, exist_ok=True)
    record = {'format_version': 1, 'platform': sys.platform,
              'test_threads': os.environ.get('RUST_TEST_THREADS'),
              'cpu_count': os.cpu_count(), 'python': sys.version,
              'runner_image': {key: os.environ.get(key) for key in ['ImageOS', 'ImageVersion']},
              'harnesses': [], 'isolated_tests': [], 'failures': []}
    result_path = args.output / 'results.json'

    def save():
        result_path.write_text(json.dumps(record, indent=2) + '\n', encoding='utf-8')

    try:
        record['source_sha'] = subprocess.run(
            ['git', 'rev-parse', 'HEAD'], check=True, text=True, encoding='utf-8', capture_output=True
        ).stdout.strip()
        artifacts = args.artifacts or args.output / 'cargo-artifacts.jsonl'
        if args.artifacts is None:
            command = ['cargo', 'test', '--locked', '--all-targets', '--no-run', '--message-format=json']
            started = time.monotonic()
            with artifacts.open('w', encoding='utf-8') as output, (args.output / 'build.log').open('w', encoding='utf-8') as errors:
                code = subprocess.run(command, stdout=output, stderr=errors, check=False).returncode
            record['build'] = {'command': command, 'seconds': time.monotonic() - started, 'exit_code': code}
            if code:
                record['failures'].append({'stage': 'build', 'exit_code': code})
                return code
        binaries = {}
        for line in artifacts.read_text(encoding='utf-8').splitlines():
            message = json.loads(line)
            if (message.get('reason') != 'compiler-artifact' or not message.get('executable')
                    or not message.get('profile', {}).get('test')):
                continue
            target = message['target']
            key = f"{target['kind'][0]}:{target['name']}"
            binaries[key] = message['executable']
        if not binaries:
            raise ValueError('all-target Cargo output contained no test executables')
        all_tests, all_ignored = [], []
        requested_suites = set(filter(None, args.per_test.split(',')))
        discovered_suites = set()
        for number, (target, binary) in enumerate(sorted(binaries.items())):
            names, ignored = listing(binary), listing(binary, ignored=True)
            item = {'target': target, 'tests': len(names), 'ignored': len(ignored)}
            all_tests.extend(identities(target, names))
            all_ignored.extend(identities(target, ignored))
            if not (args.inventory_only or args.profile_only):
                print(f'Running {target} ({len(names)} tests, {len(ignored)} ignored)', flush=True)
                item.update(run([binary], args.output / f'harness-{number:03d}.log'))
                # A successful process running zero tests must not replace a suite.
                text = (args.output / item['log']).read_text(encoding='utf-8', errors='replace')
                summary = re.search(r'test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;', text)
                if summary:
                    item['passed'], item['failed'] = int(summary[2]), int(summary[3])
                    reported_ignored = int(summary[4])
                    if item['passed'] + item['failed'] + reported_ignored != len(names):
                        record['failures'].append({'target': target, 'reason': 'test count mismatch'})
                else:
                    record['failures'].append({'target': target, 'reason': 'missing harness summary'})
                if item['exit_code']:
                    failed = sorted(set(re.findall(r'^    (\S+)$', text, re.M)) & set(names))
                    record['failures'].append({'target': target, 'exit_code': item['exit_code'], 'tests': failed})
            record['harnesses'].append(item)
            # Persist the original result before best-effort console diagnostics.
            save()
            if not (args.inventory_only or args.profile_only):
                if item['exit_code']:
                    for name in failed:
                        print('Failed test: ' + json.dumps(name, ensure_ascii=True), flush=True)
                    print(text[-16000:], flush=True)
                print(f"  {item['seconds']:.3f}s, exit {item['exit_code']}", flush=True)
            for test in names:
                suite = test.partition('::')[0] if target == 'test:integration' else target.partition(':')[2]
                discovered_suites.add(suite)
                if suite not in requested_suites or test in ignored:
                    continue
                sample = {'target': target, 'test': test, 'mode': 'isolated process; not concurrent duration'}
                sample.update(run([binary, '--exact', test, '--test-threads=1'],
                                  args.output / f'isolated-{len(record["isolated_tests"]):03d}.log'))
                record['isolated_tests'].append(sample)
                sample_text = (args.output / sample['log']).read_text(encoding='utf-8', errors='replace')
                if not re.search(r'test result: ok\. 1 passed; 0 failed; 0 ignored;', sample_text):
                    record['failures'].append({'target': target, 'test': test, 'reason': 'isolated test did not pass exactly once'})
                if sample['exit_code']:
                    record['failures'].append({'target': target, 'test': test, 'exit_code': sample['exit_code']})
                print(f"  isolated {test}: {sample['seconds']:.3f}s, exit {sample['exit_code']}", flush=True)
                save()
        unknown_suites = sorted(requested_suites - discovered_suites)
        if unknown_suites:
            record['failures'].append({'stage': 'profile selection', 'unknown_suites': unknown_suites})
        record['tests'] = sorted(all_tests)
        record['ignored'] = sorted(all_ignored)
        if len(set(all_tests)) != len(all_tests):
            raise ValueError('duplicate normalized test identity')
        if args.compare:
            baseline = json.loads(args.compare.read_text(encoding='utf-8'))
            if baseline['platform'] != record['platform']:
                raise ValueError('inventory comparisons require the same OS')
            for key in ['tests', 'ignored']:
                missing = sorted(set(baseline[key]) - set(record[key]))
                added = sorted(set(record[key]) - set(baseline[key]))
                record[f'{key}_comparison'] = {'missing': missing, 'added': added}
                if missing or added:
                    record['failures'].append({'stage': 'inventory', 'set': key, 'missing': missing, 'added': added})
        return 1 if record['failures'] else 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        record['failures'].append({'stage': 'runner', 'error': str(error)})
        print(error, file=sys.stderr)
        return 1
    finally:
        save()
        print(f'Timing record: {result_path}', flush=True)


if __name__ == '__main__':
    sys.exit(main())
