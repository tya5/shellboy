#!/usr/bin/env python3
"""Forecast textual branch conflicts without writing the source repository."""
import argparse
import itertools
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

WATCHDOG = 60


class ForecastError(Exception):
    pass


def environment(home):
    env = {k: os.environ[k] for k in ('PATH', 'SystemRoot', 'WINDIR', 'TEMP', 'TMP')
           if k in os.environ}
    env.update(HOME=str(home), USERPROFILE=str(home), LC_ALL='C',
               GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull,
               GIT_TERMINAL_PROMPT='0', GIT_OPTIONAL_LOCKS='0', GIT_NO_LAZY_FETCH='1')
    return env


def git(repo, env, *args, input=None, allowed=(0,)):
    try:
        result = subprocess.run(['git', '-C', str(repo), *args], env=env, input=input,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=WATCHDOG, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ForecastError(f'Git unavailable or watchdog expired: {error}') from error
    if result.returncode not in allowed:
        message = result.stderr.decode('utf-8', 'backslashreplace').strip()[:2000]
        raise ForecastError(f'git {args[0]} exited {result.returncode}: {message}')
    return result


def resolve(repo, env, ref):
    if not ref or ref.startswith('-') or '\0' in ref:
        raise ForecastError('Invalid ref argument')
    oid = git(repo, env, 'rev-parse', '--verify', '--end-of-options',
              ref + '^{commit}').stdout.decode().strip()
    if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', oid):
        raise ForecastError('Expected one commit object ID')
    return {'ref': ref, 'oid': oid}


def tree_files(repo, env, oid):
    raw = git(repo, env, 'ls-tree', '-r', '-z', oid).stdout
    files, attrs = [], []
    for record in raw.split(b'\0'):
        if not record:
            continue
        info, path = record.split(b'\t', 1)
        mode, kind, _ = info.split(b' ')
        if kind != b'blob':
            continue
        files.append(path)
        if path.split(b'/')[-1] == b'.gitattributes':
            if mode not in (b'100644', b'100755'):
                raise ForecastError('Linked attributes are unsupported')
            attrs.append(path)
    return files, attrs


def forecast(repo, refs, base_ref='origin/main', remote='origin', max_branches=64):
    repo = Path(repo).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='shellboy-conflict-forecast-') as root:
        root = Path(root)
        env = environment(root)
        if git(repo, env, 'rev-parse', '--is-shallow-repository').stdout.strip() != b'false':
            raise ForecastError('Shallow repositories are unsupported')
        partial = git(repo, env, 'config', '--local', '--no-includes', '--get-regexp',
                      r'^remote\..*\.promisor$|^extensions\.partialclone$', allowed=(0, 1))
        if partial.returncode == 0:
            raise ForecastError('Partial/promisor repositories are unsupported; no fetching is allowed')
        drivers = git(repo, env, 'config', '--local', '--no-includes', '--get-regexp',
                      r'^merge\..*\.driver$|^merge\.default$', allowed=(0, 1))
        if drivers.returncode == 0:
            raise ForecastError('Local custom/default merge-driver configuration is unsupported')
        base = resolve(repo, env, base_ref)
        if refs:
            branches = [resolve(repo, env, ref) for ref in sorted(set(refs)) if ref != base_ref]
        else:
            git(repo, env, 'check-ref-format', f'refs/remotes/{remote}/probe')
            listing = git(repo, env, 'for-each-ref', '--format=%(refname)%00%(objectname)%00%(symref)',
                          f'refs/remotes/{remote}/').stdout
            branches = []
            for line in listing.splitlines():
                ref, oid, symref = (part.decode('utf-8', 'surrogateescape') for part in line.split(b'\0'))
                if symref or ref == f'refs/remotes/{remote}/main' or oid == base['oid']:
                    continue
                merged = git(repo, env, 'merge-base', '--is-ancestor', oid, base['oid'], allowed=(0, 1))
                if merged.returncode == 1:
                    branches.append({'ref': ref, 'oid': oid})
        if len(branches) > max_branches:
            raise ForecastError(f'Too many branches ({len(branches)} > {max_branches}); select explicit refs')
        fmt = git(repo, env, 'rev-parse', '--show-object-format').stdout.decode().strip()
        if fmt not in ('sha1', 'sha256'):
            raise ForecastError('Unsupported object format')
        objects = git(repo, env, 'rev-parse', '--git-path', 'objects').stdout.rstrip(b'\n')
        object_path = Path(os.fsdecode(objects))
        if not object_path.is_absolute():
            object_path = repo / object_path
        scratch = root / 'merge'
        scratch.mkdir()
        git(scratch, env, 'init', '-q', '--object-format=' + fmt)
        env['GIT_ALTERNATE_OBJECT_DIRECTORIES'] = json.dumps(str(object_path.resolve()), ensure_ascii=False)
        # All merge output/index/config files belong to scratch; originals are alternates only.
        pairs = [(base, branch) for branch in branches] + list(itertools.combinations(branches, 2))
        files = {item['oid']: tree_files(scratch, env, item['oid']) for item in [base, *branches]}
        checked, materialized, previous = [], [], None

        def load_attributes(oid):
            nonlocal materialized
            for name in materialized:
                path = scratch / os.fsdecode(name)
                path.unlink()
                parent = path.parent
                while parent != scratch:
                    try:
                        parent.rmdir()
                    except OSError:
                        break
                    parent = parent.parent
            git(scratch, env, 'read-tree', oid)
            materialized = files[oid][1]
            if materialized:
                git(scratch, env, 'checkout-index', '-z', '--stdin', input=b'\0'.join(materialized) + b'\0')

        # Validate all sides, including a right-only attribute change on a fast-forward.
        names = sorted(set(path for paths, _ in files.values() for path in paths))
        for oid in files:
            load_attributes(oid)
            if names:
                attrs = git(scratch, env, 'check-attr', '--cached', '-z', '--stdin', 'merge',
                            input=b'\0'.join(names) + b'\0').stdout.split(b'\0')
                for value in attrs[2::3]:
                    if value not in (b'text', b'binary', b'union', b'set', b'unset', b'unspecified'):
                        raise ForecastError('Custom merge driver attributes unsupported: ' + repr(value))
        for left, right in pairs:
            if previous != left['oid']:
                load_attributes(left['oid'])
                previous = left['oid']
            merged = git(scratch, env, '-c', 'merge.renormalize=false', 'merge-tree', '--write-tree',
                         '--name-only', '-z', '--no-messages', left['oid'], right['oid'], allowed=(0, 1))
            parts = merged.stdout.split(b'\0')
            if not re.fullmatch(b'[0-9a-f]{40}|[0-9a-f]{64}', parts[0]):
                raise ForecastError('Unsupported merge-tree output (Git2.38+ required)')
            paths = sorted(set(os.fsdecode(path) for path in parts[1:] if path))
            if bool(paths) != (merged.returncode == 1):
                raise ForecastError('Incomplete conflict report from merge-tree')
            checked.append({'left': left, 'right': right, 'conflicts': paths})
        return {'ok': True, 'base': base, 'branches': branches, 'pairs': checked,
                'has_conflicts': any(pair['conflicts'] for pair in checked),
                'attributes': 'first-side committed attributes; builtin merge settings only'}


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        'Git2.38+. All output objects/index/attributes are temporary. No fetch or source '
        'worktree/index/ref/object writes. Default origin unmerged branches; explicit refs '
        'override discovery. Pairwise textual prediction only, not semantic safety or merge '
        'order. First-side committed attributes and builtin settings; custom drivers fail. '
        'Exit0 clean,1 conflicts,2 invalid input/Git failure. 60s Git watchdog.'))
    parser.add_argument('refs', nargs='*')
    parser.add_argument('--repo', type=Path, default=Path.cwd())
    parser.add_argument('--base', default='origin/main')
    parser.add_argument('--remote', default='origin')
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--max-branches', type=int, default=64)
    args = parser.parse_args()
    try:
        if not 1 <= args.max_branches <= 256:
            raise ForecastError('max-branches must be1..256')
        result = forecast(args.repo, args.refs, args.base, args.remote, args.max_branches)
    except (ForecastError, OSError, ValueError) as error:
        print(json.dumps({'ok': False, 'error': str(error)}, ensure_ascii=True))
        return 2
    if args.json:
        print(json.dumps(result, ensure_ascii=True))
    elif not result['has_conflicts']:
        print(f'No textual conflicts ({len(result["pairs"])} pairs checked).')
    else:
        for pair in result['pairs']:
            if pair['conflicts']:
                print(json.dumps({'left': pair['left']['ref'], 'right': pair['right']['ref'],
                                  'conflicts': pair['conflicts']}, ensure_ascii=True))
    return int(result['has_conflicts'])


if __name__ == '__main__':
    raise SystemExit(main())
