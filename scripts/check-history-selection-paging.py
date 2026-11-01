#!/usr/bin/env python3
"""Exercise selection navigation across a synthetic persisted history in a PTY."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import tempfile

from dev.pty_capture import Terminal, WATCHDOG_SECONDS

MARKER = re.compile(r"ARCHIVE_ROW_(\d{2})")


def oldest(text):
    values = [int(value) for value in MARKER.findall(text)]
    return min(values) if values else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--fixture", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="shellboy-selection-fixture-") as tmp:
        root = Path(tmp) / "fixture"
        subprocess.run([str(args.fixture.resolve()), str(root)], check=True,
                       capture_output=True, timeout=WATCHDOG_SECONDS)
        terminal = Terminal(args.binary.resolve(), root=root, cols=100, rows=36)
        try:
            terminal.expect(lambda text: "F1" in text, "ready")
            terminal.send("/switch visual-notice-shell\r")
            text = terminal.expect(lambda text: "ARCHIVE_ROW_69" in text, "tail")
            (args.out / "tail.txt").write_text(text + "\n")
            initial = oldest(text)
            assert initial is not None and initial > 0, initial
            terminal.send("\t")
            terminal.expect(lambda text: "o 原文" in text or "o原文" in text,
                            "history selection")
            # With more choices than fit in the viewport, repeated Up must
            # cross the first visible row rather than stopping at that edge.
            terminal.send("\x1b[A" * 30)
            text = terminal.expect(lambda value: oldest(value) is not None and oldest(value) < initial,
                                   "selection crosses viewport top")
            (args.out / "crossed.txt").write_text(text + "\n")
            terminal.send("\x1b[A" * 90)
            text = terminal.expect(lambda value: "ARCHIVE_ROW_00" in value,
                                   "selection reaches earlier persisted history")
            (args.out / "oldest.txt").write_text(text + "\n")
            (args.out / "result.json").write_text(json.dumps({"tail_top": initial,
                "oldest_top": oldest(text)}) + "\n")
        finally:
            terminal.close()
            (args.out / "terminal.raw").write_bytes(terminal.raw)


if __name__ == "__main__":
    main()
