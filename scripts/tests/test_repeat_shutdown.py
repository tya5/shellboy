"""The focused runner must retain failures and reject an unregistered test."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    'repeat_shutdown', Path(__file__).parents[1] / 'dev/repeat-shutdown-test.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class RepeatShutdownTests(unittest.TestCase):
    def run_fixture(self, listed=True, exits=(0, 101, 0)):
        with tempfile.TemporaryDirectory() as root:
            source = Path(root)
            output = source / 'evidence'
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == 'cargo':
                    kwargs['stdout'].write(json.dumps({
                        'reason': 'compiler-artifact', 'executable': '/fake/integration',
                        'target': {'name': 'integration'}, 'profile': {'test': True}}) + '\n')
                    return subprocess.CompletedProcess(command, 0)
                return subprocess.CompletedProcess(command, exits[len(calls) - 2])

            def check_output(command, **kwargs):
                return 'exact-sha\n' if command[0] == 'git' else (
                    MODULE.TEST + ': test\n' if listed else '0 tests\n')

            with patch.object(sys, 'argv', ['repeat', '--source', root, '--output', str(output),
                                           '--count', '3']), patch.object(MODULE.subprocess, 'run', run), \
                    patch.object(MODULE.subprocess, 'check_output', check_output), \
                    patch.object(MODULE.platform, 'platform', return_value='synthetic'):
                if not listed:
                    with self.assertRaisesRegex(ValueError, 'not registered'):
                        MODULE.main()
                    self.assertEqual(len(calls), 1)
                    return
                self.assertEqual(MODULE.main(), 1)
            record = json.loads((output / 'results.json').read_text())
            self.assertEqual(record['sha'], 'exact-sha')
            self.assertEqual(record['failures'], 1)
            self.assertEqual([item['exit_code'] for item in record['results']], list(exits))
            self.assertEqual(len(calls), 4)
            self.assertTrue(all('--exact' in call for call in calls[1:]))

    def test_keeps_running_after_a_failure(self):
        self.run_fixture()

    def test_refuses_a_zero_test_selection(self):
        self.run_fixture(listed=False)
