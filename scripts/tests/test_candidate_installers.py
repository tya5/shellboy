"""Candidate validation rejects mismatched bytes or unstamped installer assets."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('candidate_installers', ROOT / 'scripts/check-candidate-installers.py')
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)


class CandidateAssetChecks(unittest.TestCase):
    def fixture(self, directory):
        tag = 'v0.4.0'
        for target, suffix, _ in checks.TARGETS.values():
            (directory / f'shellboy-{tag}-{target}.{suffix}').write_bytes(b'synthetic archive bytes; not executed')
        for name in ['install.sh', 'install.ps1']:
            (directory / name).write_text((ROOT / name).read_text(encoding='utf-8').replace('__SHELLBOY_RELEASE_VERSION__', tag), encoding='utf-8')
        self.sums(directory)
        return tag

    def sums(self, directory):
        files = sorted(path for path in directory.iterdir() if path.name != 'SHA256SUMS')
        (directory / 'SHA256SUMS').write_text(''.join(f'{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n' for path in files), encoding='utf-8')

    def test_candidate_assets_are_exact_and_hash_verified(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tag = self.fixture(root)
            self.assertEqual(len(checks.verify_assets(root, tag)), 5)
            (root / 'install.sh').write_text('tampered', encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                checks.verify_assets(root, tag)

    def test_rehashed_unstamped_or_wrong_version_scripts_still_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tag = self.fixture(root)
            (root / 'install.sh').write_text((ROOT / 'install.sh').read_text(encoding='utf-8'), encoding='utf-8')
            self.sums(root)
            with self.assertRaisesRegex(ValueError, 'template marker'):
                checks.verify_assets(root, tag)
            self.fixture(root)
            path = root / 'install.ps1'
            path.write_text(path.read_text(encoding='utf-8').replace("$version = 'v0.4.0'", "$version = 'v0.3.0'"), encoding='utf-8')
            self.sums(root)
            with self.assertRaisesRegex(ValueError, 'not bound'):
                checks.verify_assets(root, tag)
