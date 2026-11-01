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

    def test_counts_enum_writes_and_transition_calls_without_other_ui_loading(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'src/ui').mkdir(parents=True)
            (root / 'src/ui/navigation.rs').write_text('''
struct Navigation { read: ReadState, scroll: VisualScroll, }
impl Navigation { fn start(&mut self) {
 self.read = ReadState::Idle;
 self.set_loading(true);
 self.model_picker.loading = true;
} }
''')
            result = metrics.measure(root)
            self.assertEqual(result['counts']['read'], 1)
            self.assertEqual(result['counts']['loading'], 0)
            self.assertEqual(result['transition_calls'], 1)
            self.assertEqual(result['writes_and_transitions'], 2)

    def test_masks_cfg_fields_variants_and_match_arms_without_losing_production(self):
        source = '''enum Event { #[cfg(test)] Seed(Option<(u64, String)>), Real }
struct Effects { #[cfg(test)] ticket: Option<String>, delta: isize }
fn run(e: Event) { match e { #[cfg(test)] Event::Seed(x) => (x, 0), Event::Real => 1, }; }
fn after() { self.read = state; }
'''
        code = metrics.production(source)
        self.assertNotIn('Seed', code)
        self.assertNotIn('ticket', code)
        self.assertIn('delta', code)
        self.assertIn('Event::Real', code)
        self.assertIn('self.read = state', code)
