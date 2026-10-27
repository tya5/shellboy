# 検証状況

更新: 2026-10-27。最新のv0.1.2検証は以下の節を参照してください。初版・先行commitの結果は当時の記録として残します。

## Issue #8: HTTP期限・取消し・受信経過（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1で実施しました。`6aeac45` の `sh scripts/check.sh` は終了0、fmt・strict all-target Clippy・69群の1,102 passed / 0 failed / 15 ignoredです。先行の対象テスト8群576件も成功しましたが、標準結果へ二重計上しません。

その後の `32e3359` は `src/model/http.rs` だけの差分です。HTTPエラー本文の受信も非永続の活動観測へ渡す補強とネットワーク回帰1件を加えました。`cargo test --locked --lib --test model_http --test http_deadlines` は473 passed / 0 failed / 3 ignored、最終 `cargo clippy --locked --all-targets -- -D warnings` とfmtも成功しました。この小さいHTTP差分後に69群全体は再実行していません。最終コードの検証範囲と、標準check対象を区別します。

| 確認対象 | 実施と結果 |
|---|---|
| ヘッダー・空本文・部分本文・エラー本文の停滞 | 接続を維持するlocal TCP mockで期限切れを検証。kind timeout、header/body、経過ms、正しい接続設定keyを保持。401/503の既知statusも失わない |
| 長い正常stream・無効化・接続別設定 | heartbeatが続くstreamは全体/idle設定より長くても完了。0では取消しまで待機。別clientの期限へ影響しない。モデル一覧は設定と20秒の短い方 |
| 索引 | `stalled_embeddings_retry_then_pause_and_manual_retry_restores_pending_work`：期限切れ→有限retry→pause→明示retry→次の入力も採用 |
| 検索の取消し | caller tokenとowner停止に加え、`history_search_ipc_disconnect_closes_embedding_http_and_next_observation_succeeds`で実LocalServer/IPC切断→60秒を待たずHTTP閉鎖→次の検索成功→receipt増加なし |
| 会話の保全 | `stalled_stream_enters_durable_recovery_and_preserves_received_text_without_replay`：実mock streamの部分受信後に停止、本文・timeout診断を保存して復旧待ち、追加要求・shellなし |
| 活動表示 | 単調時刻を使う閾値、再受信、終端、Thread/generation/attemptの遅着拒否、履歴layout不変を自動確認。HTTPエラー本文の各受信も観測通知として到達し、assistant本文を作らない |
| 設定 | 既定・接続別override・0・min1000、負値/不正型/範囲外拒否、Debugのredactを確認 |

指定の実proxyへ合成入力・high effortで1要求を送りましたが、HTTP400、header待機約0.893秒で拒否されました。正常streamの最長無受信時間は測定できておらず、600秒の既定を実測保証したとは扱いません。測定は本文・資格情報を保存せず、再試行・fallbackは行いませんでした。実provider互換、会社proxy、Windows/Linuxの今回sourceでのnative CI、実端末で30秒待つ表示確認は未実施です。

最終ビルド `32e3359` の同梱docsは、空cwd・環境変数なし・設定なし・owner/APIなしで33 CLI操作、13ガイド・3カテゴリ・117同梱リンクが成功しました。状態ファイルは作成しません。staged差分と全履歴の秘密情報検査は検出0件、公開文書の相対リンク493件も欠落0件です。下記のv0.1.2 CI/配布成功は当時のsourceの記録であり、今回の未リリースsourceの3 OS合格には読み替えません。

## v0.1.2の標準チェックとnative CI

対象は `77da7e3`（製品修正 `d3cfa9b` にREADMEのみを追加）です。macOS ARM64 / Rust 1.99.1で `sh scripts/check.sh` が終了0となり、fmt・strict all-target Clippy・全68群の1,083 passed / 0 failed / 15 ignoredを確認しました。標準チェック完了後にpushしました。

[3 OSのnative CI](https://github.com/tya5/shellboy/actions/runs/40418072576)もfmt・Clippy・全68群が成功しました。

| OS | passed | failed | ignored |
|---|---:|---:|---:|
| Windows x64 | 979 | 0 | 10 |
| macOS ARM64 | 1083 | 0 | 15 |
| Ubuntu x64 | 1084 | 0 | 14 |

Windowsのoverflow単独試験は別枠で成功し、上表へ二重計上していません。Issue #3のreadfree・期限切れ・照会統合、Quit受付後の起動抑止、macOS一時領域と管理画面の表示回帰を含むsourceです。

`d2d53de` のCI失敗は、readfree契約に対する古い期待値と、管理画面の補完情報・配列の`items`が欠ける製品の退行に分けて修正しました。読取りの`request_operation_id`は保存receiptが存在しないため表示しない仕様を維持し、mutation側の表示は残しています。先行失敗を今回の成功でなかったことにはしません。

一時DB・固定HTTP応答・合成データによる検証です。会社proxy・任意provider・Windowsの実IME/GUI・本物のtmpfiles経年清掃は再検証していません。既定の利用者DBを開く、移行する、ownerを停止する操作は行っていません。

## v0.1.2の配布とローカル更新

公開tag `v0.1.2` は `77da7e3` を指します。[配布workflow](https://github.com/tya5/shellboy/actions/runs/40419581339)でWindows x64・macOS ARM64・Linux x64のrelease buildと各OS上のhelp/version確認が成功し、[Release](https://github.com/tya5/shellboy/releases/tag/v0.1.2)を公開しました。全archiveとSHA256SUMSを認証なしの公開URLから取得し、HTTP 200、checksum一致、実行形式（PE AMD64 / Mach-O ARM64 / ELF x64）、README・MIT License・設定例・公開docsの同梱と、私的資料を含まないことを確認しました。

| 配布物 | bytes | SHA-256 |
|---|---:|---|
| Windows x64 ZIP | 9582492 | `e9073ae4861901a66d22b5991c50931f552dda7ee3f43228b553eb33987c7533` |
| macOS ARM64 tar.gz | 9171993 | `9d48f1c782bfdde030f07306fbfed811cf9e264ff2d8c7a1824880dc276f4a99` |
| Linux x64 tar.gz | 9916360 | `834a5d10785b32950a98b2f14be8a64aff67a4a366fda826319db2929af0f2a0` |

公開macOSバイナリをローカルへ原子的に配置し、配布版とのSHA-256一致、`shellboy --version`の0.1.2、helpの成功を確認しました。旧バイナリはローカルに退避しました。既定DBや設定を開く操作、実行中ownerの停止は行っていません。新バイナリは次回起動から使われます。

公開バイナリの同梱docsも、空cwd・環境変数なし・設定なし・owner/APIなしで33 CLI確認、13 guide・3 category・117同梱リンクが成功しました。バイナリSHA-256は `dfd87fc0cfb622f66459c0fe45f52ab7d66228758bc91f02ff1e314746dbd4d6` です。

schema v9へ更新したDBはv0.1.1で開けません。ロールバックは更新前の整合backupを別data-dirへ復元する方法を案内し、user_versionの手動引下げは勧めません。配布後の完了記録は文書のみの変更で、検証済みtagのコード・同梱guide・依存・workflowを変更していません。

## 初版のローカル標準チェック

更新: 2026-10-26。macOS / aarch64、固定Rust 1.99.1で、改名後の公開用ディレクトリから実施しました。この表のローカル標準検証対象は公開Gitの `8de8699` です。後続のプラットフォーム修正とGitHub CIは下記の別記録で区別します。非公開snapshot・過去検証asset・作業台帳を置かない構成を使っています。

| 確認 | 結果 | 手順・対象 |
|---|---|---|
| 固定依存でのビルドとCLI名 | 成功 | 全targetをビルド。実binaryの `shellboy --help` と `shellboy --version`（0.1.0）を確認 |
| fmt・全target Clippy・テスト | 成功 | `sh scripts/check.sh`。67テスト群、982 passed / 0 failed / 15 ignored。修正後の通し実行は約7分 |
| オフライン同梱ガイド | 成功 | `scripts/check-docs.py`、33 CLI確認、13 guide・3 category、同梱リンク115件。空cwd・環境変数なし・存在しない設定・owner/API不要、状態ファイルの作成なし |
| 新しい既定保存先・環境変数 | 成功 | config / CLI / runtime試験。新しい名前・OS既定path・保存先解決を確認。旧名・旧データ形式への互換は対象外 |
| 一時owner・CLI・正常終了 | 成功 | cli_process / runtime / tui_process試験。IPC接続、同ID再送、実shellからのCLI呼出し、PTYでの入力・固定HTTP応答・終了を確認 |
| TUI描画・スクロール | 自動試験成功 | TestBackendと自動PTY。実IME・OS clipboard・各端末GUIの再確認は今回未実施 |
| 実provider互換・意味検索の品質 | 今回未実施 | 通常試験はloopback固定応答・固定vector等。実API/性能/実環境のignored試験を合格数へ含めない |
| 公開用checkoutの独立性 | 成功 | 公開対象320ファイルのみで上記build/test。literal include 63件の欠落なし、旧名称残存なし、公開文書の相対リンク欠落なし |
| 公開Gitの全履歴 | 検査実施 | gitleaks v8.30.1、全ref・全履歴、allowコメント無効、redact有効。検出0件。非公開ファイル・旧履歴commitを含まない |

`CARGO_BUILD_JOBS=2`、`CARGO_INCREMENTAL=0` を指定し、ビルド生成物だけローカルcacheへ出しました。ソース・guide・migration・fixtureは公開用ディレクトリのものです。この表の実施時点ではGitHub Actionsはpush前でした。現在のGitHub実行結果は [Actions](https://github.com/tya5/shellboy/actions) を参照してください。実会話・資格情報・個人path・私的画像・生ログは検証文書へ含めません。

初回の通し検証では、名称変更で長くなった合成トークンの出力を19 bytesと正しく取得していましたが、テストに旧名の15 bytesという固定期待値が残っていました。期待値をtokenのbyte長へ変更し、該当回帰単独と上記の通し検証で成功しました。製品の出力処理は変更していません。追加のガイド検証scriptにも旧cursor期待値があったため、最終ページでcursorを省略する既存契約へ合わせました。

採用範囲は [適用仕様](../design/effective-spec.md)、現在地は [status](../development/status.md)、標準手順は [setup](../development/setup.md)、公開対象と履歴の扱いは [公開構成](../development/publication.md) を参照してください。実端末の追加確認は [手順](manual-terminal.md) と実施結果を区別します。

## 初回のネイティブCIとリリース検証

[初回CI](https://github.com/tya5/shellboy/actions/runs/40241942578) で、LinuxのClippyがUnix PTY試験の引数を拒否し、Windowsではstdin pipe作成中のC構造体がawaitを跨ぐためfutureのSend要件を満たしていないことが判明しました。前者はUnix共通4箇所をraw pointerで渡す形へ、後者はC構造体を同期block内へ閉じる形へ局所修正しました。プロセス操作順序・handle所有・待機期限・保存形式は変えていません。

修正後のローカルmacOS確認はPTY関連5件と対象strict Clippyが成功しました。Linux/Windowsの成功は再実行したネイティブCIで確認し、Releaseに対応する実行結果と分けて扱います。リリースworkflowはWindows x64・macOS ARM64・Linux x64のnative release buildと実行ファイルのhelp/version確認、配布物とSHA-256を作成します。

## TLS設定追加とWindows fixture修正

先行候補 `a67c162` の [native CI](https://github.com/tya5/shellboy/actions/runs/40243159749) はmacOS/Linuxで成功しました。WindowsはClippy成功後、管理UIの5試験でfixtureのcwdがOSの絶対パス条件を満たさず失敗したため、合格とは扱いません。fixtureだけをOS別絶対パスへ変更し、macOS上の管理UI6件は成功しました。Windowsでの確認は変更後のCIを使います。

接続ごとの `ssl_verify` 追加後、ローカル設定13件と既存HTTP12件が成功しました。合成自己署名HTTPSの2試験も成功しました。Responses・モデル一覧GET・Embeddingsについて、既定/明示trueの拒否とfalseの成功、他Connectionへ影響しないことを実要求で確認しました。課金API、利用者設定・保存データ、OSの信頼ストアは変更しません。変更後の3 OS CIの結果は下記「公開済みv0.1.0の結果」に記録します。

## 公開済みv0.1.0の結果

初版の製品コードは `1e581fc` の [native CI](https://github.com/tya5/shellboy/actions/runs/40245585424) で次を確認しました。全OSでfmt・strict all-target Clippyも成功しました。

| OS | suite | passed | failed | ignored |
|---|---:|---:|---:|---:|
| Windows x64 | 68 | 891 | 0 | 10 |
| macOS ARM64 | 68 | 986 | 0 | 15 |
| Ubuntu x64 | 68 | 986 | 0 | 14 |

Windowsの先行単独overflow試験1件は上表へ二重計上していません。添付/AGENTSのpath fixture失敗は解消し、TLS2件、native PowerShell5件、overflow単独と全体の成功を実ログで確認しました。

配布tag `v0.1.0` は `96e65bf` です。上記からの差分は配布workflowのversion検査と判断記録だけで、製品Rustソース・依存・同梱guideは同一です。最初の配布runはUbuntu 22.04のPython 3.10にtomllibがなくbuild前に失敗したため、固定Cargo metadata JSONによるversion取得へ変更しました。Release/assetの公開前に限り初期tagを更新し、旧commitと失敗runは履歴に残しています。

[配布workflow](https://github.com/tya5/shellboy/actions/runs/40246376176) は3 OSすべてでrelease build・実binaryのhelp/version・archive作成を成功させ、[Release](https://github.com/tya5/shellboy/releases/tag/v0.1.0) を公開しました。公開後に全配布物をダウンロードし、SHA256SUMSとの一致、実行ファイル・MIT License・設定例・guide・既知の制限の同梱、非公開資料を含まないことを確認しました。Windows ZIPは認証なしでHTTP 200/9,395,798 bytes、PEヘッダはAMD64でした。

| 配布物 | bytes | SHA-256 |
|---|---:|---|
| Windows x64 ZIP | 9395798 | `9756f870e25d3fc090681f25d90ff78fd5bfa92fd7a8f3d0c82eec10ba5946b2` |
| macOS ARM64 tar.gz | 8986811 | `164f313bc64d284076f48b75de21bae2f1ce54fe1c34a4cd4383c30a7ab0e897` |
| Linux x64 tar.gz | 9707525 | `c1ee749ae916fa036c8e95bc8f77f5db2f5df69bd314acdeb7f63438145b81f8` |

ダウンロードしたmacOS release binaryでもオフラインdocsの33 CLI確認/13 guides/3 categories/115 linksが成功しました。空cwd・環境変数なし・設定なし・owner/APIなしで状態ファイルは作りません。binary SHA-256は `8179b21d6c2ab60dc927cf16a239b9e39a4126710be247b57b3824ff12d02111` です。

### Windowsのテスト待機上限の追補

`96e65bf` の [再CI](https://github.com/tya5/shellboy/actions/runs/40246369812) はmacOS/Linux成功、Windowsは890 passed/1 failed/10 ignoredでした。前回成功したoverflow試験が全体実行時だけ再発し、追加診断では最初のshellがrunning・raw未到着のまま5秒を超過していました。overflow処理やsummary/retryへ進む前の待機であり、製品の5秒完了契約違反とは扱いません。

`6e45ac1` はWindowsに限りそのfixtureの待機上限を既存native shell試験と同じ15秒へ揃え、Unixの5秒・保存状態診断・全ての意味検証は保持しました。製品コード・guide・依存は配布tagと同一で、公開済みtag/assetは変更しません。[追補CI](https://github.com/tya5/shellboy/actions/runs/40247172429) も全3 OSで成功しました。fullは各68 suite、Windows 891 passed/0 failed/10 ignored、macOS 986/0/15、Ubuntu 986/0/14です。Windowsの先行単独1件は別枠で成功し、全体でも同じ回帰が成功しました。OS内部の起動遅延原因までは断定していません。

## ストリーム差分・受信本文保持の修正

初版公開後のローカル修正であり、上記v0.1.0の配布物とは別に検証します。会社Windowsの社内proxy・実モデルのSSEは未取得で、その環境の再現成功とは扱いません。固定応答のloopback HTTP、一時DB、UIのメモリ状態で確認し、資格情報・会社の会話・利用者の保存データは使いません。

回帰対象は、metadata/本文/ツール引数/推論の途中・最終差があっても検証済み最終responseを採用すること、deltaだけ異なる場合の保全、同chunkの後続不正JSONで先行受信本文を失わないこと、最終応答自体の不正は復旧待ちになることです。差分ログに本文を入れず、保存証跡を採用path・索引へ混ぜないことも照合します。

UIでは復旧待ちになった瞬間の本文保持、再起動後の復元、35件の保存記録を32件pageで辿って原文を取得すること、metadataだけの成功で重複表示しないこと、古いThread/世代の更新を採用しないことを確認します。GCでは生存Threadのpath外監査nodeを保持し、Thread削除後に失敗・成功の両監査本文とattempt内の回答/平文推論/完了item/最終rawを回収することを確認します。

修正source `d170da6` に対するmacOS/aarch64の `sh scripts/check.sh` は成功しました。fmt、strict all-target Clippy、全68 suiteの1,008 passed / 0 failed / 15 ignoredです。最終表示コードに対するstrict lib Clippyとfmt/diff再確認も成功しました。以前の受信表示full-lib確認ではattempt_id追加に伴う既存fixtureの期待値差が1件ありましたが、期待値へ同fieldを加え、キャンセル状態や旧保存値を守るassertを維持した上で今回のfullでも成功しました。

実binaryによるオフラインdocs確認は33 CLI/13 guides/3 categories/116 linksが成功し、空cwd・環境変数なし・設定なし・owner/API不要で状態ファイルを作らないことを確認しました。公開差分はgitleaks v8.30.1でallowコメント無効・redact有効で検査し、検出0件でした。会社Windows・社内proxy・実端末GUIでの再検証と実providerへの課金要求は未実施です。公開済みv0.1.0のtag/assetは変更していません。

## Issue #1: Windowsの隠し環境変数

`environment_block`の検査だけを継承環境の読込み規則へ揃えた局所変更です。macOS/aarch64で `cargo test --locked --test shell_windows_command` の4件が成功しました。`=C:`/`=::`/`=ExitCode`/`=ExitCodeAscii`と通常変数を混在させ、並び・上書き・二重NUL終端を含む環境blockの完全値を照合しました。空名、名前/値のNUL、先頭以外の`=`による注入を拒否し、診断の値漏洩防止・名前長制限・escape、値中の`=`と非Unicode UTF-16保持も確認しました。

`cargo clippy --locked --lib --test shell_windows_command --test shell_windows -- -D warnings`、全体fmt/diff確認が成功しました。前節の全1,008件は本修正前の結果であり、この差分では対象試験と対象Clippyを実施しています。PowerShellが隠し変数入りの環境blockで起動・正常終了するWindows専用回帰を1件追加しましたが、macOSではcfg対象外のため実行成功に含めません。親processの環境や利用者データは変更していません。Windowsでの起動確認、push、新配布物は未実施です。

## Issue #2: XDG設定不要のUnix IPC

macOS/aarch64で `cargo test --locked --lib --test app_ipc --test cli_process --test runtime --test runtime_hierarchy` が全5 suite、430 passed / 0 failed / 3 ignoredで成功しました。内訳はlib398、app_ipc3、cli_process4、runtime21、runtime_hierarchy4。OSが先に一時領域を回収した場合の正常清掃も含みます。直前の初回lib397件は最終430件へ二重計上していません。全targetの標準チェックを繰り返した結果ではなく、IPC・起動・Control/shellの変更対象とライブラリの検証です。

保存先の末尾slash付きsymlinkを追ってchmodしない最終調整後、専用subprocessでumask002・XDG_RUNTIME_DIRなし・group書込み可の親を組み合わせた起動/正常終了、保存先0700、新規/既存保存先の扱い、親権限の不変、symlink先の権限不変を1件の対象試験で追加確認しました。この再実行は430件へ加算しません。最終調整後の対象strict Clippyも成功しています。

旧locatorしかない段階・空directory・bind済みsocketのクラッシュを模擬し、再起動で旧領域を回収し新しいランダムpathになることを確認しました。これはDB/診断workerを停止してmetadataを残す合成fixtureで、実OSの強制終了や電源断の網羅ではありません。他の生存owner、未知file、socket/parentの置換、symlinkを保全する試験も成功しています。

実CLI subprocessではendpoint.jsonを削除した状態でもSHELLBOY_SOCKET経由で接続し、別data-dirへの明示切替ではそのmetadataへ接続、不正instanceではprotocol_mismatchとなりfallbackしないことを確認しました。実モデルtool相当のshellへsocket環境が伝わり、そのshellからCLI操作・子Run・待機が動く既存回帰も成功しました。APIはローカル固定応答で、課金要求や利用者データは使いません。

peer照合は実Unix socket pairのcredentialsを使い、同uid成功と期待uid不一致の拒否・protocol byte未送信を確認しました。実際に別uidへ切り替えたprocessやLinux SO_PEERCREDの実行は未実施です。対象strict Clippy、fmt/diff、実binaryの同梱docs33 CLI/13 guides/3 categories/116 linksは成功しました。Linux/WSL実機・公開CI・新配布物は未実施であり、macOSの合成条件をそれらの成功とは扱いません。

## Enter直後の送信表示と診断（2026-10-26）

macOS / aarch64、Rust 1.99.1で、一時DB・合成メッセージ・loopback固定応答を使いました。UI送信の7回帰は、Enter直後の描画/時計、保存とsource/履歴の順序逆転、複数revision、受付拒否時の入力保持、会話切替、窓外の完了済み送信、復旧時の静止を確認します。frontend試験は実送信のqueued/processing/savedと保存receiptのoperation ID/message IDを照合し、本文がログに入らないことを確認します。診断の2追加試験は固定分類・IDの上限/制御文字除去も確認します。

最終ソースの `cargo test --locked --lib --test frontend --test frontend_activity --test frontend_ordered --test diagnostics --test tui_recovery --test tui_process --test tui_shutdown` は8 suite、441 passed / 0 failed / 3 ignoredでした。対象のstrict Clippy（`-D warnings`）、fmt/diff、同梱docsの33 CLI確認・13 guides・3 categories・116 links、staged秘密情報スキャンも成功しました。

今回の確認には会社のWindows・実端末GUI・実provider APIを含めず、標準全targetチェックや公開版のCI結果と区別します。利用中owner・既定DB・公開済み配布物は変更していません。

## v0.1.1候補のnative CIと終了待ちfixture（2026-10-26）

候補 `7c888db` の [native CI](https://github.com/tya5/shellboy/actions/runs/40332152109) はLinux/macOS各68 suite・1,025 passedで成功しました（ignoredはLinux14、macOS15）。Windowsはfmt・Clippy・overflow個別試験を通過しましたが、既存のstale_mutation_report試験が最後のbridge終了待ち5秒でtimeoutしたため不合格です。操作結果・古いreport抑止・通知所有のassertは完了し、他のtargetも成功しました。

fixtureの終了待ちは製品既定の停止猶予5000msと同じで、DB・IPC・ログworkerの終了までを含む外側期限として余裕がありません。Windowsの試験用期限だけ30秒へ延長し、busyなyieldポーリングを5msの待機へ変更します。製品の停止期限・強制終了・保存処理は変更しません。ログから停止段階は確定できないため、原因を期限不足と断定せず、修正後のnative CI成功で区別します。

## 公開済みv0.1.1の結果（2026-10-26）

tag `v0.1.1` は `e55da58` です。[修正後のnative CI](https://github.com/tya5/shellboy/actions/runs/40333964684) は全OSでfmt・strict all-target Clippy・全68 suiteが成功しました。Windowsのoverflow個別試験は下表へ二重計上していません。

| OS | suite | passed | failed | ignored |
|---|---:|---:|---:|---:|
| Windows x64 | 68 | 925 | 0 | 10 |
| macOS ARM64 | 68 | 1,025 | 0 | 15 |
| Ubuntu x64 | 68 | 1,025 | 0 | 14 |

Windowsの先行候補の終了待ちfixture失敗は上記成功と区別します。修正後の試験は正常終了まで成功しており、製品の停止猶予・強制終了仕様は変更していません。

[配布workflow](https://github.com/tya5/shellboy/actions/runs/40335468427) は3 OSのnative release build・help/version・パッケージ生成を成功し、[v0.1.1](https://github.com/tya5/shellboy/releases/tag/v0.1.1) を正式公開しました。全3 archiveとSHA256SUMSを認証なしの公開URLからダウンロードし、SHA-256一致、単一prefix配下の構成、必要文書、各OSの実行形式を確認しました。公開macOS版をこのマシンでも実行し、help/version、同梱docsの33 CLI確認・13 guides・3 categories・116 linksを確認しました。同じ実行ファイルをローカルへ配置し、配置後versionも0.1.1です。

会社のWindows・社内API proxyによる実provider互換、実IME/clipboard/端末GUI、WSL実機の再検証は今回の成功に含めません。既定設定・履歴・実行中ownerはリリース検証で変更せず、v0.1.0のtag・配布物も保持しています。配布ソース以降の文書commitはこの完了記録だけで、検証済みtagを移動しません。

## レビューgoal再開後のローカル検証（2026-10-26、未配布）

公開v0.1.1から後続のレビュー是正を進めています。全て一時DB・匿名fixture・固定応答で、利用者の既定DBや設定を変更しません。旧レビューの証拠監査はローカル専用Gitに置き、ここには製品の新しい検証だけを記録します。

- 索引選択維持と巨大rawの第一段階はindex49件・rag_store16件・raw decoder/failure分類の単体各1件、対象strict Clippyが成功しました。
- 続く停止予算・query fingerprint・末尾cursorの4 suiteはcontrol_db20件・index51件・rag_store16件・work_status11件が成功しました。停止の試験はDB予約と保存/再送経路を検証し、OS processの停止を実行した証拠とは分けます。
- 検証済みquery markerを再読取時に書き直さない補強後、lib407件（既存ignored3）・index51件・rag_store16件と、対象strict Clippyが成功しました。SQLite trigger/total_changesでmarkerの再書込と不一致時の破壊を拒否することを確認しました。

G7の可視性判定は本番と同じsearchable SQL・bundled SQLite・専用DB workerを使い、256sourceと共有置換深さ0/8/64、live rootあり/なしで計測しました。順に6,961/8,237、20,529/20,916、162,735/159,950µsでした。これは1マシンの診断標本であり、全検索のlatency保証や実データ性能の証明ではありません。追加の常駐reachability cacheやworkerは導入していません。

ここまでは10月26日時点の段階検証です。最終コードの確認は次節へ記録します。Windows/Linuxのnative検証は今回のローカル結果に含めず、v0.1.1の過去CIと区別します。今回の修正はpush・新配布物・ローカルインストールへ反映していません。

## 最終コードの実端末・負荷確認（2026-10-27、未配布）

`f488196` の単体試験は428 passed / 0 failed / 3 ignoredでした。正式な親子終了記録のkind・詳細参照が一般fieldsの上限で落ちる条件も回帰へ追加しました。以降の確認はmacOS ARM64、Rust 1.99.1、release、専用一時DB・loopback fixtureです。利用者の設定・履歴・インストールは変更していません。

実WezTermの120×40・60×24セルを取得しました。復旧理由と操作案内から `/retry` へ進み、同じTurnのgeneration 0/incomplete→1/completedをDBでも確認しました。実shell2件は番号とcommandが対応し、出力ONE/終了0とエラー側TWO/終了7を区別できました。親子処理の終了（6f8f78a）では両shellのstopped・group_cleanup_confirmedを確認し、f488196で開き直した5詳細＋summaryが一つの「原文6件」へまとまること、F6の25項目には各詳細が残ることを確認しました。別の静的scene（6f8f78a）では中断・構造化error・索引・code caption、全8項目とO原文まで確認しました。foreground処理の途中stdoutを待つ補助手順はtimeoutし、正常終了処理後の保存出力で確認しています。画像取得はscreencapture失敗とScreenCaptureKit -3811で未実施です。これらは実端末セルの確認であり、色・pixel描画・実IME/clipboardの合格とは扱いません。

同じsourceのrelease測定は他のCargo・試験・captureを止めて実施しました。1024 delta／80,908byteのSSEを3回、製品の32件/4ms更新経路と120×32・30fps TestBackendで処理しました。snapshotは73〜74回、clone総量は2.69〜2.73MiB。source送信からpump後の表示観測までのp95は10.45〜11.75ms、Paste→TestBackendの21標本は1.82〜37.97ms、DB往復21標本の最大は1.05msでした。表示観測間のskipは更新の集約を含み、通知配送件数や純粋なqueue待ち時間ではありません。保存確定本文も確認しました。

実release binaryのPTYでは空の新規DB／同DB再openを各7回、120×32で測定しました。起動p50/p95は新規41.16/41.59ms、再open41.17/42.06ms、日本語bracketed pasteのp95は各36.31/37.73msでした。約2秒idleのRSS観測最大は17,776KiB、CPU区間値は0〜0.50%でした。マシンは8core/8GiB、開始時load averageは139.12/117.83/89.56。OS cacheは消去せず、履歴・model・embedding未設定の限定条件です。旧baselineとの非退行、大規模履歴、SLO、pixelや物理入力の性能は主張しません。

最終コードf488196の `sh scripts/check.sh` はfmt・全target strict Clippy・68群の試験が成功しました。1,058 passed / 0 failed / 15 ignoredで、上記の単体428件を含みます。明示したrelease負荷試験は別枠です。`python3 scripts/check-review-plan.py --require-complete` も成功し、178 original＋22 PR1＋19 PR2は217完了・Windows実機なし2環境対象外、396子条件の対応を確認しました。文書検査だけを製品の検証とは扱いません。Linux固有lease/実tmpfiles、Windows GUI、会社proxy、実providerは今回未実施です。

## v0.1.2タグ前の追補（2026-10-27）

文書commit `dcdbab3` でも `sh scripts/check.sh` を実行し、fmt・全target strict Clippy・68群 1,058 passed / 0 failed / 15 ignoredを確認しました。先行する `f488196` の結果と同数ですが別の実行です。この後のバージョン更新・再レビュー修正の成功へ読み替えません。

最初の準備commit `2cec582` の [native CI](https://github.com/tya5/shellboy/actions/runs/40406278067) はmacOS/Linuxが成功、WindowsはUnix専用テスト補助関数のcfg漏れによるstrict Clippy失敗でした。Windowsテストは未実行であり、成功に含めません。テスト側cfg修正と受信表示・raw検証・起動診断の追加修正は、後続commitの検証結果を別途記録します。
