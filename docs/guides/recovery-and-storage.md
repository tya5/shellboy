# 復旧・停止・ストレージ

[ガイド一覧](README.md) / [User Guide](user-guide.md) / [Agent Guide](agent-guide.md)

返事が止まったときは、同じ依頼を繰り返し送る前に保存済みの受付・Turn・Runを確認します。CLIは本体を起動したまま別の端末から使います。以下はBashの例で、IDは実際の応答へ置き換えます。

## 受付と完了を分けて調べる

```sh
shellboy thread list
shellboy thread get --thread THREAD_ID
shellboy message get --message MESSAGE_ID
shellboy turn get --turn TURN_ID
shellboy run get --run RUN_ID
```

| 観測対象 | 分かること | 取り違えないこと |
|---|---|---|
| CLIの `ok: true` | 指定操作の受付や照会が成立した | モデル回答・shell・索引処理の成功とは別 |
| messageの `accepted` / `held` | 本文は保存済み / 保存済みで実行保留 | 同じ本文を新規送信して重複させない |
| messageの `applied` | モデル入力へ反映された | 回答が完了したとは限らない |
| messageの `cancelled` | その指示は取消扱いになった | 原因を調べて次の指示を判断する |
| `assignment: pending` | Run/Turnはまだ未割当 | IDを推測せず `message get` で確認する |
| Turnの `completed` | 通常回答が確定した | 背景shellや子Runが終わったとは限らない |
| Runの `live` | 実行寿命が続いている | 回答前だけでなく所有する背景作業待ちも含む |
| Turnの `recovery_wait` | 失敗理由を保存して明示判断を待っている | 時間経過や設定変更だけでは再開しない |

回答は `answer_node_id` が返されたら `shellboy history get --node NODE_ID` で取得します。長い本文は `next_cursor` を使って続きも読みます。状態が終端でも `failed` / `stopped` を成功回答と扱わないでください。

TUIではF8の「待機 · any条件」から開始し、管理画面の小文字w又は `/cancel-wait` で観測だけを取消せます。Escで画面を閉じても対象を停止しません。Ctrl+Cは現在Threadの実行中断なので区別してください。[待機の画面操作](tui.md#待機の取消しと対象の中断を区別する) を参照してください。

実行待ちには `shellboy wait` を使えます。`timeout` は対象の停止や失敗ではありません。`satisfied` も対象の成功とは限らないため、同じ返却値のsnapshotと状態を確認します。[委譲](delegation.md) と [CLI reference](cli-reference.md) にJSON形式と状態別の分岐があります。

## 一時的なモデル通信失敗の自動再試行

一時的な通信失敗が続く場合、既定で連続8回まで自動で追加試行します。TUIの「一時的なエラーのため再試行中」は待機中の通常表示です。Ctrl+CやRun停止で待機を取り消せます。上限に達すると最後のエラーと回数を示して復旧待ちになります。最終responseの採用成功で連続失敗回数をリセットするため、tool処理を含む長い会話全体で8回という制限ではありません。

対象はHTTP429・500・502・503・504と一時的な接続失敗・通信切断・EOF・受信停滞timeoutです。既定では部分出力があっても再試行し、受信済み本文は失敗attemptへ保全します。最終responseの採用前のtool callは実行しません。認証・設定・形式・明示provider失敗などの恒久エラーは自動再送しません。HTTPエラー本文はモデル出力とは区別します。

接続別の `response_max_retries = 0` で自動再試行を無効化できます。`response_retry_partial = false` は部分出力がある失敗だけを復旧待ちにし、metadataやheartbeatのみで出力がない通信失敗は再試行します。指数待機上限は `response_retry_max_delay_ms`（既定60000）、サーバーの `Retry-After` は別の固定10分上限です。[設定](configuration.md)を参照してください。上流で処理が進んでいた場合、再試行によって料金が重複する可能性があります。失敗attemptには回数・待機・この可能性を保存します。自動再試行が終わった復旧待ちは、時間経過だけでは再開しません。 Retry-Afterが600秒（10分）を超える場合、600秒に短縮して再送せず、指定待機時間を保存・表示して復旧待ちにします。

## 送信保存に失敗した場合

初回送信の保存に失敗した場合、本文は下書きへ残ります。入力上限やqueue満杯等なら原因を解消して再送できますが、正本DBの `save_failed` は新規受付を停止するため、その場で再送を繰り返しません。画面に残る未保存本文があれば終了前に退避し、停止・終了後に保存先の原因を直します。再起動後は実際に保存できていた下書きと受付を確認してください。RAMだけに残った本文の再起動後の復元は保証できません。保存済み受付の後でprepareやAPI開始に失敗した場合は、元のmessage/Run/Turnを調べて復旧します。新しいmessageを作ることと、元のTurnをretryすることは別です。

遅い応答が次の下書きを消さないよう下書きはrevisionで区別されます。失敗snapshotを復元する `/restore REV` は [画面操作](tui.md) を参照してください。

CLIの返答だけを見失ったときは、既知のoperation IDがあれば先に照会します。

```sh
shellboy operation get --operation OPERATION_ID
```

送信後に接続が切れた場合は、返却JSONの `operation_id` と
`error.details.operation_id` に照会対象IDが残ります。IDを自分で指定していなかった場合も
この自動生成IDを使います。`request_may_have_been_applied: true` は結果不明という意味で、
実行済み・未実行・失敗のどれも断定しません。`next_operation` は受付照会への型付き案内です。
IDの文字列を新しいコマンドとして評価せず、`--operation` の引数へ渡します。

プロセス自体を終了した場合など、返却JSONを取得できずIDも不明な場合は、この案内からIDを復元できるとは限りません。既知のThread/Runと `status --all`、保存履歴を確認し、自動再送しないでください。

`operation_not_found` / `operation_expired` なら対象get・保存履歴と照合します。
`operation_conflict` なら既存のreceipt内容と要求の違いを調べます。いずれも新IDで同じ
副作用を繰り返す理由にはしません。`save_failed` なら未保存入力を保持して保存障害を調べ、
新規mutationを繰り返さないでください。

同じinstance内で同operation ID・同じ要求を再送すると、受付を重複させず保存済み結果を返します。同IDで内容を変更すると `operation_conflict` です。再起動後の旧IDも、同じ要求の保存済み結果が残っていればその結果を返し、副作用は再実行しません。旧IDの記録がなければ `operation_expired` です。結果不明のshell入力等を新IDで無条件再送しないでください。

`operation get` の `targets` から対象のgetへ進みます。`downstream` はmessageの現在配送状態、またはcompactのqueued/running/completed/failedと終端outcomeを返します。受付の `state=completed` と後続処理の成功は別です。`result_uncertain` や `replay_policy=do_not_reexecute` があれば、結果不明の副作用を安全に再実行できるとは判断しません。照会自身にはreceiptを保存せず、同じ照会IDでも現在値を確認できます。

get系の `context.recovery_reason` は構造化された理由です。再起動中断の `result_uncertain=true` は失敗確定ではなく、復旧待ちもfailureと同義ではありません。保存された時刻と所有を読み、未保存のstarted_atや全状態遷移を推定しません。Run/Turnの `lifetime_ms` は待機込みの作成から終了までで、未終了時は省略されます。

## 前回終了時の記録を見る

背景処理が残っていた会話を再起動後に開くと、履歴に「前回の終了時の記録」と作業・応答処理・コマンド・子・中断した待機観測の件数を表示します。F6で同名の記録を選び、Enterで対象IDや理由、Oで保存原文を確認できます。「前回の保存記録 · コマンド」「子の作業」「前回の待機観測は中断済み」から個別の対象へ進めます。参照したRun/Turn/shellは各getやF8で現在の保存状態を照会してください。

終了時の記録は停止処理を始める前のsnapshotです。「予定した状態」と実際の回収結果を区別し、外部副作用の結果が不確実ならそのまま表示します。通常Ctrl+Qで終了したことだけで、すべての外部処理の結果が確定したとは扱いません。再起動後に以前の処理を自動実行せず、保存した対象・回答・未反映の依頼を確認して次の操作を選びます。

## 復旧待ちを再開する

TUIでは原因と未反映指示を会話の下に表示します。別の照会結果や設定変更でも、この原因は消えません。`/retry` は現在の復旧待ちを同じ内容で再試行し、通常の入力送信は新しい指示で再開します。`/stop` またはCtrl+Cは現在の会話の作業を停止します。F8でRun/Turnの詳細を確認でき、再試行・停止フォームの既定値は現在の作業です。古い管理結果のRun IDを引き継ぎません。

復旧待ちの時間はモデルが処理を続けている時間ではなく、人の操作待ちです。保存記録から失敗段階が分からない場合は未記録と示します。確定済みツール結果は利用しますが、未確認の副作用を再実行しない保証とは区別して詳細を確認してください。

1. `turn get` / `run get` / `thread get` の復旧理由やhintを読む。
2. 原因を解消する。例：参照先ファイルを直す、接続設定やモデルを確認する、容量を回収する。
3. 同じ生存Runの復旧Turnを明示retryする。

```sh
shellboy run retry --run RUN_ID
```

外部人間CLIまたは直属の所有親が復旧を判断します。祖先であるだけのモデルや、別の独立チャットには同じ権限はありません。親から普通のmessageを送るだけでretryにはなりません。人間の新しい指示が同じ復旧Turnを明示再開する経路もありますが、原因を直した元Turnの再試行には上のコマンドが明確です。

`run retry` は終端Runを復活させません。停止・再起動後に新しい作業を始めるときは、新しい指示を送って新Run/Turnを作ります。再起動時に古いPIDを使って停止したり、shellの副作用を自動で再実行したりはしません。前回の未確認状態は履歴・再開時のcontextとして扱います。

明示されたcontext容量超過だけが自動圧縮・最大2段階のretry対象です。不明な通信失敗、byte上限、proxy障害をcontext超過と推測して自動圧縮することはありません。手動の `/compact` は [履歴と検索](history-and-search.md) を参照してください。

## 中断と終了

| 操作 | 意味 |
|---|---|
| TUIでCtrl+C | 現在のRunへ停止を要求する |
| `shellboy run stop --run RUN_ID` | 指定Runとその所有配下の停止を受け付ける |
| `shellboy shell stop --execution EXECUTION_ID` | 指定shellへ停止を要求する |
| 外部CLIのwait中にCtrl+C | その観測を切断する。対象Run/shellは継続する |
| TUIでCtrl+Q | 全体の停止・保存・DB終了を進めて通常終了する |

停止受付の成功は全PID回収完了と同義ではありません。必要ならRun終端を待ち、状態を照会します。モデルが停止できるのは自Runと現在所有する子孫、およびそのshellです。独立Threadへの依頼を送れたことは、その実行を停止できることを意味しません。

通常終了の猶予を過ぎると、TUIは待機継続か強制終了を選べるようになります。WまたはEnterで同じ終了処理を待ちます。F→Yで明示強制終了、確認中のN/Escは強制終了を取り消します。強制終了では未保存・配下の未回収が残る可能性があり、正常終了とは区別されます。

正本DBの保存障害は表示だけのエラーとして続行せず、新規受付を止めて所有処理の停止へ進みます。DBやlockファイルを削除して無理に起動させる手順にはしません。終了後にストレージ・権限・空き容量を調べます。

## 保持ルールと回収

```sh
shellboy storage status
shellboy storage collect --category raw
shellboy storage collect --category managed-images
```

`collect` は既定の保持・参照保護に従う回収です。指定カテゴリの全データを無条件に消すコマンドではありません。`all` / `raw` / `managed-images` / `history` / `index` が選べ、指定なしは `all` です。返却結果と `storage status` を読み、回収できた量・保護された量を確認します。

| 種類 | 既定の扱い |
|---|---|
| 会話・回答・推論・要約 | 期限による自動削除なし |
| shellのpipe原文 | 終端shellを対象に7日 / 合計1GiB。生存処理・pin中は保護 |
| managed clipboard画像 | 7日 / 合計256MiB。使用中・送信pin・保存下書き等の保護を優先 |
| 外部ファイル・画像 | パス参照。ShellboyのGCでは外部の実体を削除しない |
| 削除Threadの履歴本文 | 生存forkや要約出典等から不要になったものを到達性GCで回収 |

Threadを消す前に影響を照会できます。照会は削除もGCも開始しません。

```sh
shellboy thread delete-preview --thread THREAD_ID
# 子孫も削除対象とする場合
shellboy thread delete-preview --thread THREAD_ID --descendants
```

`deletable_now` は選択対象にlive/stopping Runがないことを示します。阻害Run・子孫数・添付参照・他Threadとの直接共有・completed置換の直接sourceを確認します。件数はGC全体の到達性判定や物理ファイル数ではありません。`reclaimable_bytes=null` と理由を返し、回収容量は未見積もりです。共有や子Threadがあるだけでは削除禁止にはなりません。preview後に状態は変わるため、実deleteもtransaction内で再判定します。

阻害Runがあれば停止して終端を確認してから削除します。

```sh
shellboy thread delete --thread THREAD_ID
```

`--descendants` を付けると系譜上の子Threadも削除対象になるため、対象を確認して使います。forkの共有経路や必要な出典は保持され、回収後もtombstoneから理由を確認できます。削除したThreadの専用本文まで永久に取得できる保証はありません。

## 容量表示の見方

`storage status` の主な値は以下です。

| 値 | 意味 |
|---|---|
| `logical_live_bytes` | 保存本文・raw・索引・managed画像等の論理使用量。容量制御の基準 |
| `db_bytes` / `wal_bytes` | 実ファイルのサイズ |
| `managed_bytes` / `log_bytes` | managed実体・ログの物理サイズ |
| `filesystem_free_bytes` | 取得できた場合のファイルシステム空き容量 |
| `reclaimable_bytes` | 回収可能量の下限。全GCカテゴリを含む厳密な見積りではない |

論理使用量の既定警告は8GiB、上限は10GiBです。上限では会話を自動削除せず、新しい作業を `storage_limit` で拒否または復旧待ちにします。受付保存済みの本文と、上限により受付前に拒否された操作は区別してください。停止・終了記録・GCは許可されます。回収して上限未満になっても、復旧Turnは明示retryが必要です。

SQLite内で本文を回収してもDBファイルが直ちに縮むとは限りません。再利用可能ページとOS空き容量を混同しないでください。自動VACUUMは行いません。DB・WAL・SHMを起動中にばらばらにコピー/削除する運用は避け、バックアップするなら本体を正常終了して保存データ全体を扱います。

RAMのbody/layout/history/画像Base64 cache予算はストレージ保持量とは別です。cacheを0にしても会話を消す意味ではなく、送信中の一時作業領域を含む全アプリRSSの上限にもなりません。[設定](configuration.md) に各予算を記載しています。

論理容量の上限で索引作成が止まっても、既存索引のsemantic検索は使えます。検索queryの埋め込み費用は発生し得ますが、検索だけで索引処理を再開したり過去分を再索引したりはしません。物理disk不足とoperation receiptの別予算による拒否は、それぞれの回復手順に従います。

## よくある問題

| 症状・code | 確認と対処 |
|---|---|
| `shellboy` が見つからない | ビルド・インストール先とPATHを確認（[導入手順](configuration.md)） |
| `app_not_running` | 同じdata-dirの本体を対話端末で起動。CLIは自動起動しない |
| 二重起動でlockエラー | 同じdata-dirを管理する本体を使う。別環境なら明示的に別data-dirへ |
| `config_invalid` / 資格情報不足 | 表示されたキー、接続profile、API root、環境変数名を確認。変更後のTOMLは本体再起動で読み込む |
| APIの残高・認証・モデルエラー | 保存されたproviderの理由を読む。利用可能な接続を選ぶ。別providerへ自動fallbackはしない |
| `thread_not_found` / `target_unavailable` | 対象ID・data-dir・削除/回収済み状態を確認 |
| `invalid_state` / `wait_cycle` | 状態と所有関係を確認。自分/祖先待ち、別所有Runの奪取、終端Run retryをしない |
| `operation_conflict` / `operation_expired` | 同IDで異なる操作をしない。旧操作は照会し、結果不明の副作用を無条件に再送しない |
| `cursor_expired` / `cursor_encoding_mismatch` | 新しい一覧・本文取得をcursorなしで開始。違う対象/encodingへcursorを流用しない |
| `output_expired` | rawが回収済み。履歴の引用や保存済み回答は別に調べる。原文を捏造しない |
| `input_limit` | 本文/添付/フレーム等、表示された上限と対象を確認。無言の切捨ては行わない |
| `storage_limit` | 使用量を調べ、保持ルールに沿って回収するか設定を見直し、明示retry |
| `save_failed` / `io_failed` | 保存先や参照先の空き・権限・状態を確認。未保存と保存済み受付を区別する |
| 検索できない/結果が少ない | embedding設定・選択space・index statusを確認。過去分のbackfillは明示選択する |
| サブエージェントを作ってくれない | モデルにAgent Guideの実在パスを指定して読ませる。自然言語だけでの操作成功を前提にしない |
| EnterがIME確定と送信で競合する | send_key/newline_keysを変更し再起動。端末のpreedit通知は保証しない |

エラー報告には操作名・対象の状態・code・秘密を除いた条件を使います。APIキー、origin token、環境変数一覧、実会話全文をログ共有へ含めないでください。全固定codeとJSON形式は [CLI reference](cli-reference.md) を参照してください。


## 診断ログを確認する

通常の診断ログはdata-dirの `logs/shellboy.log`（ローテーションあり）です。macOS既定は `~/Library/Application Support/shellboy/data/logs/shellboy.log`。起動・終了・DB／モデル／shellの失敗などのイベント名、時刻、対象IDを記録します。会話本文・API資格情報・完全なエラー本文を常時保存するログではありません。

Windowsの既定は `%LOCALAPPDATA%\shellboy\data\logs\shellboy.log` です。保存先を変更した場合は指定したdata-dirの下を確認します。モデル失敗の `model_failure` は固定分類と `attempt_id` を記録します。`model_stream_difference` は途中と最終応答の差分を示す診断で、それだけでは失敗や復旧待ちを意味しません。ログのThread/Run/Turn/attempt IDで対象を照合し、本文と最終応答は同じThreadの `/received` から保存原文を確認します。原文には会話やツール引数が含まれるため、そのまま外部へ共有しないでください。

TUI送信の `message_send` は `send.draft_id` / `revision` / `operation_id` で1件ずつ追えます。`queued` はfrontendが要求を取り出した時点、`processing` は送信処理開始、`saved` は保存成功、`failed` / `queue_full` は失敗・受付枠不足です。`saved` の `message_id` とThread IDを保存済み履歴に結びます。`operation_id` は既存のoperation照会にも使えます。

「送信受付中」のままなら最後の段階を確認します。`queued` だけなら処理開始前、`processing` までなら保存成功前、`saved` まであるのに画面が進まなければ通知・履歴取得・描画側の切り分けに使えます。`queued` はEnterや画面描画の記録ではなく、`saved` も画面反映完了を意味しません。ログは既存の有限・非同期writerによる最善努力の記録で、無効設定・容量不足・異常終了等による欠落があり、行の不在だけで未受付や原因を断定しません。本文・添付pathは追加記録しません。

stack overflow等のfatal abortでは正常終了処理を通らず、原因本文がファイルへ記録されない場合があります。端末の標準エラーの文面と、落ちた時刻・画面・直前の操作も調査に使います。正常shutdownがないことだけで原因までは断定しません。

### operation receiptの容量で拒否された場合

`storage_limit`とreceipt予算の案内で拒否された変更は未受付です。`shellboy status` / `storage status` / `operation get --operation ID` は新receiptを作らず、容量と保存済み操作を観測できます。get/list/search/previewは同IDでも再観測し、結果を蓄積しません。semantic検索のquery embeddingは再実行で費用が生じ得ます。waitはTurn待機状態を保存するためreceiptを維持します。

変更操作の元ID・同じ要求・有効な同callerで保全中の結果を再取得できます。終端結果はcreated_atから24時間経過後の内部保守、又は予算圧迫時に回収対象です。pendingと未反映message/添付/compactの参照は保護します。結果回収後もID/hash等を残し、同ID再送は `operation_expired` で拒否し、変更を再実行しません。`result_expired` / `replay_policy` と対象の状態・履歴を確認し、新IDで無条件に同じ副作用を繰り返さないでください。失効したOriginによる認証を迂回する仕組みではありません。

`run stop` / `shell stop` は通常256MiBに加え固定余白を使います。`control_headroom_bytes` / `control_limit_bytes` / `control_available_bytes` で確認できます。停止自身も余白を使い切れば新IDを拒否し、TUIのCtrl+C・`/stop` はreceipt不要のままです。残量は任意の次要求の受付保証ではありません。

期限切れIDのmetadataも同じ予算へ算入します。極端に多い変更や保護対象だけで満杯なら、作業を通常終了しownerを明示的に再起動して旧terminalを回収します。内部GCは継続し、旧pendingの不確実性と保護参照は維持します。自動再起動や無断のpending削除はしません。outcome1件8MiB・通常256MiBは論理予算で、SQLite物理サイズの上限ではありません。

GC結果のmanaged_deleted/managed_failuresは各128件までの詳細です。managed_deleted_count/managed_failure_countが実数、managed_details_truncatedが省略の有無を示します。詳細の省略は回収処理自体の省略を意味しません。

### 失敗時に受信した本文

画面に流れた本文は、復旧待ちへ入っても保存済み原文があれば「途中で切れた応答（保存済み・会話には含めない）」として読み続けられます。`/received` の一覧は古い失敗や成功時のストリーム差分にも到達でき、Enterで保存原文を開きます。元Turnの復旧待ちの間だけ注意色にし、回復・停止後は通常通知へ変わります。依頼抜粋・時刻付きの一覧から元の記録を選べます。詳しいページ操作は[受信記録](tui.md#保存した受信記録を読む)を参照してください。未確定の本文を残すことと、モデル再試行でその本文を採用することは別です。

### 終了の短い待ちとUnixの実行識別

Quitの後も保存と所有処理の停止が終わるまでは、Runが短時間動き続ける場合があります。通常終了の完了を待ち、保存中の強制終了は未確認の外部副作用を残し得ることを考慮してください。Unixでは所有プロセスの記録を検査して停止しますが、PGIDの検査とsignal送信の間にOSがIDを再利用する競合を原子的に除く保証はありません。

所有groupの外へ `setsid` 等で切り離した処理は、停止後も残り得ます。`nohup` 等を使う処理も、所有追跡の範囲外へ切り離す場合の完全回収は保証しません。自分でデーモン化した処理を再起動後まで追跡・維持する機能は今回の対象外です。

## HTTP応答が止まったとき

会話画面は、同じモデル要求について30秒以上受信を観測しなければ、活動表示へ受信待ち又は最後の受信観測からの経過を補足します。受信通知は最大1回/秒のため表示には1秒未満の差があります。生成や推論が止まったと断定する表示ではありません。Ctrl+Cや`run stop`で停止できます。

会話が受信停滞期限に達した場合は、受信済みの本文を保全し、前述の連続失敗予算・部分応答設定に従って自動再試行又は復旧待ちへ進みます。要約の停滞timeoutは今回の会話再試行には含めず、従来の復旧経路を使います。エラーのheader/body段階・経過ms・設定keyを確認してください。設定ガイドの`connections.<名前>.stream_idle_timeout_ms`は既定600000で、長い無通信を許すモデルには増やすか、0で期限を無効にできます。接続確立の固定30秒と手動取消しは有効なままです。未知の失敗で自動再送はしません。

embedding・意味検索・RAGは`connections.<名前>.request_timeout_ms`（既定60000、最小1000）の要求全体期限を使います。モデル一覧は同設定と20000msの短い方です。索引workerのtimeoutは既存の有限retryへ渡し、続く失敗はspaceをpauseします。statusの失敗理由を確認して接続や設定を直し、明示retryします。ownerの設定変更は再起動後に反映されます。検索CLIの切断・owner停止時は残った検索HTTPも取り消します。

## EOFによる未完了を調べる

`EOF before response.completed` は、HTTP本文が終了した時点でShellboyがcompletedイベントを認識できなかったことを示します。API側が送信しなかったことまでは、この文言だけでは分かりません。

修正版では保存される失敗理由に `received_bytes`（受信量）、`parsed_events`（解析できたJSON数）、`completed_items`、`done_markers`、`pending_line_bytes` / `pending_data_bytes`（末尾の未完了量）、`last_event`（固定分類）を含みます。`/received`の保存原文などで同じTurn/attemptの理由を確認し、共有時は本文全体でなくこれらの診断値を使ってください。通常ログの`model_failure`は従来どおり分類とattempt IDであり、完全なエラー本文を自動記録するものではありません。

`last_event=untyped` はJSONに文字列のtypeがなかった場合、`done_markers`は`[DONE]`の受信数です。いずれもcompleted responseの存在を保証しません。受信量があって解析数0でも、コメントだけの受信などがあり得るため原因を断定しないでください。最後の空行まで届かないイベントは未完了のまま保持し、自動で回答やtool実行として確定しません。
