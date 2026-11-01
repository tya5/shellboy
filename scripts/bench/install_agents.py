#!/usr/bin/env python3
"""Install the pinned #116 comparison agents into a new directory (Linux x86_64).

No login, credentials or global install: npm packages go to PREFIX/npm with a
private npm cache and HOME, and jcode's release tarball is checked against the
release's SHA256SUMS. Writes PREFIX/provenance.json and PREFIX/agents.args
(the `--agent NAME=PATH` arguments for compare.py, one per line).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
from urllib.request import urlopen

NPM = {
    'claude-code': ('@anthropic-ai/claude-code', '2.1.316', 'https://github.com/anthropics/claude-code'),
    'codex': ('@openai/codex', '0.166.0', 'https://github.com/openai/codex'),
    'opencode': ('opencode-ai', '1.19.5', 'https://github.com/sst/opencode'),
}
JCODE = ('v0.99.1', 'https://github.com/1jehuang/jcode/releases/download/{tag}/{name}')
JCODE_ASSET = 'jcode-linux-x86_64.tar.gz'


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fetch(url):
    with urlopen(url, timeout=120) as response:
        return response.read()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('prefix', type=Path, help='New directory to install into')
    args = parser.parse_args()
    if sys.platform != 'linux' or os.uname().machine != 'x86_64':
        parser.error('Pinned artifacts are Linux x86_64 only')
    prefix = args.prefix.resolve()
    prefix.mkdir(parents=True, exist_ok=False)
    npm_dir, scratch = prefix / 'npm', prefix / 'install-home'
    scratch.mkdir()
    env = {key: os.environ[key] for key in ('PATH', 'LANG') if key in os.environ}
    env.update(HOME=str(scratch), npm_config_cache=str(scratch / 'npm-cache'),
               npm_config_update_notifier='false', XDG_CONFIG_HOME=str(scratch / 'config'),
               XDG_DATA_HOME=str(scratch / 'data'), XDG_STATE_HOME=str(scratch / 'state'),
               XDG_CACHE_HOME=str(scratch / 'cache'))
    specs = [f'{package}@{version}' for package, version, _ in NPM.values()]
    subprocess.run(['npm', 'install', '--prefix', str(npm_dir), '--no-audit', '--no-fund',
                    '--no-save', *specs], env=env, check=True)
    npm_version = subprocess.run(['npm', '--version'], env=env, capture_output=True, text=True).stdout.strip()
    node_version = subprocess.run(['node', '--version'], env=env, capture_output=True, text=True).stdout.strip()
    modules = npm_dir / 'node_modules'
    paths = {
        'claude-code': (modules / '.bin/claude').resolve(strict=True),
        # The npm `codex` command is a Node launcher for this native binary;
        # measure the agent itself, as for the other native binaries.
        'codex': next((modules / '@openai').glob('codex-linux-x64/vendor/*/bin/codex')).resolve(strict=True),
        'opencode': (modules / '.bin/opencode').resolve(strict=True),
    }
    provenance = {'node': node_version, 'npm': npm_version, 'agents': {}}
    for name, (package, version, repo) in NPM.items():
        installed = json.loads((modules / package / 'package.json').read_text())['version']
        if installed != version:
            raise SystemExit(f'{package} installed {installed}, pinned {version}')
        view = subprocess.run(['npm', 'view', f'{package}@{version}', 'dist', '--json'], env=env,
                              capture_output=True, text=True, check=True)
        dist = json.loads(view.stdout)
        provenance['agents'][name] = {
            'method': f'npm install {package}@{version} (private prefix)', 'source': repo,
            'package_tarball': dist.get('tarball'), 'package_integrity': dist.get('integrity'),
            'executable': str(paths[name]), 'executable_sha256': sha256(paths[name])}
    tag = JCODE[0]
    sums = fetch(JCODE[1].format(tag=tag, name='SHA256SUMS')).decode()
    expected = next(line.split()[0] for line in sums.splitlines() if line.endswith(JCODE_ASSET))
    url = JCODE[1].format(tag=tag, name=JCODE_ASSET)
    archive = fetch(url)
    if hashlib.sha256(archive).hexdigest() != expected:
        raise SystemExit('jcode archive does not match release SHA256SUMS')
    jcode_dir = prefix / 'jcode'
    jcode_dir.mkdir()
    (jcode_dir / JCODE_ASSET).write_bytes(archive)
    with tarfile.open(jcode_dir / JCODE_ASSET) as tar:
        tar.extractall(jcode_dir, filter='data')
    binary = next(path for path in jcode_dir.rglob('jcode*') if path.is_file()
                  and os.access(path, os.X_OK) and not path.name.endswith('.tar.gz'))
    paths['jcode'] = binary.resolve()
    provenance['agents']['jcode'] = {
        'method': f'GitHub release {tag} {JCODE_ASSET}, verified against SHA256SUMS',
        'source': 'https://github.com/1jehuang/jcode', 'archive_url': url, 'archive_sha256': expected,
        'executable': str(paths['jcode']), 'executable_sha256': sha256(paths['jcode'])}
    shutil.rmtree(scratch)
    (prefix / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    (prefix / 'agents.args').write_text(''.join(f'--agent\n{name}={path}\n' for name, path in paths.items()))
    print(json.dumps(provenance, indent=2))


if __name__ == '__main__':
    main()
