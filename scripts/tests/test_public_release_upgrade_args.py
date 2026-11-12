#!/usr/bin/env python3
"""Owner-free argument/source acceptance; never executes either binary."""
from contextlib import redirect_stderr
import importlib.util
import io
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('release_upgrade', ROOT / 'scripts/check-release-upgrade.py')
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


class PublicUpgradeArguments(unittest.TestCase):
    base = ['--old', 'synthetic-old', '--new', 'synthetic-new', '--root', 'synthetic-not-created']

    def test_public_011_same_schema_patch(self):
        args = upgrade.parse_args(self.base + ['--old-version', '0.11.0', '--expected-version', '0.12.0', '--start-schema', '42'])
        self.assertEqual(args.expected_new_schema,43)
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
        self.assertEqual(versions, list(range(1, 44)))
        self.assertIn('(39, "0039_independent_runs")', names)
        self.assertEqual(upgrade.PUBLIC_SCHEMA['0.11.0'], 39)
        self.assertIn('(40, "0040_selection_nonappend_generation")', names)
        self.assertIn('(41, "0041_receipt_reply_node_lookup")', names)
        self.assertIn('(42, "0042_notification_semantic_descriptor")', names)
        self.assertIn('(43, "0043_thread_single_summary_settings")', names)
        self.assertTrue((ROOT / 'migrations/0039_independent_runs.sql').is_file())

    def test_schema_rollback_rejected(self):
        for extra in [['--start-schema', '38'], ['--expected-new-schema', '38']]:
            with self.subTest(extra=extra), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                upgrade.parse_args(self.base + ['--old-version', '0.11.0'] + extra)
            self.assertEqual(raised.exception.code, 2)


if __name__ == '__main__':
    unittest.main()
