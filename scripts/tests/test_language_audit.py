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

    def test_scope_ignores_sibling_functions_and_constants_but_not_same_item(self):
        path = self.source(f'fn reviewed() {{ let en = true; if !en {{ use_it("{JA}"); }} }}\n'
                           'fn other() { let value = 1; }\nconst OTHER: usize = 1;')
        row = self.records()[0]
        decision = {row['id']: {'category': 'a', 'reason': 'reviewed Ja branch',
                               'source_tokens_sha256': row['source_tokens_sha256']}}
        value = path.read_text(encoding='utf-8').replace('value = 1', 'value = 2').replace('usize = 1', 'usize = 3')
        path.write_text(value, encoding='utf-8')
        self.assertEqual(AUDIT['inventory'](self.root, decision)['records'][0]['category'], 'a')
        path.write_text(value.replace('!en', 'en'), encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'reviewed source changed'):
            AUDIT['inventory'](self.root, decision)

    def test_const_scope_attributes_and_enclosing_function_are_hashed(self):
        path = self.source(f'#[cfg(feature = "a")] const FIRST: &str = "{JA}";\n'
                           'const SECOND: &str = "separate";\n'
                           f'fn owner() {{ const INNER: &str = "{JA}x"; if true {{ use_it(INNER); }} }}')
        rows = self.records()
        decisions = {r['id']: {'category': 'a', 'reason': 'reviewed',
                                'source_tokens_sha256': r['source_tokens_sha256']} for r in rows}
        original = path.read_text(encoding='utf-8')
        path.write_text(original.replace('"separate"', '"changed"'), encoding='utf-8')
        AUDIT['inventory'](self.root, decisions)
        for changed in [original.replace('feature = "a"', 'feature = "b"'),
                        original.replace('if true', 'if false')]:
            path.write_text(changed, encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'reviewed source changed'):
                AUDIT['inventory'](self.root, decisions)

    def test_unknown_item_falls_back_to_file_and_generic_const_does_not_swallow_siblings(self):
        path = self.source(f'fn f<const N: usize>() {{ use_it("{JA}"); }}\nfn g() {{ let x = 1; }}')
        row = self.records()[0]
        decision = {row['id']: {'category': 'c', 'reason': 'pending review',
                               'source_tokens_sha256': row['source_tokens_sha256']}}
        path.write_text(path.read_text(encoding='utf-8').replace('let x = 1', 'let x = 2'), encoding='utf-8')
        AUDIT['inventory'](self.root, decision)
        path = self.source(f'macro_rules! x {{ () => {{ "{JA}" }} }}\nfn sibling() {{ let x = 1; }}')
        row = self.records()[0]
        decision = {row['id']: {'category': 'c', 'reason': 'pending review',
                               'source_tokens_sha256': row['source_tokens_sha256']}}
        path.write_text(path.read_text(encoding='utf-8').replace('let x = 1', 'let x = 2'), encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'reviewed source changed'):
            AUDIT['inventory'](self.root, decision)

    def test_consistency_command_allows_c_but_rejects_stale_review(self):
        path = self.source(f'fn f() {{ let en = true; if !en {{ use_it("{JA}"); }} }}\n'
                           f'fn pending() {{ use_it("{JA} pending"); }}')
        row = self.records()[0]
        decisions = self.root / 'decisions.json'
        decisions.write_text(json.dumps({row['id']: {'category': 'a', 'reason': 'reviewed',
                                                     'source_tokens_sha256': row['source_tokens_sha256']}}))
        command = [sys.executable, str(SCRIPT), '--repo', str(self.root), '--classifications', str(decisions), '--summary']
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreater(json.loads(result.stdout)['counts']['c'], 0)
        path.write_text(path.read_text(encoding='utf-8').replace('!en', 'en'), encoding='utf-8')
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('reviewed source changed', result.stderr)

    def test_protected_tokens_require_both_evidence_fields_and_remain_distinct(self):
        self.source(f'fn compare(s: &str) -> bool {{ s == "{JA}" }}')
        row = self.records()[0]
        entry = {'category': 'protected', 'reason': 'canonical equality operand, not display copy',
                 'source_tokens_sha256': row['source_tokens_sha256']}
        for evidence in [{}, {'canonical_evidence': 'src/lib.rs:compare exact comparison'}]:
            with self.assertRaisesRegex(ValueError, 'canonical and UI projection evidence'):
                AUDIT['inventory'](self.root, {row['id']: dict(entry, **evidence)})
        entry.update(canonical_evidence='src/lib.rs:compare exact comparison',
                     projection_evidence='synthetic renderer consumes enum without changing operand')
        report = AUDIT['inventory'](self.root, {row['id']: entry})
        self.assertEqual(report['counts'], {'protected': 1})
        self.assertEqual(report['records'][0]['canonical_evidence'], entry['canonical_evidence'])
        self.assertEqual(report['records'][0]['projection_evidence'], entry['projection_evidence'])

    def test_table_is_reproducible_and_ownership_is_not_a_classification_exemption(self):
        self.source(f'const A: &str = "{JA}";')
        owner = self.root / 'owners.json'
        owner.write_text(json.dumps({'src/lib.rs': 'W6'}))
        command = [sys.executable, str(SCRIPT), '--repo', str(self.root), '--table',
                   '--ownership', str(owner), '--check']
        first = subprocess.run(command, capture_output=True, text=True)
        second = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(first.returncode, 1)
        self.assertEqual(first.stdout, second.stdout)
        self.assertIn('| `src/lib.rs` | 0 | 0 | 0 | 1 | W6 |', first.stdout)

    def test_malformed_reviewed_input_is_a_controlled_input_error(self):
        self.source(f'const A: &str = "{JA}";')
        row = self.records()[0]
        for invalid in [[], [row['id']], 7]:
            with self.assertRaisesRegex(ValueError, 'JSON object'):
                AUDIT['inventory'](self.root, invalid)
        for entry in [None, 7, {'category': 'a', 'reason': 7}]:
            with self.assertRaisesRegex(ValueError, 'invalid reviewed decision'):
                AUDIT['inventory'](self.root, {row['id']: entry})

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
