#!/usr/bin/env python3
"""Check input completion/recall in a dedicated real WezTerm fixture."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-root", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/shellboy"))
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    driver = Path(__file__).with_name("capture-tui.py")
    cases = {
        "all": ["/"],
        "filtered": ["/at"],
        "selected": ["/at", "\x1b[B\t"],
        "newest": ["書きかけ", "\x1b[A"],
        "older": ["書きかけ", "\x1b[A\x1b[A"],
        "restored": ["書きかけ", "\x1b[A\x1b[B"],
        "dismissed": ["/at", "\x1b"],
    }
    scenes = {}
    for name, sequences in cases.items():
        target = args.out / f"{name}.txt"
        command = [sys.executable, str(driver), "--fixture-root", str(args.fixture_root),
                   "--binary", str(args.binary), "--thread", "visual-composer",
                   "--out", str(target), "--cols", "100", "--rows", "36", "--text-only"]
        # Normal shutdown persists drafts. Reset only this stopped, marked
        # fixture so every scene starts with an empty composer.
        reset_fixture_draft(args.fixture_root)
        for sequence in sequences:
            command.extend(["--keys", sequence])
        subprocess.run(command, check=True, timeout=60)
        text = target.read_text()
        match = re.search(r"┌メッセージ[^\n]*\n(.*?)\n└", text, re.S)
        if not match:
            raise AssertionError(f"Missing composer in {name}")
        scenes[name] = {"text": text, "draft": "\n".join(line.strip("│ ") for line in match[1].splitlines())}
    checks = {
        "all_candidates": all(name in scenes["all"]["text"] for name in ["/help", "/new", "/threads"]),
        "filtered_candidates": "/attachments" in scenes["filtered"]["text"] and "/compact" not in scenes["filtered"]["text"],
        "selection_and_tab": scenes["selected"]["draft"].strip() == "/attachments" and "Tab/Enter 補完" not in scenes["selected"]["text"],
        "unicode_multiline_recall": scenes["newest"]["draft"] == "H03 最新の依頼🙂\n次の行も復元",
        "ordered_up_burst": scenes["older"]["draft"].strip() == "/help" and "Tab/Enter 補完" not in scenes["older"]["text"],
        "down_restores_draft": scenes["restored"]["draft"].strip() == "書きかけ",
        "escape_keeps_input": scenes["dismissed"]["draft"].strip() == "/at" and "Tab/Enter 補完" not in scenes["dismissed"]["text"],
    }
    (args.out / "checks.json").write_text(json.dumps(checks, indent=2) + "\n")
    print(json.dumps(checks), flush=True)
    if not all(checks.values()):
        raise AssertionError(checks)


def reset_fixture_draft(root):
    import sqlite3
    if not (root / ".shellboy-visual-fixture").is_file() or (root / "data/endpoint.json").exists():
        raise RuntimeError("A stopped synthetic visual_fixture is required")
    database = root / "data/shellboy.db"
    if not database.is_file():
        raise RuntimeError("The synthetic fixture database does not exist")
    with sqlite3.connect(database) as db:
        if not db.execute("SELECT 1 FROM thread WHERE id='visual-composer'").fetchone():
            raise RuntimeError("Recreate the fixture with the current visual_fixture example")
        db.execute("DELETE FROM draft WHERE thread_id='visual-composer'")


if __name__ == "__main__":
    main()
