"""External catalogue contracts using synthetic temporary conversation databases."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import error_catalog as catalog
from error_catalog_fixture import ROOT, add, connect, time_at


class CatalogueTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='error-catalog-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.db = connect(self.root)
        self.addCleanup(self.db.close)

    def build(self, **kwargs):
        self.db.commit()
        return catalog.build(self.root, **kwargs)

    def rows(self):
        return catalog.read_catalog(self.root / 'derived/errors.sqlite3')[1]

    def cli(self, *args, expected=0):
        result = subprocess.run([sys.executable, str(ROOT / 'scripts/shellboy-errors.py'), '--data-dir', str(self.root), *args], capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return json.loads(result.stdout)

    def test_group_counts_exact_nodes_turn_pairing_and_since(self):
        add(self.db, 'fail-one')
        add(self.db, 'success-one', 'cargo test fixed', code=0, start=3)
        add(self.db, 'fail-two', thread='other', turn='other-turn', start=20,
            stderr='error: missing /tmp/build-999/other.rs line 77')
        add(self.db, 'success-two', 'cargo test fixed', code=0, thread='other', turn='other-turn', start=23)
        self.build()
        top = self.cli('top')['result']['items']
        self.assertEqual(len(top), 1)
        item = top[0]
        self.assertEqual((item['count'], item['threads'], item['paired'], item['mean_steps']), (2, 2, 2, 1))
        self.assertEqual(item['paired_fraction'], 1)
        self.assertEqual(item['step_counts'], {'1': 2})
        self.assertEqual(item['first_seen'], time_at(0))
        similar = self.cli('similar', '--execution', 'fail-one')['result']['items'][0]
        refs = similar['examples'][0]
        self.assertEqual(refs['node_id'], 'node-fail-one')
        self.assertEqual(refs['recovery']['id'], 'success-one')
        self.assertEqual(refs['recovery']['node_id'], 'node-success-one')
        self.assertEqual(refs['group_id'], 'group-turn')
        self.assertEqual(refs['call_id'], 'call-0')
        self.assertEqual(self.cli('top', '--since', time_at(10))['result']['items'][0]['count'], 1)
        self.assertFalse(self.cli('similar', '--execution', 'missing', expected=1)['ok'])

    def test_no_cross_turn_thread_overlap_unknown_or_step_overreach(self):
        add(self.db, 'f', end=10)
        add(self.db, 'overlap', code=0, start=2)
        add(self.db, 'other-turn', code=0, start=12, turn='second')
        add(self.db, 'other-thread', code=0, start=12, turn='third', thread='other')
        add(self.db, 'live', state='running', code=None, start=11)
        add(self.db, 'ok', code=0, start=15)
        self.build(steps=2)
        self.assertIsNone(next(r for r in self.rows() if r['id'] == 'f')['recovery'])
        self.build(steps=3)
        recovery = next(r for r in self.rows() if r['id'] == 'f')['recovery']
        self.assertEqual((recovery['id'], recovery['steps']), ('ok', 3))
        catalog.read_rows(self.db, "UPDATE shell_execution SET ended_at=NULL WHERE id='f'")
        self.build(steps=3)
        self.assertIsNone(next(r for r in self.rows() if r['id'] == 'f')['recovery'])

    def test_secret_canaries_never_reach_derived_or_output(self):
        values = ['CANARY_ENV', 'CANARY_FLAG', 'CANARY_URLUSER', 'CANARY_URLPASS', 'CANARY_QUERY',
                  'CANARY_BEARER', 'CANARY_HOME', 'sk-CANARYTOKEN123', 'CANARY_KEY_BODY']
        command = "SECRET=CANARY_ENV cargo test --token CANARY_FLAG https://CANARY_URLUSER:CANARY_URLPASS@host.invalid/?q=CANARY_QUERY /Users/CANARY_HOME/repo"
        error = 'error: Bearer CANARY_BEARER sk-CANARYTOKEN123\n-----BEGIN PRIVATE KEY-----\nCANARY_KEY_BODY\n-----END PRIVATE KEY-----'
        add(self.db, 'f', command, stderr=error)
        add(self.db, 's', command.replace('test', 'check'), code=0, start=3)
        self.build()
        encoded = (self.root / 'derived/errors.sqlite3').read_bytes() + json.dumps(self.cli('top')).encode() + json.dumps(self.cli('similar', '--execution', 'f')).encode()
        for canary in values + ['RAW_STDOUT_MUST_NOT_BE_COPIED']:
            self.assertNotIn(canary.encode(), encoded)
        self.assertNotIn(b'-----BEGIN', encoded)
        self.assertEqual(catalog.opaque_id('8305a4ce-49d4-49c0-a154-e169cc7fcadf'), '8305a4ce-49d4-49c0-a154-e169cc7fcadf')

    def test_read_only_database_rebuild_and_incremental_noop(self):
        add(self.db, 'f')
        self.db.commit()
        source = self.root / 'shellboy.db'
        digest = hashlib.sha256(source.read_bytes()).digest()
        with contextlib.closing(catalog.connect_readonly(source)) as db:
            with self.assertRaises(sqlite3.OperationalError):
                catalog.read_rows(db, "UPDATE shell_execution SET state='failed'")
        self.build()
        saved = self.rows()
        target = self.root / 'derived/errors.sqlite3'
        before = target.stat().st_mtime_ns
        result = self.build(incremental=True)
        self.assertEqual(result['unchanged'], 1)
        self.assertEqual(target.stat().st_mtime_ns, before)
        target.unlink()
        self.build()
        self.assertEqual(self.rows(), saved)
        self.assertEqual(hashlib.sha256(source.read_bytes()).digest(), digest)

    def test_refresh_in_place_terminal_transition_gc_and_deleted_thread(self):
        add(self.db, 'f')
        add(self.db, 'running', state='running', code=None, start=3)
        self.build()
        self.assertIsNone(self.rows()[0]['recovery'])
        catalog.read_rows(self.db, "UPDATE shell_execution SET state='exited',exit_code=0 WHERE id='running'")
        self.build(incremental=True)
        self.assertEqual(self.rows()[0]['recovery']['id'], 'running')
        catalog.read_rows(self.db, "UPDATE node SET deleted_reason='expired' WHERE id='node-running'")
        self.build(incremental=True)
        self.assertIsNone(self.rows()[0]['recovery']['node_id'])
        catalog.read_rows(self.db, "UPDATE thread SET deleted_at=?", (time_at(30),))
        result = self.build(incremental=True)
        self.assertEqual(result['executions'], 0)
        self.assertEqual(self.cli('top')['result']['items'], [])

    def test_limits_oversized_input_schema_options_and_atomic_failure(self):
        add(self.db, 'f', command='x' * (catalog.MAX_COMMAND + 1), stderr='error: token ' + 'SECRETBOUNDARY' * 400)
        self.build()
        row = self.rows()[0]
        self.assertEqual(row['command'], '<command-unavailable>')
        self.assertEqual(row['error'], '<stderr-unavailable>')
        target = self.root / 'derived/errors.sqlite3'
        before = target.read_bytes()
        with patch.object(catalog, 'MAX_EXECUTIONS', 0), self.assertRaises(ValueError):
            self.build()
        self.assertEqual(target.read_bytes(), before)
        with patch.object(catalog.os, 'replace', side_effect=OSError('synthetic fault')), self.assertRaises(OSError):
            self.build()
        self.assertEqual(target.read_bytes(), before)
        self.assertEqual(list(target.parent.glob('.errors-*')), [])
        with self.assertRaises(ValueError):
            self.build(incremental=True, steps=6)
        catalog.read_rows(self.db, 'PRAGMA user_version=1')
        self.assertFalse(self.cli('build', expected=1)['ok'])

    def test_sql_output_seek_categories_control_safety_and_bounded_ids(self):
        add(self.db, 'f')
        self.db.commit()
        plan = catalog.read_rows(self.db, "EXPLAIN QUERY PLAN SELECT substr(bytes,1,4096) FROM output_chunk WHERE execution_id=? AND stream='stderr' ORDER BY stream_offset LIMIT 9", ('f',))
        self.assertTrue(any('SEARCH' in str(r) and 'output_stream' in str(r) for r in plan), plan)
        cases = [('zsh: command not found', 127, 'missing-command-path'), ('Permission denied', 1, 'permission'),
                 ('Connection refused', 1, 'network'), ('invalid argument', 2, 'syntax-arguments'), ('timed out', 1, 'timeout')]
        for error, code, expected in cases:
            self.assertEqual(catalog.category(error, 'failed', None, code), expected)
        self.assertNotIn('\x1b', catalog.shape('\x1b[31merror: 99\x1b[0m'))
        for text in ['../file', 'id\nsecret', 'x' * 129]:
            with self.assertRaises(ValueError):
                catalog.opaque_id(text)
        self.assertEqual(catalog.shape('error 2026-10-31T12:00:00Z at /tmp/a 0xaffe 99'),
                         catalog.shape('error 2025-09-01T01:02:03Z at /tmp/b 0xaaaa 33'))

    def test_recovery_example_not_hidden_by_three_unpaired_occurrences(self):
        for index in range(4):
            add(self.db, f'f-{index}', turn=f'turn-{index}', start=index*10)
        add(self.db, 'fixed', 'cargo test fixed', turn='turn-3', code=0, start=35)
        self.build()
        item = self.cli('similar', '--execution', 'f-0')['result']['items'][0]
        self.assertEqual(item['statistics']['count'], 4)
        self.assertEqual(item['examples'][0]['recovery']['id'], 'fixed')

    def test_missing_malformed_encoding_and_stopped_are_not_success(self):
        add(self.db, 'f', stderr=None, node=False)
        add(self.db, 'unknown-encoding', encoding='not-a-codec', start=3)
        add(self.db, 'stopped', code=0, state='stopped', reason='timeout', start=6)
        catalog.read_rows(self.db, "UPDATE shell_execution SET shell_json='{}' WHERE id='f'")
        self.build()
        rows = {r['id']: r for r in self.rows()}
        self.assertFalse(rows['f']['command_known'])
        self.assertIsNone(rows['f']['node_id'])
        self.assertEqual(rows['unknown-encoding']['error'], '<encoding-unavailable>')
        self.assertTrue(rows['stopped']['failure'])
        self.assertEqual(rows['stopped']['category'], 'timeout')
        self.assertIsNone(rows['f']['recovery'])

    def test_redaction_split_chunks_cut_lines_and_quoted_values(self):
        add(self.db, 'f', "FOO='CANARY SPACE' cargo test --password='CANARY FLAG'", stderr='error: Bearer CANARY')
        catalog.read_rows(self.db, "INSERT INTO output_chunk VALUES('f',2,'stderr',20,?,?)", (b'_SPLIT_SECRET\n', time_at(1)))
        self.build()
        raw = (self.root / 'derived/errors.sqlite3').read_bytes()
        for canary in (b'CANARY', b'SPLIT_SECRET'):
            self.assertNotIn(canary, raw)
        catalog.read_rows(self.db, "UPDATE output_chunk SET bytes=? WHERE execution_id='f' AND sequence=1", (b'error: safe complete\n' + b'A'*5000,))
        self.build()
        self.assertEqual(self.rows()[0]['error'], 'error: safe complete')
        self.assertTrue(self.rows()[0]['stderr_limited'])

    def test_supported_schema39_through45_and_unknown_versions_fail_closed(self):
        add(self.db, 'schema-compat')
        for version in range(39, 46):
            with self.subTest(version=version):
                catalog.read_rows(self.db, f'PRAGMA user_version={version}')
                self.db.commit()
                self.build()
                self.assertTrue(self.rows())
        for version in (38, 46, 999):
            with self.subTest(version=version):
                catalog.read_rows(self.db, f'PRAGMA user_version={version}')
                self.db.commit()
                with self.assertRaisesRegex(ValueError, 'schema39 through schema45'):
                    catalog.facts(self.root / 'shellboy.db')

    def test_source_version_fail_closed_and_rebuild_after_option_change(self):
        add(self.db, 'f')
        self.build()
        self.build(steps=6)
        self.assertEqual(catalog.read_catalog(self.root / 'derived/errors.sqlite3')[0]['options']['steps'], 6)
        catalog.read_rows(self.db, 'PRAGMA user_version=999')
        self.db.commit()
        self.assertFalse(self.cli('build', expected=1)['ok'])
        self.assertFalse(self.cli('top', '--since', 'not-a-time', expected=1)['ok'])

    def tracked_connections(self, failure=None):
        # Strong references deliberately outlive each operation. A forgotten
        # close cannot be hidden by CPython refcounting or cyclic collection.
        connections = []
        real_connect = sqlite3.connect

        class Cursor(sqlite3.Cursor):
            def executemany(self, *args, **kwargs):
                if failure is not None:
                    raise failure
                return super().executemany(*args, **kwargs)

            def close(self):
                self.explicitly_closed = True
                return super().close()

        class Connection(sqlite3.Connection):
            def cursor(self):
                cursor = super().cursor(factory=Cursor)
                cursor.explicitly_closed = False
                self.held_cursors.append(cursor)
                return cursor

            def execute(self, *_args, **_kwargs):
                raise AssertionError('Use an explicitly scoped cursor')

            def close(self):
                if not all(cursor.explicitly_closed for cursor in self.held_cursors):
                    raise AssertionError('A live cursor outlived its scope')
                self.explicitly_closed = True
                return super().close()

        def connect(*args, **kwargs):
            db = real_connect(*args, factory=Connection, **kwargs)
            db.explicitly_closed = False
            db.held_cursors = []
            connections.append(db)
            return db

        return connections, connect

    def assert_connections_closed(self, connections):
        self.assertTrue(connections)
        self.assertTrue(all(db.explicitly_closed for db in connections))
        self.assertTrue(all(cursor.explicitly_closed for db in connections for cursor in db.held_cursors))

    def test_every_connection_and_cursor_closed_before_replace_unlink_and_refresh(self):
        add(self.db, 'f')
        self.db.commit()
        connections, connect = self.tracked_connections()
        real_replace = os.replace

        def replace(source, destination):
            self.assert_connections_closed(connections)
            return real_replace(source, destination)

        with patch.object(catalog.sqlite3, 'connect', side_effect=connect), patch.object(catalog.os, 'replace', side_effect=replace):
            catalog.build(self.root)
            catalog.top(self.root)
            catalog.similar(self.root, 'f')
            catalog.build(self.root, incremental=True)
            self.assert_connections_closed(connections)
            target = self.root / 'derived/errors.sqlite3'
            target.unlink()
            catalog.build(self.root)
        self.assert_connections_closed(connections)

    def test_write_and_replace_failures_close_handles_preserve_original_and_snapshot(self):
        add(self.db, 'f')
        self.build()
        target = self.root / 'derived/errors.sqlite3'
        snapshot = target.read_bytes()
        for write_failure in (True, False):
            original = sqlite3.OperationalError('synthetic write failure') if write_failure else OSError('synthetic replace failure')
            connections, connect = self.tracked_connections(original if write_failure else None)
            real_unlink = os.unlink

            def unlink(path):
                self.assert_connections_closed(connections)
                return real_unlink(path)

            with patch.object(catalog.sqlite3, 'connect', side_effect=connect), patch.object(catalog.os, 'unlink', side_effect=unlink), patch.object(catalog.os, 'replace', side_effect=original):
                with self.assertRaises(type(original)) as caught:
                    catalog.build(self.root)
                self.assertIs(caught.exception, original)
            self.assert_connections_closed(connections)
            self.assertEqual(target.read_bytes(), snapshot)
            self.assertEqual(list(target.parent.glob('.errors-*')), [])

    def test_cleanup_failure_is_chained_without_masking_original_error(self):
        add(self.db, 'f')
        self.build()
        target = self.root / 'derived/errors.sqlite3'
        snapshot = target.read_bytes()
        original = OSError('synthetic publication failure')
        cleanup = PermissionError('synthetic cleanup failure')
        with patch.object(catalog.os, 'replace', side_effect=original), patch.object(catalog.os, 'unlink', side_effect=cleanup):
            with self.assertRaises(OSError) as caught:
                catalog.build(self.root)
            self.assertIs(caught.exception, original)
            self.assertIs(caught.exception.__cause__, cleanup)
        self.assertEqual(target.read_bytes(), snapshot)
        staging = list(target.parent.glob('.errors-*'))
        self.assertEqual(len(staging), 1)
        staging[0].unlink()

    @unittest.skipIf(os.name == 'nt', 'symlink creation requires Windows privilege')
    def test_symlink_and_hardlink_output_aliases_are_rejected(self):
        add(self.db, 'f')
        self.db.commit()
        derived = self.root / 'derived'
        derived.mkdir()
        target = derived / 'errors.sqlite3'
        os.link(self.root / 'shellboy.db', target)
        self.assertFalse(self.cli('build', expected=1)['ok'])
        target.unlink()
        target.symlink_to(self.root / 'shellboy.db')
        self.assertFalse(self.cli('build', expected=1)['ok'])


if __name__ == '__main__':
    unittest.main()
