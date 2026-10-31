"""Disposable Git fixtures; never use the developer's repository as a test target."""
import hashlib
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
        self.root = Path(self.temp.name) / 'space ü repo'
        self.root.mkdir()
        self.env = {k: os.environ[k] for k in ('PATH', 'SystemRoot', 'WINDIR', 'TEMP', 'TMP') if k in os.environ}
        self.env.update(HOME=self.temp.name, USERPROFILE=self.temp.name, GIT_CONFIG_NOSYSTEM='1',
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT='0')
        self.git('init', '-b', 'main')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.write('.gitattributes', 'record.md merge=union\n')
        self.write('record.md', 'base\n')
        self.write('plain.txt', 'base\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'base')
        self.git('update-ref', 'refs/remotes/origin/main', self.git('rev-parse', 'HEAD').stdout.decode().strip())

    def git(self, *args, check=True):
        return subprocess.run(['git', '-C', str(self.root), *args], env=self.env,
                              capture_output=True, check=check, timeout=60)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8')

    def branch(self, name, changes):
        self.git('checkout', '-qb', name, 'main')
        for path, text in changes.items():
            self.write(path, text)
        self.git('add', '.')
        self.git('commit', '-qm', name)
        oid = self.git('rev-parse', 'HEAD').stdout.decode().strip()
        self.git('update-ref', f'refs/remotes/origin/{name}', oid)
        self.git('checkout', 'main')
        return oid

    def forecast(self, *refs, expected=0, extra=(), json_output=True):
        result = subprocess.run([sys.executable, str(HELPER), '--repo', str(self.root),
                                 *(['--json'] if json_output else []), *extra, *refs],
                                env=self.env, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return json.loads(result.stdout) if json_output else result.stdout.decode()

    def fingerprint(self):
        return {str(p.relative_to(self.root)): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in self.root.rglob('*') if p.is_file()}

    def test_union_only_is_clean_and_keeps_real_repository_byte_identical(self):
        self.branch('a', {'record.md': 'a\n'})
        self.branch('b', {'record.md': 'b\n'})
        # Dirty work/index, private attributes and reflogs must not be changed or trusted.
        self.write('.gitattributes', 'record.md -merge\n')
        self.write('.git/info/attributes', 'record.md -merge\n')
        self.write('plain.txt', 'staged unrelated work\n')
        self.git('add', 'plain.txt')
        before = self.fingerprint()
        value = self.forecast('a', 'b')
        self.assertFalse(value['has_conflicts'])
        self.assertEqual(len(value['pairs']), 3)
        self.assertEqual(before, self.fingerprint(), 'helper wrote source files/index/refs/objects')

    def test_conflict_and_clean_pairs_include_main_and_every_pair(self):
        self.branch('a', {'plain.txt': 'a\n'})
        self.branch('b', {'plain.txt': 'b\n'})
        self.branch('c', {'other.txt': 'c\n'})
        value = self.forecast('a', 'b', 'c', expected=1)
        self.assertEqual(len(value['pairs']), 6)
        bad = [p for p in value['pairs'] if p['conflicts']]
        self.assertEqual([(p['left']['ref'], p['right']['ref'], p['conflicts']) for p in bad],
                         [('a', 'b', ['plain.txt'])])
        self.assertIn('plain.txt', self.forecast('a', 'b', expected=1, json_output=False))
        self.assertEqual(self.forecast('a', 'c', json_output=False), 'No textual conflicts (3 pairs checked).\n')

    def test_default_discovery_excludes_main_symbolic_and_merged(self):
        self.branch('a', {'a.txt': 'a\n'})
        self.branch('b', {'b.txt': 'b\n'})
        self.git('symbolic-ref', 'refs/remotes/origin/HEAD', 'refs/remotes/origin/main')
        self.git('update-ref', 'refs/remotes/origin/old', self.git('rev-parse', 'main').stdout.decode().strip())
        value = self.forecast()
        self.assertEqual([p['ref'] for p in value['branches']], ['refs/remotes/origin/a', 'refs/remotes/origin/b'])
        self.assertEqual(len(value['pairs']), 3)
        self.forecast(expected=2, extra=('--max-branches', '1'))

    def test_no_branches_reports_one_line(self):
        self.assertEqual(self.forecast(json_output=False), 'No textual conflicts (0 pairs checked).\n')

    @unittest.skipIf(os.name == 'nt', 'Windows filenames cannot contain tabs/newlines')
    def test_nul_output_preserves_space_unicode_tab_and_newline_paths(self):
        path = 'folder ü/sp ace\tline\nfile.txt'
        self.write(path, 'base\n'); self.git('add', '.'); self.git('commit', '-qm', 'path')
        self.git('update-ref', 'refs/remotes/origin/main', self.git('rev-parse', 'HEAD').stdout.decode().strip())
        self.branch('a', {path: 'a\n'}); self.branch('b', {path: 'b\n'})
        self.assertEqual(self.forecast('a', 'b', expected=1)['pairs'][-1]['conflicts'], [path])

    def test_nested_union_and_directional_committed_attributes(self):
        self.write('dir/.gitattributes', 'record.md merge=union\n')
        self.write('dir/record.md', 'base\n'); self.git('add', '.'); self.git('commit', '-qm', 'nested')
        self.git('update-ref', 'refs/remotes/origin/main', self.git('rev-parse', 'HEAD').stdout.decode().strip())
        self.branch('a', {'dir/record.md': 'a\n'})
        self.branch('b', {'dir/record.md': 'b\n'})
        self.assertFalse(self.forecast('a', 'b')['has_conflicts'])

    def test_first_side_attribute_change_is_used_for_branch_pair(self):
        self.branch('a', {'.gitattributes': 'record.md -merge\n', 'record.md': 'a\n'})
        self.branch('b', {'record.md': 'b\n'})
        value = self.forecast('a', 'b', expected=1)
        self.assertEqual(value['pairs'][-1]['conflicts'], ['record.md'])

    def test_partial_and_custom_local_configuration_are_not_loaded(self):
        self.git('config', 'remote.origin.promisor', 'true')
        self.assertIn('Partial/promisor', self.forecast(expected=2)['error'])
        self.git('config', '--unset', 'remote.origin.promisor')
        self.git('config', 'merge.hostile.driver', 'should-never-execute')
        self.assertIn('configuration', self.forecast(expected=2)['error'])

    def test_invalid_ref_unrelated_and_custom_driver_fail_closed(self):
        self.forecast('does-not-exist', expected=2)
        self.branch('custom', {'.gitattributes': '* merge=external\n', 'plain.txt': 'custom\n'})
        value = self.forecast('custom', expected=2)
        self.assertIn('Custom merge driver', value['error'])
        self.git('checkout', '--orphan', 'unrelated')
        self.git('rm', '-rf', '.')
        self.write('different', 'x\n'); self.git('add', '.'); self.git('commit', '-qm', 'unrelated')
        self.git('checkout', 'main')
        self.forecast('unrelated', expected=2)

    def test_linked_worktree_uses_common_objects_without_writing_them(self):
        self.branch('a', {'record.md': 'a\n'}); self.branch('b', {'record.md': 'b\n'})
        linked = Path(self.temp.name) / 'linked'
        self.git('worktree', 'add', '--detach', str(linked), 'main')
        before = self.fingerprint()
        value = self.forecast('a', 'b', extra=('--repo', str(linked)))
        self.assertFalse(value['has_conflicts'])
        self.assertEqual(before, self.fingerprint())


if __name__ == '__main__':
    unittest.main()
