#!/usr/bin/env python3
"""Report record-file line deltas as one JSON line; fail if Git reports deletions."""
import argparse
import json
import os
from pathlib import Path
import subprocess

RECORDS = (
    'docs/development/status.md',
    'docs/validation/acceptance.md',
    'docs/development/decisions.md',
)


def git(repo, *args):
    env = dict(os.environ)
    for key in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE',
                'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES'):
        env.pop(key, None)
    return subprocess.run(['git', '-C', str(repo), *args], check=True, env=env,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def line_count(data):
    return data.count(b'\n') + int(bool(data) and not data.endswith(b'\n'))


def report(repo, base):
    root = Path(git(repo, 'rev-parse', '--show-toplevel').decode().rstrip('\r\n'))
    commit = git(root, 'rev-parse', '--verify', '--end-of-options', base + '^{commit}').decode().strip()
    changes = git(root, 'diff', '--no-ext-diff', '--no-textconv', '--no-renames',
                  '--numstat', commit, '--', *RECORDS).decode()
    stats = {}
    for row in changes.splitlines():
        added, removed, path = row.split('\t', 2)
        if added == '-' or removed == '-':
            raise ValueError('Record is binary: ' + path)
        stats[path] = (int(added), int(removed))
    files = {}
    for path in RECORDS:
        before = git(root, 'show', commit + ':' + path)
        after = (root / path).read_bytes()
        added, removed = stats.get(path, (0, 0))
        old, new = line_count(before), line_count(after)
        files[path] = dict(base_lines=old, current_lines=new, delta=new - old,
                           added=added, deleted=removed, only_additions=removed == 0)
    return dict(base=base, base_commit=commit, files=files,
                ok=all(value['only_additions'] for value in files.values()))


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        'Read-only; includes staged and unstaged working-tree changes. The base '
        'defaults to the local origin/main ref; this tool never fetches. '
        'only_additions means no Git-deleted lines, not necessarily insertion at EOF. '
        'Line-ending changes follow Git diff rules. Exit 0: retained lines; '
        '1: deleted/replaced lines; 2: invalid base, missing file or Git/I/O error.'))
    parser.add_argument('--base', default='origin/main', help='Commit/ref to compare (default: origin/main)')
    parser.add_argument('--repo', type=Path, default=Path('.'), help='Repository/worktree directory (default: current directory)')
    args = parser.parse_args()
    try:
        result = report(args.repo, args.base)
    except (OSError, ValueError, UnicodeError, subprocess.CalledProcessError) as error:
        detail = error.stderr.decode(errors='replace').strip() if isinstance(error, subprocess.CalledProcessError) else str(error)
        print(json.dumps(dict(ok=False, error=detail), separators=(',', ':')))
        return 2
    print(json.dumps(result, separators=(',', ':')))
    return 0 if result['ok'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
