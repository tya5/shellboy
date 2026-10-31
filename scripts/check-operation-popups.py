#!/usr/bin/env python3
"""Capture actual operation forms/help in disposable Unix PTYs, without execution."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).parent / "dev"))
from pty_capture import Terminal


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    args.out.mkdir(exist_ok=False, parents=True)
    manifest = {
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "scope": "Native Unix process/PTY frames; not physical Windows or screenshot evidence",
        "execution": "Forms and help only; no submitted owner/client operation",
        "cases": [],
    }
    forms = [
        ("run get", "作業 · 詳細", "表示"),
        ("message send", "依頼 · 送信", "送信"),
        ("docs get", "ガイド · 本文", "表示"),
        ("config init", "設定 · テンプレート作成", "実行"),
    ]
    for cols, rows in [(80, 24), (140, 40)]:
        for mode, palette, term, plain in [
            ("dark", "dark", "xterm-256color", False),
            ("light", "light", "xterm-256color", False),
            ("plain", "dark", "xterm-256color", True),
            ("vt100", "dark", "vt100", False),
        ]:
            name = f"{cols}-{mode}"
            out = args.out / name
            out.mkdir()
            terminal = Terminal(binary, cols=cols, rows=rows, palette=palette,
                                term=term, no_color=plain)

            def scene(label, predicate):
                text = terminal.expect(predicate, name + ":" + label)
                (out / (label + ".txt")).write_text(text + "\n", encoding="utf-8")
                return text

            try:
                scene("ready", lambda text: "Ctrl+T 会話" in text)
                terminal.send("\x1b[19~")  # F8
                scene("list", lambda text: "CLIと同じ操作" in text)
                for query, title, verb in forms:
                    key = query.replace(" ", "-")
                    terminal.send("\x15" + query)  # Ctrl+U: replace only the list filter.
                    scene(key + "-filtered", lambda text: "1/1件" in text and query in text)
                    terminal.send("\r")  # Open form; never submit it.
                    form = scene(key + "-form", lambda text: title in text and f"Enter {verb}" in text)
                    assert f"Enter {verb}" in form.splitlines()[-1], (name, key, "bottom hint verb")
                    terminal.send("\x1bOP")  # F1
                    help_text = scene(key + "-help", lambda text: title + " · 使い方" in text
                          and "Esc/F1 入力欄へ" in text)
                    hint = help_text.splitlines()[-1]
                    assert "Enter" not in hint and "次の項目" not in hint, (name, key, hint)
                    terminal.send("\x1b")
                    scene(key + "-restored", lambda text: title in text and f"Enter {verb}" in text
                          and title + " · 使い方" not in text)
                    terminal.send("\x1b")
                    scene(key + "-list", lambda text: "CLIと同じ操作" in text)
                manifest["cases"].append({"name": name, "forms": len(forms), "passed": True})
            except BaseException:
                (out / "failure.txt").write_text(terminal.text() + "\n", encoding="utf-8")
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
