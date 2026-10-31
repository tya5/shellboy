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
the full three-OS matrix with four Windows test threads. The manual
`default` choice also preserves all three OSes; `windows_test_threads=2` provides
a two-thread diagnosis. Experimental failures are retained and never
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
