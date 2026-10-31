#!/usr/bin/env python3
"""Observe #84 through the shared standard-library Unix PTY driver."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

from dev.pty_capture import Terminal, WATCHDOG_SECONDS




class Mock(BaseHTTPRequestHandler):
    requests = []

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.requests.append(body)
        answer = "PASTE_FIXTURE_REPLY"
        response = {"id": "paste-fixture", "status": "completed", "output": [
            {"type": "message", "id": "answer", "role": "assistant",
             "phase": "final_answer", "content": [{"type": "output_text", "text": answer}]}]}
        events = [{"type": "response.output_text.delta", "delta": answer},
                  {"type": "response.completed", "response": response}]
        payload = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    binary = args.binary.resolve()
    server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = []
    try:
        for cols in (80, 140):
            for palette, term, no_color in [
                ("dark", "xterm-256color", False),
                ("light", "xterm-256color", False),
                ("dark", "xterm-256color", True),
                ("dark", "vt100", False),
            ]:
                name = f"{cols}-{palette}-{term}-no-color-{int(no_color)}"
                case_out = args.out / name
                case_out.mkdir()
                with tempfile.TemporaryDirectory(prefix="shellboy-paste-fixture-") as scratch:
                    root = Path(scratch)
                    (root / "config.toml").write_text(
                        'default_connection = "fixture"\ndefault_model = "fixture"\n'
                        '[connections.fixture]\n'
                        f'base_url = "http://127.0.0.1:{server.server_port}/v1"\n'
                        f'[ui]\ncopy_backend="osc52"\npalette = "{palette}"\nbell = false\n')
                    terminal = Terminal(binary, root, cols, palette, term, no_color)
                    start_requests = len(Mock.requests)
                    captures = []

                    def capture(label, predicate):
                        terminal.send("\x0c")
                        text = terminal.expect(predicate, label)
                        (case_out / f"{label}.txt").write_text(text + "\n")
                        captures.append(label)

                    try:
                        capture("ready", lambda t: "fixture" in t and "F1" in t)
                        # Paste arguments first, then type the command at Home.
                        terminal.paste("pasted-argument")
                        capture("paste-only", lambda t: "pasted-argument" in t)
                        terminal.send("\x1b[H/help \r")
                        capture("paste-then-typed-command", lambda t: "操作ガイドを開く" in t)
                        assert len(Mock.requests) == start_requests
                        terminal.send("\r")
                        capture("help-closed", lambda t: "操作ガイドを開く" not in t)
                        terminal.send("/help ")
                        terminal.paste("another-argument")
                        terminal.send("\r")
                        capture("typed-command-pasted-argument", lambda t: "操作ガイドを開く" in t)
                        assert len(Mock.requests) == start_requests
                        terminal.send("\r")
                        capture("second-help-closed", lambda t: "操作ガイドを開く" not in t)
                        long_text = "/quit\n" + "synthetic document /help content\n" * 40
                        terminal.paste(long_text)
                        capture("folded-paste-warning", lambda t: "[貼付け" in t and
                                "貼り付けた文として送ります" in t and "手で入力してください" in t)
                        assert len(Mock.requests) == start_requests
                        assert "synthetic document" not in terminal.text()
                        terminal.send("\x15")
                        capture("cleared", lambda t: "[貼付け" not in t and
                                "貼り付けた文として送ります" not in t)
                        terminal.paste("/help pasted document")
                        capture("pasted-slash-warning", lambda t: "/help pasted document" in t and
                                "貼り付けた文として送ります" in t)
                        terminal.send("\r")
                        capture("literal-sse-reply", lambda t: "PASTE_FIXTURE_REPLY" in t)
                        assert len(Mock.requests) == start_requests + 1
                        assert "/help pasted document" in json.dumps(Mock.requests[-1])
                        results.append({"case": name, "passed": captures, "requests": 1})
                    except Exception:
                        (case_out / "failure.txt").write_text(terminal.text() + "\n")
                        (case_out / "terminal.raw").write_bytes(terminal.raw)
                        raise
                    finally:
                        try:
                            terminal.close()
                        finally:
                            (case_out / "terminal.raw").write_bytes(terminal.raw)
        (args.out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(json.dumps(results, indent=2))
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
