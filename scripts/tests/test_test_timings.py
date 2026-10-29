"""Protect the CI runner against false success and silently missing suites."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

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
