#!/usr/bin/env python3
"""Wheel and PageUp over a live sequence of synthetic shell tool rows."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import queue
import re
import select
import threading
import time

from dev.pty_capture import Terminal, WATCHDOG_SECONDS

MARKER = re.compile(r"TOOLROW(\d{3})")
UP, DOWN = "\x1b[<64;30;10M", "\x1b[<65;30;10M"


class Server(ThreadingHTTPServer):
    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.commands = queue.Queue()
        self.arrivals = queue.Queue()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.arrivals.put(True)
        # The test owns the watchdog; keep the live request open until teardown.
        index = self.server.commands.get()
        output = [] if index is None else [{"type": "function_call", "name": "shell",
            "call_id": f"synthetic-{index}", "arguments": json.dumps({
                "command": f"printf TOOLROW{index:03}", "description": f"TOOLROW{index:03}"})}]
        if index is None:
            output = [{"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "TOOL_FIXTURE_COMPLETE"}]}]
        response = {"id": f"tool-fixture-{index}", "status": "completed", "output": output}
        body = ("data: " + json.dumps({"type": "response.completed", "response": response}) + "\n\n").encode()
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_args):
        pass


def top(text):
    values = [int(value) for value in MARKER.findall(text)]
    return min(values) if values else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    server = Server()
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    terminal = Terminal(args.binary.resolve(), cols=100, rows=36,
                        connection_url=f"http://127.0.0.1:{server.server_port}/v1")
    snapshots = {}

    def record(label, predicate):
        text = terminal.expect(predicate, label)
        snapshots[label] = top(text)
        (args.out / (label + ".txt")).write_text(text + "\n")
        return top(text)

    def arrival():
        deadline = time.monotonic() + WATCHDOG_SECONDS
        while True:
            try:
                server.arrivals.get_nowait()
                return
            except queue.Empty:
                if time.monotonic() >= deadline:
                    raise TimeoutError("tool request watchdog")
                if terminal.process.poll() is not None:
                    raise RuntimeError("TUI exited during tool fixture")
                if select.select([terminal.master], [], [], .05)[0]:
                    terminal.read_ready()

    def step(index):
        arrival()
        server.commands.put(index)

    try:
        record("ready", lambda text: "F1" in text)
        terminal.send("synthetic tool fixture\r")
        for index in range(40):
            step(index)
        arrival()
        end = record("tail", lambda text: "TOOLROW039" in text)
        previous = end
        for index in range(4):
            terminal.send(UP)
            old = previous
            previous = record(f"up-{index}", lambda text: top(text) is not None and top(text) < old)
        server.commands.put(40)
        arrival()
        terminal.expect(lambda text: "↓ new" in text, "new tool while paused")
        for index in range(3):
            terminal.send(DOWN)
            old = previous
            previous = record(f"down-{index}", lambda text: top(text) is not None and top(text) > old)
        assert previous < end, (previous, end)
        server.commands.put(41)
        arrival()
        terminal.send(UP)
        old = previous
        previous = record("reverse", lambda text: top(text) is not None and top(text) < old)
        server.commands.put(42)
        arrival()
        terminal.send("\x1b[5~")
        record("page-up", lambda text: top(text) is not None and top(text) < previous)
        (args.out / "results.json").write_text(json.dumps(snapshots, indent=2) + "\n")
    finally:
        (args.out / "last-screen.txt").write_text(terminal.cells() + "\n")
        (args.out / "results.json").write_text(json.dumps(snapshots, indent=2) + "\n")
        server.commands.put(None)
        terminal.close()
        (args.out / "terminal.raw").write_bytes(terminal.raw)
        server.shutdown()
        server.server_close()
        worker.join(timeout=WATCHDOG_SECONDS)


if __name__ == "__main__":
    main()
