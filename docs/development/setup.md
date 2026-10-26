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
