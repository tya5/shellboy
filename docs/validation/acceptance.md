# 検証状況

更新: 2026-10-26。Shellboyへの改名後の検証結果はこれから記録します。この公開用文書の作成時点では、以下を成功と宣言しません。改名前の試験件数・commit・私的ログ・画像を現在の合格根拠として扱いません。

| 確認 | 現在の扱い | 手順・対象 |
|---|---|---|
| 固定依存でのビルドとCLI名 | 未確定 | `cargo build --locked`、`shellboy --help`、`shellboy --version` |
| fmt・全target Clippy・テスト | 未確定 | `sh scripts/check.sh` または `./scripts/check.ps1` |
| オフライン同梱ガイド | 未確定 | `shellboy docs list`、`scripts/check-docs.py`。owner・設定・APIがなくても読めること |
| 新しい既定保存先・環境変数 | 未確定 | 設定試験、一時data-dir。旧環境の変更や自動移行をしないこと |
| 一時owner・CLI・正常終了 | 未確定 | 合成fixtureで起動・送信・状態照会・終了・再openを確認 |
| TUI・端末・IME・clipboard | 未実施の環境は対象外 | [実端末手順](manual-terminal.md)。自動PTYを実IMEの成功としない |
| 実provider互換・意味検索の品質 | 未実施 | loopback固定応答・固定vectorで代用しない。実APIは別途明示した確認のみ |
| 公開用初期checkout | 未確定 | 非公開ファイルなしでbuild/test、同梱ガイド、相対リンクを確認 |

実施後は対象source、OS/architecture、コマンド、成功/失敗/未実施、制限をこの文書へ追記します。性能試験のignoreを通常テストの成功に含めず、手順だけを実施証拠にしません。実会話・資格情報・個人path・私的画像を証拠としてGitへ入れません。

採用範囲は [適用仕様](../design/effective-spec.md)、開発の現在地は [status](../development/status.md)、標準チェックは [setup](../development/setup.md) に記載します。
