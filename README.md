# Shellboy

Rust製のターミナル向けAIエージェント環境です。Responses互換proxyへの接続、shell実行、分岐可能な履歴、SQLiteによる永続化、子エージェントへの委譲を備えます。

使い始める人は [User Guide](docs/guides/user-guide.md)、Shellboy内で作業するモデルは [Agent Guide](docs/guides/agent-guide.md) を参照してください。[ガイド一覧](docs/guides/README.md) から設定・履歴検索・添付・復旧・CLIへ進めます。

現在はShellboyへの改名と公開用構成を準備しています。改名前の検証結果を改名後の合格とは扱いません。現在の確認範囲は [開発状況](docs/development/status.md) と [検証状況](docs/validation/acceptance.md) に記録します。

## ビルドと起動

RustupとOSのC/C++ビルド環境を用意します。ツールチェーンは `rust-toolchain.toml`、依存は `Cargo.lock` で固定します。

```sh
cargo build --locked
cargo run --locked --bin shellboy -- --help
cargo run --locked --bin shellboy -- --version
```

[config.example.toml](config.example.toml) を任意の設定先へコピーし、接続URLとモデルIDを設定してください。資格情報は設定した環境変数から読みます。鍵や実会話をGitへ保存しません。

```sh
cargo run --locked --bin shellboy -- --config /path/to/config.toml --data-dir /path/to/data
```

引数なしの起動は対話端末が必要です。owner接続が必要なCLI操作は同じdata-dirで起動中のShellboyへ接続します。オフラインのガイドは `shellboy docs list` で確認できます。APIを使わない試行には [端末fixture](docs/validation/manual-terminal.md) を利用できます。

標準チェックはmacOS/Linuxで `sh scripts/check.sh`、Windows PowerShellで `./scripts/check.ps1` です。詳しくは [開発環境](docs/development/setup.md) を参照してください。

## 構成

```text
src/              通常のRust moduleと製品入口
migrations/       版付きSQLite migration
tests/            結合試験と合成fixture
examples/         ローカル確認用fixture
scripts/          検証・端末キャプチャ用ツール
docs/guides/      利用者とモデル向け同梱ガイド
docs/design/      採用仕様と実装設計
docs/development/ 開発環境・現在地・判断理由
docs/validation/  確認手順と実施状況
```

単一Cargo package・実行ファイル名ともに `shellboy` です。[適用仕様](docs/design/effective-spec.md) を開発の入口とし、[AGENTS.md](AGENTS.md) に作業境界を定めます。旧製品名のコマンド・環境変数・既定保存先との互換は提供しません。既存データの移動は不要で、Shellboyは新しい既定保存先を使います。
