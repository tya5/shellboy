#!/usr/bin/env python3
"""Predict textual branch conflicts; not semantic compatibility or merge order."""
import argparse
import itertools
import json
from pathlib import Path
import subprocess
import sys


def forecast(repo, refs, base):
    def git(*words, allowed=(0,)):
        result = subprocess.run(['git', '-C', str(repo), *words], capture_output=True, text=True)
        if result.returncode not in allowed:
            raise RuntimeError(result.stderr.strip() or f'git exited {result.returncode}')
        return result

    if not refs:
        listing = git('for-each-ref', '--format=%(refname) %(symref)', 'refs/remotes/origin/').stdout
        refs = []
        for line in listing.splitlines():
            ref, *symref = line.split()
            if symref or ref == 'refs/remotes/origin/main':
                continue
            merged = git('merge-base', '--is-ancestor', ref, base, allowed=(0, 1))
            if merged.returncode == 1:
                refs.append(ref)
    refs = sorted(set(refs) - {base})
    commits = {ref: git('rev-parse', '--verify', '--end-of-options', ref + '^{commit}').stdout.strip()
               for ref in [base, *refs]}
    pairs = [(base, ref) for ref in refs] + list(itertools.combinations(refs, 2))
    checked = []
    for left, right in pairs:
        merge = git('--attr-source=' + commits[left], 'merge-tree', '--write-tree',
                    '--name-only', '-z', '--no-messages', commits[left], commits[right], allowed=(0, 1))
        paths = [path for path in merge.stdout.split('\0')[1:] if path]
        checked.append({'left': left, 'right': right, 'conflicts': paths, 'conflicted': merge.returncode == 1})
    return {'ok': True, 'commits': commits, 'pairs': checked,
            'has_conflicts': any(pair['conflicted'] for pair in checked)}


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        'Requires Git2.40+. Uses first-side committed attributes (including merge=union). '
        'Worktree/index/refs stay unchanged; merge-tree may write unreachable loose objects '
        'that normal Git gc can remove. Normal Git configuration/drivers apply. '
        'Exit0 clean,1 conflicts,2 Git/input error.'))
    parser.add_argument('refs', nargs='*', help='Explicit refs; default: unmerged origin branches')
    parser.add_argument('--repo', type=Path, default=Path.cwd())
    parser.add_argument('--base', default='origin/main')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args()
    try:
        result = forecast(args.repo, args.refs, args.base)
    except (OSError, RuntimeError, ValueError) as error:
        if args.json:
            print(json.dumps({'ok': False, 'error': str(error)}))
        else:
            print(str(error), file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(result))
    elif not result['has_conflicts']:
        print(f'No textual conflicts ({len(result["pairs"])} pairs checked).')
    else:
        for pair in result['pairs']:
            if pair['conflicted']:
                print(f'{pair["left"]} x {pair["right"]}: {", ".join(pair["conflicts"])}')
    return int(result['has_conflicts'])


if __name__ == '__main__':
    raise SystemExit(main())
