#!/usr/bin/env python3
"""Capture the real macOS WezTerm TUI against a marked, synthetic fixture.

Requires examples/visual_fixture.rs output, Python 3.11+, WezTerm and macOS.
Image capture also requires screen recording permission; --text-only records
terminal cells. Never connects to a user's running Shellboy owner.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import time
import tomllib
from urllib.parse import unquote, urlparse


def window_list():
    graphics = ctypes.CDLL(
        "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics"
    )
    foundation = ctypes.CDLL(
        "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
    )
    graphics.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    graphics.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
    foundation.CFPropertyListCreateData.argtypes = [
        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_long, ctypes.c_ulong, ctypes.c_void_p
    ]
    foundation.CFPropertyListCreateData.restype = ctypes.c_void_p
    foundation.CFDataGetLength.argtypes = [ctypes.c_void_p]
    foundation.CFDataGetLength.restype = ctypes.c_long
    foundation.CFDataGetBytePtr.argtypes = [ctypes.c_void_p]
    foundation.CFDataGetBytePtr.restype = ctypes.c_void_p
    foundation.CFRelease.argtypes = [ctypes.c_void_p]
    windows = graphics.CGWindowListCopyWindowInfo(0, 0)
    blob = foundation.CFPropertyListCreateData(None, windows, 200, 0, None)
    try:
        return plistlib.loads(ctypes.string_at(
            foundation.CFDataGetBytePtr(blob), foundation.CFDataGetLength(blob)
        ))
    finally:
        foundation.CFRelease(blob)
        foundation.CFRelease(windows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-root", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/shellboy"))
    parser.add_argument("--thread", default="visual-conversation")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--text-only", action="store_true",
                        help="Record the real terminal cells and metadata without a macOS window image")
    parser.add_argument("--image-backend", choices=("screencapture", "screen-capture-kit"),
                        default="screencapture",
                        help="screen-capture-kit needs macOS 14+ and the Swift command line tools")
    parser.add_argument("--keep-view", action="store_true",
                        help="Keep the fixture's saved source anchor instead of starting at the tail")
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--color-scheme", help="Optional bundled WezTerm color scheme for theme review")
    parser.add_argument("--no-color", action="store_true", help="Preserve NO_COLOR=1 in the isolated review process")
    parser.add_argument("--command", action="append", default=[])
    parser.add_argument("--command-wait-for", action="append", default=[],
                        help="Wait for this terminal text after the corresponding --command; save its cells before the next command")
    parser.add_argument("--keys", action="append", default=[],
                        help="Literal terminal keys after commands, without adding Enter")
    parser.add_argument("--draft", default="")
    args = parser.parse_args()
    if args.command_wait_for and len(args.command_wait_for) != len(args.command):
        parser.error("Provide one --command-wait-for for each --command")
    if sys.platform != "darwin":
        parser.error("This driver captures macOS windows; it is not a product OS requirement")
    root = args.fixture_root.resolve()
    if not (root / ".shellboy-visual-fixture").is_file():
        parser.error("Refusing an unmarked data directory; create it with visual_fixture")
    from fixture_safety import require_osc52, reject_gui_clipboard_input
    require_osc52(root / "config.toml")
    reject_gui_clipboard_input(args.command, args.keys, args.draft)
    config = tomllib.loads((root / "config.toml").read_text())
    if any(not v.get("base_url", "").startswith("http://127.0.0.1:")
           or v.get("api_key_env") for v in config.get("connections", {}).values()):
        parser.error("Only local, unauthenticated visual fixture connections are allowed")
    if not args.thread.startswith("visual-"):
        parser.error("Only synthetic visual-* threads are allowed")
    if args.cols < 35 or args.rows < 12:
        parser.error("Use at least 35 columns and 12 rows")
    binary = args.binary.resolve(strict=True)
    with binary.open("rb") as binary_file:
        binary_digest = hashlib.file_digest(binary_file, "sha256").hexdigest()
    output = args.out.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    wez = "/Applications/WezTerm.app/Contents/MacOS/wezterm"
    if not Path(wez).is_file():
        parser.error("WezTerm is not installed at its usual macOS location")
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("SHELLBOY_", "WEZTERM_"))
           and k not in ("OPENAI_API_KEY", "OPENROUTER_API_KEY", "NO_COLOR")}
    if args.no_color:
        env["NO_COLOR"] = "1"
    name = f"shellboy-visual-{os.getpid()}"

    def cli(*words, check=True):
        return subprocess.run(
            [wez, "cli", "--no-auto-start", "--class", name, *words], env=env,
            capture_output=True, text=True, timeout=15, check=check,
        )

    existing_windows = set() if args.text_only else {w["kCGWindowNumber"] for w in window_list()}
    pane = None
    with tempfile.TemporaryDirectory(prefix="shellboy-visual-terminal-") as scratch:
        terminal_config = Path(scratch) / "wezterm.lua"
        terminal_config.write_text(
            "return {font_size=12.0, enable_tab_bar=false, "
            f"initial_cols={args.cols}, initial_rows={args.rows}, "
            "window_background_opacity=1.0, check_for_updates=false"
            + (f", color_scheme={json.dumps(args.color_scheme, ensure_ascii=False)}" if args.color_scheme else "")
            + "}"
        )
        gui = subprocess.Popen(
            [wez, "--config-file", str(terminal_config), "start", "--always-new-process",
             "--class", name, "--position", "main:60,60", "--cwd", str(root),
             str(binary), "--config", str(root / "config.toml"),
             "--data-dir", str(root / "data")],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        # macOS does not reliably use --class for CLI discovery. Address this
        # exact GUI's socket; never fall back to the user's default mux.
        env["WEZTERM_UNIX_SOCKET"] = str(
            Path.home() / ".local/share/wezterm" / f"gui-sock-{gui.pid}"
        )
        try:
            deadline = time.monotonic() + 30
            while pane is None:
                result = cli("list", "--format", "json", check=False)
                if result.returncode == 0:
                    panes = json.loads(result.stdout)
                    owned = [p for p in panes if isinstance(p.get("cwd"), str)
                             and Path(unquote(urlparse(p["cwd"]).path)).resolve() == root]
                    if len(owned) == 1:
                        pane = owned[0]["pane_id"]
                        pane_size = owned[0]["size"]
                        break
                if gui.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("Dedicated visual terminal did not become available")
                time.sleep(.1)

            def keys(text):
                cli("send-text", "--pane-id", str(pane), "--no-paste", text)

            def settled_text():
                previous = None
                stable = 0
                deadline = time.monotonic() + 20
                while stable < 3:
                    time.sleep(.1)
                    text = cli("get-text", "--pane-id", str(pane)).stdout
                    pending = any(label in text for label in ("読込み中", "モデル一覧を取得中", "設定を反映中"))
                    stable = stable + 1 if text == previous and not pending else 0
                    previous = text
                    if time.monotonic() >= deadline:
                        raise RuntimeError("Terminal cells did not settle after input")
                return text

            deadline = time.monotonic() + 20
            # The header presents the selected Thread, without a fixed brand label.
            while "F1" not in cli("get-text", "--pane-id", str(pane)).stdout:
                if time.monotonic() >= deadline:
                    raise RuntimeError("Shellboy did not draw its initial screen")
                time.sleep(.1)
            keys(f"/switch {args.thread}\r")
            # A delayed source load is asynchronous; observe actual loaded text.
            deadline = time.monotonic() + 20
            while True:
                text = cli("get-text", "--pane-id", str(pane)).stdout
                if "New chat" not in text and "読込み中" not in text:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("Fixture history did not finish loading")
                time.sleep(.1)
            # Previous review runs persist their real view_state. Start each
            # case at the latest history; --keys can then move to a source anchor.
            if not args.keep_view:
                keys("\x1b[1;5F")
            time.sleep(.5)
            # Saved anchors can trigger a second bounded body fetch after the
            # title first appears. Do not send test input during that initial
            # load; these events intentionally have no scroll request to replay.
            settled_text()
            for index, command in enumerate(args.command):
                keys(command + "\r")
                if args.command_wait_for:
                    deadline = time.monotonic() + 20
                    while True:
                        cells = cli("get-text", "--pane-id", str(pane)).stdout
                        if args.command_wait_for[index] in cells:
                            output.with_suffix(f".step-{index + 1}.txt").write_text(cells)
                            break
                        if time.monotonic() >= deadline:
                            raise RuntimeError(f"Command {index + 1} did not display its expected terminal text")
                        time.sleep(.1)
                else:
                    time.sleep(.25)
            for sequence in args.keys:
                keys(sequence)
                time.sleep(.4)
            if args.draft:
                cli("send-text", "--pane-id", str(pane), args.draft)
            # Wheel bursts can cross asynchronous history windows. Capture the
            # settled terminal cells, not an intermediate loading frame.
            text = settled_text()
            output.with_suffix(".txt").write_text(text)
            if args.text_only:
                output.with_suffix(".json").write_text(json.dumps({
                    "binary": str(binary), "binary_sha256": binary_digest,
                    "thread": args.thread, "terminal": pane_size,
                    "fixture": "synthetic-history-v1", "provider": "loopback-only offline fixture",
                    "capture_kind": "terminal-cells", "commands": args.command, "command_wait_for": args.command_wait_for,
                    "input_keys": args.keys, "color_scheme": args.color_scheme, "no_color": args.no_color, "start_at_tail": not args.keep_view,
                }, ensure_ascii=False, indent=2) + "\n")
                print(json.dumps({"text": str(output.with_suffix('.txt')),
                                  "capture_kind": "terminal-cells"}), flush=True)
                return
            candidates = [w for w in window_list()
                          if w["kCGWindowNumber"] not in existing_windows
                          and w.get("kCGWindowOwnerPID") == gui.pid
                          and w.get("kCGWindowLayer") == 0
                          and w.get("kCGWindowIsOnscreen", False)
                          and w.get("kCGWindowAlpha", 0) > 0
                          and w.get("kCGWindowBounds", {}).get("Height", 0) > 100]
            # Cocoa also creates invisible helper windows. Match the terminal's
            # pixel geometry, then choose the closest decorated window.
            scale = pane_size.get("dpi", 72) / 72
            candidates.sort(key=lambda w: (
                abs(w["kCGWindowBounds"]["Width"] - pane_size["pixel_width"] / scale)
                + abs(w["kCGWindowBounds"]["Height"] - pane_size["pixel_height"] / scale)
            ))
            if candidates:
                candidates = candidates[:1]
            if len(candidates) != 1:
                diagnostic = [{"id": w.get("kCGWindowNumber"),
                               "pid": w.get("kCGWindowOwnerPID"),
                               "owner": w.get("kCGWindowOwnerName"),
                               "bounds": w.get("kCGWindowBounds")}
                              for w in window_list()
                              if w["kCGWindowNumber"] not in existing_windows]
                raise RuntimeError(f"Cannot identify GUI {gui.pid} capture window: {diagnostic}")
            window = candidates[0]
            print(json.dumps({"capturing": str(output), "window": window["kCGWindowNumber"],
                              "terminal": {"cols": args.cols, "rows": args.rows}}), flush=True)
            capture_command = (
                ["swift", str(Path(__file__).with_name("capture-window.swift")),
                 str(window["kCGWindowNumber"]), str(output)]
                if args.image_backend == "screen-capture-kit" else
                ["/usr/sbin/screencapture", "-x", "-o", "-T", "0", "-l",
                 str(window["kCGWindowNumber"]), str(output)]
            )
            subprocess.run(capture_command, check=True, timeout=120)
            if not output.is_file():
                raise RuntimeError("Screen capture produced no image")
            output.with_suffix(".json").write_text(json.dumps({
                "binary": str(binary), "binary_sha256": binary_digest,
                "thread": args.thread, "terminal": pane_size,
                "fixture": "synthetic-history-v1", "provider": "loopback-only offline fixture",
                "window": window["kCGWindowNumber"],
                "capture_kind": "window-image", "image_backend": args.image_backend,
                "commands": args.command, "command_wait_for": args.command_wait_for, "input_keys": args.keys, "color_scheme": args.color_scheme, "no_color": args.no_color,
            }, ensure_ascii=False, indent=2) + "\n")
            print(json.dumps({"captured": str(output), "bytes": output.stat().st_size,
                              "text": str(output.with_suffix('.txt'))}), flush=True)
        finally:
            if pane is not None:
                # Escape overlays, then request a normal save/stop shutdown.
                try:
                    cli("send-text", "--pane-id", str(pane), "--no-paste", "\x1b", check=False)
                    cli("send-text", "--pane-id", str(pane), "--no-paste", "\x11", check=False)
                except (subprocess.TimeoutExpired, OSError):
                    pass  # Still reclaim this exact dedicated GUI below.
            try:
                gui.wait(timeout=15)
            except subprocess.TimeoutExpired:
                # Only this dedicated GUI process is ours. No kill-pane command
                # is sent to the user's mux or to any preexisting terminal.
                gui.terminate()
                gui.wait(timeout=10)


if __name__ == "__main__":
    main()
