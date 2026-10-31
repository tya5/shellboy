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
import os
import threading
from http.server import ThreadingHTTPServer

spec = importlib.util.spec_from_file_location("capture", Path(__file__).with_name("check-paste-commands.py"))
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--mark", default="⇥")
    parser.add_argument("--columns", type=int, nargs="+", default=[80, 140])
    parser.add_argument("--live-probe", action="store_true")
    parser.add_argument("--term", default="xterm-256color")
    args = parser.parse_args()
    binary, fixture = args.binary.resolve(), args.fixture.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    results = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), capture.Mock) if args.live_probe else None
    if server:
        threading.Thread(target=server.serve_forever, daemon=True).start()
    base_url = f"http://127.0.0.1:{server.server_port}/v1" if server else "http://127.0.0.1:9/v1"
    for cols in args.columns:
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
                    f'[connections.visual]\nbase_url="{base_url}"\n'
                    f'[ui]\ncopy_backend="osc52"\npalette="{palette}"\nbell=false\n')
                with sqlite3.connect(root / "data/shellboy.db") as db:
                    expected = [json.loads(row[0])["content"] for row in db.execute(
                        "SELECT payload_json FROM node WHERE id LIKE 'visual-input-bands-node-%' ORDER BY id")]
                terminal = capture.Terminal(binary, root, cols, palette, args.term, no_color)
                plain = no_color or "256color" not in args.term
                try:
                    terminal.expect(lambda text: "F1 help" in text, "ready")
                    terminal.send("/switch visual-input-bands\r")
                    text = terminal.expect(lambda text: "CODE_END" in text and "LONG_END" in text and
                                           "名前なし" in text and "Claude" in text, "input-bands")
                    (out / "bands.txt").write_text(text + "\n")
                    (out / "cells.json").write_text(json.dumps([
                        [{"x": x, "text": cell.data, "fg": cell.fg, "bg": cell.bg}
                         for x, cell in enumerate(row)] for row in terminal.screen.buffer
                    ], ensure_ascii=False, indent=2) + "\n")
                    assert "入力 ·" not in text
                    rows = text.splitlines()
                    mark_rows = [i for i, row in enumerate(rows) if args.mark in row]
                    assert len(mark_rows) == 4, text
                    band_end = next(i for i, row in enumerate(rows) if "CODE_END" in row)
                    expected_bg = "default" if plain else ("262626" if palette == "dark" else "dadada")
                    header = terminal.screen.buffer[0]
                    header_bg = "default" if plain else ("1c1c1c" if palette == "dark" else "d0d0d0")
                    assert header[0].data == header[cols - 1].data == " ", (name, "header inset")
                    assert all(header[x].bg == header_bg for x in range(cols)), (name, "header background")
                    color_evidence = []
                    for y in range(mark_rows[0], band_end + 1):
                        row = terminal.screen.buffer[y]
                        if not rows[y].strip():
                            continue
                        backgrounds = [row[x].bg for x in range(cols)]
                        if plain:
                            assert set(backgrounds) == {"default"}, (name, y, backgrounds)
                        else:
                            colored = [x for x, bg in enumerate(backgrounds) if bg == expected_bg]
                            assert colored, (name, y, backgrounds)
                            assert backgrounds[0] == backgrounds[-1] == "default", (name, y, "outer margin", backgrounds)
                            assert backgrounds[1:-1] == [expected_bg] * (cols - 2), (name, y, "band extent", backgrounds)
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
                    terminal.expect(lambda text: "┌ 原文 " in text and selected.splitlines()[0] in text and
                                    "名乗り：" in text, "F6-original")
                    original = copy("c")
                    assert original == selected, (original, selected)
                    # A doubled Esc is decoded as Esc/Alt+Esc without waiting
                    # for another byte; both close this overlay. Item focus is
                    # cleared separately by the complete Ctrl+End sequence.
                    terminal.send("\x1b\x1b")
                    terminal.expect(lambda text: "LONG_BEGIN" in text and "名前なし" in text,
                                    "close-original")
                    terminal.send("\x1b[1;5F")
                    if args.live_probe:
                        payload = "LIVE_EDGE_BEGIN " + "外からの入力 ✨e\u0301🙂 " * 180 + "\n短い続き LIVE_EDGE_END"
                        env = {key: value for key, value in os.environ.items() if not key.startswith("SHELLBOY_")}
                        command = [str(binary), "--config", str(root / "config.toml"), "--data-dir", str(root / "data"),
                                   "message", "send", "--thread", "visual-input-bands", "--sender-label", "cell-probe", "--text", payload]
                        subprocess.run(command, check=True, capture_output=True, env=env, timeout=capture.WATCHDOG_SECONDS)
                        terminal.expect(lambda s: "LIVE_EDGE_END" in s and "PASTE_FIXTURE_REPLY" in s, "live band")
                        def save_cells(label):
                            rows = terminal.text().splitlines()
                            end = next(i for i, text in enumerate(rows) if text.startswith("┌"))
                            audit = {"band_rows": 0, "normal_rows": 0}
                            for y in range(1, end):
                                cells = terminal.screen.buffer[y]
                                assert cells[0].bg == cells[cols - 1].bg == "default", (name, label, y, "margin")
                                if not plain and cells[1].bg == expected_bg:
                                    assert all(cells[x].bg == expected_bg for x in range(1, cols - 1)), (name, label, y, "continuation extent")
                                    audit["band_rows"] += 1
                                else:
                                    assert cells[cols - 3].bg == cells[cols - 2].bg == "default", (name, label, y, "normal trailing cells")
                                    audit["normal_rows"] += 1
                            (out / (label + "-edge-checks.json")).write_text(json.dumps(audit) + "\n")
                            (out / (label + ".txt")).write_text(terminal.text() + "\n")
                            (out / (label + "-cells.json")).write_text(json.dumps([
                                [{"x": x, "text": c.data, "fg": c.fg, "bg": c.bg} for x, c in enumerate(row)]
                                for row in terminal.screen.buffer], ensure_ascii=False, indent=2) + "\n")
                        save_cells("live")
                        for page in range(16):
                            if "短い依頼" in terminal.text():
                                break
                            before_frame = terminal.frames.frame_count
                            terminal.send("\x1b[5~")
                            terminal.expect(lambda _s: terminal.frames.frame_count > before_frame, "page up completed frame")
                        assert "短い依頼" in terminal.text(), terminal.text()
                        save_cells("scroll-up")
                        terminal.send("\x1b[1;5F")
                        terminal.expect(lambda s: "LIVE_EDGE_END" in s and "PASTE_FIXTURE_REPLY" in s, "return to live tail")
                        save_cells("scroll-tail")
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
    if server:
        server.shutdown()
        server.server_close()
    (args.out / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(results, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
