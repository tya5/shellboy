"""Explicit Windows ownership and an exact disjoint execution-inventory audit."""
import json
from pathlib import Path
import re
import subprocess

MANIFEST = Path(__file__).with_name('windows-test-shards.json')


def load(path=MANIFEST):
    return json.loads(path.read_text(encoding='utf-8'))


def check_layout(root, config):
    modules = re.findall(r'^(?:pub(?:\([^)]*\))?\s+)?mod ([a-z_0-9]+);',
                         (root / 'tests/integration/main.rs').read_text(encoding='utf-8'), re.M)
    assigned = config['modules']
    if set(modules) != set(assigned):
        raise ValueError(f'Windows module ownership mismatch: {set(modules) ^ set(assigned)}')
    if any(value not in ['1', '2'] for value in [*assigned.values(), *config['targets'].values()]):
        raise ValueError('Windows owners must be shard 1 or 2')


def selection(target, names, shard, config):
    """Inventory every harness, but execute only its explicitly owned identities."""
    if target != 'test:integration':
        if target not in config['targets']:
            raise ValueError(f'unassigned Windows Cargo target: {target}')
        owner = config['targets'][target]
        return (list(names), [[]]) if owner == shard else ([], None)
    modules = config['modules']
    unknown = {name.partition('::')[0] for name in names} - set(modules)
    if unknown:
        raise ValueError(f'unassigned Windows integration modules: {unknown}')
    selected = [name for name in names if modules[name.partition('::')[0]] == shard]
    return selected, exact_batches(selected) if selected else None


def exact_batches(names, limit=24000):
    """Bound exact-name argument batches below Windows' 32767 UTF-16 limit."""
    batches, batch = [], ['--exact']
    for name in names:
        candidate = [*batch, name]
        size = len(subprocess.list2cmdline(candidate).encode('utf-16-le')) // 2
        if size > limit:
            if len(batch) == 1:
                raise ValueError('test identity exceeds Windows command-line budget')
            batches.append(batch)
            batch = ['--exact', name]
            if len(subprocess.list2cmdline(batch).encode('utf-16-le')) // 2 > limit:
                raise ValueError('test identity exceeds Windows command-line budget')
        else:
            batch = candidate
    if len(batch) > 1:
        batches.append(batch)
    return batches



def audit(records):
    if len(records) != 2 or {r.get('windows_shard') for r in records} != {'1', '2'}:
        raise ValueError('require exactly one result from each Windows shard')
    first, second = records
    for key in ['source_sha', 'platform', 'test_threads', 'tests', 'ignored']:
        if first[key] != second[key]:
            raise ValueError(f'shard full inventories differ: {key}')
    if first['platform'] != 'win32':
        raise ValueError('Windows audit requires native Windows inventories')
    for record in records:
        if record['failures']:
            raise ValueError('a shard recorded test failures')
        if len(record['tests']) != len(set(record['tests'])):
            raise ValueError('duplicate full inventory identity')
    for full, part in [('tests', 'executed_tests'), ('ignored', 'executed_ignored')]:
        combined = first[part] + second[part]
        if len(combined) != len(set(combined)) or set(combined) != set(first[full]):
            raise ValueError(f'shard execution is not an exact disjoint union: {full}')
    return {'source_sha': first['source_sha'], 'tests': len(first['tests']),
            'ignored': len(first['ignored']), 'threads': first['test_threads']}
