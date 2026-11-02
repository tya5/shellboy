# Resource Observations (#116)

Stage 1 (`footprint.py`, below) measures Shellboy alone. Stage 2
(`compare.py`, [Cross-Agent Comparison](#cross-agent-comparison-stage-2)) runs
Shellboy, Claude Code, Codex CLI, opencode and jcode on one runner against the
same fake model. Both are standard-library Python, use a deterministic loopback
model and never a provider account; there is no real-provider option. Stage 3
is published in the [Resource usage guide](../../docs/guides/resource-usage.md),
with source artifacts, observed numbers, differences and reproduction. The
measurement runners do not change Rust/Core behavior.

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

## Request bytes and storage (Stage 4)

The cross-agent run records the HTTP body length in bytes for each received POST,
without retaining request bodies. It reports total, first and last request bytes
per conversation only when every request can be attributed: the Shellboy fixture
uses its Thread identity; opencode uses its session ID header and adapter session
IDs; the other adapters use an unambiguous workspace path in the request, or the
sole session when N=1. Unattributed requests and incomplete
conversation values remain explicit, not estimates or zeroes. Auxiliary requests
and token-count requests are included when present. The fake usage is not a token
measurement; no tokenizer estimate is supplied.

Immediately after round five, before process shutdown, the run walks temporary
HOME and agent-specific data/runtime roots (Shellboy's data directory includes its
DB and WAL). It reports logical file bytes and file count; hard links are counted
once and symlinks are counted without following their targets. Workspace files and
installed executables are outside these roots. An unreadable or missing root is a
measurement error. Live background writes mean this is not an atomic snapshot.
These observations add no new PSS or startup method and do not establish real-API
usage or run-to-run variance. Results from separate workflow runs cannot be paired.

## Cross-Agent Comparison (Stage 2)

Decided on #116 (2026-11-01): compare Shellboy with jcode, Codex CLI, Claude
Code and opencode on **one CI Linux runner**, all pointed at the **same local
fake model**; no real provider, no cost, no thresholds. The numbers describe
these exact versions on this fake workload only, never real-model performance.

### Reproduce

On Linux x86_64 with Python 3.11+, Node/npm (CI pins Node 22.23.4) and the
Shellboy release binary from the stage-1 build:

```sh
python3 scripts/bench/install_agents.py /tmp/bench-agents      # pinned, no login
mapfile -t agents < /tmp/bench-agents/agents.args
python3 scripts/bench/compare.py --agent shellboy=target/release/shellboy "${agents[@]}" \
  --provenance /tmp/bench-agents/provenance.json --out /tmp/compare.json
```

Or dispatch `ci.yml` with `footprint=true` (`gh workflow run ci.yml --ref BRANCH -f
footprint=true`): the `compare` job uploads
`agent-comparison-linux-<SHA>/result.json` next to the stage-1 artifact.
`--sessions` and `--startup-runs` (default 1 5 10 20 and 5) shorten a run;
`--smoke` checks the protocols on any OS without PSS (not a measurement; jcode
is refused off Linux because its first macOS TUI start registered a launchd
hotkey agent outside the temporary HOME).

`install_agents.py` installs into a new directory with a private npm prefix,
cache and HOME, verifies the jcode tarball against its release `SHA256SUMS`,
and writes `provenance.json` (method, source, npm tarball/integrity, executable
SHA-256). Pinned versions:

| Agent | Version | Installed from | Executable measured |
|---|---|---|---|
| Shellboy | the checked-out commit | `cargo build --locked --release` | `target/release/shellboy` |
| Claude Code | 2.1.316 | npm `@anthropic-ai/claude-code` | the package's native `claude` |
| Codex CLI | 0.166.0 | npm `@openai/codex` | native `codex` from `@openai/codex-linux-x64` (the npm `codex` Node launcher is skipped) |
| opencode | 1.19.5 | npm `opencode-ai` | the package's native `opencode` |
| jcode | v0.99.1 | GitHub release `jcode-linux-x86_64.tar.gz` | the release binary |

### Fake Model

`fake_model.py` serves the stage-1 workload over three wires: OpenAI Responses
SSE (`/v1/responses`: Shellboy's stage-1 runner keeps its own equivalent server,
Codex), Chat Completions SSE (`/v1/chat/completions`: opencode, jcode) and
Anthropic Messages SSE (`/v1/messages`: Claude Code). Event shapes follow the
synthetic fixtures under `tests/fixtures/{responses,chat,messages}`. The round
is the last user message containing one of the five fixed instructions; rounds
1-4 return one call to the agent's own shell tool (`Bash`, `bash`, `shell`,
`shell_command` or `exec_command`, arguments shaped by the tool's schema) and,
after a tool result, the fixed answer; round 5 only answers. Requests without
a shell tool or fixed instruction (titles, summaries) are **auxiliary** and get
`ok`. Tool calls are capped at one per session per round (a further request is
answered instead and counted as `tool_budget_exhausted`), and all requests at
`40 N + 200`. Usage is reported as 1 token; tokens are not measured here.

A case passes only when every session returned each round's fixed answer to
its client, `hello.txt` contains `hello world` in every workspace (the tool
really ran), and the server saw exactly N tool calls and N answers per round
with no errors. Otherwise the agent's entry is `status: error` with the reason,
and other agents still run.

### Shapes, Settings And What Each Agent Did

Each agent uses its own documented headless shape (the same shapes as jcode's
`scripts/bench_headless_memory.py`); PSS is the sum over every root and its
descendants (stage-1 rules), and `processes` lists each `comm`.

| Agent | Sessions | Pointed at the fake model by | Non-default settings |
|---|---|---|---|
| Shellboy | one `serve` owner, N threads (stage-1 runner) | `config.toml` connection `base_url` | `response_max_retries=0`, OSC52 copy, no bell |
| Claude Code | N `claude -p --input-format stream-json --output-format stream-json --verbose` | `ANTHROPIC_BASE_URL`, dummy `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`/`ANTHROPIC_SMALL_FAST_MODEL` | `--permission-mode bypassPermissions`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, `DISABLE_AUTOUPDATER=1` |
| Codex CLI | N `codex app-server` (JSON-RPC), one thread each | `CODEX_HOME/config.toml` provider `bench` (`wire_api="responses"`, dummy `env_key`) | `approval_policy="never"`, `sandbox_mode="danger-full-access"`, retries 0 |
| opencode | one `opencode serve`, N sessions (`?directory=` per workspace) | `opencode.json` provider via `@ai-sdk/openai-compatible` `baseURL`, dummy key | `permission` allow, `autoupdate=false`, `share=disabled`, models/LSP download off |
| jcode | one `jcode serve`, N sessions over its debug socket | `--provider openai-compatible`, `JCODE_OPENAI_COMPAT_API_BASE`, dummy `OPENAI_COMPAT_API_KEY` | `JCODE_MEMORY_ENABLED=0`, `JCODE_NO_TELEMETRY=1`, `--no-update --no-selfdev` |

Every agent gets a new temporary HOME/XDG directories and only `PATH`/locale
from the environment: no credentials, no login, nothing read from a real home.
The full settings are recorded in each agent's `config` in the JSON. Sandboxes
and approvals are off for every agent because the workload writes a file and
Shellboy has no sandbox; Codex's sandbox helpers therefore do not run.

Observed in CI (see each case's `model` and `processes` in the JSON): Claude
Code (`Bash`), Codex (`exec_command`) and jcode (`bash`) made exactly 9 requests
per session; opencode (`bash`) made 10, its extra one an auxiliary title
request. Shellboy and jcode run as one process, opencode as one process, Claude
Code as one process per session. Codex sometimes still had a few descendant
processes alive at a sample (seen at N=1 and N=20 in different runs); they are
counted. GitHub assigns different CPU models to different runs (AMD EPYC 7763
and 9V45 were both seen), so compare agents within one artifact only. Process-per-session
agents get no readiness signal before their first turn, so the four external-agent
adapters idle 3 s before the baseline sample (as jcode's script does). Shellboy
reuses stage 1's ready-owner/created-Threads baseline without that fixed delay.

### Startup

For each agent `--startup-runs` interactive launches (default 5) each use a new
temporary HOME and a 100x36 PTY with kernel echo **off**, so only text the agent
renders counts. Common terminal queries (cursor position, device attributes,
colours, kitty keyboard, size) are answered. `first_visible_ms` is spawn to the
first three alphanumeric characters after removing escape sequences;
`input_echo_ms` is spawn to the visible echo of a typed probe (`zqxjvk`),
typed at first visible text and retyped every 50 ms until it is seen
(`probe_writes`), so agents that discard input typed during startup (Shellboy
does, by design, until "入力できます") are resolved to about 50 ms. The echo is
found in the escape-stripped output stream, or, when an agent renders the
probe across two frames, in the repository's cell decoder (`echo_detector`;
the decoder is dropped for an agent whose sequences it does not model).
Nothing is submitted. First-run dialogs are pre-answered so
the probe reaches the composer: Claude Code's onboarding, dummy-key approval and
folder trust (`~/.claude.json`) and Codex's project trust. This generic method
differs from stage 1's Shellboy-specific completed-frame/paste method, so the
two Shellboy startup numbers are not interchangeable.

### Caveats

- A fake model answers instantly with fixed text; real-model latency, token
  use, context growth and reasoning are absent. Differences reflect each
  agent's process model, runtime and history handling under this workload only.
- Workload: the stage-1 instructions (list, **create file**, grep, **count
  lines**, done), not jcode's script (list, read README title, grep,
  summarize, done); jcode's numbers come from real models and its own homes.
- Shapes differ by design: one server for N sessions (Shellboy, opencode,
  jcode) versus one process per session (Claude Code, Codex). That is part of
  what is compared, not a fixable bias.
- Disabled features (jcode memory, permission/sandbox systems, telemetry) may
  change memory relative to a default install. opencode loads its provider
  package at runtime; any network fetch it makes is part of its startup.
- One runner, sequential agents, shared CI hardware; repeat runs to see
  variance. PSS is sampled after rounds, not a peak. Agents that keep
  short-lived helpers get up to ten snapshots (`snapshot_attempts`) instead of
  stage 1's three; persistent process churn is still an error.

### Codex shared-server comparison (#151)

`--agent codex-shared=/path/to/codex` adds a second Codex process topology:
one `app-server` hosting N distinct Threads, each with its own fixture working
directory. One reader demultiplexes JSON-RPC responses by request ID and turn
notifications by Thread ID; all N turns are issued concurrently. The process
is sampled once with all descendants. It uses the same binary and configuration
as the existing `codex` adapter, which still uses one app-server per session.
The comparison workflow passes both variants on the same runner. This does not
establish equivalent products or represent real-provider tokens; server-side
features, per-session isolation and memory overhead may differ by topology.
Unexpected server-initiated requests or mismatched turn IDs fail the run.

### Repeated and in-flight observations (#151)

`compare.py --repeats 3` creates three independent fresh cases for every N.
Each case keeps its individual observations; `repeat_summary` reports the
minimum, median and maximum of baseline PSS and observed in-flight maxima.
The latter is the maximum of samples taken approximately every 100 ms during
round execution, not a guaranteed true peak. Sampling itself adds overhead;
process churn failures and sampling intervals are recorded, and unavailable
values are not converted to zero. Previously published after-round samples
remain after-round snapshots, not these observed maxima.

For Shellboy, `baseline_one_tui` is a separate fresh PTY case in which the TUI
itself hosts the owner and N threads. A second owner cannot attach to `serve`.
It is not an in-flight measurement; the ordinary in-flight result remains a
separate owner-only case without a TUI. Repetitions use the same fake-model
workload, with no real provider or token usage claims.
