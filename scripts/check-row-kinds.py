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
    args = parser.parse_args()
    binary, fixture = args.binary.resolve(), args.fixture.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = []
    try:
        for cols in (80, 140):
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
                        f'[ui]\npalette="{palette}"\nbell=false\n')
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
                        assert all(mark in static for mark in ["›", "⬦", "⁂", "◌"])
                        assert "⁂ 通知" in static and "子の回答" in static
                        # The unavailable-tool visual scene is deliberately not
                        # valid model replay. Start a fresh synthetic conversation.
                        terminal.send("/new\r")
                        scene("new-conversation", lambda s: "F1 help" in s and "UNKNOWN_OBSERVATION" not in s)
                        terminal.send("ROW_FIXTURE_START\r")
                        live = scene("background-and-foreground", lambda s: "ROW_FIXTURE_DONE" in s and
                                     "⏵ 背景" in s and "printf FAST_ONE" in s and "printf FAST_TWO" in s)
                        assert "▸ ROW_FIXTURE_BODY" in live
                        assert re.search(r"⏵.*背景.*\d+秒", live), live
                        rows = live.splitlines()
                        short = [i for i, line in enumerate(rows) if "printf FAST_" in line]
                        assert len(short) == 2 and short[1] == short[0] + 1, "consecutive tools must stay dense"
                        assert all("✓" in rows[i] and "終了 0" in rows[i] for i in short)
                        with sqlite3.connect(f"file:{root / 'data/shellboy.db'}?mode=ro", uri=True) as db:
                            execution = db.execute("SELECT id FROM shell_execution WHERE state='running' AND json_extract(shell_json,'$.command') LIKE 'read fixture_line%' LIMIT 1").fetchone()[0]
                        # Use the fixture owner only; complete the real background read.
                        result = subprocess.run([str(binary), "--data-dir", str(root / "data"),
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
                        results.append({"case": name, "observations": observations, "execution_id": execution})
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
