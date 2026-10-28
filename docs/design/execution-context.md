# 保存済み実行コンテキストの公開

採用状態: ユーザーが採用した実行コンテキスト整備のローカル拡張。引継ぎの状態モデル、CLI契約の既存get、production migration 0001の保存項目を基準にする。新たな実行・監査機構やmigrationは追加しない。

Current `run get`, `turn get`, and `message get` require `context`: every current writer constructs it from saved facts. `thread get` may omit it for a fork result, and history list/get require saved context. These genuine unknown/not-applicable cases remain optional; old-owner compatibility is not a reason to omit a required field. contextは保存済みの一次情報だけを返し、存在しない項目は省略する。Threadの親子関係 `parent_thread_id` とRunの実行所有 `owner_run_id` は別項目にする。nodeのthread/runは作成時のTurnから取得し、fork後の採用Threadへ置き換えない。

| 対象 | 時刻 | state / 理由 |
|---|---|---|
| Thread | updated_at | active / deleted。Runの実行状態とは異なる |
| Run | created_at / ended_at | 保存state、stop_reason。最新Turnの保存済みrecovery_reason |
| Turn | created_at / ended_at | 保存state、構造化recovery_reason。failed時のみ同情報をfailure_reasonにも公開 |
| message | receipt created_at / updated_at | 配送state、hold_reason、割当Run/Turn、node_id |
| history node | created_at | present / tombstone、tombstone_reason、作成TurnとRun/Thread |

`started_at` は現行の対象表に記録がないため省略する。`lifetime_ms` はRun/Turnのcreated_atからended_atまでの閉じた壁時計区間で、待機・復旧待ちも含む。API実行時間やCPU時間を意味しない。未終了、時刻形式不正、終了が作成より前の場合は省略する。現在時刻を読み出してdurationを動かすことはせず、同operationのreceipt再照会も安定させる。

recovery_reason_jsonはJSON文字列として二重encodeせず、JSON値として返す。復旧待ちは失敗と同義でないため通常のfailure_reasonへ移さない。状態遷移履歴、記録していない失敗理由、Thread作成時刻は捏造しない。Thread削除後も保持されるRun/Turn/nodeのコンテキストは取得できるが、既存thread getの削除済みThread拒否は変更しない。node本文がGCでtombstone化されても作成元と理由は残す。

ヘルパーは読み取りだけを行い、DB workerまたは既存のread transactionから呼び出す。外部I/O、時刻更新、追加の永続化、UIスレッドへのDB処理移動は行わない。
