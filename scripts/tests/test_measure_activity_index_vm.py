"""The optional VM-step helper uses synthetic, in-memory fixtures only."""
import os
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/dev/measure-activity-index-vm.sh'
LANDED_171 = 'c090fa799f1c5d873a36ea07994c936244377437'


@unittest.skipIf(os.name == 'nt' or not shutil.which('sh'), 'requires POSIX sh')
class ActivityIndexVmHelperTests(unittest.TestCase):
    def run_helper(self, *args):
        return subprocess.run(['sh', str(SCRIPT), *args], cwd=ROOT, text=True,
                              capture_output=True, timeout=60, check=False)

    def test_help_and_invalid_option_do_not_open_databases(self):
        help_result = self.run_helper('--help')
        self.assertEqual(help_result.returncode, 0, help_result.stderr)
        self.assertIn('never opens user databases', help_result.stdout)
        self.assertNotEqual(self.run_helper('--unknown').returncode, 0)

    @unittest.skipUnless(shutil.which('sqlite3') and shutil.which('git'), 'requires sqlite3 and git')
    def test_synthetic_fixture_reports_each_statement_twice(self):
        probe = subprocess.run(['sqlite3', ':memory:', "SELECT octet_length('x');"],
                               text=True, capture_output=True, check=False)
        if probe.returncode:
            self.skipTest('SQLite CLI lacks octet_length')
        commit = subprocess.run(['git', 'cat-file', '-e', f'{LANDED_171}:src/storage/current.sql'],
                                cwd=ROOT, capture_output=True, check=False)
        if commit.returncode:
            self.skipTest('landed #171 commit is unavailable')
        result = self.run_helper('--base', LANDED_171)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(LANDED_171, result.stdout)
        self.assertEqual(result.stdout.count('VM-steps:'), 10)
        for label in ('turn_insert', 'turn_running', 'shell_insert', 'shell_spawn', 'shell_exit'):
            self.assertEqual(result.stdout.count(label), 2)


if __name__ == '__main__':
    unittest.main()
