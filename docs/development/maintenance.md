# レビュー作業の引継ぎ

製品の開発・検証・公開は、このShellboyリポジトリで行います。非公開レビューの台帳と証拠は公開Gitへ含めません。

既存レビュー対応を引き継ぐ環境では、ローカル専用の `.local/review/README.md` に保存場所、正本の台帳、旧証拠を調べるGit、commitの対応を記録します。同ディレクトリは既存の `.local/` ignoreで除外し、必要なら独立したローカルGitで版管理します。公開側へ旧Gitをremote追加・mergeして証拠を取り込まないでください。

旧台帳は移管前snapshot、新しいローカル台帳だけを更新先とし、複数の進捗表を並行更新しません。旧commit hashは旧Gitの証拠であり、新しい公開hashへ機械的に読み替えません。公開版で再検証した結果は新しいcommit・環境・コマンドに結び付けます。

公開されている制限は [既知の制限](known-limitations.md) で読めます。通常の開発・ビルド・利用は、ローカル専用の資料が存在しない環境でも行えます。

## CI recovery and integration (#57)

Run the complete local standard check on the proposed tree before any push:
`sh scripts/check.sh` or `./scripts/check.ps1`. Automatic Rust checks are limited
to main pushes. Branch pushes and pull requests require an explicit manual
workflow dispatch when native-OS evidence is needed; existing inputs and matrix
coverage are unchanged.

Treat red main CI as the first integration task: investigate and restore it
before landing unrelated main work. Identify each failure by exact SHA, run/job
and test, distinguish product defects from fixture or runner failures, and retain
all observed results. For an unrelated intermittent Windows failure, an explicit
rerun of the failed job at the same SHA may gather evidence. Keep the original
failure alongside the rerun and do not turn retry success into a causal diagnosis
or suppress failures with automatic retries.

Release tags and releases use only a main SHA with successful ordinary three-OS
Rust checks. Validation does not itself authorize push or release. See the
[CI policy](../design/ci-test-performance.md#ci-execution-and-publication-policy-57)
and [publication gates](publication.md#release-validation-gates-57).
