"""Portable frame contracts and Unix-only disposable child isolation."""
import importlib.util
import os
from pathlib import Path
import sys
import unittest

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
try:
    spec = importlib.util.spec_from_file_location('pty_capture', DEV / 'pty_capture.py')
    capture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(capture)
finally:
    sys.path.remove(str(DEV))


class FrameTests(unittest.TestCase):
    def test_only_complete_frames_publish_and_utf8_can_split(self):
        frames = capture.Frames(12, 3)
        self.assertEqual(list(frames.feed(b'\x1b[?25l')), ['            \n            \n            '])
        text = '\u65e5\u672c'.encode()
        for byte in text:
            self.assertEqual(list(frames.feed(bytes([byte]))), [])
        self.assertEqual(list(frames.feed(b'\x1b[?25h\x1b[2;3H'))[0].splitlines()[0], '\u65e5\u672c        ')
        self.assertEqual(frames.frame_count, 2)

    def test_output_is_bounded(self):
        frames = capture.Frames(2, 2)
        with self.assertRaises(ValueError):
            list(frames.feed(b'x' * (capture.OUTPUT_LIMIT + 1)))


@unittest.skipUnless(os.name == 'posix', 'Live Unix PTYs are unavailable on this OS')
class LivePtyTests(unittest.TestCase):
    def test_scratch_paths_config_environment_and_normal_cleanup(self):
        # This synthetic child cannot access clipboard APIs; it only asserts
        # the arguments/environment supplied by the transport and emits VT.
        source = r'''
import json, os, pathlib, sys, termios, tty
args=dict(zip(sys.argv[1::2],sys.argv[2::2])); root=pathlib.Path.cwd()
assert pathlib.Path(args['--config']) == root/'config.toml'
assert pathlib.Path(args['--data-dir']) == root/'data'
assert 'copy_backend="osc52"' in (root/'config.toml').read_text()
assert pathlib.Path(os.environ['HOME']).is_relative_to(root)
assert not any(k.startswith('SHELLBOY_') for k in os.environ)
assert 'SYNTHETIC_API_KEY' not in os.environ
assert 'SYNTHETIC_PRIVATE_VALUE' not in os.environ
assert os.environ['NO_COLOR']=='1'
tty.setraw(sys.stdin.fileno())
sys.stdout.write('\x1b[2J\x1b[HREADY\x1b[?25h\x1b[2;1H');sys.stdout.flush()
while sys.stdin.buffer.read(1) != b'\x11': pass
'''
        from unittest.mock import patch
        with patch.dict(os.environ, {'SHELLBOY_THREAD_ID': 'must-not-leak', 'SYNTHETIC_API_KEY': 'not-a-secret', 'SYNTHETIC_PRIVATE_VALUE': 'not-a-secret'}):
            terminal = capture.Terminal([sys.executable, '-c', source], cols=20, rows=4, no_color=True)
        root = terminal.root
        try:
            self.assertIn('READY', terminal.expect(lambda text: 'READY' in text, 'ready'))
            self.assertTrue(root.exists())
        finally:
            terminal.close()
        self.assertEqual(terminal.process.returncode, 0)
        self.assertFalse(root.exists())

class ConfigAndCliTests(unittest.TestCase):
    def test_cli_replay_and_refuses_overwriting_output(self):
        import json
        import subprocess
        import tempfile
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            raw = root / 'capture.raw'
            raw.write_bytes(b'\x1b[HHELLO\x1b[?25h\x1b[2;1H')
            target = root / 'result.json'
            command = [sys.executable, str(DEV / 'pty-capture.py'), '--replay', str(raw),
                       '--cols', '12', '--rows', '3', '--until', 'HELLO', '--out', str(target)]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(result.stdout)['screens'], 1)
            before = target.read_bytes()
            self.assertIn('HELLO', json.loads(before)['screens'][0]['text'])
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(target.read_bytes(), before)

    def test_cli_rejects_clipboard_image_commands_raw_keys_and_bad_actions_before_spawn(self):
        import json
        import subprocess
        cases = [{'command': '/image', 'until': 'x'}, {'key': 'f7', 'until': 'x'}, {'key': 'f8', 'until': 'x'}, {'key': '\x1b', 'until': 'x'},
                 {'paste': '\x1b[201~', 'until': 'x'}, {'key': 'copy'},
                 {'key': 'copy', 'command': '/copy', 'until': 'x'}]
        for step in cases:
            result = subprocess.run([sys.executable, str(DEV / 'pty-capture.py'),
                '--binary', '/must-not-spawn', '--steps', json.dumps([step])], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertFalse(json.loads(result.stdout)['ok'])
            self.assertNotIn('must-not-spawn', result.stdout)

    def test_config_requires_osc52_loopback_and_no_auth(self):
        import tempfile
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for config in ['', '[ui]\ncopy_backend="auto"\n',
                           '[ui]\ncopy_backend="osc52"\n[connections.x]\nbase_url="https://example.invalid/v1"\n',
                           '[ui]\ncopy_backend="osc52"\n[connections.x]\nbase_url="http://127.0.0.1:9/v1"\napi_key_env="ANY_KEY"\n']:
                (root / 'config.toml').write_text(config)
                with self.assertRaises(ValueError):
                    capture.check_config(root)
