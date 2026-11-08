#!/usr/bin/env python3
"""Compare two Run-owner removal methods on identical disposable synthetic DBs."""
import argparse
import json
import platform
import shutil
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True, help='Git revision with owner-bearing Run schema')
    parser.add_argument('--runs', type=int, default=1000000)
    parser.add_argument('--padding-mib', type=int, default=512)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    schema = subprocess.check_output(['git', 'show', f'{args.base}:src/storage/current.sql'], text=True)
    prefix = '''DROP TRIGGER IF EXISTS reject_owner_cycle_insert;
DROP TRIGGER IF EXISTS reject_owner_cycle_update;
DROP INDEX IF EXISTS runs_by_owner;'''
    rebuild = Path('migrations/0038_independent_runs.sql').read_text().split('DROP INDEX wait_recovery_thread;')[0]
    methods = {'drop_column': prefix + '\nALTER TABLE run DROP COLUMN owner_run_id;',
               'rebuild': rebuild}
    report = {'environment': {'os': platform.platform(), 'machine': platform.machine(),
                             'python': platform.python_version(), 'sqlite': sqlite3.sqlite_version},
              'base': args.base, 'actual_user_scale': 'unknown; synthetic row counts are not a user-scale estimate',
              'runs': args.runs, 'padding_mib': args.padding_mib, 'results': []}
    with tempfile.TemporaryDirectory(prefix='shellboy-run-migration-') as root:
        root = Path(root)
        seed = root / 'seed.sqlite'
        db = sqlite3.connect(seed)
        db.executescript(schema)
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('synthetic','/','','p','m','0')")
        db.executemany("INSERT INTO run(id,thread_id,owner_run_id,state,created_at,ended_at) VALUES(?,'synthetic',?,'completed','0','1')",
                       ((f'run-{n:09}', None if n == 0 else 'run-000000000') for n in range(args.runs)))
        db.execute('CREATE TABLE synthetic_unrelated_payload(id INTEGER PRIMARY KEY,payload BLOB NOT NULL)')
        db.executemany('INSERT INTO synthetic_unrelated_payload(payload) VALUES(zeroblob(1048576))',
                       (() for _ in range(args.padding_mib)))
        db.commit()
        db.close()
        report['seed_bytes'] = seed.stat().st_size
        for name, sql in methods.items():
            path = root / f'{name}.sqlite'
            shutil.copyfile(seed, path)
            db = sqlite3.connect(path)
            db.execute('PRAGMA foreign_keys=OFF')
            before = db.total_changes
            started = time.perf_counter()
            try:
                db.executescript('BEGIN IMMEDIATE;\n' + sql + '\nCOMMIT;')
                elapsed = time.perf_counter() - started
                result = {'method': name, 'seconds': elapsed, 'changed_rows': db.total_changes - before,
                          'db_bytes_after': path.stat().st_size,
                          'run_count_after': db.execute('SELECT count(*) FROM run').fetchone()[0],
                          'foreign_key_violations': len(db.execute('PRAGMA foreign_key_check').fetchall())}
            except sqlite3.Error as error:
                result = {'method': name, 'seconds': time.perf_counter() - started, 'error': str(error)}
                db.rollback()
            report['results'].append(result)
            db.close()
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
