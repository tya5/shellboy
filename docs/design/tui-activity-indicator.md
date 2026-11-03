# TUIの実行状態と動き

採用状態: 2026-10-25、ユーザーの「コアを複雑にせず、リアクティブなUI構成を維持する」指示に基づく実装仕様。対象は既存ステータス行。仕様策定後、設計・実装・検証まで進める。[UI設計](ui.md) と [起動と受付](runtime.md) を具体化する。Run/Turnの意味を変更しない。

## 範囲

初版は常に見える画面下部の既存1行へ、マーカーと短い処理段階を表示する。行数を増やさず、経過時間・tool/取消件数・重要なエラー・既存の操作結果は保持する。会話本文・過去メッセージ・スクロール位置は動かさない。履歴のtool行やThread一覧のアニメーションは今回追加しない。保存時点でrunningだった履歴や一覧snapshotだけから、現在も実行中と推定しないためである。詳細は既存`/status`/F8へ進む。

コアの永続状態・公開CLI/protocol・DB/migration・依存・OSスレッド構成を追加しない。Issue #8では受信の事実だけを運ぶ既存in-process通知へ`ModelReceive`を追加する。共有Tokio runtime、専用DB worker、有限blocking worker、メインOSスレッドのUIという構成を維持する。実装は既存transport通知経路、frontendの読取projectionとUI内部の表示状態・描画を使う。

## 表示する段階

For message sends, [pending messages](pending-messages.md) owns the current
projection. Local accepting content is tied to draft ID/revision and the frontend
operation ID before dispatch. A paired DB history/queue read transfers committed
content even before its ACK arrives; newer edits are not cleared. A projection
failure after commit keeps source-backed content while fetching, without a resend
hint or an invented unsent state. UI clocks never query or expire the saved queue.
Immutable historical delivery and current Run/Turn activity remain separate.


通常診断には既存の有限writerでfrontend受信、処理開始、保存成功、失敗/キュー満杯を固定分類で記録し、draft ID/revision・operation ID・message IDを結ぶ。queuedはfrontendが要求を受け取った事実であり、Enter自体や画面への描画完了を意味しない。本文・添付path・token・任意エラー本文はログへ入れない。

| 表示 | 既存の根拠 | 動き |
|---|---|---|
| 準備中 | 非終端Turnのpreparing | `●`＋段階文字に控えめなshimmer |
| モデル応答待ち | 現Turnのmodel attemptがinflight、対応する受信通知をまだ観測していない | 同上 |
| 応答受信中 | 同じattempt IDの既存Streamを観測し、そのattemptがinflight | 同上 |
| 一時的なエラーのため再試行中（N/M） | Run live・Turn runningで、最新かつ同generationのfailed attemptに保存したautomatic_retryの回数と上限 | 既存の低速pulse。エラー色にしない |
| ツール処理中 | 現Turnの保存済みtool groupがrunning、モデル要求のinflightではない | 同上 |
| 処理中 | 非終端Turnで、上の内訳を確定できない | 同上 |
| 待機中 | Turnのwaiting | `●`の低速pulse。待機対象は推測しない |
| 回答済み背景処理中 | Turn completed・Run live・所有shell又は子Runが非終端 | `◌`の低速pulse |
| 実行継続中 | Run liveだがTurnは終端、上の背景資源を確認できない | `◌`の低速pulse |
| 停止処理中 | Run stopping | `◌`の低速pulse |
| 復旧待ち／異常終了／中断済み／完了 | 保存済みの対応状態 | 静止。注意状態は色と文言でも区別 |
| 入力できます | 実行中のRun/Turnがない | 静止 |

Run停止・異常・Turn復旧待ちを受信表示より優先する。モデル要求とbackground shellが並行する場合、Turnの主段階はモデル側、背景作業の詳細は`/status`で確認する。「思考中」「添付を解析中」「子の完了待ち」「圧縮中」等の細分化は今回行わない。Streamのreasoning flagから内部の思考内容や進捗を推定せず、受信した事実だけを表す。受信していないことは、providerがまだ生成していない証明ではない。

## リアクティブな更新

1. 既存Changed/Stream/エラー、Thread切替等を契機に表示状態を更新する。保存事実は既存DB worker上のprogress読取へ必要な列を加えて取得する。新しい監視API・周期DB照会・保存eventは作らない。既存の実行中500ms進捗確認は維持するが、アニメーションのために頻度・対象を増やさない。
2. UIへの読取結果はThread/generationと発行世代を照合する。表示段階にはRun/Turn/attempt IDを保持し、古いattemptのStream・切替前の遅い応答・終端後の通知で動作中へ戻さない。同じprojectionは不要な描画を起こさない。
3. 動きはUI内の単調時刻から計算する表示効果だけ。保存値・operation・下書きrevision・履歴revisionを変更しない。表示対象に動きがある間だけ、次フレームの期限でdirtyにする。上限10fpsかつ既存ui.max_fps以下。Runのみのpulseは約2秒周期、shimmerは約1.2秒周期を目安とする。
4. 通常のidle・完了・復旧待ちではアニメーション用の再描画は0。選択/コピー、終了画面、ステータスを覆うoverlay中は時計更新によるdirtyを止める。再表示時は現在の状態・時刻で再開する。入力・スクロール・受信のイベント処理を待たせない。
5. 色・属性だけを変え、レイアウト寸法・文字幅・履歴anchorは固定する。履歴layout cacheを再生成しない。Ratatuiの差分出力を使い、footerのためにMarkdown解析・DB読取・ファイル処理を起こさない。

## 設計と確認条件

frontend内の一時的なActivity projectionと、UI内の小さな表示moduleに責務を分ける。projectionは事実のIDと状態を持ち、別のジョブ状態機械にはしない。UIのStream観測は現在attempt分だけを保持し、新attempt・Thread切替・終端で破棄する。受信通知が読取より先行した場合もIDが一致するまで細分化しない。

匿名fixtureでモデル待ち/受信/ツール/背景/復旧/終端の優先順位、遅い旧通知・Thread切替、同じ状態の無駄なdirty、idle時の描画停止、overlay・選択中の停止、時刻による色の変化と文字/anchor不変を確認する。DB/ファイル処理を遅らせても入力・アニメーションが止まらない境界、既存layout cache再利用も検査する。実PTY/WezTermでは動作中の時間差と終了後idleの出力停止を確認する。標準チェック後にローカルreleaseへ反映し、利用中ownerや既定DBを変更しない。APIを必要としない表示検証を基本とする。

## HTTP受信の空白期間（Issue #8）

モデル要求の応答header、および非空body chunkの受信は既存の有限HTTP→Driver通知を経由し、HTTP層で採取した単調受信時刻を保持し、DriverがThread/attempt IDを付けた非永続`ModelReceive`でfrontendへ渡す。既存の同一32件channel上で型付き内部通知とmodel eventを区別し、providerのJSON値を受信通知へ誤認しない。header/chunkだけの通知はchannel満杯なら捨ててHTTPを待たせないが、通常のmodel eventにも元の受信時刻を付け、待ち合わせの発生時も受信の事実を補完する。Driverは先頭の受信を即通知し、その後の追加通知は最大1回/秒に間引く。長い空白後の受信は即通知される。表示は最終byteより1秒未満古い受信観測になる場合があるが、HTTPのtimeout判定はすべての非空chunkで正確に更新する。新しいtimer・taskは追加しない。SSE comment、keepalive、未完成frameも受信した事実に含める。構造化deltaや文字の生成とは区別し、headerだけで「応答受信中」へ進めず、会話本文やmodel入力へ混ぜない。公開protocol、DB、復旧状態、永続eventは増やさない。新規worker、queue、pollerも追加しない。

frontendは現在選択中Threadのgenerationを付け、UIはThread/generationと保存済みinflight attemptを照合する。UIは受信時刻を1件だけ保持し、古いattemptからの通知を現在の時計へ適用しない。headerが保存projectionに先行した場合は1件の候補として保持し、一致するprojectionだけに適用する。終端、Thread切替、観測不能で破棄する。未選択中・起動前の無通信時間を遡って推測しない。

同attemptの保存済みinflightを画面で観測した時点から30秒以上、受信事実がなければfooterに`受信待ちN秒（画面の観測開始から）`を補足する。最初のheader/byte受信後の空白は`最後の受信からN秒`とする。既存のTurn観測経過とは別であり、API全体timeoutや生成失敗の断定ではない。新しいbyteの通知で補足を解除し、最新時刻から再計測する。古い時刻の遅着で時計を巻き戻さない。

30秒の表示閾値は既存の局所アニメーション時計で更新する。UIでI/O・DB照会・周期要求を行わず、header/chunkごとの通知で履歴revision・本文retention・Markdown layout cacheを無効化しない。hidden overlay中のdirty停止は既存規則を使う。受信がないことだけで復旧待ちや自動再送へ進めない。既存の取消し・最終応答採用順序を維持する。

## 有限の自動再試行（Issue #7）

Issue #8の受信空白の観測と自動再試行の待機は区別する。Driverが既存model_attempt.error_jsonへ保存したautomatic_retry（number、limit、delay_ms等）を、既存Changedから読取projectionへ反映する。最新attemptがfailed、Turnとattemptのgenerationが一致し、Run live・Turn runningの場合だけ再試行待機を表示する。古いattempt、旧generation、復旧待ち、停止中、終端には適用しない。新しいinflight attemptを保存した後は通常のモデル応答待ちへ戻る。

footerは既存の1行・同じpulseと中立的な活動色を使い、保存済みの回数・上限を `一時的なエラーのため再試行中（1/2）` の形で表示する。表示するカウントは既存snapshotへ加えるUI向けprojectionであり、コア状態・永続表・event・worker・queueは増やさない。待機中はIssue #8のattempt別受信時計を破棄し、新attemptの受信を観測してから新たに計測する。完了・復旧後も古い受信通知で待機表示や受信時計を復活させない。終了時は保存済みの終端/復旧表示を優先し、再試行を経て成功したことだけで警告色を残さない。

## Footer fact correction (#85)

Keep the saved shared activity classifier and all Core states/protocol unchanged.
A single UI composition gives current tools precedence
over the current attempt while retaining the attempt as a secondary phase.
Matching transport receive observations (headers/body liveness, not generated
assistant content) end the response-wait label, including receive-before-snapshot.
A new attempt resets its request age using saved started_at. Tool age uses the
running group's saved creation anchor; receiving age is explicitly observed.
Other phase durations are explicitly local observations, never the Turn total.
Clock bases are internal UI facts from the existing shared view read, not timers.

A saved recovery message is current only for failed/recovery_wait state, not
merely because a historical recovery_reason_json still parses as ApiError.
Resolved execution recovery disappears on a fresh successful saved-state read;
independent command failures keep their existing notice ownership. Typed #76
usage-limit facts supply reset kind/time and wait or /model guidance. A passed
reset timestamp never promises automatic retry or bypass of connection cooldown.

Structured EngineEvent errors carrying saved recovery facts invalidate the
existing selected-view read instead of creating an unrelated sticky operation
notice. The saved RecoverySnapshot owns that footer and its resolution; storage
or unclassified errors retain their existing notification path. This is frontend
presentation ownership only: saved errors, diagnostics and Core events do not
change. No text matching or TTL is used to infer resolution.

Age anchors are data, not independent schedulers: both the primary tool/stage and
secondary request anchor advance only from the existing UI monotonic now. Saved
wall timestamps are parsed at snapshot acceptance, not on every footer frame.
Repeated identical anchors do not reset ages; generation/attempt changes reset
only the relevant stage. Send acceptance and reflection also have separate local
stage ages, choosing the oldest pending item of the displayed stage rather than
an unrelated older pending stage. Unknown or future saved times stay unknown.

## Shared elapsed units and saved terminal time (#85 follow-up)

All human elapsed labels use one presentation formatter, with stable unit keys:
seconds below a minute, minutes+seconds below an hour, hours+minutes thereafter.
Tool durations retain tenths below a minute. Stored numeric durations, source
payloads and copy/original facts stay unchanged. Terminal footer phases append
only the current saved Run ended_at (never observation time). Convert to the
viewer offset captured on the presentation worker; same local date shows HH:MM,
other dates show YYYY-MM-DD HH:MM. Missing/invalid facts explicitly say unknown.
Formatting a later explicit redraw may age the date label without any idle timer,
new read or unsolicited frame. Recovery/live phases do not show an older end time.
Latency/CPU are recorded observations only, not acceptance gates. Acceptance uses
virtual-clock/frame/read/job count bounds.

The historical one-row initial scope above is not the current layout: current
chrome::footer_lines returns two quiet rows. #85 owns first-row activity facts and
recovery content; #87 owns second-row key-hint selection/fit. These additions do
not reintroduce an earlier row layout or overwrite #87's caller-supplied hint API.

## 区画ごとに1つの動き（#90）

画面の区画ごとに、動くものは最大1つ。会話は1行だけのスピナー、「抱えている作業」は見出しの`◉`の明るさ、ステータスバーは上表の既存shimmer/pulseのまま。

- 時計は1つ：`ui/activity.rs::Animation`が全区画のコマを計算し、`UiState::animation_tick`の同じ`now`で1回だけdirtyを判断する。ステータスバーかスピナーが動く間は既存の`min(max_fps,10)`の期限を共有し、スピナーを足しても描画回数は増えない。明滅だけのとき（Turn完了・Run liveの背景作業）は、区画の既存の秒時計と同じ原点の秒境界でだけ起き、経過秒の更新と同じ1フレームにまとめる（1秒に最大1回の変化）。何も動かないときは期限を持たず、描画しない。
- スピナーの行の選び方は`ui/motion.rs::select`の1関数：Turn進行中に、最新の`running_current`かつ背景でないshell、なければ現attemptの生成中（provisional）回答行、なければなし。描画時に見えている行の印1セルだけを置き換え、layout cache・source・選択・F6/F7・scrollの計算には触れない。
- `ui.motion = "off"`で3区画とも静止した印に戻す。`NO_COLOR`では明滅しない（スピナーは動く）。overlay・選択固定中は既存の規則どおり期限を止める。
- DB/出力の読取り・worker・queue・timerは追加しない。

## Automatic compaction activity (#165)

The active proactive operation is an auxiliary fact on the existing selected
activity snapshot, not a new lifecycle phase. It is distinct from the saved
receipt-backed explicit context-edit wait above. The driver owns a per-Thread
`compact.automatic_active.*` witness in existing `app_meta`: exact Run and Turn,
the provider-reported input and configured threshold that triggered the boundary,
and the actual start time. The selected read accepts it only for its live Run
and preparing/running Turn and only when the reported input reaches a positive
threshold. Invalid, incomplete or mismatched witnesses are not projected. The
operation removes its own witness on normal return, failure or cancellation;
interrupted-instance repair removes leftovers. No new table, worker, queue,
setting, polling cadence, protocol state or marker is introduced.

Render the fact using localized wording and the shared glyph separator; the
numbers are the trigger facts, not an estimate of current context. The existing
phase clock uses the saved start time while the fact is active, falling back to
unknown for an invalid/future time. Two fixed events on the existing bounded
diagnostic writer mark start and finish with Thread/Run/Turn IDs; no message,
provider response or input content is logged. These events share the existing
best-effort bounded queue and can be dropped under pressure; a missing pair is
not evidence that no compaction occurred. The older initial-scope statement above
excluding a compaction indication is superseded by this section.

## Slow foreground DB jobs (#198)

Submitting is a local, uncommitted request, not a successful save. On submission,
the existing PendingSend owns the immutable original (including attachments and
edit target); the composer immediately becomes a new revision for the next input.
No UI or frontend-loop DB wait is introduced. The send worker can wait in
`prepare_draft_submission` and ordered send admission; the paired saved projection
still alone transfers ownership to persisted history/queue. A failed admission
restores the original only if the cleared composer is unchanged. Newer edits are
never overwritten; the existing restore route retains the original. Durable
commit evidence after an observed failure clears an untouched automatic restore,
but never newer human edits.

The foreground DB watch and local animation remain independent of the writer
queue. Preserve the existing DB debug item and its wording (user scope update:
no #198 label rewrite). Reserve the current phase's cells before fitting that
trailing item: a long compiler-derived debug identifier must not erase the
conversation state. Keep the same two footer rows and existing glyphs. Persisted
activity cannot advance before a DB commit; do not invent progress from a clock.

The regression hook holds a real foreground closure with an explicit channel
gate, with drop releasing it on failure. Tests receive the operation/status/watch
updates while that gate is still closed, edit and render the next input, and
release the gate before checking the late saved projection. Inject monotonic
instants for elapsed rendering; no wall-clock sleep establishes an assertion.
This does not speed up history SQL (#164) or establish real-terminal latency.

The status-loss bug also involved span fitting, not a DB wait: shimmer splits
its phase label into single-character styled spans. Applying whole-label
whitespace normalization to each span treated the first space as truncation
and discarded the rest of the phase. Cell fitting must preserve whitespace
spans and their styles, truncating only at the available grapheme/cell boundary.
Draft-image retention follows both local owners: attachment release declarations
include the current composer and pending send snapshots. Deleting/clearing the
next input cannot unpin an image needed by a delayed or failed send's restore.
This reuses the existing draft-image leases, with no new queue or DB request.
A late edited-send stop-confirmation is likewise matched to the pending request,
not the next composer's revision. Restore the unchanged composer first and bind
the confirmation to that restored revision. If newer input exists, keep it and
the failed snapshot, without opening a confirmation for unrelated text.
