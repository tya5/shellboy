"""Fail closed before launching an unsafe terminal fixture."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('fixture_safety', Path(__file__).resolve().parents[1] / 'fixture_safety.py')
safety = importlib.util.module_from_spec(spec)
spec.loader.exec_module(safety)


class FixtureSafetyTests(unittest.TestCase):
    def test_explicit_osc52_required_and_never_rewrites_config(self):
        with tempfile.TemporaryDirectory() as scratch:
            config = Path(scratch) / 'config.toml'
            for text in ['', '[ui]\ncopy_backend="auto"\n', '[ui]\ncopy_backend="os"\n']:
                config.write_text(text)
                with self.assertRaises(ValueError):
                    safety.require_osc52(config)
                self.assertEqual(config.read_text(), text)
            config.write_text('[ui]\ncopy_backend="osc52"\n')
            safety.require_osc52(config)

    def test_gui_cannot_forward_copy_or_image_checks_to_host_clipboard(self):
        for command in ['/copy node', '/image', '/paste-image']:
            with self.assertRaises(ValueError):
                safety.reject_gui_clipboard_input([command], [], '')
        for key in ['c', 'C', '\x19', '\x03', '/copy node\r', '\x1b[99u']:
            with self.assertRaises(ValueError):
                safety.reject_gui_clipboard_input([], [key], '')
        safety.reject_gui_clipboard_input(['/switch visual-composer'], ['\x1b[C', '\x1b[A', '/at'], '')
