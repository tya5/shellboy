#!/usr/bin/env python3
"""Require every integration suite to be registered exactly once."""
from pathlib import Path
import re
import tomllib

root = Path(__file__).resolve().parent.parent
manifest = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))
if manifest['package'].get('autotests', True):
    raise SystemExit('explicit integration layout requires autotests=false')
registered = []
for target in manifest['test']:
    path = root / target['path']
    if target['name'] == 'integration':
        for source in re.findall(r'^#\[path = "([^"]+)"\]\nmod [a-z_0-9]+;', path.read_text(encoding='utf-8'), re.M):
            registered.append((path.parent / source).resolve())
    else:
        registered.append(path.resolve())
expected = {path.resolve() for path in (root / 'tests').glob('*.rs')}
assert len(registered) == len(set(registered)), 'duplicate integration suite registration'
assert set(registered) == expected, {'missing': sorted(map(str, expected-set(registered))),
                                     'extra': sorted(map(str, set(registered)-expected))}
print(f'Integration layout: {len(registered)} suites, each registered once')
