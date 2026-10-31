"""Exercise the read-only record checker against disposable Git worktrees."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'dev/record-delta.py'
RECORDS = ('docs/development/status.md', 'docs/validation/acceptance.md',
           'docs/development/decisions.md')


class RecordDeltaTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')
        self.git('init', '-q')
        self.git('config', 'user.name', 'Synthetic fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'core.autocrlf', 'false')
        for name in RECORDS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'heading\n\nexisting\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'synthetic baseline')
        self.base = self.git('rev-parse', 'HEAD').stdout.strip()
        self.git('update-ref', 'refs/remotes/origin/main', self.base)

    def git(self, *args):
        return subprocess.run(['git', '-C', str(self.root), *args], env=self.env,
                              text=True, capture_output=True, check=True)

    def run_report(self, *args):
        result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(self.root), *args],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(len(result.stdout.splitlines()), 1, result.stderr)
        return result.returncode, json.loads(result.stdout)

    def test_default_base_unchanged_and_read_only(self):
        before = self.git('status', '--porcelain').stdout
        code, result = self.run_report()
        self.assertEqual(code, 0)
        self.assertTrue(result['ok'])
        self.assertEqual(result['base_commit'], self.base)
        self.assertEqual(self.git('status', '--porcelain').stdout, before)
        for value in result['files'].values():
            self.assertEqual((value['base_lines'], value['current_lines'], value['delta']), (3, 3, 0))
            self.assertEqual((value['added'], value['deleted']), (0, 0))

    def test_staged_and_unstaged_unicode_additions(self):
        path = self.root / RECORDS[0]
        path.write_bytes(path.read_bytes() + '\u65e5\u672c\u8a9e\n'.encode())
        self.git('add', RECORDS[0])
        path = self.root / RECORDS[1]
        path.write_bytes(b'inserted\n' + path.read_bytes())
        code, result = self.run_report('--base', 'HEAD')
        self.assertEqual(code, 0)
        self.assertTrue(result['ok'])
        for name in RECORDS[:2]:
            self.assertEqual(result['files'][name]['delta'], 1)
            self.assertEqual(result['files'][name]['deleted'], 0)

    def test_deletion_and_same_line_count_replacement_fail_without_writes(self):
        for data, delta in [(b'heading\n\n', -1), (b'heading\n\nreplacement\n', 0)]:
            path = self.root / RECORDS[2]
            path.write_bytes(data)
            status = self.git('status', '--porcelain').stdout
            code, result = self.run_report('--base', self.base)
            self.assertEqual(code, 1)
            self.assertFalse(result['ok'])
            self.assertEqual(result['files'][RECORDS[2]]['delta'], delta)
            self.assertEqual(result['files'][RECORDS[2]]['deleted'], 1)
            self.assertEqual(path.read_bytes(), data)
            self.assertEqual(self.git('status', '--porcelain').stdout, status)

    def test_invalid_or_option_shaped_base_is_json_error(self):
        for base in ['absent-ref', '--help']:
            code, result = self.run_report('--base=' + base)
            self.assertEqual(code, 2)
            self.assertFalse(result['ok'])
            self.assertIn('error', result)

    def test_missing_or_binary_current_record_is_json_error(self):
        path = self.root / RECORDS[0]
        path.unlink()
        self.assertEqual(self.run_report()[0], 2)
        path.write_bytes(b'binary\0data')
        self.assertEqual(self.run_report()[0], 2)

    def test_missing_base_record_is_error(self):
        self.git('rm', RECORDS[0])
        self.git('commit', '-qm', 'missing baseline record')
        self.assertEqual(self.run_report('--base', 'HEAD')[0], 2)

    def test_repo_option_ignores_inherited_git_routing(self):
        self.env.update(GIT_DIR=str(self.root / 'absent'), GIT_WORK_TREE='/absent')
        code, result = self.run_report()
        self.assertEqual(code, 0)
        self.assertEqual(result['base_commit'], self.base)
