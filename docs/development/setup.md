# 開発環境

製品の操作は [User Guide](../guides/user-guide.md)、接続は [設定ガイド](../guides/configuration.md)、開発上の契約は [適用仕様](../design/effective-spec.md) を参照してください。

## 必要なもの

Rustup、Git、OSのC/C++コンパイラとリンカーを用意します。bundled SQLiteとsqlite-vecのビルドに使います。macOSではCommand Line Tools、WindowsではMSVC版RustとVisual Studio Build ToolsのC++開発環境、LinuxではディストリビューションのC/C++開発環境を使用します。外部SQLiteや追加task runnerは不要です。

Rustは `rust-toolchain.toml` の1.99.1、rustfmt/clippyと依存解決は同ファイルおよび `Cargo.lock` で固定します。初回ビルドではツールチェーンとcrateの取得にネットワークを使います。

## ビルドと設定

リポジトリ直下で実行します。

```sh
rustup show
cargo build --locked
cargo run --locked --bin shellboy -- --help
cargo run --locked --bin shellboy -- --version
```

`config.example.toml` を自分の設定先へコピーし、Responses互換proxyのURLとモデルIDを指定します。`api_key_env` で指定した環境変数に資格情報を設定し、鍵をTOMLやGitへ保存しません。モデル・価格・context長は接続先依存で、例のURLやモデル名は利用可能性を保証しません。embeddingは任意で、有効化後の索引作成・検索には接続先の費用が生じえます。

```sh
cargo run --locked --bin shellboy -- --config /path/to/config.toml --data-dir /path/to/data
```

対話端末で起動します。初回会話は起動cwdを使い、既存会話のcwdは維持します。別端末の接続CLIは同じdata-dirの稼働中ownerへ接続し、未起動なら自動起動しません。

## 既定保存先

| OS | 設定 | データ |
|---|---|---|
| macOS | `~/Library/Application Support/shellboy/config.toml` | `~/Library/Application Support/shellboy/data/` |
| Linux | `$XDG_CONFIG_HOME/shellboy/config.toml`、未設定時 `~/.config/shellboy/config.toml` | `$XDG_DATA_HOME/shellboy/`、未設定時 `~/.local/share/shellboy/` |
| Windows | `%APPDATA%\shellboy\config.toml` | `%LOCALAPPDATA%\shellboy\data\` |

ownerのdata-dirはCLI引数 > `SHELLBOY_DATA_DIR` > TOML > OS既定の順です。接続CLIはTOMLを読みません。相対path等の詳細は [設定設計](../design/config.md) に従います。Shellboyは旧名の保存先・環境変数・実行名を互換として探索せず、旧データを自動移動しません。既存データの移動作業は今回不要です。

Unixでは保存先の末端にsymlinkを指定できません。自分が所有する実ディレクトリを `--data-dir` で指定してください。保存先は0700へ限定しますが、symlinkのリンク先をchmodしません。通信の起動が拒否された場合は表示された原因・次操作に従い、生存または停止を確認できない通信先を手動で削除しないでください。前回のShellboyが生存していれば通常終了し、一時領域の権限・空き容量や通信資源を確認して再起動します。これは稼働後の通信障害に対する受付済みoperation確認とは別の起動診断です。

## 標準チェック

macOS/Linuxでは `sh scripts/check.sh`、Windows PowerShellでは `./scripts/check.ps1` を使います。内容は以下です。

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

GitHub Actionsの定義はローカル成功とは別です。pushしていない間はリモートCIを実施済みとしません。改名後の実行結果は [検証状況](../validation/acceptance.md) に記録します。

開発用dev profileでは固定版 `encoding_rs` のみ `opt-level=2` とし、testも継承します。stateful decoderの非最適化処理が大量raw出力の機能試験を支配するのを避ける設定です。製品のdecoder・依存版・機能試験の期限は変えません。cache予算をプロセスRSS上限と混同しません。

通常試験は一時DBとloopback mockを使い、API資格情報を要求しません。ignoredの性能・実OS・実API試験は個別の明示実行であり、通常テスト成功に含めません。[端末fixture](../validation/manual-terminal.md) はAPIなしで操作を確認できます。実機がない項目は未実施と記録し、mockで代用合格にしません。

## One local build directory for all worktrees

Use the same Cargo output directory for ordinary checks and measurements across
local worktrees. Do not create a target directory per measurement or candidate.
From any worktree in this clone, set this before Cargo or measurement commands:

```sh
shellboy_git_common="$(git rev-parse --path-format=absolute --git-common-dir)"
shellboy_build_root="$(cd "$shellboy_git_common/.." && pwd -P)"
export CARGO_TARGET_DIR="$shellboy_build_root/target"
export CARGO_INCREMENTAL=0
```

In PowerShell, use the same repository-common location:

```powershell
$shellboyGitCommon = git rev-parse --path-format=absolute --git-common-dir
$env:CARGO_TARGET_DIR = Join-Path (Split-Path -Parent $shellboyGitCommon) 'target'
$env:CARGO_INCREMENTAL = '0'
```

For current #41 work, announce long Rust builds or clippy in the issue before
starting, so other sandbox workers can coordinate their serial build queue.
Run baseline and candidate builds sequentially. Preserve their source SHA,
configuration and measurement results; rebuild each source before running it
rather than assuming a shared executable still represents the previous checkout.
Keep logs/results outside Cargo output. Remove obsolete measurement executables
and retired target directories after all processes using them have exited.
Keep modified source worktrees and uncommitted measurement instrumentation.
A shared directory can still accumulate obsolete artifacts; inspect disk use
and clean only idle, reproducible build output when necessary.

## Integration layout and native timing (#46)

Python 3.11 or newer is used by the layout/timing scripts (standard library
only); it is not a product runtime dependency. Six timing-gate self-checks
cover lost counts, ignored identity, failure continuation, empty exact filters
and Unicode failure reporting under a strict-ASCII child console.
Standard checks first run
`python3 scripts/check-test-layout.py` (`python` on Windows). Register each new
ordinary `tests/*.rs` file as a module in `tests/integration/main.rs`; the two
self-spawning native suites remain explicit Cargo targets. The completeness
check rejects missing or duplicate registrations.

Run a complete profiled check with:

```sh
python3 scripts/test-timings.py --output .local/test-timings
cargo test --locked --test integration storage::
cargo test --locked --test integration attachment_provenance:: -- --nocapture
```

The timing script builds all targets and executes every discovered harness,
records exit codes and inventories, and fails on any test failure or count
mismatch. `--per-test storage,index,control_db` adds individual-process profiles;
these timings include process startup and do not claim concurrent test durations.
Use `--profile-only` to collect just inventories and the requested isolated
profiles. `--compare /path/to/baseline/results.json` requires same-OS identity
and ignored-set equality, allowing only the structural module prefix change.

Normal three-OS CI uploads timings and failing test names. Repeated overflow,
closed-proxy and attachment diagnostics are available through the Rust checks
workflow's manual `diagnostics` input; their underlying cases still run normally.
The Rust checks manual `performance` input profiles both the chosen baseline
and candidate on Windows with two test threads, plus a separate four-thread
contention comparison. Cache/runner conditions
and build versus execution times must accompany any claimed improvement.

For a four-thread Windows check without rerunning the baseline profiling matrix,
use the manual `windows_test_threads=4` input with `performance=false`. This
additional stress run selects Windows only; automatic pushes to `main` execute
the full three-OS matrix with four Windows test threads. A manual run on a task
branch checks Linux and macOS only (Windows is the slowest job, about 15 minutes);
`main`, `integration/*` batch branches, `all_oses=true` and
`windows_test_threads=2` run all three OSes. Nothing lands on `main` without an
exact-SHA three-OS pass, which the `reuse` job then accepts for the `main` push.
`windows_test_threads=2` provides a two-thread diagnosis. Experimental failures are retained and never
automatically retried into success.

## CI execution policy (#57)

Before pushing any proposed tree, finish the full local standard check with
`sh scripts/check.sh` or `./scripts/check.ps1`. Record its exact source and
result; focused checks do not replace this gate. Rust checks automatically run
only on pushes to `main`, not branch pushes or pull requests. For branch-level
native-OS concerns, explicitly dispatch Rust checks on that branch; the manual
inputs and matrices described above remain available.

If main CI is red, investigate and restore it before landing other main work.
Release tags/releases require successful ordinary three-OS checks on their exact
main SHA. An unrelated intermittent Windows failure can receive an explicit
same-SHA failed-job rerun for diagnosis, retaining both failure and rerun evidence;
there is no automatic retry or assumption that the initial failure was harmless.
See the [adopted CI policy](../design/ci-test-performance.md#ci-execution-and-publication-policy-57)
and [publication gates](publication.md#release-validation-gates-57).


## Batched integration and exact-SHA CI reuse (#92)

The integration owner does not build Rust locally on the memory-constrained
integration host. Authors retain the local standard-check gate. The integration
owner verifies approval, rebases each candidate on current main in an owned
worktree, returns conflicts to the author, and accumulates a batch on an
`integration/*` branch. Push that branch and explicitly dispatch ordinary Rust
checks (`windows_test_threads=default`, not the Windows-only experiment).
After the three native jobs pass on the first attempt, fetch again and
fast-forward main to **that same SHA**. If main moved, rebuild the batch and
repeat CI. A main push still creates a workflow run: its read-only Actions API
gate records the qualifying run URL in logs and summary, then skips expensive
native jobs. Missing evidence or an API error executes all checks. Manual
dispatch always executes checks; reuse-only or rerun successes never qualify as
fresh evidence. See the [design](../design/ci-test-performance.md#exact-sha-reuse-and-integration-batches-92).

Bisect failing batches by constituent branch, preserving the failed run URLs
and test names. Return the failing branch to its author and qualify the passing
subset separately. A rerun is diagnosis, not evidence that a failed batch passed.
Comment each issue with its integrated SHA and passing CI run; the reviewer
closes issues and authors clean up their branches/worktrees.

Windows retains its original single all-target job with four test threads.
The experimental split is not adopted because its extra build/link work gave
little wall-time benefit. Exposed test instability is tracked in #95. The reuse
logic has lightweight Node built-in tests, executed by Linux CI with
`node --test scripts/tests/ci-reuse.test.cjs`; Node is not a product dependency.
Measurement and release jobs are unchanged.

## Small developer tools

`python3 scripts/dev/record-delta.py [--base origin/main] [--repo WORKTREE]`
prints one JSON line with base/current line counts, deltas, Git added/deleted
counts and `only_additions` for status, acceptance and decisions. It reads the
working tree (staged and unstaged), does not fetch or edit files, and exits1 if
any baseline lines are deleted/replaced; missing files or an invalid base exit2.
`only_additions` permits insertions anywhere, not only EOF; newline-only changes
follow Git diff rules. Use `--help` for the JSON/error and exit conventions.

## Disposable PTY capture

`python3 scripts/dev/pty-capture.py --binary target/debug/shellboy --cols 80 --rows 36`
uses an existing trusted binary (no build), a fresh temporary data/config/HOME,
and fixed OSC52 copying. Output is one JSON line of completed screens; add
`--out NEW.json` for a new result file and a short stdout summary. Example:

```sh
python3 scripts/dev/pty-capture.py --binary target/debug/shellboy \
  --steps '[{"command":"/help","until":"Esc / Enter / F1","label":"help"}]' \
  --out /tmp/new-help-capture.json
python3 scripts/dev/pty-capture.py --replay capture.raw --cols 80 --rows 36 --until CODE_END
```

Use `--steps-stdin` for action JSON from stdin. Each step has one `command`,
`paste`, or named `key`, visible `until` text on a fresh completed frame, and an
optional label. `--palette light`, `--term vt100` and `--no-color` select native
capabilities. See `--help` for allowed commands/keys and bounds. Image-clipboard,
OS link-opening and raw-byte actions are not exposed by the CLI. No user
config/data path is accepted. Child environment is allowlisted; HOME/XDG/temp
paths are inside the fixture. Escape output is parsed, never forwarded to a GUI.
Raw-key/existing-root Python APIs are for trusted repository-owned temporary
fixtures only, with OSC52/loopback/no-auth/owner-stopped preflight; they are not a
sandbox for arbitrary programs, actions or fixture contents.

The shared standard-library driver now backs check-paste-commands and row-kind
capture. Its VT decoder covers this TUI's output, not a full terminal or font
engine: CJK/combining/wide emoji and basic ZWJ clusters, indexed/RGB cells,
Ratatui completed-frame boundaries, and OSC removal. Unknown cell operations,
narrow-base emoji presentation widths, oversized clusters/output/screen sizes
fail closed. Limits are 60s watchdogs, 4 MiB output and 400x200 cells. Live PTYs
require Unix; replay and most unit tests are portable. No third-party Python
module or Rust build is required to use the helper.

## Exact batch text replacement

Normally use literal full-line blocks with a quoted heredoc (no escaping):

```sh
python3 scripts/dev/replace.py --root . --blocks <<'EOF'
@@ replace: path/to/existing.txt
old lines with "quotes", $variables and \backslashes
@@ with
new lines with "quotes", $variables and \backslashes
@@ end
EOF
```

Repeat blocks for the same or different files. Payload lines include their exact
newlines; markers count only as entire exact lines. If the payload itself has a
delimiter line, use `--blocks --marker '%%'` and replace delimiter prefixes with
`%%`. Missing with/end or nested headers reject the whole request before writes.
For partial-line changes (or a final line without a newline), use the JSON form:
`[{"file":"path/to/existing.txt","before":"old text","after":"new text"}]`
on stdin without `--blocks`. Both inputs use the same preflight/commit engine.
Only explicit existing UTF-8 regular files inside the root are considered; no
path is selected implicitly. Empty input is rejected; an empty array is a no-op.
All needles must match exactly once, including overlapping matches. Multiple
rules for the same file apply in array order, in memory; every rule/file is
validated before any target is written. Newlines are exact, not normalized.
Empty needles, missing/binary/linked/out-of-root/read-only files are rejected.

Changed files are staged beside each target, originals are retained via hard
links, and each replacement is atomic. Precommit rechecks detect changed files;
ordinary write failures roll back earlier replacements. This is not a cross-file
filesystem transaction against crashes or uncoordinated concurrent editors. If
rollback itself fails or a committed file changes externally, originals are
retained in reported recovery directories rather than overwriting new user
changes. Exit0 means success, exit1 a non-unique needle, exit2 invalid input/I/O,
and exit3 incomplete recovery. Output is one short JSON line; no file content is
logged. Input is capped at 4MiB/256 rules and aggregate source/result text32MiB.
The helper is standard-library-only and does not build Rust or access Shellboy
settings, data, clipboard or credentials. Use it only on owned source changes;
no inputs/default paths are inferred from user configuration.

### DB-gated preview lifetime fixture

After building `--bin shellboy --example preview_gate`, run
`python3 scripts/check-preview-lifetime.py --binary target/debug/shellboy --fixture target/debug/examples/preview_gate --out NEW_OUTPUT_DIRECTORY`.
The driver creates only marked temporary roots, uses an injected non-clipboard
adapter and no provider, and releases the real DB worker via one loopback
connection after open/close/reopen. It verifies fixed observation text and no new
UI preview requests across unrelated title changes. The example rejects existing
data, unmarked roots, non-loopback configuration and credential headers. Both
fixture and driver are opt-in development tools; no product debug command exists.

Conflict forecast: `python3 scripts/dev/conflict-forecast.py [REF ...] [--json]` (Git2.40+).
Textual prediction only; files/index/refs unchanged, loose objects permitted. See `--help`.

### Canonical Code Copy PTY Qualification

After building `shellboy` and `visual_fixture`, run
`python3 scripts/check-code-copy.py --binary target/debug/shellboy --fixture target/debug/examples/visual_fixture --out /tmp/UNIQUE-code-copy-capture`.
The eight-case 80/140-column dark/light/NO_COLOR/vt100 matrix creates disposable
histories, decodes intercepted OSC52 bytes, and checks sole/numbered/full-body
copy plus cancellation without a host clipboard or provider call. The only
real-time bound is the shared 60-second watchdog. Raw terminal and text snapshots
stay outside the repository, with binary SHA-256 provenance in the manifest.

### Operation Form and Help PTY Qualification

After building the current `shellboy` source, run
`python3 scripts/check-operation-popups.py --binary target/debug/shellboy --out /tmp/UNIQUE-operation-popups`.
Use the existing shared CARGO_TARGET_DIR binary when working from a worktree;
do not create another target directory for this capture. The eight-case
80/140-column dark/light/NO_COLOR/vt100 matrix opens owner read, send, standalone
read and saved-change forms, verifies their Enter verbs, opens per-operation F1
help and returns to the retained form/list. It never submits an operation and
creates only isolated disposable data through the shared PTY helper. The sole
real-time bound is that helper's60-second watchdog. Text/raw frames and a binary
SHA-256 manifest are evidence of the native Unix process/PTY only, not physical
terminal screenshots or Windows UI success. Retain failures as well as passes;
the existence of this driver is not a qualification result.

### Notification Reference PTYs

After building the current source into the shared CARGO_TARGET_DIR, run
`python3 scripts/check-notice-references.py --binary SHARED_TARGET/debug/shellboy --fixture SHARED_TARGET/debug/examples/visual_fixture --out NEW_TEMP_DIRECTORY`.
Use that shared build directly; do not copy executables or create another target
directory for a measurement. Record the source and binary digest before capture.
It creates disposable synthetic databases and captures sixteen 80/140-column
shell/child scenes across dark/light/NO_COLOR/vt100, including exact original
access, duplicate short names, reactive rename and deletion. It never uses user
data, real providers or the host clipboard. The only timeout is the shared 60s
watchdog; observations are not latency acceptance gates.

## Optional External Shell Error Catalogue (#72)

Python3.10+ with SQLite3.43+ (JSON and current-schema octet_length support) can
run this standalone script; Rust Core,
owner startup, UI and conversation schema are unchanged. Always supply an explicit
data directory. No provider credentials, owner process or network is needed.

```bash
python3 scripts/shellboy-errors.py --data-dir DATA build --repo-root REPO
python3 scripts/shellboy-errors.py --data-dir DATA build --incremental --repo-root REPO
python3 scripts/shellboy-errors.py --data-dir DATA top --since 2026-10-31T00:00:00Z
python3 scripts/shellboy-errors.py --data-dir DATA similar --execution EXECUTION_ID
```

The derived file is `DATA/derived/errors.sqlite3`; deleting it and explicitly
building again recreates it. The current schema17 reader fails closed on unknown
schemas or more than10,000 source executions (no silent partial statistics).
Incremental refresh still scans the bounded current set to catch in-place terminal
changes and deletion, compares sanitized facts, and skips writing an unchanged
snapshot. A changed snapshot is replaced atomically, not a per-row read delta.
Do not use its durations as a claim of faster append-only indexing.

Outputs are historical associations, never recommendations. Read the source IDs
and verify actual conditions before using a successful command. Sanitization is
heuristic, not a guarantee of detecting arbitrary secrets; keep derived data
private and inspect before sharing. The [design](../design/error-catalog.md)
records text/identity/step limits and deletion/privacy semantics.

Tests use temporary synthetic current-schema databases only:
`python3 -m unittest discover -s scripts/tests -p test_error_catalog.py`.
`python3 scripts/measure-error-catalog.py` builds10,000 synthetic executions,
records one full/changed-fact incremental duration under the60s watchdog and
removes all generated data. It never locates or reads a real user's data-dir.

## Optional Linux Resource Benchmark

For cost-free, deterministic owner-tree PSS and PTY startup observations, see
[`scripts/bench/README.md`](../../scripts/bench/README.md) (#116 stage 1).
Use `python3 scripts/bench/footprint.py --help` for arguments. The existing
`ci.yml` workflow has an opt-in `footprint` input and retains a JSON artifact;
there are no performance thresholds and no cross-agent/provider comparisons.
