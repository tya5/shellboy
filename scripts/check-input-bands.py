#!/usr/bin/env python3
"""Observe real PTY input bands and canonical OSC52 copy, without an API."""
import argparse
import base64
import importlib.util
import json
from pathlib import Path
import re
import sqlite3
import sys
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location("capture", Path(__file__).with_name("check-paste-commands.py"))
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--mark", default="⤷")
    args = parser.parse_args()
    binary, fixture = args.binary.resolve(), args.fixture.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    results = []
    for cols in (80, 140):
        for palette, no_color in [("dark", False), ("light", False), ("dark", True)]:
            name = f"{cols}-{palette}-plain-{int(no_color)}"
            out = args.out / name
            out.mkdir()
            with tempfile.TemporaryDirectory(prefix="shellboy-input-bands-") as scratch:
                root = Path(scratch) / "fixture"
                subprocess.run([str(fixture), str(root)], check=True, capture_output=True,
                               timeout=capture.WATCHDOG_SECONDS)
                (root / "config.toml").write_text(
                    'default_connection="visual"\ndefault_model="offline-fixture"\n'
                    '[connections.visual]\nbase_url="http://127.0.0.1:9/v1"\n'
                    f'[ui]\ncopy_backend="osc52"\npalette="{palette}"\nbell=false\n')
                with sqlite3.connect(root / "data/shellboy.db") as db:
                    expected = [json.loads(row[0])["content"] for row in db.execute(
                        "SELECT payload_json FROM node WHERE id LIKE 'visual-input-bands-node-%' ORDER BY id")]
                terminal = capture.Terminal(binary, root, cols, palette, "xterm-256color", no_color)
                try:
                    terminal.expect(lambda text: "F1 help" in text, "ready")
                    terminal.send("/switch visual-input-bands\r")
                    text = terminal.expect(lambda text: "CODE_END" in text and "LONG_END" in text and
                                           "名前なし" in text and "Claude" in text, "input-bands")
                    (out / "bands.txt").write_text(text + "\n")
                    assert "入力 ·" not in text
                    rows = text.splitlines()
                    mark_rows = [i for i, row in enumerate(rows) if args.mark in row]
                    assert len(mark_rows) == 4, text
                    band_end = next(i for i, row in enumerate(rows) if "CODE_END" in row)
                    expected_bg = "default" if no_color else ("262626" if palette == "dark" else "dadada")
                    color_evidence = []
                    for y in range(mark_rows[0], band_end + 1):
                        row = terminal.screen.buffer[y]
                        if not rows[y].strip():
                            continue
                        backgrounds = [row[x].bg for x in range(cols)]
                        if no_color:
                            assert set(backgrounds) == {"default"}, (name, y, backgrounds)
                        else:
                            colored = [x for x, bg in enumerate(backgrounds) if bg == expected_bg]
                            assert colored, (name, y, backgrounds)
                            assert all(bg == expected_bg for bg in backgrounds[min(colored):max(colored)+1]), (name, y, backgrounds)
                        color_evidence.append({"row": y, "backgrounds": sorted(set(backgrounds))})
                    assert "Claude" not in rows[mark_rows[1]], "anonymous input must not inherit a label"
                    for y in range(mark_rows[2] + 1, mark_rows[3]):
                        assert "Claude" not in rows[y], "metadata cannot repeat on wraps"
                    (out / "colors.json").write_text(json.dumps(color_evidence, indent=2) + "\n")

                    def copy(command):
                        before = len(terminal.raw)
                        terminal.send(command)
                        pattern = rb"\x1b\]52;c;([A-Za-z0-9+/=]+)(?:\x07|\x1b\\)"
                        terminal.expect(lambda _text: re.search(pattern, terminal.raw[before:]) is not None, "OSC52-copy")
                        return base64.b64decode(re.search(pattern, terminal.raw[before:]).group(1)).decode()

                    copied = copy("/copy visual-input-bands-node-0\r")
                    assert copied == expected[0], copied
                    selected = copy("\tc")
                    assert selected in expected and "Claude" not in selected, selected
                    terminal.send("o")
                    terminal.expect(lambda text: "原文" in text and selected.splitlines()[0] in text and
                                    "名乗り" in text, "F6-original")
                    original = copy("c")
                    assert original == selected, (original, selected)
                    # A doubled Esc is decoded as Esc/Alt+Esc without waiting
                    # for another byte; both close this overlay. Item focus is
                    # cleared separately by the complete Ctrl+End sequence.
                    terminal.send("\x1b\x1b")
                    terminal.expect(lambda text: "LONG_BEGIN" in text and "名前なし" in text,
                                    "close-original")
                    terminal.send("\x1b[1;5F")
                    results.append({"case": name, "mark": args.mark, "band_rows": len(color_evidence),
                                    "copy_body_only": True, "selection_copy_body_only": True,
                                    "original_copy_body_only": True})
                except Exception:
                    (out / "failure.txt").write_text(terminal.text() + "\n")
                    raise
                finally:
                    failed = sys.exc_info()[0] is not None
                    try:
                        terminal.close()
                    except Exception as error:
                        if not failed:
                            raise
                        (out / "cleanup-failure.txt").write_text(str(error) + "\n")
                    finally:
                        (out / "terminal.raw").write_bytes(terminal.raw)
    (args.out / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(results, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
