"""Production derived migrations preserve synthetic originals and Thread lineage."""
import importlib.util
from pathlib import Path
import sqlite3
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('release_upgrade', ROOT / 'scripts/check-release-upgrade.py')
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)


class ReleaseUpgradeTests(unittest.TestCase):
    def fixture(self, schema=16):
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        db.execute('PRAGMA foreign_keys=ON')
        current = (ROOT / 'src/storage/current.sql').read_text(encoding='utf-8')
        base = current.split('-- Derived readback paths only:')[0]
        # The canonical schema has the version-18 index; model the old index.
        suffix = " OR key LIKE 'summary.usage.%'"
        self.assertEqual(base.count(suffix), 1)
        db.executescript(base.replace(suffix, ''))
        for ident, parent in [('parent', None), ('child', 'parent')]:
            db.execute('INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) '
                       'VALUES(?,?,?,?,?,?,?)', (ident, parent, '/synthetic', '', 'fixture', 'mock', '2026-11-01T00:00:00Z'))
        db.execute('INSERT INTO node(id,kind,created_at,payload_json) VALUES(?,?,?,?)',
                   ('original', 'user', '2026-11-01T00:00:00Z', '{"text":"synthetic original"}'))
        db.execute('INSERT INTO thread_path VALUES(?,?,?)', ('parent', 0, 'original'))
        db.commit()
        checks.prepare_schema(db, schema)
        return db

    def upgrade(self, db, start=16, target=17):
        before, records = checks.ledger(db), checks.snapshot(db)
        for version in range(start + 1, target + 1):
            db.executescript(checks.DERIVED[version].read_text(encoding='utf-8'))
        checks.check_migration(db, before, start, target)
        checks.preserved(db, records)
        return before, records

    def test_schema16_to17_preserves_rows_ledger_and_parent_index(self):
        db = self.fixture()
        before, records = self.upgrade(db)
        self.assertEqual(checks.ledger(db), before + [(17, '0017_thread_parent')])
        self.assertEqual(checks.snapshot(db), records)
        self.assertEqual(db.execute('SELECT id FROM thread WHERE parent_thread_id=?', ('parent',)).fetchall(), [('child',)])

    def test_public_schema14_to17_keeps_the_complete_prefix(self):
        db = self.fixture(14)
        before, _ = self.upgrade(db, 14)
        self.assertEqual(len(before), 14)
        self.assertEqual(checks.ledger(db)[14:], [(n, checks.DERIVED[n].stem) for n in (15, 16, 17)])

    def test_staging16_is_derived_only_and_does_not_rewrite_originals(self):
        db = self.fixture(14)
        before, records = checks.ledger(db), checks.snapshot(db)
        checks.prepare_schema(db, 16)
        self.assertEqual(checks.snapshot(db), records)
        self.assertEqual(checks.ledger(db), before + [(n, checks.DERIVED[n].stem) for n in (15, 16)])

    def test_rewritten_original_is_rejected(self):
        db = self.fixture()
        _, records = self.upgrade(db)
        db.execute('UPDATE node SET payload_json=?', ('{"text":"changed"}',))
        with self.assertRaisesRegex(AssertionError, 'saved data changed: node'):
            checks.preserved(db, records)

    def test_changed_parent_is_rejected(self):
        db = self.fixture()
        _, records = self.upgrade(db)
        db.execute('UPDATE thread SET parent_thread_id=NULL WHERE id="child"')
        with self.assertRaisesRegex(AssertionError, 'thread_lineage'):
            checks.preserved(db, records)

    def test_changed_ledger_prefix_is_rejected(self):
        db = self.fixture()
        before, _ = self.upgrade(db)
        db.execute('UPDATE schema_migration SET name="changed" WHERE version=1')
        with self.assertRaisesRegex(AssertionError, 'ledger'):
            checks.check_migration(db, before, 16, 17)

    def test_wrong_parent_index_is_rejected(self):
        db = self.fixture()
        before, _ = self.upgrade(db)
        db.executescript('DROP INDEX thread_parent; CREATE INDEX thread_parent ON thread(id);')
        with self.assertRaises(AssertionError):
            checks.check_migration(db, before, 16, 17)

    def test_stopped_checkpointed_wal_database_can_be_read_without_sidecars(self):
        import tempfile
        with tempfile.TemporaryDirectory() as scratch:
            from contextlib import closing
            source = Path(scratch) / 'source.db'
            with closing(sqlite3.connect(source)) as db:
                with closing(db.cursor()) as cursor:
                    cursor.executescript('PRAGMA journal_mode=WAL; CREATE TABLE sample(value); INSERT INTO sample VALUES(17);')
                    self.assertEqual(cursor.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone(), (0, 0, 0))
            # Make the no-sidecar condition explicit, independent of WAL unlink policy.
            path = Path(scratch) / 'checkpointed.db'
            path.write_bytes(source.read_bytes())
            self.assertFalse(Path(str(path) + '-wal').exists())
            uri = checks.read_only_uri(path, stopped=True)
            self.assertIn('immutable=1', uri)
            from contextlib import closing
            with closing(sqlite3.connect(uri, uri=True)) as reader:
                self.assertEqual(reader.execute('SELECT * FROM sample').fetchall(), [(17,)])
            self.assertFalse(Path(str(path) + '-wal').exists())
            self.assertNotIn('immutable', checks.read_only_uri(path))

    def test_existing_wal_is_never_ignored(self):
        import tempfile
        from contextlib import closing
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / 'committed-wal.db'
            with closing(sqlite3.connect(path)) as writer:
                writer.executescript('PRAGMA journal_mode=WAL; CREATE TABLE sample(value); INSERT INTO sample VALUES(17);')
                self.assertTrue(Path(str(path) + '-wal').exists())
                uri = checks.read_only_uri(path, stopped=True)
                self.assertNotIn('immutable', uri)
                with closing(sqlite3.connect(uri, uri=True)) as reader:
                    self.assertEqual(reader.execute('SELECT * FROM sample').fetchall(), [(17,)])

    def test_public_schema24_and25_staging_is_a_noop(self):
        for schema in (24, 25):
            with self.subTest(schema=schema):
                db = self.fixture(17)
                self.upgrade(db, 17, schema)
                before, records = checks.ledger(db), checks.snapshot(db)
                checks.prepare_schema(db, schema)
                self.assertEqual(checks.ledger(db), before)
                self.assertEqual(checks.snapshot(db), records)
                self.assertEqual(db.execute('PRAGMA user_version').fetchone(), (schema,))

    def test_public_schema19_noop_preserves_records_and_ledger(self):
        db = self.fixture(17)
        self.upgrade(db, 17, 19)
        before, records = checks.ledger(db), checks.snapshot(db)
        checks.prepare_schema(db, 19)
        self.assertEqual(checks.ledger(db), before)
        self.assertEqual(checks.snapshot(db), records)
        self.assertEqual(db.execute('PRAGMA user_version').fetchone(), (19,))

    def test_public_schema17_to18_preserves_records_and_extends_index(self):
        db = self.fixture(17)
        db.execute("INSERT INTO app_meta(key,value) VALUES('summary.usage.synthetic', '{\"tokens\":7}')")
        db.commit()
        before, records = self.upgrade(db, 17, 18)
        self.assertEqual(checks.ledger(db), before + [(18, '0018_summary_usage_metadata')])
        self.assertEqual(checks.snapshot(db), records)
        self.assertEqual(records['summary_usage'], [('summary.usage.synthetic', '{"tokens":7}')])
        self.assertIn("key LIKE 'summary.usage.%'", db.execute(
            "SELECT sql FROM sqlite_master WHERE name='gc_bytes_app_meta_history'").fetchone()[0])
        db.execute("UPDATE app_meta SET value='changed' WHERE key='summary.usage.synthetic'")
        with self.assertRaisesRegex(AssertionError, 'summary_usage'):
            checks.preserved(db, records)

    def test_historical_schema14_to18_keeps_complete_prefix(self):
        db = self.fixture(14)
        before, _ = self.upgrade(db, 14, 18)
        self.assertEqual(checks.ledger(db)[14:], [(n, checks.DERIVED[n].stem) for n in (15, 16, 17, 18)])
        self.assertEqual(len(before), 14)

    def test_schema20_to21_preserves_rows_and_both_index_sets(self):
        db = self.fixture(17)
        self.upgrade(db, 17, 20)
        before, records = self.upgrade(db, 20, 21)
        self.assertEqual(checks.ledger(db), before + [(21, '0021_event_retention')])
        self.assertEqual(checks.snapshot(db), records)
        indexes = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='index'")}
        self.assertTrue({'jobs_source', 'nodes_index_gc', 'node_event_retention', 'delivery_event_state'} <= indexes)
        db.executescript('DROP INDEX node_event_retention; CREATE INDEX node_event_retention ON node(id);')
        with self.assertRaisesRegex(AssertionError, 'node_event_retention'):
            checks.check_migration(db, before, 20, 21)

    def test_schema21_to22_adds_exact_counters_and_delivery_index(self):
        db = self.fixture(17)
        self.upgrade(db, 17, 21)
        before, records = self.upgrade(db, 21, 22)
        self.assertEqual(checks.ledger(db), before + [(22, '0022_logical_usage_counters')])
        self.assertEqual(checks.snapshot(db), records)
        db.execute("UPDATE quota_counter SET logical_bytes=logical_bytes+1 WHERE category='logical.raw'")
        with self.assertRaisesRegex(AssertionError, 'raw counter'):
            checks.check_migration(db, before, 21, 22)

    def test_schema22_to23_preserves_rows_and_prior_indexes(self):
        db = self.fixture(17)
        self.upgrade(db, 17, 22)
        before, records = self.upgrade(db, 22, 23)
        self.assertEqual(checks.ledger(db), before + [(23, '0023_activity_bounded')])
        self.assertEqual(checks.snapshot(db), records)
        indexes = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='index'")}
        self.assertTrue({'jobs_source', 'nodes_index_gc', 'node_event_retention', 'delivery_event_state',
                         'activity_attempt_turn', 'activity_turn_run', 'activity_turn_run_state',
                         'activity_group_turn_state', 'activity_shell_spawned_turn',
                         'activity_shell_uncollected_owner'} <= indexes)
        db.executescript('DROP INDEX activity_attempt_turn; CREATE INDEX activity_attempt_turn ON model_attempt(id);')
        with self.assertRaisesRegex(AssertionError, 'activity_attempt_turn'):
            checks.check_migration(db, before, 22, 23)

    def test_schema23_to24_drops_revisit_metadata_and_adds_completion_index(self):
        db = self.fixture(17)
        self.upgrade(db, 17, 23)
        db.execute("INSERT INTO app_meta(key,value) VALUES('revisit.run','{}')")
        before, records = self.upgrade(db, 23, 24)
        self.assertEqual(checks.ledger(db), before + [(24, '0024_revisit_messages')])
        self.assertEqual(checks.snapshot(db), records)
        self.assertEqual(db.execute("SELECT count(*) FROM app_meta WHERE key GLOB 'revisit.*'").fetchone(), (0,))
        db.executescript('DROP INDEX receipt_run_completion; CREATE INDEX receipt_run_completion ON receipt(run_id);')
        with self.assertRaisesRegex(AssertionError, 'receipt_run_completion'):
            checks.check_migration(db, before, 23, 24)

    def test_schema25_to26_adds_partial_reply_index_without_rewriting_rows(self):
        db = self.fixture(17)
        self.upgrade(db, 17, 25)
        before, records = self.upgrade(db, 25, 26)
        self.assertEqual(checks.ledger(db), before + [(26, '0026_reply_expected')])
        self.assertEqual(checks.snapshot(db), records)
        plan = db.execute("EXPLAIN QUERY PLAN SELECT rowid FROM receipt WHERE target_thread_id=? AND json_extract(result_json,'$.reply.state')='pending'", ('parent',)).fetchall()
        self.assertTrue(any('receipt_reply_pending' in row[3] for row in plan))
        db.executescript('DROP INDEX receipt_reply_pending; CREATE INDEX receipt_reply_pending ON receipt(target_thread_id);')
        with self.assertRaisesRegex(AssertionError, 'receipt_reply_pending'):
            checks.check_migration(db, before, 25, 26)

    def test_schema18_rejects_old_history_index_and_parent_index(self):
        db = self.fixture(17)
        before, _ = self.upgrade(db, 17, 18)
        db.executescript("DROP INDEX gc_bytes_app_meta_history; CREATE INDEX gc_bytes_app_meta_history ON app_meta(octet_length(value)) WHERE key LIKE 'summary.part.%';")
        with self.assertRaisesRegex(AssertionError, 'history accounting index'):
            checks.check_migration(db, before, 17, 18)
        db.executescript(checks.DERIVED[18].read_text(encoding='utf-8').split('INSERT INTO schema_migration')[0])
        db.executescript('DROP INDEX thread_parent; CREATE INDEX thread_parent ON thread(id);')
        with self.assertRaises(AssertionError):
            checks.check_migration(db, before, 17, 18)


    def test_schema24_to25_adds_attempt_lookup_index_and_preserves_rows(self):
        db = self.fixture(17)
        self.upgrade(db, 17, 24)
        before, records = self.upgrade(db, 24, 25)
        self.assertEqual(checks.ledger(db), before + [(25, '0025_history_attempt_index')])
        self.assertEqual(checks.snapshot(db), records)
        self.assertEqual([row[2] for row in db.execute('PRAGMA index_info(history_tool_group_attempt)')],
                         ['attempt_id'])
        db.executescript('DROP INDEX history_tool_group_attempt; CREATE INDEX history_tool_group_attempt ON tool_group(id);')
        with self.assertRaisesRegex(AssertionError, 'history_tool_group_attempt'):
            checks.check_migration(db, before, 24, 25)

    def test_arguments_default_to_public_v071_schema25_and_target27(self):
        required = ['--old', '/old', '--new', '/new', '--root', '/new-fixture']
        args = checks.parse_args(required)
        self.assertEqual((args.old_version, args.start_schema, args.expected_version, args.expected_new_schema),
                         ('0.7.1', 25, '0.7.1', 27))
        explicit = checks.parse_args(required + ['--old-version', '0.5.0', '--start-schema', '17', '--expected-new-schema', '19'])
        self.assertEqual((explicit.old_version, explicit.start_schema, explicit.expected_new_schema), ('0.5.0', 17, 19))
        current = checks.parse_args(required + ['--old-version', '0.6.0', '--expected-version', '0.6.1'])
        self.assertEqual((current.start_schema, current.expected_new_schema), (19, 27))
        release = checks.parse_args(required + ['--old-version', '0.7.0', '--expected-version', '0.7.1'])
        self.assertEqual((release.start_schema, release.expected_new_schema), (24, 27))
        historical = checks.parse_args(required + ['--old-version', '0.4.0', '--expected-new-schema', '17'])
        self.assertEqual((historical.start_schema, historical.expected_new_schema), (14, 17))
        explicit_public_schema = checks.parse_args(required + ['--old-version', '0.7.0', '--start-schema', '24'])
        self.assertEqual(explicit_public_schema.start_schema, 24)
        staged = checks.parse_args(required + ['--old-version', '0.4.0', '--start-schema', '16'])
        self.assertEqual(staged.start_schema, 16)
        with self.assertRaises(SystemExit):
            checks.parse_args(required + ['--start-schema', '14'])
        with self.assertRaises(SystemExit):
            checks.parse_args(required + ['--old-version', '0.4.0', '--start-schema', '17', '--expected-new-schema', '15'])
