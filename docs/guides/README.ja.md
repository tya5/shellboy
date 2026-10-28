# Shellboyガイド

[English](README.md)

shellboyを使う人と、shellboy内で動くモデルのための操作ガイドです。初めて使う人は [User Guide](user-guide.ja.md)、作業を実行するモデルは [Agent Guide](agent-guide.md) から読みます。

## 目的から読む

| したいこと | ガイド |
|---|---|
| 起動して話す、別の会話へ切り替える | [User Guide](user-guide.ja.md) |
| shellboy内のモデルに操作方法を教える | [Agent Guide](agent-guide.md) |
| 同梱ガイドの階層・本文・検索を使う | [ガイドを見つける](docs.ja.md) |
| 文書の索引・意味検索・複数範囲の検索を使う | [文書と履歴を検索する](rag.ja.md) |
| API接続・モデル・保存場所・キー操作を設定する | [設定](configuration.ja.md) |
| キー・slashコマンド・下書き・貼付けを使う | [画面操作](tui.ja.md) |
| サブエージェントを作り、依頼・報告・待機する | [委譲と子エージェント](delegation.ja.md) |
| shellを動かし、入力・出力・背景処理を扱う | [shell](shell.ja.md) |
| 過去の会話・原文・分岐・検索・圧縮を使う | [履歴と検索](history-and-search.ja.md) |
| ファイル・画像を添付する | [添付](attachments.ja.md) |
| 復旧・停止・容量回収・エラーを調べる | [復旧とストレージ](recovery-and-storage.ja.md) |
| コマンドの構文と返却JSONを調べる | [CLI reference](cli-reference.ja.md) |

人間の最初の経路は User Guide → 画面操作 → 必要な機能別ガイドです。モデルの経路は Agent Guide → 委譲/shell → CLI referenceです。各機能別ガイドは両方の読者で共有します。

## 共通の用語

| 用語 | 意味 |
|---|---|
| Thread | 保存される一つの会話。cwd・モデル設定・履歴の選択経路を持つ |
| Turn | 一回の通常回答までの区間。モデルとの通信やtool実行を複数回含む |
| Run | shellと子Runを所有する実行寿命。Turnが回答しても背景作業が残れば生存する |
| 子エージェント / サブエージェント / subagent | 親が委譲した子Thread上で動くRun |
| cwd | そのThreadのshell作業ディレクトリ。保存先data-dirとは別 |
| data-dir / owner | DB・実行状態等の保存場所と、その場所を管理している起動中のshellboy本体 |
| node / execution | 保存された履歴項目 / 実行されたshellの識別対象 |
| operation / message | 一つのCLI受付操作 / 保存された送信本文。両者のIDは別 |

## このガイドの読み方

CLI例は特記がなければBashです。`THREAD_ID`、`RUN_ID`、`NODE_ID` 等は実際の返却JSONの値へ置き換えます。JSONの `result` 内を読み、`ok: true` と処理対象の状態を別々に確認します。会話・実行制御のCLI操作には同じdata-dirの本体が起動している必要があります。`docs` と `rag` は独立コマンドで、本体を起動せず使えます。

操作名・状態名は現行実装に合わせています。モデルやproxyの機能・出力品質・料金は接続先に依存し、Shellboy側の操作説明と区別します。Windows実機やGUI端末/IMEの未検証は [受入記録](../validation/acceptance.md) を参照してください。

英語版ガイドを実行ファイルに同梱しています。日本語版はrepositoryやrelease archiveの `.ja.md` を参照してください。検索語は英語を使い、クエリの自動翻訳は行いません。`shellboy docs list` で階層を確認し、`shellboy docs search --query 'background'` で検索、`shellboy docs get --id shellboy/shell` で本文を読みます。設定・API・repoは不要です。Turn開始時に読む `AGENTS.md` はThreadの作業指示で、ガイド全文が自動展開されるわけではありません。

## 開発資料との関係

- [文書全体の案内](../README.md)：利用ガイド・設計・開発・検証の配置
- [開発環境](../development/setup.md)：ソースからのビルドと環境準備
- [ガイド体系の設計](../design/guides.md)：読者、文書分割、作成・検証範囲
- [適用仕様](../design/effective-spec.md)：採用済みの機能仕様と設計

リポジトリ直下の [AGENTS.md](../../AGENTS.md) はshellboy自体を開発するための指示です。shellboy内で一般の調査や作業をするモデル向けには、Agent GuideとそのThread固有の指示を使い分けます。
