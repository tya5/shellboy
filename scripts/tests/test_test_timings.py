"""Protect the CI runner against false success and silently missing suites."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))

spec = importlib.util.spec_from_file_location('test_timings', Path(__file__).parents[1] / 'test-timings.py')
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class TimingGateTests(unittest.TestCase):
    def exercise(self, *, failed=False, zero_exact=False, bad_count=False, profile=''):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifacts = root / 'artifacts.jsonl'
            artifacts.write_text('\n'.join(json.dumps({
                'reason': 'compiler-artifact', 'executable': target,
                'profile': {'test': True}, 'target': {'kind': ['test'], 'name': target},
            }) for target in ['integration', 'other']), encoding='utf-8')
            names = ['storage::one', 'storage::ignored']
            calls = []

            def listing(binary, ignored=False):
                return names[1:] if ignored else names

            def execute(command, log):
                calls.append(command)
                exact = '--exact' in command
                passed = 0 if bad_count or (exact and zero_exact) else 1
                is_failure = failed and command[0] == 'integration'
                if is_failure:
                    text = 'test storage::one ... FAILED\nfailures:\n    storage::one\n    unrelated-diagnostic-token\n'
                    text += 'test result: FAILED. 0 passed; 1 failed; 1 ignored;\n'
                else:
                    text = f'test result: ok. {passed} passed; 0 failed; {0 if exact else 1} ignored;\n'
                log.write_text(text, encoding='utf-8')
                return {'command': command, 'seconds': 0.01,
                        'exit_code': 1 if is_failure else 0, 'log': log.name}

            arguments = ['timings', '--output', str(root / 'results'), '--artifacts', str(artifacts)]
            if profile:
                arguments += ['--per-test', profile]
            with patch.object(sys, 'argv', arguments), patch.object(runner, 'listing', listing), \
                    patch.object(runner, 'run', execute), contextlib.redirect_stdout(io.StringIO()):
                code = runner.main()
            return code, json.loads((root / 'results/results.json').read_text()), calls

    def test_added_module_prefix_preserves_identity(self):
        self.assertEqual(runner.identities('test:storage', ['case']),
                         runner.identities('test:integration', ['storage::case']))

    def test_inventory_and_ignored_are_recorded(self):
        code, result, calls = self.exercise()
        self.assertEqual(code, 0)
        self.assertEqual(len(result['tests']), 4)
        self.assertEqual(len(result['ignored']), 2)
        self.assertEqual(len(calls), 2)

    def test_failure_keeps_names_and_runs_later_harness(self):
        code, result, calls = self.exercise(failed=True)
        self.assertEqual(code, 1)
        self.assertEqual(result['failures'][0]['tests'], ['storage::one'])
        self.assertEqual([call[0] for call in calls], ['integration', 'other'])

    def test_unicode_failure_survives_strict_ascii_child_output(self):
        for names in [['suite::plain'], ['suite::plain', 'suite::case_\u65e5\u672c']]:
            with self.subTest(names=names), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                log_text = ('UNICODE-FAIL-START: \u65e5\u672c\u8a9e \U0001f600\n' + 'x' * 17000
                            + '\nUNICODE-FAIL-TAIL: \u65e5\u672c\u8a9e \U0001f600\nfailures:\n'
                            + ''.join(f'    {name}\n' for name in names)
                            + f'test result: FAILED. 0 passed; {len(names)} failed; 0 ignored;\n')
                harness = root / 'unicode_harness.py'
                harness.write_text(
                    'import sys\n'
                    + 'names = ' + repr(names) + '\n'
                    + 'log = ' + repr(log_text) + '\n'
                    + "if '--list' in sys.argv:\n"
                    + "    text = '' if '--ignored' in sys.argv else ''.join(name + ': test\\n' for name in names)\n"
                    + "    sys.stdout.buffer.write(text.encode('utf-8'))\n"
                    + '    sys.exit(0)\n'
                    + "sys.stdout.buffer.write(log.encode('utf-8'))\n"
                    + 'sys.exit(101)\n', encoding='utf-8')
                artifacts = root / 'artifacts.jsonl'
                artifacts.write_text(json.dumps({
                    'reason': 'compiler-artifact', 'executable': str(harness),
                    'profile': {'test': True}, 'target': {'kind': ['test'], 'name': 'integration'},
                }) + '\n', encoding='utf-8')
                output = root / 'results'
                program = f"""
import importlib.util
import subprocess
import sys
from unittest.mock import patch
sys.path.insert(0, {str(Path(runner.__file__).resolve().parent)!r})
spec = importlib.util.spec_from_file_location('test_timings', {str(Path(runner.__file__).resolve())!r})
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
assert sys.stdout.encoding == 'ascii' and sys.stdout.errors == 'strict'
actual_run = subprocess.run
def dispatch(command, **kwargs):
    if command[0] == {str(harness)!r}:
        command = [sys.executable, command[0], *command[1:]]
    return actual_run(command, **kwargs)
sys.argv = ['timings', '--output', {str(output)!r}, '--artifacts', {str(artifacts)!r}]
with patch.object(runner.subprocess, 'run', dispatch):
    code = runner.main()
assert sys.stdout.encoding == 'utf-8' and sys.stdout.errors == 'replace'
assert sys.stderr.encoding == 'utf-8' and sys.stderr.errors == 'replace'
sys.exit(code)
"""
                env = {**os.environ, 'PYTHONIOENCODING': 'ascii:strict', 'PYTHONUTF8': '0',
                       'PYTHONCOERCECLOCALE': '0', 'LC_ALL': 'C'}
                completed = subprocess.run([sys.executable, '-c', program], env=env,
                                           cwd=Path(__file__).resolve().parents[2],
                                           capture_output=True, check=False)
                self.assertEqual(completed.returncode, 1, completed.stderr.decode('utf-8', errors='replace'))
                self.assertTrue((output / 'results.json').is_file(),
                                completed.stderr.decode('utf-8', errors='replace'))
                result = json.loads((output / 'results.json').read_text(encoding='utf-8'))
                self.assertEqual(result['failures'], [{'target': 'test:integration',
                                                      'exit_code': 101, 'tests': sorted(names)}])
                self.assertEqual(len(result['harnesses']), 1)
                self.assertEqual(result['harnesses'][0]['exit_code'], 101)
                self.assertEqual(result['harnesses'][0]['failed'], len(names))
                self.assertEqual(result['tests'], sorted(runner.identities('test:integration', names)))
                raw_log = output / result['harnesses'][0]['log']
                self.assertEqual(raw_log.read_bytes(), log_text.encode('utf-8'))
                console = completed.stdout.decode('utf-8')
                self.assertIn('UNICODE-FAIL-TAIL: \u65e5\u672c\u8a9e \U0001f600', console)
                for name in names:
                    visible = 'Failed test: ' + json.dumps(name, ensure_ascii=True)
                    self.assertIn(visible, console)
                    self.assertTrue(visible.isascii())
                    self.assertLess(console.index(visible), console.index('UNICODE-FAIL-TAIL:'))
                self.assertNotIn('UnicodeEncodeError', completed.stderr.decode('utf-8', errors='replace'))

    def test_zero_exact_selection_is_a_failure(self):
        code, result, _ = self.exercise(zero_exact=True, profile='storage')
        self.assertEqual(code, 1)
        self.assertTrue(any(failure.get('reason') == 'isolated test did not pass exactly once'
                            for failure in result['failures']))

    def test_missing_test_and_unknown_profile_are_failures(self):
        self.assertEqual(self.exercise(bad_count=True)[0], 1)
        self.assertEqual(self.exercise(profile='does-not-exist')[0], 1)


if __name__ == '__main__':
    unittest.main()
