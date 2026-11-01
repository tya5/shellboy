#!/usr/bin/env python3
"""Read-only #118 source metrics; --root can select a separate baseline checkout."""
import argparse
import json
import re
from pathlib import Path

TOKENS = re.compile(r'r(?P<hash>\#*)".*?"(?P=hash)|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])\'|//[^\n]*|/\*.*?\*/', re.S)
FIELDS = ('follow', 'loading', 'request', 'pending', 'history_rebase_pending',
          'committed_preview', 'initialized', 'unseen', 'moved_during_read',
          'anchor', 'row', 'within')
FILES = ('src/ui.rs', 'src/ui/view.rs', 'src/ui/history_interaction.rs',
         'src/ui/overlay.rs', 'src/ui/navigation.rs')


def masked(text):
    return TOKENS.sub(lambda m: re.sub(r'[^\n]', ' ', m[0]), text)


def closing(text, start):
    depth = 0
    for i in range(start, len(text)):
        depth += (text[i] == '{') - (text[i] == '}')
        if depth == 0:
            return i + 1
    raise ValueError('unbalanced source')


def production(text):
    code = masked(text)
    for match in reversed(list(re.finditer(r'#\[cfg\(test\)\]', code))):
        start = match.start()
        item = re.search(r'[;{]', code[match.end():])
        if not item:
            raise ValueError('missing cfg(test) item')
        begin = match.end() + item.start()
        end = closing(code, begin) if code[begin] == '{' else begin + 1
        code = code[:start] + re.sub(r'[^\n]', ' ', code[start:end]) + code[end:]
    return code


def measure(root):
    sites = {name: [] for name in FIELDS}
    transitions = []
    functions = {}
    fields = {}
    for path in FILES:
        file = root / path
        if not file.exists():
            continue
        code = production(file.read_text())
        for name in FIELDS:
            # Ignore request-builder fields and other UI domains. This is the
            # stage-1 inventory, not an instruction/event execution counter.
            pattern = rf'\.(?:navigation\.)?(?:scroll\.)?{name}(?:\.(?:node_id|char_offset))?\s*(?:=(?!=)|\+=|-=|\.take\(\))'
            for m in re.finditer(pattern, code):
                sites[name].append(f'{path}:{code.count(chr(10), 0, m.start()) + 1}')
            for m in re.finditer(rf'&mut\s+\w+(?:\.navigation)?(?:\.scroll)?\.{name}\b', code):
                sites[name].append(f'{path}:{code.count(chr(10), 0, m.start()) + 1}')
        for m in re.finditer(r'\.(?:cancel_navigation|rebase_navigation)\(', code):
            transitions.append(f'{path}:{code.count(chr(10), 0, m.start()) + 1}')
        for m in re.finditer(r'\bfn (scroll|scroll_step)\s*\(', code):
            begin = code.index('{', m.end())
            end = closing(code, begin)
            functions[f'{path}:{m[1]}'] = code.count('\n', m.start(), end) + 1
        for m in re.finditer(r'\bstruct (VisualScroll|Navigation)\s*\{', code):
            end = closing(code, code.index('{', m.start()))
            body = code[m.end():end - 1]
            fields[m[1]] = len(re.findall(r'^\s*(?:pub(?:\([^)]*\))?\s+)?\w+\s*:', body, re.M))
    return dict(counts={k: len(set(v)) for k, v in sites.items()},
                total_state_write_sites=sum(len(set(v)) for v in sites.values()),
                cancel_rebase_calls=len(transitions), functions=functions,
                struct_fields=fields, sites=sites, transitions=transitions)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    args = p.parse_args()
    print(json.dumps(measure(args.root), indent=2))


if __name__ == '__main__':
    main()
