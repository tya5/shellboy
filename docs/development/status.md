# 開発状況

更新: 2026-10-26。Shellboyへの名称変更と公開用構成の準備は完了しました。GitHubへのpushと [初版v0.1.0の公開](https://github.com/tya5/shellboy/releases/tag/v0.1.0) は完了しました。[Windows x64 ZIP](https://github.com/tya5/shellboy/releases/download/v0.1.0/shellboy-v0.1.0-x86_64-pc-windows-msvc.zip)、macOS ARM64、Linux x64の配布物とSHA-256を公開しています。最新の実行結果は [Actions](https://github.com/tya5/shellboy/actions)、配布物は [Releases](https://github.com/tya5/shellboy/releases) で確認します。

TUI、同一ownerへ接続するCLI、Responses互換通信、shell、親子の会話と実行、待機、分岐履歴、要約・スピル、添付、埋め込み索引、GC、同梱ガイド、独立RAGを実装しています。責務と対象外は [適用仕様](../design/effective-spec.md) に記載します。

## 初版公開後のストリーム修正

途中の受信JSONと最終responseの差だけで復旧待ちにする独自照合を廃止し、検証済み最終responseを採用する方針へ変更しました。途中・最終の差は保存証跡と本文を含まない診断ログへ残し、ツールは最終responseの採用成功後だけ実行します。不正な最終応答・通信失敗等は既存の復旧対象です。

受信済み回答と平文推論を復旧待ちでも参照できる表示、再起動後の保存原文、全受信記録を辿るTUI `/received` を追加しました。成功時のメタデータ差だけでは回答を二重表示しません。既存worker・表・Core状態を増やさず、Thread削除後は従来のGCで本文を回収します。

修正sourceはローカルcommit `d170da6` です。macOSの標準チェックは全68 suite、1,008 passed / 0 failed / 15 ignoredで成功しました。最終表示コードのstrict lib Clippy、同梱docsの33 CLI確認/13 guides/3 categories/116 links、秘密情報スキャンも成功しました。詳細は [検証状況](../validation/acceptance.md) を参照してください。公開済みv0.1.0の配布物は変更していません。会社のWindows・社内API proxyによる実機再検証は未実施で、以下の初版検証結果とは区別します。

## 初版公開の完了範囲

- package・実行名・Rust参照・環境変数・既定保存先・製品名付き識別子をShellboyへ統一しました。旧名や旧保存形式の互換は追加していません。
- 利用者指定のMIT Licenseを追加しました。既存のインストール・設定・保存データは変更していません。
- 既存履歴はローカルに保持し、公開用には当初選別した320ファイルから独立したGit履歴を作成しました。非公開資料・過去レビュー・個人の画像やログは同梱していません。
- 未統合だった復旧表示修正を別commitで取り込み、元の保存JSON・完全値のコピーを維持しました。
- 公開用ソースでfmt・全target Clippy・67テスト群を実行し、982 passed / 0 failed / 15 ignoredでした。同梱docsのオフライン確認も成功しました。
- 公開用Gitの全履歴をgitleaksで検査し、検出0件でした。詳細は [検証状況](../validation/acceptance.md)、公開範囲と判断は [公開構成](publication.md) と [判断記録](decisions.md) を参照してください。

接続ごとの `ssl_verify`（既定true）を初版へ追加しました。HTTP(S)プロキシ/NO_PROXYはライブラリの既存環境変数を使い、設定ガイドにBash/PowerShell例を記載しています。TLS変更後のnative CIはWindows 891件、macOS/Linux各986件が成功しました。配布物は3 OSでrelease buildとhelp/versionを確認し、全archiveの公開ダウンロードとSHA-256照合も成功しました。後続のWindows試験待機上限調整は製品コードを変更しておらず、別commitの検証として [記録](../validation/acceptance.md) します。

改名前の作業履歴と公開用の新履歴は分離しています。以前のレビュー対応goalは中断状態を維持し、この名称変更・公開準備を旧goal全体の完遂とは扱いません。

Windows/Linuxのnative検証はGitHub Actionsで実施しました。実provider API、実IME・OS clipboard、各端末GUIの再検証は今回未実施です。既知の制限は [一覧](known-limitations.md)、レビュー台帳は [運用](maintenance.md) に記載します。
