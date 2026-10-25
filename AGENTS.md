# Shellboy 開発ガイド

このファイルはShellboy自体の開発指示です。製品の利用者は [User Guide](docs/guides/user-guide.md)、Shellboy内のモデルは [Agent Guide](docs/guides/agent-guide.md) を参照してください。リンク先は自動展開されません。

## 作業の基準

- 最初に [適用仕様](docs/design/effective-spec.md) を読み、対象領域の採用設計へ進む。公開型・設定・migration・fixtureと設計を照合し、矛盾は設計へ先に反映する。
- 現在地は [開発状況](docs/development/status.md)、検証済みと未実施の区別は [検証状況](docs/validation/acceptance.md) に記録する。過去の試験や文書の存在を現変更の成功として扱わない。
- 通常の判断は自律的に行う。確信が弱い判断は [判断記録](docs/development/decisions.md) に理由・影響・再確認条件を残す。
- 文書は [配置と更新規約](docs/README.md) に従う。採用設計はこのリポジトリだけで参照できる状態を保つ。

## 実装の境界

- Rust / Ratatui / Crossterm / Tokio / bundled SQLite / sqlite-vecを使う。固定ツールチェーン・依存変更には理由と検証結果を残す。
- UIのメインOSスレッドにI/O、DB操作、join、キュー空き待ちを置かない。共有async runtime、専用DB worker、有限blocking file workerへ分ける。
- モデル向けツールはshellのみ。履歴・送信・子・待機・添付は `shellboy` CLI経由とする。
- 状態確定・永続化・取消し・遅着は [Control設計](docs/design/control.md) と対象設計の順序を維持する。新しい表・event・worker・queue・依存には必要性・上限・所有者・終了責任を記録する。
- 通常のRust moduleを使い、汎用Repositoryや独立ジョブエンジンを追加しない。未実装をstubの成功で隠さない。
- 資格情報、ローカルDB、実会話、私的ログをGitへ入れない。改名前のコマンド・環境変数・保存先への互換経路は追加しない。

## 検証と引継ぎ

- 変更に応じた検証を実施する。標準チェックは `sh scripts/check.sh` または `./scripts/check.ps1`。手順は [開発環境](docs/development/setup.md) に記載する。
- 一時DB・合成fixture・ローカルmockを優先する。実APIの課金や利用者の保存データの変更を通常の確認に混ぜない。
- 実行環境・コマンド・結果・未実施を区別し、実機がないOSや端末をmock成功で合格としない。
- 意味のある単位でローカルcommitを作る。push・リリースは利用者の明示指示に従う。既存レビュー対応を引き継ぐ場合は [台帳の運用](docs/development/maintenance.md) を確認する。作業後は開発状況を更新し、再開点を明確にする。
