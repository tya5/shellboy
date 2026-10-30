#!/usr/bin/env python3
"""Run exact assembled candidate installers with temporary destinations only.

Downloads are a strict local mirror of the candidate's intended release URLs;
this does not claim unpublished URLs work. Native binaries/archives are real.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

TARGETS = {
    'Darwin': ('aarch64-apple-darwin', 'tar.gz', 'shellboy'),
    'Linux': ('x86_64-unknown-linux-gnu', 'tar.gz', 'shellboy'),
    'Windows': ('x86_64-pc-windows-msvc', 'zip', 'shellboy.exe'),
}


def verify_assets(assets, tag):
    if not re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+', tag):
        raise ValueError('stable vX.Y.Z label required')
    expected = {f'shellboy-{tag}-{target}.{suffix}' for target, suffix, _ in TARGETS.values()}
    expected.update(['install.sh', 'install.ps1'])
    entries = {}
    for line in (assets / 'SHA256SUMS').read_text(encoding='utf-8').splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  ([^/\\\s]+)', line)
        if not match or match[2] in entries:
            raise ValueError('invalid/duplicate checksum entry')
        entries[match[2]] = match[1]
    if set(entries) != expected or {p.name for p in assets.iterdir()} != expected | {'SHA256SUMS'}:
        raise ValueError('unexpected candidate asset set')
    for name, digest in entries.items():
        if hashlib.sha256((assets / name).read_bytes()).hexdigest() != digest:
            raise ValueError(f'checksum mismatch: {name}')
    unix = (assets / 'install.sh').read_text(encoding='utf-8')
    windows = (assets / 'install.ps1').read_text(encoding='utf-8')
    if '__SHELLBOY_RELEASE_VERSION__' in unix + windows:
        raise ValueError('installer template marker survived assembly')
    if f'version=${{SHELLBOY_VERSION:-{tag}}}' not in unix or f"if (-not $version) {{ $version = '{tag}' }}" not in windows:
        raise ValueError('installer is not bound to the candidate version')
    return entries


def check(assets, tag, powershell=None):
    import platform
    entries = verify_assets(assets, tag)
    target, suffix, executable = TARGETS[platform.system()]
    archive = f'shellboy-{tag}-{target}.{suffix}'
    with tempfile.TemporaryDirectory(prefix='shellboy-candidate-install-') as tmp:
        root = Path(tmp)
        dest = root / 'isolated install directory'
        env = {k: v for k, v in os.environ.items() if k != 'SHELLBOY_VERSION'}
        env.update(SHELLBOY_INSTALL_DIR=str(dest), CANDIDATE_ASSETS=str(assets),
                   CANDIDATE_TAG=tag, CANDIDATE_ARCHIVE=archive)
        assert dest.is_relative_to(root) and not dest.exists()
        if powershell:
            wrapper = root / 'fixture.ps1'
            wrapper.write_text(r'''
$ErrorActionPreference = 'Stop'
function Invoke-WebRequest {
    param([switch]$UseBasicParsing, [string]$Uri, [string]$OutFile)
    $base = "https://github.com/tya5/shellboy/releases/download/$env:CANDIDATE_TAG/"
    if ($Uri -ne ($base + $env:CANDIDATE_ARCHIVE) -and $Uri -ne ($base + 'SHA256SUMS')) { throw "Unexpected candidate URL: $Uri" }
    $name = [IO.Path]::GetFileName($Uri)
    [IO.File]::Copy((Join-Path $env:CANDIDATE_ASSETS $name), $OutFile, $true)
}
$source = [IO.File]::ReadAllText((Join-Path $env:CANDIDATE_ASSETS 'install.ps1'))
$source | Invoke-Expression
''', encoding='utf-8')
            command = [powershell, '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', str(wrapper)]
            run = lambda: subprocess.run(command, env=env, capture_output=True, text=True, timeout=180)
        else:
            mocks = root / 'mocks'
            mocks.mkdir()
            curl = mocks / 'curl'
            curl.write_text('''#!/bin/sh
set -eu
url= out=
while [ "$#" -gt 0 ]; do
 case "$1" in -o) shift; out=$1 ;; https://*) url=$1 ;; esac
 shift
done
base=https://github.com/tya5/shellboy/releases/download/$CANDIDATE_TAG
case "$url" in "$base/$CANDIDATE_ARCHIVE"|"$base/SHA256SUMS") ;; *) echo "Unexpected candidate URL: $url" >&2; exit 1 ;; esac
cp "$CANDIDATE_ASSETS/${url##*/}" "$out"
''', encoding='utf-8')
            curl.chmod(0o755)
            env['PATH'] = str(mocks) + os.pathsep + env['PATH']
            source = (assets / 'install.sh').read_text(encoding='utf-8')
            run = lambda: subprocess.run(['sh'], input=source, env=env, capture_output=True, text=True, timeout=180)
        first = run()
        if first.returncode:
            raise AssertionError(first.stdout + first.stderr)
        binary = dest / executable
        version = subprocess.check_output([str(binary), '--version'], text=True).strip()
        assert version == 'shellboy ' + tag[1:]
        original = binary.read_bytes()
        second = run()
        if second.returncode:
            raise AssertionError(second.stdout + second.stderr)
        backups = list(dest.glob(executable + '.backup-*'))
        assert len(backups) == 1 and backups[0].read_bytes() == original
        assert binary.read_bytes() == original
        assert 'restart' in first.stdout and 'PATH' in first.stdout and 'back up' in first.stdout
        # A damaged mirror must not replace an installed binary or create a fresh destination.
        bad = root / 'bad mirror'
        bad.mkdir()
        shutil.copy2(assets / 'SHA256SUMS', bad / 'SHA256SUMS')
        (bad / archive).write_bytes(b'corrupt candidate archive')
        env['CANDIDATE_ASSETS'] = str(bad)
        # Keep the verified script, not a damaged/missing script, in this fixture.
        shutil.copy2(assets / 'install.ps1', bad / 'install.ps1')
        failed = run()
        assert failed.returncode != 0 and 'SHA-256 mismatch' in failed.stdout + failed.stderr
        assert binary.read_bytes() == original and len(list(dest.glob(executable + '.backup-*'))) == 1
        absent = root / 'must remain absent'
        env['SHELLBOY_INSTALL_DIR'] = str(absent)
        failed = run()
        assert failed.returncode != 0 and not absent.exists()
        return {'tag': tag, 'target': target, 'shell': powershell or 'sh', 'version': version,
                'archive_sha256': entries[archive], 'binary_sha256': hashlib.sha256(original).hexdigest(),
                'fresh': 'pass', 'replacement_backup': 'pass', 'checksum_failure_preserves_state': 'pass',
                'transport': 'strict local candidate asset mirror; unpublished public URLs not claimed tested',
                'temporary_install_directory': True, 'version_environment_override': False}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--assets', required=True, type=Path)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--powershell', choices=['powershell', 'pwsh'])
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    report = check(args.assets.resolve(), args.tag, args.powershell)
    text = json.dumps(report, indent=2) + '\n'
    print(text)
    if args.report:
        args.report.write_text(text, encoding='utf-8')


if __name__ == '__main__':
    main()
