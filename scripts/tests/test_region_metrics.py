import importlib.util
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("region_metrics", ROOT / "scripts/dev/region-metrics.py")
METRICS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(METRICS)


class RegionMetricsTests(unittest.TestCase):
    def test_nested_signature_groups_and_fixture_classification(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "scripts/dev").mkdir(parents=True)
            (root / "scripts/dev/language-audit.py").write_text((ROOT / "scripts/dev/language-audit.py").read_text())
            (root / "sample.rs").write_text('''
// fn not_a_function(a,b,c) {}
const EXAMPLE: &str = r#"fn hidden(a,b,c) {}"#;
fn empty() {}
fn tuple(a: (i32, i32), b: Vec<(u8,u8)>,) {}
fn generic<'a, T, F: Fn(A,B) -> Option<C>>(a: &'a T, callback: F) {}
impl Foo {
 fn method(&mut self, callback: fn((i32,i32), Vec<u8>) -> Result<A,B>, x: Option<Vec<T>>,) {}
}
#[cfg(test)] mod tests { fn case(a: A,b:B,c:C,d:D) {} }
''')
            report = METRICS.measure(root, ["sample.rs"])[0]
            arities = {f["name"]: f["parameters_including_self"] for f in report["functions"]}
            self.assertEqual(arities, dict(empty=0, tuple=2, generic=2, method=3, case=4))
            self.assertEqual(report["production_function_count"], 4)
            self.assertEqual(report["production_max_parameters"], 3)
            self.assertTrue(report["functions"][-1]["test_only"])

    def test_empty_file_has_no_functions(self):
        with tempfile.TemporaryDirectory() as temporary:
            # The source fixture can be absolute; no repository data is changed.
            source = Path(temporary) / "empty.rs"
            source.write_text("")
            report = METRICS.measure(ROOT, [str(source)])[0]
            self.assertEqual(report["physical_lines"], 0)
            self.assertEqual(report["production_function_count"], 0)
            self.assertEqual(report["production_max_parameters"], 0)
