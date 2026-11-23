"""Landing numbers `next_` migrations once per batch, in batch commit order (#359)."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/land-protocol.py'
PROTOCOL = 'pub const PROTOCOL_VERSION: u32 = 11;\n'
SCHEMA = '''{
  "$id": "https://shellboy.dev/schema/protocol-v11.json",
  "definitions": {},
  "protocol_version": 11,
  "title": "Shellboy local RPC protocol v11"
}
'''
NAMES = '''pub(crate) const MIGRATION_NAMES: &[(i64, &str)] = &[
    (1, "0001_initial"),
    (2, "0002_two"),
];
pub const SCHEMA_VERSION: i64 = MIGRATION_NAMES.len() as i64;
'''
EXECUTOR = '''pub(super) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 2,
        sql: include_str!("../../migrations/0002_two.sql"),
        foreign_keys: None,
    },
];
'''
CURRENT = '''CREATE TABLE one(x);
INSERT INTO schema_migration(version,name) VALUES(1,'0001_initial');
INSERT INTO schema_migration(version,name) VALUES(2,'0002_two');
PRAGMA user_version=2;
'''


def ledger(name, number):
    return f"INSERT INTO schema_migration(version,name) VALUES({number},'{name}');\nPRAGMA user_version={number};\n"


class LandMigrationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.git('init', '-q', '-b', 'main')
        for key, value in (('user.name', 'Test'), ('user.email', 'test@example.invalid'),
                           ('commit.gpgsign', 'false'), ('core.autocrlf', 'false')):
            self.git('config', key, value)
        self.write('src/protocol.rs', PROTOCOL)
        self.write('docs/reference/protocol.schema.json', SCHEMA)
        self.write('src/storage.rs', NAMES)
        self.write('src/storage/migrate.rs', EXECUTOR)
        self.write('src/storage/current.sql', CURRENT)
        self.write('migrations/0001_initial.sql', 'CREATE TABLE one(x);\n')
        self.write('migrations/0002_two.sql', 'CREATE INDEX one_x ON one(x);\n' + ledger('0002_two', 2))
        self.write('tests/rewind.sql', 'PRAGMA user_version=2;\n')
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

    def replace(self, name, old, new):
        text = self.read(name)
        self.assertIn(old, text)
        self.write(name, text.replace(old, new, 1))

    def commit(self, message):
        self.git('add', '-A')
        self.git('commit', '-q', '-m', message)

    def land(self):
        return subprocess.run([sys.executable, str(SCRIPT), 'main'], cwd=self.root,
                              capture_output=True, text=True, timeout=30)

    def head(self):
        return self.git('rev-parse', 'HEAD').strip()

    def add_next(self, slug, provisional=3):
        """One patch as composed into the batch: both patches registered main + 1."""
        name = f'next_{slug}'
        self.write(f'migrations/{name}.sql', f'CREATE TABLE {slug}(x);\n' + ledger(name, provisional))
        self.replace('src/storage.rs', '];\npub const', f'    ({provisional}, "{name}"),\n];\npub const')
        self.replace('src/storage/migrate.rs', '];\n', f'''    Migration {{
        version: {provisional},
        sql: include_str!("../../migrations/{name}.sql"),
        foreign_keys: None,
    }},
];
''')
        self.write('src/storage/current.sql', self.read('src/storage/current.sql')
                   + f'CREATE TABLE {slug}(x);\n' + ledger(name, provisional))
        self.write('tests/rewind.sql', f"DROP TABLE {slug};\nDELETE FROM schema_migration WHERE name='{name}';\n"
                   + self.read('tests/rewind.sql'))
        self.write(f'docs/{slug}.md', f'Migration `{name}` adds `{slug}`; see migrations/{name}.sql.\n')
        self.commit(f'patch {slug}')

    def test_two_next_migrations_get_consecutive_numbers_in_commit_order(self):
        self.add_next('zeta')
        self.add_next('alpha')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git('log', '-1', '--format=%s').strip(), 'Migrations 3-4')
        self.assertEqual(self.git('status', '--porcelain'), '')
        files = self.git('ls-files', 'migrations').split()
        self.assertEqual(files, ['migrations/0001_initial.sql', 'migrations/0002_two.sql',
                                 'migrations/0003_zeta.sql', 'migrations/0004_alpha.sql'])
        self.assertEqual(self.read('migrations/0003_zeta.sql'), 'CREATE TABLE zeta(x);\n' + ledger('0003_zeta', 3))
        self.assertEqual(self.read('migrations/0004_alpha.sql'), 'CREATE TABLE alpha(x);\n' + ledger('0004_alpha', 4))
        self.assertEqual(self.read('src/storage.rs'), NAMES.replace(
            '];\npub const', '    (3, "0003_zeta"),\n    (4, "0004_alpha"),\n];\npub const'))
        executor = self.read('src/storage/migrate.rs')
        self.assertIn('version: 3,\n        sql: include_str!("../../migrations/0003_zeta.sql")', executor)
        self.assertIn('version: 4,\n        sql: include_str!("../../migrations/0004_alpha.sql")', executor)
        self.assertEqual(self.read('src/storage/current.sql'), CURRENT + 'CREATE TABLE zeta(x);\n'
                         + ledger('0003_zeta', 3) + 'CREATE TABLE alpha(x);\n' + ledger('0004_alpha', 4))
        self.assertIn("WHERE name='0004_alpha'", self.read('tests/rewind.sql'))
        self.assertEqual(self.read('docs/zeta.md'),
                         'Migration `0003_zeta` adds `zeta`; see migrations/0003_zeta.sql.\n')
        self.assertIn('= 11;', self.read('src/protocol.rs'))

    def test_nothing_to_do_without_next_migrations(self):
        self.write('docs/prose.md', 'Prose only; next_steps stays.\n')
        self.commit('prose')
        before = self.head()
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.head(), before)
        self.assertIn('nothing to do', result.stdout)

    def test_second_run_is_a_no_op(self):
        self.add_next('zeta')
        self.assertEqual(self.land().returncode, 0)
        landed = self.head()
        self.assertEqual(self.git('log', '-1', '--format=%s').strip(), 'Migration 3')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.head(), landed)
        self.assertIn('nothing to do', result.stdout)

    def test_protocol_and_migrations_share_one_commit(self):
        self.write('docs/wire.md', '## Protocol next (#7)\n')
        self.commit('wire')
        self.add_next('zeta')
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git('log', '-1', '--format=%s').strip(), 'Protocol 12: #7 / Migration 3')
        self.assertEqual(self.git('rev-list', '--count', 'main..HEAD').strip(), '3')

    def test_hard_numbered_collision_with_base_is_refused(self):
        self.write('migrations/0002_other.sql', 'CREATE TABLE other(x);\n' + ledger('0002_other', 2))
        self.commit('old-style patch')
        before = self.head()
        result = self.land()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('0002_other.sql collides with migrations/0002_two.sql', result.stderr)
        self.assertEqual(self.head(), before)

    def test_half_numbered_batch_is_refused_before_writing(self):
        self.add_next('zeta')
        self.git('mv', 'migrations/next_zeta.sql', 'migrations/0003_zeta.sql')
        self.commit('renamed by hand only')
        before = self.head()
        result = self.land()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('names next_zeta but migrations/next_zeta.sql is missing', result.stderr)
        self.assertEqual(self.head(), before)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_missing_ledger_line_is_refused_before_writing(self):
        self.add_next('zeta')
        self.replace('src/storage/current.sql', "VALUES(3,'next_zeta')", "VALUES(3,'zeta')")
        self.commit('broken current.sql')
        before = self.head()
        result = self.land()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("src/storage/current.sql: expected exactly one ledger line VALUES(N,'next_zeta')",
                      result.stderr)
        self.assertEqual(self.head(), before)
        self.assertEqual(self.git('status', '--porcelain'), '')


if __name__ == '__main__':
    unittest.main()
