#!/usr/bin/env python3
"""Resolve and verify a release candidate without creating a tag or release."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import urllib.error
import urllib.request
from urllib.parse import urlsplit
import zipfile

SHA = re.compile(r'[0-9a-f]{40}\Z')
TAG = re.compile(r'v[0-9]+\.[0-9]+\.[0-9]+\Z')
MAX_PAGES = 10
MAX_ARCHIVE = 512 * 1024 * 1024
REQUIRED_JOBS = {
    'Build x86_64-pc-windows-msvc', 'Build aarch64-apple-darwin',
    'Build x86_64-unknown-linux-gnu',
    'Assemble validated archives, installers and SHA256SUMS',
    'Candidate installer ubuntu-22.04 sh', 'Candidate installer macos-15 sh',
    'Candidate installer windows-latest powershell',
    'Candidate installer windows-latest pwsh',
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def expected_files(tag):
    return {f'shellboy-{tag}-x86_64-pc-windows-msvc.zip',
            f'shellboy-{tag}-aarch64-apple-darwin.tar.gz',
            f'shellboy-{tag}-x86_64-unknown-linux-gnu.tar.gz',
            'install.sh', 'install.ps1'}


def verify_assets(files, tag, sums_digest):
    expected = expected_files(tag)
    if set(files) != expected | {'SHA256SUMS'}:
        raise ValueError('candidate asset set is incomplete or unexpected')
    sums = files['SHA256SUMS']
    if digest(sums) != sums_digest:
        raise ValueError('candidate SHA256SUMS digest mismatch')
    entries = {}
    for line in sums.decode('utf-8').splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  ([^/\\\s]+)', line)
        if not match or match[2] in entries:
            raise ValueError('invalid or duplicate checksum entry')
        entries[match[2]] = match[1]
    if set(entries) != expected:
        raise ValueError('candidate checksum entries differ from expected assets')
    for name, checksum in entries.items():
        if digest(files[name]) != checksum:
            raise ValueError(f'candidate checksum mismatch: {name}')
    return sums.decode('utf-8')


def unzip(data, *, max_size=MAX_ARCHIVE):
    files = {}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        total = 0
        for info in archive.infolist():
            if info.is_dir() or '/' in info.filename or '\\' in info.filename or info.filename in ('.', '..'):
                raise ValueError('unexpected artifact member path')
            total += info.file_size
            if total > max_size or info.filename in files:
                raise ValueError('artifact is too large or contains duplicate members')
            files[info.filename] = archive.read(info)
    return files


class SafeRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        redirected = super().redirect_request(request, fp, code, msg, headers, newurl)
        if redirected and urlsplit(request.full_url).netloc != urlsplit(newurl).netloc:
            redirected.remove_header('Authorization')
        return redirected


class GitHub:
    def __init__(self, repo, token):
        if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo):
            raise ValueError('invalid repository')
        self.repo, self.token = repo, token

    def get(self, path, *, binary=False):
        request = urllib.request.Request(
            f'https://api.github.com/repos/{self.repo}/{path}',
            headers={'Authorization': f'Bearer {self.token}',
                     'Accept': 'application/vnd.github+json',
                     'X-GitHub-Api-Version': '2022-11-28',
                     'User-Agent': 'shellboy-release-candidate'})
        with urllib.request.build_opener(SafeRedirect()).open(request, timeout=60) as response:
            data = response.read(MAX_ARCHIVE + 1)
        if len(data) > MAX_ARCHIVE:
            raise ValueError('GitHub response exceeds size limit')
        return data if binary else json.loads(data)

    def pages(self, path, key):
        result = []
        for page in range(1, MAX_PAGES + 1):
            sep = '&' if '?' in path else '?'
            items = self.get(f'{path}{sep}per_page=100&page={page}')[key]
            result.extend(items)
            if len(items) < 100:
                return result
        raise ValueError('candidate search reached pagination bound; cannot prove absence')

    def runs(self):
        return self.pages('actions/workflows/release.yml/runs?event=workflow_dispatch&status=success', 'workflow_runs')

    def ci_runs(self, sha):
        return self.pages(f'actions/workflows/ci.yml/runs?head_sha={sha}&status=success', 'workflow_runs')

    def run(self, run):
        return self.get(f'actions/runs/{run}')

    def artifacts(self, run):
        return self.pages(f'actions/runs/{run}/artifacts', 'artifacts')

    def jobs(self, run, attempt):
        return self.pages(f'actions/runs/{run}/attempts/{attempt}/jobs', 'jobs')

    def download(self, artifact):
        return self.get(f'actions/artifacts/{artifact}/zip', binary=True)


def plan(api, sha, tag):
    if not SHA.fullmatch(sha) or not TAG.fullmatch(tag):
        raise ValueError('expected exact 40-digit SHA and stable vX.Y.Z tag')
    prefix = f'candidate-proof-{sha}-{tag}-a'
    runs = sorted(api.runs(), key=lambda run: (run['created_at'], run['id']), reverse=True)
    for index, run in enumerate(runs):
        if run.get('status') != 'completed' or run.get('conclusion') != 'success':
            continue
        if (run.get('event') != 'workflow_dispatch' or run.get('path') != '.github/workflows/release.yml'
                or run.get('head_branch') != 'main' or run.get('head_repository', {}).get('full_name') != api.repo):
            continue
        artifacts = api.artifacts(run['id'])
        matches = [item for item in artifacts if item['name'] == prefix + str(run['run_attempt'])]
        if not matches:
            continue
        if len(matches) != 1:
            raise ValueError('duplicate candidate provenance for one attempt')
        proof_artifact = matches[0]
        if proof_artifact.get('expired'):
            raise ValueError(f'candidate run {run["id"]}: provenance expired; rerun candidate.sh {sha}, validate the candidate, then rerun release workflow without retagging')
        proof_files = unzip(api.download(proof_artifact['id']), max_size=65536)
        if set(proof_files) != {'candidate-proof.json'}:
            raise ValueError('invalid candidate provenance artifact')
        proof = json.loads(proof_files['candidate-proof.json'])
        if (proof.get('repository') != api.repo or proof.get('sha') != sha or proof.get('tag') != tag
                or proof.get('run_id') != run['id'] or proof.get('attempt') != run['run_attempt']
                or not re.fullmatch(r'[0-9a-f]{64}', proof.get('sums_sha256', ''))):
            raise ValueError('candidate provenance does not match selected run and tag')
        jobs = api.jobs(run['id'], run['run_attempt'])
        passed = {job['name'] for job in jobs if job.get('conclusion') == 'success'}
        if not REQUIRED_JOBS <= passed:
            raise ValueError(f'candidate run {run["id"]}: native build or installer checks not successful')
        matching_assets = [item for item in artifacts if item['id'] == proof.get('asset_id') and item['name'] == 'candidate-assets']
        if len(matching_assets) != 1 or matching_assets[0].get('expired'):
            raise ValueError(f'candidate run {run["id"]}: candidate-assets missing or expired; rerun candidate.sh {sha}, validate the candidate, then rerun release workflow without retagging')
        try:
            files = unzip(api.download(proof['asset_id']))
        except (urllib.error.HTTPError, urllib.error.URLError) as error:
            raise ValueError(f'candidate run {run["id"]}: candidate-assets unavailable; rerun candidate.sh {sha}, validate the candidate, then rerun release workflow without retagging') from error
        sums = verify_assets(files, tag, proof['sums_sha256'])
        # A replacement is unambiguous only when older matching assets have expired.
        for older in runs[index + 1:]:
            if (older.get('status') != 'completed' or older.get('conclusion') != 'success'
                    or older.get('event') != 'workflow_dispatch' or older.get('path') != '.github/workflows/release.yml'
                    or older.get('head_branch') != 'main' or older.get('head_repository', {}).get('full_name') != api.repo):
                continue
            older_artifacts = api.artifacts(older['id'])
            older_proofs = [item for item in older_artifacts if item['name'] == prefix + str(older['run_attempt'])]
            if len(older_proofs) > 1:
                raise ValueError('duplicate candidate provenance for one attempt')
            if not older_proofs or older_proofs[0].get('expired'):
                continue
            older_files = unzip(api.download(older_proofs[0]['id']), max_size=65536)
            if set(older_files) != {'candidate-proof.json'}:
                raise ValueError('invalid older candidate provenance artifact')
            older_proof = json.loads(older_files['candidate-proof.json'])
            if (older_proof.get('repository') != api.repo or older_proof.get('sha') != sha
                    or older_proof.get('tag') != tag or older_proof.get('run_id') != older['id']
                    or older_proof.get('attempt') != older['run_attempt']):
                raise ValueError('inconsistent older candidate provenance')
            if any(item['id'] == older_proof.get('asset_id') and item['name'] == 'candidate-assets'
                   and not item.get('expired') for item in older_artifacts):
                raise ValueError('multiple available candidates match; resolve ambiguity before release')
        required_ci = {'check (ubuntu-latest)', 'check (macos-latest)', 'check (windows-latest)'}
        ci = None
        for candidate in api.ci_runs(sha):
            if (candidate.get('head_sha') != sha or candidate.get('path') != '.github/workflows/ci.yml'
                    or candidate.get('status') != 'completed' or candidate.get('conclusion') != 'success'):
                continue
            ci_jobs = api.jobs(candidate['id'], candidate['run_attempt'])
            if required_ci <= {job['name'] for job in ci_jobs if job.get('conclusion') == 'success'}:
                ci = candidate
                break
        if ci is None:
            raise ValueError('matching candidate has no successful three-OS CI run for the exact SHA')
        for selected in (run, ci):
            current = api.run(selected['id'])
            if (current.get('run_attempt') != selected['run_attempt'] or current.get('status') != 'completed'
                    or current.get('conclusion') != 'success'):
                raise ValueError(f'run {selected["id"]} changed during candidate verification; retry')
        return {'route': 'reuse', 'run_id': run['id'], 'attempt': run['run_attempt'],
                'ci_run_id': ci['id'], 'ci_attempt': ci['run_attempt'],
                'sha': sha, 'tag': tag, 'sums': sums, 'files': files}
    return {'route': 'build', 'sha': sha, 'tag': tag}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sha', required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = plan(GitHub(os.environ['GITHUB_REPOSITORY'], os.environ['GH_TOKEN']), args.sha, args.tag)
    args.output.mkdir(parents=True, exist_ok=True)
    files = result.pop('files', {})
    for name, data in files.items():
        (args.output / name).write_bytes(data)
    print(json.dumps(result, sort_keys=True))
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as out:
            print(f"route={result['route']}", file=out)
            print(f"candidate_run={result.get('run_id', '')}", file=out)
            print(f"candidate_attempt={result.get('attempt', '')}", file=out)
            print(f"ci_run={result.get('ci_run_id', '')}", file=out)
            print(f"ci_attempt={result.get('ci_attempt', '')}", file=out)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, urllib.error.URLError, zipfile.BadZipFile) as error:
        print(f'release candidate selection failed: {error}', file=sys.stderr)
        sys.exit(1)
