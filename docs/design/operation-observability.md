# operation受付と後続処理の観測

## Current-only contract (Issue #33, 0.2.0)

Before 1.0.0, release-specific backward-compatibility paths are not part of the contract. The general schema migration runner, `user_version`/ledger validation, and protocol-version checks remain. Earlier sections describing an old-format fallback, deprecated alias, metadata repair, or compatibility shim are historical and superseded by this section; they must not be used to justify a runtime fallback.

ユーザー採用のローカル拡張。採用済みのCLI契約にある「送信受付と処理の成功は別」「旧instanceは保存結果の照会のみ」を実装へ揃える。既存のoperation ID、receipt、対象状態を再利用し、共通jobエンジンや外部副作用のexactly-once保証は追加しない。観測field自体は既存schemaを使い、後述のreceipt予算にはschema v6、開発途中版との整合にはv8を使う。

## 保存済み結果と再送

公開17 mutationを下表で監査した。DB mutationは変更と成功/失敗receiptを同一transactionへ保存する。runtime操作はpending intentを先にcommitし、副作用後にreceiptを確定する。要求のcommand/argsと認証済みcallerをfingerprint照合し、同じIDの別内容はoperation_conflict。保存結果のある旧instance IDは結果照会のみ、未記録の旧IDはoperation_expired。pending intentは再実行許可ではなく、同一IDでも副作用を二度開始しない。

構文・認証・保存前preflightの拒否にはreceiptが存在しない場合がある。これは受付済み失敗とは区別する。読取り照会自身はreceiptを作らず、同じ照会IDでも現在値を再計算する。変更操作とwaitの元IDは保存結果の照合用として区別する。旧instanceの失効したorigin tokenに新たな権限は与えない。

| mutation | 受付・receipt境界 | 後続処理・確認先 |
|---|---|---|
| thread.create | Thread作成とreceiptを原子的保存 | 作成済みThread、thread.get |
| thread.fork | path共有と新Threadとreceiptを原子的保存 | thread.get/history.list |
| thread.set | 設定更新とreceiptを原子的保存 | thread.get。再送でrevisionを増やさない |
| thread.delete | 削除判定・論理削除とreceiptを原子的保存 | GC容量回収は別、storage.status |
| message.send | user node/割当/receiptを原子的保存 | message.get、run.get/turn.get。acceptedは回答完了ではない |
| run.stop | stopping/取消等とreceiptを原子的保存 | プロセス回収はcommit後。run.get/shell.get |
| run.retry | 復旧状態遷移とreceiptを原子的保存 | 再起動後に自動副作用再実行なし、run.get |
| attachment.add | 出典node/参照/deliveryとreceiptを原子的保存 | delivery反映は別。attachment.list |
| attachment.remove | selection解除とreceiptを原子的保存 | 外部ファイルunlinkなし。attachment.list |
| compact | queued要求とreceiptを原子的保存 | operation.get.downstream、history.list |
| shell.write | pending intent→stdin書込→receipt確定 | 書込後保存失敗は結果不明、別IDで無条件再送しない |
| shell.eof | pending intent→stdin close→receipt確定 | EOF受付とshell終了は別、shell.get |
| shell.stop | pending intent→stop要求→receipt確定 | 自己停止は返信猶予後。shell.get |
| index.backfill | pending intent→job登録→receipt確定 | index.statusはspace全体。receiptから専用job群を推定しない |
| index.retry | pending intent→再試行登録→receipt確定 | 索引完了はindex.statusで確認 |
| index.rebuild | pending intent→新space/job登録→receipt確定 | API途中失敗やpauseは別、index.status |
| storage.collect | pending intent→GC→receipt確定 | 複数batchの途中停止では一部回収済みもあり得る。storage.status |

DB側は同ID再送でMutationResultのeffectsを復元せず、wake/停止等を重複発行しない。runtimeの同時再送は同一owned workerの結果を共有する。通信断は処理を無条件に取り消さず、owner停止・失敗後のpendingを成功に書き換えない。

## 修正した契約の穴

1. compactの非同期完了と起動時取消が受付receiptを上書きしていた。同じID再送が時間によってqueued受付から要約結果/失敗へ変わっていたため、受付結果を不変にした。後続結果は既存app_meta内の `compact.result.<operation ID>` に保存する。pending/inflightリストからqueued/runningを照会し、終端時は結果とended_atを保存する。起動時に残った要求は後続failed/result_uncertain=trueとして記録し、再実行しない。operation receipt自体はcompletedの受付結果を保つ。過去版ですでに上書きされたreceiptの受付を捏造復元しない。 compact.resultのmetadataは既存receiptと同じ保持期間を前提とし、GCのlogical usageにも計上する。後述の旧terminal receipt GCで結果metadataも一緒に回収する。
2. attachment.addの再送が、保存済みreceiptを読む前に現在のThread/ファイル検査をしていた。受付後のThread削除等により元結果を照会できないため、認証・構文検査の後、mutableなpreflightより先に保存receiptを照合する。初回要求は従来のplanとtransaction再検査を通る。新しい成功stubではなく元の結果を返す。

## operation.getの追加field

既存 `operation_id/state/outcome` を維持し、次を追加する。

- instance_id / created_at: 保存された受付instanceと時刻。終了時刻は既存receiptにないため推定しない。
- current_instance: 受付instanceと現在ownerが同じか。
- result_uncertain: pendingで最終結果が未保存ならtrue。実行されなかったことを意味しない。
- replay_policy: `return_saved_outcome` は保存済み成功/失敗を返す。`do_not_reexecute` はpendingであり副作用を再開始しない。どちらも「別IDなら安全」を意味しない。
- targets: 保存された成功結果にあるthread_id/message_id/run_id/turn_id/execution_id/attachment_id/space_idだけを返す。未知のpending対象や失われたIDは生成しない。配列の添付結果はそのままoutcome.resultから読む。
- downstream: compactだけkind/stateと終端outcome/ended_at、messageだけ現在delivery stateと任意hold_reason。対象削除等で一次情報がなければ省略する。他操作の後続状態はtargetsを用いてgetする。index.statusはspace単位集計で、operation単位完了と同一視しない。

結果不明とfailure reasonは区別する。compact起動時取消は副作用が部分的に済んだ可能性もあるためdownstream.result_uncertain=trueを付ける。新たな監査履歴表・全状態遷移ログ・durationをoperationへ導入しない。

## 検証

`tests/operation_contract.rs` でlost response後の同ID照会、messageがappliedになっても元acceptedが不変、別instanceからのpending非再実行、未記録旧ID拒否、実DataOwner再起動時のqueued compact取消、Thread削除後のattachment再送/内容不一致を合成DBで確認する。既存runtime_features/attachment_provenanceのcompact成功・取消試験はdownstreamを観測するよう更新する。外部APIのexactly-onceは試験の保証範囲に含めない。

2026-10-25 macOSで `cargo test --locked --test operation_contract --test context --test deletion --test cli --test protocol --test runtime_features --test attachment_provenance` を実行し、7 test binaries合計56 passed / 0 failed / 0 ignored。新設operation_contractは4件すべて成功。runtime_featuresには送信observer切断後のcommit/effects維持と、compactの安全境界での成功・停止中取消を含む。これは合成データとloopback proxyの検証であり、外部サービスの副作用保証ではない。全体標準チェック・実CLI・設置結果は今回のvalidation記録で別途管理する。

## 監査の範囲外と既存の別境界

上表の17件は公開CLI mutationの集合である。TUI初回draft送信・snapshot受付・依頼編集、およびモデル唯一のshell tool起動は別入口で、表の17件を全副作用の監査完了やexactly-once保証と読まない。既存の `tests/control_db.rs::first_send_creates_thread_and_message_atomically_and_retransmits_once` は初回送信の原子的保存と同ID再送、`tests/runtime_features.rs::editing_first_request_forks_an_empty_prefix_and_accepts_once_with_new_settings` は編集の同ID再送、`tests/runtime.rs::incomplete_attempt_is_preserved_retry_reuses_turn_instruction_snapshot_and_tools_do_not_repeat` は未完了attempt復旧時に保存済みshell callを二度spawnしないことを確認する。stdin書込の同operation再送は `tests/runtime.rs::stdin_operation_retransmission_writes_once_and_wait_any_observes_completion`。これらは対応境界の回帰証拠であり、任意のOS副作用が通信断を跨いでexactly-onceになる証明ではない。

## Receipt対象と有限保持（Issue #3）

`Command::is_read_only`を対象状態の観測分類の正本とする。get/list/search/previewとstatusはoperation receiptを作らず、同じIDでも最新値を再計算する。観測結果をoperation.getで再取得する保証はない。認証・scope・cursor・snapshot寿命・有限IPC枠・完全Responseの8MiB上限は維持する。shell.readの一時pinやhistory.searchのquery identity照合を外さず、SQLite query_onlyの強制とは別である。semantic検索のquery embeddingは再実行により費用が再発生し得る。CLIはこれを自動再送しない。

変更操作とwaitはreceiptを維持する。waitは呼出元のTurn待機/解除結果を保存し、副作用のない照会ではない。DB変更と成功/失敗outcomeは同じtransaction、runtimeはintent保存→外部操作→outcome確定の順を維持する。同ID・同要求・同callerは保全中の結果、別要求はoperation_conflict、pendingは二重実行の許可ではない。

保存outcomeは1件8MiB、通常receipt予算256MiB、停止専用の固定余白を同じ表・counterへ算入する。ID/origin/hash/時刻/本文と512bytesの管理費、current pendingの未使用予約を計上する。これはSQLite物理サイズの上限ではない。変更操作とwaitは実行前に8MiBの最大結果とmetadataを予約するため、短いackでも同時pendingは空fixtureで最大31件であり、既存結果/metadataで減る。観測はこの予約を消費しない。

現instanceのcompleted/failed結果は、created_atから24時間経過後の既存GC保守、又は予算圧迫時に古いものから回収対象とする。24時間は完了後の保証期間ではなく、長時間pendingを解放した直後に対象となる場合もある。pending、accepted/held message、未反映attachment、queued/inflight compactから参照される結果は保護する。時刻/圧迫だけで未確定の副作用を失効させない。

結果回収は既存rowのresult_ref_jsonをJSON scalar `0`へ縮退する。NULLはpendingとして維持し、通常のSavedOutcome objectと区別する。state/operation_id/request_hash/origin/created_atは保持するため、同ID再送をoperation_expired、別要求をoperation_conflictとして拒否できる。operation.get/statusはresult_expiredとreplay_policyで結果失効を示す。変更を新IDで自動再発行せず、対象状態と履歴を調べる。cursorやdownstreamの寿命を延長しない。

現在instanceのID墓標を丸ごと削除すると未知IDとして変更が再実行できるため削除しない。墓標のmetadataも同じ有限予算へ算入する。極端に多数の変更や保護対象のみで予算を埋めた場合は依然storage_limitとなり、無制限の稼働を保証しない。明示owner再起動後は旧prefixの未記録IDをoperation_expiredで拒否できるため、旧instanceの参照なしterminalは従来の7日/圧迫時回収でrow自体を削除できる。旧pendingは保護し、不必要になった予約だけを外す。

保守は1回最大128rowと検証本文累積8MiB以内で、削除と結果縮退を合わせた処理件数を返す。予約前の圧迫回収が不足しても、回収済みbatchはStorageLimit拒否前にcommitし、新要求の副作用/receiptは作らない。結果とcompact.resultの回収は同じtransactionにし、失敗時はcounter/companionともrollbackする。通常の内部GC（起動時と60秒ごと）を再利用し、worker・表・queue・依存・migrationは追加しない。全履歴の無制限scanを導入しない。

receipt-freeのstatus、operation.get、storage.status、同ID再送、TUI停止は容量回復の観測経路として残る。storage.collect自体は変更操作なので新規予約を要する。budgetのretention/expired表示は現在の方針を説明するもので、任意要求の受付を保証しない。実装と回帰は `src/control/receipt.rs`、`tests/operation_contract.rs`、`tests/control_db.rs` に置き、実行結果は [公開検証](../validation/acceptance.md) に区別して記録する。

## 通常予算が満杯でも停止を受け付ける（PR2-1）

通常操作は従来の256MiBまでとし、`run.stop`と`shell.stop`だけに、同じoperation_receiptの
aggregate上限を `256MiB + CONTROL_HEADROOM_BYTES` まで認める。
`CONTROL_HEADROOM_BYTES = 2 × (MAX_OUTCOME_BYTES + MAX_FRAME_BYTES + 4096)`、すなわち
33,562,624 bytes、停止時のaggregate上限は301,998,080 bytesとする。IPCの停止用2枠について、
1件8MiBのoutcome予約、要求frame最大8MiB以内のID/保存origin metadata、512bytesのrow管理費と
固定hash/instance/時刻を収容する4096bytesを各1件分として置く。通常要求はこの余白を使えない。

これは別のpool、quota counter、表、worker、公開操作を増やす仕組みではない。既存のusage/予約/
確定/回収をそのまま使い、受付classで上限だけを選ぶ小さな内部enumをreceipt moduleへ置く。
DB経由のRunStopとruntime経由のShellStopの両方へ適用し、内部送信/編集やその他の操作は通常classを使う。
新規停止も副作用前に同じ8MiBを予約し、fingerprint・保存outcome・原子的DB停止・runtime pending intentの
契約は変えない。同ID同要求は満杯判定より先に照合し、保存済み結果を返す。異なる引数はconflict、
pendingは再実行許可にしない。TUIの現在Run停止は従来どおりreceiptを必要としない。

停止の保存結果も同じaggregate使用量に残るため、通常側に空きが戻るとは限らない。短いmetadataなら
2件を超える停止も余白に収まるが、無限の新operation IDは受け付けない。停止自身の予約/保存結果や
保護された旧recordで停止上限も満ちれば新IDは`storage_limit`となる。同ID照会、receipt-free status、
TUI停止/通常終了、既存の回収/明示restartは維持する。未受付の停止を成功とは表示しない。

`receipt_budget`の既存`limit_bytes`/`available_bytes`/`at_limit`は通常256MiBに対する値を維持し、
`control_headroom_bytes`、`control_limit_bytes`、`control_available_bytes`を追加する。accounted_bytesは
両classの共通使用量であり、availableは個々の要求metadataと8MiB予約を差し引く前の量で、受付保証ではない。
試験は既存予約と実DB counterで通常/停止の満杯境界を作り、通常要求拒否・RunStop適用・ShellStop intent確定・
保存結果再送・引数conflict・有限余白満杯時の拒否を確認する。巨大結果や実APIは不要。

## 受付記録の照会入口

The canonical operation receipt query is `operation get` / `operation.get`. Issue #33 removes the expired `status --operation` alias and old status receipt wrapper. Work status remains its distinct current query; no compatibility projection or alias is retained.
