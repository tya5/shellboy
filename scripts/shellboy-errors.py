#!/usr/bin/env python3
"""Explicit external shell failure catalogue. No LLM, owner writes or automatic hooks."""
import argparse
import json
import sqlite3
import sys

import error_catalog


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--data-dir', required=True, help='Explicit directory containing shellboy.db')
    sub = parser.add_subparsers(dest='operation', required=True)
    build = sub.add_parser('build', help='Explicitly build/rebuild derived/errors.sqlite3')
    build.add_argument('--incremental', action='store_true', help='Bounded refresh; unchanged snapshots do no writes')
    build.add_argument('--steps', type=int, default=5)
    build.add_argument('--repo-root', help='Classify cwd relative to this root; no filesystem walk')
    for name in ('top', 'similar'):
        command = sub.add_parser(name)
        command.add_argument('--limit', type=int, default=10, choices=range(1, 101), metavar='1..100')
        if name == 'top':
            command.add_argument('--since')
        else:
            command.add_argument('--execution', required=True)
    args = parser.parse_args()
    try:
        if args.operation == 'build':
            result = error_catalog.build(args.data_dir, args.incremental, args.steps, args.repo_root)
        elif args.operation == 'top':
            result = error_catalog.top(args.data_dir, args.since, args.limit)
        else:
            result = error_catalog.similar(args.data_dir, args.execution, args.limit)
        print(json.dumps({'ok': True, 'result': result}, ensure_ascii=True, sort_keys=True))
        return 0
    except (ValueError, OSError, sqlite3.Error, KeyError, TypeError) as error:
        # SQLite errors can include untrusted source text or credential-bearing paths.
        print(json.dumps({'ok': False, 'error': {'code': 'catalog_unavailable',
              'message': 'Catalogue unavailable or incompatible; inspect inputs and explicitly rebuild.',
              'kind': type(error).__name__}}))
        return 1


if __name__ == '__main__':
    sys.exit(main())
