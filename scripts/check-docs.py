#!/usr/bin/env python3
"""Check the actual executable's complete bundled catalogue without an owner/API."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    calls = 0
    with tempfile.TemporaryDirectory(prefix='shellboy-docs-offline-') as directory:
        root = Path(directory)

        def cli(*words):
            nonlocal calls
            result = subprocess.run(
                [str(binary), '--config', str(root / 'absent.toml'),
                 '--data-dir', str(root / 'uncreated'), 'docs', *words],
                cwd=root, env={}, capture_output=True, timeout=20,
            )
            assert result.returncode == 0 and not result.stderr, result.stderr
            value = json.loads(result.stdout)
            assert value['ok'] is True, value
            calls += 1
            return value['result']

        pending = cli('list')['items']
        found = {}
        while pending:
            item = pending.pop()
            assert item['id'] not in found, 'Cycle or duplicated document ID'
            found[item['id']] = item
            pending.extend(cli('list', '--parent', item['id'])['items'])
        versions = set()
        bundled_links = 0
        for item in found.values():
            guide = cli('get', '--id', item['id'], '--limit-bytes', '65536')
            assert 'next_cursor' not in guide
            body = guide['text'].encode()
            assert hashlib.sha256(body).hexdigest() == guide['content_hash']
            assert guide['content_hash'] == item['content_hash']
            versions.add(guide['version'])
            for section in guide['sections']:
                body[:section['start']].decode()
                body[section['start']:section['end']].decode()
                assert 0 <= section['start'] < section['end'] <= len(body)
            for link in guide['links']:
                if link['kind'] == 'bundled':
                    assert link['resolved'] and link['target_id'] in found, link
                    bundled_links += 1
        assert len(versions) == 1
        assert not list(root.iterdir()), 'Offline docs created local state'
    evidence = {
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'cli_checks': calls,
        'guides': sum(item['kind'] == 'guide' for item in found.values()),
        'categories': sum(item['kind'] == 'category' for item in found.values()),
        'bundled_links': bundled_links,
        'version': versions.pop(),
        'empty_cwd': True, 'environment_cleared': True,
        'config_absent': True, 'owner_api_not_required': True,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(evidence, ensure_ascii=False))


if __name__ == '__main__':
    main()
