"""Regression guard for test-scoped deadline detection and narrow allowances."""
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from watchdog_policy import check, findings


class WatchdogPolicyTests(unittest.TestCase):
    def fixture(self, files):
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        for name, body in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body)
        return root

    def test_short_or_long_raw_guards_fail_and_production_is_not_scanned(self):
        root = self.fixture({
            'tests/native.rs': 'async fn readiness() { timeout(Duration::from_millis(5), work()).await; }',
            'src/lib.rs': 'async fn product() { timeout(deadline, work()).await; }',
        })
        self.assertEqual(len(check(root, [])), 1)
        (root / 'tests/native.rs').write_text('async fn readiness() { timeout(Duration::from_secs(60), work()).await; }')
        self.assertEqual(len(check(root, [])), 1)

    def test_nested_inline_and_external_platform_test_modules_are_detected(self):
        root = self.fixture({
            'src/lib.rs': '#[cfg(test)] mod tests { async fn example() { tokio::time::timeout(x, f()).await; } }\n#[cfg(all(test, unix))] mod native;',
            'src/native.rs': 'async fn child() { timeout(x, f()).await; }',
        })
        self.assertEqual(len(check(root, [])), 2)

    def test_aliases_and_path_selected_test_modules_are_not_a_bypass(self):
        root = self.fixture({
            'src/lib.rs': '#[cfg(test)] #[path="hidden.rs"] mod cases;',
            'src/hidden.rs': 'use tokio::time::timeout as tiny; async fn f() { tiny(budget,work()).await; }',
        })
        self.assertEqual(len(check(root, [])), 1)

    def test_cfg_arguments_and_parameters_do_not_capture_production_bodies(self):
        root = self.fixture({'src/lib.rs': """
async fn production(#[cfg(test)] value: Option<()>) {
 let deadline = Instant::now() + Duration::from_secs(5);
 timeout(product_budget, network()).await;
}
fn caller() { production(#[cfg(test)] None,); }
#[cfg(test)] async fn test_only() -> Result<(), E> { timeout(x, f()).await; }
"""})
        self.assertEqual(len(check(root, [])), 1)

    def test_comments_raw_strings_and_character_literals_do_not_create_findings(self):
        root = self.fixture({'tests/data.rs': '''
// timeout(x, f());
/* outer /* timeout(x,f()) */ nested */
fn data() { let raw = r###"timeout(x,f()) { }"###; let c = '}'; let text = "timeout(x,f())"; }
'''})
        self.assertEqual(check(root, []), [])

    def test_deferred_allowance_has_a_reason_is_exact_and_cannot_grow(self):
        body = 'async fn deferred() { timeout(x, f()).await; }'
        root = self.fixture({'tests/deferred.rs': body})
        value = next(findings(root))
        allowance = {k: value[k] for k in ('file', 'rule', 'fingerprint')}
        allowance.update(count=1, reason='Deferred until #41 integration; migrate to shared guard')
        self.assertEqual(check(root, [allowance]), [])
        (root / 'tests/deferred.rs').write_text(body + '\n' + body)
        self.assertTrue(check(root, [allowance]))
        (root / 'tests/deferred.rs').write_text('')
        self.assertIn('stale allowance', check(root, [allowance])[0])
        allowance['reason'] = ''
        with self.assertRaises(ValueError):
            check(root, [allowance])

    def test_manual_wall_deadlines_and_latency_assertions_fail(self):
        root = self.fixture({'tests/manual.rs': '''fn waits() {
let deadline = std::time::Instant::now() + Duration::from_secs(2);
assert!(started.elapsed() < Duration::from_millis(10));
stream.set_read_timeout(Some(Duration::from_secs(2)));
}'''})
        self.assertEqual({v['rule'] for v in findings(root)},
                         {'wall-deadline', 'elapsed-comparison', 'raw-timeout'})


if __name__ == '__main__':
    unittest.main()
