"""Benchmark correctness/safety gates, never memory or latency thresholds."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, build_opener, ProxyHandler

urlopen = build_opener(ProxyHandler({})).open

SCRIPT = Path(__file__).resolve().parents[1] / 'bench/footprint.py'
spec = importlib.util.spec_from_file_location('footprint', SCRIPT)
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


def instruction(index):
    return {'role': 'user', 'content': [{'type': 'input_text', 'text': bench.PROMPTS[index]}]}


class ModelTests(unittest.TestCase):
    def test_exactly_four_tools_then_done_and_history_does_not_repeat_tools(self):
        history = []
        for index in range(5):
            history.append(instruction(index))
            round_index, tool, output = bench.plan({'input': history})
            self.assertEqual(round_index, index)
            self.assertEqual(tool, index < 4)
            if tool:
                call = output[0]
                self.assertEqual(json.loads(call['arguments'])['command'], bench.COMMANDS[index])
                history.extend([call, {'type': 'function_call_output', 'call_id': call['call_id'], 'output': 'fixture'}])
                self.assertFalse(bench.plan({'input': history})[1])
            else:
                self.assertEqual(output[0]['content'][0]['text'], 'done')

    def test_unknown_instruction_is_rejected(self):
        with self.assertRaises(ValueError):
            bench.plan({'input': [{'role': 'user', 'content': 'not a benchmark instruction'}]})

    def test_loopback_sse_caps_tool_calls_even_if_client_repeats(self):
        server = bench.FakeModel(2)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            self.assertEqual(server.server_address[0], '127.0.0.1')
            server.allowed_threads.add('test-thread')
            context = {'role': 'developer', 'content': [{'type': 'input_text',
                       'text': 'Shellboy context v1: thread=test-thread run=test-run'}]}
            payload = json.dumps({'input': [instruction(0), context]}).encode()
            request = Request(server.url + '/responses', data=payload, headers={'Content-Type': 'application/json'})
            with urlopen(request, timeout=5) as response:
                self.assertEqual(response.headers['Content-Type'], 'text/event-stream')
                self.assertIn(b'function_call', response.read())
            with self.assertRaises(HTTPError) as error:
                urlopen(request, timeout=5)
            self.assertEqual(error.exception.code, 400)
            self.assertEqual(sum(server.tools), 1)
            self.assertTrue(server.errors)
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)


class PssTests(unittest.TestCase):
    def proc(self, root, pid, parent, pss=None):
        directory = root / str(pid)
        directory.mkdir()
        # state is stat field3; starttime is field22, index19 after comm.
        fields = ['S', str(parent)] + ['0'] * 17 + [str(pid * 100)]
        (directory / 'stat').write_text(f'{pid} (name with ) parentheses) ' + ' '.join(fields))
        if pss is not None:
            (directory / 'smaps_rollup').write_text(f'Rss: 999999 kB\nPss: {pss} kB\nPss_Dirty: 999 kB\n')

    def test_sums_only_owner_and_transitive_children_using_pss_not_rss(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            self.proc(root, 10, 1, 40)
            self.proc(root, 20, 10, 15)
            self.proc(root, 30, 20, 7)
            self.proc(root, 40, 1, 10000)
            result = bench.sample_pss(10, root)
            self.assertEqual(result['pss_kib'], 62)
            self.assertEqual([row['pid'] for row in result['processes']], [10, 20, 30])
            self.assertEqual(result['processes'][0]['starttime_ticks'], 1000)

    def test_missing_owner_or_missing_pss_fails_not_zero(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            with self.assertRaises(RuntimeError):
                bench.sample_pss(10, root)
            self.proc(root, 10, 1)
            with self.assertRaises(RuntimeError):
                bench.sample_pss(10, root)

    def test_pid_reuse_and_churn_never_produce_a_complete_sample(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            self.proc(root, 10, 1, 40)
            sequence = [{10: (1, n)} for n in range(6)]
            with patch.object(bench, 'process_tree', side_effect=sequence):
                with self.assertRaisesRegex(RuntimeError, 'changed'):
                    bench.sample_pss(10, root)


class SafetyTests(unittest.TestCase):
    def test_only_allowlisted_environment_and_disposable_config(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            with patch.dict(os.environ, {'OPENAI_API_KEY': 'synthetic-secret', 'HTTPS_PROXY': 'private-proxy',
                                         'SHELLBOY_THREAD_ID': 'user-thread', 'AWS_PROFILE': 'private'}):
                env = bench.isolated_env(root)
            for key in ['OPENAI_API_KEY', 'HTTPS_PROXY', 'SHELLBOY_THREAD_ID', 'AWS_PROFILE']:
                self.assertNotIn(key, env)
            self.assertEqual(env['HOME'], str(root / 'home'))
            config = bench.configure(root, 'http://127.0.0.1:9/v1').read_text()
            self.assertIn('response_max_retries=0', config)
            self.assertNotIn('api_key', config)
            self.assertIn('copy_backend="osc52"', config)

    def test_default_measurement_matrix_and_no_real_provider_mode(self):
        self.assertEqual(len(bench.PROMPTS), 5)
        self.assertEqual(len(bench.COMMANDS), 4)
        source = SCRIPT.read_text()
        self.assertIn('default=[1, 5, 10, 20]', source)
        self.assertNotIn('--provider', source)


class AdmissionTests(unittest.TestCase):
    def rejected(self, may_apply=False):
        return bench.subprocess.CompletedProcess([], 1, json.dumps({
            'ok': False, 'operation_id': 'owner:fixed', 'error': {
                'code': 'invalid_state', 'details': {'phase': 'admission',
                                                  'request_may_have_been_applied': may_apply}}}), '')

    def test_only_known_rejection_is_retried_with_same_operation(self):
        success = bench.subprocess.CompletedProcess([], 0, '{"ok":true,"result":{"accepted":true}}', '')
        with patch.object(bench.subprocess, 'run', side_effect=[self.rejected(), success]) as run:
            with patch.object(bench.time, 'sleep'):
                self.assertEqual(bench.cli(Path('binary'), Path('/fixture'), {}, 'message', 'send'), {'accepted': True})
        self.assertIn('--operation-id', run.call_args_list[1].args[0])
        self.assertIn('owner:fixed', run.call_args_list[1].args[0])

    def test_uncertain_failure_is_never_reissued(self):
        with patch.object(bench.subprocess, 'run', return_value=self.rejected(True)) as run:
            with self.assertRaises(RuntimeError):
                bench.cli(Path('binary'), Path('/fixture'), {}, 'message', 'send')
        self.assertEqual(run.call_count, 1)

    def test_admission_retries_are_finite(self):
        with patch.object(bench.subprocess, 'run', return_value=self.rejected()) as run:
            with patch.object(bench.time, 'sleep'):
                with self.assertRaisesRegex(RuntimeError, 'exhausted'):
                    bench.cli(Path('binary'), Path('/fixture'), {}, 'message', 'send')
        self.assertEqual(run.call_count, 30)


class OutputTests(unittest.TestCase):
    def test_existing_output_is_never_overwritten_or_measured(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            target = root / 'keep.json'
            target.write_text('keep this')
            arguments = ['footprint.py', '--binary', str(SCRIPT), '--out', str(target)]
            with patch.object(bench.sys, 'platform', 'linux'), patch.object(bench.sys, 'argv', arguments):
                with patch.object(bench, 'measure_startup') as measure:
                    with self.assertRaises(SystemExit):
                        bench.main()
            measure.assert_not_called()
            self.assertEqual(target.read_text(), 'keep this')


if __name__ == '__main__':
    unittest.main()
