# TUIの描画再利用と更新配送

採用: Issue #29 / #30。UIは描画・メモリ更新だけを行い、既存worker、永続履歴、確定前ストリームの採用順を変えない。

## 描画の単位と予算

既存history rendererの隣接tool call/result、quiet eventの連続、interruptionとsummaryを分割せず、一つのrender unitとして再利用する。keyは元node区間の絶対index、各nodeのprojection stampと本文windowのbase/total、幅、reasoning展開、前段のlink番号表。途中のnode追加・並替え、本文/metadata/幅/展開/link番号の変更は再利用しない。stampがないnodeは全量と同じrendererを通す。UIで表示情報を変更するときはstampを無効にする。

組み上げた履歴とunitの両cacheは、同じ`cache.layout_bytes`のLRU予算へkey、行/span、source map、link参照、保守的なmap overheadを計上する。別枠の無制限cacheにしない。unitはThreadViewではなくUiStateのlayout cacheが所有し、evictionとUI終了で解放する。原文の192KiB/128node投影上限を維持する。高さは投影に影響しないためkeyに含めず、viewportのclamp/末尾判定はその高さで再計算する。

rendererは物理折返し済みの行を返すので、組立後に全行をもう一度word-wrapして高さを数えない。全量rendererと、style・文字・link・元位置・行数が一致することを試験する。幅が変わったときは新viewportの全可視nodeと既存3node先読みを再描画要求し、非表示nodeはスクロールで必要になってから取得する。cache保護用working setの免除予算は変えない。幅/可視範囲の再計算より前にsource-window再取得のpreflight意図を捕捉し、viewport初期化/clamp後も消さない。

本文cacheのnode動的領域の合計はThreadViewが保持する。stream nodeの入替えは、そのnodeの前後の容量差だけを反映する。一般の履歴差替えやevictionでは合計を失効させて一度だけ再集計する。Vecのinline slot容量、working set免除、reasoning/committed対応表の既存計上は分離して保つ。node本文全体を毎streamで走査しない。source ID集合の掃除も一つの有限集合を使い、展開数×node数の探索を避ける。

streamの表示stampは本文の暗号学的provenanceではなく、Displayのsource serial・delta/gap revision・Thread/attempt・幅から作る一時projection識別子である。保存nodeのcontent/provenance hashを置換しない。本文64KiBの一回のsnapshot copyとMarkdown再解析は既存の有限presentation workerへ残す。並行して次のdeltaを受けるためのsnapshotと、確定保存順を守るDB admission確認は除去しない。受信済みsourceを消費済みだと誤認する最適化をしない。

## 配送の境界

```text
Control 一般 broadcast ── 従来の全件診断observer
       UI broadcast(256) ── frontend中心loop ── BufferedUpdates ── UI channel(128)
                               │                   │
                               ├─ 既存有限worker ── raw sender / permitを保持して待機
                               └─ priority Cancel/Quitを通常admissionと別に読む
```

`BufferedUpdates`はfrontendのasync bridge futureが一つ所有する明示型。task-local、追加task、worker、pollerは使わない。中心loopの状態通知はUI送信を待たず、既存selectの有限flush分岐で配送する。spawnするworkerには`worker()`で従来のraw SyncSenderを明示的に渡す。workerの待ちはその既存permitの内側なので、UIが遅くても中心loopのevent/action消費を停止しない。

中心outboxは重要結果のFIFO最大512件と、最新表示状態128key。最新へ集約できるのはStatus、DraftStatus、Progress、ModelReceive、Activity/Unavailable、RecoveryBell、ShutdownProgressだけ。receipt、message、ThreadDeletion、CommandOutcome、履歴採用、添付などはこの分類に含めない。latest keyは対象Thread/draft/generationを区別し、128keyを超えた場合は古い一時表示状態から外す。既存の永続情報、操作結果、履歴は消さない。配送順はoutboxへの順序で管理し、同一keyの旧状態だけを最新へ置換する。

重要結果128件から通常action/read admissionを止める。残る384枠は既受理のcommand 32、send 32、read 8、clipboard 2、background channel 32、draft backlog 64とその有限結果を扱うための余裕。通常actionの新規受付を止めるので、worker完了によるslot再利用で結果が際限なく増えることはない。priorityの停止/終了は止めず、継続して増え得るCoreの状態通知はlatest/coalesceまたは既存refreshへ畳む。例外的に重要FIFOの上限へ達しても、重要結果を捨てたり成功扱いしたりせず、元の値を保持してraw送信のbackpressureへ戻す。これは項目数の予算でありRSSやSQLite使用量の上限ではない。

UIが切断したらoutboxを解放し既存shutdownへ進む。通常shutdownでは受理済みworkerの保存/回収とoutboxの配送を行う。UI配送の終了待ちでもpriority ForceQuitを受け付け、明示強制終了の結果を返す。重要結果が未配送の状態を黙って正常終了にはしない。

## 購読と遅着

一般の`Control::subscribe()`は従来どおり全stream offsetを観測できる。UIだけは別の有限broadcast 256件を使用する。ControlはsingletonのUI購読owner serialとselected Thread一件だけを保持し、Stream/ModelReceiveを購読前に選別する。Changed/復旧/完了/Notice/Errorなどのlifecycleは非表示Threadも配信する。新しいCore eventや永続状態は増やさない。

購読guardのdropは、自分のowner serialと一致するときだけselected Threadを解除する。古いguardが後からdropして新しい購読を解除しない。Thread切替前に既にqueueへ入った差分はfrontendの現selected Threadとgenerationの既存fenceで捨てる。lagged時は従来の再取得で回復し、差分配送を保存・採用の証拠にしない。guardとchannelはControl/frontendの所有期間だけ生存し、shutdownでguardを解放する。

## 検証境界

baseline/afterは同じignored合成fixtureを使用する。192KiB前後の履歴と120streamフレームについてlayout/update p50/p95、height再構築、幅変更の要求数/時間を記録する。配送fixtureは実frontendと一時DBで10Threadの合成delta、既存128枠UI停止250ms×7回を使い、action p50/p95、lagged、reloadを測る。これはHTTPや実端末の性能を保証する試験ではない。local HTTP mockの生成経路、文字/style/sourceの同値、有限予算、priority停止/終了、購読遅着も別途確認する。実測の実施/未実施と環境はvalidationへ記録する。
