"""Source preparation is exact-source, fail-closed, and not native qualification."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/ci.yml'
SHA = '1' * 40


@unittest.skipIf(os.name == 'nt', 'Source-prepare commands run on Ubuntu/Bash')
class SourcePrepareTests(unittest.TestCase):
    def run_prepare(self, parity_code=0, expected=SHA, format_code=0):
        workflow = WORKFLOW.read_text(encoding='utf-8')
        block = workflow.split('      - name: Verify exact source, generate schema and test real-schema parity\n', 1)[1]
        block = block.split('        run: |\n', 1)[1].split('      - name: Retain source preparation', 1)[0]
        script = textwrap.dedent(block)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / 'bin'
            binaries.mkdir()
            mocks = {
                'git': f'#!/bin/sh\nprintf "%s\\n" "{SHA}"\n',
                'rustup': '#!/bin/sh\nexit 0\n',
                'cargo': '#!/bin/sh\ncase "$1" in\n'
                         f'fmt) exit {format_code};;\n'
                         f'run) printf \'{{"generated_from":"{SHA}"}}\\n\';;\n'
                         f'test) echo "mock parity exit {parity_code}"; exit {parity_code};;\n'
                         '*) exit 99;;\nesac\n',
            }
            for name, content in mocks.items():
                path = binaries / name
                path.write_text(content, encoding='utf-8')
                path.chmod(0o700)
            env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ['PATH'],
                       EXPECTED_SOURCE_SHA=expected, GITHUB_SHA=SHA)
            result = subprocess.run(['bash', '-c', script], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=10)
            evidence = root / 'source-prepare'
            files = {path.name: path.read_text(encoding='utf-8') for path in evidence.iterdir()}
            return result, files

    def test_success_records_source_commands_and_artifact(self):
        result, files = self.run_prepare()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f'actual={SHA}', files['manifest.txt'])
        self.assertIn('NOT STANDARD/THREE-OS QUALIFICATION', files['manifest.txt'])
        self.assertEqual(json.loads(files['protocol.schema.json']), {'generated_from': SHA})
        self.assertEqual(files['exit-codes.tsv'], 'toolchain\t0\nformat\t0\nschema\t0\nparity\t0\n')
        self.assertIn('cargo test --locked --lib', files['commands.txt'])

    def test_failed_parity_fails_workflow_and_keeps_evidence(self):
        result, files = self.run_prepare(parity_code=23)
        self.assertEqual(result.returncode, 23)
        self.assertIn('parity\t23\n', files['exit-codes.tsv'])
        self.assertIn('mock parity exit 23', files['parity.stdout'])
        self.assertIn('protocol.schema.json', files)

    def test_earlier_failure_stops_before_generation(self):
        result, files = self.run_prepare(format_code=17)
        self.assertEqual(result.returncode, 17)
        self.assertIn('format\t17\n', files['exit-codes.tsv'])
        self.assertNotIn('schema.stdout', files)
        self.assertNotIn('parity.stdout', files)

    def test_wrong_sha_fails_before_any_build(self):
        result, files = self.run_prepare(expected='0' * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('commands.txt', files)
        self.assertNotIn('protocol.schema.json', files)

    def test_mode_has_no_native_or_reuse_authority(self):
        workflow = WORKFLOW.read_text(encoding='utf-8')
        self.assertIn("if: '!inputs.source_prepare'", workflow)
        self.assertIn('if: always() && !inputs.source_prepare &&', workflow)
        self.assertIn('name: SOURCE PREPARE - Not Qualification', workflow)
        reuse = (ROOT / 'scripts/ci-reuse.cjs').read_text(encoding='utf-8')
        self.assertNotIn('SOURCE PREPARE', reuse)
        self.assertIn("'check (windows-latest)'", reuse)


if __name__ == '__main__':
    unittest.main()
