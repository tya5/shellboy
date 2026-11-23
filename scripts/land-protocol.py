#!/usr/bin/env python3
"""Bump the protocol number once for a composed landing batch (#356).

Usage: python3 scripts/land-protocol.py [BASE_REF]   (default origin/main)

Wire-changing patches keep PROTOCOL_VERSION and the schema's three version
fields at main's value and title their docs section `Protocol next (#N)`.
This script bumps only when the schema differs from BASE (ignoring the version
fields) or a docs heading says `Protocol next`: PROTOCOL_VERSION and the schema
`$id`/`title`/`protocol_version` become BASE + 1 by exact text replacement,
every `Protocol next` under docs/ becomes `Protocol N` (mentions inside a code
span such as `Protocol next` are left alone, and `#protocol-next-` anchors
follow the heading), and one commit "Protocol N: #A, #B" is created. Running it
again changes nothing.
"""
import json
from pathlib import Path
import re
import subprocess
import sys

RUST = 'src/protocol.rs'
SCHEMA = 'docs/reference/protocol.schema.json'
VERSION = re.compile(r'^(pub const PROTOCOL_VERSION: u32 = )(\d+)(;)', re.M)
SCHEMA_FIELDS = (
    re.compile(r'^(  "\$id": "[^"\n]*protocol-v)(\d+)(\.json")', re.M),
    re.compile(r'^(  "title": "[^"\n]* v)(\d+)(")', re.M),
    re.compile(r'^(  "protocol_version": )(\d+)(\b)', re.M),
)
NEXT = re.compile(r'(?<!`)Protocol next\b')
NEXT_ANCHOR = re.compile(r'#protocol-next-')
NEXT_HEADING = re.compile(r'^#{1,6}[ \t].*?(?<!`)Protocol next\b(?:[ \t]*\(([^)\n]*)\))?', re.M)


def fail(message):
    sys.exit(f'land-protocol: {message}')


def git(*args):
    result = subprocess.run(['git', *args], capture_output=True)
    if result.returncode != 0:
        fail(f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def version(text, where):
    found = VERSION.findall(text)
    if len(found) != 1:
        fail(f'{where}: expected exactly one `pub const PROTOCOL_VERSION: u32 = N;`')
    return int(found[0][1])


def schema_body(text, where):
    try:
        value = json.loads(text)
    except ValueError as error:
        fail(f'{where}: invalid JSON ({error})')
    for key in ('$id', 'title', 'protocol_version'):
        value.pop(key, None)
    return value


def set_fields(text, patterns, number, where):
    for pattern in patterns:
        text, count = pattern.subn(lambda m: f'{m.group(1)}{number}{m.group(3)}', text)
        if count != 1:
            fail(f'{where}: expected one match for {pattern.pattern!r}, found {count}')
    return text


def main():
    base = sys.argv[1] if len(sys.argv) > 1 else 'origin/main'
    if len(sys.argv) > 2:
        fail('usage: land-protocol.py [BASE_REF]')
    root = Path(git('rev-parse', '--show-toplevel').decode().strip())
    if git('-C', str(root), 'status', '--porcelain', '--untracked-files=no').strip():
        fail('tracked files have uncommitted changes; commit or stash them first')

    def show(ref, path):
        return git('-C', str(root), 'show', f'{ref}:{path}').decode('utf-8')

    base_version = version(show(base, RUST), f'{base}:{RUST}')
    head_version = version(show('HEAD', RUST), f'HEAD:{RUST}')
    target = base_version + 1
    if head_version not in (base_version, target):
        fail(f'HEAD has PROTOCOL_VERSION {head_version} but {base} has {base_version}; '
             f'only {base_version} (unbumped) or {target} (already landed) is valid. '
             'Patches must not change the protocol number.')

    docs = {}
    for name in git('-C', str(root), 'ls-files', '-z', '--', 'docs').decode().split('\0'):
        if not name:
            continue
        try:
            docs[name] = (root / name).read_bytes().decode('utf-8')
        except (UnicodeDecodeError, FileNotFoundError):
            continue
    issues = set()
    has_next = False
    for text in docs.values():
        for match in NEXT_HEADING.finditer(text):
            has_next = True
            issues.update(int(n) for n in re.findall(r'#(\d+)', match.group(1) or ''))

    schema_changed = (schema_body(show(base, SCHEMA), f'{base}:{SCHEMA}')
                      != schema_body(show('HEAD', SCHEMA), f'HEAD:{SCHEMA}'))
    if head_version == base_version and not (schema_changed or has_next):
        print(f'land-protocol: no wire change against {base}; protocol stays {base_version}')
        return

    changed = []

    def write(name, old, new):
        if new != old:
            (root / name).write_bytes(new.encode('utf-8'))
            changed.append(name)

    rust = (root / RUST).read_bytes().decode('utf-8')
    write(RUST, rust, set_fields(rust, (VERSION,), target, RUST))
    schema = (root / SCHEMA).read_bytes().decode('utf-8')
    write(SCHEMA, schema, set_fields(schema, SCHEMA_FIELDS, target, SCHEMA))
    for name, text in docs.items():
        if name == SCHEMA:
            continue
        write(name, text, NEXT_ANCHOR.sub(f'#protocol-{target}-', NEXT.sub(f'Protocol {target}', text)))

    if not changed:
        print(f'land-protocol: already Protocol {target}; nothing to do')
        return
    message = f'Protocol {target}'
    if issues:
        message += ': ' + ', '.join(f'#{n}' for n in sorted(issues))
    git('-C', str(root), 'commit', '-q', '-m', message, '--', *changed)
    print(f'land-protocol: committed "{message}" ({len(changed)} files)')


if __name__ == '__main__':
    main()
