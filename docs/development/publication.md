# 公開用リポジトリの構成

2026-10-26。公開先は `tya5/shellboy`。名称変更と公開準備を終え、利用者の明示許可によりmainと [v0.1.0 Release](https://github.com/tya5/shellboy/releases/tag/v0.1.0) を公開しました。

2026-10-26、追加修正を [v0.1.1](https://github.com/tya5/shellboy/releases/tag/v0.1.1) として公開しました。tagは `e55da58`、Windows x64・macOS ARM64・Linux x64の配布物とSHA256SUMSを提供し、公開ダウンロードと照合を確認しています。v0.1.0は保持しています。

2026-10-27、検証済みcommit `77da7e3` を [v0.1.2](https://github.com/tya5/shellboy/releases/tag/v0.1.2) として公開しました。3 OSのnative CI・配布ビルド・起動確認が成功し、全archiveの認証なしダウンロードとSHA-256照合も完了しました。ローカルバイナリは公開macOS版に更新済みです。旧tag・配布物は保持しています。

2026-10-27、commit `bbb9758` を [v0.1.3](https://github.com/tya5/shellboy/releases/tag/v0.1.3) として公開しました。[3 OSのCI](https://github.com/tya5/shellboy/actions/runs/40482476917) と [配布build](https://github.com/tya5/shellboy/actions/runs/40484109212) が成功し、Windows ZIPを含む3 archive・SHA256SUMSを認証なしで取得して照合しました。ローカルbinaryも公開macOS版へ更新しています。v0.1.2とschema v9・既存設定・CLIを維持し、新設定を追加したファイルで旧版へ戻す場合は新keyを除きます。旧tag・assetは保持しています。

## 同梱するもの

通常のRustプロジェクト構成を維持し、`src/`、`tests/` と合成fixture、`migrations/`、`examples/`、固定toolchainとlockfile、CI、汎用の検証scriptを同梱します。`docs/guides/` は実行ファイルへ組み込むため全件必要です。採用済み設計、汎用セットアップ、判断、現在地、検証手順も公開します。

## ローカルに保管するもの

非公開の原仕様snapshot、元レビュー・作業台帳、実会話を含み得る画像・ログ・過去検証記録、個人用cacheと接続環境専用の検証scriptは公開対象外です。ファイルは削除せず、Git管理から外して `.gitignore` に記載します。資格情報、DB、ビルド生成物もGit管理外です。

既存履歴には非公開資料が含まれるため、公開対象ファイルだけを別ディレクトリへ配置して新しいGit履歴を作成します。旧履歴の書換えや削除は行いません。公開側へ旧リポジトリをremote追加・fetch・mergeしないでください。新しい履歴には公開用ファイルだけをcommitします。

## 名称変更の範囲

Cargo package、実行ファイル、Rustの参照、CLI使用例、環境変数、OS既定保存先、DB名、内部の製品名付き識別子を `shellboy` へ統一します。旧形式との互換・自動移行は実装しません。既存のインストール・設定・保存データには手を加えません。新しい保存先は [setup](setup.md) を参照してください。

検証結果は [acceptance](../validation/acceptance.md) に記録します。利用者の指定により [MIT License](../../LICENSE) を同梱します。

## 公開範囲の確認（2026-10-26）

公開用Gitは旧Gitのcloneではなく、選別した320ファイルから初期化した独立した履歴です。旧commitをparent・remote・alternateとして持ちません。非公開snapshot、過去の検証asset、未追跡のレビュー画像、個人cacheは公開用ディレクトリに配置していません。元のファイルと履歴はローカルに保持しています。

公開用ファイルの旧名称残存は0件、相対リンク424件の参照先欠落は0件、Rustのliteral include参照63件の欠落は0件でした。実行時ガイドを含む公開対象ファイルだけで検証します。

公式releaseのchecksumを照合したgitleaks v8.30.1で、公開用Gitの全ref・全履歴を検査しました。既定ルールを使い、allowコメントによる除外を無効にし、結果は0件でした。結果文書を含む最終commit後にも同じ検査を実施しています。実行例は `gitleaks git . --log-opts=--all --redact=100 --ignore-gitleaks-allow` です。生成した詳細reportやlogはGit管理外に置きます。

名前の確認では、[GitHubの公開先](https://github.com/tya5/shellboy) は利用者が作成済みです。[別所有者の同名repository](https://github.com/c1982/shellboy) も存在します。今回の公開先は所有者名で区別できるため変更しません。[crates.io API](https://crates.io/api/v1/crates/shellboy) は確認時点で404（未登録）でした。名前の予約は行っておらず、Cargoの `publish = false` は維持しています。

## Release validation gates (#57)

Run the full local standard check (`sh scripts/check.sh` or
`./scripts/check.ps1`) successfully on the proposed tree before pushing. Rust
checks automatically run only for main pushes; branch pushes and pull requests
do not trigger them. Use explicit manual branch checks for native-OS concerns.
If main CI is red, investigate and restore it before landing other main work.

Create tags and publish releases only from an exact main SHA whose ordinary
three-OS Rust checks are green. Older-SHA checks, Windows-only experiments and
successful release artifact builds are separate evidence and cannot replace
that gate. Retain an unrelated intermittent Windows failure and any explicit
same-SHA failed-job rerun as separate records; rerun success does not erase the
failure or prove its cause. Existing user authorization requirements for push
and release still apply. See the [adopted CI policy](../design/ci-test-performance.md#ci-execution-and-publication-policy-57).

## Historical v0.3.0 preparation (#62, superseded)

This subsection records the pre-approval preparation stage, not current release
status. It is superseded by the [publication evidence](#v030-publication-2026-10-30)
below. At that stage, no v0.3.0 tag or GitHub Release had been authorized. The
version-independent README linked to releases/latest; version-specific changes
and upgrade warnings were submitted in the notes draft and issue62 for review.
The [retained notes file](release-v0.3.0-draft.md) now holds the published body.

The existing release workflow accepts optional `candidate_ref` (an exact main SHA).
For preparation, supply **both** that SHA and `release_tag=v0.3.0`, after the same
main SHA's ordinary three-OS CI is green. The tag input is an archive label, not a
created ref. Native build jobs verify checkout/package version/host, package and
extract the archive, then run --version, --help and docs list from the extracted
binary. Assembly uploads candidate-assets (three archives plus SHA256SUMS) for
30 days. Candidate mode skips the write-permission publish job. Omitting
candidate_ref uses real tag/publication mode and requires separate approval.

Keep the downloaded candidates and checksums outside Git. Final SHA, run links,
archive hashes and results are recorded in issue62/the preparation report rather
than making recursive metadata-only commits that change the chosen SHA. A later
rebuild can produce different archive bytes; never substitute it for qualified
assets without checking that new set. At the preparation stage, tags/publication,
unauthenticated v0.3.0 asset download, installed-binary updates and release
completion were later gates. Publication and public-byte verification are now
recorded below; installed-binary updates remain separately authorized work and
were not performed by this cleanup.

## v0.3.0 publication (2026-10-30)

[Shellboy v0.3.0](https://github.com/tya5/shellboy/releases/tag/v0.3.0) is public,
published at **2026-10-30T12:38:45Z**. Tag source is
`8fb885d843a39069aac80fa63708b5c70df0531c`. The
[retained release-body file](release-v0.3.0-draft.md) is an exact copy of the
published GitHub release body, retrieved with `gh release view v0.3.0 --json body`;
the old filename is retained for existing references.

### Public download verification

On 2026-10-30, anonymous HTTPS `curl --disable --fail --location` downloads
retrieved [SHA256SUMS](https://github.com/tya5/shellboy/releases/download/v0.3.0/SHA256SUMS)
and all three public archives, without GitHub credentials or curl configuration.
Every archive matches its entry in the downloaded manifest, and all three
archives extract successfully. These are **published archive hashes**, not the
earlier candidate hashes; rebuilt archives differ from the candidate artifacts.

| Public file | SHA-256 |
| --- | --- |
| `SHA256SUMS` | `c8a139f704027220fd51bc173e54aa0e2fbb67fd35e0f9b4eb3859714b447611` |
| `shellboy-v0.3.0-aarch64-apple-darwin.tar.gz` | `1e0010db3a34cd6c159e591d92ffac64062ce793f767972f0518346522d8f5d4` |
| `shellboy-v0.3.0-x86_64-pc-windows-msvc.zip` | `f708987b51106c347d9039cd8012daa5e26284f4f266c836131ac5c9f2c93046` |
| `shellboy-v0.3.0-x86_64-unknown-linux-gnu.tar.gz` | `d3ff9508b2b835f446b9d7285c9ff78f2eeb8fab292a04430ae4c8f04c0754d9` |

The executable extracted from the public macOS archive reports
`shellboy 0.3.0` on native macOS 26.3.1 arm64.
[Native public-byte smoke run 41058668366](https://github.com/tya5/shellboy/actions/runs/41058668366)
passes on all three OSes at isolated validation SHA
`fd8530ef95c31091b349c3442574a9f8056a0ac8`. The read-only manual workflow
anonymously downloads public `SHA256SUMS` and each native archive, verifies
independently pinned hashes, extracts it and runs its executable's `--version`.
It performs no build or publication, and its branch-only workflow is not merged
into main. Each native result is exactly `shellboy 0.3.0`:

| Runner observed | Public executable SHA-256 |
| --- | --- |
| Windows Server 2025 / AMD64 | `60053ea6f41524584d4786434bfb9eddca726b7ecc7e1984a8f61d55c763f94a` |
| Ubuntu 22.04 / x86_64 / glibc 2.35 | `a583930def92fe801da013d531a7d73008c38585f81569acff8c830a6bef15ac` |
| macOS 15.8.2 / arm64 | `2e9b1c76b6cb959852eaf2f7283051f186f6ca19acf7e4b2168f0b52e09372bb` |

Cleanup standard check (`sh scripts/check.sh`, native macOS arm64,
Rust1.99.1) passes: 1,658 passed / 0 failed / 32 ignored, layout88, Python6,
format and strict all-target Clippy. An earlier local check failed the existing
`tui_process::real_tui_accepts_text_renders_proxy_response_and_exits_cleanly`
repaint assertion for the index retry confirmation. Fresh validation-branch and
cleanup checks pass that test unchanged; the cause remains unresolved, not
claimed fixed or suppressed.

Earlier candidate native checks alone do not prove public bytes execute.
This public-byte smoke does not qualify physical terminals or real providers on
every OS, nor rerun the candidate's synthetic migration on every public binary.
Installed binaries and real user data are unchanged.

## Installer assets (#63; releases after v0.3.0)

Starting with the next release, assembly includes `install.sh` and `install.ps1`
as standalone Release assets alongside the three archives and `SHA256SUMS`.
The scripts in the repository are templates: assembly replaces their single
`__SHELLBOY_RELEASE_VERSION__` marker with the release tag, so both the latest
and version-specific asset URLs select matching archives, even after a later
release. `SHELLBOY_VERSION` explicitly overrides that default. Unprocessed repository
scripts require this variable; they do not resolve latest independently. Do not distribute
raw main URLs. Do not add assets to the already published v0.3.0 Release.

Before publication, run the manual **Release binaries** workflow on the proposed
branch with `installer_checks=true` and `release_tag=v0.3.0` (leave
`candidate_ref` empty). It downloads public v0.3.0 archives without authentication, uses temporary
`SHELLBOY_INSTALL_DIR` directories, and checks fresh/replacement installation on
macOS ARM64, Linux x64 (dash), Windows PowerShell 5.1 and PowerShell 7. Negative
checks use local download mocks for corrupt checksums and unsupported CPUs.
They do not update an installed binary or start an owner/database. This workflow
is validation only and must never publish a Release. Review its run results;
a written procedure is not evidence of a passing run.

For the next candidate release, inspect the `candidate-assets` artifact: expect
three archives, two version-bound scripts, and `SHA256SUMS` covering all five.
Check the scripts contain the proposed tag and no template marker, and run them
against the matching public release after publication using a temporary install
directory. The ordinary three-OS Rust CI gate still applies independently.

### Installer validation (2026-10-30)

[Manual branch run 41060210585](https://github.com/tya5/shellboy/actions/runs/41060210585)
at script/workflow SHA `64a5302` passes all four jobs: macOS 15 ARM64,
Ubuntu 22.04 x64 (`sh`/dash), Windows PowerShell 5.1 and PowerShell 7.
Public v0.3.0 downloads were unauthenticated. Each job uses a temporary directory
with spaces in its path, installs fresh, replaces the binary, verifies
`shellboy 0.3.0` and identical backup bytes, tests an environment version override
against a differently bound script, and tests a bound script without an override.
Mock downloads verify that checksum mismatch leaves an existing binary unchanged
and creates no fresh destination; mock CPU detection verifies explicit rejection.
PATH/restart/database-backup guidance is asserted. Build, assembly and publish
jobs are skipped in this validation mode.

Native macOS ARM64 runs the same installer checks successfully. Local validation
also rejects invalid version strings and exercises the assembly Python with
synthetic archives: three archives, two version-bound scripts and a five-entry
`SHA256SUMS` are produced. This fixture is not a release build. Full standard
checks pass locally after rebase; Rust code is unchanged. All installed-binary
checks use `SHELLBOY_INSTALL_DIR`; the user's installed binary, profiles and real
data are untouched. Downloading installer assets through the advertised Release
URLs remains untested until the next release publishes those assets; v0.3.0 is
intentionally unchanged.

### Windows running-image replacement follow-up (#63)

[Manual branch run 41062404250](https://github.com/tya5/shellboy/actions/runs/41062404250)
at `c7b155e` passes Windows PowerShell 5.1 and PowerShell 7 (and both Unix jobs).
Windows now renames the old executable to its unique backup path, then renames
the staged binary into place; a failed final rename restores the backup. If
restoration itself fails, the error names the retained backup for manual repair.
The running process is not stopped or upgraded by this operation.

The regression test keeps the real public v0.3.0 executable running, blocked on
`message send --text-stdin` with stdin left open. This holds the Windows image
without starting an interactive owner, accessing a real database or contacting
a model. Replacement succeeds while that process remains live; the new binary
runs and the backup matches the original bytes. A second live-image check
removes the staged file to inject a final-rename failure, then verifies the
original binary is restored, runnable and byte-identical, no extra backup is
left, and the held process is still live. The fixture kills and reaps its own
processes before removing its temporary installation.


## v0.4.0 preparation only (#70 / D-012)

Authorized scope stops at issue70 step8: version/notes/README, synthetic upgrade,
exact-source native CI, candidate archives/installers/checksums and isolated
installation. No v0.4.0 tag, GitHub Release (including draft), publication or user
binary replacement is authorized. The #54 review fix must be in main before
selecting the candidate. Include #41 D2 only if already in main at the cut; do
not wait for or declare the whole redesign complete.

Run Release binaries manually with both release_tag=v0.4.0 and candidate_ref set
to an exact main SHA whose ordinary three-OS CI is green. Candidate mode retains
contents:read and skips the publish job. The candidate-installers matrix consumes
the assembled candidate-assets (three archives, two stamped scripts, SHA256SUMS)
on native macOS ARM64/Linux x64 and Windows PowerShell5.1/7. All five asset hashes
are checked; template markers/wrong stamps fail. Each actual installer runs with
SHELLBOY_VERSION removed and SHELLBOY_INSTALL_DIR set to a disposable path with
spaces, then replaces that installation, verifies backup bytes, and rejects a
corrupt archive without changing existing or creating fresh destination state.
Downloads are a strict local mirror of the intended release URLs: unpublished
v0.4.0 URLs cannot be qualified before approval/publication. No public v0.3.0
asset is changed, and no user install/profile/PATH/database is touched.

scripts/check-release-upgrade.py takes an independently verified public0.3.0
binary and an exact candidate binary. Owned Unix PTYs create completed/recovery/
queued model-sender conversations through real0.3.0 operations, plus synthetic
indexes/RAG. Stop that fixture owner, preserve committed WAL via SQLite backup,
then open the copy with the candidate. Expect schema14/unchanged migration ledger,
RAG3, readable conversation/sender/pending/usage and successful loopback exec.
All endpoints are local mocks. Preserve a stopped, checkpointed RAG file without
WAL sidecars via read-only immutable backup only after every writer is reaped;
never ignore an existing conversation WAL. No real user data is used.

Keep exact candidate SHA/run links, all six SHA-256 values and installer/upgrade
results in issue70 and outside-Git artifact evidence rather than changing the
qualified SHA recursively. The retained release-note draft is review text, not
a created GitHub Release. Stop at step8 for review and publication authorization.
