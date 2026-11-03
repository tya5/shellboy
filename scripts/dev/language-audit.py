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
    # Only syntax that proves the role of a particular literal is classified.
    # Unknown macros, nonliteral arguments, other match patterns and arbitrary
    # constants remain pending; identifiers are never inferred from their names.
    fixtures, localized, excluded, modules = [], [], [], []
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
        # The known macros accept literal Japanese arguments: local! has one,
        # glyphs::localized! has narrow and fallback variants. Do not infer
        # nested expressions or a different qualified macro's semantics.
        if (token.text in ('local', 'localized')
                and texts[i + 1:i + 3] == ['!', '(']
                and (i < 2 or texts[i - 2:i] != [':', ':']
                     or (token.text == 'local' and texts[i - 3:i] == ['i18n', ':', ':'])
                     or (token.text == 'localized' and texts[i - 3:i] == ['glyphs', ':', ':']))):
            stop = pairs[i + 2]
            args, begin, j = [], i + 3, i + 3
            while j < stop:
                if texts[j] == ',':
                    args.append((begin, j))
                    begin = j + 1
                j = pairs[j] + 1 if j in pairs else j + 1
            args.append((begin, stop))
            expected = 3 if token.text == 'local' else 5
            if len(args) == expected or (len(args) == expected + 1 and args[-1][0] == args[-1][1]):
                for begin, end in args[1:2 if token.text == 'local' else 3]:
                    if end == begin + 1 and items[begin].kind in ('string', 'raw_string'):
                        localized.append((items[begin].start, items[begin].end))
        # Explicitly qualified logging macros: the direct format literal only.
        if (token.text in ('trace', 'debug', 'info', 'warn', 'error')
                and i >= 3 and texts[i - 3:i] in (['log', ':', ':'], ['tracing', ':', ':'])
                and texts[i + 1:i + 3] == ['!', '(']):
            first = i + 3
            if first < pairs[i + 2] and items[first].kind in ('string', 'raw_string'):
                excluded.append((items[first].start, items[first].end))
        # A literal serde rename is a serialization identifier, not UI copy.
        if texts[i:i + 3] == ['#', '[', 'serde'] and texts[i + 3:i + 4] == ['(']:
            stop = pairs[i + 3]
            for j in range(i + 4, stop - 2):
                if (texts[j] == 'rename' and texts[j + 1] == '='
                        and items[j + 2].kind in ('string', 'raw_string')):
                    excluded.append((items[j + 2].start, items[j + 2].end))
        if token.text == 'match':
            # Parse only arms of an actual match; a Ja-looking expression or
            # an alternative pattern is not evidence that this is a Ja arm.
            j = i + 1
            while j < len(items) and texts[j] not in ('{', ';', '}'):
                j = pairs[j] + 1 if j in pairs else j + 1
            if j >= len(items) or texts[j] != '{':
                continue
            stop, j = pairs[j], j + 1
            while j < stop:
                begin = j
                while j < stop and texts[j:j + 2] != ['=', '>']:
                    if texts[j] == ',':
                        break
                    j = pairs[j] + 1 if j in pairs else j + 1
                if j >= stop or texts[j:j + 2] != ['=', '>']:
                    break
                pattern = texts[begin:j]
                # Guard expressions are not part of the pattern.
                if 'if' in pattern:
                    pattern = pattern[:pattern.index('if')]
                j += 2
                expression = j
                if j < stop and texts[j] == '{' and pairs[j] + 1 < stop and (
                        items[pairs[j] + 1].kind == 'word' or texts[pairs[j] + 1] == '('):
                    # A block arm can omit its comma before the next arm.
                    j = pairs[j] + 1
                    next_arm = j
                else:
                    while j < stop and texts[j] != ',':
                        j = pairs[j] + 1 if j in pairs else j + 1
                    next_arm = j + 1
                if (expression < j and '|' not in pattern
                        and any(pattern[k:k + 4] == ['UiLanguage', ':', ':', 'Ja']
                                for k in range(len(pattern) - 3))):
                    localized.append((items[expression].start, items[j - 1].end))
                j = next_arm
    return fixtures, localized, excluded, modules


def paired_render_ranges(items, pairs, spans):
    """Pair exact (En/Ja, Self pattern) arms in a typed, imported renderer.

    This deliberately does not guess from field names, similar strings, or two
    arbitrary tuples. Both arms and a Japanese-free English literal are needed.
    """
    texts = [t.text for t in items]
    result = []
    for start, end in spans:
        header_end = next((i for i in range(start, end) if texts[i] == '{'), end)
        header = texts[start:header_end]
        if not any(header[i:i + 8] == ['fn', 'render', '(', 'self', ',', 'language', ':', 'UiLanguage']
                   for i in range(len(header) - 7)):
            continue
        body = texts[header_end:end]
        imports = ['use', 'UiLanguage', ':', ':', '{', 'En', ',', 'Ja', '}', ';']
        if not any(body[i:i + len(imports)] == imports for i in range(len(body) - len(imports) + 1)):
            continue
        for i in range(header_end, end):
            if texts[i:i + 6] != ['match', '(', 'language', ',', 'self', ')']:
                continue
            left = i + 6
            if left not in pairs or texts[left] != '{':
                continue
            stop, j, arms = pairs[left], left + 1, {}
            while j < stop:
                if texts[j] != '(' or j not in pairs:
                    break
                pattern_end = pairs[j]
                pattern = texts[j + 1:pattern_end]
                if texts[pattern_end + 1:pattern_end + 3] != ['=', '>']:
                    break
                expression = pattern_end + 3
                j = expression
                if j < stop and texts[j] == '{' and pairs[j] + 1 < stop and (
                        items[pairs[j] + 1].kind == 'word' or texts[pairs[j] + 1] == '('):
                    j = pairs[j] + 1
                    next_arm = j
                else:
                    while j < stop and texts[j] != ',':
                        j = pairs[j] + 1 if j in pairs else j + 1
                    next_arm = j + 1
                if (expression < j and len(pattern) >= 5 and pattern[0] in ('Ja', 'En')
                        and pattern[1:5] == [',', 'Self', ':', ':'] and '|' not in pattern):
                    key = tuple(pattern[2:])
                    arms.setdefault(key, {}).setdefault(pattern[0], []).append((expression, j))
                j = next_arm
            for versions in arms.values():
                if len(versions.get('Ja', [])) != 1 or len(versions.get('En', [])) != 1:
                    continue
                en_start, en_end = versions['En'][0]
                english = [t for t in items[en_start:en_end] if t.kind in ('string', 'raw_string')]
                if (not english or any(JAPANESE.search(decoded(t)) for t in english)
                        or not any(re.search(r'[A-Za-z]', decoded(t)) for t in english)):
                    continue
                ja_start, ja_end = versions['Ja'][0]
                result.append((items[ja_start].start, items[ja_end - 1].end))
    return result


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


# These are token fingerprints of the reviewed typed-key construction and UI
# projection functions, not source literals. If any projection changes, the
# catalogue rule fails closed until that path is independently re-examined.
CATALOGUE_PROOF = {
    'src/operations.rs': {
        '2b97db37bf2875e148ef296b39c83352e4b4e919222669f1e790444abc738176',
        '22d418c2ece3eccac350f8da1ef06ddf368adfb6ac848573cdf0bb92b78ee20e',
        'e4350e84095cef8970f0671e172f6d45d5d4bd3831f5b0bd0f02ce4ecc7263d9',
        '87f0e07828f8adfa929dd3e73a805281faf33f276263ada2de8c4f2746acbf44',
    },
    'src/ui/operations/metadata.rs': {
        '3a514953c6ad8883b36faaebc96b4476d747e23e6c9fa6c4e35cccee70527f4c',
        'ee111179d4c571e341289d42946dafd042ef682247dd759b65e40e1e587bdf50',
    },
    'src/ui/operations.rs': {
        'cdf8536fc4654255a83a3fad9095a1277dc7423b7798c4cacdd2e03b24ba6f9d',
        'cf6d600a439dad85a962b9fa509667c84b953f7dd125939dd6f9a5f7583cd5cb',
    },
}


def catalogue_pairs(catalogue, metadata):
    """Join literal catalogue fields to same-ID local! arms; never join by text alone."""
    source, dest = {}, {}
    texts = [t.text for t in catalogue]
    pairs = groups(catalogue)
    specs = {'FieldSpec': ('FieldId', ('label', 'help')),
             'OperationSpec': ('OperationId', ('label', 'meaning'))}
    for i, token in enumerate(catalogue):
        if token.text not in specs or texts[i + 1:i + 2] != ['{']:
            continue
        enum, fields = specs[token.text]
        stop = pairs[i + 1]
        body = texts[i + 2:stop]
        if body[:3] != ['id', ':', enum] or body[3:5] != [':', ':'] or len(body) < 6:
            continue
        key = (enum, body[5])
        for j in range(i + 2, stop - 2):
            if (texts[j] in fields and texts[j + 1] == ':'
                    and catalogue[j + 2].kind in ('string', 'raw_string')):
                source.setdefault((key, texts[j]), []).append(catalogue[j + 2])
    texts = [t.text for t in metadata]
    pairs = groups(metadata)
    spans = item_ranges(metadata, pairs)
    functions = {'field_label_for': ('FieldId', 'label'), 'field_help_for': ('FieldId', 'help'),
                 'operation_label_for': ('OperationId', 'label'), 'operation_meaning_for': ('OperationId', 'meaning')}
    for start, end in spans:
        head = texts[start:min(end, start + 12)]
        if 'fn' not in head or head.index('fn') + 1 >= len(head):
            continue
        name = head[head.index('fn') + 1]
        if name not in functions:
            continue
        enum, field = functions[name]
        for i in range(start, end - 12):
            if texts[i:i + 4] != [enum, ':', ':', texts[i + 3]]:
                continue
            if texts[i + 4:i + 8] != ['=', '>', 'local', '!'] or texts[i + 8] != '(':
                continue
            stop = pairs[i + 8]
            args = metadata[i + 9:stop]
            # language, Ja literal, En literal, with optional trailing comma.
            if len(args) not in (5, 6) or [t.text for t in args[:2]] != ['language', ',']:
                continue
            if (args[2].kind not in ('string', 'raw_string') or args[3].text != ','
                    or args[4].kind not in ('string', 'raw_string')
                    or (len(args) == 6 and args[5].text != ',')
                    or JAPANESE.search(decoded(args[4])) or not re.search('[A-Za-z]', decoded(args[4]))):
                continue
            dest.setdefault(((enum, texts[i + 3]), field), []).append(args[2].text)
    return [(values[0].start, values[0].end) for key, values in source.items()
            if len(values) == 1 and dest.get(key) == [values[0].text]]


def catalogue_ranges(root, prepared):
    for relative, required in CATALOGUE_PROOF.items():
        path = root / relative
        if path not in prepared:
            return []
        _, items, _, _, _, _, spans = prepared[path]
        fingerprints = {scope_fingerprint(items, spans, start, {}) for start, _ in spans}
        if not required <= fingerprints:
            return []
    return catalogue_pairs(prepared[root / 'src/operations.rs'][1],
                           prepared[root / 'src/ui/operations/metadata.rs'][1])


def inventory(root, classifications=None):
    paths = sorted((root / 'src').rglob('*.rs'))
    prepared, fixture_paths = {}, set()
    for path in paths:
        source = path.read_text(encoding='utf-8')
        items = tokens(source)
        pairs = groups(items)
        fixture, localized, excluded, modules = ranges(items, pairs)
        spans = item_ranges(items, pairs)
        paired = paired_render_ranges(items, pairs, spans)
        prepared[path] = source, items, fixture, localized, paired, excluded, spans
        for module in modules:
            parent = path.parent if path.name in ('lib.rs', 'main.rs', 'mod.rs') else path.with_suffix('')
            fixture_paths.update([parent / (module + '.rs'), parent / module / 'mod.rs'])
    catalogue = catalogue_ranges(root, prepared)
    if classifications is not None and not isinstance(classifications, dict):
        raise ValueError('reviewed decisions must be a JSON object')
    decisions = classifications or {}
    seen, records = set(), []
    for path, (source, items, fixture, localized, paired, excluded, spans) in prepared.items():
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
                category, reason = 'a', 'explicit Japanese macro argument or match arm'
            elif contains(paired):
                category, reason = 'a', 'paired typed En/Ja renderer arms'
            elif relative == 'src/operations.rs' and contains(catalogue):
                category, reason = 'a', 'same-ID bilingual operation catalogue with pinned UI projection'
            elif contains(excluded):
                category, reason = 'b', 'qualified logging format or serde identifier'
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
        'a: explicit Japanese UI side; b: test-only, logging format or serde identifier; '
        'c: pending review/English cleanup; '
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
    parser.add_argument('--ui-pending', action='store_true',
                        help='Report all pending literals in src/ui.rs and src/ui/** (not just status surfaces)')
    args = parser.parse_args()
    try:
        root = args.repo.resolve(strict=True)
        if not (root / 'src').is_dir():
            raise ValueError('repository src/ directory not found')
        decisions = json.loads(args.classifications.read_text(encoding='utf-8')) if args.classifications else None
        report = inventory(root, decisions)
        if args.ui_pending:
            report['records'] = [r for r in report['records'] if r['category'] == 'c'
                                 and (r['path'] == 'src/ui.rs' or r['path'].startswith('src/ui/'))]
            report['counts'] = dict(Counter(r['category'] for r in report['records']))
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
