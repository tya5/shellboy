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
        db.executescript(current.split('-- Derived readback paths only:')[0])
        for ident, parent in [('parent', None), ('child', 'parent')]:
            db.execute('INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) '
                       'VALUES(?,?,?,?,?,?,?)', (ident, parent, '/synthetic', '', 'fixture', 'mock', '2026-11-01T00:00:00Z'))
        db.execute('INSERT INTO node(id,kind,created_at,payload_json) VALUES(?,?,?,?)',
                   ('original', 'user', '2026-11-01T00:00:00Z', '{"text":"synthetic original"}'))
        db.execute('INSERT INTO thread_path VALUES(?,?,?)', ('parent', 0, 'original'))
        db.commit()
        checks.prepare_schema(db, schema)
        return db

    def upgrade(self, db, start=16):
        before, records = checks.ledger(db), checks.snapshot(db)
        for version in range(start + 1, 18):
            db.executescript(checks.DERIVED[version].read_text(encoding='utf-8'))
        checks.check_migration(db, before, start, 17)
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
