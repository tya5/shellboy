# 検証案内

[検証状況](acceptance.md) に実施環境と結果を記録し、[実端末手順](manual-terminal.md) で手動確認を行います。

標準チェックはリポジトリ直下で `sh scripts/check.sh`（PowerShellは `./scripts/check.ps1`）です。API利用や私的データは必要ありません。

追加の合成fixtureと検証ツールは `tests/`、`examples/`、`scripts/` にあります。生成物はGit管理外の `target/` 等へ保存し、実会話・個人のパス・資格情報を公開記録へ含めません。
