#!/usr/bin/env python3
"""Measure one 10k synthetic full build and one changed-fact incremental refresh."""
import argparse
import json
from pathlib import Path
import platform
import sqlite3
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).parent / 'tests'))
import error_catalog as catalog
from error_catalog_fixture import add, connect


def measure():
    with tempfile.TemporaryDirectory(prefix='shellboy-errors-scale-') as temp:
        root = Path(temp)
        db = connect(root)
        try:
            for index in range(10_000):
                add(db, f'shell-{index:05}', 'cargo test broken' if index % 2 == 0 else 'cargo test fixed',
                    code=1 if index % 2 == 0 else 0, start=index*3,
                    thread=f'thread-{index//100}', turn=f'turn-{index//20}',
                    stderr=f'error: missing /tmp/build-{index}/file.rs line {index}')
            db.commit()
            full = catalog.build(root)
            assert full['executions'] == 10_000 and full['failures'] == 5000
            # In-place terminal correction, not a watermark-only append benchmark.
            db.execute("UPDATE shell_execution SET exit_code=1 WHERE id='shell-00001'")
            db.commit()
            incremental = catalog.build(root, incremental=True)
            assert incremental['executions'] == 10_000 and incremental['failures'] == 5001
            assert incremental['changed'] >= 2
            return {'fixture': 'synthetic temporary current-schema data only', 'executions': 10_000,
                    'python': platform.python_version(), 'sqlite': sqlite3.sqlite_version,
                    'full': full, 'incremental': incremental, 'timings_are_observations_only': True}
        finally:
            db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        print(json.dumps(measure(), indent=2, sort_keys=True))
    else:
        result = subprocess.run([sys.executable, str(Path(__file__).resolve()), '--worker'], timeout=60,
                                capture_output=True, text=True)
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
        return result.returncode
    return 0


if __name__ == '__main__':
    sys.exit(main())
