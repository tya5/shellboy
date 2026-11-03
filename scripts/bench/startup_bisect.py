#!/usr/bin/env python3
"""Bisect cold PTY startup on one runner using the footprint measurement.

Builds only disposable Git worktrees; no real model, API key, or user data.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile

import footprint


def git(*args, cwd=None):
    return subprocess.check_output(['git', *args], cwd=cwd, text=True).strip()


def main():
    if len(sys.argv) != 4:
        raise SystemExit('usage: startup_bisect.py GOOD BAD OUTPUT.json')
    good, bad, output = sys.argv[1:]
    good = git('rev-parse', '--verify', good + '^{commit}')
    bad = git('rev-parse', '--verify', bad + '^{commit}')
    if subprocess.run(['git', 'merge-base', '--is-ancestor', good, bad]).returncode:
        raise SystemExit('good must be an ancestor of bad')
    commits = [good] + git('rev-list', '--first-parent', '--reverse', f'{good}..{bad}').splitlines()
    if len(commits) < 2 or commits[-1] != bad:
        raise SystemExit('invalid first-parent interval')
    output = Path(output)
    if output.exists():
        raise SystemExit('output must not already exist')
    output.parent.mkdir(parents=True, exist_ok=True)
    cpu = next((x.split(':', 1)[1].strip() for x in Path('/proc/cpuinfo').read_text().splitlines()
                if x.startswith('model name')), 'unknown')
    result = {'good': good, 'bad': bad, 'cpu': cpu, 'platform': platform.platform(),
              'ci_run': os.environ.get('GITHUB_RUN_ID'), 'samples_per_commit': 5,
              'measurement': 'footprint.measure_startup: fresh data, completed frame',
              'samples': [], 'status': 'running'}

    def save():
        output.write_text(json.dumps(result, indent=2) + '\n')

    with tempfile.TemporaryDirectory(prefix='shellboy-192-') as temp:
        source = Path(temp) / 'source'
        git('worktree', 'add', '--detach', str(source), good)
        try:
            target = Path(temp) / 'target'
            env = dict(os.environ, CARGO_TARGET_DIR=str(target))
            cache = {}

            def measure(index):
                if index in cache:
                    return cache[index]
                sha = commits[index]
                subprocess.run(['git', 'switch', '--detach', sha], cwd=source, check=True)
                subprocess.run(['cargo', 'build', '--locked', '--release', '--bin', 'shellboy'],
                               cwd=source, env=env, check=True)
                binary = target / 'release' / 'shellboy'
                measurements = [footprint.measure_startup(binary) for _ in range(5)]
                times = [x['first_screen_ms'] for x in measurements]
                entry = {'commit': sha, 'index': index,
                         'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                         'measurements': measurements, 'median_ms': statistics.median(times)}
                result['samples'].append(entry)
                cache[index] = entry
                save()
                print(json.dumps(entry), flush=True)
                return entry

            lo, hi = 0, len(commits) - 1
            low, high = measure(lo), measure(hi)
            low_values = [x['first_screen_ms'] for x in low['measurements']]
            high_values = [x['first_screen_ms'] for x in high['measurements']]
            if min(high_values) - max(low_values) < 100:
                raise RuntimeError('endpoints are not separated by at least 100 ms; cannot bisect')
            threshold = (max(low_values) + min(high_values)) / 2
            result['threshold_ms'] = threshold
            while hi - lo > 1:
                mid = (lo + hi) // 2
                point = measure(mid)
                times = [x['first_screen_ms'] for x in point['measurements']]
                if min(times) < threshold < max(times):
                    raise RuntimeError(f'noisy classification at {point["commit"]}: {times}')
                if point['median_ms'] >= threshold:
                    hi = mid
                else:
                    lo = mid
            result.update(status='complete', last_good=commits[lo], first_bad=commits[hi],
                          caveat='first-parent monotonic classification; test any earlier independent regressions separately')
            save()
            print(json.dumps(result, indent=2), flush=True)
        except Exception as error:
            result.update(status='error', error=f'{type(error).__name__}: {error}')
            save()
            raise
        finally:
            subprocess.run(['git', 'worktree', 'remove', '--force', str(source)], check=True)


if __name__ == '__main__':
    main()
