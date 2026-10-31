#!/usr/bin/env python3
"""Apply exact-once text replacements as a preflighted batch of existing files."""
import argparse
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import tempfile

REQUEST_LIMIT = 4 * 1024 * 1024
TEXT_LIMIT = 32 * 1024 * 1024


class MatchError(ValueError):
    pass


class RecoveryError(RuntimeError):
    def __init__(self, directories):
        super().__init__('Rollback incomplete; retained originals require manual recovery')
        self.directories = directories


def identity(path):
    value = path.lstat()
    if not stat.S_ISREG(value.st_mode):
        raise ValueError('Not a regular file: ' + str(path))
    return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns,
            value.st_mode, value.st_uid, value.st_gid)


def plan(rules, root):
    if not isinstance(rules, list) or len(rules) > 256:
        raise ValueError('Expected an array of at most 256 replacement objects')
    root = Path(root).resolve(strict=True)
    if not root.is_dir():
        raise ValueError('Root is not a directory')
    files = {}
    total = 0
    for index, rule in enumerate(rules):
        if (not isinstance(rule, dict) or set(rule) != {'file', 'before', 'after'} or
                not all(isinstance(value, str) for value in rule.values()) or
                not rule['file'] or not rule['before']):
            raise ValueError('Rule must have string file/before/after and a nonempty file/needle')
        path = root / rule['file']
        if path.is_symlink():
            raise ValueError('Symlink target is not allowed')
        path = path.resolve(strict=True)
        path.relative_to(root)
        if path not in files:
            key = identity(path)
            info = path.stat()
            if info.st_nlink != 1 or not info.st_mode & 0o222:
                raise ValueError('Linked or read-only target: ' + str(path))
            if hasattr(os, 'getuid') and info.st_uid != os.getuid():
                raise ValueError('Target is not owned by this user')
            total += info.st_size
            if total > TEXT_LIMIT:
                raise ValueError('Aggregate source exceeds 32 MiB')
            original = path.read_bytes()
            if len(original) != info.st_size or identity(path) != key:
                raise ValueError('Target changed while being read')
            if b'\0' in original:
                raise ValueError('Binary target is not allowed')
            text = original.decode('utf-8')
            files[path] = dict(path=path, key=key, old=original, text=text,
                               label=path.relative_to(root).as_posix())
        item = files[path]
        before = rule['before']
        first = item['text'].find(before)
        if first < 0 or item['text'].find(before, first + 1) >= 0:
            raise MatchError('Rule %d must match exactly once: %s' % (index, item['label']))
        item['text'] = item['text'][:first] + rule['after'] + item['text'][first + len(before):]
    for item in files.values():
        item['new'] = item.pop('text').encode('utf-8')
    if sum(len(item['new']) for item in files.values()) > TEXT_LIMIT:
        raise ValueError('Aggregate result exceeds 32 MiB')
    return [item for item in files.values() if item['old'] != item['new']]


def unchanged(item, new=False):
    path = item['path']
    return identity(path) == item['new_key' if new else 'key'] and path.read_bytes() == item['new' if new else 'old']


def apply(rules, root):
    files = plan(rules, root)
    stages, committed, retain = [], [], False
    failure = None
    warnings = []
    try:
        for item in files:
            directory = Path(tempfile.mkdtemp(prefix='.replace-', dir=item['path'].parent))
            item['stage'] = directory
            stages.append(item)
            target = directory / 'new'
            with target.open('xb') as stream:
                stream.write(item['new'])
                stream.flush()
                os.fsync(stream.fileno())
            os.chmod(target, stat.S_IMODE(item['key'][4]))
            if hasattr(os, 'getuid') and target.stat().st_gid != item['key'][6]:
                os.chown(target, -1, item['key'][6])
            item['new_key'] = identity(target)
            os.link(item['path'], directory / 'old')
        for item in files:
            if not unchanged(item):
                raise ValueError('Target changed before commit: ' + item['label'])
        for item in files:
            if not unchanged(item):
                raise ValueError('Target changed during commit: ' + item['label'])
            committed.append(item)  # Recover an interrupt immediately after rename.
            os.replace(item['stage'] / 'new', item['path'])
    except BaseException as error:
        failure = error
        failures = []
        for item in reversed(committed):
            try:
                if unchanged(item):
                    continue
                if not unchanged(item, new=True):
                    raise ValueError('Committed target changed externally')
                os.replace(item['stage'] / 'old', item['path'])
            except BaseException:
                failures.append(item)
        if failures:
            retain = True
            raise RecoveryError([str(item['stage']) for item in stages])
        raise
    finally:
        if not retain:
            for item in stages:
                try:
                    shutil.rmtree(item['stage'])
                except OSError:
                    warnings.append(str(item['stage']))
            if warnings and failure is not None:
                failure.cleanup_warnings = warnings
    return dict(ok=True, replacements=len(rules), changed_files=[item['label'] for item in files],
                cleanup_warnings=warnings)


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        'stdin: [{"file":"existing.txt","before":"old","after":"new"}]. '
        'All rules preflight before writes; rules on one file are sequential. '
        'No new files, symlinks, hardlinks, binaries or out-of-root paths. '
        'Atomic per-file with rollback, not a crash-safe multi-file transaction. '
        'Exit 0 success, 1 nonunique needle, 2 input/I/O, 3 manual recovery.'))
    parser.add_argument('--root', type=Path, default=Path('.'), help='Allowed source root (default: cwd)')
    args = parser.parse_args()
    try:
        raw = sys.stdin.buffer.read(REQUEST_LIMIT + 1)
        if len(raw) > REQUEST_LIMIT:
            raise ValueError('Request exceeds 4 MiB')
        result = apply(json.loads(raw.decode('utf-8')), args.root)
        code = 0
    except RecoveryError as error:
        result, code = dict(ok=False, error=str(error), recovery_dirs=error.directories), 3
    except (OSError, ValueError, KeyboardInterrupt) as error:
        message = 'Invalid UTF-8 input/target' if isinstance(error, UnicodeError) else str(error)
        result, code = dict(ok=False, error=message or 'Interrupted'), 1 if isinstance(error, MatchError) else 2
        if getattr(error, 'cleanup_warnings', None):
            result['cleanup_warnings'] = error.cleanup_warnings
    print(json.dumps(result, separators=(',', ':')))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
