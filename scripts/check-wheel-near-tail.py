#!/usr/bin/env python3
"""Exercise near-tail wheel reversal in a disposable PTY with loopback SSE."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import queue
import re
import threading

from dev.pty_capture import Terminal, WATCHDOG_SECONDS

MARKER = re.compile(r"WHEELROW(\d{3})")
UP = "\x1b[<64;30;10M"
DOWN = "\x1b[<65;30;10M"


class FixtureServer(ThreadingHTTPServer):
    def __init__(self, live):
        super().__init__(("127.0.0.1", 0), Handler)
        self.live = live
        self.commands = queue.Queue()
        self.requests = 0


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.requests += 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()

        def event(value):
            self.wfile.write(("data: " + json.dumps(value) + "\n\n").encode())
            self.wfile.flush()

        text = "".join(f"WHEELROW{i:03} synthetic output\n\n" for i in range(80))
        try:
            event({"type": "response.output_text.delta", "delta": text})
            if self.server.live:
                while True:
                    command = self.server.commands.get(timeout=WATCHDOG_SECONDS)
                    if command is None:
                        break
                    extra = f"WHEELROW{command:03} synthetic output\n\n"
                    text += extra
                    event({"type": "response.output_text.delta", "delta": extra})
            response = {"id": "wheel-fixture", "status": "completed", "output": [
                {"type": "message", "id": "wheel-answer", "role": "assistant",
                 "phase": "final_answer", "content": [{"type": "output_text", "text": text}]}]}
            event({"type": "response.completed", "response": response})
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_args):
        pass


def top(text):
    ids = [int(value) for value in MARKER.findall(text)]
    return min(ids) if ids else None


def run(binary, out, live):
    server = FixtureServer(live)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    terminal = Terminal(binary, cols=100, rows=36,
                        connection_url=f"http://127.0.0.1:{server.server_port}/v1")
    snapshots = {}

    def record(label, predicate):
        text = terminal.expect(predicate, label)
        snapshots[label] = {"top": top(text), "markers": MARKER.findall(text)}
        (out / (label + ".txt")).write_text(text + "\n")
        return snapshots[label]["top"]

    try:
        record("ready", lambda text: "F1" in text)
        terminal.send("synthetic wheel test\r")
        end = record("tail", lambda text: "WHEELROW079" in text)
        if not live:
            # Wait for the committed response rather than racing the SSE finish.
            terminal.expect(lambda text: "WHEELROW079" in text and "✓" in text, "completed")
        above = end
        for index in range(4):
            terminal.send(UP)
            previous = above
            above = record(f"up-{index}", lambda text: top(text) is not None and top(text) < previous)
        if live:
            server.commands.put(80)
            terminal.expect(lambda text: "v new" in text, "live update while paused")
        near = above
        for index in range(3):
            terminal.send(DOWN)
            previous = near
            near = record(f"down-{index}", lambda text: top(text) is not None and top(text) > previous)
        assert near < end, (near, end)
        if live:
            server.commands.put(81)
        terminal.send(UP)
        reversed_top = record("reverse", lambda text: top(text) is not None and top(text) < near)
        if live:
            server.commands.put(82)
        terminal.send("\x1b[5~")  # PageUp, independently of the mouse input path.
        record("page-up", lambda text: top(text) is not None and top(text) < reversed_top)
        assert server.requests == 1, server.requests
        return snapshots
    except Exception:
        (out / "failure.txt").write_text(terminal.text() + "\n")
        raise
    finally:
        server.commands.put(None)
        terminal.close()
        (out / "terminal.raw").write_bytes(terminal.raw)
        server.shutdown()
        server.server_close()
        thread.join(timeout=WATCHDOG_SECONDS)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    results = {}
    for live in (False, True):
        name = "live" if live else "stopped"
        out = args.out / name
        out.mkdir()
        results[name] = run(args.binary.resolve(), out, live)
    (args.out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
