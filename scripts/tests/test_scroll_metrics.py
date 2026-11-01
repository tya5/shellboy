import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('scroll_metrics', Path(__file__).resolve().parents[1] / 'dev/scroll-metrics.py')
metrics = importlib.util.module_from_spec(spec)
spec.loader.exec_module(metrics)


class ScrollMetricsTests(unittest.TestCase):
    def test_masks_literals_and_comments_without_moving_lines(self):
        source = '"{ self.follow = false; }"\n// self.loading = true;\nr#"{ x }"#\n'
        masked = metrics.masked(source)
        self.assertNotIn('{', masked)
        self.assertEqual(masked.count('\n'), source.count('\n'))

    def test_ignores_test_modules_but_keeps_later_production(self):
        source = '#[cfg(test)] mod tests { fn x() { self.follow = false; } }\nimpl View { fn y() { self.follow = true; } }'
        code = metrics.production(source)
        self.assertNotIn('false', code)
        self.assertIn('true', code)

    def test_ignores_test_only_use_and_balances_braces(self):
        self.assertNotIn('use', metrics.production('#[cfg(test)] use super::*;\nfn a() {}'))
        self.assertEqual(metrics.closing('{ { } } rest', 0), 7)
