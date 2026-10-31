#!/usr/bin/env python3
"""Observe #84 through a real Unix PTY and pyte screen (test-only dependency)."""
import argparse
import codecs
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time

import pyte

WATCHDOG_SECONDS = 60


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


class Terminal:
    def __init__(self, binary, root, cols, palette, term, no_color):
        self.screen = pyte.Screen(cols, 36)
        self.stream = pyte.Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.raw = bytearray()
        self.paint = bytearray()
        self.completed_text = ""
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, cols, 0, 0))
        env = dict(os.environ, TERM=term)
        env.pop("COLORTERM", None)
        env.pop("NO_COLOR", None)
        # Do not inherit the agent's conversation/owner context.
        env = {k: v for k, v in env.items() if not k.startswith("SHELLBOY_")}
        if no_color:
            env["NO_COLOR"] = "1"
        self.process = subprocess.Popen(
            [str(binary), "--config", str(root / "config.toml"),
             "--data-dir", str(root / "data")], cwd=root, env=env,
            stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        os.set_blocking(self.master, False)

    def cells(self):
        # pyte's display property crashes on orphan wide-character stub cells
        # during Ratatui differential repaints. Read its parsed cell data directly.
        return "\n".join("".join(self.screen.buffer[y][x].data
                                 for x in range(self.screen.columns))
                         for y in range(self.screen.lines))

    def text(self):
        return self.completed_text

    def feed_output(self, chunk):
        self.raw.extend(chunk)
        self.paint.extend(chunk)
        # Ratatui ends each painted frame by hiding the cursor or showing and
        # positioning it. Capture there, not halfway through a wide-cell repaint.
        boundary = rb"\x1b\[\?25l|\x1b\[\?25h\x1b\[\d+;\d+H"
        while match := re.search(boundary, self.paint):
            self.stream.feed(self.decoder.decode(self.paint[:match.end()]))
            del self.paint[:match.end()]
            self.completed_text = self.cells()
        if len(self.raw) > 4 * 1024 * 1024:
            raise AssertionError("PTY output flood")

    def read_ready(self):
        try:
            self.feed_output(os.read(self.master, 65536))
        except BlockingIOError:
            pass

    def send(self, text):
        pending = text.encode()
        deadline = time.monotonic() + WATCHDOG_SECONDS
        # Drain output while delivering paste: both PTY directions are bounded.
        while pending:
            if time.monotonic() >= deadline:
                raise AssertionError("60s PTY input watchdog")
            readable, writable, _ = select.select([self.master], [self.master], [], .05)
            if readable:
                self.read_ready()
            if writable:
                try:
                    pending = pending[os.write(self.master, pending[:256]):]
                except BlockingIOError:
                    pass

    def paste(self, text):
        self.send("\x1b[200~" + text + "\x1b[201~")

    def expect(self, predicate, label):
        # This is solely a hung-test watchdog, never a latency acceptance gate.
        deadline = time.monotonic() + WATCHDOG_SECONDS
        while not predicate(self.text()):
            if time.monotonic() >= deadline:
                raise AssertionError(f"60s watchdog: {label}\n{self.text()}")
            if self.process.poll() is not None:
                raise AssertionError(f"TUI exited before {label}: {self.process.returncode}")
            readable, _, _ = select.select([self.master], [], [], .05)
            if readable:
                self.read_ready()
        return self.text()

    def close(self):
        # Ctrl+U clears only this disposable draft; typed /quit requests normal shutdown.
        if self.process.poll() is None:
            self.send("\x15/quit\r")
            deadline = time.monotonic() + WATCHDOG_SECONDS
            while self.process.poll() is None:
                if time.monotonic() >= deadline:
                    os.killpg(self.process.pid, signal.SIGKILL)
                    self.process.wait()
                    raise AssertionError("60s shutdown watchdog")
                readable, _, _ = select.select([self.master], [], [], .05)
                if readable:
                    try:
                        self.raw.extend(os.read(self.master, 65536))
                    except OSError:
                        break
            self.process.wait()
        os.close(self.master)
        if self.process.returncode:
            raise AssertionError(f"TUI exit code {self.process.returncode}")


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
                        f'[ui]\npalette = "{palette}"\nbell = false\n')
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
