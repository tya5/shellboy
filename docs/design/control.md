# ControlのDB受付と操作receipt

採用設計。受付・履歴選択・実行所有・停止・late応答の確定境界を具体化する。初期詳細設計・CLI契約・引継ぎ資料は非同梱。現行の境界は本書と [境界対応表](control-boundary-map.md) に示す。

StoreはDB専用workerへ短いtransactionを渡す。mutationとoperation receiptを一括commitし、commit後にwake_threads/stop_runs/retry_runsをControlへ返す。Store内でAPI要求・shell起動・停止・ファイル読取りを実行しない。operation IDはinstance UUIDとnonceを分けた文字列。保存済みIDはrequest commandと検証済みoriginのSHA-256 fingerprintを照合し、同内容なら保存済み結果の照会のみ、異内容はoperation_conflict。旧instanceの未記録IDはoperation_expired。変更/waitのpendingと未反映参照は保護する。終端結果は24時間経過後の保守/圧迫時に本文だけを回収できるがID/hash等を残し、同ID再送はoperation_expiredで再実行を拒否する。対象を変えない観測はreceiptを作らず再計算する。詳細は [operation設計](operation-observability.md)。

message受付はnode本文・receipt・Run/Turn割当を保存する。要求進行中の採用経路を変更しないよう、受付nodeを即thread_pathへ追加せず、driverが安全な入力境界で採用する。receipt.message_node_idはそのnode IDである。受理時に決まったRun/Turn割当は変更しない。停止中はheld＋未割当、復旧待ちへの機械メッセージはheld＋同Turn。人間新指示は同Turnの明示再開にする。所有親の通常メッセージをretryへ変換しない。

子Thread系譜とRun所有辺を分離する。検証済みoriginが現在の親Threadからその子を開始した時だけ新Runへownerを付け、owner生存を同transactionで確認する。他者が所有する実行中の子Runを奪う操作は拒否する。独立Threadへの送信は独立処理を維持する。

Thread設定のcwdは字句的絶対化し、既存AGENTS参照はcwd変更に追従させない。子作成・forkはAGENTS参照を親から継承する。model/profile変更はreasoning epochとprofile revisionを更新し、旧opaque reasoningの自動復活を防ぐ。実要求への採用はdriverの安全境界。

Thread削除は生存Runなしを確認し、Thread tombstone・root revision・再開可能なGC job・未実行receiptのcancelledを同transactionで確定する。node本体を直ちに消さず、共有経路・出典の到達性GCへ委ねる。停止要求もstopping保存を先に行い、返信後の実resource回収はControlの責務である。

停止中に保存された未割当held指示は、人間の新しい送信、または所有親による終端子への新しい送信を明示再開境界とする。新Run/Turnへ一度だけ割り当て、次の準備で新指示と併せて取り込む。既割当receiptは動かさず、live recovery_waitへの通常機械メッセージは再開にしない。公開契約にmessage.retryがないため、これを「明示再開」の具体化として採用する（原文7節の停止時保留・4節割当不変と整合）。

接続またはmodelが設定にも明示引数にもなければ、新規Thread作成をconfig_invalidとして拒否する。設定読込みと既存履歴読取りは利用可能。初回送信はThread作成と受付を同transactionにして、設定不備や保存失敗で空Threadだけを残さず下書きを保持する。存在しないprofileや空の仮modelを補わない。

## 人間UIの編集分岐・管理結果・終了選択

フォーク・依頼差替え、索引管理、停止・アプリ終了の採用契約を具体化する。保存済みのuser nodeを編集する時は、DB/file処理をUIスレッドで行わず、adapter workerで原文全文とその依頼に由来するactive添付参照を取得する。世代・元下書きrevisionを返信へ付け、遅い読込みで現在の入力を置き換えない。編集用には別の未送信下書きを作り、元下書きと元nodeを維持する。

送信は特定Run IDへの人間の停止同意を渡す。読込み後に別Runが始まった場合も無断で停止せず、`confirmation_required` と現在Run IDをUIへ返し、編集本文を保持する。Controlは旧Runの実停止を待ってから、対象依頼直前のprefixをforkし、編集本文と新しい添付refを一括commitする。最初の依頼の編集は空prefixを扱う。外部添付は新要求の準備時に現在のファイルを読み直し、古いsend manifestや旧nodeを変えず、binaryを履歴用にコピーしない。新forkのcwd/model/effort変更も同じ受付transactionで適用する。

通常Commandの成功結果を無言で捨てず、管理overlayへ対象操作とJSONを返す。Indexのpreviewは正確なscopeをtyped返信へ付け、API呼出し前の対象数・文字数・segment数と料金注意を確認できるようにする。別scopeの遅いpreviewで現在scopeへの実行同意を成立させない。backfill/retry/rebuildは明示scopeと人間の確認を経て既存の公開操作へ渡す。 IndexStatusのUI結果には独立したreadonly管理snapshotを補い、旧space一覧、選択space、scope別の未索引／部分索引／索引済み／coverage未検証を表示する。管理表示のall既定を過去分の実行許可へ転用せず、full rawの終端を確認していない対象を索引完了と推定しない。CLIの既存IndexStatus型は変更しない。

Quit受付時に新規受付と取消しを開始し、受付済み送信worker、所有処理、DB、診断・ownerの終了順序を保つ。終了futureを待つ間もUI操作受付を続け、`io.shutdown_grace_ms` を超えたら経過と「待機継続／強制終了」を表示する。待機継続は停止・保存処理を再実行せず同じfutureを待つ。強制終了は猶予後の明示選択に限り、terminalを復元して未保存・配下未回収の可能性を示し、通常のjoin/runtime破棄待ちへ戻らない。正常終了と保存失敗は既存Resultで区別する。

UIのtool回数はtool_call受付数ではなく、実OS spawn後に保存した `shell_execution.shell_json.spawned` を数える。completed Turnとlive Runの所有背景処理を区別し、背景処理中・停止処理中を状態へ表示する。

検証: `cargo test --test frontend` の8件で、64 KiBを超える元依頼の全文取得、最初の依頼前へのfork、読込み後の新Runに対する再同意、管理結果、previewのtyped scope、猶予後の待機／強制選択、実spawn回数と背景表示を確認する。実端末での強制終了・全OS回収・性能判定はこのadapter局所試験から達成としない。

### 追加の状態観測と要求境界

Threadのcwd/connection/model/effortの実変更は、同じtransactionで`event(kind=context_changed)`へ変更前後と操作者IDを保存する。Originの認証tokenは含めない。Run開始、Turn開始/前回Turn終了、明示復旧再開、Turnごとの指示ファイル観測にも時刻とローカルtimezone offsetを保存する。固定system prefixへ時刻を追加しない。

context通知は、それ自体ではdeliveryを作らず、既に実行可能なTurnのモデル要求境界でのみ現在Runへ割り当てる。背景処理待ち/復旧待ちを解除せず、通信失敗では未処理を維持する。次Runでも未処理通知を引き継ぎ、正常completed応答の採用transactionでのみ処理済みにする。指示の差分通知はパス/hash/存在状態であり、ファイル監視はしない。通常shell結果は開始/終了時刻と経過msを含める。

### 実行所有に対する停止・stdin操作

原文Shellboy §10の「他者所有の独立チャットをエージェントが再試行・強制終了する権限は持たない」を、Run停止とshellの副作用操作にも適用する。originなしの人間は任意対象を操作できる。検証済みモデルoriginは、自Runと現在の所有辺で到達する子孫Runだけを停止し、その所有shellだけへwrite/EOF/stopを送れる。get/readは既存の履歴照会範囲を維持する。retryは復旧判断の規則どおり直属親または人間に限る。独立チャットへのmessage送信を停止権限へ転用しない。

正本DB保存失敗はUIの下書き保存workerからもControlへ伝える。表示キュー満杯や照会対象消滅を保存障害と混同しない。致命失敗の受付停止・index取消は最初のawait前に確定し、shell停止はobserverから独立したtaskが保持する。read observerの取消しで停止まで中断しない。index worker/collectorのjoinは通常終了の責務として残し、observer取消でjoin handleを失わない。

## 状態境界の規範表

採用状態: レビューW3/SP2/R1-2の実装照合基準（2026-10-25）。
[適用仕様](effective-spec.md) の受付・所有・遅着契約を具体化する。
この表は汎用遷移エンジンを追加する仕様ではない。既存のdomainごとの関数/DB transactionを対応させる。
[実装と代表回帰の対応表](control-boundary-map.md)で各境界の保存・所有入口を追跡する。
未修正の実装を表へ追認しない。特にTurn waitingの保存、通常終了の復旧待ち情報、重複kickの処置は
台帳C3/C5/C6とR1-2で実装/回帰の証拠を別途必要とする。

「保存→通知」はcommit成功後のみ通知/副作用開始という順序を表す。状態の前提検査と更新は同じControl順序/transactionに置く。
`UPDATE 0 rows` は一律エラーではない。対象不存在、世代不一致、既に同じ終端、取消し済みを再読して区別し、
不正入力は対象/状態の操作エラー、取消し/lateは不採用、保存障害だけをsave_failedにする。

| 対象・契機 | 前状態 → 後状態 / 保存点 | 禁止・重複・遅着 | 通知・UI / 終了・再起動 |
|---|---|---|---|
| message新規受付 | 宛先/所有を検査しnode・receipt・割当を一括保存→accepted、又はheld/pending | 削除との先後をDBで決定。同IDは照会だけ。割当済みRun/Turnを変更しない | 保存後に受付、必要なDriverだけwake。未保存ならdraft保持 |
| message反映 | accepted→appliedを正常なモデル入力反映境界で保存。heldは明示再受付でacceptedへ戻した後に反映 | 反映前の設定変更/通知で勝手にappliedへしない。取消しと二重反映を防ぐ | 回答の成功とは別。未反映は停止/再起動情報へ残す |
| message取消し | 未反映→cancelled。停止中の新規受付はheld/未割当 | 既にappliedな事実を書き換えない。割当の再利用禁止 | 理由と保全された本文を照会可能にする |
| Run開始 | 未実行→liveを割当/所有辺と保存 | stoppingの親へ子を増やさない。他者所有Runを奪わない | commit後Driverを一つだけ起動。Thread系譜と所有辺は別 |
| Run継続/正常終了 | live→live又はcompleted。未完Turn/所有実行/未処理指示通知がないことを同順序で確認 | 回答だけでRun完了にしない。完了直前の追加指示の先後を保存 | Turn completed＋Run liveなら背景処理中。終了資源は解放 |
| Run停止受付 | live→stoppingを保存し所有木の新規開始を禁止 | 重複stopは既存状態を返す。終端Runを復活させない | 受付後停止。self stopは返信/有限猶予を先に確保 |
| Run停止完了/致命失敗 | 停止はlive/stopping→stopped。配下の回収と結果保存後に終端。failedは既知終端として保持し、要求失敗を一律変換しない | 独立ThreadのRunを止めない。停止を最終回答で偽装しない | 上から停止要求、下から解放確認。未確認は理由付き |
| Turn開始/準備 | 新規又は明示retryのrecovery_wait→preparing、準備成立→running | prepare世代/選択/profile不一致は採用しない。API直前にも検査 | 準備中を表示。AGENTS snapshotは同Turn retryで維持 |
| Turn明示wait | running→waiting。モデル実行の成立待ちの登録境界で保存 | 外部人間の観測やbackground shellだけでTurn待機にしない | UIは対象の待機を表す。元Runはlive |
| Turn待機解除 | waiting→runningを成立/timeout/interrupted/取消しの確定に合わせ保存 | 一度だけ解除、待機辺除去。終端/世代不一致なら再開しない | 結果をモデルへ返す。timeoutは対象停止ではない |
| Turn復旧待ち | preparing/running/waiting→recovery_wait、原因と未確認情報を保存 | 設定変更/通常機械message/時間で再開しない | 原因・対象・保全・次の操作。直属親又は人間が判断 |
| Turn明示新指示 | recovery_wait→preparing。同Turnの人間新指示を保存し明示再開 | 普通の親messageをretryへ変換しない。既受付割当は不変 | 元message保持。新指示とretryの選択を区別 |
| Turn正常回答 | running→completed、回答node/attempt採用と保存 | 正常completedでtool callなし。phaseだけで復旧待ちへしない | Turn回答通知。背景所有があればRunはlive |
| Turn停止/終端失敗 | 進行状態→stopped、理由を保存。failedは既知終端として保持し、通常の準備/通信失敗はrecovery_wait | 終端から遅い回答を採用しない。根拠なしにfailedへ変換しない | 停止した子に架空回答を生成させない |
| attempt開始/終端 | preparing→inflight→completed/failed/incomplete/cancelled | 保存済み世代を検査。cancel/lateは最終回答へ混ぜない。未知失敗をcontext超過へしない | 試行失敗と操作受付の結果は別。中断情報は残す |
| tool group実行 | 全形式検証→call直列実行→全call result保存→次要求 | 後続取消しも対応result必須。1callの結果を次callの結果と混同しない | call/result対応で表示。重複kickで実行を増やさない |
| shell起動 | starting保存→spawn→running保存 | DB受付前spawn禁止、取消し後spawn禁止 | 実spawnを数える。cwdは起動snapshot |
| shell出力/終了 | running/stoppingでraw batch保存→drain→exited/failed/stoppedとoutput_complete保存 | process exitだけで出力完了としない。stream decoder/raw cursorは別 | 原文保存後に完了通知。observer取消しは回収taskを取消さない |
| shell背景化 | foreground表示/待機を解除しshellは生存、未実行callはcancelled結果 | 通常完了通知で勝手に背景化しない。後続callを実spawnしない | 対象継続と取消件数をUI/モデルへ通知 |
| receipt受付/結果 | 未記録→pending意図/受付結果を副作用前保存→結果確定 | 同ID同要求は共有/再生、異要求conflict。結果不明pendingで副作用を再実行しない | 受付とdownstreamを別表示。旧instanceは照会だけ |
| wait観測 | 登録/状態確認→satisfied/timeout/interrupted/recovery_wait/unavailableを一度確定 | 循環を先に拒否。通知は再読のきっかけ、timeout境界も最終確認 | observer終了で対象を止めない。辺/保留情報を解放。詳細はwait設計 |
| 添付受付/採用 | source nodeとref一括保存→安全境界で反映 | 人間messageへ付替えない。tool group全結果より前にモデル添付を入れない | 所属/選択/実体版を別表示。欠落は元位置に状態 |
| 添付prepare | 選択/profile/sourceの世代検査→現在実体を準備→API直前再確認 | 更新/消失/取消しでstale bufferを送らない。既確定所属/順序は維持 | 送信pinは要求終了で解放。失敗は同messageを保持 |
| managed画像 | creating→ready/failed、ready→deleting→deletedを保存 | deletingへ入った後pin/再活性化禁止 | unlinkはfile worker。再起動で掃除継続、外部原本は削除しない |
| index job | pending→running→done/failed。共通障害ではspace pause | source版/space不一致のlate embedding不採用。過去全体を暗黙索引しない | 原文/vecを同transaction。失敗数/明示retryを表示 |
| summary/spill採用 | 対象snapshot→生成→世代/選択CAS→completed置換と出典/新経路を一括保存 | 取消し/停止後のlate要約を採用しない。通常keepと安全境界を混同しない | 原経路/出典へ到達可能。復旧待ちの手動compactは自動再開しない |
| GC batch | root snapshot/revision/pin確認→有限batchで回収/進捗保存 | 新root/共有/fork/未完了回答の保護を省略しない。revision変化は再評価 | 原文回収はtombstone理由。rawとcanonical索引の寿命を区別 |
| draft送信/返信 | 本文/添付/revision snapshot→commit成功で同revisionだけ解除 | 古い成功/拒否で次draftを消さない。queue拒否も元内容を保持 | 受付/準備/失敗を視覚識別。Thread切替も世代付き返信 |
| アプリ通常終了 | Quit受付成立→新規受付停止→所有回収→DB drain/close→lock解放 | queue拒否ならquitting表示へ進めない。終了処理を重ねない | 復旧待ちの原因/不確実情報を停止理由だけで上書きしない |
| アプリ保存障害 | 最初の確定保存失敗→受付停止/所有停止→可能な診断と終了 | 一般操作エラー/queue満杯/lateを正本保存障害に昇格しない | 未保存本文と保存済み受付を区別。通常成功として終了しない |
| アプリ強制終了 | 猶予超過→F→Yの明示確定→端末復元/終了 | 通常Quitへ不要な同意を増やさない。古いPIDを停止しない | 未保存/配下未回収の可能性を通知。孤児完全回収は保証外 |
| 次回起動 | 所有lock取得→DB/版確認→旧未終端snapshot保存→中断/結果未確認の終端化 | 自動API/shell再実行なし。旧receiptの割当を書き換えない | 新指示時に新Run/Turn。旧子/待機/未反映/復旧理由は原文へ到達可能 |

境界試験は表の前後を入れ替える代表順序、保存失敗、取消し、late応答、重複操作を選ぶ。
全組合せを無差別生成せず、台帳の対象IDへ実装commitと試験を接続する。
表を書いただけではW3.4やC6を完了としない。本書の状態境界表、[wait](wait.md)、[runtime](runtime.md)、
[operation](operation-observability.md)、[添付](attachment-provenance.md)、[GC](gc.md) を併せて照合する。

## 復旧再試行とforeground解除

明示retryはTurnの世代更新をcommit後、対象Threadへ一度だけquiet wakeする。新しい人間入力による割込みではなく、重複kickやRun IDからの二重解決を行わない。

foreground解除は現在call専用の単一値watchで渡す。Run共通Notifyのpermitを再利用せず、完了／背景化／割込みのどの結果でもcall終了時にsenderを解除する。完了と重なった人間の背景化や古い割込みを後のcallへ配送しない。`background` は現在受信者へ配送できたときだけ成功する。 collector完了を選んだ時点でsenderを解除し、結果のexcerpt読取待ちをforeground実行と扱わない。追加指示の通知は合流し、人間の背景化が未消費の追加指示通知に拒否されたり、後着通知に上書きされたりしない。値はcall終了で破棄し、他callへ持ち越さない（PR1-14）。

追加指示は保存済みreceiptを正本とする。call専用受信者の登録後に同Turnの未反映accepted receiptを再確認し、モデル要求中〜call登録前の受付もforegroundを解除する。登録後の受付はそのcallのwatchへ通知し、受信時にもaccepted状態を再確認する。既反映入力の遅いkickは解除せず、同じcallの次の正当な通知を受け続ける。既にモデルへ反映したreceiptを後続callの割込みにせず、未実行callの取消結果は既存tool group境界で保存する。内部のrelease理由enumと単一値watchは既存DriverSlotの3つの通知／フラグを置換する局所同期であり、新しい業務event・表・worker・依存を追加しない。


### AGENTS snapshotのbyte数と再読込（P7）

各新Turnは有限control-file workerで現ファイルを8MiB上限付き読取・hashし、同Turnのretryは
保存済みsnapshotを再利用する。mtime/sizeだけでは同属性の内容差替えを検出できないため、
stat一致だけによるTurn間cache省略は採用しない。UI threadでの読取/hashや追加RAM cacheは置かない。
snapshot.sizeは読み取ったUTF-8本文の実byte数とし、読取前statの長さを本文の長さと偽らない。
modifiedは読取前に観測した属性であり、読取中の編集がない保証や原子ファイルsnapshotの証明ではない。
stat後の増加でも実読取8MiB上限を適用し、元の本文/hashを変更しない。将来cacheを導入する場合は、
同size/mtimeの差替え検出を失わず、Turn固定snapshotと追加RAM予算を両立できることを再検証する。


### 通常終了前の中断context保全（C5）

通常終了も、既受付送信/下書きの保全後、Run停止・wait解除より先に既存interruption_contextを保存する。
再起動時と同じ有界projection/source nodeを使い、理由はapp_shutdown、actorはshutdownとして区別する。
これは停止開始前の保存事実であり、shellの最終終了codeや副作用の確定を捏造しない。元の復旧理由、
待機条件、子Run、shell/未反映messageへの参照を残し、次回の明示人間入力へ既存replayで渡す。

Controlの既存OnceCellを1 fieldだけ追加してcapture結果を共有し、Frontendとshutdown本体の二重保存を防ぐ。
保存成功/失敗のどちらでも停止とdrainは実施し、保全失敗は終了結果へ伝播する。同期begin_shutdownは
保存障害時の緊急停止として維持し、通常Frontendは非UI threadで保存付き停止経路を呼ぶ。
DB保存が詰まる間は所有処理が継続し得るが、UI/IPCの新規受付は既に閉じ、既存grace/forceはこの段階も観測する。
新しいworker/表/event/公開操作を追加しない。再起動時は既にstoppedのRunを自動再開せず、保存contextも重複生成しない。
drainで停止するRunにもapp_shutdownを保存し、人間のexplicit_stopを上書きしない。

captureはUI/IPC入口を閉じ、既受付send workerとdraft保存workerを完了させた後に実行する。
前方保存やcaptureが詰まる間、既所有処理はまだ継続し得る。既存grace/forceがこの非UI async区間全体を観測し、
強制終了では保全・回収未完の可能性を明示する。保存失敗でもearly returnせず、既存storage failure経路で
停止して、wait解除・shell回収・owner closeまで進み、終了結果へ失敗を返す。
shutdownのsource projectionはstartup_stateをplanned_stateとして示し、停止前の観測と予定を最終結果から区別する。
保存済みexplicit_stopがあればRun/projectionとも上書きしない。


C5の受付境界は新規admission close→既受付のcontext mutation完了→snapshot→cancel/drainの順とする。
既存RequestGuardへ分類とcontext件数を追加し、通常のThread/Run/入力/添付/圧縮操作、UI送信・編集・再読込を数える。
file準備待ち中もguardを持ち、保存結果または失敗が確定してDropされるまでcaptureを待つ。
Waitと長時間runtime readはこの件数に含めず、snapshot後のcancel/drainで終える。
新しい並列registryやguardは作らず、既存active_requestsは全要求drain用のまま保つ。
追加は受付closeのatomic flagとcontext件数のatomic counter各一個で、件数は現存する既受付worker数だけ、
増減責任は既存guardの生成/Dropに限定し、既存Notifyで待機を解除する。close再確認により受付競合を拒否する。
この前段待機も既存grace/Forceの対象であり、Forceはsnapshot完了を保証せず上書きできる。
mutationの保存失敗は既存SaveFailed診断・停止責務を保ち、captureの成否にかかわらず最終drainを実施する。

受付closeとcontext counterの増減・capture側確認はSeqCstの一つの全順序に揃える。
先着guardはclose後の件数確認に含まれ、close後着のguardは二度目の受付確認で必ず拒否される。
線形化点は成功するguardの二度目close確認と、prepare側のclose store＋件数ゼロ確認であり、
別atomicのRelease/Acquireだけで双方が古い値を読むstore-buffering順序を許さない。


UI内の依頼編集では、旧Run停止は返却を待つshellプロセスからの自己停止ではないため、
self_reply_grace_msを挟まず直ちに適用する（PR1-4）。確認済み停止、旧Run terminal確認、
編集のfile準備・fork/message保存の順序と既受付guardは維持する。旧Runのterminal待ちは既存30秒上限を持ち、
保存前の編集をadmission freezeだけで破棄しない。通常終了はこの有限な保全待ちも既存grace/Forceの対象とし、
新しい編集状態・永続表・取消しworkerを追加しない。元依頼は残るが、未確定のローカル編集本文の永続化を
operation fingerprintや停止成功だけで保証しない。失敗/Forceで未保存データが残り得る既存境界を維持する。

## B06 状態遷移整理の順序（R1-2〜R1-6）

DBを唯一の状態正本として維持する。最初にmodel_attemptの既存終端UPDATEをcontrol::stateへ移し、SQL bytes/前状態predicate/引数順・呼出し側transaction・変更行数の扱いを保持する。failed/completed/incomplete/run取消し/前instance中断は別の意味を持つ関数とし、共通の汎用setterや別の状態engineは作らない。attempt INSERTはTurn snapshot/添付admissionの原子的境界に残し、driver phase抽出時に扱う。

前状態/CASと0行の意味はこの純構造変更に混ぜず、各確定境界で別に監査する。0行を一律InvalidState/SaveFailedにせず、冪等終端・generation失効・競合・未存在を区別する。C5の通常停止context保存と前instance復旧、C6 explicit waitのwaiting、C3 foreground token、既存overflow/添付/late-response修正は再実装しない。終了順序を整理目的で変えない。

context-limit時は、削減処理を始める前にattemptのfailed/rawを保存する。削減のno_target等で同じ失敗を復旧情報へ渡す既知のfailed→incomplete再分類を、一般的なlate応答と混同しない。最終分類を遅延して一度だけ保存する案は、削減中の障害/取消しで監査情報を失うおそれがあり採用しない。前状態制約を足す際はこの再分類を明示し、completed/cancelledの復活防止、generation/Turnの失効、正常停止/前instance中断の違いを別々に検証する。

## Driver要求ループの段階整理（R1-3）

OverflowBudgetは従来のTurn ID/generation/stageの3局所値を所有し、同じTurn/generation内のHTTP要求で予算を共有する。失敗/rawの永続保存後にold-groups段階を消費し、no_target又は2度目の超過でcurrent-suffix段階へ進む。最大二段階、明示context超過だけを削減し、同Turnの追加要求や正常stream開始で予算をリセットしない。Turn又はgenerationが変わった場合だけリセットする。これは既存Driverの一時的な再試行予算であり、Run/Turnの状態正本や新engine/workerではない。既存spill_and_summarizeのM7境界・進捗判断は再実装しない。

要求ごとの準備はrequest moduleへ分離する。Instructionsは従来のTurn ID/本文（不在も含む）の2局所値を所有し、初回観測の保存後にsnapshotを取り直してからHTTPへ進む。prepare_attemptは接続/添付/入力構築とadmissionを順に行い、既存4局所値をAdmittedRequestとして返す。Snapshot::admit_attempt_txは世代/selection/profileの照合、running遷移、attempt INSERT、添付admissionを同一transactionに保つ。競合は再snapshot、準備失敗は共通fail_turnで既存の復旧保存後に元errorを返す。DB障害時のerror優先順位を変えない。新しい業務状態・worker・公開型・個別依存注入は追加しない。

stream_responseは既存32件channelとdelta配送だけを所有し、終了したHTTP future/channelを戻る時点で解放する。既存AttemptFailureの有限partial/items/rawをそのまま返し、独自のerror箱やdecoderを追加しない。replay ReadPinの解放は呼出し側でHTTP終了直後、取消確認・削減・採用より前に保つ。添付のPreparedAttachmentsは準備失敗の復旧中も保持し、成功時は採用transactionのmark_successまで保持する。

adoption moduleはresponse/receipt/event/tool-group/添付成功を既存の一つのtransactionで確定し、その後にindex wake、Thread変更通知、owner通知を行う。tools moduleは全callのencoding/initial inputを最初の副作用より前に検証し、直列実行・残りの取消結果保存・group完了・添付配送を順に所有する。overflowのHTTP失敗出口も既存のraw保存→有限削減→再snapshot又はincomplete復旧にまとめる。drain/停止は呼出し側の既存境界に残す。driveは90行（元約350行）で、80行目標のための一回限りの引数包みや新TurnLoop engineを追加しない。

元レビューの物理行数目安（drive 80行、総行数+5%以内）は採用の絶対条件にしない。今回のdriveは90行で、元の巨大1行closureを分ける関数signature/展開によりmodule合計の物理行数は増える。SQL literalの集合・DB transaction数・HTTP要求数・永続/メモリ業務stateは増やさず、行数のためにSQLを再び1行へ圧縮したり一回限りの転送wrapperを置かない。recover(None)の共通出口はfail_turnに揃える一方、admit_attempt自身の直接利用時にも失敗を保存する既存C8境界は保持し、単なる呼出し数目標のために外さない。

## model_attempt終端のCAS（R1-2）

終端書込みはattemptの前状態と、attempt.generation＝所属Turn.generation、生存Run、対応するTurn状態を同じUPDATEで照合する。completeはinflightから、failはpreparing/inflightから、incompleteはpreparing/inflightに加え既知のcontext-limit failedから許可する。incompleteはrecover_turn_txの後に呼ぶためrecovery_waitも許可する。completed/cancelledの監査fieldsを遅着失敗で上書きせず、古いgenerationの応答を新しいTurnへ採用しない。

変更行数と保存状態から型付きのApplied/AlreadyApplied/Obsoleteを区別する。同じ終端への再保存は既存本文・時刻を保持し、終了済み又は世代/Run/Turn失効はObsoleteとする。未存在はTargetUnavailable、未知又は現在の遷移で許可しない前状態はInvalidState、現存する許可済み前状態なのに変更されない場合はOperationConflictとし、真のDB障害だけSaveFailedへ伝える。adoptはAppliedだけを新node/result採用へ進め、失効したoverflow失敗は削減しない。recoverは既存の遅着raw監査nodeとroot revision保全を維持し、attemptの終端不変性と監査の保持を分ける。追加の永続state/schema/workerは作らず、既存AttemptStateを分類に再利用する。

### 終端の保存責務と順序の集約

Turn復旧と親への単発通知、所有資源の回収後に行うRun群の停止確定、前instanceの中断確定、Control終了時の最終fallbackをcontrol::stateへ移す。それぞれの既存SQL・transaction・条件・呼出し順・各時刻採取箇所を保持する。停止確定は所有木の凍結/子driverとshellの取消し/生存handle消滅を待った後だけ呼ぶ。前instance中断はbounded context保存後、旧attemptをincompleteにし、自動再実行しない。通常終了のfallbackは既所有workerと受付をdrainした後に呼び、既存のstop reasonを保持する。

異なる終端を一つの理由flag付き万能関数へ統合せず、state module内で責務を揃えた別の境界関数にする。通常停止のcancelledと前instanceの結果不確実なincomplete、既受付receipt/添付のhold範囲を混同しない。ownerへの通知も同じcaller transaction内で行い、停止前snapshot、取消し、最終保存の前後を整理目的で動かさない。

状態集合の分岐は既存AttemptState/TurnState/RunStateと有限のACTIVE_ATTEMPTS/REQUEST_TURNSを再利用する。SQLの前状態は各境界のstatic文に明示し、型付き遷移行列で照合する。元レビューの「状態集合の文字列0件」を達成するための動的SQL組立てやSQL断片macro/汎用遷移DSLは採らない。実際の判定/保存責務とtransactionの見通しを優先する。

### 前状態backstopと再保存（R1-2）

同一transactionの既存witness検査を残し、attempt admissionのTurn-running、人間によるrecovery_wait再開、正常Run/Turn完了、tool-call起動/結果、tool-group完了、shell spawn失敗/collector完了にも前状態条件を置く。正常経路は既存の一回のUPDATEのままで、0件の場合だけ同じDB job内で保存状態を読み分ける。再保存したRun/Turnのended_atや回答nodeを変更しない。同じ回答は冪等、別回答はOperationConflict、停止/終端への遅い完了は失効、対象不在はTargetUnavailable、未知の保存状態はInvalidStateとする。admit/採用のgeneration/selection/profile検査は従来の同一transactionに残す。

既にrunning/completedのtool-callは新しいshell_execution INSERTをrollbackし、OS spawnへ進めない。正常な全call検証→直列実行→取消しを含む全result保存の順序は維持する。tool-groupの完了再保存は添付採用/通知を重複させない。collectorの完了再保存は確定済み終了情報・completion JSON・単発event/deliveryを保持する。spawn失敗はstartingに加え保存済stop intentのstoppingからも確定し、collectorのないstopping executionを残さない。既にexited/failed/stoppedならその事実を上書きしない。raw batchの保存/通知とdrain/output_completeの意味は変えない。公開型・永続形式・worker・再実行は追加しない。

## 生存handleの所有（R1-5）

LiveRegistryは既存のdriver/Shell登録mapを所有し、単一Runの取消し、凍結済み所有木の取消しと生存確認、全shellへの停止通知をまとめる。単なるfield束ではなく、停止をどのhandleへ配送し、現在のdriver自身を待機対象から除外するかをここで扱う。DBのRun/Turn状態を複製する状態engineは追加しない。通常の単一Run停止ではThread keyによるdriver検索を保ち、全driver走査へ変更しない。所有木は従来のHashSetを再利用する。

Controlの既存2登録mapを一つのArc<LiveRegistry>へ移す。保有要素は従来どおり現存する登録だけで、追加queue/cache/worker/Run上限を設けない。Registryはprivate map内で登録または既存slotへのnudge/interrupt、slot ID照合後の削除、foreground解除、shell handleの登録/削除/clone/停止を所有する。新規slotのUUID・child token・Notify・foregroundは未登録時だけ同じguard内で生成し、既存slotへのnudge/interruptもguard内で行う。Control/DriverはDB観測・spawn・既存changed通知・owner nudge・遅着work再確認を保持する。DB内の停止凍結→handle取消し→通知を先に登録した生存確認loop→終端保存の順序を変えない。2つのmap guardを同時に保持せず、通常停止とshutdownのlock順を保つ。UI側のbegin_shutdownはtry_lockだけで停止通知し、I/Oやlock待ちを置かない。

保存障害時の停止taskはRegistryのArcを所有し、読取observer取消しでshell停止を失わない既存C1境界を保持する。このArcはControlへの逆参照を持たず、新しい寿命cycleを作らない。maintenance/index workerをfield束にする抽出は行わない。

### runtime要求の所有と配送（R1-5）

runtime moduleは既存Control implの要求dedup、永続intent予約/結果確定、自己shell停止のreply graceと起動時intent再適用をまとめる。ModelListの接続解決/明示Thread検証/切断取消しと、Waitの観測/失敗時release reason保存は別の処理として読めるようにする。Stop/EOFは既存の保存後handle配送を共有し、各commandの認可、保存、通知、実副作用の順序を変えない。

RuntimeOperation mapは既存Controlに残す。別のRegistry classへControl/Store/stop/reply graceを再注入するとfield転送だけが増え、起動中shellとの必須lock順序（operation登録解除を保有したままshell handle照合）を隠すため採らない。既受付mutationを所有する既存workerとwatch結果共有を維持し、observer切断でworkerを取消さない。既保存の結果は新しい停止効果やgraceを作らず、成功した自己停止だけ結果公開→grace→登録解除/handle停止の順を守る。再適用は登録済みgrace ownerを一度だけsnapshotし、失敗ownerで別の成功ownerの猶予を失わせない。公開wire/永続形式、mapやworkerの数、取消し範囲は増やさない。

応答のJSON値化はencode_resultへまとめ、操作ごとの既存error codeと文言を引数で保持する。shellのIoFailedと管理系のInvalidStateを一律化しない。このhelperは値の直列化だけを所有し、receiptの有限frame検査/保存やHTTPエラー分類は別の既存境界に残す。

元レビューのfield数/参照数目安は絶対条件にしない。LiveRegistryの停止・生存確認は実際の重複責務を減らす一方、runtime map、maintenance、index workerの寿命を数字のために同じ箱へ入れない。関連するshutdownのdrain順序と実際のhandle所有が明示できる範囲を採用する。

## 送信の割当判断（R1-6）

assign_messageは送信transaction内のRun/Turn割当・明示再開・hold判断を所有し、MessageAssignmentとして従来の6局所結果を返す。停止中Runへの入力は未割当heldのまま、recovery_waitへのモデル入力は割当済みheldのままとする。この通常送信経路では人間の明示入力だけが同Turnの世代を進め、また人間/生存する直接ownerの明示再始動だけが従来の未割当heldを新Run/Turnへ割り当てる。既に割当済みreceiptの帰属は変更しない。親owner認可、context記録、held receipt再受付、Run/Turn生成は元のSQL・時刻/ID採取順序で同一transactionに残し、user node/索引許可/receipt保存/Thread revision更新/wake効果の順序を変えない。

TxCtxは追加しない。既存Transactionの借用、caller、operation ID、時刻をまとめるだけでは新しい振る舞いの所有にならず、順序やerrorの意味が異なる呼出しへ汎用引数束を渡す層が増える。Run/Turn生成はcallerのclock回数と時刻を保持して共通保存境界だけをまとめ、明示の割当policyとdriverの自発イベント処理の境界を保つ。query/receiptは既採用P2のread-only境界を維持し、今回の整理で新しいreceiptや暗黙処理を追加しない。

### 中断snapshotの保存責務（R1-6）

起動/終了時のcapture_interrupted_contextをcontrol::interruptionへ移し、同じtransaction内で状態確定より先に呼ぶ境界を保つ。起動はprevious_instance_interrupted/startup、通常終了はapp_shutdown/shutdownのままとし、既存stop reasonとplanned_state/startup_stateの区別を維持する。Controlの受付drain→snapshot一度保存→取消し→所有資源drain→終端fallback、起動時のsnapshot→旧work中断→旧queue整理の順を変更しない。既存の失敗時rollback・再保存しないOnceCell契約も保持する。

SectionWriterはsection単位のpreview/count、原文chunkのflush、保存済みdetail参照を所有する。preview64件/detail参照64件/待機条件preview4件、原文chunkの32件又は元入力65536byte到達という既存しきい値を名前付き定数で保持する（1原文自体を切断するbyte上限へ変更しない）。stop reason/planned_stateの変換は原文chunk保存より先、previewの条件切詰めは原文複製より後とし、待機原本は個別interrupted_waitのまま二重保存しない。最後の未満chunkも同じtransactionでflushする。snapshot対象/並び順とcredentialsを含めないallowlistは不変で、新queue/schema/workerやsnapshot再要求は追加しない。

### 局所SQLの見通し（R1-4/R1-6）

中断対象の選定、各section、待機原本、context event保存の9 SQLは用途別のmodule内定数へ置き、列・join・前状態・順序を複数行で読めるようにする。quoted値を含むSQL token列とplaceholder位置は元の文に一致させ、query/transaction数や保存JSONを変えない。driverのprepare/admit/adopt/recoverなどの命名したtransaction責務と組み合わせ、保存順序を追える形を優先する。1回だけ使うqueryのための転送wrapperやrepository層は追加しない。

元レビュー時点の長行数（105→15未満）は現在の機能追加を含む全control moduleへ機械的に適用しない。今回変更した境界の長大closure/SQLを対象に整理し、独立した固有queryやtest fixtureの行を数字だけのために分割する変更を避ける。SQLの意味を隠す汎用query builderも採らない。

## foreground解除のUI/モデル通知（F14/A03）

採用: 人間操作による完了待ち解除を以下の契約とする。背景化と追加指示によるforeground待機解除は対象shellを停止せず、既存tool resultへreason/execution_idを保存し、同じgroupの未実行callにもcancelled結果を保存する。UIは全結果とgroup完了の保存成功後、既存EngineEvent::Noticeで解除理由・継続対象IDと、後続があれば未実行取消件数を一度通知する。後続callが0件でも背景化を通知するが、存在しない取消しは表示せず「後続の未実行呼出しはありません」と区別し、モデルへの次要求に渡す結果との対応を保つ。明示background起動や通常完了、他callの古い通知を人間の背景化と表示しない。

追加の永続event/queue/worker/公開型は作らず、group処理中だけ既存ForegroundReleaseと対象IDの局所値を持つ。通知は解除時に観測した操作結果であり、以後のprocess生存や成功完了を保証しない。後続の実終了は既存shell completion保存/通知で区別する。保存失敗なら成功Noticeを送らず、従来の保存障害停止へ進む。元のユーザー発言やshell rawを通知のために生成/変更しない。

Ctrl+CのRun中断は背景化と分ける。stopping保存→Changedによる「停止処理中」投影→取消/回収→stopped投影という既存reactive経路を用い、停止受付を回収完了と表示しない。中断通知のために停止したモデルを新たに起こさず、既存のcall対応結果・保存された理由/所有終了情報を保持する。通常終了/再起動の詳細保全はinterruption_contextの既存境界による。

### 重複tool受付とincomplete再分類の保全（PR2-19）

run_shellのadmissionでOperationConflictとなった場合、run_tool_groupはその競合を上位へ返し、tool error結果の新規保存やcall/group完了へ変換しない。既存running callの実処理・operation ID、completed callの確定結果を維持する。保存障害も既存の停止通知後に直ちに返し、障害を説明する追加tool結果をDBへ書こうとしない。通常のtool検証・OS spawn失敗の結果保存契約は維持する。

model_attemptのpreparing/inflight→incompleteは既存通り。failed→incompleteは、保存済みerror_json内のModelErrorが既存のis_context_limitで明示context超過と判定できる場合に限る。保存済みJSONを分類し、読取ったerror_jsonとgeneration/Turn/Run前状態をUPDATE時にも照合する。入力引数だけをcontext超過に偽装して他原因の保存済み失敗を上書きしない。不明・不正・他原因のfailedはInvalidStateで原文・error・ended_atを保持し、旧generationや停止済みRunは従来通りObsoleteに分類する。新しいattempt状態・表・event・workerは加えない。

### Turn作成の共通保存境界（PR2-10・構造）

message割当、既存Runへのevent/添付到着、snapshot以前の準備失敗で必要なTurn作成は、state::create_turn_txへUUID発行・preparing INSERT・context::turn_startedをまとめる。作成するかの判定、Run/Threadの検証、transactionの開始/commit、wake/復旧方針は各callerに残す。helperはrusqlite::Resultを返し、DB受付の既存sql_errorとdriver側のStorageErrorへの変換を維持する。

created_atとturn_startedの時刻は別引数で受け取り、DB受付は従来のrequest時刻を両方に、driverは従来通り別々に採取した時刻を渡す。event actorは既存context::turn_started内部のshellboyを維持する。復旧の対象Turn/Runやreceiptの解放範囲はこの構造変更では変更せず、新しい状態・worker・transactionを加えない。

### 明示復旧の対象Turnとreceipt（PR2-10・挙動）

人間の新指示と明示Run retryは、認可後に解決した一つのrecovery_wait Turn ID/generationへ再開を束縛する。state::resume_recovery_turn_txがTurn/Run/Thread/generation/生存Runを同じUPDATEで照合し、成功した場合だけそのTurn/Run/Threadに割り当て済みのheld recovery_wait receiptを再受付する。過去の別Turnのheld receiptや別hold reasonをRun全体から解放しない。0件ならcallerの既存競合/状態エラーへ戻し、receipt・context eventを変更しない。

モデルの通常messageは引き続きheld、人間の新指示は通常wake、retryは人間または生存直接ownerの認可とquiet wakeを維持する。context actor/payloadはcallerの既存形式で同一transactionへ記録する。Turn再開・receipt再受付・eventはcommit/rollbackを共有し、途中状態は公開しない。新たな状態・receipt・queue・workerは追加しない。

### 生存mapの非公開化（PR2-11）

productionへdriver/shell mapやguardを公開せず、登録・比較後削除・停止配送という実際の所有責務をLiveRegistryのmethodへ集める。runtimeのself-stopはruntime_operations guardを保持したままRegistryのshell停止を呼び、従来のruntime_operations→shell lock順とstarterの再確認を維持する。EOF等はhandleをcloneしてguardを離してからawaitする。テストのfixture登録・競合gateだけcfg(test) accessorで既存mutexを参照できる。

cancel_runはThread keyのO(1) driver検索とdriver有無の戻り値、cancel_ownedは凍結済みRun集合の走査と現在driver除外という別契約を持つため統合しない。DB状態をRegistryへ複製せず、新queue・cache・worker・Run数上限を導入しない。

### Quit受理時の新規tool起動の閉鎖

production TUIは起動前に既存action mailboxへownerのtool-start gateをbindする。Quitをmailboxへ受理する同じ短いguard区間で共有AtomicBoolを一方向に閉じ、bindが遅れた場合も既に受理済みQuitを反映する。追加するのはowner/ShellSupervisorとmailboxで共有する一つの閉鎖factで、新しい業務状態・queue・worker・表は作らない。UIはatomic storeだけを加え、OS起動やDB/I/O、起動mutex待ちをしない。

各callと起動準備のawait後に閉鎖を検査し、Unix spawn/Windows CreateProcessW直前のSeqCst loadを開始受付の線形化点にする。Quit前にその点を通過済みの起動中処理は既存として扱い、それ以降の新しい起動は拒否する。検査からnative作成までawaitは置かない。既存starting intentを持つ拒否はstopped_before_spawn/cancelledとして保存し、spawn失敗や再起動許可を捏造しない。

既に受け付けたSend/draft保存→C5中断context capture→取消し/drainの順序は維持する。Force確認の取消しや待機を選んでもgateを開き直さない。plain Tokio channelを使うRust embedderはfrontendがQuitを受信した時点で閉鎖し、production mailboxの受理時保証と区別する。

## 会話HTTPの有限再試行

未受信の一時失敗は [model HTTP](model-http.md#会話の連続一時失敗の再試行issue-711) の条件に限り既存Driverで再試行する。failed attempt保存後にThread境界を解放して取消し可能な待機を行い、次の通常受付でgeneration/選択/容量/添付を再検証する。context超過が明示されている場合は圧縮を優先する。既存の復旧待ちを時間だけで再開しない。

## Bounded shutdown-stage diagnostics (#32 review)

Reuse the existing DiagnosticEvent::Shutdown and 256-record nonblocking writer
queue. Each optional ShutdownTrace has fixed scope/stage/boundary enums and a
u64 elapsed millisecond value computed from a monotonic clock at the caller,
before enqueueing. It contains no path, raw error, message, provider text or
other unbounded data. Frontend and owner use independent start instants; elapsed
values are comparable only within their scope. Trace stage pairs bracket existing
listener, draft enqueue/save, accepted-send joins, interruption capture, task/read
joins, image cleanup, Control shutdown, worker joins, DB close and endpoint cleanup.
Trace records are observations, not new Core states or completion guarantees.

Keep the diagnostic writer alive until endpoint metadata cleanup has finished,
then close it last within DataOwner::shutdown. Control drain still precedes DB
close, DB close precedes endpoint cleanup, and owner-lock release remains after
owned shutdown returns. Even a metadata worker error must still drain diagnostics.
This changes only the placement of cosmetic diagnostic-writer close; UI priority,
accepted-save ordering, shutdown grace and result precedence stay unchanged.
Frontend owner-begin and owner diagnostics-begin intentionally have no after
record: the writer is closed during that final await and cannot report its own
completion. Never synthesize an end after the writer has closed. All records use
existing best-effort overflow/drop accounting; missing stages alone are not proof
of a hang. In a non-overloaded synthetic shutdown, the IPC frontend regression
checks complete expected stage records, no drops and nondecreasing elapsed time
within each scope after the existing writer has drained.
