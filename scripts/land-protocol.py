#!/usr/bin/env python3
"""Fix the protocol and migration numbers once for a composed landing batch (#356, #359).

Usage: python3 scripts/land-protocol.py [BASE_REF]   (default origin/main)

Protocol (#356): wire-changing patches keep PROTOCOL_VERSION and the schema's
three version fields at main's value and title their docs section
`Protocol next (#N)`. This script bumps only when the schema differs from BASE
(ignoring the version fields) or a docs heading says `Protocol next`:
PROTOCOL_VERSION and the schema `$id`/`title`/`protocol_version` become
BASE + 1 by exact text replacement, every `Protocol next` under docs/ becomes
`Protocol N` (mentions inside a code span such as `Protocol next` are left
alone, and `#protocol-next-` anchors follow the heading).

Migrations (#359): a patch adds `migrations/next_<slug>.sql` and registers it
under the name `next_<slug>` with a provisional number. In batch commit order
each one gets the next free number N: the file becomes `NNNN_<slug>.sql`, the
number in its ledger line, the following `PRAGMA user_version` line (in the
file and in src/storage/current.sql), its MIGRATION_NAMES entry and its
MIGRATIONS `version:` becomes N, and the token `next_<slug>` becomes
`NNNN_<slug>` in every tracked text file.

Both land in one commit, "Protocol N: #A, #B / Migrations M-K". Running it
again changes nothing; inconsistent or half-numbered states are refused
before anything is written.
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

REGISTRY = 'src/storage.rs'
EXECUTOR = 'src/storage/migrate.rs'
CURRENT = 'src/storage/current.sql'
NUMBERED_FILE = re.compile(r'^migrations/(\d{4})_(\w+)\.sql$')
NEXT_FILE = re.compile(r'^migrations/next_(\w+)\.sql$')
NEXT_REFERENCES = (  # a `next_` migration named in a list must have its file
    (REGISTRY, re.compile(r'\(\d+, "next_(\w+)"\)')),
    (EXECUTOR, re.compile(r'migrations/next_(\w+)\.sql')),
    (CURRENT, re.compile(r"VALUES\(\d+,'next_(\w+)'\)")),
)


def fail(message):
    sys.exit(f'land-protocol: {message}')


def git(*args, check=True):
    result = subprocess.run(['git', *args], capture_output=True)
    if check and result.returncode != 0:
        fail(f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout if check else result


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


class Tree:
    """Tracked text files of the work tree, edited in memory and written together."""

    def __init__(self, root, base):
        self.root, self.base = root, base
        self.names = [n for n in git('-C', str(root), 'ls-files', '-z').decode().split('\0') if n]
        self.texts, self.original, self.renames = {}, {}, []

    def show(self, ref, path):
        return git('-C', str(self.root), 'show', f'{ref}:{path}').decode('utf-8')

    def text(self, name):
        if name not in self.texts:
            try:
                self.texts[name] = (self.root / name).read_bytes().decode('utf-8')
            except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
                self.texts[name] = None
            self.original[name] = self.texts[name]
        return self.texts[name]

    def all_texts(self):
        return [(name, self.text(name)) for name in self.names if self.text(name) is not None]

    def apply(self):
        changed = []
        for name, text in self.texts.items():
            if text is not None and text != self.original[name]:
                (self.root / name).write_bytes(text.encode('utf-8'))
                changed.append(name)
        if changed:
            git('-C', str(self.root), 'add', '--', *changed)
        for old, new in self.renames:
            git('-C', str(self.root), 'mv', old, new)
        return bool(changed or self.renames)


def plan_protocol(tree):
    """Edit the protocol number in memory; return the commit subject part or None."""
    base = tree.base
    base_version = version(tree.show(base, RUST), f'{base}:{RUST}')
    head_version = version(tree.show('HEAD', RUST), f'HEAD:{RUST}')
    target = base_version + 1
    if head_version not in (base_version, target):
        fail(f'HEAD has PROTOCOL_VERSION {head_version} but {base} has {base_version}; '
             f'only {base_version} (unbumped) or {target} (already landed) is valid. '
             'Patches must not change the protocol number.')

    docs = [(n, tree.text(n)) for n in tree.names if n.startswith('docs/') and tree.text(n) is not None]
    issues = set()
    has_next = False
    for _, text in docs:
        for match in NEXT_HEADING.finditer(text):
            has_next = True
            issues.update(int(n) for n in re.findall(r'#(\d+)', match.group(1) or ''))

    schema_changed = (schema_body(tree.show(base, SCHEMA), f'{base}:{SCHEMA}')
                      != schema_body(tree.show('HEAD', SCHEMA), f'HEAD:{SCHEMA}'))
    if head_version == base_version and not (schema_changed or has_next):
        print(f'land-protocol: no wire change against {base}; protocol stays {base_version}')
        return None

    tree.texts[RUST] = set_fields(tree.text(RUST), (VERSION,), target, RUST)
    tree.texts[SCHEMA] = set_fields(tree.text(SCHEMA), SCHEMA_FIELDS, target, SCHEMA)
    for name, text in docs:
        if name != SCHEMA:
            tree.texts[name] = NEXT_ANCHOR.sub(f'#protocol-{target}-', NEXT.sub(f'Protocol {target}', text))
    if all(text == tree.original[name] for name, text in tree.texts.items()):
        print(f'land-protocol: already Protocol {target}; nothing to do for the protocol')
        return None
    subject = f'Protocol {target}'
    if issues:
        subject += ': ' + ', '.join(f'#{n}' for n in sorted(issues))
    return subject


def numbered(paths, where):
    found = {}
    for path in paths:
        match = NUMBERED_FILE.match(path)
        if match:
            number = int(match.group(1))
            if number in found:
                fail(f'{where}: migrations {found[number]} and {path} share number {number}')
            found[number] = path
    return found


def replace_one(tree, name, pattern, number, what):
    text = tree.text(name)
    if text is None:
        fail(f'{name} is missing; cannot number {what}')
    text, count = pattern.subn(lambda m: f'{m.group(1)}{number}{m.group(2)}', text)
    if count != 1:
        fail(f'{name}: expected exactly one {what}, found {count}; fix the patch to the '
             'next-migration convention in docs/development/setup.md')
    tree.texts[name] = text


def plan_migrations(tree):
    """Number every `next_` migration in memory; return the commit subject part or None."""
    base, root = tree.base, str(tree.root)
    base_paths = [n for n in git('-C', root, 'ls-tree', '-r', '-z', '--name-only', base, '--',
                                 'migrations').decode().split('\0') if n]
    if any(NEXT_FILE.match(n) for n in base_paths):
        fail(f'{base} itself has an unnumbered migrations/next_*.sql; land it first')
    base_numbered = numbered(base_paths, base)
    base_max = max(base_numbered, default=0)
    for path in sorted(set(tree.names) - set(base_paths)):
        match = NUMBERED_FILE.match(path)
        if match and int(match.group(1)) <= base_max:
            fail(f'{path} collides with {base_numbered.get(int(match.group(1)), "a migration")} on {base}; '
                 f'rename it to migrations/next_<slug>.sql and register it as next_<slug>')
    head_numbered = numbered(tree.names, 'HEAD')
    head_max = max(head_numbered, default=0)
    if sorted(head_numbered) != list(range(1, head_max + 1)):
        fail(f'numbered migrations are not contiguous from 1 to {head_max}')

    slugs = [NEXT_FILE.match(n).group(1) for n in tree.names if NEXT_FILE.match(n)]
    for name, pattern in NEXT_REFERENCES:
        text = tree.text(name) if name in tree.names else None
        for slug in pattern.findall(text or ''):
            if slug not in slugs:
                fail(f'{name} names next_{slug} but migrations/next_{slug}.sql is missing '
                     '(half-numbered batch?)')
    if not slugs:
        return None

    numbered_slugs = {NUMBERED_FILE.match(p).group(2) for p in head_numbered.values()}
    registry = tree.text(REGISTRY) or ''
    order = []
    commits = git('-C', root, 'rev-list', '--reverse', f'{base}..HEAD').decode().split()
    for slug in slugs:
        if slug in numbered_slugs:
            fail(f'migrations/next_{slug}.sql and a numbered *_{slug}.sql both exist (half-numbered batch?)')
        token = f'next_{slug}'
        if git('-C', root, 'grep', '-q', '-w', '-F', token, base, '--', check=False).returncode == 0:
            fail(f'{token} already appears on {base}; choose a slug whose next_<slug> token is new')
        added = git('-C', root, 'log', '--format=%H', '--diff-filter=A', f'{base}..HEAD', '--',
                    f'migrations/{token}.sql').decode().split()
        position = commits.index(added[-1]) if added and added[-1] in commits else len(commits)
        provisional = re.search(r'\((\d+), "' + token + r'"\)', registry)
        order.append((position, int(provisional.group(1)) if provisional else 0, slug))

    assigned = []
    for offset, (_, _, slug) in enumerate(sorted(order), start=1):
        number = head_max + offset
        old, new = f'next_{slug}', f'{number:04d}_{slug}'
        pair = re.compile(r"(VALUES\()\d+(,'" + old + r"'\);[ \t]*\r?\nPRAGMA user_version=)\d+(?=;)")

        def both(m, number=number):
            return f'{m.group(1)}{number}{m.group(2)}{number}'

        for name in (f'migrations/{old}.sql', CURRENT):
            text = tree.text(name)
            if text is None:
                fail(f'{name} is missing; cannot number {old}')
            text, count = pair.subn(both, text)
            if count != 1:
                fail(f"{name}: expected exactly one ledger line VALUES(N,'{old}') followed by "
                     f'PRAGMA user_version=N, found {count}; see docs/development/setup.md')
            tree.texts[name] = text
        replace_one(tree, REGISTRY, re.compile(r'(\()\d+(, "' + old + r'"\))'), number,
                    f'MIGRATION_NAMES entry ({old})')
        replace_one(tree, EXECUTOR, re.compile(
            r'(version: )\d+(,\s*sql: include_str!\("\.\./\.\./migrations/' + old + r'\.sql"\))'),
            number, f'MIGRATIONS entry for {old}.sql')
        token = re.compile(r'(?<!\w)' + old + r'(?!\w)')
        for name, text in tree.all_texts():
            tree.texts[name] = token.sub(new, text)
        tree.renames.append((f'migrations/{old}.sql', f'migrations/{new}.sql'))
        assigned.append(number)

    names = tree.texts[REGISTRY].split('MIGRATION_NAMES', 1)[-1].split('];', 1)[0]
    registered = [int(n) for n in re.findall(r'\((\d+), "', names)]
    if registered != list(range(1, assigned[-1] + 1)):
        fail('MIGRATION_NAMES would not be 1..N in order after numbering; keep the next '
             'entries in batch commit order')
    first, last = assigned[0], assigned[-1]
    return f'Migration {first}' if first == last else f'Migrations {first}-{last}'


def main():
    base = sys.argv[1] if len(sys.argv) > 1 else 'origin/main'
    if len(sys.argv) > 2:
        fail('usage: land-protocol.py [BASE_REF]')
    root = Path(git('rev-parse', '--show-toplevel').decode().strip())
    if git('-C', str(root), 'status', '--porcelain', '--untracked-files=no').strip():
        fail('tracked files have uncommitted changes; commit or stash them first')
    tree = Tree(root, base)
    parts = [part for part in (plan_protocol(tree), plan_migrations(tree)) if part]
    if not parts or not tree.apply():
        print('land-protocol: nothing to do')
        return
    message = ' / '.join(parts)
    git('-C', str(root), 'commit', '-q', '-m', message)
    print(f'land-protocol: committed "{message}"')


if __name__ == '__main__':
    main()
