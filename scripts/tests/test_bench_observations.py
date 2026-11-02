"""Measurements are exact or unavailable, never fabricated."""
import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'bench'))
from observations import RequestLog, storage


class RequestTests(unittest.TestCase):
    def test_unattributed_invalidates_conversation_totals(self):
        log = RequestLog(2)
        log.set_workspaces(['/tmp/workspace-0', '/tmp/workspace-1'])
        log.record('/v1/responses', b'abc /tmp/workspace-0')
        log.record('/v1/responses', b'/tmp/workspace-1')
        log.record('/v1/responses', b'unknown')
        result = log.result()
        self.assertEqual([r['bytes'] for r in result['requests']], [20, 16, 7])
        self.assertEqual(result['unattributed_requests'], 1)
        self.assertTrue(all(c['total_bytes'] is None for c in result['conversations']))

    def test_single_session_and_explicit_conversation(self):
        log = RequestLog(1)
        log.record('/model', b'\xc3\xa9')
        log.record('/model', b'abc', 0)
        self.assertEqual(log.result()['conversations'][0], {
            'conversation': 0, 'requests': 2, 'total_bytes': 5, 'first_bytes': 2, 'last_bytes': 3})

    def test_ambiguous_workspace_is_not_assigned(self):
        log = RequestLog(2)
        log.set_workspaces(['/tmp/workspace-1', '/tmp/workspace-10'])
        log.record('/model', b'/tmp/workspace-10')
        self.assertIsNone(log.result()['requests'][0]['conversation'])


class StorageTests(unittest.TestCase):
    def test_deduplicates_nested_roots_and_never_follows_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'nested').mkdir()
            (root / 'nested' / 'one').write_bytes(b'1234')
            os.link(root / 'nested' / 'one', root / 'two')
            (root / 'link').symlink_to('/a/long/external/path')
            result = storage([root, root / 'nested'])
            self.assertEqual(result['files'], 2)
            self.assertEqual(result['bytes'], 4 + len('/a/long/external/path'))

    def test_missing_root_is_error_not_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(FileNotFoundError):
                storage([Path(tmp) / 'missing'])


if __name__ == '__main__':
    unittest.main()
