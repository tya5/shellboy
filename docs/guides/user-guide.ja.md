# User Guide

[ガイド一覧](README.md) · [設定・導入](configuration.md) · [画面操作](tui.md) · [English](user-guide.md)

Shellboyは端末で会話し、モデルにshell作業を依頼するアプリです。会話の保存先を管理するTUIを一つ起動し、外部の`shellboy` CLIもその起動済みアプリへ接続します。会話には接続先のResponses対応proxy、モデルID、資格情報を設定します。ガイドは設定前でも `shellboy docs list` から閲覧でき、文書検索の `shellboy rag` も独立して使えます。

## 会話と作業状態の読み方

| 通常画面で読む言葉 | 意味 | 詳細で対応する内部情報 |
|---|---|---|
| 会話 | 履歴・作業フォルダー・モデルをまとめた保存先 | Thread。親子の会話は履歴の系譜で、実行の所有関係とは別 |
| 依頼 | 送った本文。受付済みなら保存されている | message。accepted/held/appliedは回答の成否ではない |
| 処理中 | 準備、モデル応答、ツール実行や明示待機を進めている | Runは作業の寿命、Turnは次の回答までの処理単位。attemptは個々の通信 |
| 背景処理 | 回答後もshellや所有する子の処理が続いている | 完了したTurnとliveなRunは両立する。会話切替でも処理は継続 |
| 復旧待ち | 理由を保存し、再試行・新指示・停止の判断を待っている | Turnのrecovery_wait。Runはliveのまま、時間経過だけでは再開しない |

会話を選ぶ操作、現在の作業を止める操作、過去の結果を詳しく読む操作を区別します。
「受付済み」は保存の成功、「回答完了」はTurnの完了で、背景処理を含む作業全体の完了とは限りません。
IDやreceiptは調査の詳細情報です。普段は状態と理由を読み、必要なときに詳細から取り出せます。

復旧待ちでは保存済みの依頼を送り直す前に、理由と対象Runを確認します。
原因を直して同じ処理を試すにはF8のRun再試行か `shellboy run retry --run RUN_ID`、
やめるにはCtrl+Cか `shellboy run stop --run RUN_ID` を使います。
人間の新指示で同じ復旧Turnを再開することと、元の処理をretryすることは別です。
停止・再起動後の終端Runをretryで復活させることはできません。
保存済み内容と未確認の副作用を確認する手順は [復旧ガイド](recovery-and-storage.md) にあります。

次の区切りを待つ保存済みの入力は、入力欄の上と `/queue` に表示されます。CLIや別の会話から届いた入力も対象です。通常の送信は区切りまで待ちます。Ctrl+Enter（通常送信に設定済みなら除く）またはCtrl+X、Ctrl+Sの順で「すぐ渡す」を選べますが、応答の生成は中断しません。Escは画面を閉じる操作を優先し、通常の会話画面ではTurnだけを中断します。Ctrl+CはRunと、その背景の作業も停止します。未反映の入力は `/queue` で確認したIDだけを取り消せます。保存した入力は `/restore list` と `/restore draft ID` から復元でき、編集中の新しい入力は保持します。上限と確認操作は [TUI controls](tui.md) を参照してください。

## 1. 作業フォルダーで起動する

作業中の処理をまとめて見るにはTUIで`/status`、別の端末で`shellboy status`を使います。background shell、子Run、依頼の保留、モデル要求を保存済みIDから追跡できます。終了済みも見る場合は`/status all`又は`shellboy status --all`です。詳しくは[画面操作](tui.md#slash-commands)と[CLIの作業状態](cli-reference.md#common-syntax-and-connection)を参照してください。

まず[環境別の起動方法](configuration.md#starting-on-each-platform)で、端末・OS版binary・モデル用shellの組合せを選びます。[設定・導入](configuration.md) に従ってビルド・インストールと接続設定を行った後、ターミナルで次を実行します。例はBash/zsh用です。

```sh
command -v shellboy
shellboy --version
cd /path/to/project
shellboy
```

`/path/to/project`は実在する作業フォルダーへ置き換えます。短い名前がPATH上で見つからない場合は、インストール先のディレクトリを利用者のPATHへ追加するか、binaryを絶対パスで呼んでください。モデル用shellのCLI探索にはShellboyがPATHを自動設定します。接続先と資格情報の設定は [設定・導入](configuration.md) を参照してください。ソースcheckoutからのビルド手順は[開発環境](../development/setup.md)にもあります。

設定と保存先を明示する場合は次の形です。

```sh
shellboy --config /path/to/config.toml --data-dir /path/to/shellboy-data
```

引数なしの起動には対話端末が必要です。同じdata-dirではownerを二重起動できません。別ターミナルからのサブコマンドは起動済みownerを使い、ownerを自動起動しません。独立した環境が必要なら別のdata-dirを選びます。

起動中は設定・DB更新・前回中断した処理の整理など、現在の準備内容を表示します。会話入力は起動完了後に行います。Esc / Ctrl+C / Ctrl+Qで終了を要求すると、開始済みの初期化と保存処理を安全な境界まで進めてから閉じます。DB更新を巻き戻したり、直ちにlockを解放したりする操作ではありません。起動に失敗した場合は表示された理由を確認し、EnterかEscで閉じます。

新しいルートThreadのcwdは起動フォルダーです。保存済みThreadを開くと、そのThreadのcwdを使い、今回の起動フォルダーで上書きしません。`/cwd`、又はF8の「会話 · 詳細」で確認できます。

## 2. 最初の依頼を送る

下書き欄に依頼を入力し、既定ではEnterで送信します。Ctrl+JまたはShift+Enterで改行します。例えば「このフォルダーのREADMEを読み、構成と次の作業候補を説明してください」と依頼できます。モデルにはshellツールがあるので、実ファイルを変更する依頼はその作業フォルダーで実行され得ます。

初回送信でThreadと依頼が保存されます。「受付済み」はDB保存を表し、回答・shell・Runの完了とは別です。受付後の準備や通信に失敗した場合も、保存済み依頼は残ります。失敗内容に従い対処してから明示的にretryします。[復旧・保存](recovery-and-storage.md)を参照してください。

受付完了までは本文・添付を表示し続けます。この間の編集は送信時点のsnapshotには反映されず、遅れて届く受付結果も編集後の下書きを消しません。キュー満杯や保存失敗の表示が出た場合は、本文を確認してから再操作します。長文貼付けは自動送信されず、[折りたたみ表示](tui.md#pasting-long-text)から確認できます。

## 3. 会話を切り替える

- `/new`またはCtrl+Nで新しい下書きを作ります。最初の送信前はまだ保存済みThreadではありません。
- `/threads`またはCtrl+Tで1行ずつの一覧を開き、上下キーとEnterで選びます。左右キーで親子を開閉・移動し、文字入力で絞り込みます。選択した項目の詳細と短い会話抜粋は下部、完全値はF1から確認できます。
- IDが分かる場合は`/switch THREAD_ID`で移動できます。

Threadごとに下書きと閲覧位置を持ちます。切替は実行停止を意味しません。前のThreadでshellや子Runが動いていれば処理を続けます。回答後も背景処理が残ると、Turnは完了していてもRunは継続する場合があります。Thread・Run・Turnの違いは[委譲](delegation.md)を参照してください。

通常の会話は発話・実行結果・状態変更を種類別に表示します。完全なIDや元の値を調べるときはF6で項目を選んで詳細、Oで原文pageへ進みます。F7はリンク一覧と完全URLのコピーです。F8では「この会話で」「よく使う」「すべての操作」を表示します。人向け名・CLI名・関連するslashで絞り込み、対象IDや引数をfieldごとに入力して履歴・Run・shell・索引・保存へ進めます。結果はfieldと値で読み、必要な部分を詳細pageで取得します。[画面操作](tui.md#read-source-text-and-links-from-details) にキーとコピー範囲をまとめています。

履歴を読むと末尾追従を止め、「新着あり」を表示します。Ctrl+Endで末尾追従へ戻ります。フォークや依頼編集、検索filterと出典・直接の関係の確認は[履歴と検索](history-and-search.md)で説明します。

## 4. 接続・モデルを変える

`/model`だけで現在値を表示します。`/models`で現在の接続先のモデル一覧を取得できます。文字入力で絞込み、上下で選択、Enterで現在の会話のモデルを変更します。Escは閉じるだけです。別の接続先を見る場合は `/models CONNECTION` を使います。直接指定する場合は以下です。

```text
/model CONNECTION MODEL_ID
/effort medium
/cwd /path/to/other-project
```

`CONNECTION`と`MODEL_ID`は実際の設定値へ置き換えます。一覧は接続先から取得し、モデルや対応effortをShellboyが内蔵しているわけではありません。接続先がembedding等も列挙した場合は一覧に含まれ、会話APIで使える保証ではありません。取得未対応の接続先では `/model CONNECTION MODEL_ID` で直接指定できます。`/effort none`はeffort指定を解除し、モデルへ`none`を必ず送る指示ではありません。モデル変更でも既存effortは保持するので、必要なら明示的に変更・解除します。

変更は以後の要求に反映され、既に送信済みの通信を書き換えません。接続・モデル変更では推論の送信epochも変わり、元モデルへ戻しても古いopaque reasoningを自動復活しません。会話原文は残ります。

## 5. 子へ委譲する

「サブエージェントを使って」と言うだけで、モデルがShellboyの子Thread操作を正確に理解する保証はありません。Agent Guideは自動注入されません。checkoutにある実在する絶対パスを指定して、まず読ませます。

```text
まず /path/to/shellboy/docs/guides/agent-guide.md と
/path/to/shellboy/docs/guides/delegation.md をshellで読んでください。
その手順で所有する子Threadを作り、調査だけを委譲してください。
子の依頼を送った後は割当と完了状態を確認し、原文を取得して結果をまとめてください。
```

binaryだけを別フォルダーへ置いた場合、docsが一緒に存在するとは限りません。ガイドの実際の保存先へ書き換えてください。親子の停止範囲、途中報告、recovery待ちの判断は[委譲ガイド](delegation.md)にあります。

Threadの`AGENTS.md`参照先は作業上の指示です。新しいルートThreadの既定はcwdの`AGENTS.md`で、新Turnの準備で読み、同じTurnのretryは保存済みsnapshotを使います。Agent Guideは操作の説明書、リポジトリ直下の`AGENTS.md`はそのリポジトリの作業指示で、用途が異なります。

## 6. 中断・終了する

Ctrl+Cは現在Threadの中断、Ctrl+Bは現在のforeground shellを背景へ移す操作です。背景化では対象shellを継続し、同じgroupに未実行後続callがあれば取消しにします。画面を切り替える操作とは区別してください。 背景化後は対象shellと後続の扱いを画面へ通知し、モデルにも対応する結果を渡します。後続がない場合は存在しない取消しを表示しません。中断は保存済み状態に従って停止処理中から中断済みへ変わり、背景化や完了と区別されます。

処理の完了を観測するwaitと、対象を中断する操作は別です。F8の待機フォームから開始した観測だけをやめるなら、結果画面で小文字w、又は `/cancel-wait` を使います。対象Run/shellは止まりません。フォーム内ではEnterが実行、複数行の本文入力はCtrl+J、候補値の置換はCtrl+Uです。

終了はCtrl+Qまたは`/quit`です。保存と所有する処理の停止・回収を待ちます。猶予を超えるとW/Enterで待機継続、Fで強制終了の確認へ進み、さらにYで確定します。強制終了は未保存・未回収の可能性を伴います。詳細は[復旧・保存](recovery-and-storage.md)にあります。

次に使う機能は[添付](attachments.md)、[履歴と検索](history-and-search.md)、[全CLI参照](cli-reference.md)から確認できます。

削除前にF8の「会話 · 削除影響」で、動作中Run・子孫・共有や出典・添付参照を確認できます。照会だけでは削除せず、容量の回収見積もりは未対応です。受付や圧縮結果を見失った場合はF8の「操作 · 受付結果」で対象operationを照会し、後続状態も確認します。詳しくは[復旧・保存](recovery-and-storage.md)を参照してください。

## 保存済み使用量の確認

F8の「使用量 · 合計」で、保存されたモデル要求の使用量とRun・モデル要求・shellの時間を別々に確認できます。子会話を含めることもできます。
CLIは `shellboy usage get --thread THREAD_ID --include-children`、または子を含めず `--run` / `--turn` を指定します。既存のgetにも `usage_totals` が付きます。
未報告の値を0とはみなしません。summary・embeddingの未記録はcoverageに明示します。実行中の読取時点の値は完了分と別で、重複する3種類の時間を実働時間として足しません。
詳細は開いた時にDB workerで読みます。変更通知は前回結果後3秒間まとめ、既存の500ms UI tickで1件だけ再照会します（通常3.5秒程度＋worker/DB待ち）。表示はsnapshotで、描画中は再読しません。
`<data_dir>/logs/shellboy.log` の `event=usage_totals_slow` と `usage_slow.trigger/scope/elapsed_ms` で、固定100ms以上の集計だけを確認できます。高速な集計の診断は出さず、UI描画時間ではありません。
