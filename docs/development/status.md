# 開発状況

更新: 2026-10-26。Shellboyへの名称変更と公開用構成の準備は完了しました。GitHubへのpushと [初版v0.1.0の公開](https://github.com/tya5/shellboy/releases/tag/v0.1.0) は完了しました。[Windows x64 ZIP](https://github.com/tya5/shellboy/releases/download/v0.1.0/shellboy-v0.1.0-x86_64-pc-windows-msvc.zip)、macOS ARM64、Linux x64の配布物とSHA-256を公開しています。最新の実行結果は [Actions](https://github.com/tya5/shellboy/actions)、配布物は [Releases](https://github.com/tya5/shellboy/releases) で確認します。

TUI、同一ownerへ接続するCLI、Responses互換通信、shell、親子の会話と実行、待機、分岐履歴、要約・スピル、添付、埋め込み索引、GC、同梱ガイド、独立RAGを実装しています。責務と対象外は [適用仕様](../design/effective-spec.md) に記載します。

## Enter直後の送信状態

Enterを押した時点で既存ステータス行に「送信受付中」を表示し、同じアニメーション時計で動かします。保存成功後は対応message IDを履歴で確認するまで「保存済み・表示待ち」を表示します。すでにRun/Turnが進んでいる場合はその段階を主表示に維持し、表示待ちを併記します。複数送信・経過時間・失敗時の入力保持と、通知順序の前後・高速完了・会話切替を扱います。

既存の有限診断writerへmessage_sendを追加し、draft ID/revision・実際のoperation ID・保存message IDを関連付けます。保存成功をUI通知前に記録するため、保存前の待ちと通知・履歴・描画側の問題の切り分けに使えます。本文・添付pathのログや新しいCore状態・表・workerは追加しません。設計は [実行状態表示](../design/tui-activity-indicator.md)、調査手順は [診断ログ](../guides/recovery-and-storage.md#診断ログを確認する) を参照してください。

macOSで対象8 suiteの441 passed / 0 failed / 3 ignored、strict Clippy、fmt/diff、同梱docs検証、staged秘密情報スキャンが成功しました。詳細と未実施範囲は [検証状況](../validation/acceptance.md) に記録します。変更はローカルのみで、push・公開済み配布物・利用中ownerは変更していません。

## 初版公開後のストリーム修正

途中の受信JSONと最終responseの差だけで復旧待ちにする独自照合を廃止し、検証済み最終responseを採用する方針へ変更しました。途中・最終の差は保存証跡と本文を含まない診断ログへ残し、ツールは最終responseの採用成功後だけ実行します。不正な最終応答・通信失敗等は既存の復旧対象です。

受信済み回答と平文推論を復旧待ちでも参照できる表示、再起動後の保存原文、全受信記録を辿るTUI `/received` を追加しました。成功時のメタデータ差だけでは回答を二重表示しません。既存worker・表・Core状態を増やさず、Thread削除後は従来のGCで本文を回収します。

修正sourceはローカルcommit `d170da6` です。macOSの標準チェックは全68 suite、1,008 passed / 0 failed / 15 ignoredで成功しました。最終表示コードのstrict lib Clippy、同梱docsの33 CLI確認/13 guides/3 categories/116 links、秘密情報スキャンも成功しました。詳細は [検証状況](../validation/acceptance.md) を参照してください。公開済みv0.1.0の配布物は変更していません。会社のWindows・社内API proxyによる実機再検証は未実施で、以下の初版検証結果とは区別します。

## Windows隠し環境変数の修正

[Issue #1](https://github.com/tya5/shellboy/issues/1)へ対応し、`=::`、`=ExitCode`等を含む継承環境をshellへ渡せる検査へ変更しました。空名・NUL・名前の途中の`=`は引き続き拒否し、診断へは値を含めず変数名だけを有界・escape付きで表示します。環境値の`=`やUTF-16 code unit、設定envの名前制約は維持します。

macOSで共通UTF-16組立の4試験、対象strict Clippy、fmt/diffが成功しました。Windows専用のPowerShell起動回帰も追加しましたが、Windowsでの実行は未実施です。変更はローカルに保持し、push・配布物の更新は行っていません。詳細は [検証状況](../validation/acceptance.md) に記録しています。

## WSL/LinuxのIPC起動条件の修正

[Issue #2](https://github.com/tya5/shellboy/issues/2)へ対応し、XDG_RUNTIME_DIRが使えない環境でもOS一時領域に専用0700 socket directoryを自動で用意します。固定hashや保存先祖先の権限への依存を除き、接続両側のpeer uid照合、モデルshellへのSHELLBOY_SOCKET伝播、既存endpoint.json経由の人向けCLI接続を揃えました。別data-dir指定やinstance不一致で別ownerへfallbackしません。

mkdir前に予定pathを記録し、locatorのみ・空directory・bind後の各段階で落ちた場合も次回起動で検証して回収します。別owner・改変file・symlink・未知の内容は削除しません。Unix保存先は0700へ限定し、親の権限は変更しません。macOSで対象5 suiteの430件、umask002/XDGなしの複合条件の追加確認、strict Clippy、同梱docs検証が成功しました。詳細は [検証状況](../validation/acceptance.md) に記録しています。Linux/WSL実機・別uidの実processでの確認、push・新配布物は未実施です。

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
