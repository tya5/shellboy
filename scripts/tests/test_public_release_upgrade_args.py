#!/usr/bin/env python3
"""Owner-free argument/source acceptance; never executes either binary."""
from contextlib import redirect_stderr
import importlib.util
import io
from pathlib import Path
import re
import sqlite3
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('release_upgrade', ROOT / 'scripts/check-release-upgrade.py')
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


class PublicUpgradeArguments(unittest.TestCase):
    base = ['--old', 'synthetic-old', '--new', 'synthetic-new', '--root', 'synthetic-not-created']

    def test_public_011_same_schema_patch(self):
        args = upgrade.parse_args(self.base + ['--old-version', '0.11.0', '--expected-version', '0.12.0', '--start-schema', '42'])
        self.assertEqual(args.expected_new_schema,upgrade.CURRENT_SCHEMA_VERSION)
        self.assertEqual(args.start_schema,42)
        args = upgrade.parse_args(self.base + ['--old-version', '0.11.0', '--expected-version', '0.11.1', '--expected-new-schema', '39'])
        self.assertEqual((args.old_version, args.start_schema, args.expected_new_schema, args.expected_version), ('0.11.0', 39, 39, '0.11.1'))

    def test_historical_default_preserved(self):
        args = upgrade.parse_args(self.base)
        self.assertEqual((args.old_version, args.start_schema), ('0.10.0', 32))

    def test_current_schema_source_matches_public_entry(self):
        source = (ROOT / 'src/storage.rs').read_text()
        names = source.split('MIGRATION_NAMES', 1)[1].split('];', 1)[0]
        versions = [int(x) for x in re.findall(r'\((\d+),\s*"', names)]
        self.assertEqual(versions, list(range(1, upgrade.CURRENT_SCHEMA_VERSION + 1)))
        self.assertIn('(39, "0039_independent_runs")', names)
        self.assertEqual(upgrade.PUBLIC_SCHEMA['0.11.0'], 39)
        self.assertIn('(40, "0040_selection_nonappend_generation")', names)
        self.assertIn('(41, "0041_receipt_reply_node_lookup")', names)
        self.assertIn('(42, "0042_notification_semantic_descriptor")', names)
        self.assertIn('(43, "0043_thread_single_summary_settings")', names)
        self.assertIn('(44, "0044_thread_max_tokens")', names)
        self.assertIn('(45, "0045_thread_spill_override")', names)
        self.assertIn('(46, "0046_scope_identity_v2")', names)
        self.assertEqual(upgrade.CURRENT_SCHEMA_VERSION, 46)
        self.assertTrue((ROOT / 'migrations/0046_scope_identity_v2.sql').is_file())
        self.assertTrue((ROOT / 'migrations/0045_thread_spill_override.sql').is_file())
        self.assertTrue((ROOT / 'migrations/0039_independent_runs.sql').is_file())

    def test_schema44_to_current_spill_migration_replays_and_converges(self):
        fixture = ROOT / 'tests/fixtures/storage-current-schema43.sql'
        db = sqlite3.connect(':memory:')
        db.executescript(fixture.read_text())
        upgrade.prepare_schema(db, 44)
        self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 44)
        before_45 = upgrade.ledger(db)
        before_columns = {row[1] for row in db.execute('PRAGMA table_info(thread)')}
        self.assertNotIn('spill_override', before_columns)
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('old','/','','p','m','now')")
        upgrade.prepare_schema(db, upgrade.CURRENT_SCHEMA_VERSION)
        self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], upgrade.CURRENT_SCHEMA_VERSION)
        self.assertEqual(db.execute("SELECT version,name FROM schema_migration ORDER BY version DESC LIMIT 1").fetchone(), (upgrade.CURRENT_SCHEMA_VERSION, '0046_scope_identity_v2'))
        self.assertIsNone(db.execute("SELECT spill_override FROM thread WHERE id='old'").fetchone()[0])
        # The spill constraint remains pinned to 45 even after data-only 46.
        upgrade.check_migration(db, before_45, 44, upgrade.CURRENT_SCHEMA_VERSION)
        columns = [tuple(row) for row in db.execute('PRAGMA table_info(thread)')]
        fresh = sqlite3.connect(':memory:')
        current_sql = (ROOT / 'src/storage/current.sql').read_text()
        fresh.executescript(current_sql)
        self.assertEqual(fresh.execute('PRAGMA user_version').fetchone()[0], upgrade.CURRENT_SCHEMA_VERSION)
        self.assertEqual(columns, [tuple(row) for row in fresh.execute('PRAGMA table_info(thread)')])
        for value in (0, 1):
            db.execute("UPDATE thread SET spill_override=? WHERE id='old'", (value,))
            self.assertEqual(db.execute("SELECT spill_override FROM thread WHERE id='old'").fetchone()[0], value)
        for value in (2, -1, 'bad', 0.5):
            with self.subTest(value=value), self.assertRaises(sqlite3.IntegrityError):
                db.execute("UPDATE thread SET spill_override=? WHERE id='old'", (value,))
        self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
        db.close()
        fresh.close()

    def test_sql_only_staging_rejects_fact_conversion_without_advancing_checkpoint(self):
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        db.executescript((ROOT / 'src/storage/current.sql').read_text())
        db.execute('DELETE FROM schema_migration WHERE version=46')
        db.execute('PRAGMA user_version=45')
        old = '{"version":1,"scope_generation":0,"boundary_generation":0,"connection":"c","model":"m","first":null,"last_reset_origin":null}'
        db.execute("INSERT INTO app_meta VALUES('compact.a_state.t',?)", (old,))
        with self.assertRaisesRegex(AssertionError, 'native startup converter'):
            upgrade.prepare_schema(db, upgrade.CURRENT_SCHEMA_VERSION)
        self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 45)
        self.assertEqual(db.execute('SELECT count(*) FROM schema_migration WHERE version=46').fetchone()[0], 0)
        self.assertEqual(db.execute("SELECT value FROM app_meta WHERE key='compact.a_state.t'").fetchone()[0], old)

    def test_migration_registry_version_derivation_rejects_empty_duplicate_and_gap(self):
        for registry in ("", '(1, "m1"),\n(1, "duplicate")', '(1, "m1"),\n(3, "gap")'):
            with self.subTest(registry=registry), self.assertRaisesRegex(ValueError, 'nonempty, unique, and contiguous'):
                upgrade.registered_migration_versions(registry)
        self.assertEqual(upgrade.registered_migration_versions('(1, "m1"),\n(2, "m2")'), (1, 2))

    def test_migration45_spill_check_is_pinned_to_its_introduction_boundary(self):
        self.assertEqual(upgrade.SPILL_OVERRIDE_SCHEMA_VERSION, 45)
        self.assertIn(upgrade.SPILL_OVERRIDE_SCHEMA_VERSION, upgrade.REGISTERED_MIGRATION_VERSIONS)
        db = sqlite3.connect(':memory:')
        db.executescript((ROOT / 'src/storage/current.sql').read_text())
        upgrade.check_spill_override_schema(db, 45)
        db.execute('ALTER TABLE thread DROP COLUMN spill_override')
        with self.assertRaisesRegex(AssertionError, 'migration 45 spill override missing'):
            upgrade.check_spill_override_schema(db, 45)
        db.close()

    def test_schema_rollback_rejected(self):
        for extra in [['--start-schema', '38'], ['--expected-new-schema', '38']]:
            with self.subTest(extra=extra), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                upgrade.parse_args(self.base + ['--old-version', '0.11.0'] + extra)
            self.assertEqual(raised.exception.code, 2)


if __name__ == '__main__':
    unittest.main()
