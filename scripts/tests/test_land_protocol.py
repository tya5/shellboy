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

    def test_next_heading_alone_bumps_and_names_issues(self):
        doc = (DOC + '\n## Protocol next (#12)\n\nSee [it](#protocol-next-12); '
               'patches write `Protocol next (#N)`.\n')
        self.write('docs/design/protocol.md', doc)
        self.write('docs/design/other.md', '### Protocol next (#9, #30)\n')
        self.commit('patch')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.subject(), 'Protocol 12: #9, #12, #30')
        text = self.read('docs/design/protocol.md')
        self.assertIn('## Protocol 12 (#12)', text)
        self.assertIn('(#protocol-12-12)', text)
        self.assertIn('`Protocol next (#N)`', text)
        self.assertEqual(self.read('docs/design/other.md'), '### Protocol 12 (#9, #30)\n')
        self.assertIn('"protocol_version": 12,', self.read('docs/reference/protocol.schema.json'))

    def test_second_run_does_nothing(self):
        self.write('docs/design/protocol.md', DOC + '\n## Protocol next (#12)\n')
        self.commit('patch')
        self.assertEqual(self.land().returncode, 0)
        landed = self.head()
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.head(), landed)
        self.assertIn('nothing to do', result.stdout)

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
