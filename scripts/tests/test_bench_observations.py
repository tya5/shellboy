"""Measurements are exact or unavailable, never fabricated."""
import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'bench'))
from observations import RequestLog, storage
from report_stage4 import render


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

    def test_explicit_session_id_attributes_request_without_workspace(self):
        log = RequestLog(2)
        log.set_session_ids(['first-id', 'second-id'])
        log.record('/model', b'auxiliary', session_id='second-id')
        log.record('/model', b'missing', session_id='unknown-id')
        self.assertEqual(log.result()['requests'][0]['conversation'], 1)
        self.assertIsNone(log.result()['requests'][1]['conversation'])
        with self.assertRaises(ValueError):
            log.set_session_ids(['same', 'same'])

    def test_workspace_prefix_collision_and_ambiguity(self):
        log = RequestLog(2)
        log.set_workspaces(['/tmp/workspace-1', '/tmp/workspace-10'])
        log.record('/model', b'/tmp/workspace-10')
        log.record('/model', b'/tmp/workspace-1 and /tmp/workspace-10')
        self.assertEqual(log.result()['requests'][0]['conversation'], 1)
        self.assertIsNone(log.result()['requests'][1]['conversation'])


class ReportTests(unittest.TestCase):
    def test_report_never_substitutes_zero_for_unknown(self):
        case = {'n': 1, 'traffic': {'conversations': [
            {'total_bytes': None, 'first_bytes': None, 'last_bytes': None}]},
            'storage': {'bytes': 7, 'files': 1}}
        report = {'status': 'complete', 'smoke': False, 'agents': [
            {'agent': 'fixture', 'status': 'complete', 'cases': [case]}]}
        table = render(report)
        self.assertIn('| fixture | 1 | unknown | unknown | unknown | 7 | 1 |', table)
        case['storage']['bytes'] = None
        self.assertIn('| fixture | 1 | unknown | unknown | unknown | unknown | 1 |', render(report))
        report['status'] = 'error'
        with self.assertRaises(ValueError):
            render(report)


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
            # Symlink lstat size is platform-specific (Windows may report 0).
            self.assertEqual(result['bytes'], 4 + (root / 'link').lstat().st_size)

    def test_missing_root_is_error_not_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(FileNotFoundError):
                storage([Path(tmp) / 'missing'])


if __name__ == '__main__':
    unittest.main()
