# Resource usage

[Guide index](README.md) - [Configuration](configuration.md) - [Search and indexing](rag.md)

These are observations of a **fake-model workload**, not product rankings or
predictions for a real model. They describe sampled process memory and observed
terminal startup, not response quality, capability, token costs or peak memory.
Only compare agents **within the same comparison run and artifact**. Even two
jobs of one workflow are separate runners; do not combine their measurements.

## Sources and runners

Results below are from [workflow run 41307629104](https://github.com/tya5/shellboy/actions/runs/41307629104),
2026-11-01 UTC, measured source commit
`5a2e8474a52928eaafd35d64c3eb29dfd6c34626`. The later comparison-branch revision
`4d6f128a` corrects documentation, not these measurements. Shellboy reports
`0.5.0`; the release executable SHA-256 in both artifacts is
`e541719c4b91eaa7b840ca56c8e9a61eee27bb2d41522f4bde4dd04bc7726a74`.

| Runner key | Job and artifact | Environment recorded by the measurement |
| --- | --- | --- |
| F | `footprint`, [Shellboy-only artifact 13084027318](https://github.com/tya5/shellboy/actions/runs/41307629104/artifacts/13084027318) | GitHub `ubuntu-latest`, Ubuntu 24.04.5, Linux x86_64, AMD EPYC 7763, 4 CPUs, 16367512 KiB RAM; started 12:45:32 UTC |
| C | `compare`, [comparison artifact 13084610570](https://github.com/tya5/shellboy/actions/runs/41307629104/artifacts/13084610570) | GitHub `ubuntu-latest`, runner `GitHub Actions 1000181815`, image `ubuntu24` / `20261026.354.1`, Ubuntu 24.04.5, Linux x86_64, AMD EPYC 7763, 4 CPUs, 16367520 KiB RAM; 12:45:52-12:49:19 UTC |

Each archive contains `result.json`, including machine details, source SHA,
settings and process samples. C also records package/release installation
provenance, executable hashes, requested models and shell tools. CPU models can
differ between workflow runs; the shared CPU name here does not make F and C a
paired comparison. Artifact retention is finite: retain downloaded JSON and its
source identifiers when citing a measurement.

## Workload and memory method

N is 1, 5, 10 or 20 conversations/sessions. Each N starts fresh processes and
workspaces; sessions persist for five rounds: list files, create `hello.txt`,
search for `fn main`, count README lines, then answer `done`. There are four
shell calls and five fixed answers per session. The loopback server supplies
deterministic replies, not model reasoning. Different protocol adapters use
Responses, Chat Completions or Anthropic Messages SSE. This is **not** the
upstream jcode script's read-title/summarize workload or its real-model results.

Memory is Linux **PSS in KiB** (1024 bytes): sum `Pss:` from
`/proc/<pid>/smaps_rollup` for all agent roots and their transitive descendants.
Shared pages are proportionally attributed by the kernel. The driver, fake
model server and sibling CLI clients are excluded. Samples are after all N
sessions finish each round, not peaks, per-session increments, RSS, or an atomic
snapshot of the machine. Process membership and PID start times are checked;
missing data or persistent churn is an error, never a zero-memory result.

### Stage 1: Shellboy alone

One `shellboy serve` owner hosts N Threads. Baseline is after creating Threads;
each round is submitted concurrently and observed complete before sampling.
The table gives all five samples, not their maximum. Do not compare F with C.

| Agent/version | Runner / date (UTC) | N | Baseline KiB | Round 1 | Round 2 | Round 3 | Round 4 | Round 5 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Shellboy 0.5.0 (`5a2e8474`) | F / 2026-11-01 | 1 | 20237 | 25488 | 25752 | 25884 | 26068 | 26132 |
| Shellboy 0.5.0 (`5a2e8474`) | F / 2026-11-01 | 5 | 20345 | 25924 | 26612 | 27060 | 30120 | 30362 |
| Shellboy 0.5.0 (`5a2e8474`) | F / 2026-11-01 | 10 | 20398 | 32160 | 33088 | 33928 | 34672 | 35004 |
| Shellboy 0.5.0 (`5a2e8474`) | F / 2026-11-01 | 20 | 20531 | 27566 | 30374 | 31326 | 32190 | 32538 |

A separate fresh-data TUI launch on F, 100x36 PTY, observed the first nonempty
completed frame at **4.465311 ms** and pasted-composer probe echo at
**64.438170 ms** after spawn. This is one observation. The probe is not sent;
the echo is an observable roundtrip bound, not an internal ready timestamp.

### Stage 2: same-run agent comparison

C runs agents sequentially on one runner, using the same five instructions and
fresh temporary HOME/XDG/workspaces. The four external-agent adapters idle three
seconds before baseline; Shellboy reuses stage 1's ready-owner/created-Threads
baseline without that fixed delay. These are **total PSS after round 5**, not baseline-subtracted or peak
values. Baselines and rounds 1-4 remain available in the comparison JSON.

| Agent/version | Runner / date (UTC) | N=1 KiB | N=5 KiB | N=10 KiB | N=20 KiB |
| --- | --- | ---: | ---: | ---: | ---: |
| Shellboy 0.5.0 (`5a2e8474`) | C / 2026-11-01 | 26127 | 27518 | 29389 | 32103 |
| Claude Code 2.1.316 | C / 2026-11-01 | 197603 | 635830 | 1144765 | 2124372 |
| Codex CLI 0.166.0 | C / 2026-11-01 | 183771 | 301773 | 467884 | 813106 |
| opencode 1.19.5 | C / 2026-11-01 | 691056 | 1023844 | 1255949 | 1599639 |
| jcode v0.99.1 (`7206efd0c`) | C / 2026-11-01 | 23912 | 32994 | 41653 | 59166 |

Every agent completed the checked workload. Claude Code, Codex and jcode made
nine requests per session; opencode made ten, including an auxiliary title
request. Shellboy made nine. These counts reflect fixture traffic, not token
consumption; the server reports synthetic usage. All workspaces contained the
expected file contents and every round returned its fixed answer.

### Startup in the comparison

C makes five separate interactive launches per agent in a 100x36 PTY with
kernel echo off. First visible means the first three alphanumeric characters
in escape-stripped output, **not a completed frame**. Input echo means the
agent-rendered probe `zqxjvk`; it is first typed at visible text and retyped
every 50 ms until observed, never submitted. Onboarding/trust dialogs are
pre-answered. Values are **median [minimum, maximum] milliseconds**.

| Agent/version | Runner / date (UTC) | First visible ms | Input echo ms |
| --- | --- | ---: | ---: |
| Shellboy 0.5.0 (`5a2e8474`) | C / 2026-11-01 | 3.477 [3.333, 3.923] | 64.974 [64.536, 65.168] |
| Claude Code 2.1.316 | C / 2026-11-01 | 388.574 [381.151, 391.198] | 457.982 [446.961, 461.404] |
| Codex CLI 0.166.0 | C / 2026-11-01 | 21.397 [21.331, 34.204] | 44.897 [38.259, 54.310] |
| opencode 1.19.5 | C / 2026-11-01 | 3302.588 [3108.376, 3495.011] | 3364.709 [3143.499, 3565.000] |
| jcode v0.99.1 (`7206efd0c`) | C / 2026-11-01 | 11.454 [11.169, 13.206] | 32.342 [30.644, 35.709] |

A startup message such as jcode's `Starting server...` can satisfy first visible
before the composer appears. Input discarded during startup is resolved only
to about 50 ms by the repeated probe. Echo detection uses the output stream or
the cell decoder recorded in JSON. F's completed-frame/paste method and C's
generic typed-probe method are different: their Shellboy startup figures are
not interchangeable. Neither establishes physical-terminal or IME behavior.

## Process and feature differences

These settings matter when interpreting the numbers. They are **not an
ablation study** proving which feature caused a difference. The workload does
not establish that the products implement equivalent capabilities.

| Agent | Measured process shape | Exercised features and non-default settings |
| --- | --- | --- |
| Shellboy | One `serve` owner, N Threads | The model has only the shell tool; other operations go through its CLI. Embeddings use a configured external API, not a model loaded into the executable; no embedding/indexing is run here. Provider retries off; OSC52 clipboard output drained, bell off. |
| Claude Code | N native `claude -p` processes, stream-JSON input/output | The fake model calls `Bash`, not the rest of its tool surface. `bypassPermissions`; nonessential traffic and updater disabled; dummy credentials and preaccepted onboarding/trust. |
| Codex CLI | N native `codex app-server` processes, one Thread each | JSON-RPC adapter calls `exec_command`. Approval `never`, sandbox `danger-full-access`, retries off. Sandbox helper overhead is therefore absent; any other live descendants in a sample are included. The Node npm launcher is skipped. |
| opencode | One `opencode serve`, N sessions | Adapter calls `bash`; one auxiliary title request per session. Permissions allow, sharing/updater off, model/LSP downloads off. The provider SDK is loaded at runtime; any package fetch is included in startup. |
| jcode | One `jcode serve`, N sessions via debug socket | Adapter calls `bash`. Memory feature disabled (`JCODE_MEMORY_ENABLED=0`); telemetry off; update/self-development off. No claim is made for its default memory/embedding features. |

Process shapes are adapter choices, not claims that a product requires that
many processes or that all supported configurations were tested.

Approvals and sandboxes are disabled for this comparison, because the fixed
workload writes a file and Shellboy does not provide a sandbox. This removes
safety-system costs; it is not a recommended deployment configuration. Use a
trusted disposable Linux VM. Temporary HOME and dummy keys do **not** constitute
an OS security boundary. No real provider account, credential or paid model is
used; installing pinned packages needs network access. Other products' default
features, resident embedding models and tool surfaces are not normalized or
otherwise measured. Real-model latency, context growth, reasoning, long sessions,
default-install behavior and OS-wide resource use are outside these results.

## Reproduce

From a source checkout on Linux x86_64, in Bash, using the pinned Rust toolchain, Python 3.11+,
Node/npm (recorded run: Node 22.23.4, npm 10.9.9) and ordinary Unix tools.
Use a fresh checkout of the measured source SHA above to recreate that setup;
use your chosen current commit for a new observation and record that commit:

```bash
cargo build --locked --release --bin shellboy
work=$(mktemp -d)
python3 scripts/bench/footprint.py --binary target/release/shellboy --out "$work/footprint.json"
python3 scripts/bench/install_agents.py "$work/agents"
mapfile -t agents < "$work/agents/agents.args"  # Bash, Linux
python3 scripts/bench/compare.py --agent shellboy=target/release/shellboy "${agents[@]}" \
  --provenance "$work/agents/provenance.json" --out "$work/comparison.json"
```

The installer uses a private npm prefix/cache/HOME, pins the versions above,
checks the jcode release checksum, and records provenance; it does not log in.
Output paths must not exist. The scripts do not rebuild automatically. Use
`--sessions 1 5` for a smaller memory observation or `--startup-runs` to change
the comparison repetition count. A new observation is not expected to reproduce
these exact numbers. Compare agents only inside that new comparison JSON.

Alternatively, on a branch containing both runners:

```sh
gh workflow run ci.yml --ref YOUR_BRANCH -f footprint=true
gh run download RUN_ID --name agent-comparison-linux-SOURCE_SHA --dir comparison-run
gh run download RUN_ID --name shellboy-footprint-linux-SOURCE_SHA --dir footprint-run
```

Do not use a transport `--smoke` run as PSS evidence. jcode smoke/startup is
refused off Linux because a macOS first launch was observed to register a
launchd hotkey agent outside temporary HOME. See the source checkout's
[`scripts/bench/README.md`](../../scripts/bench/README.md) for exact settings,
snapshot retries and workload provenance. Inspect `status`, `smoke`, source
SHA, versions, `machine`, `config`, `processes` and request/answer counts before
using a JSON result; do not replace errors with zero or compare partial cases.
