"""Partition selection and cross-job coverage must fail closed on omissions."""
import copy
from pathlib import Path
import sys
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).parents[1]))
from windows_shards import audit, selection, check_layout


class WindowsShardTests(unittest.TestCase):
    config = {'targets': {'lib:shellboy': '1'}, 'modules': {'one': '1', 'two': '2'}}

    def records(self):
        base = {'source_sha': 'abc', 'platform': 'win32', 'test_threads': '4',
                'tests': ['one', 'two'], 'ignored': ['two'], 'failures': []}
        return [dict(base, windows_shard='1', executed_tests=['one'], executed_ignored=[]),
                dict(base, windows_shard='2', executed_tests=['two'], executed_ignored=['two'])]

    def test_layout_includes_visibility_qualified_support_modules(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'tests/integration/main.rs'
            source.parent.mkdir(parents=True)
            source.write_text('mod one;\npub(crate) mod test_watchdog;\npub mod support;\n',
                              encoding='utf-8')
            config = {'targets': {}, 'modules': {'one': '1'}}
            with self.assertRaises(ValueError):
                check_layout(root, config)
            config['modules'].update(test_watchdog='1', support='2')
            check_layout(root, config)

    def test_complete_partition(self):
        self.assertEqual(audit(self.records())['tests'], 2)

    def test_module_selection(self):
        selected, args = selection('test:integration', ['one::a', 'two::b'], '1', self.config)
        self.assertEqual(selected, ['one::a'])
        self.assertEqual(args, ['--skip', 'two::'])

    def test_nonowned_harness_still_inventoried(self):
        self.assertEqual(selection('lib:shellboy', ['case'], '2', self.config), ([], None))

    def test_unknown_harness_and_module_rejected(self):
        for target, names in [('test:new', []), ('test:integration', ['new::case'])]:
            with self.assertRaises(ValueError):
                selection(target, names, '1', self.config)

    def test_substring_collision_rejected(self):
        with self.assertRaises(ValueError):
            selection('test:integration', ['one::nested::two::case'], '1', self.config)

    def test_omission_duplicate_failure_or_different_inventory_rejected(self):
        for key, value in [('executed_tests', []), ('executed_tests', ['one', 'two']),
                           ('executed_ignored', []), ('source_sha', 'other'), ('tests', []),
                           ('ignored', []), ('failures', ['failed']), ('platform', 'darwin'),
                           ('test_threads', '2'), ('windows_shard', '1')]:
            with self.subTest(key=key, value=value):
                records = copy.deepcopy(self.records())
                records[1][key] = value
                with self.assertRaises(ValueError):
                    audit(records)

    def test_both_results_required(self):
        with self.assertRaises(ValueError):
            audit(self.records()[:1])


if __name__ == '__main__':
    unittest.main()
