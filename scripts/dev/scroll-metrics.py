#!/usr/bin/env python3
"""Read-only #118 source metrics; --root can select a separate baseline checkout."""
import argparse
import json
import re
from pathlib import Path

TOKENS = re.compile(r'r(?P<hash>\#*)".*?"(?P=hash)|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])\'|//[^\n]*|/\*.*?\*/', re.S)
FIELDS = ('follow', 'loading', 'request', 'pending', 'history_rebase_pending',
          'committed_preview', 'initialized', 'unseen', 'moved_during_read',
          'anchor', 'row', 'within', 'mode', 'read', 'view_revision',
          'width', 'height', 'revision', 'focus', 'origin', 'area', 'intent')
FILES = ('src/ui.rs', 'src/ui/view.rs', 'src/ui/history_interaction.rs',
         'src/ui/overlay.rs', 'src/ui/navigation.rs', 'src/ui/view/navigation.rs', 'src/ui/view/navigation/read.rs')


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
        depth = {'(': 0, '[': 0, '<': 0}
        end = None
        for i in range(match.end(), len(code)):
            c = code[i]
            if c in depth:
                depth[c] += 1
            elif c in ')]>':
                opener = {')': '(', ']': '[', '>': '<'}[c]
                depth[opener] = max(0, depth[opener] - 1)
            elif not any(depth.values()):
                if c == '{':
                    end = closing(code, i)
                    break
                if c in ';,':
                    end = i + 1
                    break
        if end is None:
            raise ValueError('missing cfg(test) item boundary')
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
        if path.endswith('/navigation/read.rs'):
            for marker in (r'\*self\s*=(?!=)', r'std::mem::take\(self\)'):
                for match in re.finditer(marker, code):
                    sites['intent'].append(f'{path}:{code.count(chr(10), 0, match.start()) + 1}')
        for name in FIELDS:
            # Ignore request-builder fields and other UI domains. This is the
            # stage-1 inventory, not an instruction/event execution counter.
            owner = r'(?:self|view|state)\s*(?:\.\s*(?:active_)?view_mut\(\))?'
            projection_only = name in ('width', 'height', 'revision', 'focus', 'origin', 'area', 'intent')
            scroll_path = r'scroll\s*\.\s*' if projection_only else r'(?:scroll\s*\.\s*)?'
            path_pattern = owner + r'\s*\.\s*(?:navigation\s*\.\s*)?' + scroll_path
            pattern = path_pattern + rf'{name}(?:\s*\.\s*(?:node_id|char_offset))?\s*(?:=(?!=)|\+=|-=|\.take\(\))'
            for m in re.finditer(pattern, code):
                sites[name].append(f'{path}:{code.count(chr(10), 0, m.start()) + 1}')
            for m in re.finditer(rf'&mut\s+{path_pattern}{name}\b', code):
                sites[name].append(f'{path}:{code.count(chr(10), 0, m.start()) + 1}')
        for m in re.finditer(r'\.(?:cancel_navigation|rebase_navigation|set_follow|set_unseen|set_mode|set_loading|set_request|take_request|begin_read|resume_follow|begin_rebase|restore|cancel|rebase|fail_read|finish_reply|read_event|on_event)\(', code):
            transitions.append(f'{path}:{code.count(chr(10), 0, m.start()) + 1}')
        for m in re.finditer(r'\bfn (scroll|scroll_step)\s*\(', code):
            begin = code.index('{', m.end())
            end = closing(code, begin)
            functions[f'{path}:{m[1]}'] = code.count('\n', m.start(), end) + 1
        for m in re.finditer(r'\bstruct (ThreadView|VisualScroll|Navigation|ReadIntent)\s*\{', code):
            end = closing(code, code.index('{', m.start()))
            body = code[m.end():end - 1]
            names = re.findall(r'^\s*(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:', body, re.M)
            if m[1] == 'ThreadView':
                names = [name for name in names if name in ('anchor', 'follow', 'unseen', 'loading', 'history_rebase_pending', 'committed_preview', 'view_revision')]
            fields[m[1]] = len(names)
    return dict(counts={k: len(set(v)) for k, v in sites.items()},
                total_state_write_sites=sum(len(set(v)) for v in sites.values()),
                transition_calls=len(transitions),
                writes_and_transitions=sum(len(set(v)) for v in sites.values()) + len(transitions),
                stored_fields=sum(fields.values()) - int('Navigation' in fields) - int('ReadIntent' in fields), functions=functions,
                struct_fields=fields, sites=sites, transitions=transitions)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    args = p.parse_args()
    print(json.dumps(measure(args.root), indent=2))


if __name__ == '__main__':
    main()
