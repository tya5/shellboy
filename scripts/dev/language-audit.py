#!/usr/bin/env python3
"""Read-only Japanese Rust string inventory; conservative a/b/c classification."""
import argparse
from collections import Counter
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import sys

JAPANESE = re.compile('[\u3000-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uff66-\uff9f\U00020000-\U0003134f]')
RAW = re.compile(r'(?:br|cr|r)(#*)"')
CHAR = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")


@dataclass
class Token:
    kind: str
    text: str
    start: int
    end: int


def tokens(source):
    """Skip comments, retain string/char boundaries, and reject unfinished input."""
    result = []
    i = 0
    while i < len(source):
        start = i
        if source[i].isspace():
            i += 1
            continue
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = len(source) if end < 0 else end
            continue
        if source.startswith('/*', i):
            depth = 1
            i += 2
            while i < len(source) and depth:
                if source.startswith('/*', i):
                    depth += 1
                    i += 2
                elif source.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            if depth:
                raise ValueError(f'unterminated block comment at {start}')
            continue
        raw = RAW.match(source, i)
        if raw:
            end = source.find('"' + raw[1], raw.end())
            if end < 0:
                raise ValueError(f'unterminated raw string at {start}')
            i = end + 1 + len(raw[1])
            kind = 'raw_string'
        elif source[i] == '"' or source.startswith(('b"', 'c"'), i):
            i += 1 if source[i] == '"' else 2
            while i < len(source) and source[i] != '"':
                i += 2 if source[i] == '\\' else 1
            if i >= len(source):
                raise ValueError(f'unterminated string at {start}')
            i += 1
            kind = 'string'
        elif source[i] == "'" and (char := CHAR.match(source, i)):
            i = char.end()
            kind = 'char'
        elif source[i].isalpha() or source[i] == '_':
            i += 1
            while i < len(source) and (source[i].isalnum() or source[i] == '_'):
                i += 1
            kind = 'word'
        else:
            i += 1
            kind = 'punct'
        result.append(Token(kind, source[start:i], start, i))
    return result


def groups(items):
    stack, pairs = [], {}
    for i, token in enumerate(items):
        if token.kind != 'punct':
            continue
        if token.text in '([{':
            stack.append(i)
        elif token.text in ')]}':
            if not stack or items[stack[-1]].text != {')': '(', ']': '[', '}': '{'}[token.text]:
                raise ValueError(f'unbalanced delimiter at {token.start}')
            left = stack.pop()
            pairs[left] = i
    if stack:
        raise ValueError(f'unclosed delimiter at {items[stack[-1]].start}')
    return pairs


def ranges(items, pairs):
    fixtures, localized, modules = [], [], []
    texts = [t.text for t in items]
    for i, token in enumerate(items):
        if token.text == '#' and texts[i + 1:i + 2] == ['[']:
            end = pairs[i + 1]
            attr = ''.join(texts[i + 2:end])
            if attr == 'cfg(test)' or attr == 'test' or re.match(r'\w+::test(?:\(|$)', attr):
                j = end + 1
                # Multiple attributes may precede the same item.
                while texts[j:j + 2] == ['#', '[']:
                    j = pairs[j + 1] + 1
                item = j
                while j < len(items) and texts[j] not in ('{', ';'):
                    j += 1
                if j < len(items):
                    stop = pairs[j] if texts[j] == '{' else j
                    fixtures.append((token.start, items[stop].end))
                    if attr == 'cfg(test)' and texts[item:item + 1] == ['mod'] and texts[j] == ';':
                        modules.append(texts[item + 1])
        if token.text == 'local' and texts[i + 1:i + 3] == ['!', '(']:
            stop = pairs[i + 2]
            j, arg, begin = i + 3, 0, i + 3
            while j < stop:
                if texts[j] == ',':
                    if arg == 1:
                        localized.append((items[begin].start, items[j].start))
                    arg += 1
                    begin = j + 1
                j = pairs[j] + 1 if j in pairs else j + 1
        if texts[i:i + 4] == ['UiLanguage', ':', ':', 'Ja']:
            # Only an explicit Japanese match arm, never a file-wide exemption.
            j = i + 4
            while j < len(items) and texts[j] not in ('=', ';', '{'):
                j += 1
            if texts[j:j + 2] == ['=', '>']:
                begin = j + 2
                j = begin
                while j < len(items) and texts[j] not in (',', '}', ';'):
                    j = pairs[j] + 1 if j in pairs else j + 1
                if begin < len(items):
                    localized.append((items[begin].start, items[j - 1].end))
    return fixtures, localized, modules


def item_ranges(items, pairs):
    """Find conservative function/const/static item spans; unknown forms use the file."""
    texts = [t.text for t in items]
    reverse = {right: left for left, right in pairs.items()}
    spans = []
    for i, token in enumerate(items):
        if token.text not in ('fn', 'const', 'static'):
            continue
        if i + 1 >= len(items) or items[i + 1].kind != 'word':
            continue
        if token.text == 'const' and texts[i + 1] == 'fn':
            continue
        start = i
        # Include immediately attached attributes and visibility/async modifiers.
        while start:
            if texts[start - 1] in ('pub', 'async', 'unsafe', 'const', 'default'):
                start -= 1
            elif texts[start - 1] == ')' and start - 1 in reverse:
                left = reverse[start - 1]
                if left and texts[left - 1] == 'pub':
                    start = left - 1
                else:
                    break
            elif texts[start - 1] == ']' and start - 1 in reverse:
                left = reverse[start - 1]
                if left and texts[left - 1] == '#':
                    start = left - 1
                else:
                    break
            else:
                break
        j, has_initializer = i + 2, False
        while j < len(items):
            text = texts[j]
            if token.text == 'fn' and text == '{':
                spans.append((start, pairs[j] + 1))
                break
            if text == ';':
                if token.text != 'fn' and has_initializer:
                    spans.append((start, j + 1))
                break
            if token.text != 'fn' and text == '=':
                has_initializer = True
            if text == '}' or (token.text != 'fn' and text in (',', '>')):
                break
            j = pairs[j] + 1 if j in pairs else j + 1
    return spans


def scope_fingerprint(items, spans, index, cache):
    # Keep enclosing functions (rather than nested constants) so local language
    # predicates cannot change without invalidating the reviewed decision.
    enclosing = [(a, b) for a, b in spans if a <= index < b]
    span = max(enclosing, key=lambda pair: pair[1] - pair[0]) if enclosing else (0, len(items))
    if span not in cache:
        a, b = span
        data = [(t.kind, t.text) for t in items[a:b]]
        cache[span] = hashlib.sha256(json.dumps(data).encode()).hexdigest()
    return cache[span]


def decoded(token):
    if token.kind == 'raw_string':
        return token.text
    return re.sub(r'\\(?:u\{([0-9a-fA-F_]+)\}|.)',
                  lambda m: chr(int(m[1].replace('_', ''), 16)) if m[1] else m[0],
                  token.text, flags=re.DOTALL)


def inventory(root, classifications=None):
    paths = sorted((root / 'src').rglob('*.rs'))
    prepared, fixture_paths = {}, set()
    for path in paths:
        source = path.read_text(encoding='utf-8')
        items = tokens(source)
        pairs = groups(items)
        fixture, localized, modules = ranges(items, pairs)
        spans = item_ranges(items, pairs)
        prepared[path] = source, items, fixture, localized, spans
        for module in modules:
            parent = path.parent if path.name in ('lib.rs', 'main.rs', 'mod.rs') else path.with_suffix('')
            fixture_paths.update([parent / (module + '.rs'), parent / module / 'mod.rs'])
    if classifications is not None and not isinstance(classifications, dict):
        raise ValueError('reviewed decisions must be a JSON object')
    decisions = classifications or {}
    seen, records = set(), []
    for path, (source, items, fixture, localized, spans) in prepared.items():
        occurrences, fingerprints = Counter(), {}
        for index, token in enumerate(items):
            if token.kind not in ('string', 'raw_string') or not JAPANESE.search(decoded(token)):
                continue
            fingerprint = scope_fingerprint(items, spans, index, fingerprints)
            digest = hashlib.sha256(token.text.encode()).hexdigest()[:16]
            occurrences[digest] += 1
            relative = path.relative_to(root).as_posix()
            identity = f'{relative}:{digest}:{occurrences[digest]}'
            seen.add(identity)
            contains = lambda spans: any(a <= token.start < b for a, b in spans)
            if path in fixture_paths or contains(fixture):
                category, reason = 'b', 'test-only item/module'
            elif contains(localized):
                category, reason = 'a', 'explicit Japanese local! argument or language arm'
            else:
                category, reason = 'c', 'review required; not proof this is generated copy'
            if identity in decisions:
                entry = decisions[identity]
                if (not isinstance(entry, dict)
                        or entry.get('category') not in ('a', 'b', 'c', 'protected')
                        or not isinstance(entry.get('reason'), str) or not entry['reason'].strip()):
                    raise ValueError(f'invalid reviewed decision: {identity}')
                if entry.get('source_tokens_sha256') != fingerprint:
                    raise ValueError(f'reviewed source changed; re-review required: {identity}')
                if entry['category'] == 'protected' and not all(
                    isinstance(entry.get(key), str) and entry[key].strip()
                    for key in ('canonical_evidence', 'projection_evidence')
                ):
                    raise ValueError(f'protected token needs canonical and UI projection evidence: {identity}')
                category, reason = entry['category'], entry['reason']
            records.append(dict(id=identity, path=relative, line=source.count('\n', 0, token.start) + 1,
                                kind=token.kind, category=category, reason=reason, literal=token.text,
                                source_tokens_sha256=fingerprint))
            if category == 'protected':
                records[-1].update({key: decisions[identity][key]
                                    for key in ('canonical_evidence', 'projection_evidence')})
    stale = set(decisions) - seen
    if stale:
        raise ValueError('stale reviewed decisions: ' + ', '.join(sorted(stale)))
    return dict(files_scanned=len(paths), counts=dict(Counter(r['category'] for r in records)), records=records)


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        'a: explicit Japanese UI side; b: fixture; c: pending review/English cleanup; '
        'protected: individually reviewed canonical tokens with canonical and UI projection evidence. '
        'Comments and character literals are not string literals. c is conservative, '
        'never authorization to translate source data. No writes, DB or network access. '
        'Exit 0: inventory (or zero c under --check); 1: remaining c; 2: input error.'))
    parser.add_argument('--repo', type=Path, default=Path('.'))
    parser.add_argument('--classifications', type=Path,
                        help='Reviewed ID -> {category,reason,source_tokens_sha256} JSON map')
    formats = parser.add_mutually_exclusive_group()
    formats.add_argument('--summary', action='store_true', help='Omit individual literals')
    formats.add_argument('--table', action='store_true', help='Print reproducible per-file Markdown counts')
    parser.add_argument('--ownership', type=Path, help='Optional path -> owner label JSON map for --table')
    parser.add_argument('--check', action='store_true', help='Fail if any category c remains')
    args = parser.parse_args()
    try:
        root = args.repo.resolve(strict=True)
        if not (root / 'src').is_dir():
            raise ValueError('repository src/ directory not found')
        decisions = json.loads(args.classifications.read_text(encoding='utf-8')) if args.classifications else None
        report = inventory(root, decisions)
        pending = report['counts'].get('c', 0)
        if args.table:
            owners = json.loads(args.ownership.read_text(encoding='utf-8')) if args.ownership else {}
            if not isinstance(owners, dict) or not all(isinstance(v, str) for v in owners.values()):
                raise ValueError('ownership must be a path -> label JSON object')
            print('| Source | (a) | (b) | Protected Canonical | (c), Review Pending | Editing Boundary |')
            print('| --- | ---: | ---: | ---: | ---: | --- |')
            for path in sorted({r['path'] for r in report['records']}):
                counts = Counter(r['category'] for r in report['records'] if r['path'] == path)
                owner = owners.get(path, 'review in C').replace('|', '/')
                values = ' | '.join(str(counts[c]) for c in ('a', 'b', 'protected', 'c'))
                print(f'| `{path}` | {values} | {owner} |')
            return int(args.check and pending > 0)
        if args.summary:
            report['by_file'] = {path: dict(Counter(r['category'] for r in report['records'] if r['path'] == path))
                                 for path in sorted({r['path'] for r in report['records']})}
            del report['records']
        print(json.dumps(report, ensure_ascii=True, separators=(',', ':')))
        return int(args.check and pending > 0)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(json.dumps({'error': str(error)}), file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
