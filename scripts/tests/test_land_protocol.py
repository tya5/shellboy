"""The landing protocol bump runs once per batch, only for wire changes, and is idempotent."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/land-protocol.py'
RUST = 'pub const PROTOCOL_VERSION: u32 = 11;\npub const OTHER: u32 = 11;\n'
SCHEMA = '''{
  "$id": "https://shellboy.dev/schema/protocol-v11.json",
  "definitions": {
    "Accepted": {
      "title": "Accepted v11",
      "type": "object"
    }
  },
  "protocol_version": 11,
  "title": "Shellboy local RPC protocol v11"
}
'''
DOC = '# Protocol\n\n## Protocol 11 (#317)\n\nOld section.\n'


class LandProtocolTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.git('init', '-q', '-b', 'main')
        for key, value in (('user.name', 'Test'), ('user.email', 'test@example.invalid'),
                           ('commit.gpgsign', 'false'), ('core.autocrlf', 'false')):
            self.git('config', key, value)
        self.write('src/protocol.rs', RUST)
        self.write('docs/reference/protocol.schema.json', SCHEMA)
        self.write('docs/design/protocol.md', DOC)
        self.commit('base')
        self.git('switch', '-q', '-c', 'batch')

    def tearDown(self):
        self.directory.cleanup()

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.root, check=True,
                              capture_output=True, text=True).stdout

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(text.encode('utf-8'))

    def read(self, name):
        return (self.root / name).read_bytes().decode('utf-8')

    def commit(self, message):
        self.git('add', '-A')
        self.git('commit', '-q', '-m', message)

    def land(self):
        return subprocess.run([sys.executable, str(SCRIPT), 'main'], cwd=self.root,
                              capture_output=True, text=True, timeout=30)

    def head(self):
        return self.git('rev-parse', 'HEAD').strip()

    def subject(self):
        return self.git('log', '-1', '--format=%s').strip()

    def test_no_wire_change_makes_no_commit(self):
        self.write('docs/design/protocol.md', DOC + '\nProse only.\n')
        self.commit('prose')
        before = self.head()
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.head(), before)
        self.assertIn('= 11;', self.read('src/protocol.rs'))

    def test_schema_definition_change_bumps_once(self):
        changed = SCHEMA.replace('"type": "object"', '"type": "object",\n      "x": 1')
        self.write('docs/reference/protocol.schema.json', changed)
        self.commit('patch')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.subject(), 'Protocol 12')
        self.assertEqual(self.read('src/protocol.rs'), RUST.replace('VERSION: u32 = 11', 'VERSION: u32 = 12'))
        expected = (changed.replace('protocol-v11.json', 'protocol-v12.json')
                    .replace('"protocol_version": 11', '"protocol_version": 12')
                    .replace('protocol v11"', 'protocol v12"'))
        self.assertEqual(self.read('docs/reference/protocol.schema.json'), expected)
        self.assertIn('"title": "Accepted v11"', expected)
        self.assertEqual(self.git('status', '--porcelain'), '')
        self.assertEqual(self.git('rev-list', '--count', 'main..HEAD').strip(), '2')

    def test_next_files_merge_into_one_version_file(self):
        self.write('docs/design/protocol-versions/next-30.md',
                   '# Protocol next (#30)\n\nThirty; builds on [twelve](next-12.md#detail).\n\n'
                   '## Detail thirty\n\nMore thirty.\n')
        self.write('docs/design/protocol-versions/next-12.md',
                   '# Protocol next (#12)\n\nTwelve, see [base](../protocol.md#protocol).\n\n'
                   '## Detail\n\n```\n# not a heading\n```\n')
        self.write('docs/guides/use.md', 'See [12](../design/protocol-versions/next-12.md#protocol-next-12), '
                   '[30](../design/protocol-versions/next-30.md#detail-thirty) and '
                   '[folder](../design/protocol-versions/).\n')
        self.commit('two patches')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.subject(), 'Protocol 12: #12, #30')
        self.assertEqual(self.read('docs/design/protocol-versions/12.md'),
                         '# Protocol 12 (#12, #30)\n\n'
                         'Twelve, see [base](../protocol.md#protocol).\n\n'
                         '## Detail\n\n```\n# not a heading\n```\n\n'
                         'Thirty; builds on [twelve](#detail).\n\n'
                         '## Detail thirty\n\nMore thirty.\n')
        self.assertFalse((self.root / 'docs/design/protocol-versions/next-12.md').exists())
        self.assertFalse((self.root / 'docs/design/protocol-versions/next-30.md').exists())
        self.assertEqual(self.read('docs/guides/use.md'),
                         'See [12](../design/protocol-versions/12.md#protocol-12-12-30), '
                         '[30](../design/protocol-versions/12.md#detail-thirty) and '
                         '[folder](../design/protocol-versions/).\n')
        self.assertEqual(self.read('docs/design/protocol.md'), DOC)
        self.assertIn('VERSION: u32 = 12;', self.read('src/protocol.rs'))
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_old_in_file_heading_folds_into_version_file(self):
        doc = (DOC + '\n## Protocol next (#12)\n\nTwelve uses [y](other.md#y) and '
               '[eleven](#protocol-11-317).\n\n### Twelve detail\n\nMore.\n\n'
               '## Later\n\nKept; see [it](#protocol-next-12); '
               'patches write `Protocol next (#N)` or `# Protocol next (#issue)`; Protocol next prose.\n')
        self.write('docs/design/protocol.md', doc)
        self.write('docs/design/other.md', '# Other\n\n### Protocol next (#9, #30)\n\nNine.\n')
        self.write('docs/guides/use.md', 'See [p](../design/protocol.md#protocol-next-12).\n')
        self.commit('patches in flight')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.subject(), 'Protocol 12: #9, #12, #30')
        self.assertEqual(self.read('docs/design/protocol-versions/12.md'),
                         '# Protocol 12 (#9, #12, #30)\n\nNine.\n\n'
                         'Twelve uses [y](../other.md#y) and [eleven](../protocol.md#protocol-11-317).\n\n'
                         '## Twelve detail\n\nMore.\n')
        self.assertEqual(self.read('docs/design/protocol.md'),
                         DOC + '\n## Later\n\nKept; see [it](protocol-versions/12.md#protocol-12-9-12-30); '
                         'patches write `Protocol next (#N)` or `# Protocol next (#issue)`; Protocol 12 prose.\n')
        self.assertEqual(self.read('docs/design/other.md'), '# Other\n')
        self.assertEqual(self.read('docs/guides/use.md'),
                         'See [p](../design/protocol-versions/12.md#protocol-12-9-12-30).\n')
        self.assertIn('"protocol_version": 12,', self.read('docs/reference/protocol.schema.json'))

    def test_second_run_does_nothing(self):
        self.write('docs/design/protocol-versions/next-12.md', '# Protocol next (#12)\n\nTwelve.\n')
        self.write('docs/design/protocol.md', DOC + '\n## Protocol next (#13)\n\nThirteen.\n')
        self.commit('patch')
        self.assertEqual(self.land().returncode, 0)
        landed = self.head()
        notes = self.read('docs/design/protocol-versions/12.md')
        self.assertEqual(notes, '# Protocol 12 (#12, #13)\n\nTwelve.\n\nThirteen.\n')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.head(), landed)
        self.assertIn('nothing to do', result.stdout)
        self.assertEqual(self.read('docs/design/protocol-versions/12.md'), notes)

    def test_patch_added_after_landing_joins_the_version_file(self):
        self.write('docs/design/protocol-versions/next-12.md', '# Protocol next (#12)\n\nTwelve.\n')
        self.commit('patch')
        self.assertEqual(self.land().returncode, 0)
        self.write('docs/design/protocol-versions/next-8.md', '# Protocol next (#8)\n\nEight.\n')
        self.commit('later patch')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.subject(), 'Protocol 12: #8')
        self.assertEqual(self.read('docs/design/protocol-versions/12.md'),
                         '# Protocol 12 (#8, #12)\n\nTwelve.\n\nEight.\n')
        self.assertIn('VERSION: u32 = 12;', self.read('src/protocol.rs'))

    def test_version_two_ahead_fails(self):
        self.write('src/protocol.rs', RUST.replace('VERSION: u32 = 11', 'VERSION: u32 = 13'))
        self.commit('bad patch')
        before = self.head()
        result = self.land()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('PROTOCOL_VERSION 13', result.stderr)
        self.assertEqual(self.head(), before)


if __name__ == '__main__':
    unittest.main()
