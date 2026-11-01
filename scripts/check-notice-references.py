#!/usr/bin/env python3
"""Qualify bounded notice names and exact original access with disposable PTYs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).parent / "dev"))
from pty_capture import Terminal, WATCHDOG_SECONDS


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    binary, fixture = args.binary.resolve(), args.fixture.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    manifest = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "cases": []}
    env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "SYSTEMROOT", "WINDIR") if key in os.environ}
    for cols in (80, 140):
        for mode, palette, term, plain in (("dark", "dark", "xterm-256color", False),
                ("light", "light", "xterm-256color", False), ("plain", "dark", "xterm-256color", True),
                ("vt100", "dark", "vt100", False)):
            for target in ("shell", "child"):
                name = f"{cols}-{mode}-{target}"
                out = args.out / name
                out.mkdir()
                with tempfile.TemporaryDirectory(prefix="shellboy-notice-reference-") as scratch:
                    root = Path(scratch) / "fixture"
                    subprocess.run([str(fixture), str(root)], check=True, capture_output=True, env=env, timeout=WATCHDOG_SECONDS)
                    with (root / "config.toml").open("a") as config:
                        config.write(f'palette="{palette}"\nbell=false\n')
                    terminal = Terminal(binary, root, cols, palette, term, plain)

                    def scene(label, predicate):
                        text = terminal.expect(predicate, name + ":" + label)
                        (out / (label + ".txt")).write_text(text + "\n")
                        return text

                    def cli(*command):
                        result = subprocess.run([str(binary), "--config", str(root / "config.toml"),
                            "--data-dir", str(root / "data"), *command], check=True,
                            capture_output=True, env=env, timeout=WATCHDOG_SECONDS)
                        payload = json.loads(result.stdout)
                        assert payload.get("ok"), payload

                    try:
                        scene("ready", lambda s: "Ctrl+T 会話" in s)
                        if target == "shell":
                            terminal.send("/switch visual-notice-shell\r")
                            text = scene("saved-command", lambda s: "NOTICE_SOURCE_COMMAND" in s and "背景指定あり" in s)
                            assert "EXACT_NOTICE_SOURCE_MARKER" not in text
                            terminal.send("\t\r")
                            scene("exact-original", lambda s: "EXACT_NOTICE_SOURCE_MARKER" in s and "原文" in s)
                        else:
                            terminal.send("/switch visual-notice-parent\r")
                            scene("current-name", lambda s: "現在の名前: Twin" in s)
                            terminal.send("\t")
                            # Tab pauses follow and selects history, not F2 display freeze.
                            scene("paused-child", lambda s: "o 原文" in s and "v" in s and "現在の名前: Twin" in s)
                            cli("thread", "set", "--thread", "visual-notice-child", "--title", "RenamedChild")
                            scene("renamed-without-input", lambda s: "現在の名前: RenamedChild" in s)
                            terminal.send("\r")
                            text = scene("exact-child", lambda s: "RIGHT_CHILD_MARKER" in s)
                            assert "WRONG_CHILD_MARKER" not in text
                            terminal.send("/switch visual-notice-parent\r")
                            scene("parent-again", lambda s: "現在の名前: RenamedChild" in s)
                            cli("thread", "delete", "--thread", "visual-notice-child")
                            scene("deleted-without-input", lambda s: "削除済み" in s or "参照先の会話を取得できません" in s)
                        assert b"\x1b]52;" not in terminal.raw
                        manifest["cases"].append({"name": name, "no_host_clipboard": True})
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
