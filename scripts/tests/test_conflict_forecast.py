"""Synthetic repository cases for the developer-only forecast helper."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HELPER = Path(__file__).resolve().parents[1] / 'dev/conflict-forecast.py'


class ForecastTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='forecast-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.write('plain.txt', 'base\n')
        self.git('add', '.'); self.git('commit', '-qm', 'base')
        self.git('update-ref', 'refs/remotes/origin/main', self.git('rev-parse', 'HEAD').stdout.strip())

    def git(self, *args):
        # Isolate fixture setup from the developer's Git identity/configuration.
        env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
        return subprocess.run(['git', '-C', str(self.root), *args], env=env,
                              capture_output=True, text=True, check=True)

    def write(self, name, text):
        (self.root / name).write_text(text, encoding='utf-8')

    def branch(self, name, changes):
        self.git('checkout', '-qb', name, 'main')
        for path, text in changes.items():
            self.write(path, text)
        self.git('add', '.'); self.git('commit', '-qm', name)
        self.git('update-ref', f'refs/remotes/origin/{name}', self.git('rev-parse', 'HEAD').stdout.strip())
        self.git('checkout', 'main')

    def forecast(self, *refs, expected=0, json_output=True):
        result = subprocess.run([sys.executable, str(HELPER), '--repo', str(self.root),
                                 *(['--json'] if json_output else []), *refs], capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return json.loads(result.stdout) if json_output else result.stdout

    def test_conflicting_branches_report_path_and_leave_worktree_index_refs(self):
        self.branch('a', {'plain.txt': 'a\n'}); self.branch('b', {'plain.txt': 'b\n'})
        before = [(self.root / '.git/index').read_bytes(), self.git('show-ref').stdout,
                  (self.root / 'plain.txt').read_bytes(), self.git('status', '--porcelain').stdout]
        value = self.forecast('a', 'b', expected=1)
        self.assertEqual(len(value['pairs']), 3)
        self.assertEqual(value['pairs'][-1]['conflicts'], ['plain.txt'])
        after = [(self.root / '.git/index').read_bytes(), self.git('show-ref').stdout,
                 (self.root / 'plain.txt').read_bytes(), self.git('status', '--porcelain').stdout]
        self.assertEqual(before, after)
        self.assertFalse(self.forecast('missing', expected=2)['ok'])

    def test_clean_pairs_and_one_line_output(self):
        self.branch('a', {'a.txt': 'a\n'}); self.branch('b', {'b.txt': 'b\n'})
        self.assertFalse(self.forecast('a', 'b')['has_conflicts'])
        self.assertEqual(self.forecast('a', 'b', json_output=False), 'No textual conflicts (3 pairs checked).\n')

    def test_union_from_left_commit_without_worktree_attribute_file(self):
        self.branch('a', {'.gitattributes': 'plain.txt merge=union\n', 'plain.txt': 'a\n'})
        self.branch('b', {'.gitattributes': 'plain.txt merge=union\n', 'plain.txt': 'b\n'})
        self.assertFalse((self.root / '.gitattributes').exists())
        value = self.forecast('a', 'b')
        self.assertFalse(value['has_conflicts'])
        left = value['commits']['a']; right = value['commits']['b']
        without = subprocess.run(['git', '-C', str(self.root), 'merge-tree', '--write-tree',
                                  left, right], capture_output=True, text=True)
        self.assertEqual(without.returncode, 1, without.stdout + without.stderr)
        raw = self.git('--attr-source=' + left, 'merge-tree', '--write-tree', left, right)
        tree = raw.stdout.splitlines()[0]
        self.assertEqual(self.git('show', tree + ':plain.txt').stdout, 'a\nb\n')

    def test_default_discovery_excludes_main_symref_and_merged(self):
        self.branch('a', {'a.txt': 'a\n'}); self.branch('b', {'b.txt': 'b\n'})
        self.git('symbolic-ref', 'refs/remotes/origin/HEAD', 'refs/remotes/origin/main')
        self.git('update-ref', 'refs/remotes/origin/old', self.git('rev-parse', 'main').stdout.strip())
        value = self.forecast()
        self.assertEqual(set(value['commits']), {'origin/main', 'refs/remotes/origin/a', 'refs/remotes/origin/b'})
        self.assertEqual(len(value['pairs']), 3)


if __name__ == '__main__':
    unittest.main()
