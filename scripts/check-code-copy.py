#!/usr/bin/env python3
"""Qualify canonical code copying with synthetic histories and intercepted OSC52."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).parent / "dev"))
from pty_capture import Terminal, WATCHDOG_SECONDS


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--fixture", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    binary, fixture = args.binary.resolve(), args.fixture.resolve()
    args.out.mkdir(exist_ok=False, parents=True)
    manifest = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "cases": []}
    prompt = "prompt 日本語 e\u0301🙂\n" * 800
    many = "First block:\n\n```rust\nlet first = 1;\n```\n\nSecond block:\n\n```text\nsecond 日本語 e\u0301🙂\n```\n"
    for cols in [80, 140]:
        for mode, palette, term, plain in [("dark", "dark", "xterm-256color", False),
                ("light", "light", "xterm-256color", False),
                ("plain", "dark", "xterm-256color", True), ("vt100", "dark", "vt100", False)]:
            name = f"{cols}-{mode}"
            out = args.out / name
            out.mkdir()
            with tempfile.TemporaryDirectory(prefix="shellboy-code-copy-") as scratch:
                root = Path(scratch) / "fixture"
                subprocess.run([str(fixture), str(root)], check=True, capture_output=True, timeout=WATCHDOG_SECONDS)
                with (root / "config.toml").open("a") as config:
                    config.write(f'palette="{palette}"\nbell=false\n')
                terminal = Terminal(binary, root, cols, palette, term, plain)
                def scene(label, predicate):
                    text = terminal.expect(predicate, name + ":" + label)
                    (out / (label + ".txt")).write_text(text + "\n")
                    return text
                def copy(keys, expected, label):
                    before = len(terminal.raw)
                    terminal.send(keys)
                    pattern = rb"\x1b\]52;c;([A-Za-z0-9+/=]+)(?:\x07|\x1b\\)"
                    scene(label, lambda text: re.search(pattern, terminal.raw[before:]) is not None and "OSC52:" in text)
                    encoded = re.search(pattern, terminal.raw[before:]).group(1)
                    actual = base64.b64decode(encoded).decode()
                    assert actual == expected, (name, label, len(actual), len(expected))
                try:
                    scene("ready", lambda s: "Ctrl+T 会話" in s)
                    terminal.send("/switch visual-copy-one\r")
                    scene("long-answer", lambda s: "After the prompt." in s)
                    copy("/copy\r", prompt, "sole-block")
                    scene("sole-count", lambda s: "コードの塊 1" in s and str(len(prompt)) in s)
                    terminal.send("/switch visual-copy-many\r")
                    scene("numbered-answer", lambda s: "[1] コピー" in s and "[2] コピー" in s)
                    terminal.send("\tb")
                    scene("chooser", lambda s: "コードの塊をコピー" in s and "[2] コピー" in s)
                    copy("2", "second 日本語 e\u0301🙂\n", "second-block")
                    terminal.send("\x1b")
                    scene("chooser-closed", lambda s: "コードの塊をコピー" not in s)
                    terminal.send("\x1b[1;5F")
                    copy("/copy all\r", many, "whole-body")
                    terminal.send("/switch visual-copy-none\r")
                    scene("no-code-answer", lambda s: "Plain 日本語" in s)
                    copy("/copy\r", "Plain 日本語 e\u0301🙂 body.", "no-code-body")
                    terminal.send("/switch visual-copy-many\r")
                    scene("many-again", lambda s: "[2] コピー" in s)
                    before = len(terminal.raw)
                    terminal.send("/copy\r")
                    scene("multiple-waits", lambda s: "コードの塊をコピー" in s)
                    assert b"\x1b]52;" not in terminal.raw[before:]
                    terminal.send("\x1b\x1b")
                    scene("closed", lambda s: "コードの塊をコピー" not in s and "Second block:" in s)
                    manifest["cases"].append({"name": name, "canonical_payloads": 4, "no_host_clipboard": True})
                except BaseException:
                    (out / "failure.txt").write_text(terminal.text() + "\n")
                    raise
                finally:
                    failed = sys.exc_info()[0] is not None
                    try:
                        terminal.close()
                    except Exception:
                        if not failed:
                            raise
                    finally:
                        (out / "terminal.raw").write_bytes(terminal.raw)
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
