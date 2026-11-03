#!/usr/bin/env python3
"""Run the opt-in writer-lane handler measurement against a verified copy."""
import os
import sqlite3
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DB = Path(os.environ.get("WRITER_AUDIT_DB", ""))
if not DB.is_file():
    raise SystemExit("WRITER_AUDIT_DB must name the verified disposable copy")
uri = f"file:{DB}?immutable=1&mode=ro"
with sqlite3.connect(uri, uri=True) as conn:
    schema = conn.execute("PRAGMA user_version").fetchone()[0]
    if schema != 25:
        raise SystemExit(f"audit copy schema mismatch: expected 25, found {schema}")
    check = conn.execute("PRAGMA quick_check").fetchone()[0]
    if check != "ok":
        raise SystemExit("database copy failed PRAGMA quick_check")
    thread = conn.execute(
        "SELECT p.thread_id FROM thread_path p JOIN receipt r "
        "ON r.target_thread_id=p.thread_id GROUP BY p.thread_id "
        "HAVING count(DISTINCT p.node_id) BETWEEN 100 AND 400 ORDER BY count(DISTINCT p.node_id) DESC LIMIT 1"
    ).fetchone()
    history = conn.execute(
        "SELECT thread_id FROM thread_path GROUP BY thread_id "
        "ORDER BY count(*) DESC LIMIT 1"
    ).fetchone()
if thread is None and os.environ.get("WRITER_AUDIT_TEST_FILTER", "writer_lane_audit_measurement") == "writer_lane_audit_measurement":
    raise SystemExit("database copy contains no receipt-backed 100-400 node thread history")

# Never print the selected identifier or any query result contents.
env = os.environ.copy()
env["WRITER_AUDIT_DB"] = str(DB)
env["WRITER_AUDIT_THREAD"] = (thread or history)[0]
process = subprocess.Popen(
    [
        str(Path.home() / "Workspace/junk/shellboy-sandbox/.local/bin/rust-slot"),
        "cargo",
        "test",
        "--lib",
        os.environ.get("WRITER_AUDIT_TEST_FILTER", "writer_lane_audit_measurement"),
        "--",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ],
    cwd=REPO,
    env={**env, "WRITER_AUDIT_TEST_FILTER": os.environ.get("WRITER_AUDIT_TEST_FILTER", "writer_lane_audit_measurement")},
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
    text=True,
    bufsize=1,
)
for line in process.stdout:
    if line.startswith(("rust-slot:", "Compiling", "Finished", "AUDIT_START", "AUDIT kind=", "AUDIT_SQL", "AUDIT_PLAN", "AUDIT_GROWTH_SCAN", "AUDIT_TIMES", "error:", "thread ", "test result:", "FAILED", "panicked")):
        print(line[:700], end="", flush=True)
if process.wait():
    raise SystemExit(process.returncode)
