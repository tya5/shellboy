# 開発状況

更新: 2026-10-27。検証済みcommit `bbb9758` を [v0.1.3](https://github.com/tya5/shellboy/releases/tag/v0.1.3) として公開しました。#6/#7/#8、Thread一覧の状態色、起動取消し試験の安定化を含みます。ローカル標準checkは70群1,127 passed / 0 failed / 15 ignored、[3 OSのnative CI](https://github.com/tya5/shellboy/actions/runs/40482476917) と [配布ビルド・起動確認・公開](https://github.com/tya5/shellboy/actions/runs/40484109212) はすべて成功しました。全archiveの認証なしダウンロード・SHA-256・同梱内容を確認し、ローカルbinaryも公開macOS版0.1.3へ更新しました。設定・既定DB・利用中ownerは変更していません。

v0.1.2と同じschema v9・保存先・既存CLIを使い、既存設定は編集せず引き継げます。新しい設定項目を記入した後にv0.1.2へ戻す際は、新項目を除いてください。旧tag・配布物は保持しています。以下の個別変更節にある未実施・未公開は当該変更時点の記録で、公開時点の最終結果は [検証状況](../validation/acceptance.md) のv0.1.3節を参照してください。

## Issue #11/#12: 長時間の再試行と復旧ベル（未リリース）

会話の自動再試行を連続失敗予算へ変更し、正常応答の採用成功でリセットします。既定8回・指数待機上限60秒、metadataのみの切断/timeoutと部分応答の切断も対象にしました。部分応答は原文を保全して会話入力から除外し、response_retry_partial=falseで後者を無効化できます。Retry-Afterを600秒まで短縮せず待ち、超過は指定時間を示して復旧します。

既定有効のui.bellは復旧CASのcommit後だけBELを出し、背景/子会話も対象に2秒で集約します。起動済み状態・停止・自動再試行待機では鳴らしません。新しい表・永続状態・worker・queue・依存は追加していません。既存broadcastはbest effortです。

対象8群562件、別枠の61秒proxy障害（実測63.15秒）からの自動復帰、実binary/macOS PTYの有効/無効BEL数1/0を確認しました。最終静的検査と検証範囲は [acceptance](../validation/acceptance.md) に記録します。続いて#13は動作を維持するHTTP整理、#14は接続client再利用として独立commitで進めます。push・release・インストール済みbinaryの更新は行いません。

## Issue #10: 接続先固有の履歴item ID（未リリース）

送信用copyからmessage/function_callの直下idだけを外し、call_id・phase・status・未知フィールド・同epoch reasoningを維持します。通常履歴・wrapper・直接role・保存済みspillに共通適用し、adapter版を2へ上げました。元DBとspill原文は書き換えません。

既存DBの再オープン、生存owner内のHTTP400復旧待ち→明示/retry、実spillの再送、旧adapter cacheの再構築を確認しました。対象6群553 passed / 0 failed / 3 ignored、最終all-target strict Clippy・fmtが成功しました。owner再起動で停止する旧Runをretryする新仕様は加えていません。同じThreadの履歴を使った新しい依頼は現行変換を使います。ローカルLiteLLM/lunaの合成2要求は正常完了しましたが、reasoningは返らず、その実provider再送とWindows社内proxyは未検証です。最終テスト結果は [検証状況](../validation/acceptance.md) に記録します。push・release・インストール済みbinaryの更新は行いません。

## Issue #4: 環境別の起動ガイドとPATH（未リリース）

設定ガイドに端末・起動元shell・モデル用shellの区別と、Windows PowerShell/Git Bash、mintty、WSL、macOS/Linuxの構成を追加しました。PowerShell 7とGit BashのTOML例、避けるランチャー、Windows版からWSL shellを使う構成が対象外であることを示します。minttyはConPTY対応があるため一律非対応とはせず、未検証の構成として案内します。

モデル用shellのPATHは自動設定される実装へ説明を修正しました。macOSの実binaryを配置先なしの親PATHから絶対パスで起動し、モデルのBashから短いCLI名でversion/docsを実行できました。ガイド単体8件と最終CLI3件、TOML例3件、相対リンク検査が成功しました。Windows/WSLの対話実機は未実施です。製品Rustコード・設定・保存データ・公開版・ローカルbinaryの変更はありません。

## Issue #9: Ctrl+Lの全画面再描画（未リリース）

通常UI・一覧・詳細・popup・選択固定中のCtrl+Lで、状態を保持して画面と差分buffer・リンク領域を無効化します。送信/改行へ同じキーを割り当てた場合は設定を優先し、helpへ無効理由を表示します。再描画予約はbool一つへ集約し、既存max_fpsで処理します。Core操作・表・worker・queue・依存は増やしていません。

実PTYで物理cursor照会待ちを検出したため、同一viewportのresize経路でclear相当を行います。最終5群549 passed / 0 failed / 3 ignoredとall-target strict Clippy・fmtが成功しました。GUI端末の視覚確認とWindows実機検証は未実施です。保存データ・公開版・インストール済みbinaryは変更していません。

## EOF実装監査とSSE framing修正（未リリース）

Windows/v0.1.2のEOF頻発報告を受け、自前parserで先頭BOMとCR単独改行によるcompleted取りこぼしを再現し、修正しました。BOMの単一completed streamは報告と同じEOF文言、CR単独はpartial EOFです。通常のcreated先行streamではBOMだけで頻発を説明できず、会社Windowsの実原因は未確定です。HTTP受信loopにEOF通知の追い越しやWindows専用parserは見つかっていません。

既存の完成条件・tool採用・retry条件は維持し、EOFに本文なしの受信/解析カウンタと固定分類を追加します。対象8群529件（重複排除）と最終all-target strict Clippy・fmtが成功しました。修正はローカルsourceのみで、公開済み0.1.3とインストール済みbinaryには未反映です。検証範囲と環境の区別は [検証状況](../validation/acceptance.md) に記録しています。


TUI、同一ownerへ接続するCLI、Responses互換通信、shell、親子の会話と実行、待機、分岐履歴、要約・スピル、添付、埋め込み索引、GC、同梱ガイド、独立RAGを実装しています。責務と対象外は [適用仕様](../design/effective-spec.md) に記載します。

## Thread一覧の状態色（v0.1.3）

取得済みの状態に応じてタイトルと状態欄を色分けしました。実行/準備/稼働はシアン、待機/停止処理中は黄色、復旧待ち/失敗は赤、完了は緑、それ以外は通常の前景色です。文字・親子/現在Threadのmarker・選択背景は維持し、狭い画面でもタイトル色を使います。一覧page更新で同じIDの状態が変わると色も変わります。Core・一覧取得/更新の契約は変更していません。対象テスト13件が成功しました。検証範囲は [検証状況](../validation/acceptance.md) に記録します。

## Issue #6: 受信記録の注意表示（v0.1.3）

途中で切れた応答のpreviewは、元Turnが現在復旧待ちの間だけ注意色にし、回復・停止・処理再開後は通常通知へ変えます。空本文の非表示、監査原文、元Turn・source ID、モデル再送対象外は維持します。スクロール位置を固定している本文窓にも最新の表示分類を反映し、通常通知の省略groupで本文を隠しません。

受信一覧は通常のRust子moduleへ投影を分け、ローカル時刻・同Turnの元依頼の有限抜粋・保存状態で見分けられるようにしました。ページ32件と原文への導線は維持しています。新しいCore状態・表・worker・event・依存は追加していません。対象6群571件と最終all-target strict Clippy・fmtが成功しました。検証結果と未実施は [検証状況](../validation/acceptance.md) に記録します。

## Issue #7: 未受信時の有限再試行（v0.1.3）

#8のHTTP期限・受信観測を利用して、会話の未受信一時失敗だけを既定2回再試行します。接続別の回数・待機上限、Retry-After、取消し可能な待機、既存attemptへの失敗/回数/重複課金可能性の記録とTUIの回数表示を追加しました。予算はTurn/generation内で共有し、tool成功やcontext圧縮では復活させません。受信済み断片、timeout、認証・形式エラーは再送しません。新しい表・worker・queue・EngineEvent・依存は追加していません。

`3569524` の標準checkは70群1,120 passed / 0 failed / 15 ignoredで成功しました。対象9群535件も成功していますが二重計上しません。その後の上限到達時のTUI段階名の補正は対象4群474件と最終all-target strict Clippy・fmtが成功しました。全体結果と最終表示差分の検証範囲は [検証状況](../validation/acceptance.md) に記録します。先行 `13b96a1` の [CI](https://github.com/tya5/shellboy/actions/runs/40449915241) は3 OSとも成功しましたが、今回#7のnative CI成功には読み替えません。利用者の設定・保存データ・利用中owner・インストール済みbinaryは変更していません。

## 公開後のWindows CI失敗の切分け（v0.1.3）

文書のみのcommit `211750e` の [CI 40420705214](https://github.com/tya5/shellboy/actions/runs/40420705214) はmacOS/Linux成功、Windowsで起動取消し試験1件が失敗しました。v0.1.2の3 OS成功は当時の記録として維持します。旧試験の外部SQLite writer解除を2.3秒遅らせるとmacOSでも同じエラーを再現でき、製品の2秒のbusy timeoutと試験の通知待ちが競合する条件を確認しました。Windowsで実際に発生した待機時間は未確定です。

起動進捗callbackの同じ呼出しで取消す試験へ変更し、実移行完了・close・owner解放・endpoint未公開を検証します。受付済みwriterの所有保持は既存storage試験で別に保証します。製品のtimeoutや取消し順序は変更していません。検証結果は [検証状況](../validation/acceptance.md) に記録します。`305e088` のpush前標準checkは全69群1,103 passed / 0 failed / 15 ignoredで成功しました。修正後のWindows native CI結果、release、Issue #3のクローズは本記録時点では未確認・未実施です。

## Issue #8: HTTP停滞対策（v0.1.3）

接続別のHTTP期限を共通化しました。会話・要約は無受信600秒（0で無効）、embeddingは要求全体60秒、モデル一覧は同設定と20秒の短い方です。ヘッダー待ち・本文・エラー本文を対象とし、timeoutには段階・経過・設定名を残します。索引は既存retry/pauseへ渡し、検索には呼出し元切断・owner停止を伝播します。TUIは30秒以上の無受信を既存footerへ補足し、途中本文は復旧待ちでも保全します。新しい表・worker・queue・依存は追加していません。

`6aeac45` の標準checkは69群1,102 passed / 0 failed / 15 ignored。その後の `32e3359` はHTTPエラー本文の非永続受信通知だけを補強し、単体・HTTP関連473件と最終all-target strict Clippy・fmtが成功しました。対象を分けた証拠は [検証状況](../validation/acceptance.md) に記載します。高推論の実proxy測定はHTTP400で成立せず、600秒は変更可能な暫定既定値です。未受信時の自動retryは別Issue #7として分離しています。既定DB・設定・実行中owner・インストール済みbinaryは変更していません。

## レビュー対応goal

v0.1.1公開後、利用者の指示でレビュー対応を再開しました。製品はこのリポジトリ、非公開台帳・旧証拠監査は独立したローカルGitへ置きます。台帳に追加指摘19件も含め、古い定型完了文を個別根拠へ直し、反証や確認不足の親項目は検証待ちへ戻して再検証しました。旧hashを公開hashへ読み替えません。

索引選択・巨大raw参照・停止receipt・検索identity・末尾cursorに加え、復旧CASとTurn作成の共通化、LiveRegistryの所有境界、Core通知の表示責務、Turn境界SQLの索引化を実装しました。TUIは保存本文の取得まで受信済み表示を保持し、中断・ツール・エラーを構造化して表示します。長い出力の省略・折返しを含め、原文との位置対応を修正しました。

`f488196` の単体試験428件が成功しました。実WezTermセルで復旧→retry、複数tool、正式な終了記録のgroupとF6原文を確認し、現行UI経路のrelease負荷測定3回と実PTY14回も成功しました。画像取得はOS側の失敗で未実施です。条件は [検証状況](../validation/acceptance.md) に記録しています。標準checkは全68群・1,058成功・失敗0、台帳の全終端gateも成功しました。219項目は217完了・Windows実機なし2環境対象外で、判断待ちはありません。レビュー完了時点ではローカルcommitのみでした。利用者の指示により、後続の追加レビュー対応を含めてv0.1.2として公開しました。v0.1.1のtag・配布物は保持し、既定設定/DBを変更する操作は行いません。

## v0.1.2公開完了

受信本文の位置・重複表示、RAG rawの一括検証走査、管理画面の詳細表示、起動診断、macOSユーザー一時領域、Quit受付後のtool起動抑止を修正しました。Issue #3は観測のreceipt除外、現instanceの終端結果の期限切れ保護、`operation get`への照会統合で対応しています。追加レビュー7項目は全件の検証を終え、判断理由は [判断記録](decisions.md) に残しています。

`~/.local/bin/shellboy`も公開済みmacOS版0.1.2へ更新し、配布版とのSHA-256一致とhelp/versionを確認しました。設定・既定DB・利用中のprocessは変更していません。次回起動から新バイナリが使われます。schema v9更新後は旧版で同じDBを開けないため、戻す可能性がある場合は起動前に整合backupを用意してください。詳細はリリースノートを参照してください。

## v0.1.1公開完了

v0.1.0以降の修正を、検証済みcommit `e55da58` のtag `v0.1.1` として公開しました。最終Responsesの採用と受信済み本文の保持、Windowsの隠し環境変数、Unix IPCの自動配置、Enter直後の送信状態・診断が対象です。3 OSのnative CIはWindows 925件、macOS/Linux各1,025件成功。Windows x64・macOS ARM64・Linux x64のrelease buildと起動確認、全archiveの公開ダウンロードとSHA-256照合も成功しました。初版のtag・配布物は置換していません。

ローカルの `~/.local/bin/shellboy` も、公開済みmacOS版を検証して配置しました。当該リリース時点の `shellboy --version` は0.1.1でした。実行中processの停止や設定・履歴の変更はしていません。CI・配布物の確認範囲は [検証状況](../validation/acceptance.md) に記録します。

## Enter直後の送信状態

ローカル試用用に `796437e` の開発ビルドをユーザーの `~/.local/bin/shellboy` へ配置し、ビルド元とのSHA-256一致、配置後のhelp/versionを確認しました。未作成だったShellboy設定へ既存のLiteLLM接続設定を引き継ぎました。旧履歴の移行や実行中processの切替はしていません。

Enterを押した時点で既存ステータス行に「送信受付中」を表示し、同じアニメーション時計で動かします。保存成功後は対応message IDを履歴で確認するまで「保存済み・表示待ち」を表示します。すでにRun/Turnが進んでいる場合はその段階を主表示に維持し、表示待ちを併記します。複数送信・経過時間・失敗時の入力保持と、通知順序の前後・高速完了・会話切替を扱います。

既存の有限診断writerへmessage_sendを追加し、draft ID/revision・実際のoperation ID・保存message IDを関連付けます。保存成功をUI通知前に記録するため、保存前の待ちと通知・履歴・描画側の問題の切り分けに使えます。本文・添付pathのログや新しいCore状態・表・workerは追加しません。設計は [実行状態表示](../design/tui-activity-indicator.md)、調査手順は [診断ログ](../guides/recovery-and-storage.md#診断ログを確認する) を参照してください。

macOSで対象8 suiteの441 passed / 0 failed / 3 ignored、strict Clippy、fmt/diff、同梱docs検証、staged秘密情報スキャンが成功しました。詳細と未実施範囲は [検証状況](../validation/acceptance.md) に記録します。修正はv0.1.1へ含めて公開しました。利用中ownerは変更していません。

## 初版公開後のストリーム修正

途中の受信JSONと最終responseの差だけで復旧待ちにする独自照合を廃止し、検証済み最終responseを採用する方針へ変更しました。途中・最終の差は保存証跡と本文を含まない診断ログへ残し、ツールは最終responseの採用成功後だけ実行します。不正な最終応答・通信失敗等は既存の復旧対象です。

受信済み回答と平文推論を復旧待ちでも参照できる表示、再起動後の保存原文、全受信記録を辿るTUI `/received` を追加しました。成功時のメタデータ差だけでは回答を二重表示しません。既存worker・表・Core状態を増やさず、Thread削除後は従来のGCで本文を回収します。

修正sourceはcommit `d170da6` です。macOSの標準チェックは全68 suite、1,008 passed / 0 failed / 15 ignoredで成功しました。最終表示コードのstrict lib Clippy、同梱docsの33 CLI確認/13 guides/3 categories/116 links、秘密情報スキャンも成功しました。詳細は [検証状況](../validation/acceptance.md) を参照してください。公開済みv0.1.0の配布物は変更していません。会社のWindows・社内API proxyによる実機再検証は未実施で、以下の初版検証結果とは区別します。

## Windows隠し環境変数の修正

[Issue #1](https://github.com/tya5/shellboy/issues/1)へ対応し、`=::`、`=ExitCode`等を含む継承環境をshellへ渡せる検査へ変更しました。空名・NUL・名前の途中の`=`は引き続き拒否し、診断へは値を含めず変数名だけを有界・escape付きで表示します。環境値の`=`やUTF-16 code unit、設定envの名前制約は維持します。

macOSで共通UTF-16組立の4試験、対象strict Clippy、fmt/diffが成功しました。Windows専用のPowerShell起動回帰もv0.1.1のnative CIで成功しました。修正はv0.1.1へ含めて公開しています。詳細は [検証状況](../validation/acceptance.md) に記録しています。

## WSL/LinuxのIPC起動条件の修正

[Issue #2](https://github.com/tya5/shellboy/issues/2)へ対応し、XDG_RUNTIME_DIRが使えない環境でもOS一時領域に専用0700 socket directoryを自動で用意します。固定hashや保存先祖先の権限への依存を除き、接続両側のpeer uid照合、モデルshellへのSHELLBOY_SOCKET伝播、既存endpoint.json経由の人向けCLI接続を揃えました。別data-dir指定やinstance不一致で別ownerへfallbackしません。

mkdir前に予定pathを記録し、locatorのみ・空directory・bind後の各段階で落ちた場合も次回起動で検証して回収します。別owner・改変file・symlink・未知の内容は削除しません。Unix保存先は0700へ限定し、親の権限は変更しません。macOSで対象5 suiteの430件、umask002/XDGなしの複合条件の追加確認、strict Clippy、同梱docs検証が成功しました。詳細は [検証状況](../validation/acceptance.md) に記録しています。Linux native CIもv0.1.1で成功し、配布物を公開しました。WSL実機・別uidの実processでの確認は未実施です。

## 初版公開の完了範囲

- package・実行名・Rust参照・環境変数・既定保存先・製品名付き識別子をShellboyへ統一しました。旧名や旧保存形式の互換は追加していません。
- 利用者指定のMIT Licenseを追加しました。既存のインストール・設定・保存データは変更していません。
- 既存履歴はローカルに保持し、公開用には当初選別した320ファイルから独立したGit履歴を作成しました。非公開資料・過去レビュー・個人の画像やログは同梱していません。
- 未統合だった復旧表示修正を別commitで取り込み、元の保存JSON・完全値のコピーを維持しました。
- 公開用ソースでfmt・全target Clippy・67テスト群を実行し、982 passed / 0 failed / 15 ignoredでした。同梱docsのオフライン確認も成功しました。
- 公開用Gitの全履歴をgitleaksで検査し、検出0件でした。詳細は [検証状況](../validation/acceptance.md)、公開範囲と判断は [公開構成](publication.md) と [判断記録](decisions.md) を参照してください。

接続ごとの `ssl_verify`（既定true）を初版へ追加しました。HTTP(S)プロキシ/NO_PROXYはライブラリの既存環境変数を使い、設定ガイドにBash/PowerShell例を記載しています。TLS変更後のnative CIはWindows 891件、macOS/Linux各986件が成功しました。配布物は3 OSでrelease buildとhelp/versionを確認し、全archiveの公開ダウンロードとSHA-256照合も成功しました。後続のWindows試験待機上限調整は製品コードを変更しておらず、別commitの検証として [記録](../validation/acceptance.md) します。

改名前の作業履歴と公開用の新履歴は分離しています。名称変更・公開準備は旧goal全体の完遂とは扱いません。レビュー対応goalはv0.1.1公開後に再開しました。

Windows/Linuxのnative検証はGitHub Actionsで実施しました。実provider API、実IME・OS clipboard、各端末GUIの再検証は今回未実施です。既知の制限は [一覧](known-limitations.md)、レビュー台帳は [運用](maintenance.md) に記載します。
