import io
import json
from pathlib import Path
import sys
import unittest
import urllib.error
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release_candidate as rc

SHA = 'a' * 40
TAG = 'v1.2.3'
REPO = 'owner/repo'


def archive(files):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, 'w') as out:
        for name, value in files.items():
            out.writestr(name, value)
    return buffer.getvalue()


def run(identifier):
    return {'id': identifier, 'created_at': f'2026-01-{identifier:02}T00:00:00Z',
            'run_attempt': 1, 'status': 'completed', 'conclusion': 'success',
            'event': 'workflow_dispatch', 'path': '.github/workflows/release.yml',
            'head_branch': 'main', 'head_repository': {'full_name': REPO}}


class FakeGitHub:
    repo = REPO

    def __init__(self):
        self.runs_data = [run(2)]
        self.ci = {'id': 20, 'head_sha': SHA, 'path': '.github/workflows/ci.yml',
                   'run_attempt': 1, 'status': 'completed', 'conclusion': 'success'}
        self.files = {name: name.encode() for name in rc.expected_files(TAG)}
        self.files['SHA256SUMS'] = ''.join(
            f'{rc.digest(data)}  {name}\n' for name, data in sorted(self.files.items())
        ).encode()
        self.data = {}
        self.artifacts_data = {}
        self.add_candidate(2)
        self.changed = False
        self.ci_pass = True

    def add_candidate(self, identifier, expired=False):
        proof_id, asset_id = identifier * 10, identifier * 10 + 1
        proof = {'repository': REPO, 'sha': SHA, 'tag': TAG, 'run_id': identifier,
                 'attempt': 1, 'asset_id': asset_id,
                 'sums_sha256': rc.digest(self.files['SHA256SUMS'])}
        self.data[proof_id] = archive({'candidate-proof.json': json.dumps(proof)})
        self.data[asset_id] = archive(self.files)
        self.artifacts_data[identifier] = [
            {'id': proof_id, 'name': f'candidate-proof-{SHA}-{TAG}-a1', 'expired': False},
            {'id': asset_id, 'name': 'candidate-assets', 'expired': expired},
        ]

    def runs(self): return self.runs_data
    def artifacts(self, identifier): return self.artifacts_data[identifier]
    def download(self, identifier): return self.data[identifier]
    def ci_runs(self, sha): return [self.ci]
    def jobs(self, identifier, attempt):
        names = rc.REQUIRED_JOBS if identifier != 20 else {
            'check (ubuntu-latest)', 'check (macos-latest)', 'check (windows-latest)'}
        return [{'name': name, 'conclusion': 'success' if identifier != 20 or self.ci_pass else 'failure'} for name in names]
    def run(self, identifier):
        current = dict(self.ci if identifier == 20 else next(r for r in self.runs_data if r['id'] == identifier))
        if self.changed:
            current['run_attempt'] = 2
        return current


class PlanTests(unittest.TestCase):
    def setUp(self): self.api = FakeGitHub()

    def plan(self): return rc.plan(self.api, SHA, TAG)

    def test_reuse_is_byte_identical(self):
        result = self.plan()
        self.assertEqual(result['route'], 'reuse')
        self.assertEqual(result['files'], self.api.files)
        self.assertEqual(result['ci_run_id'], 20)

    def test_no_match_builds(self):
        self.api.runs_data = []
        self.assertEqual(self.plan()['route'], 'build')

    def test_expired_asset_fails_with_recovery(self):
        self.api.artifacts_data[2][1]['expired'] = True
        with self.assertRaisesRegex(ValueError, 'rerun candidate.sh .*without retagging'):
            self.plan()

    def test_missing_asset_fails(self):
        self.api.artifacts_data[2].pop()
        with self.assertRaisesRegex(ValueError, 'missing or expired'):
            self.plan()

    def test_unavailable_asset_fails(self):
        old = self.api.download
        def download(identifier):
            if identifier == 21:
                raise urllib.error.HTTPError('https://example.com', 410, 'expired', {}, None)
            return old(identifier)
        self.api.download = download
        with self.assertRaisesRegex(ValueError, 'unavailable; rerun candidate.sh'):
            self.plan()

    def test_proof_expired_fails(self):
        self.api.artifacts_data[2][0]['expired'] = True
        with self.assertRaisesRegex(ValueError, 'provenance expired'):
            self.plan()

    def test_checksum_corruption_fails(self):
        self.api.files['install.sh'] = b'corrupt'
        self.api.data[21] = archive(self.api.files)
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            self.plan()

    def test_wrong_provenance_fails(self):
        self.api.data[20] = archive({'candidate-proof.json': '{}'})
        with self.assertRaisesRegex(ValueError, 'provenance does not match'):
            self.plan()

    def test_rerun_race_fails(self):
        self.api.changed = True
        with self.assertRaisesRegex(ValueError, 'changed during'):
            self.plan()

    def test_ci_failure_fails(self):
        self.api.ci_pass = False
        with self.assertRaisesRegex(ValueError, 'three-OS CI'):
            self.plan()

    def test_multiple_available_candidates_fail(self):
        self.api.runs_data.append(run(1))
        self.api.add_candidate(1)
        with self.assertRaisesRegex(ValueError, 'multiple available'):
            self.plan()

    def test_replacement_after_expiry_succeeds(self):
        self.api.runs_data.append(run(1))
        self.api.add_candidate(1, expired=True)
        self.assertEqual(self.plan()['run_id'], 2)

    def test_non_main_run_cannot_assert_candidate(self):
        self.api.runs_data[0]['head_branch'] = 'untrusted'
        self.assertEqual(self.plan()['route'], 'build')

    def test_zip_path_traversal_fails(self):
        with self.assertRaisesRegex(ValueError, 'member path'):
            rc.unzip(archive({'../file': b'x'}))

    def test_api_error_fails(self):
        def fail(): raise OSError('API unavailable')
        self.api.runs = fail
        with self.assertRaisesRegex(OSError, 'API unavailable'):
            self.plan()


class PaginationTests(unittest.TestCase):
    def test_pagination_and_bound(self):
        api = rc.GitHub(REPO, 'not-a-real-token')
        seen = []
        def get(path):
            seen.append(path)
            return {'items': list(range(100)) if len(seen) < 3 else [100]}
        api.get = get
        self.assertEqual(len(api.pages('something', 'items')), 201)
        api.get = lambda path: {'items': list(range(100))}
        with self.assertRaisesRegex(ValueError, 'pagination bound'):
            api.pages('something', 'items')


if __name__ == '__main__':
    unittest.main()
