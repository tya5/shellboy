#!/usr/bin/env python3
"""Read-only Rust source/function metrics for staged region extraction.

Counts physical file lines and declaration parameters (including self), not AST
complexity or execution cost. The existing audit lexer skips strings/comments
and cfg(test) fixtures. Nested groups and generic commas stay within parameters.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import sys


def measure(root, paths):
    spec = importlib.util.spec_from_file_location("region_audit", root / "scripts/dev/language-audit.py")
    audit = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = audit
    spec.loader.exec_module(audit)
    result = []
    for path in paths:
        source = (root / path).read_text()
        tokens = audit.tokens(source)
        pairs = audit.groups(tokens)
        fixtures, _, _, _ = audit.ranges(tokens, pairs)
        functions = []
        for i, token in enumerate(tokens):
            if token.kind != "word" or token.text != "fn":
                continue
            name = tokens[i + 1]
            if name.kind != "word":
                continue
            start = i + 2
            angle = 0
            while start < len(tokens):
                text = tokens[start].text
                if text == "<":
                    angle += 1
                elif text == ">" and tokens[start - 1].text != "-":
                    angle -= 1
                elif text == "(" and angle == 0:
                    break
                start += 1
            if start not in pairs:
                raise ValueError(f"unbalanced signature: {path}:{name.text}")
            end = pairs[start]
            arity, angle, cursor = 0, 0, start + 1
            has_parameter = False
            while cursor < end:
                text = tokens[cursor].text
                if cursor in pairs:
                    has_parameter = True
                    cursor = pairs[cursor] + 1
                    continue
                if text == "<":
                    angle += 1
                elif text == ">" and tokens[cursor - 1].text != "-":
                    angle -= 1
                if text == "," and angle == 0:
                    arity += int(has_parameter)
                    has_parameter = False
                else:
                    has_parameter = True
                cursor += 1
            arity += int(has_parameter)
            test = path.endswith("_tests.rs") or any(a <= token.start < b for a, b in fixtures)
            functions.append(dict(name=name.text, line=source.count("\n", 0, token.start) + 1,
                                  parameters_including_self=arity, test_only=test))
        production = [f for f in functions if not f["test_only"]]
        result.append(dict(path=path, physical_lines=len(source.splitlines()),
                           production_function_count=len(production),
                           production_max_parameters=max((f["parameters_including_self"] for f in production), default=0),
                           functions=functions))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("paths", nargs="+", help="repository-relative Rust source paths")
    args = parser.parse_args()
    print(json.dumps(measure(args.repo, args.paths), indent=2))


if __name__ == "__main__":
    main()
