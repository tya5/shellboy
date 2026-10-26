# Shellboy

Rust製の軽量なターミナル向けAIエージェント環境です。モデルのツールはshellだけで、モデルは `shellboy` CLIを通じて自分の履歴を検索・分岐し、子エージェントへ再帰的に委譲します。単一の実行ファイルで動き、SQLiteに保存し、Responses互換のproxyへ接続します。

## 特徴

- **軽量な単一binary**：実行ファイルは `shellboy` 1つです（Windows x64・macOS Apple Silicon・Linux x64）。会話と索引はローカルのSQLiteに保存し、常駐サービスや別のデータベースを必要としません。release binaryの起動は約41ms、待機中のRSSは約17MiBでした（空のDB・モデル未設定の限定条件。[検証状況](docs/validation/acceptance.md)）。
- **ツールはshellだけ**：モデルに渡すツールはshell 1つです。履歴の検索、送信、子の会話、待機、添付は、すべて同じ `shellboy` CLIで行います。人も同じCLIを別の端末から使えます。
- **モデルが自分の履歴を使える**：会話（Thread）、作業の寿命（Run）、次の回答までの処理（Turn）を区別して保存します。モデルはCLIで過去の発話やshellの出力をキーワード・意味の両方で検索し、原文と前後へ戻れます。履歴は書き換えずに分岐するので、要約や圧縮の後も出典へたどれます。
- **再帰的な委譲**：モデルは子のThreadを作って調査や作業を任せ、その結果を待って読めます。親子の系譜と、どのRunが処理を所有するかは別に管理します。
- **止めても失わない**：受け付けた依頼は先に保存します。通信の失敗やモデルの異常応答では、理由を残して復旧待ちにし、再試行・新しい指示・停止を選べます。

使い始める人は [User Guide](docs/guides/user-guide.md)、Shellboy内で作業するモデルは [Agent Guide](docs/guides/agent-guide.md) を参照してください。[ガイド一覧](docs/guides/README.md) から設定・履歴検索・添付・復旧・CLIへ進めます。

名称変更後の公開用ソースで標準チェックを実施しています。現在の実装・確認範囲は [開発状況](docs/development/status.md) と [検証状況](docs/validation/acceptance.md) に記録します。

## バイナリのダウンロード

[Releases](https://github.com/tya5/shellboy/releases) でWindows x64、macOS Apple Silicon、Linux x64向けの配布物と `SHA256SUMS` を提供します。v0.1.2の配布先は [v0.1.2](https://github.com/tya5/shellboy/releases/tag/v0.1.2) です。

Windowsは `shellboy-v0.1.2-x86_64-pc-windows-msvc.zip` をダウンロードして展開します。PowerShellの例:

```powershell
Expand-Archive .\shellboy-v0.1.2-x86_64-pc-windows-msvc.zip -DestinationPath .
.\shellboy-v0.1.2-x86_64-pc-windows-msvc\shellboy.exe --help
```

接続先とモデルは [設定ガイド](docs/guides/configuration.md) に従って設定してください。既知のreceipt予算・索引切替・巨大raw検索の制限は [既知の制限](docs/development/known-limitations.md) に記載しています。

v0.1.2は初回起動時に既存DBへschema v9の索引2本を追加します。旧binaryは更新後のschemaを開けません。旧版へ戻す手順は [保存設計](docs/design/storage.md) を参照してください。

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

## ライセンス

[MIT](LICENSE)
