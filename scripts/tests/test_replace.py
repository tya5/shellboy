import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'dev/replace.py'
spec = importlib.util.spec_from_file_location('exact_replace', SCRIPT)
edit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(edit)


def rule(file, before, after):
    return dict(file=file, before=before, after=after)


class ReplaceTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name).resolve()
        (self.root / 'a').write_bytes(b'A\r\n')
        (self.root / 'b').write_bytes(b'B\n')

    def assert_no_stages(self):
        self.assertEqual(list(self.root.glob('.replace-*')), [])

    def test_multiple_files_sequential_rules_unicode_and_exact_newlines(self):
        data = 'e\u0301\U0001f642\r\n'.encode()
        (self.root / 'a').write_bytes(data)
        result = edit.apply([rule('a', 'e\u0301', 'X'), rule('b', 'B', 'BB'),
                             rule('a', 'X', 'Y')], self.root)
        self.assertEqual((self.root / 'a').read_bytes(), 'Y\U0001f642\r\n'.encode())
        self.assertEqual((self.root / 'b').read_bytes(), b'BB\n')
        self.assertEqual(result['changed_files'], ['a', 'b'])
        self.assertEqual(result['replacements'], 3)
        self.assert_no_stages()

    def test_any_missing_or_ambiguous_rule_preserves_all_files_and_metadata(self):
        original = edit.identity(self.root / 'a')
        for before, body in [('missing', 'BBB'), ('B', 'BB'), ('aba', 'ababa')]:
            (self.root / 'b').write_text(body)
            with self.assertRaises(edit.MatchError):
                edit.apply([rule('a', 'A', 'changed'), rule('b', before, 'new')], self.root)
            self.assertEqual((self.root / 'a').read_bytes(), b'A\r\n')
            self.assertEqual(edit.identity(self.root / 'a'), original)
            self.assertEqual((self.root / 'b').read_text(), body)
            self.assert_no_stages()

    def test_invalid_rules_missing_binary_directory_and_outside_targets_write_nothing(self):
        (self.root / 'binary').write_bytes(b'\0ABC')
        cases = [None, {}, [rule('a', '', 'x')], [dict(file='a', before='A', after=3)],
                 [rule('absent', 'A', 'x')], [rule('binary', 'A', 'x')],
                 [rule('.', 'A', 'x')], [rule('../a', 'A', 'x')]]
        for rules in cases:
            with self.assertRaises((OSError, ValueError)):
                edit.apply(rules, self.root)
            self.assertEqual((self.root / 'a').read_bytes(), b'A\r\n')
            self.assert_no_stages()

    def test_noop_does_not_replace_inode(self):
        before = edit.identity(self.root / 'a')
        for rules in ([], [rule('a', 'A', 'A')]):
            self.assertEqual(edit.apply(rules, self.root)['changed_files'], [])
            self.assertEqual(edit.identity(self.root / 'a'), before)
        self.assert_no_stages()

    def test_readonly_and_hardlinked_files_are_rejected(self):
        os.chmod(self.root / 'a', 0o444)
        try:
            with self.assertRaises(ValueError):
                edit.apply([rule('a', 'A', 'x')], self.root)
        finally:
            os.chmod(self.root / 'a', 0o644)
        os.link(self.root / 'a', self.root / 'alias')
        with self.assertRaises(ValueError):
            edit.apply([rule('a', 'A', 'x')], self.root)
        self.assertEqual((self.root / 'alias').read_bytes(), b'A\r\n')
        self.assert_no_stages()

    @unittest.skipUnless(os.name == 'posix', 'Unix symlink/mode behavior')
    def test_symlink_is_rejected_and_executable_mode_preserved(self):
        (self.root / 'alias').symlink_to(self.root / 'a')
        with self.assertRaises(ValueError):
            edit.apply([rule('alias', 'A', 'x')], self.root)
        os.chmod(self.root / 'a', 0o755)
        edit.apply([rule('a', 'A', 'new')], self.root)
        self.assertEqual(stat.S_IMODE((self.root / 'a').stat().st_mode), 0o755)
        self.assert_no_stages()

    def test_stage_failure_does_not_commit_first_file(self):
        link = os.link
        def fail_second(source, target):
            if Path(source).name == 'b':
                raise OSError('synthetic stage failure')
            return link(source, target)
        with patch.object(edit.os, 'link', fail_second), self.assertRaises(OSError):
            edit.apply([rule('a', 'A', 'new'), rule('b', 'B', 'new')], self.root)
        self.assertEqual((self.root / 'a').read_bytes(), b'A\r\n')
        self.assertEqual((self.root / 'b').read_bytes(), b'B\n')
        self.assert_no_stages()

    def test_second_commit_failure_restores_first_original_inode(self):
        before = (self.root / 'a').stat()
        replace = os.replace
        def fail_second(source, target):
            if Path(source).name == 'new' and Path(target).name == 'b':
                raise OSError('synthetic write failure')
            return replace(source, target)
        with patch.object(edit.os, 'replace', fail_second), self.assertRaises(OSError):
            edit.apply([rule('a', 'A', 'new'), rule('b', 'B', 'new')], self.root)
        self.assertEqual((self.root / 'a').read_bytes(), b'A\r\n')
        self.assertEqual((self.root / 'b').read_bytes(), b'B\n')
        self.assertEqual((self.root / 'a').stat().st_ino, before.st_ino)
        self.assertEqual((self.root / 'a').stat().st_mtime_ns, before.st_mtime_ns)
        self.assert_no_stages()

    def test_incomplete_rollback_reports_recovery_and_keeps_original(self):
        replace = os.replace
        def fail_write_and_restore(source, target):
            if Path(source).name == 'old' or Path(target).name == 'b':
                raise OSError('synthetic persistent I/O failure')
            return replace(source, target)
        with patch.object(edit.os, 'replace', fail_write_and_restore):
            with self.assertRaises(edit.RecoveryError) as failure:
                edit.apply([rule('a', 'A', 'new'), rule('b', 'B', 'new')], self.root)
        copies = [Path(path) / 'old' for path in failure.exception.directories]
        self.assertIn(b'A\r\n', [path.read_bytes() for path in copies])
        self.assertEqual((self.root / 'b').read_bytes(), b'B\n')

    def test_concurrent_edits_are_not_overwritten_during_rollback(self):
        replace = os.replace
        def alter_committed_then_fail(source, target):
            if Path(source).name == 'new' and Path(target).name == 'b':
                (self.root / 'a').write_text('new external edit')
                raise OSError('synthetic write failure')
            return replace(source, target)
        with patch.object(edit.os, 'replace', alter_committed_then_fail):
            with self.assertRaises(edit.RecoveryError):
                edit.apply([rule('a', 'A', 'new'), rule('b', 'B', 'new')], self.root)
        self.assertEqual((self.root / 'a').read_text(), 'new external edit')

    def test_size_bound_is_preflighted(self):
        for limit in (2, 3):
            with patch.object(edit, 'TEXT_LIMIT', limit), self.assertRaises(ValueError):
                edit.apply([rule('a', 'A', 'longer')], self.root)
        self.assertEqual((self.root / 'a').read_bytes(), b'A\r\n')
        self.assert_no_stages()

    def test_cli_json_success_failure_and_no_new_file(self):
        def run(value):
            return subprocess.run([sys.executable, str(SCRIPT), '--root', str(self.root)],
                                  input=value, text=True, capture_output=True)
        for value, code in [('', 2), ('{}', 2), (json.dumps([rule('a', 'missing', 'X')]), 1),
                            (json.dumps([rule('absent', 'x', 'y')]), 2), ('[]', 0),
                            (json.dumps([rule('a', 'A', 'X')]), 0)]:
            result = run(value)
            self.assertEqual(result.returncode, code, result.stderr + result.stdout)
            self.assertEqual(json.loads(result.stdout)['ok'], code == 0)
        self.assertFalse((self.root / 'absent').exists())
        self.assertEqual((self.root / 'a').read_bytes(), b'X\r\n')
        self.assert_no_stages()

    def test_interrupt_after_rename_restores_original(self):
        replace = os.replace
        def interrupt_after_swap(source, target):
            replace(source, target)
            if Path(source).name == 'new':
                raise KeyboardInterrupt()
        with patch.object(edit.os, 'replace', interrupt_after_swap), self.assertRaises(KeyboardInterrupt):
            edit.apply([rule('a', 'A', 'new')], self.root)
        self.assertEqual((self.root / 'a').read_bytes(), b'A\r\n')
        self.assert_no_stages()

    def test_existing_outside_file_is_rejected(self):
        nested = self.root / 'nested'
        nested.mkdir()
        with self.assertRaises(ValueError):
            edit.apply([rule('../a', 'A', 'new')], nested)
        self.assertEqual((self.root / 'a').read_bytes(), b'A\r\n')

    def test_change_during_staging_prevents_all_commits(self):
        link = os.link
        def change_first_after_backup(source, target):
            link(source, target)
            if Path(source).name == 'b':
                (self.root / 'a').write_text('external edit')
        with patch.object(edit.os, 'link', change_first_after_backup), self.assertRaises(ValueError):
            edit.apply([rule('a', 'A', 'new'), rule('b', 'B', 'new')], self.root)
        self.assertEqual((self.root / 'a').read_text(), 'external edit')
        self.assertEqual((self.root / 'b').read_bytes(), b'B\n')
        self.assert_no_stages()
