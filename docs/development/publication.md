# 公開用リポジトリの構成

2026-10-26。公開先は `tya5/shellboy`。名称変更と公開準備を終え、利用者の明示許可によりmainと [v0.1.0 Release](https://github.com/tya5/shellboy/releases/tag/v0.1.0) を公開しました。

2026-10-26、追加修正を [v0.1.1](https://github.com/tya5/shellboy/releases/tag/v0.1.1) として公開しました。tagは `e55da58`、Windows x64・macOS ARM64・Linux x64の配布物とSHA256SUMSを提供し、公開ダウンロードと照合を確認しています。v0.1.0は保持しています。

2026-10-27、検証済みcommit `77da7e3` を [v0.1.2](https://github.com/tya5/shellboy/releases/tag/v0.1.2) として公開しました。3 OSのnative CI・配布ビルド・起動確認が成功し、全archiveの認証なしダウンロードとSHA-256照合も完了しました。ローカルバイナリは公開macOS版に更新済みです。旧tag・配布物は保持しています。

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
