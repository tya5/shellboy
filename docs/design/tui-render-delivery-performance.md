# TUIの描画再利用と更新配送

採用: Issue #29 / #30。UIは描画・メモリ更新だけを行い、既存worker、永続履歴、確定前ストリームの採用順を変えない。

## 描画の単位と予算

既存history rendererの隣接tool call/result、quiet eventの連続、interruptionとsummaryを分割せず、一つのrender unitとして再利用する。keyは元node区間の絶対index、各nodeのprojection stampと本文windowのbase/total、幅、reasoning展開、前段のlink番号表。途中のnode追加・並替え、本文/metadata/幅/展開/link番号の変更は再利用しない。stampがないnodeは全量と同じrendererを通す。UIで表示情報を変更するときはstampを無効にする。

組み上げた履歴とunitの両cacheは、同じ`cache.layout_bytes`のLRU予算へkey、行/span、source map、link参照、保守的なmap overheadを計上する。別枠の無制限cacheにしない。unitはThreadViewではなくUiStateのlayout cacheが所有し、evictionとUI終了で解放する。原文の192KiB/128node投影上限を維持する。高さは投影に影響しないためkeyに含めず、viewportのclamp/末尾判定はその高さで再計算する。

rendererは物理折返し済みの行を返すので、組立後に全行をもう一度word-wrapして高さを数えない。全量rendererと、style・文字・link・元位置・行数が一致することを試験する。幅が変わったときは新viewportの全可視nodeと既存3node先読みを再描画要求し、非表示nodeはスクロールで必要になってから取得する。cache保護用working setの免除予算は変えない。幅/可視範囲の再計算より前にsource-window再取得のpreflight意図を捕捉し、viewport初期化/clamp後も消さない。

本文cacheのnode動的領域の合計はThreadViewが保持する。stream nodeの入替えは、そのnodeの前後の容量差だけを反映する。一般の履歴差替えやevictionでは合計を失効させて一度だけ再集計する。Vecのinline slot容量、working set免除、reasoning/committed対応表の既存計上は分離して保つ。node本文全体を毎streamで走査しない。source ID集合の掃除も一つの有限集合を使い、展開数×node数の探索を避ける。

streamの表示stampは本文の暗号学的provenanceではなく、Displayのsource serial・delta/gap revision・Thread/attempt・幅から作る一時projection識別子である。保存nodeのcontent/provenance hashを置換しない。本文64KiBの一回のsnapshot copyとMarkdown再解析は既存の有限presentation workerへ残す。並行して次のdeltaを受けるためのsnapshotと、確定保存順を守るDB admission確認は除去しない。受信済みsourceを消費済みだと誤認する最適化をしない。

## Delivery boundary (reopened #19 / #30)

```text
Control UI broadcast(256) -> frontend loop -----+
                              finite workers --+-> shared BufferedUpdates -> UI channel(128)
priority Cancel/Quit -> separate action admission
```

The frontend and its existing finite workers clone one `Arc`-owned delivery
state. Every producer uses the same lock, sequence, queue and latest-state
coalescing. No worker receives a raw sender. This changes neither Core events
nor persistence ordering and adds no task, service, channel or background poller.
The frontend's existing select branch flushes at most 32 entries per turn.

The shared outbox holds at most 512 critical results and 128 latest-state keys.
Only Status, DraftStatus, Progress, ModelReceive, Activity/Unavailable,
RecoveryBell and ShutdownProgress coalesce. Keys distinguish Thread, draft and
generation. Receipts, messages, ThreadDeletion, CommandOutcome, history adoption
and attachments never coalesce. Insertion order is shared across producers;
replacing a latest-state key removes only its superseded transient state.
Consequently an earlier preparing/saving status cannot arrive after a later
worker completion or image failure.

Normal action/read admission stops at 128 queued critical results. The existing
512-entry critical budget reserves its last 32 entries for the frontend's main
producer. Worker handles can insert critical results only while the shared count
is below 480; they retain rejected values and their existing permits and retry
through the same outbox. Main background-result admission and draft-failure
projection require the count to be at most 480, leaving space for the at-most-two
critical updates in a single admitted main handler. Normal-action handlers are
already limited by the 128-entry threshold. No producer bypasses queued entries
by sending directly to the UI channel. These are item bounds, not RSS limits.

The main producer does not retry a large receipt-result loop inside a handler.
Each refresh selects at most 128 pending receipt IDs. A checked bit on each
existing pending receipt schedules not-yet-checked entries before repeating a
scan; a further dirty refresh covers the remaining IDs. This avoids an unbounded
query/result vector or a separate cursor cache. There is one frontend-owned
continuation of at most 128 `(message, status)` entries, drained synchronously
at most 32 entries per frontend turn and only while main capacity is available.
Further background-result intake pauses until that continuation is empty; the
existing 32-entry background channel and existing worker permits retain their
backpressure responsibilities. Each insertion keeps the shared sequence/FIFO;
important results are neither coalesced nor truncated. Initialization similarly
retains at most one undelivered update instead of blocking the action loop.

Priority Cancel/Quit remain polled even with the raw channel and all 512 critical
slots full. Queued Send/SendEdited actions drained during Quit retain their
existing send-worker ownership and permit acquisition,
so closing action drainage cannot consume the main reserve indefinitely. Closing
sends skip the obsolete transient Accepting projection and persist before result
delivery; normal Send retains its immediate main-loop receipt. Closing draft snapshots retain their existing
coalesced autosave ownership. No extra task, worker service, Core API, persistent
state or queue is introduced.

UI disconnect releases queued values and enters existing shutdown. Normal
shutdown joins admitted work and drains delivery. The final UI-only drain does
not restart the owner's shutdown grace: an explicit ForceQuit is honored
immediately and reported as forced termination, never successful delivery.
The existing grace policy while owned work is still running is unchanged.

## 購読と遅着

一般の`Control::subscribe()`は従来どおり全stream offsetを観測できる。UIだけは別の有限broadcast 256件を使用する。ControlはsingletonのUI購読owner serialとselected Thread一件だけを保持し、Stream/ModelReceiveを購読前に選別する。Changed/復旧/完了/Notice/Errorなどのlifecycleは非表示Threadも配信する。新しいCore eventや永続状態は増やさない。

購読guardのdropは、自分のowner serialと一致するときだけselected Threadを解除する。古いguardが後からdropして新しい購読を解除しない。Thread切替前に既にqueueへ入った差分はfrontendの現selected Threadとgenerationの既存fenceで捨てる。lagged時は従来の再取得で回復し、差分配送を保存・採用の証拠にしない。guardとchannelはControl/frontendの所有期間だけ生存し、shutdownでguardを解放する。

## 検証境界

baseline/afterは同じignored合成fixtureを使用する。192KiB前後の履歴と120streamフレームについてlayout/update p50/p95、height再構築、幅変更の要求数/時間を記録する。配送fixtureは実frontendと一時DBで10Threadの合成delta、既存128枠UI停止250ms×7回を使い、action p50/p95、lagged、reloadを測る。これはHTTPや実端末の性能を保証する試験ではない。local HTTP mockの生成経路、文字/style/sourceの同値、有限予算、priority停止/終了、購読遅着も別途確認する。実測の実施/未実施と環境はvalidationへ記録する。

### Selection-gap limitation (reopened #30, Low)

Selection filtering happens when Control publishes an event. Until the frontend
consumes a queued SelectThread action, deltas for its future target may still be
filtered out. Updating the subscription at action handling prevents further
loss, but cannot replay that prefix. Offset gaps remain marked as incomplete;
terminal/Changed refresh reloads canonical persisted history. Switching threads
is therefore not a promise of lossless live-delta observation. General diagnostic
observers remain unfiltered. A Core replay cache or new selection handshake is
deferred: either adds ownership, budgets and cross-layer coupling for transient
presentation, while canonical results already recover through the existing path.

### Full-reserve continuation (Issue #32)

The reserve and incremental continuation are a single design: a reserve alone
cannot contain an arbitrarily large main batch, and continuation alone cannot
progress when workers can consume every main slot. With both, the main loop
retains at most one bounded batch and continues polling priority controls while
UI delivery is stopped. Work results waiting in worker futures retain their
original admission permits. Normal shutdown first stops IPC intake, saves draft backlog and joins admitted sends,
then completes owned shutdown before admitting retained initialization and receipt
projections. UI-only capacity waits cannot delay durable persistence. The outer
shutdown select continues flushing UI output and accepting existing shutdown choices.
After owned shutdown completes, at most one error status and one Shutdown marker
remain in a local terminal continuation. Its capacity retry still polls explicit
ForceQuit, so a full 512-entry outbox cannot trap the shutdown-complete branch.
This UI-only wait does not restart the owned-work grace. Ordinary shutdown does
not claim delivery until the outbox drains; explicit force may abandon
undelivered presentation and returns forced-shutdown failure.
UI disconnect releases the outbox and bounded continuation through their current
owners. The 512/128/32 budgets are unchanged; no additional queue or service is
introduced.

A private test-only startup fixture runs the actual frontend with a full raw UI
channel, 480 worker critical results, 32 reserved main results and a retained
128-entry receipt batch. It must prove durable cancellation, drainage of 40
queued closing sends, and Quit/one explicit ForceQuit without reading any UI
update. Owned-shutdown-complete fixtures cover terminal-marker saturation for
both success and error, plus ordinary FIFO delivery after UI resumes. Separate continuation tests verify FIFO,
lossless receipt order, permit retention/release, bounded query scans and
receiver-disconnect cleanup. Synthetic watchdogs establish regression behavior,
not scheduling latency guarantees on all terminals or OSes. Existing ten-thread
pressure and HTTP-mock/PTY fixtures remain separate acceptance checks.

### Re-review: worker wakeup and explicit-force display

The shared delivery state owns one Tokio Notify (at most one stored wake permit).
An empty-to-pending transition notifies the frontend. Its existing select always
polls that notification, even when the conditional flush timer was disabled when
select began. The branch performs the same bounded 32-item flush. No new task,
channel, Core event or periodic poller is added; all worker clones share the
existing Arc owner and shutdown releases it after joins/drain.

ForceShutdown delivery is best effort after explicit force: a nonempty/full UI
queue can prevent that presentation marker from reaching the UI. The frontend
still returns forced_shutdown failure and main reports the shutdown error with
nonzero exit. Do not restore a capacity/drain wait to guarantee a cosmetic marker;
that would undo the accepted one-request force boundary. This exception does not
permit dropping ordinary operation results during normal delivery.

The worker-wakeup fix and the full-reserve continuation serve separate purposes:
Notify wakes an idle select; the reserve and continuation prevent one main
handler from waiting for permanently stalled UI capacity. Neither replaces the
other.

The priority guarantee uses the TUI's reserved `ui::actions::channel` mailbox.
The retained plain Tokio receiver adapter has one ordinary FIFO and no separate
priority slots; it cannot promise priority bypass of earlier normal actions.
It preserves its existing embedding behavior and is not the actual TUI path.

### Issue #32 re-review: persistence and scan fairness

Autosave pumps its existing bounded worker independently of UI capacity. A closed
worker retains one failed-owner notification until the main reserve is available;
further backlog stays owned by DraftSaver rather than being dropped. Shutdown
completes durable work before waiting for retained UI-only projections. Closing
sends omit their obsolete Accepting projection and commit before result delivery.

Changed during a bounded receipt scan sets a repeat bit on the existing pending
receipts of that Thread. Complete the current scan fairly before clearing checked
bits for one further pass; repeated changes coalesce. No per-Thread cursor cache,
new channel or worker is introduced. Clone of a delivery handle always produces
a worker-limited handle; shutdown borrows the unique original main handle.

### Further #32 review: one closed-worker notice and fresh scan boundaries

Once DraftSaver observes its worker receiver closed, one existing local boolean
stops further worker pump attempts. The latest snapshots remain in the existing
backlog; one failure projection per affected Thread may wait for UI capacity,
but ticks and later revisions of the same retained entry do not repeat it. The existing worker join still
reports its failed shutdown. No replacement worker or notification queue is
created; owner restart is required to recreate the autosave worker.

A receipt pass starts fresh when none of the Thread's pending receipts has been
checked, or when all were checked by the previous completed pass. Starting that
pass clears both checked and repeat bits: changes observed before its query are
already included. Mixed checked/unchecked state is a partial pass; its repeat
bits survive so changes during the pass request exactly one fair further pass.


### Completed receipt passes and stopped saver visibility (#32 follow-up)

A completed receipt pass clears checked bits before returning idle, including
when pending unconsumed receipts remain. Thus a later new receipt cannot make
old completed work look like a partial pass. Changed during an actual partial
pass still requests one fair full follow-up.

A closed autosave worker is never pumped again. Each existing backlog entry
carries one local notification bit: warn each affected Thread once, including
Threads first edited after closure, without repeating for newer revisions of
the same retained entry. Backlog limits and normal-shutdown failure remain.
No notification queue, extra worker or unbounded per-Thread registry is added.
