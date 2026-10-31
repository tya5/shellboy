#!/usr/bin/env python3
"""Actual Unix PTY row vocabulary check; uses #84 pyte capture and loopback SSE."""
import argparse
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import tempfile
import threading

spec = importlib.util.spec_from_file_location("paste_capture", Path(__file__).with_name("check-paste-commands.py"))
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


class Mock(BaseHTTPRequestHandler):
    count = 0

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        Mock.count += 1
        phase = (Mock.count - 1) % 3
        text = ["ROW_FIXTURE_BODY", "ROW_FIXTURE_DONE", "ROW_FIXTURE_NOTIFICATION"][phase]
        output = [{"type": "message", "id": f"row-message-{Mock.count}", "role": "assistant",
                   "phase": "final_answer", "content": [{"type": "output_text", "text": text}]}]
        if phase == 0:
            for index, (command, background) in enumerate([
                ("read fixture_line; printf BACKGROUND_DONE", True),
                ("printf FAST_ONE", False), ("printf FAST_TWO", False),
            ]):
                arguments = {"command": command, "background": background}
                if background:
                    arguments["stdin_open"] = True
                output.append({"type": "function_call", "name": "shell", "id": f"row-call-{index}",
                               "call_id": f"row-call-{index}", "arguments": json.dumps(arguments)})
        if phase == 2:
            assert "shell_ended" in json.dumps(request), "notification must reach model input"
        response = {"id": f"row-response-{Mock.count}", "status": "completed", "output": output}
        events = [{"type": "response.output_text.delta", "delta": text},
                  {"type": "response.completed", "response": response}]
        body = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--columns", type=int, nargs="+", default=[80, 140])
    parser.add_argument("--metadata-baseline", type=Path)
    args = parser.parse_args()
    binary, fixture = args.binary.resolve(), args.fixture.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = []
    try:
        for cols in args.columns:
            for palette, term, no_color in [("dark", "xterm-256color", False),
                                            ("light", "xterm-256color", False),
                                            ("dark", "xterm-256color", True),
                                            ("dark", "vt100", False)]:
                name = f"{cols}-{palette}-{term}-plain-{int(no_color)}"
                out = args.out / name
                out.mkdir()
                with tempfile.TemporaryDirectory(prefix="shellboy-row-fixture-") as scratch:
                    root = Path(scratch) / "fixture"
                    subprocess.run([str(fixture), str(root)], check=True, capture_output=True,
                                   timeout=capture.WATCHDOG_SECONDS)
                    (root / "config.toml").write_text(
                        'default_connection="visual"\ndefault_model="offline-fixture"\n'
                        '[connections.visual]\n'
                        f'base_url="http://127.0.0.1:{server.server_port}/v1"\n'
                        f'[ui]\ncopy_backend="osc52"\npalette="{palette}"\nbell=false\n')
                    terminal = capture.Terminal(binary, root, cols, palette, term, no_color)
                    observations = []

                    def scene(label, predicate):
                        terminal.send("\x0c")
                        text = terminal.expect(predicate, label)
                        (out / (label + ".txt")).write_text(text + "\n")
                        observations.append(label)
                        return text

                    try:
                        scene("ready", lambda s: "F1 help" in s)
                        terminal.send("/switch visual-row-kinds\r")
                        static = scene("static-kinds", lambda s: "ROW_FIXTURE_USER" in s and
                                       "ROW_FIXTURE_SUMMARY" in s and "Fixture child" in s and
                                       "UNKNOWN_OBSERVATION" in s)
                        # The synthetic input is external CLI, not a TUI human receipt.
                        assert all(mark in static for mark in ["⇥", "⬦", "⁂", "◌"])
                        assert "⁂ 通知" in static and "子の回答" in static
                        terminal.send("/switch visual-shell-metadata\r")
                        scene("shell-metadata", lambda s: all(f"METADATA_{kind}" in s for kind in
                              ("SUCCESS", "NONZERO", "FAILED", "CANCELLED")))

                        def metadata_cells(display):
                            facts = {}
                            for y in range(display.screen.lines):
                                row = display.screen.buffer[y]
                                text = "".join(row[x].data for x in range(cols))
                                for kind in ("SUCCESS", "NONZERO", "FAILED", "CANCELLED"):
                                    if f"METADATA_{kind}" not in text:
                                        continue
                                    # pyte may retain empty orphan stubs after a differential repaint;
                                    # they display as blank cells, not different visible text.
                                    cells = [(row[x].data or " ", row[x].fg, row[x].bg) for x in range(cols)]
                                    while cells and cells[-1][0] == " ":
                                        cells.pop()
                                    facts[kind] = cells
                            assert len(facts) == 4, facts
                            return facts

                        metadata = metadata_cells(terminal)
                        muted = "default" if no_color or term == "vt100" else (
                            "8a8a8a" if palette == "dark" else "4e4e4e")
                        for kind, cells in metadata.items():
                            expected = muted if kind == "SUCCESS" or no_color else "ff0000"
                            assert cells[-1][1] == expected, (kind, cells[-1], expected)
                        (out / "metadata-cells.json").write_text(json.dumps(metadata, ensure_ascii=False, indent=2))
                        if no_color and args.metadata_baseline:
                            with tempfile.TemporaryDirectory(prefix="shellboy-metadata-baseline-") as scratch_old:
                                old_root = Path(scratch_old) / "fixture"
                                subprocess.run([str(fixture), str(old_root)], check=True,
                                               capture_output=True, timeout=capture.WATCHDOG_SECONDS)
                                (old_root / "config.toml").write_text((root / "config.toml").read_text())
                                previous = capture.Terminal(args.metadata_baseline.resolve(), old_root,
                                                            cols, palette, term, no_color)
                                try:
                                    previous.expect(lambda s: "F1 help" in s, "baseline-ready")
                                    previous.send("/switch visual-shell-metadata\r")
                                    previous.expect(lambda s: "METADATA_CANCELLED" in s,
                                                    "baseline-metadata")
                                    old_metadata = metadata_cells(previous)
                                    (out / "baseline-cells.json").write_text(json.dumps(old_metadata, ensure_ascii=False, indent=2))
                                    differences = {kind: [(i, old, new) for i, (old, new) in
                                                   enumerate(zip(old_metadata[kind], metadata[kind]))
                                                   if old != new][:8] for kind in metadata}
                                    assert old_metadata == metadata, ("NO_COLOR differs from baseline", differences)
                                finally:
                                    previous.close()
                                    (out / "baseline.raw").write_bytes(previous.raw)
                        terminal.send("/switch visual-row-kinds\r")
                        scene("return-to-static", lambda s: "UNKNOWN_OBSERVATION" in s)
                        # The unavailable-tool visual scene is deliberately not
                        # valid model replay. Start a fresh synthetic conversation.
                        terminal.send("/new\r")
                        scene("new-conversation", lambda s: "F1 help" in s and "UNKNOWN_OBSERVATION" not in s)
                        terminal.send("ROW_FIXTURE_START\r")
                        live = scene("background-and-foreground", lambda s: "ROW_FIXTURE_DONE" in s and
                                     "⏵ 背景" in s and "printf FAST_ONE" in s and "printf FAST_TWO" in s)
                        assert "▸ ROW_FIXTURE_BODY" in live
                        live_color = None
                        for y in range(terminal.screen.lines):
                            row = terminal.screen.buffer[y]
                            text = "".join(row[x].data for x in range(cols))
                            if "⏵" in text and "背景" in text:
                                occupied = [row[x] for x in range(cols) if row[x].data.strip()]
                                live_color = occupied[-1].fg
                        assert live_color == muted, (live_color, muted)

                        assert re.search(r"⏵.*背景.*\d+秒", live), live
                        rows = live.splitlines()
                        short = [i for i, line in enumerate(rows) if "printf FAST_" in line]
                        assert len(short) == 2 and short[1] == short[0] + 1, "consecutive tools must stay dense"
                        assert all("✓" in rows[i] and "終了 0" in rows[i] for i in short)
                        with sqlite3.connect(f"file:{root / 'data/shellboy.db'}?mode=ro", uri=True) as db:
                            execution = db.execute("SELECT id FROM shell_execution WHERE state='running' AND json_extract(shell_json,'$.command') LIKE 'read fixture_line%' LIMIT 1").fetchone()[0]
                        # Use the fixture owner only; complete the real background read.
                        result = subprocess.run([str(binary), "--config", str(root / "config.toml"), "--data-dir", str(root / "data"),
                                                 "shell", "write", "--execution", execution,
                                                 "--text", "complete\n"], capture_output=True,
                                                text=True, timeout=capture.WATCHDOG_SECONDS, check=True)
                        assert json.loads(result.stdout)["ok"]
                        notice = scene("shell-notification", lambda s: "ROW_FIXTURE_NOTIFICATION" in s and
                                       "shell 終了" in s and "モデルに渡した" in s)
                        assert "終了 0" in notice and "Enter参照先" in "".join(notice.split())
                        # Last two items are quiet groups; choose the shell
                        # notice before the final answer and follow it with Enter.
                        terminal.send("\t\x1b[A\x1b[A\x1b[A\r ")
                        scene("notice-jump-open-tool", lambda s: "│ BACKGROUND_DONE" in s)
                        terminal.send(" \t")
                        scene("tool-closed-input", lambda s: "│ BACKGROUND_DONE" not in s and "F1 help" in s)
                        results.append({"case": name, "observations": observations, "execution_id": execution, "metadata_colors": {kind: cells[-1][1] for kind, cells in metadata.items()}})
                    except Exception:
                        (out / "failure.txt").write_text(terminal.text() + "\n")
                        raise
                    finally:
                        try:
                            terminal.close()
                        finally:
                            (out / "terminal.raw").write_bytes(terminal.raw)
        (args.out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(json.dumps(results, indent=2))
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
