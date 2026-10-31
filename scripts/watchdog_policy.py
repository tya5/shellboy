"""Lexical Rust test-scope inventory; no provider, build or third-party parser."""
import re, json
from pathlib import Path

def mask(s):
    out = list(s)
    i = 0
    while i < len(s):
        start = i
        if s.startswith('//', i):
            j = s.find('\n', i)
            i = len(s) if j < 0 else j
        elif s.startswith('/*', i):
            i += 2
            depth = 1
            while i < len(s) and depth:
                if s.startswith('/*', i):
                    depth += 1
                    i += 2
                elif s.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
        elif (m := re.compile('(?:br|r)(#*)"').match(s, i)):
            end = '"' + m[1]
            j = s.find(end, i + len(m[0]))
            i = len(s) if j < 0 else j + len(end)
        elif s[i] == '"':
            i += 1
            while i < len(s):
                if s[i] == '\\':
                    i += 2
                elif s[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
        elif (m := re.compile("'(?:\\\\(?:u\\{[0-9a-fA-F_]+\\}|x[0-9a-fA-F]{2}|.)|[^'\\\\\\n])'").match(s, i)):
            i += len(m[0])
        else:
            i += 1
            continue
        for j in range(start, min(i, len(s))):
            if s[j] != '\n':
                out[j] = ' '
    return ''.join(out)

def call(s, m, start):
    depth = 0
    comma = None
    for i in range(start, len(m)):
        if m[i] in '([{':
            depth += 1
        elif m[i] in ')]}':
            depth -= 1
            if depth == 0:
                return (i, comma)
        elif m[i] == ',' and depth == 1 and (comma is None):
            comma = i
    raise ValueError('unterminated')

def module_file(path, name):
    parent = path.parent if path.name in ('mod.rs', 'lib.rs', 'main.rs') else path.with_suffix('')
    for candidate in [parent / (name + '.rs'), parent / name / 'mod.rs']:
        if candidate.exists():
            return candidate
    return None

def test_scopes(root):
    root = root.resolve()
    files = {p: [(0, len(p.read_text(encoding='utf-8')))] for p in (root / 'tests').rglob('*.rs')}
    external = set()
    for p in (root / 'src').rglob('*.rs'):
        text = p.read_text(encoding='utf-8')
        masked = mask(text)
        spans = []
        for attr in re.finditer('#\\[cfg\\(([^\\]]*)\\)\\]', masked):
            if not re.search('\\btest\\b', attr[1]) or re.search('not\\s*\\(\\s*test', attr[1]):
                continue
            rest = masked[attr.end():]
            declaration = re.match('\\s*(?:#\\[[^\\]]*\\]\\s*)*(?:pub(?:\\([^)]*\\))?\\s+)?mod\\s+(\\w+)\\s*([;{])', rest)
            if declaration and declaration[2] == ';':
                prefix = text[attr.end():attr.end() + declaration.end()]
                override = re.search('#\\[path\\s*=\\s*"([^"]+)"\\]', prefix)
                target = (p.parent / override[1]).resolve() if override else module_file(p, declaration[1])
                if target is not None and (not target.exists()):
                    target = None
                if target:
                    external.add(target)
            opening = re.search('[;{]', rest)
            if opening and opening[0] == '{':
                start = attr.end() + opening.start()
                end, _ = call(text, masked, start)
                spans.append((attr.start(), end + 1))
        if spans:
            files[p] = spans
    while external:
        p = external.pop()
        if p in files and files[p] == [(0, len(p.read_text(encoding='utf-8')))]:
            continue
        text = p.read_text(encoding='utf-8')
        files[p] = [(0, len(text))]
        for m in re.finditer('\\bmod\\s+(\\w+)\\s*;', mask(text)):
            target = module_file(p, m[1])
            if target:
                external.add(target)
    return files

def findings(root):
    root = root.resolve()
    import hashlib
    for path, spans in sorted(test_scopes(root).items()):
        text = path.read_text(encoding='utf-8')
        masked = mask(text)
        aliases = re.findall('\\btimeout(?:_at)?\\s+as\\s+(\\w+)', masked)
        clocks = re.findall('\\bInstant\\s+as\\s+(\\w+)', masked)
        patterns = {'raw-timeout': '\\b(?:timeout|timeout_at|recv_timeout|wait_timeout|set_read_timeout|set_write_timeout)\\s*\\(', 'wall-deadline': '\\bInstant::now\\s*\\(\\s*\\)\\s*(?:[+<>]|[!=]=)', 'elapsed-comparison': '\\.elapsed\\s*\\(\\s*\\)\\s*(?:[<>!=]=?|\\.as_(?:millis|secs|micros)\\(\\)\\s*[<>!=])'}
        if aliases:
            patterns['raw-timeout'] = '\\b(?:timeout|timeout_at|recv_timeout|wait_timeout|set_read_timeout|set_write_timeout|' + '|'.join(aliases) + ')\\s*\\('
        if clocks:
            patterns['wall-deadline'] = '\\b(?:Instant|' + '|'.join(clocks) + ')::now\\s*\\(\\s*\\)\\s*(?:[+<>]|[!=]=)'
        for rule, pattern in patterns.items():
            for m in re.finditer(pattern, masked):
                if not any((a <= m.start() < b for a, b in spans)):
                    continue
                end = m.end()
                if rule == 'raw-timeout':
                    end, _ = call(text, masked, masked.index('(', m.start()))
                else:
                    end = masked.find(';', m.end())
                    if end < 0:
                        end = len(masked) - 1
                functions = list(re.finditer('\\bfn\\s+(\\w+)', masked[:m.start()]))
                function = functions[-1][1] if functions else '<module>'
                signature = function + ':' + re.sub('\\s+', '', masked[m.start():end + 1])
                yield {'file': path.relative_to(root).as_posix(), 'rule': rule, 'fingerprint': hashlib.sha256(signature.encode()).hexdigest(), 'line': text.count('\n', 0, m.start()) + 1, 'function': function}

def check(root, allowances):
    from collections import Counter
    actual = list(findings(root))
    counts = Counter(((v['file'], v['rule'], v['fingerprint']) for v in actual))
    allowed = {}
    for v in allowances:
        if not v.get('reason', '').strip():
            raise ValueError('allowance must give a reason')
        key = (v['file'], v['rule'], v['fingerprint'])
        if key in allowed:
            raise ValueError('duplicate allowance')
        allowed[key] = v['count']
    errors = []
    for key, count in counts.items():
        if count > allowed.get(key, 0):
            v = next((v for v in actual if (v['file'], v['rule'], v['fingerprint']) == key))
            errors.append(f'{v['file']}:{v['line']}: {v['rule']} in {v['function']} ({count} occurrences, allowed {allowed.get(key, 0)}); use test_watchdog or justify a virtual-clock/deferred exception')
    for key, count in allowed.items():
        if counts[key] < count:
            errors.append(f'stale allowance {key[0]} {key[1]} {key[2]}; remove/update it')
    return errors
