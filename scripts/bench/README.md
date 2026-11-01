# Shellboy Resource Observations (#116, Stage 1)

This standard-library-only benchmark measures Shellboy, not other agents. It
uses a deterministic loopback Responses SSE fixture, never a provider account.
There is no real-provider option. Stages 2 (cross-agent comparison) and 3 (a
published comparison page) are not implemented. No Rust/Core change is needed.

## Reproduce On Linux

Build with the repository's pinned toolchain and locked dependencies, then:

```sh
cargo build --locked --release --bin shellboy
python3 scripts/bench/footprint.py --binary target/release/shellboy --out /tmp/shellboy-footprint.json
# A smaller observation:
python3 scripts/bench/footprint.py --binary target/release/shellboy --sessions 1 5 --out /tmp/shellboy-small.json
```

Requires Python 3.11+, Linux `/proc/<pid>/smaps_rollup`, and ordinary Unix tools
(`find`, `sort`, `printf`, `grep`, `wc`). No external Python packages are needed.
The output path must not exist. Config, data, HOME, workspaces and model endpoint
are temporary. Only basic locale/PATH environment variables are inherited: no
credentials, proxy settings or Shellboy caller identity. Clipboard output is
OSC52 drained by the PTY, never the OS clipboard. No user data directory is read.

Each N (default 1, 5, 10, 20) starts a **fresh `shellboy serve` owner**. After
readiness, N conversations receive separate identical tiny workspaces. Each
round submits the same instruction to all N conversations using a start barrier,
waits for every Run, checks completed Turns and answers, then records PSS. It is
concurrent submission, not a guarantee that the OS schedules all requests at the
same instant. Conversations persist across all five rounds; owners do not persist
between N values. The fake model issues four shell calls and five fixed answers
per conversation (nine requests total), with server-side caps and retries off.

## Fixed Workload And Provenance

Issue #116 explicitly requests: list files, create a file, grep `fn main`, count
lines, and `done`. `footprint.py` records the exact five English instructions and
contains the four fixed commands. The workspaces contain `Cargo.toml`, a two-line
`README.md`, and `src/main.rs`; round 2 creates `hello.txt`. The model follows
fixed instructions, not actual reasoning or model-dependent tool selection.

The named upstream reference is
[`jcode/scripts/bench_headless_memory.py`](https://github.com/1jehuang/jcode/blob/12a0dd30e4ad9d6351ca83bb7795d9f8aae9d522/scripts/bench_headless_memory.py).
Inspection of that immutable revision and the initial revision
`0da8f4006b95d6650209db0bf4a589ebf5f2073e` finds **different** round 2/4
instructions (read README title / summarize, rather than create file / count
lines). This stage follows the explicit issue workload, retaining upstream's
listing/search/done wording. It does **not** claim byte-identical upstream prompts
or comparable cross-agent numbers. Any later comparison must agree one exact
workload first. Other agents and paid providers require separate authorization.

## What The Numbers Mean

JSON schema version 1 includes UTC start time, CPU/OS/RAM information, executable
SHA-256, CI source SHA/run when present, N, exact instructions, and per-round
observations. Memory is **KiB** (Linux smaps `kB`, 1024 bytes), not decimal MB.

PSS is summed for the owner and its transitive descendants using Linux parent
PIDs, including live shell descendants if any remain at the observation. Each
sample enumerates the tree, reads every `Pss:` (not RSS or `Pss_Dirty`), and
rechecks PID/start-time membership. Churn retries at most three times; missing
PSS, permission failure or persistent churn is an error, never zero/partial
success. Sampling is sequential, not an atomic kernel-wide snapshot or peak
memory measurement. Idle after-round shells generally have already exited.
The Python driver/model server and sibling CLI clients are **not** owner
processes and are excluded. The baseline is after creating the N conversations.
The `processes` array makes exactly what was summed inspectable.

Startup is a separate **cold-data-dir TUI**, not `serve` readiness. A 100x36
PTY is drained into the repository's VT frame parser. A monotonic timer starts
immediately before spawning the executable. `first_screen_ms` is the first
nonempty completed frame. Once the UI says it accepts input, a unique bracketed
paste probe is sent; `input_echo_ms` ends at its visible completed-frame echo.
This includes scheduling, frame parsing, paste handling and repaint, so is an
**observed input round-trip upper bound**, not an internal ready timestamp,
physical-display latency or a provider response. No instruction is submitted.
One sample is taken per invocation; repeat invocations to characterize variance.

There are **no memory/time thresholds**, rankings or winner claims. Shared CI
hardware, allocator state, build profile and feature differences affect results.
A mock workload is not real provider performance. Earlier macOS footprint data
uses a different metric and must not be directly compared to these PSS values.

## Manual CI

In Actions, select **Rust checks**, Run workflow, choose the desired ref and enable
`footprint`. Or:

```sh
gh workflow run ci.yml --ref YOUR_BRANCH -f footprint=true
```

The opt-in `footprint` job uses Linux, builds a release binary, and uploads
`shellboy-footprint-linux-<source SHA>/result.json`, including on measurement
failure. It is not run on normal main pushes and is not a performance acceptance
gate. Numbers have no pass/fail criteria; instrumentation/cleanup failures still
fail the optional job and set JSON `status: error`, so broken observations cannot
masquerade as valid samples. Standard correctness checks remain separate jobs.

Portable unit checks (including synthetic `/proc` fixtures):

```sh
python3 -m unittest discover -s scripts/tests -p test_footprint.py -v
```

At N=20, message acceptance remains concurrent but completion observers run
sequentially so long-poll `wait` clients do not occupy all bounded IPC slots.
A known admission rejection is retried at most 30 times with the **same**
operation ID, only when the owner explicitly says it was not accepted/applied.
Uncertain transport/mutation failures are not reissued. Observation/answer and
shell-exit checks precede each PSS sample and are identical across N values.
