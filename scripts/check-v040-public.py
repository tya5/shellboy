"""Isolated release-publication evidence; no build, owner or real data access."""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

TAG = 'v0.4.0'
BASE = 'https://github.com/tya5/shellboy/releases/download/' + TAG + '/'
LATEST = 'https://github.com/tya5/shellboy/releases/latest/download/'
assert os.environ['RELEASE_TAG'] == TAG
pins = json.loads(Path('scripts/v040-public-hashes.json').read_text())
curl = shutil.which('curl.exe' if os.name == 'nt' else 'curl')

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def download(url, path):
    subprocess.run([curl, '--disable', '--fail', '--location', '--silent',
                    '--show-error', '--retry', '3', '--max-time', '180',
                    '--output', str(path), url], check=True)

with tempfile.TemporaryDirectory(prefix='shellboy-public-v040-') as temp:
    root = Path(temp)
    for name, expected in pins.items():
        download(BASE + name, root / name)
        assert digest(root / name) == expected, name
    for line in (root / 'SHA256SUMS').read_text().splitlines():
        expected, name = line.split()
        assert digest(root / name) == expected == pins[name], name
    system = platform.system()
    target = {'Darwin': 'aarch64-apple-darwin', 'Linux': 'x86_64-unknown-linux-gnu',
              'Windows': 'x86_64-pc-windows-msvc'}[system]
    ext = '.zip' if system == 'Windows' else '.tar.gz'
    archive = root / ('shellboy-' + TAG + '-' + target + ext)
    extract = root / 'extracted'
    if ext == '.zip':
        with zipfile.ZipFile(archive) as bundle:
            bundle.extractall(extract)
    else:
        with tarfile.open(archive) as bundle:
            bundle.extractall(extract, filter='data')
    binary_name = 'shellboy.exe' if system == 'Windows' else 'shellboy'
    binary = extract / ('shellboy-' + TAG + '-' + target) / binary_name
    assert subprocess.check_output([str(binary), '--version'], text=True).strip() == 'shellboy 0.4.0'
    subprocess.run([str(binary), '--help'], check=True, stdout=subprocess.DEVNULL)
    subprocess.run([str(binary), 'docs', 'list'], check=True, stdout=subprocess.DEVNULL)
    for name in ('install.sh', 'install.ps1'):
        latest = root / ('latest-' + name)
        download(LATEST + name, latest)
        assert digest(latest) == pins[name], name
        assert '__SHELLBOY_RELEASE_VERSION__' not in latest.read_text()
        assert TAG in latest.read_text()
    env = dict(os.environ)
    env.pop('SHELLBOY_VERSION', None)
    destination = root / 'temporary install with spaces'
    env['SHELLBOY_INSTALL_DIR'] = str(destination)
    installer_shell = os.environ['INSTALLER_SHELL']
    if installer_shell == 'sh':
        command = ['sh', '-c', 'set -eu; curl --disable -fsSL ' + LATEST + 'install.sh | sh']
    else:
        command = [installer_shell, '-NoProfile', '-NonInteractive', '-Command',
                   "$ErrorActionPreference='Stop'; irm '" + LATEST + "install.ps1' | iex"]
    logs = []
    for _ in range(2):
        result = subprocess.run(command, env=env, check=True, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        logs.append(result.stdout)
        print(result.stdout)
        installed = destination / binary_name
        assert digest(installed) == digest(binary)
        assert subprocess.check_output([str(installed), '--version'], text=True).strip() == 'shellboy 0.4.0'
    backups = list(destination.glob(binary_name + '.backup-*'))
    assert len(backups) == 1 and digest(backups[0]) == digest(binary)
    report = dict(tag=TAG, environment=platform.platform(), target=target,
                  installer_shell=installer_shell, anonymous_all_assets='pass',
                  native_version='shellboy 0.4.0', latest_scripts_match='pass',
                  temporary_one_line_install='pass', replacement_backup='pass',
                  binary_sha256=digest(binary), installer_output=logs)
Path('public-v040-result.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
