"""Synthetic Rust sources only; the audit never modifies the inspected tree."""
import json
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'dev/language-audit.py'
AUDIT = runpy.run_path(str(SCRIPT))
JA = '\u65e5\u672c\u8a9e'


class LanguageAuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'src').mkdir()

    def source(self, value, name='lib.rs'):
        path = self.root / 'src' / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding='utf-8')
        return path

    def records(self):
        return AUDIT['inventory'](self.root)['records']

    def test_comments_nested_comments_characters_and_lifetimes_are_not_strings(self):
        self.source(f'// "{JA}"\n/* nested /* "{JA}" */ */\n'
                    f"fn f<'a>(x: &'a str) {{ let c = '\u65e5'; let s = \"{JA}\"; }}")
        rows = self.records()
        self.assertEqual(len(rows), 1)
        self.assertEqual((rows[0]['line'], rows[0]['category']), (3, 'c'))

    def test_raw_multiline_escaped_and_unicode_escape_literals(self):
        self.source(f'const A: &str = r###"{JA}\n"# still inside"###;\n'
                    f'const B: &str = "escaped \\\" {JA}";\n'
                    r'const C: &str = "\u{65e5}\u{672c}\u{8a9e}";' + '\n'
                    r'const D: &str = "\\u{65e5}";' + '\n'
                    r'const E: &str = r"\u{65e5}";')
        rows = self.records()
        self.assertEqual(len(rows), 3)
        self.assertEqual([r['kind'] for r in rows], ['raw_string', 'string', 'string'])

    def test_local_only_classifies_japanese_argument_not_english_or_neighbors(self):
        self.source(f'fn f() {{ before("{JA}"); local!(lang, "{JA}", "{JA}"); after("{JA}"); }}')
        self.assertEqual([r['category'] for r in self.records()], ['c', 'a', 'c', 'c'])

    def test_match_arm_is_bounded_and_does_not_exempt_whole_file(self):
        self.source(f'fn f() {{ match lang {{ UiLanguage::Ja => format!("{JA}"), '
                    f'UiLanguage::En => "{JA}".into(), }} after("{JA}"); }}')
        self.assertEqual([r['category'] for r in self.records()], ['a', 'c', 'c'])

    def test_terminal_japanese_arm_without_comma_cannot_hide_neighbor_literals(self):
        self.source(f'fn f() {{ match lang {{ UiLanguage::En => "English", '
                    f'UiLanguage::Ja => "{JA}" }} after("{JA}"); }}')
        self.assertEqual([r['category'] for r in self.records()], ['a', 'c'])

    def test_test_module_and_async_test_items_but_not_production_siblings(self):
        self.source(f'#[cfg(test)] mod tests {{ fn fixture() {{ use_it("{JA}"); }} }}\n'
                    f'#[tokio::test(flavor="multi_thread")] async fn t() {{ use_it("{JA}"); }}\n'
                    f'fn production() {{ use_it("{JA}"); }}')
        self.assertEqual([r['category'] for r in self.records()], ['b', 'b', 'c'])

    def test_external_test_module_requires_explicit_cfg_test_not_filename(self):
        self.source('#[cfg(test)] mod fixture; mod production_tests;')
        self.source(f'fn helper() {{ use_it("{JA}"); }}', 'fixture.rs')
        self.source(f'fn production() {{ use_it("{JA}"); }}', 'production_tests.rs')
        self.assertEqual([r['category'] for r in self.records()], ['b', 'c'])

    def test_review_ids_stable_across_line_shifts_but_not_literal_changes(self):
        path = self.source(f'const A: &str = "{JA}";')
        identity = self.records()[0]['id']
        decisions = {identity: {'category': 'a', 'reason': 'reviewed typed Message Japanese branch',
                                'source_tokens_sha256': self.records()[0]['source_tokens_sha256']}}
        path.write_text('\n' + path.read_text(encoding='utf-8'), encoding='utf-8')
        self.assertEqual(AUDIT['inventory'](self.root, decisions)['records'][0]['category'], 'a')
        path.write_text('const A: &str = "English";', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'stale reviewed decisions'):
            AUDIT['inventory'](self.root, decisions)

    def test_changed_en_predicate_requires_review_even_when_literal_id_is_unchanged(self):
        path = self.source(f'fn f() {{ let en = true; if !en {{ use_it("{JA}"); }} }}')
        row = self.records()[0]
        decision = {row['id']: {'category': 'a', 'reason': 'reviewed Ja branch',
                               'source_tokens_sha256': row['source_tokens_sha256']}}
        path.write_text(path.read_text(encoding='utf-8').replace('!en', 'en'), encoding='utf-8')
        self.assertEqual(self.records()[0]['id'], row['id'])
        with self.assertRaisesRegex(ValueError, 'reviewed source changed'):
            AUDIT['inventory'](self.root, decision)

    def test_pending_gate_and_error_never_modify_inputs(self):
        path = self.source(f'const A: &str = "{JA}";')
        before = path.read_bytes()
        result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(self.root), '--check', '--summary'],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(json.loads(result.stdout)['counts']['c'], 1)
        self.assertEqual(path.read_bytes(), before)
        path.write_text('const BAD: &str = r#"unterminated;', encoding='utf-8')
        result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(self.root)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('unterminated raw string', result.stderr)


if __name__ == '__main__':
    unittest.main()
