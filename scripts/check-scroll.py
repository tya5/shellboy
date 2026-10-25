#!/usr/bin/env python3
"""Opt-in real-WezTerm scroll checks using visual_fixture's synthetic histories.

Records terminal cells, not window screenshots. No model/provider calls.
"""
import argparse
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-root", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/shellboy"))
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if not (args.fixture_root / ".shellboy-visual-fixture").is_file():
        parser.error("Create a dedicated synthetic fixture with visual_fixture first")
    output = args.out.resolve()
    output.mkdir(parents=True, exist_ok=True)
    driver = Path(__file__).with_name("capture-tui.py")
    up, down, end = "\x1b[<64;30;10M", "\x1b[<65;30;10M", "\x1b[1;5F"
    cases = [
        ("tail", "lines", []), ("up", "lines", [up]),
        ("inverse", "lines", [up, down]), ("burst", "lines", [up * 10]),
        ("page", "lines", ["\x1b[5~"]),
        ("overlay", "lines", ["\x1bOP", up, "\x1b"]),
        ("messages-tail", "messages", []), ("messages-up", "messages", [up]),
        ("messages-return-tail", "messages", [up, end, up]),
        ("messages-boundary", "messages", [up * 40]),
        ("messages-slow-forward", "messages", [up * 40] + [down] * 20),
        ("wrap-tail", "wrap", []), ("wrap-up", "wrap", [up]),
        ("body-start", "body", []), ("body-backward", "body", [up * 30]),
        ("body-forward", "body", ["\x1b[6~" * 35]),
    ]
    results = {}
    for label, thread, keys in cases:
        target = output / f"{label}.txt"
        command = [sys.executable, str(driver), "--fixture-root", str(args.fixture_root),
                   "--binary", str(args.binary), "--thread", "visual-scroll-" + thread,
                   "--out", str(target), "--cols", "100", "--rows", "36", "--text-only"]
        for key in keys:
            command.extend(["--keys", key])
        if thread == "body":
            # Fixture setup only, while its dedicated owner is stopped. Each
            # comparison starts at the same saved original-character position.
            if (args.fixture_root / "data/endpoint.json").exists():
                raise RuntimeError("The dedicated fixture owner is still running")
            if not (args.fixture_root / "data/shellboy.db").is_file():
                raise RuntimeError("The synthetic fixture database does not exist")
            with sqlite3.connect(args.fixture_root / "data/shellboy.db") as db:
                changed = db.execute("UPDATE view_state SET anchor_node_id='visual-scroll-body-node-0', anchor_offset=65000, follow_tail=0 WHERE thread_id='visual-scroll-body'").rowcount
                if changed != 1:
                    raise AssertionError("Recreate the fixture with the current visual_fixture example")
            command.append("--keep-view")
        subprocess.run(command, check=True, timeout=60)
        markers = re.findall(r"[RMP]\d{3}|B\d{5}", target.read_text())
        if not markers:
            raise AssertionError(f"No fixture row markers in {target}")
        results[label] = {"first": markers[0], "last": markers[-1]}
        print(json.dumps({label: results[label]}), flush=True)

    def body(label):
        # Fixed 100x36 terminal and empty composer: the real history interior.
        return [line.rstrip() for line in
                (output / f"{label}.txt").read_text().splitlines()[2:30]]

    def absolute_top(label):
        # Each short-message node is one content row plus its separator row.
        for row, line in enumerate(body(label)):
            match = re.search(r"M(\d{3})", line)
            if match:
                return 2 * (int(match[1]) - 1) - row
        raise AssertionError(f"No message marker in {label}")

    checks = {
        "wheel_three_rows": body("up")[3:] == body("tail")[:-3],
        "inverse_exact": body("inverse") == body("tail"),
        "burst_thirty_rows": int(results["tail"]["first"][1:]) - int(results["burst"]["first"][1:]) == 30,
        "page_twentyseven_rows": int(results["tail"]["first"][1:]) - int(results["page"]["first"][1:]) == 27,
        "overlay_background_fixed": body("overlay") == body("tail"),
        "short_messages_three_rows": body("messages-up")[3:] == body("messages-tail")[:-3],
        "return_tail_ignores_old_anchor": body("messages-return-tail") == body("messages-up"),
        "wrapped_japanese_three_rows": body("wrap-up")[3:] == body("wrap-tail")[:-3],
        "async_boundary_preserves_120_rows": absolute_top("messages-tail") - absolute_top("messages-boundary") == 120,
        "settled_forward_wheels_preserve_60_rows": absolute_top("messages-slow-forward") - absolute_top("messages-boundary") == 60,
        "body_window_backward_90_rows": int(results["body-start"]["first"][1:]) - int(results["body-backward"]["first"][1:]) == 90,
        "body_window_forward_945_rows": int(results["body-forward"]["first"][1:]) - int(results["body-start"]["first"][1:]) == 945,
    }
    (output / "summary.json").write_text(json.dumps(results, indent=2) + "\n")
    (output / "checks.json").write_text(json.dumps(checks, indent=2) + "\n")
    print(json.dumps(checks), flush=True)
    if not all(checks.values()):
        raise AssertionError(checks)


if __name__ == "__main__":
    main()
